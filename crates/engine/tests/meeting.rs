//! Integration tests for the meeting lifecycle: startup and health, the
//! suggestion state machine, stop/flush semantics, panics, compression and
//! profiles, all through a real `Engine` over the scripted sources and both
//! mock servers from `support`.

mod support;

use std::time::{Duration, Instant};

use clueless_types::audio::SourceError;
use clueless_types::events::{
    EngineCommand, MeetingState, Speaker, StatusLevel, StatusSource, SuggestionEnd, UiEvent,
};
use engine::deps::EngineTimings;
use tokio::time::timeout;

use support::*;

const WAIT: Duration = Duration::from_secs(8);

/// Send `Shutdown` and wait for the engine loop to return.
async fn finish(h: &mut MeetingHarness) {
    h.cmd(EngineCommand::Shutdown);
    timeout(WAIT, &mut h.engine)
        .await
        .expect("engine loop returns")
        .expect("engine task does not panic");
}

/// A running Me-only meeting over an endless silent source.
async fn running_me_only(asr: &MockAsr, llm: &MockLlm, opts: MeetingOpts) -> MeetingHarness {
    let h = MeetingHarness::start(
        asr,
        llm,
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan::endless(200))],
        )]),
        vec![VadScript::Probs(Vec::new())],
        opts,
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);
    assert!(
        h.wait_state(MeetingState::Running, WAIT).await,
        "meeting reaches Running: {:?}",
        h.states()
    );
    h
}

