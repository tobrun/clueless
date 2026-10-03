//! The standard-mode window: a titled, resizable [`NSWindow`] subclass that
//! is the single store of the shared frame through AppKit frame autosave
//! (spec D-frame-store: no file format of our own, no window delegate, no
//! save-on-quit hook). The close override turns the red button and `cmd+W`
//! into a hide (spec D-close). The style rationale is in
//! `docs/decisions.md` (D-standard-style, D-space).

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class};
use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, ns_string};

use crate::mode::{Frame, MIN_SIZE, default_content_frame};
use crate::model::clamp_origin;
use crate::panel::visible_frame_of;

/// Frame autosave name; the frame lives in the user defaults of the bundle
/// id under this name (spec D-frame-store, D-default-frame).
fn autosave_name() -> Retained<objc2_foundation::NSString> {
    NSString::from_str("CluelessMainWindow")
}

#[derive(Default)]
pub(crate) struct WindowIvars {
    /// Called instead of closing: hides the window through the same path as
    /// the show/hide action (spec D-close). Set once by [`crate::ui::run`]
    /// before the window is ever shown.
    on_close: RefCell<Option<Box<dyn Fn()>>>,
}

define_class!(
    // SAFETY: the superclass NSWindow supports subclassing; the ivar holds a
    // boxed callback reached only from the main thread (MainThreadOnly).
    #[unsafe(super = NSWindow)]
    #[thread_kind = MainThreadOnly]
    #[name = "CluelessMainWindow"]
    #[ivars = WindowIvars]
    /// Standard window: titled, closable, resizable, normal level, appears
    /// on the Space the user is on (spec D-standard-style, D-space). Never
    /// miniaturizable - a menu-bar-only app has no Dock icon to restore
    /// from. The sharing type is never touched: this window is always
    /// capturable (spec D-capture-scope).
    pub(crate) struct StandardWindow;

    impl StandardWindow {
        /// The red close button and `performClose:` (`cmd+W`) land here; the
        /// window is hidden instead of closed so the app keeps running in
        /// the menu bar (spec D-close).
        #[unsafe(method(close))]
        fn close(&self) {
            if let Some(on_close) = self.ivars().on_close.borrow().as_ref() {
                on_close();
            }
        }
    }
);

impl StandardWindow {
    /// Create the configured window, not yet ordered in. The content view is
    /// installed by the caller ([`crate::views::install`]). Restores the
    /// saved frame, or falls back to [`default_content_frame`] on the first
    /// launch (spec D-frame-store, D-default-frame).
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(WindowIvars::default());
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(crate::mode::DEFAULT_SIZE.0, crate::mode::DEFAULT_SIZE.1),
        );
        // SAFETY: NSWindow's designated initialiser signature; the returned
        // object is always this class.
        let this: Retained<Self> = unsafe {
            objc2::msg_send![
                super(this),
                initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Resizable,
                backing: NSBackingStoreType::Buffered,
                defer: false,
            ]
        };
        this.setTitle(ns_string!("clueless"));
        this.setCollectionBehavior(NSWindowCollectionBehavior::MoveToActiveSpace);
        this.setContentMinSize(NSSize::new(MIN_SIZE.0, MIN_SIZE.1));
        // Required when creating windows outside a window controller.
        // SAFETY: plain setter.
        unsafe {
            this.setReleasedWhenClosed(false);
        }
        // Autosave first, then try to restore: no saved frame (first launch,
        // or a frame AppKit rejected) falls back to the default frame
        // (spec: error handling "use the default frame").
        this.setFrameAutosaveName(&autosave_name());
        if !this.setFrameUsingName(&autosave_name()) {
            let default = default_content_frame(visible_frame_of(&this));
            set_content_frame_raw(&this, default);
        }
        this
    }

    /// Install the hide-instead-of-close callback.
    pub fn set_on_close(&self, on_close: impl Fn() + 'static) {
        *self.ivars().on_close.borrow_mut() = Some(Box::new(on_close));
    }

    /// The content area in screen coordinates - the frame both modes share
    /// (spec D-frame-mapping: the part below the title bar).
    pub fn content_frame(&self) -> Frame {
        let rect = self.contentRectForFrameRect(self.frame());
        (
            rect.origin.x,
            rect.origin.y,
            rect.size.width,
            rect.size.height,
        )
    }

    /// Move the content area to `frame` and persist it: `saveFrameUsingName`
    /// writes the change so hidden-mode moves survive the next launch
    /// (spec: invariant "after any move in hidden mode ... the frame has
    /// been saved").
    pub fn set_content_frame(&self, frame: Frame) {
        set_content_frame_raw(self, frame);
        self.saveFrameUsingName(&autosave_name());
    }

    /// Shift the content area by `(dx, dy)` points, clamped with the
    /// window's real size (spec D-move-keys), and persist the result.
    pub fn move_by(&self, dx: f64, dy: f64) {
        let (x, y, w, h) = self.content_frame();
        let origin = clamp_origin((x + dx, y + dy), (w, h), visible_frame_of(self));
        self.set_content_frame((origin.0, origin.1, w, h));
    }
}

/// Position the frame without saving: used before the window exists long
/// enough to own a saved frame (first-launch fallback).
fn set_content_frame_raw(window: &NSWindow, frame: Frame) {
    let (x, y, w, h) = frame;
    let content = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
    let frame_rect = window.frameRectForContentRect(content);
    window.setFrame_display(frame_rect, true);
}
