//! The wiring: a thread-local [`Ui`] holding the retained views and the pure
//! [`UiModel`], the any-thread [`post`] entry point, and [`run`], which
//! builds the panel, status item and hotkeys and enters the AppKit run loop
//! (spec: AppKit objects live only on the main thread; other threads send
//! `UiEvent` values through the main dispatch queue).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

use block2::RcBlock;
use clueless_types::{
    CommandSink, Config, EngineCommand, MeetingState, StatusLevel, StatusSource, UiEvent,
};
use dispatch2::DispatchQueue;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSWindow};
use objc2_foundation::NSTimer;

use crate::hotkeys::{
    self, HotkeyAction, MOVE_STEP, ParsedHotkey, register_always_on, sync_meeting_keys,
};
use crate::model::UiModel;
use crate::panel::OverlayPanel;
use crate::status_item::StatusItemController;
use crate::views::{self, OverlayViews};

/// Quit deadline after `Shutdown` (terminate on `MeetingState(Idle)` or
/// after 6 s at the latest; the rationale is in `docs/decisions.md`).
const QUIT_DEADLINE_SECS: f64 = 6.0;

/// Everything the main thread owns. Reachable only through [`with_ui`].
struct Ui {
    model: UiModel,
    panel: Retained<OverlayPanel>,
    views: OverlayViews,
    status_item: StatusItemController,
    commands: CommandSink,
    hotkey_manager: Option<GlobalHotKeyManager>,
    parsed_hotkeys: Vec<ParsedHotkey>,
    /// (id, action) of the hotkeys currently registered.
    registered: Vec<(u32, HotkeyAction)>,
    overlay_visible: bool,
    quit_timer: Option<Retained<NSTimer>>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.borrow_mut().as_mut().map(f))
}

/// Hotkey id -> action for every parsed hotkey; only registered keys fire,
/// so meeting-only keys are naturally filtered by registration state.
/// The handler runs on the hotkey monitor's thread, outside the reach of
/// the thread-local [`Ui`], hence a process-wide map.
static ACTION_BY_ID: OnceLock<Mutex<HashMap<u32, HotkeyAction>>> = OnceLock::new();

fn actions() -> &'static Mutex<HashMap<u32, HotkeyAction>> {
    ACTION_BY_ID.get_or_init(Default::default)
}

/// Apply an action on the main thread; `panel` and `status_item` changes
/// happen here, engine commands go out through the sink.
fn handle_action(action: HotkeyAction) {
    if let Some(mtm) = MainThreadMarker::new() {
        with_ui(|ui| {
            if let Some(command) = action.engine_command() {
                (ui.commands)(command);
                return;
            }
            match action {
                HotkeyAction::ToggleOverlay => {
                    ui.overlay_visible = !ui.overlay_visible;
                    if ui.overlay_visible {
                        ui.panel.orderFrontRegardless();
                    } else {
                        ui.panel.orderOut(None);
                    }
                    let running = ui.model.meeting() == MeetingState::Running;
                    ui.status_item
                        .rebuild_menu(running, ui.overlay_visible, mtm);
                }
                HotkeyAction::ToggleClickThrough => {
                    let interactive = !ui.panel.is_interactive();
                    ui.panel.set_interactive(interactive);
                    if interactive {
                        ui.panel.makeKeyAndOrderFront(None);
                    }
                }
                HotkeyAction::MoveLeft => ui.panel.move_by(-MOVE_STEP, 0.0),
                HotkeyAction::MoveRight => ui.panel.move_by(MOVE_STEP, 0.0),
                HotkeyAction::MoveUp => ui.panel.move_by(0.0, MOVE_STEP),
                HotkeyAction::MoveDown => ui.panel.move_by(0.0, -MOVE_STEP),
                _ => {}
            }
        });
    }
}