fn body_messages(body: &serde_json::Value) -> Vec<(String, String)> {
    body["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .map(|m| {
            (
                m["role"].as_str().unwrap_or_default().to_owned(),
                m["content"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn user_content(body: &serde_json::Value) -> String {
    body_messages(body)
        .into_iter()
        .rev()
        .find(|(role, _)| role == "user")
        .expect("a user message")
        .1
}

/// The transcript part is the user message up to the first blank line.
fn transcript_part_of(content: &str) -> String {
    content.split("\n\n").next().unwrap_or(content).to_owned()
}

fn is_final(event: &UiEvent) -> bool {
    matches!(event, UiEvent::TranscriptFinal(_))
}

fn start_delta_of(event: &UiEvent, id: u64) -> bool {
    matches!(event, UiEvent::SuggestionDelta { id: got, .. } if *got == id)
}

// ------------------------------------------------------------------ startup

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_with_both_mocks_up_reaches_running_with_ok_statuses() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let mut h = running_me_only(&asr, &llm, MeetingOpts::default()).await;

    assert_eq!(
        h.states(),
        vec![MeetingState::Starting, MeetingState::Running]
    );
    let statuses = h.statuses();
    assert!(
        statuses.iter().any(|(source, level, _)| {
            *source == StatusSource::Asr && *level == StatusLevel::Info
        }),
        "one Asr ok status: {statuses:?}"
    );
    assert!(
        statuses.iter().any(|(source, level, _)| {
            *source == StatusSource::Llm && *level == StatusLevel::Info
        }),
        "one Llm ok status: {statuses:?}"
    );
    assert!(
        statuses
            .iter()
            .all(|(_, level, _)| *level != StatusLevel::Error),
        "no error statuses: {statuses:?}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_llm_and_missing_asr_model_report_errors_but_run() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let opts = MeetingOpts {
        llm_port: Some(closed_port().await),
        asr_model: Some("model-not-on-the-server".to_owned()),
        ..MeetingOpts::default()
    };
    let mut h = running_me_only(&asr, &llm, opts).await;

    let statuses = h.statuses();
    assert!(
        statuses.iter().any(|(source, level, text)| {
            *source == StatusSource::Llm
                && *level == StatusLevel::Error
                && text.contains("LLM offline")
        }),
        "Llm Error naming offline: {statuses:?}"
    );
    assert!(
        statuses.iter().any(|(source, level, text)| {
            *source == StatusSource::Asr
                && *level == StatusLevel::Error
                && text.contains("model-not-on-the-server")
        }),
        "Asr Error naming the model: {statuses:?}"
    );
    assert!(h.states().contains(&MeetingState::Running));
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn me_only_factory_emits_no_system_audio_status() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let mut h = running_me_only(&asr, &llm, MeetingOpts::default()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(
        h.statuses()
            .iter()
            .all(|(source, _, _)| *source != StatusSource::SystemAudio),
        "no SystemAudio status at all: {:?}",
        h.statuses()
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn them_permission_failure_runs_me_only_with_grant_status() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let factory = ScriptedFactory::new(vec![
        (Speaker::Me, vec![OpenPlan::Ok(SourcePlan::endless(200))]),
        (
            Speaker::Them,
            vec![OpenPlan::Fail(SourceError::PermissionMissing(
                "grant system audio permission in System Settings".to_owned(),
            ))],
        ),
    ]);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        factory,
        vec![VadScript::Probs(Vec::new())],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    h.wait_state(MeetingState::Running, WAIT).await;
    let statuses = h.statuses();
    assert!(
        statuses.iter().any(|(source, level, text)| {
            *source == StatusSource::SystemAudio
                && *level == StatusLevel::Error
                && text.contains("grant system audio permission")
        }),
        "SystemAudio Error with the grant text: {statuses:?}"
    );
    assert!(h.states().contains(&MeetingState::Running));
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_sources_failing_returns_to_idle() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let factory = ScriptedFactory::new(vec![
        (
            Speaker::Me,
            vec![OpenPlan::Fail(SourceError::DeviceNotFound(
                "no microphone device".to_owned(),
            ))],
        ),
        (
            Speaker::Them,
            vec![OpenPlan::Fail(SourceError::PermissionMissing(
                "grant microphone permission".to_owned(),
            ))],
        ),
    ]);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        factory,
        vec![VadScript::Probs(Vec::new())],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    assert!(
        h.wait_state(MeetingState::Idle, WAIT).await,
        "back to Idle: {:?}",
        h.states()
    );
    assert!(!h.states().contains(&MeetingState::Running));
    assert_eq!(h.states(), vec![MeetingState::Starting, MeetingState::Idle]);
    finish(&mut h).await;
}

// ---------------------------------------------------------------- suggestions

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggest_while_idle_emits_nothing() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan::endless(200))],
        )]),
        vec![VadScript::Probs(Vec::new())],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::Suggest);
    h.cmd(EngineCommand::ClearSuggestion);
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(h.snapshot().is_empty(), "no events while Idle");
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggest_while_running_streams_deltas_and_ends_done() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    llm.enqueue(LlmReply::stream(&["Here", " is a reply"]));
    let mut h = running_me_only(&asr, &llm, MeetingOpts::default()).await;

    h.cmd(EngineCommand::Suggest);
    let events = h
        .wait_until(WAIT, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    UiEvent::SuggestionEnd {
                        id: 1,
                        end: SuggestionEnd::Done
                    }
                )
            })
        })
        .await;

    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            UiEvent::SuggestionDelta { id: 1, text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["Here", " is a reply"]);
    assert!(events.contains(&UiEvent::SuggestionStart { id: 1 }));

    // The prompt reaches the mock in the shape every request must have.
    let body = llm.last_body().expect("one chat request");
    assert_eq!(body["stream"], serde_json::json!(true));
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        serde_json::json!(false)
    );
    assert_eq!(body["model"], serde_json::json!("mock-model"));
    let messages = body_messages(&body);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].0, "system");
    assert_eq!(messages[1].0, "user");
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_suggest_cancels_first_before_starting_second() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let words: Vec<String> = (0..20).map(|n| format!("word{n} ")).collect();
    let deltas: Vec<&str> = words.iter().map(String::as_str).collect();
    llm.enqueue(LlmReply::slow_stream(&deltas, Duration::from_millis(40)));
    llm.enqueue(LlmReply::stream(&["all done here"]));
    let mut h = running_me_only(&asr, &llm, MeetingOpts::default()).await;

    h.cmd(EngineCommand::Suggest);
    h.wait_until(WAIT, |events| events.iter().any(|e| start_delta_of(e, 1)))
        .await;
    h.cmd(EngineCommand::Suggest);

    h.wait_until(WAIT, |events| {
        events.iter().any(|event| {
            matches!(
                event,
                UiEvent::SuggestionEnd {
                    id: 2,
                    end: SuggestionEnd::Done
                }
            )
        })
    })
    .await;
    let events = h.snapshot();

    // End 1 Cancelled strictly precedes Start 2.
    let end1 = events
        .iter()
        .position(|e| {
            matches!(
                e,
                UiEvent::SuggestionEnd {
                    id: 1,
                    end: SuggestionEnd::Cancelled
                }
            )
        })
        .expect("SuggestionEnd 1 Cancelled");
    let start2 = events
        .iter()
        .position(|e| *e == UiEvent::SuggestionStart { id: 2 })
        .expect("SuggestionStart 2");
    assert!(end1 < start2, "Cancelled 1 before Start 2: {events:?}");

    // And no delta for id 1 arrives after Start 2.
    assert!(
        !events[start2..].iter().any(|e| start_delta_of(e, 1)),
        "no id-1 delta after Start 2"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_suggestion_cancels_then_clears() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let words: Vec<String> = (0..30).map(|n| format!("word{n} ")).collect();
    let deltas: Vec<&str> = words.iter().map(String::as_str).collect();
    llm.enqueue(LlmReply::slow_stream(&deltas, Duration::from_millis(40)));
    let mut h = running_me_only(&asr, &llm, MeetingOpts::default()).await;

    h.cmd(EngineCommand::Suggest);
    h.wait_until(WAIT, |events| events.iter().any(|e| start_delta_of(e, 1)))
        .await;
    h.cmd(EngineCommand::ClearSuggestion);

    h.wait_until(WAIT, |events| events.contains(&UiEvent::ClearSuggestion))
        .await;
    let events = h.snapshot();
    let end1 = events
        .iter()
        .position(|e| {
            matches!(
                e,
                UiEvent::SuggestionEnd {
                    id: 1,
                    end: SuggestionEnd::Cancelled
                }
            )
        })
        .expect("SuggestionEnd 1 Cancelled");
    let clear = events
        .iter()
        .position(|e| *e == UiEvent::ClearSuggestion)
        .expect("ClearSuggestion event");
    assert!(end1 < clear, "Cancelled before the clear event: {events:?}");
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn llm_http_400_fails_and_mid_stream_close_interrupts() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    llm.enqueue(LlmReply::Http {
        status: 400,
        body: r#"{"error":"bad request"}"#.to_owned(),
    });
    llm.enqueue(LlmReply::CloseMidStream(vec![Step::chunk(
        "Partial piece of an answer.",
    )]));
    let mut h = running_me_only(&asr, &llm, MeetingOpts::default()).await;

    h.cmd(EngineCommand::Suggest);
    let events = h
        .wait_until(WAIT, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    UiEvent::SuggestionEnd {
                        id: 1,
                        end: SuggestionEnd::Failed(_)
                    }
                )
            })
        })
        .await;
    match events
        .iter()
        .find(|event| matches!(event, UiEvent::SuggestionEnd { id: 1, .. }))
    {
        Some(UiEvent::SuggestionEnd {
            end: SuggestionEnd::Failed(text),
            ..
        }) => assert!(text.contains("400"), "Failed names the status: {text}"),
        other => panic!("expected Failed end for 1, got {other:?}"),
    }

    h.cmd(EngineCommand::Suggest);
    h.wait_until(WAIT, |events| {
        events.iter().any(|event| {
            matches!(
                event,
                UiEvent::SuggestionEnd {
                    id: 2,
                    end: SuggestionEnd::Interrupted
                }
            )
        })
    })
    .await;
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_llm_stream_ends_interrupted() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    llm.enqueue(LlmReply::Stream(vec![
        Step::chunk("opening"),
        Step::sleep_ms(1000),
        Step::chunk("never seen"),
    ]));
    let opts = MeetingOpts {
        timings: EngineTimings {
            llm_stall: Duration::from_millis(200),
            ..fast_timings()
        },
        ..MeetingOpts::default()
    };
    let mut h = running_me_only(&asr, &llm, opts).await;

    let at = Instant::now();
    h.cmd(EngineCommand::Suggest);
    h.wait_until(WAIT, |events| {
        events.iter().any(|event| {
            matches!(
                event,
                UiEvent::SuggestionEnd {
                    id: 1,
                    end: SuggestionEnd::Interrupted
                }
            )
        })
    })
    .await;
    let elapsed = at.elapsed();
    assert!(
        elapsed < Duration::from_millis(900),
        "stall cut at the 200 ms timeout, not the mock's 1 s: {elapsed:?}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggest_after_them_final_quotes_it() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(
        "could you send me the report tomorrow morning",
    ));
    let (frames, probs) = utterance(11, 20, 20);
    let factory = ScriptedFactory::new(vec![(
        Speaker::Them,
        vec![OpenPlan::Ok(SourcePlan::new(frames))],
    )]);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        factory,
        vec![VadScript::Probs(probs)],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    h.wait_until(WAIT, |events| events.iter().any(is_final))
        .await;
    h.cmd(EngineCommand::Suggest);
    assert!(llm.wait_bodies(1, WAIT).await, "one chat request");

    let body = llm.last_body().expect("the body");
    let last = body_messages(&body).pop().expect("last message").1;
    assert!(
        last.contains(
            "The last thing Them said was: \"could you send me the report tomorrow morning\""
        ),
        "last message quotes the Them final: {last}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggest_while_them_interim_shows_in_progress_block() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.set_final_min_ms(2200);
    asr.enqueue_interim(Respond::text("they are saying something right now"));
    let (frames, probs) = pattern(&[(11, 0.0), (90, 0.9)]);
    let factory = ScriptedFactory::new(vec![(
        Speaker::Them,
        vec![OpenPlan::Ok(SourcePlan::endless(frames))],
    )]);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        factory,
        vec![VadScript::Probs(probs)],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    let events = h
        .wait_until(WAIT, |events| {
            events
                .iter()
                .any(|event| matches!(event, UiEvent::TranscriptInterim { .. }))
        })
        .await;
    let interim_text = match events
        .iter()
        .find(|event| matches!(event, UiEvent::TranscriptInterim { .. }))
    {
        Some(UiEvent::TranscriptInterim { text, .. }) => text.clone(),
        other => panic!("expected an interim, got {other:?}"),
    };
    h.cmd(EngineCommand::Suggest);
    assert!(llm.wait_bodies(1, WAIT).await, "one chat request");

    let content = user_content(&llm.last_body().expect("the body"));
    assert!(
        content.contains("IN PROGRESS (may be incomplete):"),
        "in-progress block present: {content}"
    );
    assert!(
        content.contains(&interim_text),
        "interim text present: {content}"
    );
    finish(&mut h).await;
}

