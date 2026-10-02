//! Append-only transcript store with compression support.
//!
//! Every committed utterance gets a line id that only increases and survives
//! compression, so callers can track "everything since my last trigger" by
//! remembering one id.

use clueless_types::Utterance;

/// One committed utterance with its stable line id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedLine {
    pub id: u64,
    pub utterance: Utterance,
}

/// The committed transcript of one meeting: a summary block plus committed
/// lines in commit order.
#[derive(Debug, Clone)]
pub struct TranscriptStore {
    summary: Option<String>,
    lines: Vec<CommittedLine>,
    next_id: u64,
}

impl Default for TranscriptStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptStore {
    /// An empty store for a new meeting.
    pub fn new() -> Self {
        Self {
            summary: None,
            lines: Vec::new(),
            next_id: 1,
        }
    }

    /// Commit an utterance and return its line id. Ids only increase and are
    /// never reused, not across compression.
    pub fn push(&mut self, utterance: Utterance) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.lines.push(CommittedLine { id, utterance });
        id
    }

    /// The committed lines in commit order.
    pub fn lines(&self) -> &[CommittedLine] {
        &self.lines
    }

    /// The committed utterances with a line id strictly greater than
    /// `line_id`. With `line_id = 0` this is every committed utterance.
    pub fn utterances_after(&self, line_id: u64) -> Vec<&Utterance> {
        self.lines
            .iter()
            .filter(|line| line.id > line_id)
            .map(|line| &line.utterance)
            .collect()
    }

    /// The line id of the newest committed line, or 0 when nothing is
    /// committed. Callers store this at a trigger to ask later what arrived
    /// since.
    pub fn last_line_id(&self) -> u64 {
        self.lines.last().map_or(0, |line| line.id)
    }

    /// Replace the oldest `replaced_count` committed lines with a summary
    /// block. Surviving lines keep their ids; the first id handed out by a
    /// later `push` is never one that was already used.
    pub fn set_summary(&mut self, text: String, replaced_count: usize) {
        let count = replaced_count.min(self.lines.len());
        self.lines.drain(..count);
        self.summary = Some(text);
    }

    /// The summary block, when compression has produced one.
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// The number of committed lines (the summary block is not a line).
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// True when no line is committed.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clueless_types::{Speaker, UtteranceId};

    fn utterance(speaker: Speaker, seq: u64, text: &str) -> Utterance {
        Utterance {
            id: UtteranceId { speaker, seq },
            t0_ms: seq * 1000,
            t1_ms: seq * 1000 + 500,
            text: text.to_string(),
        }
    }

    #[test]
    fn line_ids_increase_and_survive_compression() {
        // Spec: "The store gives every committed line a line id that only
        // increases and survives compression; since the last trigger is
        // tracked by line id."
        let mut store = TranscriptStore::new();
        let id1 = store.push(utterance(Speaker::Them, 0, "first"));
        let trigger = store.push(utterance(Speaker::Me, 0, "trigger line"));
        let id3 = store.push(utterance(Speaker::Them, 1, "after trigger"));
        let id4 = store.push(utterance(Speaker::Them, 2, "still after"));
        assert_eq!((id1, trigger, id3, id4), (1, 2, 3, 4));

        // A trigger fired at `trigger`; compression now folds the oldest lines
        // (the two before and including the trigger line) into a summary.
        store.set_summary("earlier talk".to_string(), 2);

        let later = store.utterances_after(trigger);
        assert_eq!(
            later.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(),
            vec!["after trigger", "still after"],
        );

        // Ids are never reused: the next push continues past every id so far.
        let id5 = store.push(utterance(Speaker::Me, 1, "next"));
        assert!(id5 > id4);
    }

    #[test]
    fn set_summary_replaces_the_oldest_lines_and_keeps_the_rest_in_order() {
        // Spec scenario: set_summary(text, 10) on 20 lines leaves 10 lines
        // after the summary block, in order.
        let mut store = TranscriptStore::new();
        for i in 0..20 {
            store.push(utterance(Speaker::Me, i, &format!("line {i:02}")));
        }

        store.set_summary("summary of the first ten".to_string(), 10);

        assert_eq!(store.len(), 10);
        assert_eq!(store.summary(), Some("summary of the first ten"));
        let remaining: Vec<&str> = store
            .lines()
            .iter()
            .map(|line| line.utterance.text.as_str())
            .collect();
        let expected: Vec<String> = (10..20).map(|i| format!("line {i:02}")).collect();
        assert_eq!(remaining, expected);
        // The kept lines keep their original ids.
        assert_eq!(store.lines().first().map(|l| l.id), Some(11));
    }
}
