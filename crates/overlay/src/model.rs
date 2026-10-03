//! Pure overlay state: the `UiModel` reducer over [`UiEvent`]s and the
//! panel placement math. No AppKit types here - this is the unit-testable
//! seam between the engine event stream and the views rendered on screen.

use clueless_types::profile::AssistProfile;
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
    pub profile: bool,
}

impl Changes {
    pub const NONE: Changes = Changes {
        status: false,
        ticker: false,
        suggestion: false,
        meeting: false,
        terminate: false,
        hotkeys: false,
        profile: false,
    };

    pub fn merge(&mut self, other: Changes) {
        self.status |= other.status;
        self.ticker |= other.ticker;
        self.suggestion |= other.suggestion;
        self.meeting |= other.meeting;
        self.terminate |= other.terminate;
        self.hotkeys |= other.hotkeys;
        self.profile |= other.profile;
    }

    /// Whether any rendered text part changed: only these three force a
    /// repaint; meeting, hotkey and terminate signals never touch pixels.
    pub fn repaints(&self) -> bool {
        self.status || self.ticker || self.suggestion
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
    /// The feed: one `(suggestion id, text)` entry per answer that showed
    /// text, oldest first, at most [`FEED_KEEP`] entries.
    feed: Vec<(u64, String)>,
    /// Id of the latest `SuggestionStart`; deltas and ends for any other id
    /// are ignored (spec invariant C-suggestion-id). `None` after an end or
    /// `ClearSuggestion`, so late deltas of a finished suggestion drop.
    suggestion_id: Option<u64>,
    /// The active assist profile; `None` until the engine reports one.
    profile: Option<AssistProfile>,
    ticker: Vec<TickerLine>,
    /// Latest status per source, in first-seen order. `Error` and `Warn`
    /// stay until a newer status for the same source replaces them.
    status: Vec<(StatusSource, StatusLevel, String)>,
    quit_requested: bool,
}

/// Ticker lines kept (the view shows the last three; keep a little more so
/// the cap itself is invisible).
const TICKER_KEEP: usize = 12;
/// Feed entries kept; older answers drop so the whole-text repaint stays cheap.
pub const FEED_KEEP: usize = 30;
/// Lines the ticker view shows.
pub const TICKER_LINES: usize = 3;

impl Default for UiModel {
    fn default() -> Self {
        Self {
            meeting: MeetingState::Idle,
            feed: Vec::new(),
            suggestion_id: None,
            profile: None,
            ticker: Vec::new(),
            status: Vec::new(),
            quit_requested: false,
        }
    }
}

impl UiModel {
    fn apply_suggestion_delta(&mut self, id: u64, text: String) -> Changes {
        if self.suggestion_id != Some(id) {
            return Changes::NONE;
        }
        match self.feed.iter_mut().find(|(entry, _)| *entry == id) {
            Some((_, entry)) => entry.push_str(&text),
            None => {
                self.feed.push((id, text));
                if self.feed.len() > FEED_KEEP {
                    let excess = self.feed.len() - FEED_KEEP;
                    self.feed.drain(..excess);
                }
            }
        }
        Changes {
            suggestion: true,
            ..Changes::NONE
        }
    }