/// Two utterances in one source, the second final delayed so the first
/// suggestion definitely sees only the first line.
fn two_utterance_factory() -> ScriptedFactory {
    let (frames, _) = pattern(&[(11, 0.0), (20, 0.9), (25, 0.0), (20, 0.9), (25, 0.0)]);
    ScriptedFactory::new(vec![(
        Speaker::Me,
        vec![OpenPlan::Ok(SourcePlan::new(frames))],
    )])
}

fn two_utterance_probs() -> Vec<f32> {
    let (_, probs) = pattern(&[(11, 0.0), (20, 0.9), (25, 0.0), (20, 0.9), (25, 0.0)]);
    probs
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_suggest_transcript_part_extends_the_first() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("the deploy went out at noon"));
    asr.enqueue_final(Respond::Delay {
        ms: 800,
        then: Box::new(Respond::text("the tests passed afterwards")),
    });
    llm.enqueue(LlmReply::stream(&["first answer"]));
    llm.enqueue(LlmReply::stream(&["second answer"]));
    let probs = two_utterance_probs();
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        two_utterance_factory(),
        vec![VadScript::Probs(probs)],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    h.wait_until(WAIT, |events| {
        events.iter().filter(|e| is_final(e)).count() >= 1
    })
    .await;
    h.cmd(EngineCommand::Suggest);
    assert!(llm.wait_bodies(1, WAIT).await, "first chat request");

    h.wait_until(WAIT, |events| {
        events.iter().filter(|e| is_final(e)).count() >= 2
    })
    .await;
    h.cmd(EngineCommand::Suggest);
    assert!(llm.wait_bodies(2, WAIT).await, "second chat request");

    let bodies = llm.bodies();
    let first = transcript_part_of(&user_content(&bodies[0]));
    let second = transcript_part_of(&user_content(&bodies[1]));
    assert!(
        !first.contains("the tests passed afterwards"),
        "first request predates the second line: {first}"
    );
    assert!(
        second.starts_with(&first),
        "second transcript extends the first:\nfirst: {first}\nsecond: {second}"
    );
    assert!(second.contains("the tests passed afterwards"));
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_requests_have_stream_on_and_thinking_off() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("one final sentence here"));
    llm.enqueue(LlmReply::stream(&["an answer"]));
    let (frames, probs) = utterance(11, 20, 20);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan::new(frames))],
        )]),
        vec![VadScript::Probs(probs)],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    h.wait_until(WAIT, |events| events.iter().any(is_final))
        .await;
    h.cmd(EngineCommand::Suggest);
    assert!(llm.wait_bodies(1, WAIT).await, "one chat request");

    for body in llm.bodies() {
        assert_eq!(body["stream"], serde_json::json!(true), "stream on");
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"],
            serde_json::json!(false),
            "thinking off"
        );
    }
    finish(&mut h).await;
}

