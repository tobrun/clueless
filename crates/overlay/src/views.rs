//! The view tree inside each window: an [`NSVisualEffectView`] holding the
//! status line, the transcript ticker and a scrollable suggestion text view.
//! Installed twice, once per window ([`crate::panel::OverlayPanel`] and
//! [`crate::window::StandardWindow`]), differing only in [`ViewStyle`]: the
//! panel keeps its rounded HUD card, the standard window gets the opaque
//! window-background material (spec D-standard-style). Plain AppKit widgets
//! with manual frames; all rendering flows from [`crate::model::UiModel`]
//! via [`crate::ui`].

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSColor, NSFont, NSScrollView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, ns_string};

use crate::model::{UiModel, speaker_label};

/// Corner radius of the panel background (spec: 12).
const CORNER_RADIUS: f64 = 12.0;
/// Margin from the panel edges to the content.
const MARGIN: f64 = 14.0;
/// Height reserved for the status line.
const STATUS_HEIGHT: f64 = 14.0;
/// One ticker line's height; the ticker shows three.
const TICKER_LINE_HEIGHT: f64 = 16.0;

/// How the shared view tree dresses the window it is installed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewStyle {
    /// Hidden overlay: HUD material, rounded 12 pt card.
    Hud,
    /// Standard window: window-background material, square corners; the
    /// opaque window and its shadow are window properties AppKit already
    /// gives a titled window (spec D-standard-style).
    Window,
}

/// Handles to the labels the UI layer updates.
pub struct OverlayViews {
    /// The content view; kept as the root handle of the tree.
    #[allow(dead_code)]
    pub effect: Retained<NSVisualEffectView>,
    pub status: Retained<objc2_app_kit::NSTextField>,
    pub ticker: Retained<objc2_app_kit::NSTextField>,
    /// Kept as the suggestion area's chrome; the text view does the work.
    #[allow(dead_code)]
    pub scroll: Retained<NSScrollView>,
    pub text: Retained<objc2_app_kit::NSTextView>,
    /// Last suggestion text rendered, so [`Self::render`] can skip work and
    /// decide when to scroll without converting the text view's string.
    last_suggestion: String,
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Plain frames of the view tree for one content size, computed by the pure
/// [`layout_for`] so the geometry is testable without AppKit.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    /// Width inside the side margins; every row's width.
    inner_width: f64,
    /// Full-size effect view.
    effect: NSRect,
    /// Status line at the top.
    status: NSRect,
    /// Three ticker lines at the bottom.
    ticker: NSRect,
    /// Suggestion scroll view between status and ticker.
    scroll: NSRect,
}

/// The manual-frame geometry of [`install`]: status line flush under the top
/// margin, three ticker lines above the bottom margin, suggestion area
/// between them, all `MARGIN` in from the sides.
fn layout_for(size: (f64, f64)) -> Layout {
    let (width, height) = size;
    let inner_width = width - 2.0 * MARGIN;
    let scroll_y = MARGIN + 3.0 * TICKER_LINE_HEIGHT + MARGIN;
    let scroll_top = height - MARGIN - STATUS_HEIGHT - MARGIN;
    Layout {
        inner_width,
        effect: rect(0.0, 0.0, width, height),
        status: rect(
            MARGIN,
            height - MARGIN - STATUS_HEIGHT,
            inner_width,
            STATUS_HEIGHT,
        ),
        ticker: rect(MARGIN, MARGIN, inner_width, 3.0 * TICKER_LINE_HEIGHT),
        scroll: rect(MARGIN, scroll_y, inner_width, scroll_top - scroll_y),
    }
}

