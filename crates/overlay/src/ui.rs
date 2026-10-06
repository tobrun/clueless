//! The wiring: a thread-local [`Ui`] owning both windows (standard window
//! and hidden overlay panel), their view trees, the pure [`UiModel`] and the
//! [`Presentation`] that decides which window is on screen; the any-thread
//! [`post`] entry point; and [`run`], which builds everything and enters the
//! AppKit run loop (spec: AppKit objects live only on the main thread; other
//! threads send `UiEvent` values through the main dispatch queue).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

use block2::RcBlock;
use clueless_types::profile::AssistProfile;
use clueless_types::{
    CommandSink, Config, EngineCommand, MeetingState, StatusLevel, StatusSource, UiEvent,
};
use dispatch2::DispatchQueue;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::NSTimer;

use crate::app_menu;
use crate::hotkeys::{self, HotkeyAction, ParsedHotkey, register_always_on, sync_meeting_keys};
use crate::mode::{self, Presentation, UiMode};
use crate::model::{Changes, UiModel};
use crate::panel::{self, OverlayPanel};
use crate::status_item::{MenuCallbacks, StatusItemController};
use crate::views::{self, OverlayViews, ViewStyle};
use crate::window::StandardWindow;

/// Quit deadline after `Shutdown` (terminate on `MeetingState(Idle)` or
/// after 6 s at the latest; the rationale is in `docs/decisions.md`).
const QUIT_DEADLINE_SECS: f64 = 6.0;

/// Everything the main thread owns. Reachable only through [`with_ui`].
struct Ui {
    model: UiModel,
    /// The hidden-mode panel; ordered in only while [`UiMode::Hidden`].
    panel: Retained<OverlayPanel>,
    panel_views: OverlayViews,
    /// The standard-mode window; ordered in only while [`UiMode::Standard`]
    /// and visible. Owns the saved frame (spec D-frame-store).
    main: Retained<StandardWindow>,
    window_views: OverlayViews,
    status_item: StatusItemController,
    commands: CommandSink,
    hotkey_manager: Option<GlobalHotKeyManager>,
    parsed_hotkeys: Vec<ParsedHotkey>,
    /// (id, action) of the hotkeys currently registered.
    registered: Vec<(u32, HotkeyAction)>,
    presentation: Presentation,
    quit_timer: Option<Retained<NSTimer>>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.borrow_mut().as_mut().map(f))
}

impl Ui {
    /// One move-key press applied to the window of the active mode,
    /// followed by the shared-frame write-back (spec D-move-keys).
    fn move_active(&mut self, dx: f64, dy: f64) {
        if self.presentation.mode == UiMode::Standard {
            self.main.move_by(dx, dy);
            return;
        }
        self.panel.move_by(dx, dy);
        self.mirror_after_move();
    }

    /// Hidden mode mirrors the panel's moved frame back into the standard
    /// window, which saves it (spec D-frame-store: the frame is the single
    /// source of truth for where the UI is).
    fn mirror_after_move(&mut self) {
        let mirrored =
            mode::mirrored_move_target(self.presentation.mode, self.panel.content_frame());
        if let Some(frame) = mirrored {
            self.main.set_content_frame(frame);
        }
    }
}

/// Hotkey id -> action for every parsed hotkey; only registered keys fire,
/// so meeting-only keys are naturally filtered by registration state.
/// The handler runs on the hotkey monitor's thread, outside the reach of
/// the thread-local [`Ui`], hence a process-wide map.
static ACTION_BY_ID: OnceLock<Mutex<HashMap<u32, HotkeyAction>>> = OnceLock::new();

fn actions() -> &'static Mutex<HashMap<u32, HotkeyAction>> {
    ACTION_BY_ID.get_or_init(Default::default)
}

/// Rebuild the status menu from the current model and presentation; every
/// menu change goes through here so the titles always come from
/// [`mode::menu_titles`] (spec D-mode-menu).
fn refresh_menu(ui: &Ui, mtm: MainThreadMarker) {
    let running = ui.model.meeting() == MeetingState::Running;
    let profile = ui.model.profile().unwrap_or_default();
    let source_errors: Vec<String> = ui
        .model
        .sources_down()
        .into_iter()
        .map(|(label, text)| format!("{label}: {text}"))
        .collect();
    ui.status_item
        .rebuild_menu(running, ui.presentation, profile, &source_errors, mtm);
}