// ----------------------------------------------------------- stop and panic

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_during_speech_flushes_final_then_idle() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("we were mid sentence talking"));
    // Speed 2 (16 ms per frame) keeps the source delivering frames when the
    // stop arrives, so no Empty-gap can close the segment first.
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan {
                frames: 60,
                speed: 2.0,
                never_end: true,
            })],
        )]),
        vec![VadScript::Probs(vec![0.9; 60])],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);
    h.wait_state(MeetingState::Running, WAIT).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !h.snapshot().iter().any(is_final),
        "nothing committed while the segment stays open"
    );

    h.cmd(EngineCommand::StopMeeting);
    let events = h
        .wait_until(WAIT, |events| {
            events.contains(&UiEvent::MeetingState(MeetingState::Idle))
        })
        .await;

    let final_at = events.iter().position(is_final).expect("flushed final");
    let idle_at = events
        .iter()
        .rposition(|e| *e == UiEvent::MeetingState(MeetingState::Idle))
        .expect("Idle");
    assert!(final_at < idle_at, "final emitted before Idle: {events:?}");
    match events.iter().find(|e| is_final(e)) {
        Some(UiEvent::TranscriptFinal(utterance)) => {
            assert_eq!(utterance.text, "we were mid sentence talking")
        }
        other => panic!("expected a final, got {other:?}"),
    }
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_releases_blocked_final_within_stop_wait() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.block();
    asr.enqueue_final(Respond::Blocked(Box::new(Respond::text(
        "too late to matter",
    ))));
    let (frames, probs) = utterance(11, 20, 20);
    let opts = MeetingOpts {
        timings: EngineTimings {
            stop_wait: Duration::from_millis(300),
            ..fast_timings()
        },
        ..MeetingOpts::default()
    };
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan::new(frames))],
        )]),
        vec![VadScript::Probs(probs)],
        opts,
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);
    h.wait_state(MeetingState::Running, WAIT).await;
    assert!(
        asr.wait_requests(1, WAIT).await,
        "the final request reaches the mock and blocks"
    );

    let at = Instant::now();
    h.cmd(EngineCommand::StopMeeting);
    let events = h
        .wait_until(WAIT, |events| {
            events.contains(&UiEvent::MeetingState(MeetingState::Idle))
        })
        .await;
    assert!(
        at.elapsed() < Duration::from_secs(2),
        "Idle within 2 s of StopMeeting: {:?}",
        at.elapsed()
    );

    let dropped_at = events
        .iter()
        .position(|e| matches!(e, UiEvent::TranscriptDropped { .. }))
        .expect("TranscriptDropped for the blocked final");
    let idle_at = events
        .iter()
        .rposition(|e| *e == UiEvent::MeetingState(MeetingState::Idle))
        .expect("Idle");
    assert!(dropped_at < idle_at, "dropped before Idle: {events:?}");
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toggle_starts_when_idle_and_stops_when_running() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan::endless(200))],
        )]),
        vec![VadScript::Probs(Vec::new())],
        MeetingOpts::default(),
    )
    .await;

    h.cmd(EngineCommand::ToggleMeeting);
    assert!(h.wait_state(MeetingState::Running, WAIT).await);
    h.cmd(EngineCommand::ToggleMeeting);
    let events = h
        .wait_until(WAIT, |events| {
            events
                .iter()
                .filter(|e| matches!(e, UiEvent::MeetingState(MeetingState::Idle)))
                .count()
                >= 1
        })
        .await;
    let states: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            UiEvent::MeetingState(state) => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        vec![
            MeetingState::Starting,
            MeetingState::Running,
            MeetingState::Stopping,
            MeetingState::Idle,
        ]
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_running_and_reports_idle_when_idle() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;

    // While Running: the stop steps run and Idle is emitted.
    let mut h = running_me_only(&asr, &llm, MeetingOpts::default()).await;
    h.cmd(EngineCommand::Shutdown);
    timeout(WAIT, &mut h.engine)
        .await
        .expect("engine returns")
        .expect("engine task ok");
    assert_eq!(
        h.states(),
        vec![
            MeetingState::Starting,
            MeetingState::Running,
            MeetingState::Stopping,
            MeetingState::Idle,
        ]
    );

    // While Idle: an Idle event is emitted again.
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan::endless(200))],
        )]),
        vec![VadScript::Probs(Vec::new())],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::Shutdown);
    timeout(WAIT, &mut h.engine)
        .await
        .expect("engine returns")
        .expect("engine task ok");
    assert_eq!(h.states(), vec![MeetingState::Idle]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vad_panic_becomes_app_error_and_idle() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan::endless(200))],
        )]),
        vec![VadScript::PanicsAt(3)],
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    let events = h
        .wait_until(WAIT, |events| {
            events.contains(&UiEvent::MeetingState(MeetingState::Idle))
                && events.iter().any(|event| {
                    matches!(
                        event,
                        UiEvent::Status {
                            source: StatusSource::App,
                            level: StatusLevel::Error,
                            ..
                        }
                    )
                })
        })
        .await;
    assert!(
        h.ordered(
            |e| matches!(
                e,
                UiEvent::Status {
                    source: StatusSource::App,
                    level: StatusLevel::Error,
                    ..
                }
            ),
            |e| *e == UiEvent::MeetingState(MeetingState::Idle)
        ),
        "App Error precedes Idle: {events:?}"
    );
    let statuses = h.statuses();
    assert!(
        statuses.iter().any(|(source, level, text)| {
            *source == StatusSource::App
                && *level == StatusLevel::Error
                && text.contains("panicked")
        }),
        "the panic status explains itself: {statuses:?}"
    );
    finish(&mut h).await;
}

