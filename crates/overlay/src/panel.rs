//! The floating panel: an [`NSPanel`] subclass that is click-through by
//! default and only becomes key window in interactive mode, sits at the
//! status window level on every space, and moves clamped to the visible
//! frame. The rationale for the objc2 stack, the status-level
//! panel and capture hiding is in `docs/decisions.md`.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSStatusWindowLevel, NSWindowCollectionBehavior,
    NSWindowSharingType, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::model::clamp_origin;

/// Panel size in points (spec: 560 by 320).
pub const PANEL_SIZE: (f64, f64) = (560.0, 320.0);
/// Points below the menu bar for the default position (spec: 24).
pub const TOP_INSET: f64 = 24.0;

#[derive(Debug, Default)]
pub(crate) struct PanelIvars {
    /// Interactive mode: the panel may become key so its text view can be
    /// scrolled, selected and copied from.
    interactive: Cell<bool>,
}

define_class!(
    // SAFETY: the superclass NSPanel supports subclassing; ivars are plain
    // data reached only from the main thread (MainThreadOnly).
    #[unsafe(super = NSPanel)]
    #[thread_kind = MainThreadOnly]
    #[name = "CluelessOverlayPanel"]
    #[ivars = PanelIvars]
    /// Borderless non-activating panel. Borderless means no title bar chrome
    /// and, by AppKit rules, no key window status; the `interactive` ivar
    /// lifts the key restriction only in interactive mode (spec: the default
    /// is click-through).
    pub(crate) struct OverlayPanel;

    impl OverlayPanel {
        // SAFETY: correct signatures for the NSWindow queries.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            self.ivars().interactive.get()
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }
    }
);

impl OverlayPanel {
    /// Create the configured panel, not yet ordered in. The content view is
    /// installed by the caller ([`crate::views::install`]).
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PanelIvars::default());
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(PANEL_SIZE.0, PANEL_SIZE.1),
        );
        // SAFETY: NSPanel's designated initialiser signature; the returned
        // object is always this class.
        let this: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false,
            ]
        };
        this.setLevel(NSStatusWindowLevel);
        this.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        this.setOpaque(false);
        this.setBackgroundColor(Some(&NSColor::clearColor()));
        this.setHasShadow(false);
        this.setHidesOnDeactivate(false);
        // Required when creating windows outside a window controller.
        // SAFETY: plain setter.
        unsafe {
            this.setReleasedWhenClosed(false);
            this.setIgnoresMouseEvents(true);
        }
        this
    }

    /// Turn interactive mode on or off: interactive means mouse events reach
    /// the panel and it may become key (spec: the click-through hotkey).
    pub fn set_interactive(&self, interactive: bool) {
        self.ivars().interactive.set(interactive);
        // SAFETY: plain setter; `-resignKey` is the NSWindow method taking
        // no arguments and returning void (its typed wrapper needs the
        // NSResponder feature, which the workspace pin leaves out).
        unsafe {
            self.setIgnoresMouseEvents(!interactive);
            if !interactive && self.isKeyWindow() {
                let _: () = msg_send![self, resignKey];
            }
        }
    }

    pub fn is_interactive(&self) -> bool {
        self.ivars().interactive.get()
    }

    /// The overlay must not appear in screen
    /// captures, driven by `overlay.hide_from_capture`.
    pub fn apply_capture_policy(&self, hide_from_capture: bool) {
        self.setSharingType(if hide_from_capture {
            NSWindowSharingType::None
        } else {
            NSWindowSharingType::ReadOnly
        });
    }

    /// Shift the panel by `(dx, dy)` points, kept inside the screen's
    /// visible frame (spec: move keys never push it off screen).
    pub fn move_by(&self, dx: f64, dy: f64) {
        let frame = self.frame();
        let target = (frame.origin.x + dx, frame.origin.y + dy);
        let origin = clamp_origin(target, PANEL_SIZE, visible_frame_of(self));
        self.setFrameOrigin(NSPoint::new(origin.0, origin.1));
    }

    /// Place the panel top-centre of its screen, [`TOP_INSET`] points below
    /// the menu bar (spec default position), clamped on screen.
    pub fn place_default(&self) {
        let visible = visible_frame_of(self);
        let raw = (
            visible.0 + (visible.2 - PANEL_SIZE.0) / 2.0,
            visible.1 + visible.3 - TOP_INSET - PANEL_SIZE.1,
        );
        let origin = clamp_origin(raw, PANEL_SIZE, visible);
        self.setFrameOrigin(NSPoint::new(origin.0, origin.1));
    }
}

/// The visible frame of the screen the panel is on, falling back to the main
/// screen and then to a 1440x900 safe default.
///
/// The workspace pins `objc2-app-kit` without the `NSScreen` feature (owned
/// by change set 1), so the screen is reached through untyped messaging on
/// the `NSScreen` class, which every macOS has.
pub fn visible_frame_of(panel: &OverlayPanel) -> (f64, f64, f64, f64) {
    // SAFETY: `-screen` returns the window's NSScreen or nil; `+[NSScreen
    // mainScreen]` and `-visibleFrame` are ancient stable APIs.
    let screen = unsafe {
        let screen: Option<Retained<AnyObject>> = msg_send![panel, screen];
        screen.or_else(|| msg_send![class!(NSScreen), mainScreen])
    };
    let Some(screen) = screen else {
        return (0.0, 0.0, 1440.0, 900.0);
    };
    // SAFETY: `-visibleFrame` returns an NSRect by value.
    let rect: NSRect = unsafe { msg_send![&screen, visibleFrame] };
    (
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_default_math_centers_and_tops_the_panel() {
        // Pure math of `place_default` on a 1440x900 screen.
        let visible = (0.0, 0.0, 1440.0, 900.0);
        let raw = (
            visible.0 + (visible.2 - PANEL_SIZE.0) / 2.0,
            visible.1 + visible.3 - TOP_INSET - PANEL_SIZE.1,
        );
        assert_eq!(clamp_origin(raw, PANEL_SIZE, visible), (440.0, 556.0));
    }
}
