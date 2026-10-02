//! Echo test: decide whether a "Me" final is really the other side's voice
//! coming back through the speakers (D-echo-filter).
//!
//! The caller holds a Me final, gathers the committed Them utterances plus the
//! Them text still in progress (appended as one more entry with the open
//! segment's time range), and calls [`is_echo`] once.

use clueless_types::Utterance;

/// True when `me` overlaps the `them` entries in time for at least 70 percent
/// of its own length and at least 60 percent of its words reappear, in order,
/// in the text of the overlapping entries.
pub fn is_echo(me: &Utterance, them: &[Utterance]) -> bool {
    let me_words = tokenize(&me.text);
    if me_words.is_empty() {
        return false;
    }
    let me_len = me.t1_ms.saturating_sub(me.t0_ms);
    if me_len == 0 {
        return false;
    }

    // Time rule: the shared time is measured over the union of the overlaps,
    // so several Them entries side by side add up.
    let mut overlaps: Vec<(u64, u64)> = them
        .iter()
        .filter_map(|t| {
            let start = me.t0_ms.max(t.t0_ms);
            let end = me.t1_ms.min(t.t1_ms);
            (end > start).then_some((start, end))
        })
        .collect();
    if overlaps.is_empty() {
        return false;
    }
    overlaps.sort_unstable();
    let mut shared = 0_u64;
    let (mut run_start, mut run_end) = overlaps[0];
    for &(start, end) in &overlaps[1..] {
        if start <= run_end {
            run_end = run_end.max(end);
        } else {
            shared += run_end - run_start;
            run_start = start;
            run_end = end;
        }
    }
    shared += run_end - run_start;
    if shared * 100 < me_len * 70 {
        return false;
    }

    // Word rule: longest common word subsequence with the text of the
    // overlapping entries, as a share of the Me word count.
    let them_words: Vec<String> = them
        .iter()
        .filter(|t| me.t1_ms > t.t0_ms && me.t0_ms < t.t1_ms)
        .flat_map(|t| tokenize(&t.text))
        .collect();
    let common = longest_common_subsequence(&me_words, &them_words);
    common * 100 >= me_words.len() * 60
}

/// Lowercase words with punctuation removed, in order.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split_whitespace()
        .map(|word| word.chars().filter(char::is_ascii_alphanumeric).collect())
        .filter(|word: &String| !word.is_empty())
        .collect()
}

/// Length of the longest common subsequence of two word lists.
fn longest_common_subsequence(a: &[String], b: &[String]) -> usize {
    let mut previous = vec![0_usize; b.len() + 1];
    let mut current = vec![0_usize; b.len() + 1];
    for ai in a {
        current[0] = 0;
        for (j, bj) in b.iter().enumerate() {
            current[j + 1] = if ai == bj {
                previous[j] + 1
            } else {
                current[j].max(previous[j + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use clueless_types::{Speaker, UtteranceId};

    fn utterance(speaker: Speaker, t0_ms: u64, t1_ms: u64, text: &str) -> Utterance {
        Utterance {
            id: UtteranceId { speaker, seq: 0 },
            t0_ms,
            t1_ms,
            text: text.to_string(),
        }
    }

    fn them(t0_ms: u64, t1_ms: u64, text: &str) -> Utterance {
        utterance(Speaker::Them, t0_ms, t1_ms, text)
    }

    #[test]
    fn same_words_over_the_same_time_are_echo() {
        // Me 1000-3000 "we can ship friday" vs Them 900-3100
        // "we can ship on friday": 100 % overlap, 4 of 4 words in order.
        let me = utterance(Speaker::Me, 1000, 3000, "we can ship friday");
        let them = [them(900, 3100, "we can ship on friday")];
        assert!(is_echo(&me, &them));
    }

    #[test]
    fn different_words_over_the_same_time_are_not_echo() {
        let me = utterance(Speaker::Me, 1000, 3000, "I disagree with that plan");
        let them = [them(900, 3100, "we can ship on friday")];
        assert!(!is_echo(&me, &them));
    }

    #[test]
    fn matching_words_with_only_40_percent_time_overlap_are_not_echo() {
        // Same words, but Them covers only 800 ms of Me's 2000 ms.
        let me = utterance(Speaker::Me, 1000, 3000, "we can ship friday");
        let them = [them(1800, 2600, "we can ship friday")];
        assert!(!is_echo(&me, &them));
    }

    #[test]
    fn overlap_spread_over_two_consecutive_them_utterances_can_be_echo() {
        // Two Them utterances side by side cover 1800 of Me's 2000 ms and
        // their joined text matches Me word for word.
        let me = utterance(Speaker::Me, 1000, 3000, "we can ship friday");
        let them = [
            them(900, 1900, "we can ship"),
            them(2000, 3100, "friday afternoon"),
        ];
        assert!(is_echo(&me, &them));
    }

    #[test]
    fn no_them_entries_is_not_echo() {
        let me = utterance(Speaker::Me, 1000, 3000, "we can ship friday");
        assert!(!is_echo(&me, &[]));
    }

    #[test]
    fn me_text_without_words_is_not_echo() {
        let me = utterance(Speaker::Me, 1000, 3000, "... !?");
        let them = [them(900, 3100, "... !?")];
        assert!(!is_echo(&me, &them));
    }
}
