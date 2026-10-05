//! The LLM side of a recorded meeting: suggestion and compression calls
//! leave `llm_request`, `llm_delta` and `llm_end` records behind, tied
//! together by one call number. Every test drives a real `Engine` through
//! `MeetingHarness` with a `MemoryOpener`.

mod support;

use std::sync::Arc;
use std::time::Duration;

use clueless_types::UiEvent;
use clueless_types::events::{EngineCommand, Speaker};
use clueless_types::profile::AssistProfile;
use engine::deps::EngineTimings;
use trace::record::{Body, Channel, EndReason, LlmOutcome, Profile, Purpose, SuggestionOrigin};
use trace::sink::MemoryOpener;

use support::*;

fn is_request(purpose: Purpose, call: u64) -> impl Fn(&Body) -> bool {
    move |body| matches!(body, Body::LlmRequest { purpose: p, call: c, .. } if *p == purpose && *c == call)
}

fn is_end(outcome: LlmOutcome, call: u64) -> impl Fn(&Body) -> bool {
    move |body| matches!(body, Body::LlmEnd { outcome: o, call: c, .. } if *o == outcome && *c == call)
}

/// Poll until an `llm_end` for `call` exists and return its fields.
async fn wait_end(
    opener: &Arc<MemoryOpener>,
    call: u64,
) -> (
    LlmOutcome,
    String,
    String,
    bool,
    Option<String>,
    Option<trace::record::Usage>,
) {
    let bodies = loop_bodies_until(opener, |bodies| {
        bodies
            .iter()
            .any(|b| matches!(b, Body::LlmEnd { call: c, .. } if *c == call))
    })
    .await;
    match bodies
        .iter()
        .find(|b| matches!(b, Body::LlmEnd { call: c, .. } if *c == call))
        .unwrap_or_else(|| panic!("an llm_end for call {call}: {bodies:#?}"))
    {
        Body::LlmEnd {
            outcome,
            raw_text,
            shown_text,
            passed,
            finish_reason,
            usage,
            ..
        } => (
            *outcome,
            raw_text.clone(),
            shown_text.clone(),
            *passed,
            finish_reason.clone(),
            usage.clone(),
        ),
        _ => unreachable!("checked above"),
    }
}

/// Poll until the `llm_end` for call 1 records an `Error`, and return
/// (outcome, error, text kept so far, all bodies seen).
async fn wait_failed_end(
    opener: &Arc<MemoryOpener>,
) -> (LlmOutcome, Option<String>, String, Vec<Body>) {
    let bodies = loop_bodies_until(opener, |bodies| {
        bodies.iter().any(|b| {
            matches!(
                b,
                Body::LlmEnd {
                    call: 1,
                    outcome: LlmOutcome::Error,
                    ..
                }
            )
        })
    })
    .await;
    match bodies
        .iter()
        .find(|b| matches!(b, Body::LlmEnd { call: 1, .. }))
    {
        Some(Body::LlmEnd {
            outcome,
            error,
            raw_text,
            ..
        }) => (*outcome, error.clone(), raw_text.clone(), bodies),
        _ => unreachable!("the poll just found the error end"),
    }
}

