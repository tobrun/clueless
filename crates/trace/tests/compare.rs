//! Unit-level proofs for the compare report, the sessions listing and the
//! show rendering (change set 4). The traces are built in memory from
//! `Record` values; only the `list` tests touch a scratch directory, and
//! only their own temp directory.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use trace::compare::{
    Latency, Outcome, Side, Verdict, compare, transcript_before, word_distance, words,
};
use trace::list::list;
use trace::manifest::{
    LlmSettings, MANIFEST_FILE, Manifest, Origin, SessionStart, SpeechSettings, Timings,
    VoiceDetector,
};
use trace::paths::RUNS_DIR;
use trace::reader::Trace;
use trace::record::{
    AsrOutcome, Body, DropReason, EndReason, LlmOutcome, Profile, Purpose, Record, SegmentKind,
    Speaker, SuggestionOrigin, SuggestionOutcome,
};
use trace::show::show;

// --- in-memory trace builders

fn session_start() -> SessionStart {
    SessionStart {
        speakers: vec!["me".into(), "them".into()],
        profile: Profile::Manual,
        llm: LlmSettings {
            base_url: "http://host:8000".into(),
            model: "m".into(),
            max_tokens: 220,
            temperature: 0.4,
        },
        speech: SpeechSettings {
            base_url: "http://host:9000".into(),
            model: "w".into(),
            language: None,
        },
        voice_detector: VoiceDetector {
            start_threshold: 0.5,
            end_threshold: 0.35,
            end_silence_frames: 19,
            max_segment_ms: 15_000,
        },
        timings_ms: Timings {
            echo_hold_ms: 700,
            stop_wait_ms: 1500,
            health_timeout_ms: 2000,
            asr_timeout_ms: 15_000,
            llm_connect_ms: 2000,
            llm_stall_ms: 10_000,
        },
        compress_threshold_tokens: 90_000,
    }
}

fn manifest_with_speed(speed: f64) -> Manifest {
    Manifest {
        schema: trace::manifest::SCHEMA,
        started_at_ms: 1_791_209_002_000,
        origin: Origin::Live,
        speed,
        app_version: "0.1.0".into(),
        git_commit: "test".into(),
        audio: false,
        session: session_start(),
    }
}

/// An in-memory trace: `seq` numbers itself from the push order.
fn push(records: &mut Vec<Record>, at_ms: u64, body: Body) {
    let seq = records.len() as u64 + 1;
    records.push(Record { seq, at_ms, body });
}

fn trace_named(name: &str, speed: f64, records: Vec<Record>) -> Trace {
    Trace {
        dir: PathBuf::from("/nonexistent").join(name),
        manifest: manifest_with_speed(speed),
        records,
        cut_off: false,
    }
}

/// A meeting clock at `at_ms`, the start of every fixture trace.
fn clock(records: &mut Vec<Record>, at_ms: u64) {
    push(records, at_ms, Body::ClockStarted);
}

fn final_at(records: &mut Vec<Record>, at_ms: u64, speaker: Speaker, utterance: u64, text: &str) {
    push(
        records,
        at_ms,
        Body::TranscriptFinal {
            speaker,
            utterance,
            t0_ms: at_ms,
            t1_ms: at_ms + 500,
            text: text.to_string(),
        },
    );
}

fn end(records: &mut Vec<Record>, at_ms: u64) {
    push(
        records,
        at_ms,
        Body::End {
            reason: EndReason::Stop,
        },
    );
}

