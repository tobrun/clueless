//! The golden fixture: a version 1 trace on disk must keep parsing.
//!
//! `fixtures/trace/v1` holds one record of every kind of the Records table
//! as literal JSON. Renaming a wire enum or reshaping a body must break
//! this test, not silently change what an old trace means.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use trace::manifest::Origin;
use trace::reader;
use trace::record::{Body, EndReason, Record};

fn fixture_dir() -> PathBuf {
    // The repo-root fixtures directory, shared with the audio fixtures.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("trace")
        .join("v1")
}

/// The kinds of the Records table, exactly as the format spells them.
const KINDS: [&str; 27] = [
    "asr_call",
    "audio_anchor",
    "clear_suggestion",
    "clock_started",
    "command",
    "echo_check",
    "end",
    "llm_delta",
    "llm_end",
    "llm_request",
    "meeting_state",
    "notes",
    "piece_done",
    "policy",
    "profile",
    "records_lost",
    "segment",
    "sources_drained",
    "status",
    "suggestion_delta",
    "suggestion_end",
    "suggestion_start",
    "summary_applied",
    "transcript_dropped",
    "transcript_final",
    "transcript_interim",
    "utterance_dropped",
];

fn kind_of(record: &Record) -> String {
    serde_json::to_value(&record.body).unwrap()["kind"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn the_v1_fixture_parses_with_one_record_of_every_kind() {
    let dir = fixture_dir();
    let trace = reader::read(&dir).expect("the v1 fixture must keep parsing");
    assert!(!trace.cut_off, "the fixture ends with an end record");
    assert_eq!(trace.manifest.schema, trace::manifest::SCHEMA);
    assert_eq!(trace.manifest.origin, Origin::Live);
    assert_eq!(trace.manifest.app_version, "0.1.0");
    assert_eq!(trace.manifest.session.compress_threshold_tokens, 90_000);
    assert_eq!(trace.manifest.session.timings_ms.echo_hold_ms, 700);

    assert!(
        trace
            .records
            .iter()
            .all(|record| record.body != Body::Unknown),
        "every kind of the fixture is known to this build"
    );

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for record in &trace.records {
        *counts.entry(kind_of(record)).or_default() += 1;
    }
    let expected: BTreeMap<String, usize> =
        KINDS.iter().map(|kind| (kind.to_string(), 1)).collect();
    assert_eq!(counts, expected, "one record of every kind");

    let seqs: Vec<u64> = trace.records.iter().map(|r| r.seq).collect();
    let expected_seqs: Vec<u64> = (1..=KINDS.len() as u64).collect();
    assert_eq!(seqs, expected_seqs, "sorted by seq, 1..=27");

    assert_eq!(
        trace.records.last().unwrap().body,
        Body::End {
            reason: EndReason::Stop
        }
    );
}

#[test]
fn every_fixture_line_round_trips_through_the_current_types() {
    let dir = fixture_dir();
    let trace = reader::read(&dir).unwrap();
    for record in &trace.records {
        let line = serde_json::to_string(record).unwrap();
        let back: Record = serde_json::from_str(&line).unwrap();
        assert_eq!(&back, record, "wire form of seq {} is stable", record.seq);
    }
}

#[test]
fn scanning_the_fixture_visits_the_same_records_as_reading() {
    let dir = fixture_dir();
    let counted = std::sync::Mutex::new(0_usize);
    reader::scan(&dir, |_| *counted.lock().unwrap() += 1).unwrap();
    assert_eq!(*counted.lock().unwrap(), KINDS.len());
}

/// The fixture directory is what `read` gets a path to; guard against a
/// refactor silently pointing the tests somewhere else.
#[test]
fn the_fixture_directory_holds_the_two_files() {
    let dir = fixture_dir();
    assert!(dir.join(trace::manifest::MANIFEST_FILE).is_file());
    assert!(dir.join(trace::paths::EVENTS_FILE).is_file());
}
