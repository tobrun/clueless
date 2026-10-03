//! Pure UI-mode state: which of the two windows (standard window, hidden
//! overlay) is the active one, whether it is visible, and the geometry math
//! they share. No AppKit types here - this is the unit-testable seam the
//! window layer drives.

use crate::model::clamp_origin;

/// The two UI modes. Exactly one window is on screen at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    /// A standard resizable window, visible in screen shares.
    Standard,
    /// The borderless panel that is hidden from screen capture.
    Hidden,
}

/// Mode plus visibility of the active window. Every mode switch shows the
/// new mode's window (spec D-mode-switch: asking for a mode means wanting
/// to see it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presentation {
    pub mode: UiMode,
    pub visible: bool,
}

impl Presentation {
    /// Every launch starts in standard mode with the window visible
    /// (spec D-launch-mode).
    pub fn launch() -> Self {
        Self {
            mode: UiMode::Standard,
            visible: true,
        }
    }

    /// Switch to the other mode; the new mode's window is always shown,
    /// also when the UI was toggled hidden (spec D-mode-switch).
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            UiMode::Standard => UiMode::Hidden,
            UiMode::Hidden => UiMode::Standard,
        };
        self.visible = true;
    }

    /// Show or hide the active window (spec D-toggle-visibility).
    pub fn toggle_visible(&mut self) {
        self.visible = !self.visible;
    }

    /// Hide the active window; the app keeps running (spec D-close).
    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// Only the hidden overlay may be click-through; the standard window
    /// is a normal window and always takes its clicks.
    pub fn click_through_allowed(&self) -> bool {
        matches!(self.mode, UiMode::Hidden)
    }
}

/// A screen rectangle `(x, y, width, height)` in AppKit bottom-left
/// coordinates, as `clamp_origin` and the panel already use.
pub type Frame = (f64, f64, f64, f64);

/// Default content size, the size the fixed overlay panel had before
/// frames became movable (spec D-default-frame).
pub const DEFAULT_SIZE: (f64, f64) = (560.0, 320.0);

/// Points the default frame sits below the screen's visible top edge,
/// matching the panel's original placement.
pub const TOP_INSET: f64 = 24.0;

/// Smallest content size of the standard window; the status line, three
/// ticker lines and margins need 118 points of height (spec D-min-size).
pub const MIN_SIZE: (f64, f64) = (320.0, 200.0);

/// The first-launch content frame: [`DEFAULT_SIZE`] at the top centre of
/// the visible area, [`TOP_INSET`] below its top edge - the spot the
/// overlay used before frames were movable (spec D-default-frame).
pub fn default_content_frame(visible: Frame) -> Frame {
    let (vx, vy, vw, vh) = visible;
    let (w, h) = DEFAULT_SIZE;
    let raw = (vx + (vw - w) / 2.0, vy + vh - TOP_INSET - h);
    let (x, y) = clamp_origin(raw, (w, h), visible);
    (x, y, w, h)
}

/// Fit a stored frame to the screen: shrink it to the visible size if it
/// is larger, then pull the origin back inside with `clamp_origin`, so a
/// frame saved on a display that is now unplugged still lands on screen
/// (spec D-fit).
pub fn fit_frame(frame: Frame, visible: Frame) -> Frame {
    let (x, y, w, h) = frame;
    let (_, _, vw, vh) = visible;
    let (w, h) = (w.min(vw), h.min(vh));
    let (x, y) = clamp_origin((x, y), (w, h), visible);
    (x, y, w, h)
}