    fn apply_suggestion_end(&mut self, id: u64, end: clueless_types::SuggestionEnd) -> Changes {
        if self.suggestion_id != Some(id) {
            return Changes::NONE;
        }
        self.suggestion_id = None;
        let mut suggestion = false;
        match end {
            clueless_types::SuggestionEnd::Done => {}
            clueless_types::SuggestionEnd::Cancelled => {
                let before = self.feed.len();
                self.feed.retain(|(entry, _)| *entry != id);
                suggestion = self.feed.len() != before;
            }
            clueless_types::SuggestionEnd::Interrupted => {
                if let Some((_, entry)) = self.feed.iter_mut().find(|(e, _)| *e == id) {
                    entry.push_str("\n[interrupted]");
                    suggestion = true;
                }
            }
            // The reason goes to the status line, not into the feed.
            clueless_types::SuggestionEnd::Failed(_) => {}
        }
        Changes {
            status: true,
            suggestion,
            ..Changes::NONE
        }
    }

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
                if state == MeetingState::Starting && !self.feed.is_empty() {
                    self.feed.clear();
                    changes.suggestion = true;
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
                // Nothing on screen changes; the status line gains " ...".
                Changes {
                    status: true,
                    ..Changes::NONE
                }
            }
            UiEvent::SuggestionDelta { id, text } => self.apply_suggestion_delta(id, text),
            UiEvent::SuggestionEnd { id, end } => self.apply_suggestion_end(id, end),
            UiEvent::ClearSuggestion => {
                let was_active = self.suggestion_id.take().is_some();
                self.feed.clear();
                Changes {
                    status: was_active,
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
            UiEvent::Profile(profile) => {
                self.profile = Some(profile);
                Changes {
                    status: true,
                    profile: true,
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

    /// The feed text to render: entries joined by one blank line.
    pub fn suggestion_text(&self) -> String {
        self.feed
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Number of entries in the feed.
    pub fn feed_len(&self) -> usize {
        self.feed.len()
    }

    /// The active assist profile, once the engine has reported one.
    pub fn profile(&self) -> Option<AssistProfile> {
        self.profile
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

    /// The status line: the profile name (with " ..." while a request is
    /// running), then all per-source texts, joined by the separator.
    pub fn status_line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(profile) = self.profile {
            let working = if self.suggestion_id.is_some() {
                " ..."
            } else {
                ""
            };
            parts.push(format!("{}{working}", profile.name()));
        }
        parts.extend(self.status.iter().map(|(_, _, text)| text.clone()));
        parts.join("  |  ")
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

    fn answer(m: &mut UiModel, id: u64, text: &str) {
        m.apply(UiEvent::SuggestionStart { id });
        m.apply(UiEvent::SuggestionDelta {
            id,
            text: text.into(),
        });
        m.apply(UiEvent::SuggestionEnd {
            id,
            end: SuggestionEnd::Done,
        });
    }

    #[test]
    fn a_new_suggestion_keeps_the_previous_answer_in_the_feed() {
        let mut m = UiModel::default();
        answer(&mut m, 1, "a");
        m.apply(UiEvent::SuggestionStart { id: 2 });
        assert_eq!(m.suggestion_text(), "a", "start changes nothing on screen");
        m.apply(UiEvent::SuggestionDelta {
            id: 2,
            text: "b".into(),
        });
        assert_eq!(m.suggestion_text(), "a\n\nb");
    }

    #[test]
    fn an_answer_without_text_leaves_no_entry() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionEnd {
            id: 1,
            end: SuggestionEnd::Done,
        });
        assert_eq!(m.feed_len(), 0);
        assert_eq!(m.suggestion_text(), "");
    }

    #[test]
    fn cancelled_removes_the_entry() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionDelta {
            id: 1,
            text: "a".into(),
        });
        m.apply(UiEvent::SuggestionEnd {
            id: 1,
            end: SuggestionEnd::Cancelled,
        });
        assert_eq!(m.feed_len(), 0);
    }

    #[test]
    fn cancelled_removes_only_its_own_entry() {
        let mut m = UiModel::default();
        answer(&mut m, 1, "a");
        m.apply(UiEvent::SuggestionStart { id: 2 });
        m.apply(UiEvent::SuggestionDelta {
            id: 2,
            text: "b".into(),
        });
        m.apply(UiEvent::SuggestionEnd {
            id: 2,
            end: SuggestionEnd::Cancelled,
        });
        assert_eq!(m.suggestion_text(), "a");
    }

    #[test]
    fn interrupted_with_no_text_adds_nothing() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionEnd {
            id: 1,
            end: SuggestionEnd::Interrupted,
        });
        assert_eq!(m.feed_len(), 0);
        assert_eq!(m.suggestion_text(), "");
    }

