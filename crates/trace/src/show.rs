//! The `--show` rendering of one trace: finals and suggestions in meeting
//! time order (D-inspect-tools).
//!
//! Finals print as `[mm:ss] Me: text` at their `t0_ms`, which is already
//! meeting time; each suggestion sits at its start's meeting time under a
//! `--- suggestion N (auto, interview) ---` line, with `(PASS)` printed
//! for a suppressed answer.

use crate::compare::{Outcome, suggestions};
use crate::reader::Trace;
use crate::record::{Body, Profile, Speaker, SuggestionOrigin};

/// The whole trace as transcript with suggestions, per the Outputs
/// section: everything in meeting time order.
pub fn show(trace: &Trace) -> String {
    // (meeting time, sort tie, lines): finals tie-break on their seq,
    // suggestions on one after it, so a final at the same millisecond as
    // a suggestion start prints first.
    let mut items: Vec<(u64, u64, String)> = Vec::new();
    for record in &trace.records {
        if let Body::TranscriptFinal {
            speaker,
            t0_ms,
            text,
            ..
        } = &record.body
        {
            items.push((
                *t0_ms,
                record.seq,
                format!("{} {}", clock_stamp(*t0_ms), final_line(*speaker, text)),
            ));
        }
    }
    for suggestion in suggestions(trace) {
        items.push((
            suggestion.meeting_ms,
            u64::MAX,
            suggestion_lines(&suggestion),
        ));
    }
    items.sort_by_key(|(ms, tie, _)| (*ms, *tie));
    let mut out = String::new();
    for (_, _, lines) in items {
        out.push_str(&lines);
        if !lines.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn final_line(speaker: Speaker, text: &str) -> String {
    let name = match speaker {
        Speaker::Me => "Me",
        Speaker::Them => "Them",
    };
    format!("{name}: {text}")
}

fn suggestion_lines(suggestion: &crate::compare::Suggestion) -> String {
    let origin = match suggestion.origin {
        SuggestionOrigin::Manual => "manual",
        SuggestionOrigin::Auto => "auto",
    };
    let mut header = format!("--- suggestion {} ({origin}", suggestion.suggestion);
    if let Some(profile) = suggestion.profile {
        header.push_str(&format!(", {}", profile_name(profile)));
    }
    header.push_str(") ---");
    let body = if suggestion.outcome == Outcome::Passed {
        "  (PASS)".to_string()
    } else if suggestion.shown_text.trim().is_empty() {
        String::new()
    } else {
        suggestion
            .shown_text
            .lines()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if body.is_empty() {
        header
    } else {
        format!("{header}\n{body}")
    }
}

fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::Manual => "manual",
        Profile::Interview => "interview",
        Profile::Brainstorm => "brainstorm",
    }
}

/// A final's `[mm:ss]` stamp; `t0_ms` is meeting time, so no conversion
/// applies.
pub(crate) fn clock_stamp(meeting_ms: u64) -> String {
    let secs = meeting_ms / 1000;
    format!("[{:02}:{:02}]", secs / 60, secs % 60)
}