/// Bring this app to the front for the standard window. Hotkey presses
/// arrive through a listen-only tap, so no user event reaches this app, and
/// macOS 26 logs `programmatic-activation-denied` for every app-level
/// activation attempt from that context (verified empirically: `activate`,
/// `activateIgnoringOtherApps:` and even AX `set frontmost` all fail to
/// steal the frontmost slot from the terminal). The activation call stays
/// as a best effort: on launches and clicks the OS does honour it. The
/// "in front" invariant itself is carried by `orderFrontRegardless` in
/// [`present_standard_window`], which raises the window above other apps'
/// windows without needing the app to be active.
#[allow(deprecated)]
fn force_activate(app: &NSApplication) {
    app.activateIgnoringOtherApps(true);
}

/// Put the standard window on screen in front of everything: key for text
/// and menu key equivalents, ordered front regardless of this app's active
/// state (spec D-mode-switch "in front"; see [`force_activate`] for why
/// app activation alone is not enough on macOS 26).
fn present_standard_window(ui: &Ui, app: &NSApplication) {
    ui.panel.orderOut(None);
    present_standard(&ui.main, app);
}

/// Order the standard window in front of everything and take key status;
/// shared by every path that shows it (see [`present_standard_window`] for
/// the full presentation, which also orders the panel out).
fn present_standard(main: &StandardWindow, app: &NSApplication) {
    main.makeKeyAndOrderFront(None);
    main.orderFrontRegardless();
    force_activate(app);
}

/// Order the active mode's window in and the other one out (spec D-mode-menu
/// invariants: at most one window on screen; the invisible mode's window is
/// ordered out).
fn show_active_window(ui: &Ui, mtm: MainThreadMarker) {
    match ui.presentation.mode {
        UiMode::Standard => {
            present_standard_window(ui, &NSApplication::sharedApplication(mtm));
        }
        UiMode::Hidden => {
            ui.main.orderOut(None);
            ui.panel.orderFrontRegardless();
        }
    }
}

/// Switch between the two windows. Into hidden mode: the panel copies the
/// standard window's content frame, fitted to the screen (spec
/// D-frame-mapping, D-fit), click-through is off again (spec
/// D-click-through-standard) and focus returns to the app that had it (spec
/// D-mode-switch; `deactivate` is verified by the Window modes manual check).
/// Into standard mode: the window returns at the frame it already owns.
fn switch_mode(ui: &mut Ui, mtm: MainThreadMarker) {
    ui.presentation.toggle_mode();
    let app = NSApplication::sharedApplication(mtm);
    match ui.presentation.mode {
        UiMode::Hidden => {
            ui.main.orderOut(None);
            ui.panel.set_interactive(false);
            let frame =
                mode::fit_frame(ui.main.content_frame(), panel::visible_frame_of(&ui.panel));
            ui.panel.set_frame(frame);
            ui.panel.orderFrontRegardless();
            // The panel is non-activating, so without this the app stays
            // active with no key window and keystrokes reach nobody.
            app.deactivate();
        }
        UiMode::Standard => {
            present_standard_window(ui, &app);
        }
    }
}

/// Apply an action on the main thread; the windows and `status_item` change
/// here, engine commands go out through the sink.
fn handle_action(action: HotkeyAction) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    with_ui(|ui| {
        let Some(command) = action.engine_command() else {
            apply_local_action(ui, action, mtm);
            return;
        };
        (ui.commands)(command);
    });
}