    #[test]
    fn interrupted_keeps_the_partial_and_marks_it() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionDelta {
            id: 1,
            text: "a".into(),
        });
        m.apply(UiEvent::SuggestionEnd {
            id: 1,
            end: SuggestionEnd::Interrupted,
        });
        assert_eq!(m.suggestion_text(), "a\n[interrupted]");
    }

    #[test]
    fn failed_adds_nothing_to_the_feed() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        m.apply(UiEvent::SuggestionEnd {
            id: 1,
            end: SuggestionEnd::Failed("LLM offline".into()),
        });
        assert_eq!(m.feed_len(), 0);
        assert_eq!(m.suggestion_text(), "");
    }

    #[test]
    fn a_delta_for_an_id_that_is_not_active_is_ignored() {
        let mut m = UiModel::default();
        m.apply(UiEvent::SuggestionStart { id: 1 });
        let changes = m.apply(UiEvent::SuggestionDelta {
            id: 9,
            text: "x".into(),
        });
        assert_eq!(changes, Changes::NONE);
        assert_eq!(m.feed_len(), 0);
    }

    #[test]
    fn the_feed_keeps_the_newest_thirty_entries() {
        let mut m = UiModel::default();
        for id in 1..=31 {
            answer(&mut m, id, &format!("answer {id}"));
        }
        assert_eq!(m.feed_len(), 30);
        let text = m.suggestion_text();
        assert!(text.starts_with("answer 2\n\nanswer 3"), "{text:?}");
        assert!(text.ends_with("answer 31"));
    }

    #[test]
    fn clear_empties_the_feed_and_drops_late_deltas() {
        let mut m = UiModel::default();
        answer(&mut m, 1, "a");
        answer(&mut m, 2, "b");
        m.apply(UiEvent::SuggestionStart { id: 7 });
        m.apply(UiEvent::SuggestionDelta {
            id: 7,
            text: "partial".into(),
        });
        assert_eq!(m.feed_len(), 3);
        m.apply(UiEvent::ClearSuggestion);
        assert_eq!(m.suggestion_text(), "");
        m.apply(UiEvent::SuggestionDelta {
            id: 7,
            text: "late".into(),
        });
        assert_eq!(m.suggestion_text(), "");
    }

    #[test]
    fn a_meeting_start_empties_the_feed() {
        let mut m = UiModel::default();
        answer(&mut m, 1, "a");
        answer(&mut m, 2, "b");
        answer(&mut m, 3, "c");
        let changes = m.apply(UiEvent::MeetingState(MeetingState::Starting));
        assert_eq!(m.feed_len(), 0);
        assert!(changes.suggestion);
    }

    // --- profile in the status line ---

    #[test]
    fn the_status_line_starts_with_the_profile_name() {
        let mut m = UiModel::default();
        let changes = m.apply(UiEvent::Profile(AssistProfile::Interview));
        assert_eq!(m.status_line(), "Interview");
        assert!(changes.profile && changes.status);
    }

    #[test]
    fn the_status_line_shows_dots_while_a_request_runs() {
        let mut m = UiModel::default();
        m.apply(UiEvent::Profile(AssistProfile::Interview));
        m.apply(UiEvent::SuggestionStart { id: 1 });
        assert_eq!(m.status_line(), "Interview ...");
        m.apply(UiEvent::SuggestionEnd {
            id: 1,
            end: SuggestionEnd::Done,
        });
        assert_eq!(m.status_line(), "Interview");
    }

    #[test]
    fn the_profile_name_comes_before_the_source_statuses() {
        let mut m = UiModel::default();
        m.apply(UiEvent::Profile(AssistProfile::Brainstorm));
        m.apply(UiEvent::Status {
            source: StatusSource::Llm,
            level: StatusLevel::Info,
            text: "LLM ready".into(),
        });
        assert_eq!(m.status_line(), "Brainstorm  |  LLM ready");
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
    fn repaints_tracks_only_the_visible_parts() {
        assert!(!Changes::NONE.repaints());
        assert!(
            Changes {
                status: true,
                ..Changes::NONE
            }
            .repaints()
        );
        assert!(
            Changes {
                ticker: true,
                ..Changes::NONE
            }
            .repaints()
        );
        assert!(
            Changes {
                suggestion: true,
                ..Changes::NONE
            }
            .repaints()
        );
        // Signals that never touch pixels never force a repaint.
        assert!(
            !Changes {
                meeting: true,
                terminate: true,
                hotkeys: true,
                ..Changes::NONE
            }
            .repaints()
        );
    }

    /// The change report `apply` returns is what the UI repaints and
    /// re-registers from, so every event class must report its own part -
    /// dropping a field from any of `apply`'s literals must fail here.
    #[test]
    fn apply_reports_exactly_the_changed_parts() {
        // Assert the full report so deleting any field write in `apply`
        // fails: (status, ticker, suggestion, meeting, hotkeys, repaints).
        fn report(
            c: Changes,
            status: bool,
            ticker: bool,
            suggestion: bool,
            meeting: bool,
            hotkeys: bool,
        ) {
            assert_eq!(c.status, status);
            assert_eq!(c.ticker, ticker);
            assert_eq!(c.suggestion, suggestion);
            assert_eq!(c.meeting, meeting);
            assert_eq!(c.hotkeys, hotkeys);
            assert_eq!(c.repaints(), status || ticker || suggestion);
        }

        let mut m = UiModel::default();
        let c = m.apply(UiEvent::Status {
            source: StatusSource::App,
            level: StatusLevel::Info,
            text: "ready".into(),
        });
        report(c, true, false, false, false, false);

        let c = m.apply(UiEvent::TranscriptInterim {
            id: uid(Speaker::Me, 0),
            text: "hello".into(),
        });
        report(c, false, true, false, false, false);

        m.apply(UiEvent::SuggestionStart { id: 1 });
        // Start marks the status (" ..." appears) but not the feed.
        let c = m.apply(UiEvent::SuggestionStart { id: 2 });
        report(c, true, false, false, false, false);
        let c = m.apply(UiEvent::SuggestionDelta {
            id: 2,
            text: "try".into(),
        });
        report(c, false, false, true, false, false);
        // End marks the status and, when the cancel removed an entry, the feed.
        let c = m.apply(UiEvent::SuggestionEnd {
            id: 2,
            end: SuggestionEnd::Cancelled,
        });
        report(c, true, false, true, false, false);
        let c = m.apply(UiEvent::SuggestionEnd {
            id: 2,
            end: SuggestionEnd::Cancelled,
        });
        report(c, false, false, false, false, false);
        let c = m.apply(UiEvent::ClearSuggestion);
        report(c, false, false, true, false, false);

        let c = m.apply(UiEvent::Profile(AssistProfile::Brainstorm));
        report(c, true, false, false, false, false);
        assert!(c.profile);

        // Running registers the meeting keys, so the switch reports both;
        // a repeated Running changes nothing but `meeting` itself.
        let c = m.apply(UiEvent::MeetingState(MeetingState::Running));
        report(c, false, false, false, true, true);
        let c = m.apply(UiEvent::MeetingState(MeetingState::Running));
        report(c, false, false, false, true, false);

        // Ignored events report nothing; a drop that actually removes a
        // line reports the ticker.
        m.apply(UiEvent::TranscriptInterim {
            id: uid(Speaker::Me, 0),
            text: "hello".into(),
        });
        let c = m.apply(UiEvent::TranscriptDropped {
            id: uid(Speaker::Me, 0),
        });
        report(c, false, true, false, false, false);
        let c = m.apply(UiEvent::TranscriptDropped {
            id: uid(Speaker::Them, 99),
        });
        report(c, false, false, false, false, false);
        assert_eq!(m.apply(UiEvent::SourcesDrained), Changes::NONE);
    }

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