/// How the fixture suggestion answers.
enum Answer {
    Text(&'static str),
    Pass,
    Error,
}

/// One whole suggestion: start, request, streamed answer, call end and
/// UI end, all at `at_ms`, with the given origin and profile.
fn suggestion(
    records: &mut Vec<Record>,
    id: u64,
    at_ms: u64,
    origin: SuggestionOrigin,
    profile: Profile,
    answer: Answer,
) {
    push(records, at_ms, Body::SuggestionStart { suggestion: id });
    push(
        records,
        at_ms,
        Body::LlmRequest {
            call: id,
            purpose: Purpose::Suggestion,
            suggestion: Some(id),
            origin: Some(origin),
            profile: Some(profile),
            body: serde_json::json!({"model": "m"}),
        },
    );
    let (outcome, passed, shown, error) = match &answer {
        Answer::Text(text) => (LlmOutcome::Done, false, text.to_string(), None),
        Answer::Pass => (LlmOutcome::Done, true, String::new(), None),
        Answer::Error => (
            LlmOutcome::Error,
            false,
            String::new(),
            Some("500 boom".into()),
        ),
    };
    push(
        records,
        at_ms,
        Body::LlmEnd {
            call: id,
            outcome,
            error,
            finish_reason: Some("stop".into()),
            usage: None,
            raw_text: shown.clone(),
            shown_text: shown,
            passed,
            first_content_ms: Some(40),
            first_reasoning_ms: None,
        },
    );
    push(
        records,
        at_ms,
        Body::SuggestionEnd {
            suggestion: id,
            end: match answer {
                Answer::Error => SuggestionOutcome::Failed,
                _ => SuggestionOutcome::Done,
            },
            message: None,
        },
    );
}

/// One final-segment speech call of `duration_ms`.
fn asr_final(records: &mut Vec<Record>, utterance: u64, duration_ms: u64) {
    push(
        records,
        0,
        Body::AsrCall {
            speaker: Speaker::Me,
            utterance,
            segment_kind: SegmentKind::Final,
            started_at_ms: 0,
            duration_ms,
            outcome: AsrOutcome::Text,
            raw_text: Some("text".into()),
            error: None,
        },
    );
}

fn dropped(records: &mut Vec<Record>, reason: DropReason) {
    push(
        records,
        0,
        Body::UtteranceDropped {
            speaker: Speaker::Me,
            utterance: 1,
            reason,
        },
    );
}

// --- words and word distance

#[test]
fn words_lowercases_strips_punctuation_and_keeps_apostrophes() {
    assert_eq!(words("Hello, World! It's"), ["hello", "world", "it's"]);
}

#[test]
fn word_distance_of_one_substituted_word_is_one() {
    let a = words("a b c");
    let b = words("a x c");
    assert_eq!(word_distance(&a, &b), 1);
}

#[test]
fn word_distance_against_an_empty_list_is_the_other_length() {
    let a = words("a b c");
    let empty: Vec<String> = words("");
    assert_eq!(word_distance(&a, &empty), 3);
    assert_eq!(word_distance(&empty, &a), 3);
}

// --- transcript numbers

fn two_finals_each_speaker() -> Vec<Record> {
    let mut records = Vec::new();
    clock(&mut records, 0);
    final_at(&mut records, 1000, Speaker::Me, 1, "we ship on friday");
    final_at(&mut records, 2000, Speaker::Them, 1, "sounds good");
    end(&mut records, 3000);
    records
}

#[test]
fn identical_traces_have_zero_distance_and_zero_rate_for_both_speakers() {
    let a = trace_named("A", 1.0, two_finals_each_speaker());
    let b = trace_named("B", 1.0, two_finals_each_speaker());
    let report = compare(&a, &b);
    assert_eq!(report.transcripts.len(), 2);
    for row in &report.transcripts {
        assert_eq!(row.distance, 0, "{:?}", row.speaker);
        assert_eq!(row.rate, Some(0.0), "{:?}", row.speaker);
        assert_eq!((row.a_finals, row.b_finals), (1, 1));
    }
    assert_eq!(report.transcripts[0].speaker, Speaker::Me);
    assert_eq!(report.transcripts[1].speaker, Speaker::Them);
}

#[test]
fn one_changed_word_scores_distance_one_over_the_baseline_word_count() {
    let mut a_records = Vec::new();
    clock(&mut a_records, 0);
    final_at(&mut a_records, 1000, Speaker::Me, 1, "we ship on friday");
    let mut b_records = Vec::new();
    clock(&mut b_records, 0);
    final_at(&mut b_records, 1000, Speaker::Me, 1, "we ship on monday");
    let report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 1.0, b_records),
    );
    let me = &report.transcripts[0];
    assert_eq!(me.speaker, Speaker::Me);
    assert_eq!(me.a_words, 4);
    assert_eq!(me.distance, 1);
    assert_eq!(me.rate, Some(0.25));
}