/// Quit path: ask the engine to shut down, and terminate
/// when it confirms `MeetingState(Idle)` - [`apply_event`] does that - or
/// after [`QUIT_DEADLINE_SECS`] no matter what.
fn request_quit() {
    with_ui(|ui| {
        ui.model.request_quit();
        (ui.commands)(EngineCommand::Shutdown);
        if ui.quit_timer.is_none() {
            // SAFETY: the block takes the timer it fires from, ignores it,
            // and the timer is scheduled on this (main) run loop.
            let block = RcBlock::new(|_timer: NonNull<NSTimer>| {
                tracing::warn!("quit deadline reached, terminating");
                std::process::exit(0);
            });
            ui.quit_timer = Some(unsafe {
                NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                    QUIT_DEADLINE_SECS,
                    false,
                    &block,
                )
            });
        }
    });
}

/// Apply one UI event on the main thread: update the model, repaint the
/// parts that changed, sync the menu-bar item and the meeting-only hotkeys,
/// and terminate when the quit path is released.
fn apply_event(event: UiEvent) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(changes) = with_ui(|ui| {
        let changes = ui.model.apply(event);
        if changes.status || changes.ticker || changes.suggestion {
            // Spec: deltas scroll to the end unless interactive mode is on.
            ui.views.render(&ui.model, !ui.panel.is_interactive());
        }
        if changes.meeting {
            let running = ui.model.meeting() == MeetingState::Running;
            ui.status_item.set_meeting_running(running, mtm);
            ui.status_item
                .rebuild_menu(running, ui.overlay_visible, mtm);
        }
        if changes.hotkeys
            && let Some(manager) = ui.hotkey_manager.as_ref()
        {
            let meeting = ui.model.meeting_hotkeys_wanted();
            for result in
                sync_meeting_keys(manager, &ui.parsed_hotkeys, meeting, &mut ui.registered)
            {
                if let Err(err) = result {
                    tracing::warn!(%err, "meeting hotkey registration failed");
                }
            }
        }
        changes
    }) else {
        return;
    };
    if changes.terminate {
        tracing::info!("engine idle after shutdown, terminating");
        std::process::exit(0);
    }
}

/// Send a UI event to the overlay from any thread (the engine's seam).
/// Events posted before [`run`] builds the UI are dropped.
pub fn post(event: UiEvent) {
    DispatchQueue::main().exec_async(move || apply_event(event));
}

fn post_status(text: String) {
    post(UiEvent::Status {
        source: StatusSource::App,
        level: StatusLevel::Error,
        text,
    });
}

/// A hotkey press, delivered by the global monitor on its own thread:
/// filter to presses, map the id, and hop to the main thread.
fn on_hotkey_event(event: GlobalHotKeyEvent) {
    if event.state() != HotKeyState::Pressed {
        return;
    }
    let action = actions()
        .lock()
        .ok()
        .and_then(|map| map.get(&event.id()).copied());
    let Some(action) = action else { return };
    DispatchQueue::main().exec_async(move || handle_action(action));
}

/// [`run`] with the main-thread marker taken here, so the binary does not
/// need to link AppKit itself. Never returns in a healthy app.
pub fn run_on_main_thread(config: Config, commands: CommandSink) {
    let mtm = MainThreadMarker::new().expect("overlay::run_on_main_thread runs on the main thread");
    run(mtm, config, commands);
}

