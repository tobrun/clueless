//! The diff report between two traces: transcript numbers, drop tables,
//! percentiles and paired suggestions (D-compare-metric).
//!
//! A baseline trace `a` and a candidate trace `b` (a re-run) are compared
//! on meeting time, so a re-run at speed 4 lines up with a live meeting
//! (D-timestamps). Suggestions pair per D-judge-pairing: manual by order,
//! automatic by nearest start within 10 s; only pairs where both sides
//! ended done with shown text are judgeable, a PASS is reported, not
//! judged. The verdicts themselves come from the judge (a later change
//! set) and are set onto [`Pair::verdict`] before [`Report::render`].

use std::collections::BTreeMap;
use std::path::Path;

use crate::reader::Trace;
use crate::record::{
    Body, DropReason, LlmOutcome, Profile, Purpose, Speaker, SuggestionOrigin, SuggestionOutcome,
};

/// Maximum number of transcript characters the judge context shows.
pub const JUDGE_CONTEXT_CHARS: usize = 6000;

/// Meeting milliseconds an automatic suggestion may drift from its pair
/// and still pair (D-judge-pairing).
const AUTO_PAIR_WINDOW_MS: i64 = 10_000;

/// Lowercase alphanumeric words, apostrophes kept: the same rules as the
/// replay helpers in `crates/app/tests/replay.rs`.
pub fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '\'')
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Word-level Levenshtein distance, the same rules as the helper of the
/// same name in `crates/app/tests/replay.rs`.
pub fn word_distance<T: AsRef<str>>(a: &[T], b: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, wa) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, wb) in b.iter().enumerate() {
            let cost = usize::from(wa.as_ref() != wb.as_ref());
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// How one side of a pair ended, folding the `suggestion_end` UI record
/// and the `llm_end` call facts together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Ended done with an answer.
    Done,
    /// The answer was a PASS, suppressed in the UI.
    Passed,
    /// The request failed or the suggestion was reported failed.
    Failed,
    /// Cancelled, superseded by a newer suggestion.
    Cancelled,
    /// Interrupted, for example by a meeting stop.
    Interrupted,
    /// The call ended with an error and no UI end arrived.
    Error,
    /// Started but never ended (a cut-off trace).
    Incomplete,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "cancelled",
            Outcome::Interrupted => "interrupted",
            Outcome::Error => "error",
            Outcome::Incomplete => "incomplete",
        }
    }

    /// True for outcomes counted as a failure.
    fn is_failure(self) -> bool {
        matches!(
            self,
            Outcome::Failed | Outcome::Cancelled | Outcome::Interrupted | Outcome::Error
        )
    }
}

/// One suggestion of one trace, as far as the pairing and the judge need
/// it: id, origin, meeting time, shown text and outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub suggestion: u64,
    pub origin: SuggestionOrigin,
    pub profile: Option<Profile>,
    /// Meeting milliseconds of its `suggestion_start`.
    pub meeting_ms: u64,
    pub shown_text: String,
    pub outcome: Outcome,
}

/// Which side of a pair a verdict names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    A,
    B,
}

/// The judge's opinion on one pair.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// One side is better, with the judge's reason.
    Winner(Side, String),
    /// Both orders judged a tie, or the orders disagreed.
    Tie(String),
    /// The pair could not be judged, with the reason.
    NotJudged(String),
}

/// One paired suggestion from each trace.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub a: Suggestion,
    pub b: Suggestion,
    /// True when both sides ended done with shown text; only judgeable
    /// pairs get a judge request.
    pub judgeable: bool,
    /// The judge's verdict, filled in by a later change set; `None`
    /// while the pair has not been judged.
    pub verdict: Option<Verdict>,
}

impl Pair {
    /// Why a pair is not judgeable, in the words the report prints; `None`
    /// when the pair is judgeable.
    pub fn skip_reason(&self) -> Option<String> {
        if self.judgeable {
            return None;
        }
        if self.b.outcome == Outcome::Passed {
            return Some("B passed".to_string());
        }
        if self.a.outcome == Outcome::Passed {
            return Some("A passed".to_string());
        }
        if self.a.outcome != Outcome::Done {
            return Some(format!("A ended {}", self.a.outcome.label()));
        }
        if self.b.outcome != Outcome::Done {
            return Some(format!("B ended {}", self.b.outcome.label()));
        }
        Some("no shown text".to_string())
    }
}

