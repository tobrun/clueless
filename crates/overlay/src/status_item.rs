//! The menu-bar item: a circle that fills while a meeting runs, a menu with
//! Start/Stop Meeting, Show/Hide (window kind follows the active mode), the
//! mode switch, the three profiles and Quit, and the quit path that waits for the engine's
//! `MeetingState(Idle)` (with a 6 s deadline) so capture streams close
//! before exit (the rationale is in `docs/decisions.md`).

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use clueless_types::profile::AssistProfile;

use crate::mode::{self, Presentation};

/// A menu callback kept in the handler's ivars.
pub type Action = Box<dyn Fn()>;
/// The profile menu callback: receives the picked profile.
pub type ProfileAction = Box<dyn Fn(AssistProfile)>;

#[derive(Default)]
pub(crate) struct HandlerIvars {
    on_toggle_meeting: Option<Action>,
    on_toggle_overlay: Option<Action>,
    on_toggle_mode: Option<Action>,
    on_set_profile: Option<ProfileAction>,
    on_quit: Option<Action>,
}

define_class!(
    // SAFETY: superclass NSObject has no subclassing requirements; the ivars
    // hold boxed callbacks and the class is MainThreadOnly (the menu only
    // fires on the main run loop).
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "CluelessMenuHandler"]
    #[ivars = HandlerIvars]
    /// Target for the status item menu actions; also targeted by the app
    /// menu's Quit item (see [`crate::app_menu`]).
    pub(crate) struct MenuHandler;

    unsafe impl NSObjectProtocol for MenuHandler {}

    impl MenuHandler {
        // SAFETY: action signatures take an optional sender pointer.
        #[unsafe(method(toggleMeeting:))]
        fn toggle_meeting(&self, _sender: Option<&objc2_app_kit::NSMenuItem>) {
            if let Some(action) = &self.ivars().on_toggle_meeting {
                action();
            }
        }

        #[unsafe(method(toggleOverlay:))]
        fn toggle_overlay(&self, _sender: Option<&objc2_app_kit::NSMenuItem>) {
            if let Some(action) = &self.ivars().on_toggle_overlay {
                action();
            }
        }

        #[unsafe(method(toggleMode:))]
        fn toggle_mode(&self, _sender: Option<&objc2_app_kit::NSMenuItem>) {
            if let Some(action) = &self.ivars().on_toggle_mode {
                action();
            }
        }

        #[unsafe(method(setProfileManual:))]
        fn set_profile_manual(&self, _sender: Option<&objc2_app_kit::NSMenuItem>) {
            self.pick_profile(AssistProfile::Manual);
        }

        #[unsafe(method(setProfileInterview:))]
        fn set_profile_interview(&self, _sender: Option<&objc2_app_kit::NSMenuItem>) {
            self.pick_profile(AssistProfile::Interview);
        }

        #[unsafe(method(setProfileBrainstorm:))]
        fn set_profile_brainstorm(&self, _sender: Option<&objc2_app_kit::NSMenuItem>) {
            self.pick_profile(AssistProfile::Brainstorm);
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: Option<&objc2_app_kit::NSMenuItem>) {
            if let Some(action) = &self.ivars().on_quit {
                action();
            }
        }
    }
);

impl MenuHandler {
    fn pick_profile(&self, profile: AssistProfile) {
        if let Some(action) = &self.ivars().on_set_profile {
            action(profile);
        }
    }

    fn new(mtm: MainThreadMarker, callbacks: MenuCallbacks) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(HandlerIvars {
            on_toggle_meeting: Some(callbacks.toggle_meeting),
            on_toggle_overlay: Some(callbacks.toggle_overlay),
            on_toggle_mode: Some(callbacks.toggle_mode),
            on_set_profile: Some(callbacks.set_profile),
            on_quit: Some(callbacks.quit),
        });
        // SAFETY: NSObject's init signature.
        unsafe { msg_send![super(this), init] }
    }
}

/// The five menu callbacks the status item wires to its handler.
pub struct MenuCallbacks {
    pub toggle_meeting: Action,
    pub toggle_overlay: Action,
    pub toggle_mode: Action,
    pub set_profile: ProfileAction,
    pub quit: Action,
}