/// Apply a non-engine [`HotkeyAction`] to the windows and presentation
/// directly; the engine-command variants can never reach here because
/// [`handle_action`] sends them out through the sink first.
fn apply_local_action(ui: &mut Ui, action: HotkeyAction, mtm: MainThreadMarker) {
    match action {
        HotkeyAction::Suggest
        | HotkeyAction::ClearSuggestion
        | HotkeyAction::ToggleMeeting
        | HotkeyAction::CycleProfile => {
            unreachable!("engine commands exit through the sink above")
        }
        HotkeyAction::ToggleOverlay => {
            toggle_overlay(ui, mtm);
        }
        HotkeyAction::ToggleMode => {
            switch_mode(ui, mtm);
            refresh_menu(ui, mtm);
        }
        HotkeyAction::ToggleClickThrough => {
            toggle_click_through(ui);
        }
        HotkeyAction::MoveLeft
        | HotkeyAction::MoveRight
        | HotkeyAction::MoveUp
        | HotkeyAction::MoveDown => {
            let (dx, dy) = action.move_delta().expect("move actions carry a delta");
            ui.move_active(dx, dy);
        }
    }
}

/// Show or hide the active mode's window and refresh the menu titles to
/// match (spec D-toggle-visibility).
fn toggle_overlay(ui: &mut Ui, mtm: MainThreadMarker) {
    ui.presentation.toggle_visible();
    if ui.presentation.visible {
        show_active_window(ui, mtm);
    } else {
        ui.panel.orderOut(None);
        ui.main.orderOut(None);
    }
    refresh_menu(ui, mtm);
}

/// Flip the panel's click-through state (spec D-click-through-standard:
/// an overlay-only feature, the standard window always takes its clicks).
fn toggle_click_through(ui: &mut Ui) {
    if ui.presentation.click_through_allowed() {
        set_panel_interactive(ui, !ui.panel.is_interactive());
    }
}