/// Per-speaker transcript numbers: finals, words and the distance with
/// its rate over the baseline's word count.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerReport {
    pub speaker: Speaker,
    pub a_finals: usize,
    pub a_words: usize,
    pub b_finals: usize,
    pub b_words: usize,
    pub distance: usize,
    /// `distance / a_words`, `None` when the baseline has no words for
    /// this speaker (printed as `n/a`).
    pub rate: Option<f64>,
}

/// One row of the drops table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropRow {
    pub reason: DropReason,
    pub a: usize,
    pub b: usize,
}

/// Median and 95th percentile of a list of durations by nearest rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Latency {
    pub count: usize,
    pub median_ms: Option<u64>,
    pub p95_ms: Option<u64>,
}

impl Latency {
    /// Percentiles of `values` by nearest rank: the value at rank
    /// `ceil(p * n / 100)` of the sorted list.
    pub fn of(values: &[u64]) -> Self {
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        Self {
            count: sorted.len(),
            median_ms: percentile(&sorted, 50),
            p95_ms: percentile(&sorted, 95),
        }
    }

    fn render(&self) -> String {
        match (self.median_ms, self.p95_ms) {
            (Some(median), Some(p95)) => {
                format!("{} calls, median {median}, p95 {p95}", self.count)
            }
            _ => "no calls".to_string(),
        }
    }
}

fn percentile(sorted: &[u64], percent: usize) -> Option<u64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    let rank = (percent * n).div_ceil(100); // at least 1
    sorted.get(rank - 1).copied()
}

/// Manual / automatic counts and PASS and failure counts of one trace's
/// suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub manual: usize,
    pub automatic: usize,
    pub passed: usize,
    pub failed: usize,
}

/// The whole report of one baseline against one candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Name or path of the baseline trace.
    pub baseline: String,
    /// Name or path of the candidate trace.
    pub candidate: String,
    pub transcripts: Vec<SpeakerReport>,
    pub drops: Vec<DropRow>,
    /// `asr_call` durations of final segments, `[a, b]`.
    pub asr_ms: [Latency; 2],
    /// `llm_end.first_content_ms` of suggestion calls, `[a, b]`.
    pub first_answer_ms: [Latency; 2],
    /// Suggestion counts, `[a, b]`.
    pub counts: [Counts; 2],
    pub pairs: Vec<Pair>,
    /// Suggestions only the baseline has.
    pub only_a: Vec<Suggestion>,
    /// Suggestions only the candidate has.
    pub only_b: Vec<Suggestion>,
}

