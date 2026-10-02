//! Token budget: the characters/3.5 estimate, the compression threshold, and
//! the summary request for the oldest half of the transcript (D-compression).

use crate::prompt::{PromptMessage, format_line, transcript_part};
use crate::store::TranscriptStore;

/// Above this many estimated transcript tokens the engine compresses.
pub const COMPRESS_THRESHOLD_TOKENS: usize = 90_000;

/// `max_tokens` for the compression request.
pub const COMPRESS_MAX_TOKENS: u32 = 1500;

/// The system message for the compression request.
pub const COMPRESS_RULES: &str = concat!(
    "You compress meeting transcripts. Write one short factual summary of the ",
    "given transcript part, keeping names, decisions, numbers and open ",
    "questions. Plain text, no preamble.",
);

/// Estimated token count: characters divided by 3.5, rounded up.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().saturating_mul(2).div_ceil(7)
}

/// True when the transcript part of the prompt exceeds
/// [`COMPRESS_THRESHOLD_TOKENS`] estimated tokens.
pub fn needs_compression(store: &TranscriptStore) -> bool {
    needs_compression_at(store, COMPRESS_THRESHOLD_TOKENS)
}

/// Same rule with an explicit threshold, for the engine's injected budget.
pub fn needs_compression_at(store: &TranscriptStore, threshold: usize) -> bool {
    estimate_tokens(&transcript_part(store)) > threshold
}

/// Build the messages of the one compression request: a summary of the oldest
/// half of the committed lines plus the existing summary block, so the new
/// summary can replace both.
///
/// The returned count is what a successful summary replaces via
/// [`TranscriptStore::set_summary`]. With fewer than two lines there is
/// nothing to fold and the message list is empty.
pub fn compression_request(store: &TranscriptStore) -> (Vec<PromptMessage>, usize) {
    let replaced_count = store.lines().len() / 2;
    if replaced_count == 0 {
        return (Vec::new(), 0);
    }

    let mut content = String::from("Summarize the transcript part below.");
    if let Some(summary) = store.summary() {
        content.push_str("\n\nEARLIER IN THIS MEETING (summary):\n");
        content.push_str(summary);
        content.push_str("\n\nLINES ADDED SINCE THAT SUMMARY:");
    }
    for line in &store.lines()[..replaced_count] {
        content.push('\n');
        content.push_str(&format_line(
            line.utterance.t0_ms,
            line.utterance.id.speaker,
            &line.utterance.text,
        ));
    }

    (
        vec![
            PromptMessage {
                role: "system".to_string(),
                content: COMPRESS_RULES.to_string(),
            },
            PromptMessage {
                role: "user".to_string(),
                content,
            },
        ],
        replaced_count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clueless_types::{Speaker, Utterance, UtteranceId};

    fn utterance(speaker: Speaker, seq: u64, text: &str) -> Utterance {
        Utterance {
            id: UtteranceId { speaker, seq },
            t0_ms: seq * 1000,
            t1_ms: seq * 1000 + 500,
            text: text.to_string(),
        }
    }

    #[test]
    fn estimate_tokens_divides_characters_by_three_point_five_rounding_up() {
        // Spec scenario: 350 characters estimate to 100 tokens (350 / 3.5 = 100).
        assert_eq!(estimate_tokens(&"x".repeat(350)), 100);
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("x"), 1);
        assert_eq!(estimate_tokens(&"x".repeat(7)), 2);
        assert_eq!(estimate_tokens(&"x".repeat(8)), 3);
        assert_eq!(estimate_tokens(&"x".repeat(351)), 101);
    }

    #[test]
    fn needs_compression_fires_when_the_transcript_part_passes_90000_tokens() {
        // 90000 tokens * 3.5 chars/token: exactly 315000 characters is at the
        // threshold, not over it.
        const CHARS_AT_THRESHOLD: usize = 90_000 * 7 / 2;

        let mut store = TranscriptStore::new();
        while transcript_part(&store).len() < CHARS_AT_THRESHOLD - 2000 {
            store.push(utterance(Speaker::Me, 0, &"x".repeat(1000)));
        }
        // Close the gap exactly with one final line: a line adds one newline,
        // the "[mm:ss] Me: " stamp of 12 characters, and the text.
        let gap = CHARS_AT_THRESHOLD - transcript_part(&store).len();
        store.push(utterance(Speaker::Me, 0, &"y".repeat(gap - 13)));
        assert_eq!(transcript_part(&store).len(), CHARS_AT_THRESHOLD);

        assert!(!needs_compression(&store));

        store.push(utterance(Speaker::Me, 0, "more"));
        assert!(needs_compression(&store));
    }

    #[test]
    fn compression_request_covers_the_oldest_half_plus_the_existing_summary() {
        let mut store = TranscriptStore::new();
        for i in 0..5 {
            store.push(utterance(Speaker::Them, i, &format!("l0{i}")));
        }
        store.set_summary("they compared release dates".to_string(), 5);
        for i in 6..27 {
            store.push(utterance(Speaker::Them, i, &format!("l{i:02}")));
        }
        assert_eq!(store.len(), 21);

        let (messages, replaced_count) = compression_request(&store);

        // The oldest half of 21 lines is the first 10.
        assert_eq!(replaced_count, 10);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");

        let content = &messages[1].content;
        assert!(content.contains("they compared release dates"));
        for i in 6..16 {
            assert!(content.contains(&format!("l{i:02}")), "missing l{i:02}");
        }
        for i in 16..27 {
            assert!(!content.contains(&format!("l{i:02}")), "leaked l{i:02}");
        }
    }
}