#[test]
fn a_speaker_without_baseline_finals_prints_rate_as_not_applicable() {
    let mut a_records = Vec::new();
    clock(&mut a_records, 0);
    final_at(&mut a_records, 1000, Speaker::Me, 1, "hello");
    let mut b_records = Vec::new();
    clock(&mut b_records, 0);
    final_at(&mut b_records, 1000, Speaker::Me, 1, "hello again");
    let report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 1.0, b_records),
    );
    let them = &report.transcripts[1];
    assert_eq!(them.speaker, Speaker::Them);
    assert_eq!((them.a_words, them.b_words, them.distance), (0, 0, 0));
    assert_eq!(them.rate, None, "no division by zero");
    assert!(
        report.render().contains("n/a"),
        "the rendering shows n/a:\n{}",
        report.render()
    );
}

// --- drops table

#[test]
fn drops_are_counted_per_reason_across_both_traces() {
    let mut a_records = Vec::new();
    clock(&mut a_records, 0);
    dropped(&mut a_records, DropReason::Echo);
    dropped(&mut a_records, DropReason::Echo);
    let mut b_records = Vec::new();
    clock(&mut b_records, 0);
    dropped(&mut b_records, DropReason::Echo);
    dropped(&mut b_records, DropReason::NoSpeech);
    let report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 1.0, b_records),
    );
    let rendered: Vec<String> = report.render().lines().map(str::to_string).collect();
    assert!(
        rendered.iter().any(|line| line == "echo 2 1"),
        "{rendered:?}"
    );
    assert!(
        rendered.iter().any(|line| line == "no_speech 0 1"),
        "{rendered:?}"
    );
}

// --- percentiles

#[test]
fn four_final_call_durations_give_median_two_hundred_and_p95_four_hundred() {
    let mut records = Vec::new();
    clock(&mut records, 0);
    for (utterance, duration) in [100_u64, 200, 300, 400].into_iter().enumerate() {
        asr_final(&mut records, utterance as u64 + 1, duration);
    }
    let report = compare(
        &trace_named("A", 1.0, records.clone()),
        &trace_named("B", 1.0, Vec::new()),
    );
    assert_eq!(
        report.asr_ms[0],
        Latency {
            count: 4,
            median_ms: Some(200),
            p95_ms: Some(400),
        }
    );
    assert_eq!(report.asr_ms[1], Latency::default());
}

#[test]
fn interim_calls_stay_out_of_the_percentiles() {
    let mut records = Vec::new();
    clock(&mut records, 0);
    for duration in [100_u64, 200, 300, 400] {
        asr_final(&mut records, 1, duration);
    }
    for duration in [1_u64, 999_999] {
        push(
            &mut records,
            0,
            Body::AsrCall {
                speaker: Speaker::Me,
                utterance: 1,
                segment_kind: SegmentKind::Interim,
                started_at_ms: 0,
                duration_ms: duration,
                outcome: AsrOutcome::Text,
                raw_text: Some("t".into()),
                error: None,
            },
        );
    }
    let report = compare(
        &trace_named("A", 1.0, records),
        &trace_named("B", 1.0, Vec::new()),
    );
    assert_eq!(report.asr_ms[0].count, 4);
    assert_eq!(report.asr_ms[0].median_ms, Some(200));
    assert_eq!(report.asr_ms[0].p95_ms, Some(400));
}

// --- pairing (D-judge-pairing)

