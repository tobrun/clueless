//! Pure overlay state: the `UiModel` reducer over [`UiEvent`]s and the
//! panel placement math. No AppKit types here - this is the unit-testable
//! seam between the engine event stream and the views rendered on screen.

use clueless_types::{
    MeetingState, Speaker, StatusLevel, StatusSource, UiEvent, Utterance, UtteranceId,
};

/// Position a panel at `origin` (AppKit bottom-left coordinates), moving it
/// the smallest distance back inside `visible` `(x, y, width, height)` of the
/// screen's visible frame. An origin already inside the frame is unchanged;
/// a panel larger than the frame is pinned to its origin corner so the top
/// left stays on screen.
pub fn clamp_origin(
    origin: (f64, f64),
    size: (f64, f64),
    visible: (f64, f64, f64, f64),
) -> (f64, f64) {
    let (vx, vy, vw, vh) = visible;
    let (w, h) = size;
    // Largest legal origin per axis; `min` keeps oversized panels pinned to
    // the frame's own origin corner instead of mirroring them off-screen.
    let max_x = (vx + vw - w).max(vx);
    let max_y = (vy + vh - h).max(vy);
    (
        origin.0.clamp(vx.min(max_x), max_x),
        origin.1.clamp(vy.min(max_y), max_y),
    )
}

/// Parts of the UI a model update touched; the view layer only refreshes
/// what changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Changes {
    pub status: bool,
    pub ticker: bool,
    pub suggestion: bool,
    pub meeting: bool,
    pub terminate: bool,
    pub hotkeys: bool,
}

impl Changes {
    pub const NONE: Changes = Changes {
        status: false,
        ticker: false,
        suggestion: false,
        meeting: false,
        terminate: false,
        hotkeys: false,
    };

    pub fn merge(&mut self, other: Changes) {
        self.status |= other.status;
        self.ticker |= other.ticker;
        self.suggestion |= other.suggestion;
        self.meeting |= other.meeting;
        self.terminate |= other.terminate;
        self.hotkeys |= other.hotkeys;
    }
}

/// One entry of the ticker: the latest known text of one utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickerLine {
    pub id: UtteranceId,
    pub text: String,
}

/// Everything the overlay shows, derived only from the event stream.
#[derive(Debug, Clone, PartialEq)]
pub struct UiModel {
    meeting: MeetingState,
    suggestion_text: String,
    /// Id of the latest `SuggestionStart`; deltas and ends for any other id
    /// are ignored (spec invariant C-suggestion-id). `None` after an end or
    /// `ClearSuggestion`, so late deltas of a finished suggestion drop.
    suggestion_id: Option<u64>,
    ticker: Vec<TickerLine>,
    /// Latest status per source, in first-seen order. `Error` and `Warn`
    /// stay until a newer status for the same source replaces them.
    status: Vec<(StatusSource, StatusLevel, String)>,
    quit_requested: bool,
}

/// Ticker lines kept (the view shows the last three; keep a little more so
/// the cap itself is invisible).
const TICKER_KEEP: usize = 12;
/// Lines the ticker view shows.
pub const TICKER_LINES: usize = 3;

impl Default for UiModel {
    fn default() -> Self {
        Self {
            meeting: MeetingState::Idle,
            suggestion_text: String::new(),
            suggestion_id: None,
            ticker: Vec::new(),
            status: Vec::new(),
            quit_requested: false,
        }
    }
}