// --------------------------------------------------------- meetings & store

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_meeting_restarts_seq_and_store() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("first meeting one two three"));
    asr.enqueue_final(Respond::text("second meeting four five six"));
    llm.enqueue(LlmReply::stream(&["answer"]));
    let (frames, probs) = utterance(11, 20, 20);
    let factory = ScriptedFactory::new(vec![(
        Speaker::Me,
        vec![
            OpenPlan::Ok(SourcePlan::new(frames)),
            OpenPlan::Ok(SourcePlan::new(frames)),
        ],
    )]);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        factory,
        vec![VadScript::Probs(probs.clone()), VadScript::Probs(probs)],
        MeetingOpts::default(),
    )
    .await;

    h.cmd(EngineCommand::StartMeeting);
    h.wait_until(WAIT, |events| events.iter().any(is_final))
        .await;
    h.cmd(EngineCommand::StopMeeting);
    h.wait_state(MeetingState::Idle, WAIT).await;

    h.cmd(EngineCommand::StartMeeting);
    h.wait_until(WAIT, |events| {
        events.iter().filter(|e| is_final(e)).count() >= 2
    })
    .await;
    let finals: Vec<_> = h
        .snapshot()
        .into_iter()
        .filter_map(|event| match event {
            UiEvent::TranscriptFinal(utterance) => Some(utterance),
            _ => None,
        })
        .collect();
    assert_eq!(finals.len(), 2);
    assert_eq!(finals[0].text, "first meeting one two three");
    assert_eq!(finals[1].text, "second meeting four five six");
    assert_eq!(
        finals[1].id.seq, 0,
        "the per-speaker seq restarts with the meeting"
    );

    // And the second meeting's store holds only the second line.
    h.cmd(EngineCommand::Suggest);
    assert!(llm.wait_bodies(1, WAIT).await, "one chat request");
    let content = user_content(&llm.last_body().expect("the body"));
    assert!(content.contains("second meeting four five six"));
    assert!(
        !content.contains("first meeting one two three"),
        "the old meeting's lines are gone: {content}"
    );
    finish(&mut h).await;
}