#[test]
fn manual_suggestions_pair_first_with_first() {
    let build = |records: &mut Vec<Record>| {
        clock(records, 0);
        suggestion(
            records,
            1,
            5_000,
            SuggestionOrigin::Manual,
            Profile::Manual,
            Answer::Text("one"),
        );
        suggestion(
            records,
            2,
            9_000,
            SuggestionOrigin::Manual,
            Profile::Manual,
            Answer::Text("two"),
        );
        end(records, 10_000);
    };
    let mut a_records = Vec::new();
    build(&mut a_records);
    let mut b_records = Vec::new();
    build(&mut b_records);
    let report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 1.0, b_records),
    );
    assert_eq!(report.pairs.len(), 2);
    assert_eq!(report.pairs[0].a.suggestion, 1);
    assert_eq!(report.pairs[0].b.suggestion, 1);
    assert_eq!(report.pairs[1].a.suggestion, 2);
    assert_eq!(report.pairs[1].b.suggestion, 2);
    assert!(report.pairs.iter().all(|pair| pair.judgeable));
    assert!(report.only_a.is_empty() && report.only_b.is_empty());
}

#[test]
fn automatic_suggestions_four_seconds_apart_pair() {
    let mut a_records = Vec::new();
    clock(&mut a_records, 0);
    suggestion(
        &mut a_records,
        1,
        10_000,
        SuggestionOrigin::Auto,
        Profile::Interview,
        Answer::Text("a"),
    );
    let mut b_records = Vec::new();
    clock(&mut b_records, 0);
    suggestion(
        &mut b_records,
        1,
        14_000,
        SuggestionOrigin::Auto,
        Profile::Interview,
        Answer::Text("b"),
    );
    let report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 1.0, b_records),
    );
    assert_eq!(report.pairs.len(), 1);
    assert_eq!(report.pairs[0].a.meeting_ms, 10_000);
    assert_eq!(report.pairs[0].b.meeting_ms, 14_000);
    assert!(report.only_a.is_empty() && report.only_b.is_empty());
}

#[test]
fn automatic_suggestions_fifteen_seconds_apart_stay_unpaired() {
    let mut a_records = Vec::new();
    clock(&mut a_records, 0);
    suggestion(
        &mut a_records,
        1,
        10_000,
        SuggestionOrigin::Auto,
        Profile::Interview,
        Answer::Text("a"),
    );
    let mut b_records = Vec::new();
    clock(&mut b_records, 0);
    suggestion(
        &mut b_records,
        1,
        25_000,
        SuggestionOrigin::Auto,
        Profile::Interview,
        Answer::Text("b"),
    );
    let report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 1.0, b_records),
    );
    assert!(report.pairs.is_empty());
    assert_eq!(report.only_a.len(), 1);
    assert_eq!(report.only_b.len(), 1);
}

#[test]
fn meeting_time_scaling_pairs_a_speed_one_trace_with_a_speed_four_trace() {
    let mut a_records = Vec::new();
    clock(&mut a_records, 1_000);
    suggestion(
        &mut a_records,
        1,
        11_000,
        SuggestionOrigin::Auto,
        Profile::Interview,
        Answer::Text("a"),
    );
    let mut b_records = Vec::new();
    clock(&mut b_records, 500);
    suggestion(
        &mut b_records,
        1,
        3_100,
        SuggestionOrigin::Auto,
        Profile::Interview,
        Answer::Text("b"),
    );
    let report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 4.0, b_records),
    );
    assert_eq!(report.pairs.len(), 1, "10.0 s and 10.4 s meeting time");
    assert_eq!(report.pairs[0].a.meeting_ms, 10_000);
    assert_eq!(report.pairs[0].b.meeting_ms, 10_400);
    let rendered = report.render();
    assert!(
        rendered.contains("a 10.0 s / b 10.4 s"),
        "seconds printed:\n{rendered}"
    );
}