/// The three status-item menu titles for a meeting state and presentation:
/// the meeting toggle, the show/hide item naming the active window kind
/// (spec D-toggle-visibility), and the mode switch naming the destination
/// (spec D-mode-menu).
pub fn menu_titles(running: bool, p: Presentation) -> (&'static str, &'static str, &'static str) {
    let meeting = if running {
        "Stop Meeting"
    } else {
        "Start Meeting"
    };
    let visibility = match (p.mode, p.visible) {
        (UiMode::Standard, true) => "Hide Window",
        (UiMode::Standard, false) => "Show Window",
        (UiMode::Hidden, true) => "Hide Overlay",
        (UiMode::Hidden, false) => "Show Overlay",
    };
    let mode_switch = match p.mode {
        UiMode::Standard => "Switch to Hidden Overlay",
        UiMode::Hidden => "Switch to Standard Window",
    };
    (meeting, visibility, mode_switch)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Presentation ---

    #[test]
    fn every_launch_starts_standard_and_visible() {
        let p = Presentation::launch();
        assert_eq!(p.mode, UiMode::Standard);
        assert!(p.visible);
    }

    #[test]
    fn toggling_mode_from_standard_shows_the_hidden_overlay() {
        let mut p = Presentation::launch();
        p.toggle_mode();
        assert_eq!(p.mode, UiMode::Hidden);
        assert!(p.visible);
    }

    #[test]
    fn toggling_mode_while_hidden_shows_the_standard_window_even_after_hide() {
        let mut p = Presentation {
            mode: UiMode::Hidden,
            visible: false,
        };
        p.toggle_mode();
        assert_eq!(p.mode, UiMode::Standard);
        assert!(p.visible, "a switch that shows nothing looks broken");
    }

    #[test]
    fn toggling_visibility_keeps_the_mode_and_flips_visibility() {
        let mut p = Presentation {
            mode: UiMode::Hidden,
            visible: true,
        };
        p.toggle_visible();
        assert_eq!(p.mode, UiMode::Hidden);
        assert!(!p.visible);
    }

    #[test]
    fn hide_hides_the_active_window_and_keeps_the_mode() {
        let mut p = Presentation::launch();
        p.hide();
        assert_eq!(p.mode, UiMode::Standard);
        assert!(!p.visible);
    }

    #[test]
    fn only_the_hidden_overlay_may_be_click_through() {
        assert!(!Presentation::launch().click_through_allowed());
        let hidden = Presentation {
            mode: UiMode::Hidden,
            visible: true,
        };
        assert!(hidden.click_through_allowed());
    }

    // --- geometry ---

    #[test]
    fn the_default_content_frame_is_top_centre_with_the_legacy_size() {
        // The maths of the original `place_default` on a 1440x900 screen:
        // x = (1440-560)/2 = 440, y = 900-24-320 = 556.
        assert_eq!(
            default_content_frame((0.0, 0.0, 1440.0, 900.0)),
            (440.0, 556.0, 560.0, 320.0)
        );
    }

    #[test]
    fn fit_frame_leaves_a_frame_inside_the_screen_unchanged() {
        assert_eq!(
            fit_frame((100.0, 100.0, 560.0, 320.0), (0.0, 0.0, 1440.0, 900.0)),
            (100.0, 100.0, 560.0, 320.0)
        );
    }

    #[test]
    fn fit_frame_pulls_a_frame_past_the_right_edge_back_inside() {
        // 1200 + 560 > 1440: origin pinned to 1440 - 560 = 880.
        assert_eq!(
            fit_frame((1200.0, 100.0, 560.0, 320.0), (0.0, 0.0, 1440.0, 900.0)),
            (880.0, 100.0, 560.0, 320.0)
        );
    }

    #[test]
    fn fit_frame_recovers_a_frame_from_an_unplugged_display() {
        // A frame saved far off the current screen lands at the right edge.
        assert_eq!(
            fit_frame((3000.0, 100.0, 560.0, 320.0), (0.0, 0.0, 1440.0, 900.0)),
            (880.0, 100.0, 560.0, 320.0)
        );
    }

    #[test]
    fn fit_frame_shrinks_a_frame_larger_than_the_screen() {
        // 2000x1200 shrinks to the 1440x875 visible area (menu bar inset)
        // and the origin pins to the visible frame's own origin.
        assert_eq!(
            fit_frame((0.0, 0.0, 2000.0, 1200.0), (0.0, 25.0, 1440.0, 875.0)),
            (0.0, 25.0, 1440.0, 875.0)
        );
    }

    // --- menu titles ---

    #[test]
    fn menu_titles_in_standard_mode_name_the_window_and_the_hidden_destination() {
        assert_eq!(
            menu_titles(false, Presentation::launch()),
            ("Start Meeting", "Hide Window", "Switch to Hidden Overlay")
        );
    }

    #[test]
    fn menu_titles_in_hidden_mode_name_the_overlay_and_the_standard_destination() {
        let hidden_hidden = Presentation {
            mode: UiMode::Hidden,
            visible: false,
        };
        assert_eq!(
            menu_titles(true, hidden_hidden),
            ("Stop Meeting", "Show Overlay", "Switch to Standard Window")
        );
    }
}