impl UiModel {
    /// Apply one engine event; report which UI parts changed. Events that
    /// contradict the current state (old suggestion ids, dropped ids that
    /// were never shown) are ignored, never a panic.
    pub fn apply(&mut self, event: UiEvent) -> Changes {
        match event {
            UiEvent::MeetingState(state) => {
                let wanted_before = self.meeting_hotkeys_wanted();
                self.meeting = state;
                let mut changes = Changes {
                    meeting: true,
                    ..Changes::NONE
                };
                if self.meeting_hotkeys_wanted() != wanted_before {
                    changes.hotkeys = true;
                }
                if self.terminate_now() {
                    changes.terminate = true;
                }
                changes
            }
            UiEvent::TranscriptInterim { id, text } => {
                self.upsert_ticker(id, text);
                Changes {
                    ticker: true,
                    ..Changes::NONE
                }
            }
            UiEvent::TranscriptFinal(Utterance { id, text, .. }) => {
                self.upsert_ticker(id, text);
                Changes {
                    ticker: true,
                    ..Changes::NONE
                }
            }
            UiEvent::TranscriptDropped { id } => {
                let before = self.ticker.len();
                self.ticker.retain(|line| line.id != id);
                Changes {
                    ticker: self.ticker.len() != before,
                    ..Changes::NONE
                }
            }
            UiEvent::SuggestionStart { id } => {
                self.suggestion_id = Some(id);
                self.suggestion_text.clear();
                Changes {
                    suggestion: true,
                    ..Changes::NONE
                }
            }
            UiEvent::SuggestionDelta { id, text } => {
                if self.suggestion_id != Some(id) {
                    return Changes::NONE;
                }
                self.suggestion_text.push_str(&text);
                Changes {
                    suggestion: true,
                    ..Changes::NONE
                }
            }
            UiEvent::SuggestionEnd { id, end } => {
                if self.suggestion_id != Some(id) {
                    return Changes::NONE;
                }
                self.suggestion_id = None;
                match end {
                    clueless_types::SuggestionEnd::Done => {}
                    clueless_types::SuggestionEnd::Cancelled => {}
                    clueless_types::SuggestionEnd::Interrupted => {
                        if !self.suggestion_text.ends_with("[interrupted]") {
                            if !self.suggestion_text.is_empty() {
                                self.suggestion_text.push('\n');
                            }
                            self.suggestion_text.push_str("[interrupted]");
                        }
                    }
                    clueless_types::SuggestionEnd::Failed(err) => {
                        if !self.suggestion_text.is_empty() {
                            self.suggestion_text.push('\n');
                        }
                        self.suggestion_text.push_str(&err);
                    }
                }
                Changes {
                    suggestion: true,
                    ..Changes::NONE
                }
            }
            UiEvent::ClearSuggestion => {
                self.suggestion_id = None;
                self.suggestion_text.clear();
                Changes {
                    suggestion: true,
                    ..Changes::NONE
                }
            }
            UiEvent::Status {
                source,
                level,
                text,
            } => {
                match self.status.iter_mut().find(|(s, _, _)| *s == source) {
                    Some(entry) => entry.1 = level,
                    None => self.status.push((source, level, String::new())),
                }
                // Replace the text after taking the slot so first-seen order
                // is stable.
                if let Some((_, _, slot)) = self.status.iter_mut().find(|(s, _, _)| *s == source) {
                    *slot = text;
                }
                Changes {
                    status: true,
                    ..Changes::NONE
                }
            }
            UiEvent::SourcesDrained => Changes::NONE,
        }
    }

    /// Ask the app to quit; `Changes::terminate` fires when the engine
    /// confirms `MeetingState(Idle)` (spec: capture streams must close
    /// before exit, the caller adds a 6 s deadline of its own).
    pub fn request_quit(&mut self) {
        self.quit_requested = true;
    }

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    /// True once a quit request exists and the engine reached `Idle`.
    pub fn terminate_now(&self) -> bool {
        self.quit_requested && self.meeting == MeetingState::Idle
    }

    /// Suggestion text to render.
    pub fn suggestion_text(&self) -> &str {
        &self.suggestion_text
    }

    /// Ticker lines, oldest first; the view shows the last [`TICKER_LINES`].
    pub fn ticker(&self) -> &[TickerLine] {
        &self.ticker
    }

    /// The last ticker lines to render, oldest first.
    pub fn ticker_display(&self) -> &[TickerLine] {
        let skip = self.ticker.len().saturating_sub(TICKER_LINES);
        &self.ticker[skip..]
    }

    /// Latest status per source, first-seen order.
    pub fn status(&self) -> &[(StatusSource, StatusLevel, String)] {
        &self.status
    }