/// Build the content view for `window` at its current content `size` and
/// return the update handles.
pub fn install(
    window: &NSWindow,
    size: (f64, f64),
    style: ViewStyle,
    mtm: MainThreadMarker,
) -> OverlayViews {
    let layout = layout_for(size);

    let effect = install_effect_view(layout.effect, style, mtm);
    let status = install_status_label(layout.status, mtm);
    let ticker = install_ticker_label(layout.ticker, mtm);
    let (scroll, text) = install_suggestion_view(layout.scroll, layout.inner_width, mtm);

    effect.addSubview(&status);
    effect.addSubview(&ticker);
    effect.addSubview(&scroll);
    window.setContentView(Some(&effect));

    OverlayViews {
        effect,
        status,
        ticker,
        scroll,
        text,
        last_suggestion: String::new(),
    }
}

/// The style's material behind the whole view tree (spec D-standard-style).
fn material_for(style: ViewStyle) -> NSVisualEffectMaterial {
    if style == ViewStyle::Hud {
        NSVisualEffectMaterial::HUDWindow
    } else {
        NSVisualEffectMaterial::WindowBackground
    }
}

/// Only the HUD style is the rounded card; the standard window stays square
/// (spec D-standard-style).
fn rounds_corners(style: ViewStyle) -> bool {
    style == ViewStyle::Hud
}

/// Root content view: the full-size effect view; the HUD style is the
/// rounded card, the window style stays square.
fn install_effect_view(
    frame: NSRect,
    style: ViewStyle,
    mtm: MainThreadMarker,
) -> Retained<NSVisualEffectView> {
    let effect = NSVisualEffectView::new(mtm);
    effect.setFrame(frame);
    effect.setMaterial(material_for(style));
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    effect.setWantsLayer(true);
    if rounds_corners(style) {
        round_hud_corners(&effect);
    }
    effect
}

/// Round the HUD card. The layer is reached untyped because the workspace
/// pins objc2-app-kit without the `objc2-quartz-core` feature; CALayer
/// always answers these two setters.
fn round_hud_corners(effect: &NSVisualEffectView) {
    // SAFETY: `-layer` returns the CALayer created by wantsLayer;
    // setCornerRadius:/setMasksToBounds: take a CGFloat and a BOOL.
    unsafe {
        let layer: Option<Retained<AnyObject>> = msg_send![effect, layer];
        if let Some(layer) = layer {
            let _: () = msg_send![&layer, setCornerRadius: CORNER_RADIUS];
            let _: () = msg_send![&layer, setMasksToBounds: true];
        }
    }
}

/// Shared label dressing: system font at `size` pt, secondary text color
/// and the given resize behavior.
fn dress_label(
    label: &objc2_app_kit::NSTextField,
    size: f64,
    autoresizing: objc2_app_kit::NSAutoresizingMaskOptions,
) {
    label.setFont(Some(&NSFont::systemFontOfSize(size)));
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label.setAutoresizingMask(autoresizing);
}

/// Status line: 11 pt, top of the card.
fn install_status_label(
    frame: NSRect,
    mtm: MainThreadMarker,
) -> Retained<objc2_app_kit::NSTextField> {
    let status = objc2_app_kit::NSTextField::labelWithString(ns_string!(""), mtm);
    status.setFrame(frame);
    dress_label(
        &status,
        11.0,
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewMinYMargin,
    );
    // One line that truncates its tail: the status line orders problems
    // first, so anything the width cuts off is the least important part.
    // Same untyped-cell pattern as `install_ticker_label`.
    // SAFETY: -cell returns the NSTextFieldCell; setLineBreakMode: takes an
    // NSInteger enum.
    unsafe {
        let cell: Option<Retained<AnyObject>> = msg_send![&status, cell];
        if let Some(cell) = cell {
            let _: () = msg_send![&cell, setLineBreakMode: 5isize];
        }
    }
    status
}

/// Ticker resize behavior: stretch with the card's width, keep the bottom
/// margin pinned (the height is fixed at three lines).
fn ticker_autoresizing() -> objc2_app_kit::NSAutoresizingMaskOptions {
    objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
        | objc2_app_kit::NSAutoresizingMaskOptions::ViewMaxYMargin
}

