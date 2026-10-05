//! The compare judge (change set 7): two requests per pair in swapped
//! order against the `MockLlm` of the shared support module, checking the
//! verdict folding, the answer parsing, the one-at-a-time pacing, the
//! swapped labels, the 6000-character context cut and the request shape.

mod support;

use std::path::PathBuf;
use std::time::Duration;

use clueless_types::config::LlmConfig;
use engine::judge::{JudgeConfig, judge_pair, judge_report, judge_report_from_config};
use llm::client::LlmClient;
use trace::compare::{JUDGE_CONTEXT_CHARS, Outcome, Pair, Report, Side, Suggestion, Verdict};
use trace::manifest::{
    LlmSettings, Manifest, Origin, SessionStart, SpeechSettings, Timings, VoiceDetector,
};
use trace::reader::Trace;
use trace::record::{Body, Profile, Record, Speaker, SuggestionOrigin};

use support::{LlmReply, MockLlm};

const CONTEXT: &str = "Me: what do we ship\nThem: the trace feature\n";

fn judge_client(mock: &MockLlm) -> LlmClient {
    LlmClient::new(
        mock.base_url.clone(),
        "mock-model",
        None,
        Duration::from_secs(2),
        Duration::from_secs(5),
    )
}

fn cfg() -> JudgeConfig {
    JudgeConfig {
        temperature: 0.0,
        max_tokens: 200,
        enable_thinking: Some(false),
        include_usage: true,
    }
}

/// One streamed JSON answer from the judge mock.
fn answer(json: &str) -> LlmReply {
    LlmReply::stream(&[json])
}

async fn judge(
    mock: &MockLlm,
    cfg: &JudgeConfig,
    context: &str,
    a_text: &str,
    b_text: &str,
) -> Verdict {
    judge_pair(&judge_client(mock), cfg, context, a_text, b_text).await
}

// ------------------------------------------------------ baseline traces

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

/// An in-memory baseline trace with `Me:` finals at the given meeting
/// times; the judge only reads finals.
fn baseline(finals: &[(u64, &str)]) -> Trace {
    let mut records: Vec<Record> = Vec::new();
    records.push(Record {
        seq: 1,
        at_ms: 0,
        body: Body::ClockStarted,
    });
    for (index, (t0_ms, text)) in finals.iter().enumerate() {
        records.push(Record {
            seq: index as u64 + 2,
            at_ms: *t0_ms,
            body: Body::TranscriptFinal {
                speaker: Speaker::Me,
                utterance: index as u64 + 1,
                t0_ms: *t0_ms,
                t1_ms: t0_ms + 500,
                text: text.to_string(),
            },
        });
    }
    Trace {
        dir: PathBuf::from("/nonexistent").join("baseline"),
        manifest: Manifest {
            schema: trace::manifest::SCHEMA,
            started_at_ms: 1_791_209_002_000,
            origin: Origin::Live,
            speed: 1.0,
            app_version: "0.1.0".into(),
            git_commit: "test".into(),
            audio: false,
            session: session_start(),
        },
        records,
        cut_off: false,
    }
}

// ------------------------------------------------------------- reports

fn suggestion(id: u64, meeting_ms: u64, text: &str) -> Suggestion {
    Suggestion {
        suggestion: id,
        origin: SuggestionOrigin::Manual,
        profile: Some(Profile::Manual),
        meeting_ms,
        shown_text: text.to_string(),
        outcome: Outcome::Done,
    }
}

fn judgeable_pair(index: u64, a_text: &str, b_text: &str) -> Pair {
    Pair {
        a: suggestion(index, 10_000, a_text),
        b: suggestion(index, 10_400, b_text),
        judgeable: true,
        verdict: None,
    }
}

fn report_with(pairs: Vec<Pair>) -> Report {
    Report {
        baseline: "baseline".into(),
        candidate: "run-1".into(),
        transcripts: Vec::new(),
        drops: Vec::new(),
        asr_ms: [Default::default(), Default::default()],
        first_answer_ms: [Default::default(), Default::default()],
        counts: [Default::default(), Default::default()],
        pairs,
        only_a: Vec::new(),
        only_b: Vec::new(),
    }
}