/// Apply the interactive flag; becoming interactive also makes the panel
/// key so it can take keyboard focus (spec: interactive mode).
fn set_panel_interactive(ui: &mut Ui, interactive: bool) {
    ui.panel.set_interactive(interactive);
    if interactive {
        ui.panel.makeKeyAndOrderFront(None);
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

/// Repaint both view trees when a visible part of the model changed. Both
/// windows render: only one is on screen, but the hidden one must be current
/// when a switch brings it forward. The standard window always follows its
/// transcript (spec D-autoscroll-standard); the panel scrolls to the end only
/// while click-through is on, so a user reading history keeps their scroll
/// position in interactive mode (spec: interactive mode).
fn repaint_views(ui: &mut Ui, changes: &Changes) {
    if changes.repaints() {
        let scroll = !ui.panel.is_interactive();
        ui.panel_views.render(&ui.model, scroll);
        ui.window_views.render(&ui.model, true);
    }
}

/// Keep the menu-bar item in step with the meeting state and the capture
/// sources: its icon and its menu, whose titles depend on the mode as well
/// (spec D-mode-menu).
fn update_meeting_indicator(ui: &Ui, changes: &Changes, mtm: MainThreadMarker) {
    if changes.meeting || changes.status {
        ui.status_item.set_meeting_running(
            ui.model.meeting() == MeetingState::Running,
            !ui.model.sources_down().is_empty(),
            mtm,
        );
    }
    refresh_menu_if_needed(ui, changes, mtm);
}

fn refresh_menu_if_needed(ui: &Ui, changes: &Changes, mtm: MainThreadMarker) {
    if menu_needs_refresh(changes) {
        refresh_menu(ui, mtm);
    }
}

/// The menu titles depend on the meeting state and on the profile, and the
/// disabled source-error items on the statuses.
fn menu_needs_refresh(changes: &Changes) -> bool {
    changes.meeting || changes.profile || changes.status
}

/// Re-register the meeting-only hotkeys when the model says the wanted set
/// changed; a key that cannot be registered is logged, not fatal.
fn sync_hotkeys(ui: &mut Ui, changes: &Changes) {
    let Some(manager) = ui.hotkey_manager.as_ref().filter(|_| changes.hotkeys) else {
        return;
    };
    let meeting = ui.model.meeting_hotkeys_wanted();
    log_hotkey_errors(sync_meeting_keys(
        manager,
        &ui.parsed_hotkeys,
        meeting,
        &mut ui.registered,
    ));
}

/// Surface the meeting-key registrations that failed; a failed meeting-key
/// registration is not fatal (spec: the always-on keys keep working).
fn log_hotkey_errors(results: Vec<Result<HotkeyAction, String>>) {
    for err in results.into_iter().filter_map(Result::err) {
        tracing::warn!(%err, "meeting hotkey registration failed");
    }
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
        repaint_views(ui, &changes);
        update_meeting_indicator(ui, &changes, mtm);
        sync_hotkeys(ui, &changes);
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
    dispatch_action(action);
}

/// Hop a pressed action onto the main thread, where the [`Ui`] lives.
fn dispatch_action(action: HotkeyAction) {
    DispatchQueue::main().exec_async(move || handle_action(action));
}

/// [`run`] with the main-thread marker taken here, so the binary does not
/// need to link AppKit itself. Never returns in a healthy app.
pub fn run_on_main_thread(config: Config, commands: CommandSink) {
    let mtm = MainThreadMarker::new().expect("overlay::run_on_main_thread runs on the main thread");
    run(mtm, config, commands);
}

/// What [`run`] wires up for the global hotkeys: the manager (when global
/// hotkeys are available at all), everything parsed from the config, and the
/// keys currently registered.
#[derive(Default)]
struct HotkeyWiring {
    manager: Option<GlobalHotKeyManager>,
    parsed: Vec<ParsedHotkey>,
    registered: Vec<(u32, HotkeyAction)>,
}

/// The standard window's close handler: hide instead of close; the app keeps
/// running in the menu bar (spec D-close). The close button fires on the
/// main run loop.
fn on_main_window_close() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    with_ui(|ui| {
        ui.presentation.hide();
        ui.main.orderOut(None);
        refresh_menu(ui, mtm);
    });
}

/// Build the menu-bar item with its five callbacks. The meeting toggle goes
/// straight to the sink; the other three run on the main thread, where the
/// [`Ui`] lives.
fn install_status_item(mtm: MainThreadMarker, commands: CommandSink) -> StatusItemController {
    let toggle_commands = commands.clone();
    StatusItemController::new(
        mtm,
        false,
        Presentation::launch(),
        AssistProfile::default(),
        MenuCallbacks {
            toggle_meeting: Box::new(move || toggle_commands(EngineCommand::ToggleMeeting)),
            toggle_overlay: Box::new(post_toggle_overlay),
            toggle_mode: Box::new(post_toggle_mode),
            set_profile: Box::new(move |profile| commands(EngineCommand::SetProfile(profile))),
            quit: Box::new(quit_from_menu),
        },
    )
}

/// Menu Show/Hide item: hop the toggle onto the main thread.
fn post_toggle_overlay() {
    dispatch_action(HotkeyAction::ToggleOverlay);
}

/// Menu mode-switch item: hop the switch onto the main thread.
fn post_toggle_mode() {
    dispatch_action(HotkeyAction::ToggleMode);
}

/// Menu and `cmd+Q` quit: the deadline timer is a main-thread object, so
/// only proceed when running there.
fn quit_from_menu() {
    if MainThreadMarker::new().is_some() {
        request_quit();
    }
}

/// Parse every configured hotkey; a bad config string is reported through
/// the status line and yields no wiring (spec: bad hotkey shows a status
/// error, app keeps running).
fn parse_hotkeys(config: &Config) -> Option<Vec<ParsedHotkey>> {
    hotkeys::parse_all(&config.hotkeys)
        .inspect_err(|err| {
            tracing::warn!("{err}");
            post_status(err.to_string())
        })
        .ok()
}

/// Route presses by id, register the always-on keys and keep the manager
/// alive; per-key and manager-level failures surface as status errors, never
/// as a dead app.
fn wire_hotkeys(parsed: Vec<ParsedHotkey>) -> Option<HotkeyWiring> {
    let map = parsed.iter().map(|p| (p.hotkey.id, p.action)).collect();
    let _ = ACTION_BY_ID.set(Mutex::new(map));
    GlobalHotKeyEvent::set_event_handler(Some(on_hotkey_event));
    let manager = GlobalHotKeyManager::new()
        .map_err(|err| {
            let msg = format!("global hotkeys unavailable: {err}");
            tracing::warn!("{msg}");
            post_status(msg)
        })
        .ok()?;
    let (registered, errors) = register_always_on(&manager, &parsed);
    errors.into_iter().for_each(|err| {
        // The status line is the user-facing surface (spec: bad hotkey keeps
        // the app running), but leave a durable trace too, like the
        // meeting-key path does in `log_hotkey_errors`.
        tracing::warn!("{err}");
        post_status(err);
    });
    Some(HotkeyWiring {
        manager: Some(manager),
        parsed,
        registered,
    })
}

/// Build both windows, the status item, the main menu and the hotkeys, and
/// run the AppKit loop. Never returns in a healthy app.
pub fn run(mtm: MainThreadMarker, config: Config, commands: CommandSink) {
    let app = NSApplication::sharedApplication(mtm);
    // Menu-bar-only app: no Dock icon (spec: Accessory policy). The invisible
    // main menu installed below still carries the cmd key equivalents.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // The standard window first: its restored (or default) content frame
    // seeds the panel's frame (spec D-frame-mapping, D-default-frame).
    let main = StandardWindow::new(mtm);
    main.set_on_close(on_main_window_close);
    let main_size = main.content_frame();
    let window_views = views::install(&main, (main_size.2, main_size.3), ViewStyle::Window, mtm);

    let panel = OverlayPanel::new(
        mtm,
        mode::fit_frame(main.content_frame(), panel::visible_frame_of(&main)),
    );
    let panel_frame = panel.content_frame();
    let panel_views = views::install(&panel, (panel_frame.2, panel_frame.3), ViewStyle::Hud, mtm);
    // Capture hiding applies to the panel only (spec D-capture-scope).
    panel.apply_capture_policy(config.overlay.hide_from_capture);

    // Every launch starts standard and visible (spec D-launch-mode); the
    // panel has never been ordered in at this point, so the shared core of
    // `present_standard_window` is the whole story (no `Ui` exists yet).
    present_standard(&main, &app);

    let status_item = install_status_item(mtm, commands.clone());
    app_menu::install(mtm, status_item.handler());
    let wiring = parse_hotkeys(&config)
        .and_then(wire_hotkeys)
        .unwrap_or_default();

    UI.with(|cell| {
        *cell.borrow_mut() = Some(Ui {
            model: UiModel::default(),
            panel,
            panel_views,
            main,
            window_views,
            status_item,
            commands,
            hotkey_manager: wiring.manager,
            parsed_hotkeys: wiring.parsed,
            registered: wiring.registered,
            presentation: Presentation::launch(),
            quit_timer: None,
        })
    });

    app.run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn menu_refreshes_on_meeting_profile_or_status_changes() {
        let none = Changes::NONE;
        assert!(!menu_needs_refresh(&none));
        assert!(menu_needs_refresh(&Changes {
            meeting: true,
            ..Changes::NONE
        }));
        assert!(menu_needs_refresh(&Changes {
            profile: true,
            ..Changes::NONE
        }));
        assert!(menu_needs_refresh(&Changes {
            status: true,
            ..Changes::NONE
        }));
    }

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

    /// [`log_hotkey_errors`] passes the successful registrations and logs
    /// the failed ones without panicking (the app keeps running either way).
    #[test]
    fn log_hotkey_errors_accepts_ok_and_err_results() {
        log_hotkey_errors(vec![
            Ok(HotkeyAction::Suggest),
            Err("hotkey cmd+shift+arrows (move_left) could not be registered".into()),
            Ok(HotkeyAction::MoveUp),
            Err("hotkey cmd+shift+space (clear) could not be registered".into()),
        ]);
    }

    /// [`parse_hotkeys`] over a real [`Config`]: the defaults yield the full
    /// parsed list, and one bad hotkey string yields `None` (the status
    /// post is a no-op while no UI exists, so this is testable headlessly).
    #[test]
    fn parse_hotkeys_returns_the_parsed_list_or_none_for_a_bad_config() {
        let parsed = parse_hotkeys(&Config::default()).expect("default hotkeys must parse");
        assert_eq!(parsed.len(), 11, "every hotkeys table entry is parsed");

        let mut broken = Config::default();
        broken.hotkeys.toggle_overlay = "cmd+Nope".into();
        assert!(
            parse_hotkeys(&broken).is_none(),
            "an unparseable hotkey yields no wiring"
        );
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