const MEETING_RUNNING_TITLE: &str = "●";
const MEETING_IDLE_TITLE: &str = "○";
/// Appended to the circle while a capture source is down, so a failure is
/// visible in the menu bar even with both windows hidden.
const SOURCE_DOWN_MARKER: &str = "!";

/// Owns the retained status item (spec: NSStatusItem kept retained - letting
/// it drop removes the item from the menu bar).
pub struct StatusItemController {
    item: Retained<NSStatusItem>,
    handler: Retained<MenuHandler>,
}

impl StatusItemController {
    /// Create the item and wire the five callbacks.
    pub fn new(
        mtm: MainThreadMarker,
        meeting_running: bool,
        presentation: Presentation,
        profile: AssistProfile,
        callbacks: MenuCallbacks,
    ) -> Self {
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        let handler = MenuHandler::new(mtm, callbacks);
        let controller = Self { item, handler };
        controller.rebuild_menu(meeting_running, presentation, profile, &[], mtm);
        controller.set_meeting_running(meeting_running, false, mtm);
        controller
    }

    /// The menu target, so the app menu can point its Quit item at the same
    /// handler (spec: `cmd+Q` goes through the graceful quit path).
    pub(crate) fn handler(&self) -> &Retained<MenuHandler> {
        &self.handler
    }

    /// Hollow circle when idle, filled circle while a meeting runs, plus a
    /// `!` marker while a capture source is down (spec: a failed source
    /// cannot be missed).
    pub fn set_meeting_running(&self, running: bool, source_down: bool, mtm: MainThreadMarker) {
        let base = if running {
            MEETING_RUNNING_TITLE
        } else {
            MEETING_IDLE_TITLE
        };
        let title = if source_down {
            format!("{base}{SOURCE_DOWN_MARKER}")
        } else {
            base.to_string()
        };
        // The title lives on the item's button; `NSStatusItem::setTitle` is
        // deprecated in favour of exactly this.
        if let Some(button) = self.item.button(mtm) {
            button.setTitle(&NSString::from_str(&title));
        }
    }

    /// Rebuild the menu after a meeting-state or presentation change so the
    /// item titles stay truthful (spec D-mode-menu: titles come from
    /// [`mode::menu_titles`]). `source_errors` holds the full text of every
    /// down capture source and lands as disabled items at the top, so the
    /// exact failure is readable without the windows.
    pub fn rebuild_menu(
        &self,
        running: bool,
        p: Presentation,
        profile: AssistProfile,
        source_errors: &[String],
        mtm: MainThreadMarker,
    ) {
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let handler = &*self.handler;
        for text in source_errors {
            let item = NSMenuItem::new(mtm);
            item.setTitle(&NSString::from_str(text));
            item.setEnabled(false);
            menu.addItem(&item);
        }
        if !source_errors.is_empty() {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
        }
        let (meeting_title, visibility_title, mode_title) = mode::menu_titles(running, p);

        let meeting_item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(meeting_title),
                Some(sel!(toggleMeeting:)),
                &NSString::from_str(""),
            )
        };
        let overlay_item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(visibility_title),
                Some(sel!(toggleOverlay:)),
                &NSString::from_str(""),
            )
        };
        let mode_item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(mode_title),
                Some(sel!(toggleMode:)),
                &NSString::from_str(""),
            )
        };
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let selectors = [
            sel!(setProfileManual:),
            sel!(setProfileInterview:),
            sel!(setProfileBrainstorm:),
        ];
        let profile_items: Vec<_> = mode::profile_titles(profile)
            .iter()
            .zip(selectors)
            .map(|(title, selector)| unsafe {
                menu.addItemWithTitle_action_keyEquivalent(
                    &NSString::from_str(title),
                    Some(selector),
                    &NSString::from_str(""),
                )
            })
            .collect();
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let quit_item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str("Quit"),
                Some(sel!(quitApp:)),
                &NSString::from_str(""),
            )
        };
        // SAFETY: the handler lives as long as the controller; AppKit menu
        // targets are weak, and this one is retained by self.handler.
        unsafe {
            meeting_item.setTarget(Some(handler));
            overlay_item.setTarget(Some(handler));
            mode_item.setTarget(Some(handler));
            for item in &profile_items {
                item.setTarget(Some(handler));
            }
            quit_item.setTarget(Some(handler));
        }
        self.item.setMenu(Some(&menu));
    }
}