#[test]
fn a_pass_against_an_answer_is_listed_but_not_judged() {
    let mut a_records = Vec::new();
    clock(&mut a_records, 0);
    suggestion(
        &mut a_records,
        1,
        5_000,
        SuggestionOrigin::Manual,
        Profile::Manual,
        Answer::Text("ask X"),
    );
    let mut b_records = Vec::new();
    clock(&mut b_records, 0);
    suggestion(
        &mut b_records,
        1,
        5_000,
        SuggestionOrigin::Manual,
        Profile::Manual,
        Answer::Pass,
    );
    let report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 1.0, b_records),
    );
    assert_eq!(report.pairs.len(), 1, "the pair is listed");
    assert!(!report.pairs[0].judgeable);
    assert_eq!(report.pairs[0].b.outcome, Outcome::Passed);
    assert!(
        report.render().contains("B passed"),
        "the report says B passed:\n{}",
        report.render()
    );
}

#[test]
fn a_side_that_ended_in_error_makes_the_pair_not_judgeable() {
    let mut a_records = Vec::new();
    clock(&mut a_records, 0);
    suggestion(
        &mut a_records,
        1,
        5_000,
        SuggestionOrigin::Manual,
        Profile::Manual,
        Answer::Error,
    );
    let mut b_records = Vec::new();
    clock(&mut b_records, 0);
    suggestion(
        &mut b_records,
        1,
        5_000,
        SuggestionOrigin::Manual,
        Profile::Manual,
        Answer::Text("answer"),
    );
    let report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 1.0, b_records),
    );
    assert_eq!(report.pairs.len(), 1);
    assert!(!report.pairs[0].judgeable);
    assert_eq!(report.counts[0].failed, 1);
}

#[test]
fn the_tally_line_counts_a_win_each_way_a_tie_and_a_not_judged() {
    let build = |records: &mut Vec<Record>| {
        clock(records, 0);
        for id in 1..=4_u64 {
            suggestion(
                records,
                id,
                id * 5_000,
                SuggestionOrigin::Manual,
                Profile::Manual,
                Answer::Text("answer"),
            );
        }
        end(records, 30_000);
    };
    let mut a_records = Vec::new();
    build(&mut a_records);
    let mut b_records = Vec::new();
    build(&mut b_records);
    let mut report = compare(
        &trace_named("A", 1.0, a_records),
        &trace_named("B", 1.0, b_records),
    );
    assert_eq!(report.pairs.len(), 4);
    let verdicts = [
        Verdict::Winner(Side::A, "sharper".into()),
        Verdict::Winner(Side::B, "sharper".into()),
        Verdict::Tie("same".into()),
        Verdict::NotJudged("server offline".into()),
    ];
    for (pair, verdict) in report.pairs.iter_mut().zip(verdicts) {
        pair.verdict = Some(verdict);
    }
    let rendered = report.render();
    assert_eq!(
        rendered.lines().last(),
        Some("A better 1, B better 1, tie 1, not judged 1"),
        "tally line:\n{rendered}"
    );
}

// --- transcript_before

#[test]
fn transcript_before_takes_only_the_finals_up_to_the_time() {
    let mut records = Vec::new();
    clock(&mut records, 0);
    final_at(&mut records, 1_000, Speaker::Me, 1, "first line");
    final_at(&mut records, 5_000, Speaker::Them, 1, "second line");
    final_at(&mut records, 9_000, Speaker::Me, 2, "third line");
    let trace = trace_named("A", 1.0, records);
    assert_eq!(
        transcript_before(&trace, 6_000, 10_000),
        "Me: first line\nThem: second line\n"
    );
}

#[test]
fn transcript_before_cuts_to_the_last_max_chars_characters() {
    let mut records = Vec::new();
    clock(&mut records, 0);
    final_at(&mut records, 1_000, Speaker::Me, 1, &"x".repeat(10_000));
    let trace = trace_named("A", 1.0, records);
    let whole = format!("Me: {}\n", "x".repeat(10_000));
    let expected: String = whole.chars().skip(whole.chars().count() - 6_000).collect();
    let kept = transcript_before(&trace, 60_000, 6_000);
    assert_eq!(kept.chars().count(), 6_000);
    assert_eq!(kept, expected);
}

// --- list

