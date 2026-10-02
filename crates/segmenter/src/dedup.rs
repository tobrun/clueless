//! Remove the duplicate words a forced cut with a carried overlap produces.
//!
//! When the segmenter forces a cut inside speech it carries 1.0 s of audio into
//! the next piece, so both transcriptions contain the words of the overlap.
//! [`strip_overlap`] drops from the start of the next text the longest word
//! suffix (up to 8 words) that repeats the end of the previous text.

/// Longest word overlap that is removed.
const MAX_OVERLAP_WORDS: usize = 8;

/// Returns `next_text` with its first `k` words removed, where `k` is the
/// longest `k <= 8` for which the last `k` normalized words of `prev_text`
/// equal the first `k` normalized words of `next_text`.
///
/// Normalization compares lowercased words with punctuation removed, except
/// apostrophes inside words. The surviving words keep their original casing.
pub fn strip_overlap(prev_text: &str, next_text: &str) -> String {
    let prev_words = normalize_words(prev_text);
    let next_raw: Vec<&str> = next_text.split_whitespace().collect();
    let next_words: Vec<String> = next_raw.iter().map(|w| normalize_word(w)).collect();

    let max_k = MAX_OVERLAP_WORDS
        .min(prev_words.len())
        .min(next_words.len());
    for k in (1..=max_k).rev() {
        let prev_suffix = &prev_words[prev_words.len() - k..];
        let next_prefix = &next_words[..k];
        // a word that normalizes to nothing (pure punctuation) never matches
        if next_prefix.iter().any(|w| w.is_empty()) {
            continue;
        }
        if prev_suffix == next_prefix {
            return next_raw[k..].join(" ");
        }
    }
    next_text.to_string()
}

fn normalize_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(normalize_word).collect()
}

/// Lowercase, drop punctuation except apostrophes that sit inside a word.
fn normalize_word(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if c == '\''
            && i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_alphanumeric()
            && chars[i + 1].is_alphanumeric()
        {
            out.push('\'');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_overlap_removes_repeated_suffix_words() {
        assert_eq!(
            strip_overlap("we should ship on friday", "on Friday if the tests pass"),
            "if the tests pass"
        );
    }

    #[test]
    fn strip_overlap_without_common_words_returns_next_unchanged() {
        assert_eq!(
            strip_overlap("we should ship on friday", "the build is green again"),
            "the build is green again"
        );
    }

    #[test]
    fn strip_overlap_removes_at_most_eight_words() {
        let prev = "one two three four five six seven eight nine ten";
        let next = "three four five six seven eight nine ten alpha beta";
        assert_eq!(strip_overlap(prev, next), "alpha beta");
    }

    #[test]
    fn strip_overlap_keeps_apostrophes_inside_words() {
        assert_eq!(
            strip_overlap("c'est l'heure", "L'heure de partir"),
            "de partir"
        );
    }

    #[test]
    fn strip_overlap_returns_empty_when_next_is_all_overlap() {
        assert_eq!(
            strip_overlap("all good things end", "All Good Things End"),
            ""
        );
    }

    #[test]
    fn strip_overlap_ignores_surrounding_punctuation_when_matching() {
        assert_eq!(
            strip_overlap("Let's ship it on Friday!", "on friday, we celebrate"),
            "we celebrate"
        );
    }
}