fn content_deltas(bodies: &[Body], call: u64) -> Vec<String> {
    bodies
        .iter()
        .filter_map(|b| match b {
            Body::LlmDelta {
                call: c,
                channel: Channel::Content,
                text,
            } if *c == call => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Send `Suggest` and wait until the mock received request number `count`.
async fn suggest_and_wait(h: &MeetingHarness, llm: &MockLlm, count: usize) {
    h.cmd(EngineCommand::Suggest);
    assert!(
        llm.wait_bodies(count, WAIT).await,
        "request {count} reaches the mock"
    );
}

/// Suggest once, wait for call 1 to end, and read back the recorded bodies.
async fn suggested_bodies(
    h: &MeetingHarness,
    llm: &MockLlm,
    opener: &Arc<MemoryOpener>,
) -> Vec<Body> {
    suggest_and_wait(h, llm, 1).await;
    wait_end(opener, 1).await;
    last_bodies(&single_trace(opener))
}

/// A traced running meeting on one speaker scripted to end one Them turn.
async fn traced_them_turn_meeting(
    asr: &MockAsr,
    llm: &MockLlm,
    opener: &Arc<MemoryOpener>,
) -> MeetingHarness {
    let (factory, vad) = one_them_turn();
    let opts = MeetingOpts {
        start_profile: AssistProfile::Interview,
        ..MeetingOpts::default()
    };
    running_tracing_meeting(asr, llm, factory, vad, opener, opts).await
}

/// Wait for the automatic request to fire and take the answer fields of its
/// `llm_end` record: (outcome, raw text, shown text, passed).
async fn auto_answer_end(
    llm: &MockLlm,
    opener: &Arc<MemoryOpener>,
) -> (LlmOutcome, String, String, bool) {
    assert!(
        llm.wait_bodies(1, WAIT).await,
        "the automatic request fires"
    );
    let (outcome, raw, shown, passed, _, _) = wait_end(opener, 1).await;
    (outcome, raw, shown, passed)
}

/// A fresh opener and a traced Them-turn meeting, for the automatic-answer tests.
async fn traced_auto(asr: &MockAsr, llm: &MockLlm) -> (Arc<MemoryOpener>, MeetingHarness) {
    let opener = opener();
    let h = traced_them_turn_meeting(asr, llm, &opener).await;
    (opener, h)
}

/// A traced Them-turn meeting right after its automatic answer ended, with the
/// opener and the answer fields: (outcome, raw text, shown text, passed).
async fn auto_answered(
    asr: &MockAsr,
    llm: &MockLlm,
) -> (
    Arc<MemoryOpener>,
    MeetingHarness,
    (LlmOutcome, String, String, bool),
) {
    let (opener, h) = traced_auto(asr, llm).await;
    let answer = auto_answer_end(llm, &opener).await;
    (opener, h, answer)
}

// ------------------------------------------------------------- suggestion calls

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manual_suggestion_records_request_deltas_and_end() {
    let (asr, llm) = start_mocks().await;
    llm.enqueue(LlmReply::stream(&["Say ", "hello"]));
    let (opener, mut h) = traced_running(&asr, &llm).await;

    suggest_and_wait(&h, &llm, 1).await;
    let (outcome, raw, shown, passed, finish_reason, usage) = wait_end(&opener, 1).await;
    let bodies = last_bodies(&single_trace(&opener));

    // The request names the call, the suggestion and the full body.
    let request = bodies
        .iter()
        .find(|b| is_request(Purpose::Suggestion, 1)(b))
        .expect("the request record");
    match request {
        Body::LlmRequest {
            purpose,
            suggestion,
            origin,
            profile,
            body,
            ..
        } => {
            assert_eq!(*purpose, Purpose::Suggestion);
            assert_eq!(*suggestion, Some(1));
            assert_eq!(*origin, Some(SuggestionOrigin::Manual));
            assert_eq!(*profile, Some(Profile::Manual));
            assert_eq!(body["model"], "mock-model");
            assert_eq!(body["messages"].as_array().expect("messages").len(), 2);
            assert!(body.get("stream_options").is_some(), "usage was asked for");
        }
        _ => unreachable!(),
    }

    // Both deltas are recorded before the end, under the same call.
    assert_eq!(content_deltas(&bodies, 1), vec!["Say ", "hello"]);
    assert!(
        ordered(
            &bodies,
            &[
                &is_request(Purpose::Suggestion, 1),
                &|b| matches!(b, Body::LlmDelta { call: 1, text, .. } if text == "Say "),
                &|b| matches!(b, Body::LlmDelta { call: 1, text, .. } if text == "hello"),
                &is_end(LlmOutcome::Done, 1),
            ]
        ),
        "request, deltas, end in order: {bodies:#?}"
    );

    assert_eq!(outcome, LlmOutcome::Done);
    assert_eq!(raw, "Say hello");
    assert_eq!(shown, "Say hello");
    assert!(!passed);
    assert_eq!(finish_reason, None);
    assert_eq!(usage, None);
    match bodies
        .iter()
        .find(|b| matches!(b, Body::LlmEnd { call: 1, .. }))
    {
        Some(Body::LlmEnd {
            first_content_ms, ..
        }) => assert!(first_content_ms.is_some(), "the first delta was timed"),
        _ => unreachable!(),
    }

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_call_number_ties_a_request_to_its_end_and_differs_between_suggestions() {
    let (asr, llm) = start_mocks().await;
    llm.enqueue(LlmReply::stream(&["first"]));
    llm.enqueue(LlmReply::stream(&["second"]));
    let (opener, mut h) = traced_running(&asr, &llm).await;

    suggest_and_wait(&h, &llm, 1).await;
    wait_end(&opener, 1).await;
    suggest_and_wait(&h, &llm, 2).await;
    wait_end(&opener, 2).await;
    let bodies = last_bodies(&single_trace(&opener));

    let requests: Vec<u64> = bodies
        .iter()
        .filter_map(|b| match b {
            Body::LlmRequest { call, .. } => Some(*call),
            _ => None,
        })
        .collect();
    assert_eq!(
        requests,
        vec![1, 2],
        "one number per call, in arrival order"
    );
    let ends: Vec<u64> = bodies
        .iter()
        .filter_map(|b| match b {
            Body::LlmEnd { call, .. } => Some(*call),
            _ => None,
        })
        .collect();
    assert_eq!(ends, vec![1, 2], "every request's call ends");
    // The deltas belong to their own call (two chunks per suggestion).
    assert_eq!(content_deltas(&bodies, 1), vec!["first"]);
    assert_eq!(content_deltas(&bodies, 2), vec!["second"]);

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reasoning_is_recorded_but_never_shown_and_timed_first() {
    let (asr, llm) = start_mocks().await;
    llm.enqueue(LlmReply::Stream(vec![
        Step::reasoning("think"),
        Step::chunk("ok"),
    ]));
    let (opener, mut h) = traced_running(&asr, &llm).await;

    let bodies = suggested_bodies(&h, &llm, &opener).await;

    assert!(
        bodies.iter().any(|b| matches!(
            b,
            Body::LlmDelta {
                call: 1,
                channel: Channel::Reasoning,
                text,
            } if text == "think"
        )),
        "the thinking piece is recorded: {bodies:#?}"
    );
    assert!(
        !h.snapshot()
            .iter()
            .any(|e| matches!(e, UiEvent::SuggestionDelta { text, .. } if text.contains("think"))),
        "no UI delta carries the thinking: {:?}",
        h.snapshot()
    );

    let bodies2 = bodies
        .iter()
        .find_map(|b| match b {
            Body::LlmEnd {
                first_content_ms,
                first_reasoning_ms,
                raw_text,
                ..
            } => Some((*first_content_ms, *first_reasoning_ms, raw_text.clone())),
            _ => None,
        })
        .expect("an end record");
    let (first_content, first_reasoning, raw) = bodies2;
    assert_eq!(raw, "ok", "reasoning is not part of the answer text");
    assert!(
        first_reasoning.is_some_and(|r| first_content.is_some_and(|c| r <= c)),
        "reasoning started no later than content: {first_reasoning:?} vs {first_content:?}"
    );

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_content_only_answer_has_no_first_reasoning_time() {
    let (asr, llm) = start_mocks().await;
    llm.enqueue(LlmReply::stream(&["only content"]));
    let (opener, mut h) = traced_running(&asr, &llm).await;

    suggest_and_wait(&h, &llm, 1).await;
    let bodies = loop_bodies_until(&opener, |bodies| {
        bodies
            .iter()
            .any(|b| matches!(b, Body::LlmEnd { call: 1, .. }))
    })
    .await;
    match bodies
        .iter()
        .find(|b| matches!(b, Body::LlmEnd { call: 1, .. }))
    {
        Some(Body::LlmEnd {
            first_reasoning_ms,
            first_content_ms,
            ..
        }) => {
            assert!(first_reasoning_ms.is_none(), "no reasoning was sent");
            assert!(first_content_ms.is_some());
        }
        _ => unreachable!(),
    }

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finish_reason_and_usage_land_in_the_end_record() {
    let (asr, llm) = start_mocks().await;
    llm.enqueue(LlmReply::Stream(vec![
        Step::chunk("the answer"),
        Step::finish("stop"),
        Step::usage(100, 5, 105),
    ]));
    let (opener, mut h) = traced_running(&asr, &llm).await;

    suggest_and_wait(&h, &llm, 1).await;
    let (outcome, raw, _shown, _passed, finish_reason, usage) = wait_end(&opener, 1).await;

    assert_eq!(outcome, LlmOutcome::Done);
    assert_eq!(raw, "the answer");
    assert_eq!(finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        usage,
        Some(trace::record::Usage {
            prompt_tokens: Some(100),
            completion_tokens: Some(5),
            total_tokens: Some(105),
        })
    );

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_request_record_carries_exactly_the_body_the_mock_received() {
    let (asr, llm) = start_mocks().await;
    llm.enqueue(LlmReply::stream(&["answered"]));
    let (opener, mut h) = traced_running(&asr, &llm).await;

    let bodies = suggested_bodies(&h, &llm, &opener).await;
    let recorded = bodies
        .iter()
        .find_map(|b| match b {
            Body::LlmRequest { body, .. } => Some(body.clone()),
            _ => None,
        })
        .expect("the request record");
    let sent = llm.last_body().expect("the mock parsed the body");
    assert_eq!(recorded, sent);

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_manual_suggestion_cancels_the_first_and_is_recorded_after_it() {
    let (asr, llm) = start_mocks().await;
    llm.enqueue(LlmReply::slow_stream(
        &["first ", "second ", "third"],
        Duration::from_millis(400),
    ));
    let (opener, mut h) = traced_running(&asr, &llm).await;

    suggest_and_wait(&h, &llm, 1).await;
    h.wait_until(WAIT, |events| {
        events
            .iter()
            .any(|e| matches!(e, UiEvent::SuggestionDelta { id: 1, .. }))
    })
    .await;
    h.cmd(EngineCommand::Suggest);

    let bodies = loop_bodies_until(&opener, |bodies| {
        bodies.iter().any(|b| {
            matches!(
                b,
                Body::LlmEnd {
                    call: 1,
                    outcome: LlmOutcome::Cancelled,
                    ..
                }
            )
        }) && bodies
            .iter()
            .any(|b| matches!(b, Body::LlmRequest { call: 2, .. }))
    })
    .await;
    assert!(
        ordered(
            &bodies,
            &[&is_end(LlmOutcome::Cancelled, 1), &|b| matches!(
                b,
                Body::LlmRequest { call: 2, .. }
            ),]
        ),
        "the cancelled end precedes the second request: {bodies:#?}"
    );

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_stream_records_an_error_naming_the_stall() {
    let (asr, llm) = start_mocks().await;
    llm.enqueue(LlmReply::Stream(vec![
        Step::chunk("half an "),
        Step::sleep_ms(2500),
        Step::chunk("answer"),
    ]));
    let opener = opener();
    let opts = MeetingOpts {
        timings: EngineTimings {
            llm_stall: Duration::from_millis(400),
            ..fast_timings()
        },
        ..MeetingOpts::default()
    };
    let mut h = traced_running_meeting(&asr, &llm, &opener, opts).await;

    suggest_and_wait(&h, &llm, 1).await;
    let (outcome, error, raw, _bodies) = wait_failed_end(&opener).await;

    assert_eq!(outcome, LlmOutcome::Error);
    let error = error.expect("the error is named");
    assert!(
        error.contains("stall"),
        "the error names the stall: {error}"
    );
    assert_eq!(raw, "half an ", "the text so far is kept");

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_error_records_the_backend_body_and_fails_the_suggestion() {
    let (asr, llm) = start_mocks().await;
    llm.enqueue(LlmReply::Http {
        status: 500,
        body: r#"{"error":"boom"}"#.to_owned(),
    });
    let (opener, mut h) = traced_running(&asr, &llm).await;

    suggest_and_wait(&h, &llm, 1).await;
    let (outcome, error, _raw, bodies) = wait_failed_end(&opener).await;

    assert_eq!(outcome, LlmOutcome::Error);
    let error = error.expect("the error text");
    assert!(error.contains("500"), "the status is named: {error}");
    assert!(error.contains("boom"), "the backend body is kept: {error}");
    assert!(
        bodies.iter().any(|b| matches!(
            b,
            Body::SuggestionEnd {
                suggestion: 1,
                end: trace::record::SuggestionOutcome::Failed,
                ..
            }
        )),
        "the suggestion_end says failed: {bodies:#?}"
    );
    let leaked = bodies.iter().any(|b| match b {
        Body::Status { text, .. } => text.contains("boom"),
        _ => false,
    });
    assert!(!leaked, "no status record carries the backend body");

    finish(&mut h).await;
}

// -------------------------------------------------- automatic answers and PASS

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_automatic_pass_shows_nothing_and_is_recorded_as_passed() {
    let (asr, llm) = start_mocks().await;
    asr.enqueue_final(Respond::text("what is the release date?"));
    llm.enqueue(LlmReply::stream(&["PASS"]));
    let (opener, mut h, (outcome, raw, shown, passed)) = auto_answered(&asr, &llm).await;

    assert_eq!(outcome, LlmOutcome::Done);
    assert_eq!(raw, "PASS");
    assert_eq!(shown, "");
    assert!(passed, "an answer held back to the end passed");
    let bodies = last_bodies(&single_trace(&opener));
    assert!(
        !bodies
            .iter()
            .any(|b| matches!(b, Body::SuggestionDelta { .. })),
        "nothing was shown, so no delta record exists: {bodies:#?}"
    );

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_automatic_answer_that_only_starts_like_pass_is_not_passed() {
    let (asr, llm) = start_mocks().await;
    asr.enqueue_final(Respond::text("can we skip the migration?"));
    llm.enqueue(LlmReply::stream(&["Passing", " is fine"]));
    let (_opener, mut h, (outcome, raw, shown, passed)) = auto_answered(&asr, &llm).await;

    assert_eq!(outcome, LlmOutcome::Done);
    assert_eq!(raw, "Passing is fine");
    assert_eq!(shown, raw, "the released answer shows in full");
    assert!(!passed);

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_automatic_answer_that_said_nothing_is_not_passed() {
    let (asr, llm) = start_mocks().await;
    asr.enqueue_final(Respond::text("what is the release date?"));
    llm.enqueue(LlmReply::stream(&[]));
    let (_opener, mut h, (outcome, raw, shown, passed)) = auto_answered(&asr, &llm).await;

    assert_eq!(outcome, LlmOutcome::Done);
    assert_eq!(raw, "");
    assert_eq!(shown, "");
    assert!(
        !passed,
        "an empty answer shows nothing but did not pass: it just said nothing"
    );

    finish(&mut h).await;
}

// ----------------------------------------------------------------- compression

/// Eight long utterances; around 160 estimated tokens each, so the default
/// 1000-token threshold is only crossed by the seventh commit and the
/// summary then folds at least three lines.
fn long_utterances(asr: &MockAsr) {
    for word in 0..8 {
        asr.enqueue_final(Respond::text(format!("line{word} ").repeat(90).trim_end()));
    }
}

/// `(frames, probs)` for `n` back-to-back utterances.
fn many_utterances(n: usize) -> (usize, Vec<f32>) {
    let mut runs = vec![];
    for _ in 0..n {
        runs.push((11usize, 0.0f32));
        runs.push((20, 0.9));
        runs.push((25, 0.0));
    }
    pattern(&runs)
}

fn many_utterance_factory(n: usize) -> ScriptedFactory {
    let (frames, _) = many_utterances(n);
    ScriptedFactory::new(vec![(
        Speaker::Me,
        vec![OpenPlan::Ok(SourcePlan::new(frames))],
    )])
}

fn many_utterance_probs(n: usize) -> Vec<f32> {
    many_utterances(n).1
}

/// A traced running Me meeting whose eight long utterances cross the
/// 1000-token compression threshold.
async fn compression_meeting(
    asr: &MockAsr,
    llm: &MockLlm,
    opener: &Arc<MemoryOpener>,
) -> MeetingHarness {
    let opts = MeetingOpts {
        compress_threshold_tokens: 1000,
        ..MeetingOpts::default()
    };
    running_tracing_meeting(
        asr,
        llm,
        many_utterance_factory(8),
        vec![VadScript::Probs(many_utterance_probs(8))],
        opener,
        opts,
    )
    .await
}

/// A fresh opener and a compression-ready meeting, for the compression tests.
async fn traced_compression(asr: &MockAsr, llm: &MockLlm) -> (Arc<MemoryOpener>, MeetingHarness) {
    let opener = opener();
    let h = compression_meeting(asr, llm, &opener).await;
    (opener, h)
}

/// The call number and recorded body of the compress request in `bodies`.
fn compress_request(bodies: &[Body]) -> (u64, serde_json::Value) {
    match bodies.iter().find(|b| {
        matches!(
            b,
            Body::LlmRequest {
                purpose: Purpose::Compress,
                ..
            }
        )
    }) {
        Some(Body::LlmRequest { call, body, .. }) => (*call, body.clone()),
        other => panic!("a compress request was recorded: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compression_records_request_end_and_the_applied_range() {
    let (asr, llm) = start_mocks().await;
    long_utterances(&asr);
    llm.enqueue(LlmReply::stream(&["early summary of the discussion"]));
    let (opener, mut h) = traced_compression(&asr, &llm).await;

    let bodies = loop_bodies_until(&opener, |bodies| {
        bodies
            .iter()
            .any(|b| matches!(b, Body::SummaryApplied { .. }))
    })
    .await;

    let (call, request_body) = compress_request(&bodies);
    assert_eq!(request_body["model"], "mock-model");
    assert_eq!(
        request_body["max_tokens"], 1500,
        "summary requests stay small"
    );
    assert!(
        ordered(
            &bodies,
            &[
                &is_request(Purpose::Compress, call),
                &is_end(LlmOutcome::Done, call),
                &|b| matches!(b, Body::SummaryApplied { call: c, .. } if *c == call),
            ]
        ),
        "request, end, summary_applied in order: {bodies:#?}"
    );
    match bodies
        .iter()
        .find(|b| matches!(b, Body::LlmEnd { call: c, .. } if *c == call))
    {
        Some(Body::LlmEnd {
            outcome, raw_text, ..
        }) => {
            assert_eq!(*outcome, LlmOutcome::Done);
            assert_eq!(raw_text, "early summary of the discussion");
        }
        _ => unreachable!(),
    }
    match bodies
        .iter()
        .find(|b| matches!(b, Body::SummaryApplied { call: c, .. } if *c == call))
    {
        Some(Body::SummaryApplied { replaced, .. }) => {
            assert!(*replaced >= 2, "the summary folded at least two lines");
        }
        _ => unreachable!(),
    }

    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_compression_records_the_error_and_no_summary_applied() {
    let (asr, llm) = start_mocks().await;
    long_utterances(&asr);
    llm.enqueue(LlmReply::Http {
        status: 500,
        body: r#"{"error":"boom"}"#.to_owned(),
    });
    let (opener, mut h) = traced_compression(&asr, &llm).await;

    let bodies = loop_bodies_until(&opener, |bodies| {
        bodies.iter().any(|b| {
            matches!(
                b,
                Body::LlmEnd {
                    outcome: LlmOutcome::Error,
                    ..
                }
            )
        })
    })
    .await;

    let (call, _request_body) = compress_request(&bodies);
    match bodies
        .iter()
        .find(|b| matches!(b, Body::LlmEnd { call: c, .. } if *c == call))
    {
        Some(Body::LlmEnd { outcome, error, .. }) => {
            assert_eq!(*outcome, LlmOutcome::Error);
            let error = error.clone().expect("the error text");
            assert!(error.contains("500") && error.contains("boom"), "{error}");
        }
        _ => unreachable!(),
    }
    assert!(
        !bodies
            .iter()
            .any(|b| matches!(b, Body::SummaryApplied { .. })),
        "a failed summary replaces nothing: {bodies:#?}"
    );
    assert!(
        h.statuses().iter().any(|(source, level, text)| *source
            == clueless_types::events::StatusSource::App
            && *level == clueless_types::events::StatusLevel::Warn
            && text == "transcript compression failed: LLM error 500"),
        "the unchanged Warn status is still emitted: {:?}",
        h.statuses()
    );

    finish(&mut h).await;
}

// -------------------------------------------------------------------- secrets

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_keys_never_appear_in_any_record() {
    let (asr, llm) = start_mocks().await;
    asr.set_default_final(Respond::text("what is the release date?"));
    llm.enqueue(LlmReply::stream(&["Friday"]));
    let opener = opener();
    let opts = MeetingOpts {
        llm_api_key: Some("sk-llm-secret-key".to_owned()),
        asr_api_key: Some("asr-secret-key".to_owned()),
        ..trace_opts(&opener)
    };
    let mut h = traced_running_meeting(&asr, &llm, &opener, opts).await;
    suggest_and_wait(&h, &llm, 1).await;
    stop_and_wait(&h).await;
    finish(&mut h).await;
    assert_eq!(
        single_trace(&opener).closed(),
        Some(EndReason::Stop),
        "the trace closed with everything in it"
    );

    for record in single_trace(&opener).records() {
        let json = serde_json::to_string(&record.body).expect("a record serializes");
        assert!(!json.contains("sk-llm-secret-key"), "{json}");
        assert!(!json.contains("asr-secret-key"), "{json}");
    }
}
