//! The menu-bar item: a circle that fills while a meeting runs, a menu with
//! Start/Stop Meeting, Show/Hide Overlay and Quit, and the quit path that
//! waits for the engine's `MeetingState(Idle)` (with a 6 s deadline) so
//! capture streams close before exit (the rationale is in `docs/decisions.md`).

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

/// A menu callback kept in the handler's ivars.
type Action = Box<dyn Fn()>;

#[derive(Default)]
struct HandlerIvars {
    on_toggle_meeting: Option<Action>,
    on_toggle_overlay: Option<Action>,
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
    /// Target for the status item menu actions.
    struct MenuHandler;

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

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: Option<&objc2_app_kit::NSMenuItem>) {
            if let Some(action) = &self.ivars().on_quit {
                action();
            }
        }
    }
);

impl MenuHandler {
    fn new(
        mtm: MainThreadMarker,
        on_toggle_meeting: impl Fn() + 'static,
        on_toggle_overlay: impl Fn() + 'static,
        on_quit: impl Fn() + 'static,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(HandlerIvars {
            on_toggle_meeting: Some(Box::new(on_toggle_meeting)),
            on_toggle_overlay: Some(Box::new(on_toggle_overlay)),
            on_quit: Some(Box::new(on_quit)),
        });
        // SAFETY: NSObject's init signature.
        unsafe { msg_send![super(this), init] }
    }
}

const MEETING_RUNNING_TITLE: &str = "●";
const MEETING_IDLE_TITLE: &str = "○";

/// Owns the retained status item (spec: NSStatusItem kept retained - letting
/// it drop removes the item from the menu bar).
pub struct StatusItemController {
    item: Retained<NSStatusItem>,
    handler: Retained<MenuHandler>,
}

impl StatusItemController {
    /// Create the item and wire the three callbacks.
    pub fn new(
        mtm: MainThreadMarker,
        meeting_running: bool,
        overlay_visible: bool,
        on_toggle_meeting: impl Fn() + 'static,
        on_toggle_overlay: impl Fn() + 'static,
        on_quit: impl Fn() + 'static,
    ) -> Self {
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        let handler = MenuHandler::new(mtm, on_toggle_meeting, on_toggle_overlay, on_quit);
        let controller = Self { item, handler };
        controller.rebuild_menu(meeting_running, overlay_visible, mtm);
        controller.set_meeting_running(meeting_running, mtm);
        controller
    }

    /// Hollow circle when idle, filled circle while a meeting runs.
    pub fn set_meeting_running(&self, running: bool, mtm: MainThreadMarker) {
        let title = if running {
            MEETING_RUNNING_TITLE
        } else {
            MEETING_IDLE_TITLE
        };
        // The title lives on the item's button; `NSStatusItem::setTitle` is
        // deprecated in favour of exactly this.
        if let Some(button) = self.item.button(mtm) {
            button.setTitle(&NSString::from_str(title));
        }
    }

    /// Rebuild the menu after a meeting-state or visibility change so the
    /// item titles stay truthful.
    pub fn rebuild_menu(&self, running: bool, overlay_visible: bool, mtm: MainThreadMarker) {
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let handler = &*self.handler;

        let meeting_item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(if running {
                    "Stop Meeting"
                } else {
                    "Start Meeting"
                }),
                Some(sel!(toggleMeeting:)),
                &NSString::from_str(""),
            )
        };
        let overlay_item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(if overlay_visible {
                    "Hide Overlay"
                } else {
                    "Show Overlay"
                }),
                Some(sel!(toggleOverlay:)),
                &NSString::from_str(""),
            )
        };
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
            quit_item.setTarget(Some(handler));
        }
        self.item.setMenu(Some(&menu));
    }
}