/// Compare `a` (the baseline) with `b` (the candidate).
pub fn compare(a: &Trace, b: &Trace) -> Report {
    let suggestions = [suggestions(a), suggestions(b)];
    let (pairs, only_a, only_b) = pair_suggestions(&suggestions[0], &suggestions[1]);
    Report {
        baseline: dir_name(&a.dir),
        candidate: dir_name(&b.dir),
        transcripts: [Speaker::Me, Speaker::Them]
            .into_iter()
            .map(|speaker| speaker_report(a, b, speaker))
            .collect(),
        drops: drop_rows(a, b),
        asr_ms: [final_call_ms(a), final_call_ms(b)],
        first_answer_ms: [first_answer_ms(a), first_answer_ms(b)],
        counts: [counts(&suggestions[0]), counts(&suggestions[1])],
        pairs,
        only_a,
        only_b,
    }
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// The suggestions one trace recorded, in start order: each
/// `suggestion_start` with the facts of its `llm_request`, `llm_end` and
/// `suggestion_end`.
pub fn suggestions(trace: &Trace) -> Vec<Suggestion> {
    struct Request {
        call: u64,
        origin: SuggestionOrigin,
        profile: Option<Profile>,
    }
    struct End {
        outcome: LlmOutcome,
        passed: bool,
        shown_text: String,
    }
    let mut requests: BTreeMap<u64, Request> = BTreeMap::new();
    let mut ends: BTreeMap<u64, End> = BTreeMap::new();
    let mut ui_ends: BTreeMap<u64, SuggestionOutcome> = BTreeMap::new();
    let mut deltas: BTreeMap<u64, String> = BTreeMap::new();
    let mut starts = Vec::new();
    for record in &trace.records {
        match &record.body {
            Body::SuggestionStart { suggestion } => {
                let meeting_ms = trace.meeting_ms(record);
                starts.push((*suggestion, meeting_ms));
            }
            Body::LlmRequest {
                call,
                purpose: Purpose::Suggestion,
                suggestion: Some(suggestion),
                origin,
                profile,
                ..
            } => {
                requests.entry(*suggestion).or_insert(Request {
                    call: *call,
                    origin: origin.unwrap_or(SuggestionOrigin::Manual),
                    profile: *profile,
                });
            }
            Body::LlmDelta {
                call,
                channel: crate::record::Channel::Content,
                text,
            } => {
                deltas.entry(*call).or_default().push_str(text);
            }
            Body::LlmEnd {
                call,
                outcome,
                passed,
                shown_text,
                ..
            } => {
                ends.insert(
                    *call,
                    End {
                        outcome: *outcome,
                        passed: *passed,
                        shown_text: shown_text.clone(),
                    },
                );
            }
            Body::SuggestionEnd {
                suggestion, end, ..
            } => {
                ui_ends.insert(*suggestion, *end);
            }
            _ => {}
        }
    }
    starts
        .into_iter()
        .map(|(suggestion, meeting_ms)| {
            let request = requests.get(&suggestion);
            let end = request.and_then(|r| ends.get(&r.call));
            let shown_text = match end {
                Some(end) => end.shown_text.clone(),
                None => request
                    .and_then(|r| deltas.get(&r.call))
                    .cloned()
                    .unwrap_or_default(),
            };
            let outcome = match end {
                Some(end) if end.passed => Outcome::Passed,
                _ => match ui_ends.get(&suggestion) {
                    Some(outcome) => match outcome {
                        SuggestionOutcome::Done => Outcome::Done,
                        SuggestionOutcome::Cancelled => Outcome::Cancelled,
                        SuggestionOutcome::Interrupted => Outcome::Interrupted,
                        SuggestionOutcome::Failed => Outcome::Failed,
                    },
                    None => match end.map(|end| end.outcome) {
                        Some(LlmOutcome::Done) => Outcome::Done,
                        Some(LlmOutcome::Cancelled) => Outcome::Cancelled,
                        Some(LlmOutcome::Error) => Outcome::Error,
                        None => Outcome::Incomplete,
                    },
                },
            };
            Suggestion {
                suggestion,
                origin: request.map_or(SuggestionOrigin::Manual, |r| r.origin),
                profile: request.and_then(|r| r.profile),
                meeting_ms,
                shown_text,
                outcome,
            }
        })
        .collect()
}

fn counts(list: &[Suggestion]) -> Counts {
    let mut counts = Counts::default();
    for suggestion in list {
        match suggestion.origin {
            SuggestionOrigin::Manual => counts.manual += 1,
            SuggestionOrigin::Auto => counts.automatic += 1,
        }
        if suggestion.outcome == Outcome::Passed {
            counts.passed += 1;
        }
        if suggestion.outcome.is_failure() {
            counts.failed += 1;
        }
    }
    counts
}

/// Pair per D-judge-pairing: manual first with first, automatic with the
/// nearest unpaired automatic within 10 s of meeting time; the rest are
/// returned as the two only-in-one-trace lists.
fn pair_suggestions(
    a: &[Suggestion],
    b: &[Suggestion],
) -> (Vec<Pair>, Vec<Suggestion>, Vec<Suggestion>) {
    let mut used_b = vec![false; b.len()];
    let mut paired_a = vec![false; a.len()];
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    // Manual pair by order: first manual of A with first manual of B.
    let a_manual: Vec<usize> = manual_indices(a);
    let b_manual: Vec<usize> = manual_indices(b);
    for (index, &ai) in a_manual.iter().enumerate() {
        if let Some(&bi) = b_manual.get(index) {
            pairs.push((ai, bi));
            paired_a[ai] = true;
            used_b[bi] = true;
        }
    }
    // Automatic pair by nearest start within the window.
    let a_auto: Vec<usize> = a
        .iter()
        .enumerate()
        .filter(|(_, s)| s.origin == SuggestionOrigin::Auto)
        .map(|(i, _)| i)
        .collect();
    for ai in a_auto {
        let mut best: Option<(i64, usize)> = None;
        for (bi, suggestion) in b.iter().enumerate() {
            if used_b[bi] || suggestion.origin != SuggestionOrigin::Auto {
                continue;
            }
            let delta = (suggestion.meeting_ms as i64 - a[ai].meeting_ms as i64).abs();
            if delta <= AUTO_PAIR_WINDOW_MS && best.is_none_or(|(d, _)| delta < d) {
                best = Some((delta, bi));
            }
        }
        if let Some((_, bi)) = best {
            pairs.push((ai, bi));
            paired_a[ai] = true;
            used_b[bi] = true;
        }
    }
    pairs.sort_by_key(|&(ai, bi)| a[ai].meeting_ms.min(b[bi].meeting_ms));
    let pairs = pairs
        .into_iter()
        .map(|(ai, bi)| {
            let (sa, sb) = (a[ai].clone(), b[bi].clone());
            let judgeable = is_judgeable(&sa, &sb);
            Pair {
                a: sa,
                b: sb,
                judgeable,
                verdict: None,
            }
        })
        .collect();
    let only_a = a
        .iter()
        .enumerate()
        .filter(|(i, _)| !paired_a[*i])
        .map(|(_, s)| s.clone())
        .collect();
    let only_b = b
        .iter()
        .enumerate()
        .filter(|(i, _)| !used_b[*i])
        .map(|(_, s)| s.clone())
        .collect();
    (pairs, only_a, only_b)
}

fn manual_indices(list: &[Suggestion]) -> Vec<usize> {
    list.iter()
        .enumerate()
        .filter(|(_, s)| s.origin == SuggestionOrigin::Manual)
        .map(|(i, _)| i)
        .collect()
}

/// Only pairs where both ended done with shown text are judged.
fn is_judgeable(a: &Suggestion, b: &Suggestion) -> bool {
    [a, b]
        .iter()
        .all(|side| side.outcome == Outcome::Done && !side.shown_text.trim().is_empty())
}

fn speaker_report(a: &Trace, b: &Trace, speaker: Speaker) -> SpeakerReport {
    let (a_words, a_finals) = final_words(a, speaker);
    let (b_words, b_finals) = final_words(b, speaker);
    let distance = word_distance(&a_words, &b_words);
    SpeakerReport {
        speaker,
        a_finals,
        a_words: a_words.len(),
        b_finals,
        b_words: b_words.len(),
        distance,
        rate: (!a_words.is_empty()).then(|| distance as f64 / a_words.len() as f64),
    }
}

fn final_words(trace: &Trace, speaker: Speaker) -> (Vec<String>, usize) {
    let finals: Vec<&String> = trace
        .records
        .iter()
        .filter_map(|record| match &record.body {
            Body::TranscriptFinal {
                speaker: s, text, ..
            } if *s == speaker => Some(text),
            _ => None,
        })
        .collect();
    let words = finals
        .iter()
        .flat_map(|text| words(text))
        .collect::<Vec<String>>();
    (words, finals.len())
}

fn drop_rows(a: &Trace, b: &Trace) -> Vec<DropRow> {
    let count = |trace: &Trace, reason: DropReason| {
        trace
            .records
            .iter()
            .filter(|record| {
                matches!(&record.body, Body::UtteranceDropped { reason: r, .. } if *r == reason)
            })
            .count()
    };
    let all = [
        DropReason::Cancelled,
        DropReason::NoSpeech,
        DropReason::AsrError,
        DropReason::EmptyAfterOverlap,
        DropReason::Echo,
        DropReason::QueueFull,
    ];
    let mut rows: Vec<DropRow> = all
        .into_iter()
        .filter_map(|reason| {
            let (a, b) = (count(a, reason), count(b, reason));
            (a + b > 0).then_some(DropRow { reason, a, b })
        })
        .collect();
    rows.sort_by_key(|row| drop_reason_name(row.reason).to_string());
    rows
}

/// The `drop_reason` spelling the format and the table use.
pub fn drop_reason_name(reason: DropReason) -> &'static str {
    match reason {
        DropReason::Cancelled => "cancelled",
        DropReason::NoSpeech => "no_speech",
        DropReason::AsrError => "asr_error",
        DropReason::EmptyAfterOverlap => "empty_after_overlap",
        DropReason::Echo => "echo",
        DropReason::QueueFull => "queue_full",
    }
}

