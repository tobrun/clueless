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

/// Build the content view for `window` at its current content `size` and
/// return the update handles.
pub fn install(
    window: &NSWindow,
    size: (f64, f64),
    style: ViewStyle,
    mtm: MainThreadMarker,
) -> OverlayViews {
    let (width, height) = size;
    let inner_width = width - 2.0 * MARGIN;

    let effect = NSVisualEffectView::new(mtm);
    effect.setFrame(rect(0.0, 0.0, width, height));
    effect.setMaterial(match style {
        ViewStyle::Hud => NSVisualEffectMaterial::HUDWindow,
        ViewStyle::Window => NSVisualEffectMaterial::WindowBackground,
    });
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    // Rounded card for the HUD style: layer-backed with a corner radius.
    // The layer is reached untyped because the workspace pins objc2-app-kit
    // without the `objc2-quartz-core` feature; CALayer always answers these
    // two setters. The window style stays square.
    effect.setWantsLayer(true);
    if style == ViewStyle::Hud {
        // SAFETY: `-layer` returns the CALayer created by wantsLayer;
        // setCornerRadius:/setMasksToBounds: take a CGFloat and a BOOL.
        unsafe {
            let layer: Option<Retained<AnyObject>> = msg_send![&effect, layer];
            if let Some(layer) = layer {
                let _: () = msg_send![&layer, setCornerRadius: CORNER_RADIUS];
                let _: () = msg_send![&layer, setMasksToBounds: true];
            }
        }
    }

    // Status line: 11 pt, top of the card.
    let status = objc2_app_kit::NSTextField::labelWithString(ns_string!(""), mtm);
    status.setFrame(rect(
        MARGIN,
        height - MARGIN - STATUS_HEIGHT,
        inner_width,
        STATUS_HEIGHT,
    ));
    status.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    status.setTextColor(Some(&NSColor::secondaryLabelColor()));
    status.setAutoresizingMask(
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewMinYMargin,
    );

    // Ticker: last three transcript lines, 12 pt secondary, bottom.
    let ticker_height = 3.0 * TICKER_LINE_HEIGHT;
    let ticker = objc2_app_kit::NSTextField::labelWithString(ns_string!(""), mtm);
    ticker.setFrame(rect(MARGIN, MARGIN, inner_width, ticker_height));
    ticker.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    ticker.setTextColor(Some(&NSColor::secondaryLabelColor()));
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
    ticker.setAutoresizingMask(
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewMaxYMargin,
    );

    // Suggestion: 14 pt in a scroll view between the status line and the
    // ticker.
    let scroll_y = MARGIN + ticker_height + MARGIN;
    let scroll_top = height - MARGIN - STATUS_HEIGHT - MARGIN;
    let scroll = NSScrollView::new(mtm);
    scroll.setFrame(rect(MARGIN, scroll_y, inner_width, scroll_top - scroll_y));
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
    text.setFrameSize(NSSize::new(inner_width, scroll_top - scroll_y));
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

impl OverlayViews {
    /// Repaint the three labels from the model. `scroll_to_end` follows the
    /// spec: deltas scroll to the end unless interactive mode is on.
    pub fn render(&mut self, model: &UiModel, scroll_to_end: bool) {
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

        let lines: Vec<String> = model
            .ticker_display()
            .iter()
            .map(|line| format!("{}: {}", speaker_label(line.id.speaker), line.text))
            .collect();
        self.ticker
            .setStringValue(&NSString::from_str(&lines.join("\n")));

        let suggestion = model.suggestion_text();
        if self.last_suggestion != suggestion {
            self.last_suggestion = suggestion.to_string();
            self.text.setString(&NSString::from_str(suggestion));
            if scroll_to_end {
                let range = objc2_foundation::NSRange::new(suggestion.len(), 0);
                self.text.scrollRangeToVisible(range);
            }
        }
    }
}