/// Build the panel, views, status item and hotkeys and run the AppKit
/// loop. Never returns in a healthy app.
pub fn run(mtm: MainThreadMarker, config: Config, commands: CommandSink) {
    let app = NSApplication::sharedApplication(mtm);
    // Menu-bar-only app: no Dock icon, no app menu (spec: Accessory policy).
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let panel = OverlayPanel::new(mtm);
    let window: Retained<NSWindow> = Retained::into_super(Retained::into_super(panel.clone()));
    let views = views::install(&window, mtm);
    panel.apply_capture_policy(config.overlay.hide_from_capture);
    panel.orderFrontRegardless();
    panel.place_default();

    let status_item = StatusItemController::new(
        mtm,
        false,
        true,
        {
            let commands = commands.clone();
            move || commands(EngineCommand::ToggleMeeting)
        },
        move || {
            DispatchQueue::main().exec_async(|| {
                handle_action(HotkeyAction::ToggleOverlay);
            })
        },
        move || {
            if MainThreadMarker::new().is_some() {
                request_quit();
            }
        },
    );

    let mut parsed_hotkeys = Vec::new();
    let mut hotkey_manager = None;
    let mut registered: Vec<(u32, HotkeyAction)> = Vec::new();
    match hotkeys::parse_all(&config.hotkeys) {
        Ok(parsed) => {
            let mut map = HashMap::new();
            for p in &parsed {
                map.insert(p.hotkey.id, p.action);
            }
            let _ = ACTION_BY_ID.set(Mutex::new(map));
            GlobalHotKeyEvent::set_event_handler(Some(on_hotkey_event));
            match GlobalHotKeyManager::new() {
                Ok(manager) => {
                    let (reg, errors) = register_always_on(&manager, &parsed);
                    for err in errors {
                        post_status(err);
                    }
                    registered = reg;
                    parsed_hotkeys = parsed;
                    hotkey_manager = Some(manager);
                }
                Err(err) => post_status(format!("global hotkeys unavailable: {err}")),
            }
        }
        Err(err) => post_status(err.to_string()),
    }

    UI.with(|cell| {
        *cell.borrow_mut() = Some(Ui {
            model: UiModel::default(),
            panel,
            views,
            status_item,
            commands,
            hotkey_manager,
            parsed_hotkeys,
            registered,
            overlay_visible: true,
            quit_timer: None,
        })
    });

    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Changes;
    use clueless_types::{SuggestionEnd, Utterance, UtteranceId};

    /// The [`apply_event`] path with no UI installed: engine events that
    /// arrive before `run()` builds the panel (or after it is gone) must be
    /// dropped quietly, never panic.
    #[test]
    fn apply_event_without_ui_is_dropped_quietly() {
        assert!(
            UI.with(|cell| cell.borrow().is_none()),
            "tests run before run() so no UI exists"
        );
        apply_event(UiEvent::SuggestionStart { id: 1 });
        apply_event(UiEvent::MeetingState(MeetingState::Running));
        apply_event(UiEvent::Status {
            source: StatusSource::App,
            level: StatusLevel::Error,
            text: "boom".into(),
        });
        // Still no UI, and the process survived the round trip.
        assert!(UI.with(|cell| cell.borrow().is_none()));
    }

    /// The reducer half of the wiring, driven through the same
    /// event -> apply -> Changes stream `apply_event` consumes: a realistic
    /// meeting sequence ends with the view data the panel would show and the
    /// hotkey and terminate signals the wiring acts on.
    #[test]
    fn event_stream_drives_model_and_signals() {
        let mut model = UiModel::default();
        let mut changes = Changes::NONE;
        changes.merge(model.apply(UiEvent::MeetingState(MeetingState::Running)));
        assert!(changes.hotkeys && changes.meeting);
        changes.merge(model.apply(UiEvent::TranscriptInterim {
            id: UtteranceId {
                speaker: clueless_types::Speaker::Them,
                seq: 1,
            },
            text: "hello".into(),
        }));
        changes.merge(model.apply(UiEvent::SuggestionStart { id: 1 }));
        changes.merge(model.apply(UiEvent::SuggestionDelta {
            id: 1,
            text: "Ask about".into(),
        }));
        changes.merge(model.apply(UiEvent::SuggestionEnd {
            id: 1,
            end: SuggestionEnd::Done,
        }));
        assert_eq!(model.suggestion_text(), "Ask about");
        assert_eq!(model.ticker_display().len(), 1);
        assert!(model.meeting_hotkeys_wanted());
        assert!(!changes.terminate);

        // Quit path: request_quit then Shutdown's confirmation releases it.
        model.request_quit();
        let mut changes = Changes::NONE;
        changes.merge(model.apply(UiEvent::MeetingState(MeetingState::Stopping)));
        assert!(!changes.terminate);
        changes.merge(model.apply(UiEvent::MeetingState(MeetingState::Idle)));
        assert!(changes.terminate);
        // And a committed utterance still reaches the ticker data the panel
        // renders from.
        changes = Changes::NONE;
        changes.merge(model.apply(UiEvent::TranscriptFinal(Utterance {
            id: UtteranceId {
                speaker: clueless_types::Speaker::Me,
                seq: 1,
            },
            t0_ms: 0,
            t1_ms: 1,
            text: "I will follow up".into(),
        })));
        assert!(changes.ticker);
        assert_eq!(
            model.ticker_display().last().unwrap().text,
            "I will follow up"
        );
    }
}