fn final_call_ms(trace: &Trace) -> Latency {
    let values: Vec<u64> = trace
        .records
        .iter()
        .filter_map(|record| match &record.body {
            Body::AsrCall {
                segment_kind: crate::record::SegmentKind::Final,
                duration_ms,
                ..
            } => Some(*duration_ms),
            _ => None,
        })
        .collect();
    Latency::of(&values)
}

fn first_answer_ms(trace: &Trace) -> Latency {
    let calls: Vec<u64> = trace
        .records
        .iter()
        .filter_map(|record| match &record.body {
            Body::LlmRequest {
                call,
                purpose: Purpose::Suggestion,
                ..
            } => Some(*call),
            _ => None,
        })
        .collect();
    let values: Vec<u64> = trace
        .records
        .iter()
        .filter_map(|record| match &record.body {
            Body::LlmEnd {
                call,
                first_content_ms: Some(ms),
                ..
            } if calls.contains(call) => Some(*ms),
            _ => None,
        })
        .collect();
    Latency::of(&values)
}

/// The `Me:` and `Them:` lines of the finals up to `meeting_ms`, cut to
/// the last `max_chars` characters: the context the judge sees.
pub fn transcript_before(trace: &Trace, meeting_ms: u64, max_chars: usize) -> String {
    let mut finals: Vec<(u64, u64, Speaker, &String)> = trace
        .records
        .iter()
        .filter_map(|record| match &record.body {
            Body::TranscriptFinal {
                speaker,
                t0_ms,
                text,
                ..
            } if *t0_ms <= meeting_ms => Some((*t0_ms, record.seq, *speaker, text)),
            _ => None,
        })
        .collect();
    finals.sort();
    let mut text = String::new();
    for (_, _, speaker, line) in finals {
        let name = match speaker {
            Speaker::Me => "Me",
            Speaker::Them => "Them",
        };
        text.push_str(&format!("{name}: {line}\n"));
    }
    let total = text.chars().count();
    if total > max_chars {
        let skip = total - max_chars;
        text = text.chars().skip(skip).collect();
    }
    text
}