/// Scratch directory per test, unique per process run; only these tests
/// touch disk, each only inside its own directory.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "clueless-trace-list-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_manifest(dir: &Path, started_at_ms: u64) {
    std::fs::create_dir_all(dir).unwrap();
    let manifest = Manifest {
        started_at_ms,
        ..manifest_with_speed(1.0)
    };
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();
}

fn write_events(dir: &Path, records: &[Record]) {
    let text: String = records
        .iter()
        .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
        .collect();
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
}

#[test]
fn list_shows_two_sessions_newest_first_with_runs_and_cut_off() {
    let scratch = Scratch::new("two");
    let sessions = scratch.path().join("sessions");
    let older = sessions.join("2026-10-04T10-00-00Z");
    let newer = sessions.join("2026-10-05T14-03-22Z");
    write_manifest(&older, 1_791_100_000_000);
    let mut older_records = Vec::new();
    clock(&mut older_records, 0);
    final_at(&mut older_records, 1_000, Speaker::Me, 1, "hello");
    write_events(&older, &older_records); // no end record: cut off
    write_manifest(&newer, 1_791_209_002_000);
    let mut newer_records = Vec::new();
    clock(&mut newer_records, 0);
    final_at(&mut newer_records, 1_000, Speaker::Them, 1, "hi");
    end(&mut newer_records, 2_000);
    write_events(&newer, &newer_records);
    write_manifest(
        &newer.join(RUNS_DIR).join("2026-10-06T09-00-00Z"),
        1_791_300_000_000,
    );

    let rows = list(scratch.path());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "2026-10-05T14-03-22Z", "newest first");
    assert_eq!(rows[0].runs, 1);
    assert!(!rows[0].cut_off);
    assert_eq!(rows[1].name, "2026-10-04T10-00-00Z");
    assert!(rows[1].cut_off);
    assert!(rows[0].render().contains("runs 1"));
    assert!(rows[1].render().contains("cut off"));
}

#[test]
fn list_of_an_empty_or_missing_data_dir_returns_no_rows_without_error() {
    let scratch = Scratch::new("empty");
    assert!(list(scratch.path()).is_empty());
    assert!(list(&scratch.path().join("does-not-exist")).is_empty());
}

#[test]
fn a_session_directory_without_a_manifest_lists_as_unreadable() {
    let scratch = Scratch::new("unreadable");
    std::fs::create_dir_all(scratch.path().join("sessions").join("junk")).unwrap();
    let rows = list(scratch.path());
    assert_eq!(rows.len(), 1);
    assert!(rows[0].unreadable);
    assert!(rows[0].render().contains("unreadable"));
}

// --- show

#[test]
fn show_prints_finals_and_the_suggestion_in_meeting_time_order() {
    let mut records = Vec::new();
    clock(&mut records, 0);
    final_at(&mut records, 1_000, Speaker::Me, 1, "hello there");
    final_at(&mut records, 4_000, Speaker::Them, 1, "yes indeed");
    suggestion(
        &mut records,
        1,
        6_000,
        SuggestionOrigin::Auto,
        Profile::Interview,
        Answer::Text("Ask about the deadline"),
    );
    end(&mut records, 7_000);
    let rendered = show(&trace_named("A", 1.0, records));
    let lines: Vec<&str> = rendered.lines().collect();
    let position = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?}:\n{rendered}"))
    };
    let me = position("[00:01] Me: hello there");
    let them = position("[00:04] Them: yes indeed");
    let header = position("--- suggestion 1 (auto, interview) ---");
    let text = position("Ask about the deadline");
    assert!(me < them && them < header && header < text, "{lines:?}");
}

#[test]
fn show_marks_a_suppressed_suggestion_as_pass() {
    let mut records = Vec::new();
    clock(&mut records, 0);
    suggestion(
        &mut records,
        1,
        6_000,
        SuggestionOrigin::Auto,
        Profile::Interview,
        Answer::Pass,
    );
    end(&mut records, 7_000);
    let rendered = show(&trace_named("A", 1.0, records));
    assert!(rendered.contains("(PASS)"), "{rendered}");
}