/// Two long utterances push the transcript part past 200 estimated tokens
/// only after the second commit.
fn long_utterances(asr: &MockAsr) {
    asr.enqueue_final(Respond::text("alpha ".repeat(90).trim_end()));
    asr.enqueue_final(Respond::text("bravo ".repeat(90).trim_end()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compression_fires_once_and_summary_leads_the_transcript() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    long_utterances(&asr);
    llm.enqueue(LlmReply::stream(&["early summary of the discussion"]));
    let opts = MeetingOpts {
        compress_threshold_tokens: 200,
        ..MeetingOpts::default()
    };
    let probs = two_utterance_probs();
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        two_utterance_factory(),
        vec![VadScript::Probs(probs)],
        opts,
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    h.wait_until(WAIT, |events| {
        events.iter().filter(|e| is_final(e)).count() >= 2
    })
    .await;
    assert!(
        llm.wait_bodies(1, WAIT).await,
        "one compression request crosses the threshold"
    );
    // Give the summary a moment to land in the store.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(llm.body_count(), 1, "exactly one compression request");

    llm.enqueue(LlmReply::stream(&["suggestion after summary"]));
    h.cmd(EngineCommand::Suggest);
    assert!(
        llm.wait_bodies(2, WAIT).await,
        "then the suggestion request"
    );

    let bodies = llm.bodies();
    let content = user_content(&bodies[1]);
    assert!(
        content.starts_with(
            "TRANSCRIPT SO FAR:\nEARLIER IN THIS MEETING (summary):\nearly summary of the discussion"
        ),
        "the store's transcript starts with the summary block: {content}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compression_failure_warns_and_retries_after_the_interval() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    long_utterances(&asr);
    llm.enqueue(LlmReply::Http {
        status: 500,
        body: r#"{"error":"boom"}"#.to_owned(),
    });
    let opts = MeetingOpts {
        compress_threshold_tokens: 200,
        timings: EngineTimings {
            compress_retry: Duration::from_millis(500),
            ..fast_timings()
        },
        ..MeetingOpts::default()
    };
    let probs = two_utterance_probs();
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        two_utterance_factory(),
        vec![VadScript::Probs(probs)],
        opts,
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    fn warn_count(events: &[UiEvent]) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    UiEvent::Status {
                        source: StatusSource::App,
                        level: StatusLevel::Warn,
                        ..
                    }
                )
            })
            .count()
    }
    h.wait_until(WAIT, |events| warn_count(events) >= 1).await;
    assert_eq!(llm.body_count(), 1, "one failed attempt so far");
    assert_eq!(warn_count(&h.snapshot()), 1, "exactly one Warn status");

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(llm.body_count(), 1, "no second attempt within 300 ms");
    assert_eq!(warn_count(&h.snapshot()), 1, "still one Warn status");

    llm.enqueue(LlmReply::stream(&["late summary lands"]));
    assert!(
        llm.wait_bodies(2, Duration::from_secs(3)).await,
        "a second attempt after the 500 ms interval"
    );
    assert_eq!(warn_count(&h.snapshot()), 1, "the retry succeeds quietly");
    finish(&mut h).await;
}