    /// The status line: all per-source texts joined.
    pub fn status_line(&self) -> String {
        self.status
            .iter()
            .map(|(_, _, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("  |  ")
    }

    /// The worst level currently shown, for status line coloring.
    pub fn status_level(&self) -> Option<StatusLevel> {
        [StatusLevel::Error, StatusLevel::Warn, StatusLevel::Info]
            .into_iter()
            .find(|level| self.status.iter().any(|(_, l, _)| l == level))
    }

    pub fn meeting(&self) -> MeetingState {
        self.meeting
    }

    /// Meeting-only hotkeys (suggest, clear and the four move keys) are
    /// registered while a meeting runs (spec: registered on `Running`,
    /// unregistered on `Idle`).
    pub fn meeting_hotkeys_wanted(&self) -> bool {
        self.meeting == MeetingState::Running
    }

    fn upsert_ticker(&mut self, id: UtteranceId, text: String) {
        if text.trim().is_empty() {
            return;
        }
        match self.ticker.iter_mut().find(|line| line.id == id) {
            Some(line) => line.text = text,
            None => {
                self.ticker.push(TickerLine { id, text });
                if self.ticker.len() > TICKER_KEEP {
                    let excess = self.ticker.len() - TICKER_KEEP;
                    self.ticker.drain(..excess);
                }
            }
        }
    }
}

/// Convenience: the speaker prefix for a ticker line ("Me" / "Them").
pub fn speaker_label(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::Me => "Me",
        Speaker::Them => "Them",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clueless_types::SuggestionEnd;

    fn uid(speaker: Speaker, seq: u64) -> UtteranceId {
        UtteranceId { speaker, seq }
    }

    fn final_line(speaker: Speaker, seq: u64, text: &str) -> UiEvent {
        UiEvent::TranscriptFinal(Utterance {
            id: uid(speaker, seq),
            t0_ms: 0,
            t1_ms: 1,
            text: text.into(),
        })
    }

    // --- suggestion stream ---

    #[test]
    fn deltas_append_to_the_current_suggestion() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionDelta {
            id: 1,
            text: "a".into(),
        });
        let changes = m.apply(UiEvent::SuggestionDelta {
            id: 1,
            text: "b".into(),
        });
        assert_eq!(m.suggestion_text(), "ab");
        assert!(changes.suggestion);
    }