/// Ticker: last three transcript lines, 12 pt secondary, bottom.
fn install_ticker_label(
    frame: NSRect,
    mtm: MainThreadMarker,
) -> Retained<objc2_app_kit::NSTextField> {
    let ticker = objc2_app_kit::NSTextField::labelWithString(ns_string!(""), mtm);
    ticker.setFrame(frame);
    dress_label(&ticker, 12.0, ticker_autoresizing());
    ticker.setMaximumNumberOfLines(3);
    // Wrap instead of truncating. The NSLineBreakMode enum lives in the
    // NSParagraphStyle module, which is not in the enabled feature set, so
    // the cell takes the raw value: 0 is `NSLineBreakByWordWrapping`.
    // SAFETY: -cell returns the NSTextFieldCell; setLineBreakMode: takes an
    // NSInteger enum.
    unsafe {
        let cell: Option<Retained<AnyObject>> = msg_send![&ticker, cell];
        if let Some(cell) = cell {
            let _: () = msg_send![&cell, setLineBreakMode: 0isize];
        }
    }
    ticker
}

/// Suggestion: 14 pt in a scroll view at `frame`. Returns the scroll view
/// and the text view that does the work.
fn install_suggestion_view(
    frame: NSRect,
    inner_width: f64,
    mtm: MainThreadMarker,
) -> (Retained<NSScrollView>, Retained<objc2_app_kit::NSTextView>) {
    let scroll = NSScrollView::new(mtm);
    scroll.setFrame(frame);
    scroll.setHasVerticalScroller(true);
    scroll.setDrawsBackground(false);
    scroll.setAutoresizingMask(
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    let text = objc2_app_kit::NSTextView::new(mtm);
    text.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    text.setTextColor(Some(&NSColor::labelColor()));
    text.setEditable(false);
    text.setSelectable(true);
    text.setRichText(false);
    text.setVerticallyResizable(true);
    text.setHorizontallyResizable(false);
    text.setFrameSize(NSSize::new(inner_width, frame.size.height));
    // The view itself follows the clip view's width on window resizes, so
    // wrapped text re-wraps to the new width (spec D-resize).
    text.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
    // The container tracks the view width so the text wraps; the
    // `NSTextContainer` type is not in the enabled feature set, so this is
    // untyped messaging on the live container object.
    // SAFETY: NSTextView::new always creates a text container; these are
    // standard NSTextContainer setters.
    unsafe {
        let container: Option<Retained<AnyObject>> = msg_send![&text, textContainer];
        if let Some(container) = container {
            let _: () = msg_send![&container, setWidthTracksTextView: true];
            let container_size = NSSize::new(inner_width, f64::INFINITY);
            let _: () = msg_send![&container, setContainerSize: container_size];
        }
    }
    scroll.setDocumentView(Some(&text));

    (scroll, text)
}

impl OverlayViews {
    /// Repaint the three labels from the model. `scroll_to_end` follows the
    /// spec: deltas scroll to the end unless interactive mode is on.
    pub fn render(&mut self, model: &UiModel, scroll_to_end: bool) {
        self.render_status(model);
        self.render_ticker(model);
        self.render_suggestion(model, scroll_to_end);
    }

    /// Status line text, red while the worst level is an error.
    fn render_status(&mut self, model: &UiModel) {
        self.status
            .setStringValue(&NSString::from_str(&model.status_line()));
        let error = model
            .status_level()
            .is_some_and(|level| level == clueless_types::StatusLevel::Error);
        let color = if error {
            NSColor::systemRedColor()
        } else {
            NSColor::secondaryLabelColor()
        };
        self.status.setTextColor(Some(&color));
    }

    /// The last transcript lines, one per row with the speaker prefix.
    fn render_ticker(&mut self, model: &UiModel) {
        let lines: Vec<String> = model
            .ticker_display()
            .iter()
            .map(|line| format!("{}: {}", speaker_label(line.id.speaker), line.text))
            .collect();
        self.ticker
            .setStringValue(&NSString::from_str(&lines.join("\n")));
    }

    /// The suggestion text; untouched while the text is unchanged, so a
    /// delta only re-renders and (per `scroll_to_end`) follows the tail.
    fn render_suggestion(&mut self, model: &UiModel, scroll_to_end: bool) {
        let suggestion = model.suggestion_text();
        if self.last_suggestion != suggestion {
            self.text.setString(&NSString::from_str(&suggestion));
            Self::follow_suggestion_tail(&self.text, scroll_to_end, suggestion.len());
            self.last_suggestion = suggestion;
        }
    }

    fn follow_suggestion_tail(
        text: &objc2_app_kit::NSTextView,
        scroll_to_end: bool,
        suggestion_len: usize,
    ) {
        if scroll_to_end {
            let range = objc2_foundation::NSRange::new(suggestion_len, 0);
            text.scrollRangeToVisible(range);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The style -> material mapping (spec D-standard-style): HUD card gets
    /// the HUD material, the standard window the window background.
    #[test]
    fn each_style_gets_its_material() {
        assert_eq!(
            material_for(ViewStyle::Hud),
            NSVisualEffectMaterial::HUDWindow
        );
        assert_eq!(
            material_for(ViewStyle::Window),
            NSVisualEffectMaterial::WindowBackground
        );
    }

    /// Only the HUD style is the rounded card (spec D-standard-style).
    #[test]
    fn only_the_hud_style_rounds_its_corners() {
        assert!(rounds_corners(ViewStyle::Hud));
        assert!(!rounds_corners(ViewStyle::Window));
    }

    /// [`layout_for`] at 400 x 300, with the numbers hand-computed from the
    /// constants (MARGIN 14, STATUS_HEIGHT 14, TICKER_LINE_HEIGHT 16):
    /// inner width 400 - 28 = 372; status at y 300 - 14 - 14 = 272 with
    /// height 14; ticker at y 14 with height 3 * 16 = 48; scroll from
    /// y 14 + 48 + 14 = 76 up to 300 - 14 - 14 - 14 = 258, height 182.
    #[test]
    fn layout_at_400x300_is_the_hand_computed_frames() {
        let layout = layout_for((400.0, 300.0));
        assert_eq!(layout.inner_width, 372.0);
        assert_eq!(layout.effect, rect(0.0, 0.0, 400.0, 300.0));
        assert_eq!(layout.status, rect(14.0, 272.0, 372.0, 14.0));
        assert_eq!(layout.ticker, rect(14.0, 14.0, 372.0, 48.0));
        assert_eq!(layout.scroll, rect(14.0, 76.0, 372.0, 182.0));
    }

    /// A second size so constant swaps or sign flips cannot hide at one
    /// coincidental value. 320 x 240 by hand: inner 292; status y 212;
    /// scroll from 76 to 198, height 122.
    #[test]
    fn layout_at_320x240_is_the_hand_computed_frames() {
        let layout = layout_for((320.0, 240.0));
        assert_eq!(layout.inner_width, 292.0);
        assert_eq!(layout.effect, rect(0.0, 0.0, 320.0, 240.0));
        assert_eq!(layout.status, rect(14.0, 212.0, 292.0, 14.0));
        assert_eq!(layout.ticker, rect(14.0, 14.0, 292.0, 48.0));
        assert_eq!(layout.scroll, rect(14.0, 76.0, 292.0, 122.0));
    }

    /// The ticker stretches horizontally and keeps its bottom pinned: raw
    /// NSView autoresizing bits per NSView.h, NSWidthSizable = 1 << 1 and
    /// NSMaxYMargin = 1 << 5, and nothing else.
    #[test]
    fn ticker_autoresizing_sets_width_and_max_y_bits() {
        assert_eq!(ticker_autoresizing().bits(), (1 << 1) | (1 << 5));
    }
}