// ------------------------------------------------------------------ profile

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_lands_in_system_message() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let path = temp_path("profile").with_extension("txt");
    std::fs::write(&path, "Prefers concise bullet answers.").expect("write profile");
    let opts = MeetingOpts {
        profile_path: Some(path.display().to_string()),
        ..MeetingOpts::default()
    };
    llm.enqueue(LlmReply::stream(&["an answer"]));
    let mut h = running_me_only(&asr, &llm, opts).await;

    h.cmd(EngineCommand::Suggest);
    assert!(llm.wait_bodies(1, WAIT).await, "one chat request");
    let body = llm.last_body().expect("the body");
    let messages = body_messages(&body);
    assert_eq!(messages[0].0, "system");
    assert!(
        messages[0].1.ends_with("Prefers concise bullet answers."),
        "system message ends with the profile file: {}",
        messages[0].1
    );
    assert!(messages[0].1.contains("PROFILE:"));
    finish(&mut h).await;
    let _ = std::fs::remove_file(&path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_profile_warns_and_meeting_starts() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let missing = temp_path("missing-profile").with_extension("txt");
    let opts = MeetingOpts {
        profile_path: Some(missing.display().to_string()),
        ..MeetingOpts::default()
    };
    let mut h = running_me_only(&asr, &llm, opts).await;

    let statuses = h.statuses();
    assert!(
        statuses.iter().any(|(source, level, text)| {
            *source == StatusSource::App && *level == StatusLevel::Warn && text.contains("profile")
        }),
        "App Warn about the profile file: {statuses:?}"
    );
    assert!(h.states().contains(&MeetingState::Running));
    finish(&mut h).await;
}