    #[test]
    fn deltas_of_an_old_suggestion_id_are_ignored() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionStart { id: 2 });
        m.apply(UiEvent::SuggestionDelta {
            id: 1,
            text: "x".into(),
        });
        assert_eq!(m.suggestion_text(), "");
    }

    #[test]
    fn a_new_suggestion_clears_the_previous_text() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionDelta {
            id: 1,
            text: "hello".into(),
        });
        assert_eq!(m.suggestion_text(), "hello");
        m.apply(UiEvent::SuggestionStart { id: 2 });
        assert_eq!(m.suggestion_text(), "");
    }

    #[test]
    fn interrupted_keeps_the_partial_and_marks_it() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionDelta {
            id: 1,
            text: "the answer is".into(),
        });
        m.apply(UiEvent::SuggestionEnd {
            id: 1,
            end: SuggestionEnd::Interrupted,
        });
        let text = m.suggestion_text();
        assert!(text.starts_with("the answer is"));
        assert!(
            text.lines().last() == Some("[interrupted]"),
            "text ends with the interrupted line: {text:?}"
        );
    }

    #[test]
    fn failed_shows_the_error_message() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionEnd {
            id: 1,
            end: SuggestionEnd::Failed("LLM offline".into()),
        });
        assert_eq!(m.suggestion_text(), "LLM offline");
    }

    #[test]
    fn clear_empties_the_text_and_drops_late_deltas() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 7 });
        m.apply(UiEvent::SuggestionDelta {
            id: 7,
            text: "partial".into(),
        });
        m.apply(UiEvent::ClearSuggestion);
        assert_eq!(m.suggestion_text(), "");
        m.apply(UiEvent::SuggestionDelta {
            id: 7,
            text: "late".into(),
        });
        assert_eq!(m.suggestion_text(), "");
    }

    // --- ticker ---

    #[test]
    fn interim_is_replaced_by_its_final() {
        let mut m = UiModel::default();
        m.apply(UiEvent::TranscriptInterim {
            id: uid(Speaker::Them, 1),
            text: "hel".into(),
        });
        m.apply(final_line(Speaker::Them, 1, "hello there"));
        let lines = m.ticker_display();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "hello there");
    }

    #[test]
    fn interim_is_removed_on_drop() {
        let mut m = UiModel::default();
        m.apply(UiEvent::TranscriptInterim {
            id: uid(Speaker::Them, 1),
            text: "hel".into(),
        });
        m.apply(UiEvent::TranscriptDropped {
            id: uid(Speaker::Them, 1),
        });
        assert!(m.ticker().is_empty());
    }

    #[test]
    fn ticker_shows_the_last_three_finals_in_order() {
        let mut m = UiModel::default();
        for seq in 1..=5 {
            m.apply(final_line(Speaker::Them, seq, &format!("line {seq}")));
        }
        let lines: Vec<&str> = m.ticker_display().iter().map(|l| l.text.as_str()).collect();
        assert_eq!(lines, ["line 3", "line 4", "line 5"]);
    }

    #[test]
    fn interims_of_both_speakers_are_separate_lines() {
        let mut m = UiModel::default();
        m.apply(UiEvent::TranscriptInterim {
            id: uid(Speaker::Me, 3),
            text: "I think".into(),
        });
        m.apply(UiEvent::TranscriptInterim {
            id: uid(Speaker::Them, 1),
            text: "I believe".into(),
        });
        let lines = m.ticker_display();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "I think");
        assert_eq!(lines[1].text, "I believe");
        assert_eq!(lines[0].id.speaker, Speaker::Me);
        assert_eq!(lines[1].id.speaker, Speaker::Them);
    }

    // --- status ---

    #[test]
    fn status_is_tracked_per_source() {
        let mut m = UiModel::default();
        m.apply(UiEvent::Status {
            source: StatusSource::Asr,
            level: StatusLevel::Error,
            text: "ASR server offline".into(),
        });
        m.apply(UiEvent::Status {
            source: StatusSource::Llm,
            level: StatusLevel::Info,
            text: "LLM ready".into(),
        });
        assert_eq!(m.status().len(), 2);
        assert_eq!(
            m.status()[0],
            (
                StatusSource::Asr,
                StatusLevel::Error,
                "ASR server offline".into()
            )
        );
        assert_eq!(m.status_level(), Some(StatusLevel::Error));
    }

    #[test]
    fn a_newer_status_for_the_same_source_replaces_the_error() {
        let mut m = UiModel::default();
        m.apply(UiEvent::Status {
            source: StatusSource::Asr,
            level: StatusLevel::Error,
            text: "ASR server offline".into(),
        });
        m.apply(UiEvent::Status {
            source: StatusSource::Asr,
            level: StatusLevel::Info,
            text: "ASR ready".into(),
        });
        assert_eq!(m.status().len(), 1);
        assert_eq!(m.status()[0].1, StatusLevel::Info);
        assert_eq!(m.status()[0].2, "ASR ready");
        assert_eq!(m.status_line(), "ASR ready");
    }

    // --- meeting state, hotkeys, quit ---

    #[test]
    fn meeting_hotkeys_follow_the_meeting_state() {
        let mut m = UiModel::default();
        assert!(!m.meeting_hotkeys_wanted());
        let changes = m.apply(UiEvent::MeetingState(MeetingState::Running));
        assert!(m.meeting_hotkeys_wanted());
        assert!(changes.hotkeys);
        let changes = m.apply(UiEvent::MeetingState(MeetingState::Idle));
        assert!(!m.meeting_hotkeys_wanted());
        assert!(changes.hotkeys);
    }

    #[test]
    fn quit_waits_for_the_idle_state() {
        let mut m = UiModel::default();
        m.apply(UiEvent::MeetingState(MeetingState::Running));
        m.request_quit();
        assert!(!m.terminate_now());
        // Stop passes through Stopping; only Idle releases the process.
        let changes = m.apply(UiEvent::MeetingState(MeetingState::Stopping));
        assert!(!changes.terminate);
        let changes = m.apply(UiEvent::MeetingState(MeetingState::Idle));
        assert!(changes.terminate);
        assert!(m.terminate_now());
    }

    // --- clamp_origin ---

    #[test]
    fn clamp_origin_pulls_a_panel_back_from_past_the_right_edge() {
        let visible = (0.0, 0.0, 1440.0, 900.0);
        let size = (560.0, 320.0);
        // Moved 40 points past the right edge.
        let origin = (1440.0 - 560.0 + 40.0, 500.0);
        let (x, y) = clamp_origin(origin, size, visible);
        assert_eq!(x, 1440.0 - 560.0);
        assert_eq!(y, 500.0);

        // 40 points below the bottom edge too (bottom-left origin).
        let (x, y) = clamp_origin((700.0, -40.0), size, visible);
        assert_eq!(y, 0.0);
        assert_eq!(x, 700.0);

        // A screen with a menu bar inset: top edge is the frame top.
        let inset = (0.0, 0.0, 1440.0, 870.0);
        let (x, y) = clamp_origin((400.0, 870.0 + 40.0), size, inset);
        assert_eq!(y, 870.0 - 320.0);
        assert_eq!(x, 400.0);
    }

    #[test]
    fn clamp_origin_leaves_an_inside_panel_unchanged() {
        let visible = (0.0, 0.0, 1440.0, 900.0);
        let size = (560.0, 320.0);
        assert_eq!(clamp_origin((100.0, 200.0), size, visible), (100.0, 200.0));
        // Exactly at the legal edge stays put.
        assert_eq!(clamp_origin((880.0, 580.0), size, visible), (880.0, 580.0));
    }
}
