//! Printing the engine's events in the headless replay modes, shared by
//! `--replay` and the session re-run so both print exactly the same thing:
//! transcript lines and suggestions on stdout, statuses on stderr.

use clueless_types::events::{Speaker, StatusLevel, StatusSource, SuggestionEnd, Utterance};

/// One final as `[mm:ss] Me: text` on stdout.
pub fn print_final(final_: &Utterance) {
    println!(
        "{} {}: {}",
        clock_label(final_.t0_ms),
        speaker_label(final_.id.speaker),
        final_.text
    );
}

/// One status as `App Info: text` on stderr.
pub fn print_status(source: StatusSource, level: StatusLevel, text: &str) {
    eprintln!("{source:?} {level:?}: {text}");
}

pub fn speaker_label(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::Me => "Me",
        Speaker::Them => "Them",
    }
}

/// `t0_ms` as `[mm:ss]`, clamping minutes at 99 for absurd offsets.
pub fn clock_label(t0_ms: u64) -> String {
    let total_seconds = t0_ms / 1000;
    let minutes = (total_seconds / 60).min(99);
    format!("[{minutes:02}:{:02}]", total_seconds % 60)
}

/// Prints suggestion deltas and ends, with the "--- suggestion ---" header
/// once per answer.
#[derive(Default)]
pub struct SuggestionPrinter {
    /// The id of the answer whose header is out.
    header_for: Option<u64>,
}

impl SuggestionPrinter {
    pub fn delta(&mut self, id: u64, text: &str, out: &mut impl std::io::Write) {
        if self.header_for != Some(id) {
            let _ = writeln!(out, "--- suggestion ---");
            self.header_for = Some(id);
        }
        let _ = write!(out, "{text}");
    }

    pub fn end(
        &self,
        id: u64,
        end: &SuggestionEnd,
        out: &mut impl std::io::Write,
        err: &mut impl std::io::Write,
    ) {
        if self.header_for == Some(id) {
            let _ = writeln!(out);
        }
        if let SuggestionEnd::Failed(reason) = end {
            let _ = writeln!(err, "suggestion failed: {reason}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(buffer: Vec<u8>) -> String {
        String::from_utf8(buffer).expect("utf-8")
    }

    #[test]
    fn the_header_is_printed_once_per_answer_and_the_end_closes_the_line() {
        let mut printer = SuggestionPrinter::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        printer.delta(1, "a", &mut out);
        printer.delta(1, "b", &mut out);
        printer.end(1, &SuggestionEnd::Done, &mut out, &mut err);
        printer.delta(2, "c", &mut out);
        assert_eq!(text(out), "--- suggestion ---\nab\n--- suggestion ---\nc");
        assert!(err.is_empty());
    }

    #[test]
    fn a_failed_end_is_reported_on_stderr_and_an_unseen_id_prints_no_newline() {
        let printer = SuggestionPrinter::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        printer.end(
            7,
            &SuggestionEnd::Failed("boom".to_string()),
            &mut out,
            &mut err,
        );
        assert!(out.is_empty());
        assert_eq!(text(err), "suggestion failed: boom\n");
    }
}