impl Report {
    /// The plain-text report on stdout for `--compare`.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "compare {} vs {}\n\nTranscripts\n",
            self.baseline, self.candidate
        ));
        out.push_str("speaker finals a/b words a/b distance rate\n");
        for row in &self.transcripts {
            let name = match row.speaker {
                Speaker::Me => "Me",
                Speaker::Them => "Them",
            };
            let rate = match row.rate {
                Some(rate) => format!("{rate:.3}"),
                None => "n/a".to_string(),
            };
            out.push_str(&format!(
                "{name} {}/{} {}/{} {} {}\n",
                row.a_finals, row.b_finals, row.a_words, row.b_words, row.distance, rate
            ));
        }
        out.push_str("\nDrops\n");
        if self.drops.is_empty() {
            out.push_str("none\n");
        }
        for row in &self.drops {
            out.push_str(&format!(
                "{} {} {}\n",
                drop_reason_name(row.reason),
                row.a,
                row.b
            ));
        }
        out.push_str("\nLatency\n");
        out.push_str(&format!("asr final call a: {}\n", self.asr_ms[0].render()));
        out.push_str(&format!("asr final call b: {}\n", self.asr_ms[1].render()));
        out.push_str(&format!(
            "first answer piece a: {}\n",
            self.first_answer_ms[0].render()
        ));
        out.push_str(&format!(
            "first answer piece b: {}\n",
            self.first_answer_ms[1].render()
        ));
        out.push_str("\nSuggestions a/b\n");
        out.push_str(&format!(
            "manual {} {}\n",
            self.counts[0].manual, self.counts[1].manual
        ));
        out.push_str(&format!(
            "auto {} {}\n",
            self.counts[0].automatic, self.counts[1].automatic
        ));
        out.push_str(&format!(
            "pass {} {}\n",
            self.counts[0].passed, self.counts[1].passed
        ));
        out.push_str(&format!(
            "fail {} {}\n",
            self.counts[0].failed, self.counts[1].failed
        ));
        out.push_str("\nPairs\n");
        if self.pairs.is_empty() {
            out.push_str("none\n");
        }
        for (index, pair) in self.pairs.iter().enumerate() {
            out.push_str(&format!(
                "pair {} ({}, a {} s / b {} s)\n",
                index + 1,
                origin_name(pair.a.origin),
                seconds(pair.a.meeting_ms),
                seconds(pair.b.meeting_ms)
            ));
            out.push_str(&indented("A", &pair.a.shown_text));
            out.push_str(&indented("B", &pair.b.shown_text));
            out.push_str(&format!("  {}\n", self.verdict_line(pair)));
        }
        for suggestion in &self.only_a {
            out.push_str(&only_line("a", suggestion));
        }
        for suggestion in &self.only_b {
            out.push_str(&only_line("b", suggestion));
        }
        out.push_str(&self.tally_line());
        out
    }

    fn verdict_line(&self, pair: &Pair) -> String {
        match &pair.verdict {
            Some(Verdict::Winner(Side::A, reason)) => format!("A better: {reason}"),
            Some(Verdict::Winner(Side::B, reason)) => format!("B better: {reason}"),
            Some(Verdict::Tie(reason)) => format!("tie: {reason}"),
            Some(Verdict::NotJudged(reason)) => format!("not judged: {reason}"),
            None => match pair.skip_reason() {
                Some(reason) => format!("not judged: {reason}"),
                None => "not judged".to_string(),
            },
        }
    }

    /// The verdict tally, one line: `A better 1, B better 1, tie 1, not judged 1`.
    fn tally_line(&self) -> String {
        let mut tally = [0usize; 4]; // A, B, tie, not judged
        for pair in &self.pairs {
            match &pair.verdict {
                Some(Verdict::Winner(Side::A, _)) => tally[0] += 1,
                Some(Verdict::Winner(Side::B, _)) => tally[1] += 1,
                Some(Verdict::Tie(_)) => tally[2] += 1,
                Some(Verdict::NotJudged(_)) | None => tally[3] += 1,
            }
        }
        format!(
            "\nA better {}, B better {}, tie {}, not judged {}\n",
            tally[0], tally[1], tally[2], tally[3]
        )
    }
}

fn origin_name(origin: SuggestionOrigin) -> &'static str {
    match origin {
        SuggestionOrigin::Manual => "manual",
        SuggestionOrigin::Auto => "auto",
    }
}

fn seconds(meeting_ms: u64) -> String {
    format!("{:.1}", meeting_ms as f64 / 1000.0)
}

fn indented(side: &str, text: &str) -> String {
    if text.trim().is_empty() {
        return format!("  {side}: (no text)\n");
    }
    text.lines()
        .map(|line| format!("  {side}: {line}\n"))
        .collect()
}

fn only_line(side: &str, suggestion: &Suggestion) -> String {
    format!(
        "only in {side}: suggestion {} ({}, {} s, {})\n",
        suggestion.suggestion,
        origin_name(suggestion.origin),
        seconds(suggestion.meeting_ms),
        suggestion.outcome.label()
    )
}