/// The user message of request `index` the mock saw.
fn user_message(mock: &MockLlm, index: usize) -> String {
    let bodies = mock.bodies();
    bodies[index]["messages"][1]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------- verdicts

#[tokio::test]
async fn both_orders_naming_the_same_side_make_it_the_winner() {
    let mock = MockLlm::start().await;
    mock.enqueue(answer(r#"{"winner":"A","reason":"more specific"}"#));
    mock.enqueue(answer(r#"{"winner":"B","reason":"more specific"}"#));
    let verdict = judge(&mock, &cfg(), CONTEXT, "answer a", "answer b").await;
    assert_eq!(
        verdict,
        Verdict::Winner(Side::A, "more specific".to_string())
    );
}

#[tokio::test]
async fn orders_naming_opposite_answers_are_an_order_dependent_tie() {
    let mock = MockLlm::start().await;
    mock.enqueue(answer(r#"{"winner":"A","reason":"clearer"}"#));
    mock.enqueue(answer(r#"{"winner":"A","reason":"clearer"}"#));
    let verdict = judge(&mock, &cfg(), CONTEXT, "answer a", "answer b").await;
    assert_eq!(verdict, Verdict::Tie("order-dependent".to_string()));
}

#[tokio::test]
async fn both_orders_tying_is_a_tie() {
    let mock = MockLlm::start().await;
    mock.enqueue(answer(r#"{"winner":"tie","reason":"same"}"#));
    mock.enqueue(answer(r#"{"winner":"tie","reason":"same"}"#));
    let verdict = judge(&mock, &cfg(), CONTEXT, "answer a", "answer b").await;
    assert_eq!(verdict, Verdict::Tie("same".to_string()));
}

#[tokio::test]
async fn a_json_answer_wrapped_in_prose_is_read() {
    let mock = MockLlm::start().await;
    mock.enqueue(answer(
        "Sure! {\"winner\":\"B\",\"reason\":\"x\"} hope it helps",
    ));
    mock.enqueue(answer(r#"{"winner":"A","reason":"x"}"#));
    let verdict = judge(&mock, &cfg(), CONTEXT, "answer a", "answer b").await;
    assert_eq!(verdict, Verdict::Winner(Side::B, "x".to_string()));
}

#[tokio::test]
async fn an_answer_without_json_is_not_judged() {
    let mock = MockLlm::start().await;
    mock.enqueue(answer("I cannot decide"));
    let verdict = judge(&mock, &cfg(), CONTEXT, "answer a", "answer b").await;
    match verdict {
        Verdict::NotJudged(reason) => assert!(reason.contains("unreadable answer"), "{reason}"),
        other => panic!("expected NotJudged, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unknown_winner_is_not_judged() {
    let mock = MockLlm::start().await;
    mock.enqueue(answer(r#"{"winner":"C"}"#));
    let verdict = judge(&mock, &cfg(), CONTEXT, "answer a", "answer b").await;
    match verdict {
        Verdict::NotJudged(reason) => assert!(reason.contains("unreadable answer"), "{reason}"),
        other => panic!("expected NotJudged, got {other:?}"),
    }
}

#[tokio::test]
async fn a_server_error_is_not_judged_and_names_the_status() {
    let mock = MockLlm::start().await;
    mock.enqueue(LlmReply::Http {
        status: 500,
        body: "boom".to_string(),
    });
    let verdict = judge(&mock, &cfg(), CONTEXT, "answer a", "answer b").await;
    match verdict {
        Verdict::NotJudged(reason) => assert!(reason.contains("500"), "{reason}"),
        other => panic!("expected NotJudged, got {other:?}"),
    }
}

// ---------------------------------------------------------------- pacing

#[tokio::test]
async fn three_pairs_get_six_requests_one_at_a_time() {
    let mock = MockLlm::start().await;
    for _ in 0..6 {
        mock.enqueue(answer(r#"{"winner":"tie","reason":"same"}"#));
    }
    let mut report = report_with(vec![
        judgeable_pair(1, "a1", "b1"),
        judgeable_pair(2, "a2", "b2"),
        judgeable_pair(3, "a3", "b3"),
    ]);
    judge_report(
        &judge_client(&mock),
        &cfg(),
        &baseline(&[(1000, "ship it")]),
        &mut report,
    )
    .await;
    assert_eq!(mock.body_count(), 6);
    assert_eq!(mock.max_inflight(), 1);
    for pair in &report.pairs {
        assert_eq!(pair.verdict, Some(Verdict::Tie("same".to_string())));
    }
}

#[tokio::test]
async fn the_swapped_order_shows_the_baseline_answer_under_label_b() {
    let mock = MockLlm::start().await;
    for _ in 0..2 {
        mock.enqueue(answer(r#"{"winner":"tie","reason":"same"}"#));
    }
    let mut report = report_with(vec![judgeable_pair(
        1,
        "baseline answer",
        "candidate answer",
    )]);
    judge_report(&judge_client(&mock), &cfg(), &baseline(&[]), &mut report).await;
    let first = user_message(&mock, 0);
    let second = user_message(&mock, 1);
    assert!(first.contains("Answer A:\nbaseline answer"), "{first}");
    assert!(first.contains("Answer B:\ncandidate answer"), "{first}");
    assert!(second.contains("Answer B:\nbaseline answer"), "{second}");
    assert!(second.contains("Answer A:\ncandidate answer"), "{second}");
}

#[tokio::test]
async fn the_context_is_the_last_characters_without_the_judge_instructions() {
    let mock = MockLlm::start().await;
    for _ in 0..2 {
        mock.enqueue(answer(r#"{"winner":"tie","reason":"same"}"#));
    }
    // Far more than 6000 characters of finals before the suggestion.
    let finals: Vec<(u64, String)> = (0..11)
        .map(|index| {
            let marker = match index {
                0 => "FIRSTFINAL".to_string(),
                10 => "LASTFINAL".to_string(),
                _ => String::new(),
            };
            (1000 + index * 1000, format!("{marker}{}", "x".repeat(1000)))
        })
        .collect();
    let mut report = report_with(vec![judgeable_pair(1, "answer a", "answer b")]);
    // The pair's suggestion sits past the last final.
    report.pairs[0].a.meeting_ms = 99_000;
    judge_report(
        &judge_client(&mock),
        &cfg(),
        &baseline(
            &finals
                .iter()
                .map(|(t, x)| (*t, x.as_str()))
                .collect::<Vec<_>>(),
        ),
        &mut report,
    )
    .await;
    let user = user_message(&mock, 0);
    // The full transcript the format builds, cut to the last 6000 chars.
    let full: String = finals
        .iter()
        .map(|(_, text)| format!("Me: {text}\n"))
        .collect();
    let tail: String = full
        .chars()
        .skip(full.chars().count() - JUDGE_CONTEXT_CHARS)
        .collect();
    assert_eq!(tail.chars().count(), JUDGE_CONTEXT_CHARS);
    assert!(user.contains(&tail), "user message lost the cut transcript");
    assert!(user.contains("LASTFINAL"), "the newest final must survive");
    assert!(!user.contains("FIRSTFINAL"), "the oldest final must be cut");
    // Instructions are the system message's job alone.
    assert!(
        !user.contains("exactly one JSON object"),
        "the user message must carry no judge instructions"
    );
}

#[tokio::test]
async fn a_pair_that_is_not_judgeable_gets_no_request() {
    let mock = MockLlm::start().await;
    let mut pair = judgeable_pair(1, "a", "b");
    pair.judgeable = false;
    let mut report = report_with(vec![pair]);
    judge_report(&judge_client(&mock), &cfg(), &baseline(&[]), &mut report).await;
    assert_eq!(mock.body_count(), 0);
    assert_eq!(report.pairs[0].verdict, None);
}

// ------------------------------------------------------------ request shape

#[tokio::test]
async fn thinking_unset_in_the_config_omits_chat_template_kwargs() {
    let mock = MockLlm::start().await;
    for _ in 0..4 {
        mock.enqueue(answer(r#"{"winner":"tie","reason":"same"}"#));
    }
    let unset = JudgeConfig {
        enable_thinking: None,
        ..cfg()
    };
    judge(&mock, &unset, CONTEXT, "a", "b").await;
    let body = mock.bodies()[0].clone();
    assert!(
        body.get("chat_template_kwargs").is_none(),
        "unset thinking must omit the key: {body}"
    );
    // A set value still reaches the server like a live request does.
    judge(&mock, &cfg(), CONTEXT, "a", "b").await;
    let body = mock.bodies()[2].clone();
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        serde_json::json!(false)
    );
    // The usage switch comes from the config too.
    assert_eq!(
        body["stream_options"]["include_usage"],
        serde_json::json!(true)
    );
    assert_eq!(body["temperature"], serde_json::json!(0.0));
    assert_eq!(body["max_tokens"], serde_json::json!(200));
}

#[tokio::test]
async fn judge_report_from_config_builds_the_client_from_the_llm_config() {
    let mock = MockLlm::start().await;
    for _ in 0..2 {
        mock.enqueue(answer(r#"{"winner":"tie","reason":"same"}"#));
    }
    let config = LlmConfig {
        base_url: mock.base_url.clone(),
        model: "mock-model".into(),
        enable_thinking: Some(false),
        include_usage: true,
        ..Default::default()
    };
    let mut report = report_with(vec![judgeable_pair(1, "answer a", "answer b")]);
    judge_report_from_config(&config, &baseline(&[(1000, "ship it")]), &mut report).await;
    assert_eq!(mock.body_count(), 2);
    assert_eq!(
        report.pairs[0].verdict,
        Some(Verdict::Tie("same".to_string()))
    );
    let body = mock.bodies()[0].clone();
    assert_eq!(body["model"], serde_json::json!("mock-model"));
}

#[test]
fn the_judge_config_is_fixed_except_for_the_live_thinking_and_usage() {
    let config = LlmConfig {
        enable_thinking: Some(true),
        include_usage: false,
        ..Default::default()
    };
    let built = JudgeConfig::from_llm_config(&config);
    assert_eq!(built.temperature, 0.0);
    assert_eq!(built.max_tokens, 200);
    assert_eq!(built.enable_thinking, Some(true));
    assert!(!built.include_usage);
}
