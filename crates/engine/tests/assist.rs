//! Automatic assistance through a real `Engine` over the scripted sources and
//! both mock servers: when each profile asks the model by itself, what it
//! sends, how silence (PASS), failures, overlap and profile switches behave.

mod support;

use std::time::Duration;

use clueless_types::events::{
    EngineCommand, MeetingState, Speaker, StatusLevel, StatusSource, SuggestionEnd, UiEvent,
};
use clueless_types::profile::AssistProfile;
use engine::deps::EngineTimings;
use tokio::time::timeout;

use support::*;

const WAIT: Duration = Duration::from_secs(8);
const PASS_RULE_END: &str = "reply with exactly: PASS";

async fn finish(h: &mut MeetingHarness) {
    h.cmd(EngineCommand::Shutdown);
    timeout(WAIT, &mut h.engine)
        .await
        .expect("engine loop returns")
        .expect("engine task does not panic");
}

fn opts(profile: AssistProfile) -> MeetingOpts {
    MeetingOpts {
        start_profile: profile,
        ..MeetingOpts::default()
    }
}

fn opts_with(profile: AssistProfile, timings: EngineTimings) -> MeetingOpts {
    MeetingOpts {
        start_profile: profile,
        timings,
        ..MeetingOpts::default()
    }
}

/// One live-like source of `speaker`; frames arrive at `speed` times real time.
fn source(speaker: Speaker, frames: usize, speed: f64) -> ScriptedFactory {
    ScriptedFactory::new(vec![(
        speaker,
        vec![OpenPlan::Ok(SourcePlan {
            frames,
            speed,
            never_end: true,
        })],
    )])
}

/// `count` short utterances back to back, each closed by 25 quiet frames.
fn utterances(count: usize) -> (usize, Vec<f32>) {
    let mut runs: Vec<(usize, f32)> = vec![(11, 0.0), (20, 0.9), (25, 0.0)];
    for _ in 1..count {
        runs.push((20, 0.9));
        runs.push((25, 0.0));
    }
    pattern(&runs)
}

async fn start(
    asr: &MockAsr,
    llm: &MockLlm,
    speaker: Speaker,
    count: usize,
    opts: MeetingOpts,
) -> MeetingHarness {
    let (frames, probs) = utterances(count);
    let h = MeetingHarness::start(
        asr,
        llm,
        source(speaker, frames, 1000.0),
        vec![VadScript::Probs(probs)],
        opts,
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);
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

fn system_content(body: &serde_json::Value) -> String {
    body_messages(body)
        .into_iter()
        .find(|(role, _)| role == "system")
        .expect("a system message")
        .1
}

fn transcript_part_of(content: &str) -> String {
    content.split("\n\n").next().unwrap_or(content).to_owned()
}

fn is_final(event: &UiEvent) -> bool {
    matches!(event, UiEvent::TranscriptFinal(_))
}

fn is_end(event: &UiEvent) -> bool {
    matches!(event, UiEvent::SuggestionEnd { .. })
}

fn starts(events: &[UiEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|event| match event {
            UiEvent::SuggestionStart { id } => Some(*id),
            _ => None,
        })
        .collect()
}

fn ends(events: &[UiEvent]) -> Vec<(u64, SuggestionEnd)> {
    events
        .iter()
        .filter_map(|event| match event {
            UiEvent::SuggestionEnd { id, end } => Some((*id, end.clone())),
            _ => None,
        })
        .collect()
}

fn deltas(events: &[UiEvent], id: u64) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            UiEvent::SuggestionDelta { id: got, text } if *got == id => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn index_of(events: &[UiEvent], pred: impl Fn(&UiEvent) -> bool) -> Option<usize> {
    events.iter().position(pred)
}

const QUESTION: &str = "could you send me the report tomorrow morning";

// ------------------------------------------------------------------ Manual

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_profile_sends_no_request_by_itself() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    let mut h = start(&asr, &llm, Speaker::Them, 1, opts(AssistProfile::Manual)).await;

    h.wait_until(WAIT, |events| events.iter().any(is_final))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(llm.body_count(), 0, "Manual never asks by itself");
    finish(&mut h).await;
}

// --------------------------------------------------------------- Interview

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_asks_by_itself_after_a_them_question() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    llm.enqueue(LlmReply::stream(&["Yes, tomorrow."]));
    let mut h = start(&asr, &llm, Speaker::Them, 1, opts(AssistProfile::Interview)).await;

    assert!(llm.wait_bodies(1, WAIT).await, "one chat request");
    let events = h.wait_until(WAIT, |events| events.iter().any(is_end)).await;

    let content = user_content(&llm.last_body().expect("body"));
    assert!(
        content.ends_with(PASS_RULE_END),
        "automatic requests end with the PASS rule: {content}"
    );
    assert!(
        content.contains(&format!("The last thing Them said was: \"{QUESTION}\"")),
        "the request quotes the question: {content}"
    );
    assert_eq!(starts(&events), vec![1]);
    assert_eq!(deltas(&events, 1), vec!["Yes, tomorrow.".to_owned()]);
    assert_eq!(ends(&events), vec![(1, SuggestionEnd::Done)]);
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_answer_pass_shows_nothing() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    llm.enqueue(LlmReply::stream(&["PASS"]));
    let mut h = start(&asr, &llm, Speaker::Them, 1, opts(AssistProfile::Interview)).await;

    let events = h.wait_until(WAIT, |events| events.iter().any(is_end)).await;

    assert_eq!(starts(&events), vec![1]);
    assert!(deltas(&events, 1).is_empty(), "PASS is never shown");
    assert_eq!(ends(&events), vec![(1, SuggestionEnd::Done)]);
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_answer_pass_split_in_two_chunks_shows_nothing() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    llm.enqueue(LlmReply::stream(&["Pa", "ss."]));
    let mut h = start(&asr, &llm, Speaker::Them, 1, opts(AssistProfile::Interview)).await;

    let events = h.wait_until(WAIT, |events| events.iter().any(is_end)).await;

    assert!(deltas(&events, 1).is_empty(), "Pass. is never shown");
    assert_eq!(ends(&events), vec![(1, SuggestionEnd::Done)]);
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_answer_starting_with_pass_is_shown_in_full_from_its_first_character() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    llm.enqueue(LlmReply::stream(&["Pass", "ing the test is step one"]));
    let mut h = start(&asr, &llm, Speaker::Them, 1, opts(AssistProfile::Interview)).await;

    let events = h.wait_until(WAIT, |events| events.iter().any(is_end)).await;

    assert_eq!(
        deltas(&events, 1),
        vec!["Passing the test is step one".to_owned()],
        "what was held back is released as one delta"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_ignores_a_filler_turn() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("right ok"));
    let mut h = start(&asr, &llm, Speaker::Them, 1, opts(AssistProfile::Interview)).await;

    h.wait_until(WAIT, |events| events.iter().any(is_final))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(llm.body_count(), 0, "8 characters and no question mark");
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_does_not_ask_for_the_users_own_speech() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("can you hear me okay over there?"));
    let mut h = start(&asr, &llm, Speaker::Me, 1, opts(AssistProfile::Interview)).await;

    h.wait_until(WAIT, |events| events.iter().any(is_final))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(llm.body_count(), 0, "Interview listens to Them only");
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_waits_for_the_next_piece_of_a_turn_before_asking_once() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("so how would you scale that?"));
    asr.enqueue_final(Respond::text("and how long would it take"));
    // Real-time frames: the second piece opens 6 quiet frames (about 190 ms)
    // after the first one is closed, well inside the 600 ms settle time.
    let timings = EngineTimings {
        turn_settle: Duration::from_millis(600),
        turn_max_wait: Duration::from_secs(5),
        ..fast_timings()
    };
    let (frames, probs) = utterances(2);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        source(Speaker::Them, frames, 1.0),
        vec![VadScript::Probs(probs)],
        opts_with(AssistProfile::Interview, timings),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    assert!(llm.wait_bodies(1, WAIT).await, "one chat request");
    tokio::time::sleep(Duration::from_millis(900)).await;

    assert_eq!(llm.body_count(), 1, "exactly one request for the turn");
    let content = user_content(&llm.bodies()[0]);
    assert!(
        content.contains("so how would you scale that?")
            && content.contains("and how long would it take"),
        "the request holds both pieces: {content}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_asks_while_them_is_still_busy_once_the_hard_deadline_passed() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let text = "so how would you scale that?";
    asr.set_default_final(Respond::text(text));
    asr.set_default_interim(Respond::text(text));
    // A committed question, then three seconds of speech that never pauses.
    let timings = EngineTimings {
        turn_settle: Duration::from_millis(300),
        turn_max_wait: Duration::from_millis(400),
        ..fast_timings()
    };
    let (frames, probs) = pattern(&[(11, 0.0), (20, 0.9), (19, 0.0), (94, 0.9), (25, 0.0)]);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        source(Speaker::Them, frames, 1.0),
        vec![VadScript::Probs(probs)],
        opts_with(AssistProfile::Interview, timings),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    assert!(
        llm.wait_bodies(1, WAIT).await,
        "a request despite the busy speaker"
    );
    let finals = h.snapshot().iter().filter(|event| is_final(event)).count();
    assert_eq!(finals, 1, "the second piece is still open when it starts");
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_second_turn_carries_the_previous_answer() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("so how would you scale that?"));
    asr.enqueue_final(Respond::Delay {
        ms: 500,
        then: Box::new(Respond::text("and what about the database layer?")),
    });
    llm.enqueue(LlmReply::stream(&["Shard by tenant."]));
    let mut h = start(&asr, &llm, Speaker::Them, 2, opts(AssistProfile::Interview)).await;

    assert!(llm.wait_bodies(2, WAIT).await, "two chat requests");

    let second = user_content(&llm.bodies()[1]);
    assert!(
        second.contains(
            "YOUR PREVIOUS ANSWER (already shown, add only what is new):\nShard by tenant."
        ),
        "the second request holds the first answer: {second}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_suggest_key_cancels_an_automatic_answer_and_drops_the_pass_rule() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    llm.enqueue(LlmReply::slow_stream(
        &["first ", "second ", "third"],
        Duration::from_millis(400),
    ));
    let mut h = start(&asr, &llm, Speaker::Them, 1, opts(AssistProfile::Interview)).await;

    h.wait_until(WAIT, |events| !deltas(events, 1).is_empty())
        .await;
    h.cmd(EngineCommand::Suggest);
    let events = h
        .wait_until(WAIT, |events| starts(events).contains(&2))
        .await;

    assert!(
        ends(&events).contains(&(1, SuggestionEnd::Cancelled)),
        "the automatic answer was cancelled: {events:?}"
    );
    assert!(
        index_of(&events, |e| matches!(
            e,
            UiEvent::SuggestionEnd { id: 1, .. }
        )) < index_of(&events, |e| matches!(e, UiEvent::SuggestionStart { id: 2 })),
        "the cancel comes before the new start"
    );
    assert!(llm.wait_bodies(2, WAIT).await, "the manual request");
    let manual = user_content(&llm.bodies()[1]);
    assert!(
        !manual.contains("PASS"),
        "a manual request has no PASS rule: {manual}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_http_error_goes_to_the_status_line_and_pauses_automatic_requests() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    asr.enqueue_final(Respond::Delay {
        ms: 200,
        then: Box::new(Respond::text("and the second question is this one?")),
    });
    asr.enqueue_final(Respond::Delay {
        ms: 1300,
        then: Box::new(Respond::text("and finally the third question?")),
    });
    llm.enqueue(LlmReply::Http {
        status: 500,
        body: "{}".to_owned(),
    });
    let mut h = start(&asr, &llm, Speaker::Them, 3, opts(AssistProfile::Interview)).await;

    // Pieces commit at about 0 ms, 200 ms (inside the 800 ms pause) and 1500 ms.
    assert!(llm.wait_bodies(2, WAIT).await, "the third piece asks again");
    h.wait_until(WAIT, |events| ends(events).len() >= 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(llm.body_count(), 2, "nothing was asked inside the pause");
    let events = h.snapshot();
    let error = index_of(&events, |e| {
        matches!(e, UiEvent::Status { source: StatusSource::Llm, level: StatusLevel::Error, text }
            if text == "LLM error 500")
    })
    .expect("the failure reaches the status line");
    let recovered = events.iter().enumerate().position(|(index, e)| {
        index > error
            && matches!(e, UiEvent::Status { source: StatusSource::Llm, level: StatusLevel::Info, text }
                if text.contains("reachable"))
    });
    assert!(recovered.is_some(), "a good answer restores the LLM status");
    assert!(
        events
            .iter()
            .all(|e| !matches!(e, UiEvent::SuggestionDelta { id: 1, .. })),
        "the failed answer shows no text"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_stream_closed_before_any_text_is_interrupted_and_pauses() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    asr.enqueue_final(Respond::Delay {
        ms: 200,
        then: Box::new(Respond::text("and the second question is this one?")),
    });
    llm.enqueue(LlmReply::CloseMidStream(Vec::new()));
    let mut h = start(&asr, &llm, Speaker::Them, 2, opts(AssistProfile::Interview)).await;

    let events = h.wait_until(WAIT, |events| events.iter().any(is_end)).await;
    assert_eq!(ends(&events), vec![(1, SuggestionEnd::Interrupted)]);
    h.wait_until(WAIT, |events| {
        events.iter().filter(|e| is_final(e)).count() >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(llm.body_count(), 1, "the pause holds back the second turn");
    assert!(
        h.statuses().iter().any(|(source, level, text)| {
            *source == StatusSource::Llm
                && *level == StatusLevel::Error
                && text == "LLM answer interrupted"
        }),
        "the interruption reaches the status line: {:?}",
        h.statuses()
    );
    finish(&mut h).await;
}

// -------------------------------------------------------------- Brainstorm

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn brainstorm_spaces_requests_by_four_times_the_gap_and_never_overlaps() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("we could cache the results"));
    asr.enqueue_final(Respond::Delay {
        ms: 200,
        then: Box::new(Respond::text("or shard by customer id")),
    });
    asr.enqueue_final(Respond::Delay {
        ms: 50,
        then: Box::new(Respond::text("maybe use a queue instead")),
    });
    let mut h = start(&asr, &llm, Speaker::Me, 3, opts(AssistProfile::Brainstorm)).await;

    assert!(llm.wait_bodies(2, WAIT).await, "two requests");
    tokio::time::sleep(Duration::from_millis(900)).await;

    assert_eq!(llm.body_count(), 2, "pieces 2 and 3 share one request");
    let arrivals = llm.arrivals();
    let gap = arrivals[1].duration_since(arrivals[0]);
    assert!(
        gap >= Duration::from_millis(580),
        "four times the 150 ms gap, got {gap:?}"
    );
    assert_eq!(llm.max_inflight(), 1);
    let second = user_content(&llm.bodies()[1]);
    assert!(
        second.contains("or shard by customer id") && second.contains("maybe use a queue instead"),
        "the second request sees both later pieces: {second}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn brainstorm_does_not_cancel_a_running_answer_and_asks_once_after_it() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("we could cache the results"));
    asr.enqueue_final(Respond::Delay {
        ms: 150,
        then: Box::new(Respond::text("or shard by customer id")),
    });
    asr.enqueue_final(Respond::Delay {
        ms: 150,
        then: Box::new(Respond::text("maybe use a queue instead")),
    });
    llm.enqueue(LlmReply::slow_stream(
        &["one ", "two ", "three"],
        Duration::from_millis(300),
    ));
    let mut h = start(&asr, &llm, Speaker::Me, 3, opts(AssistProfile::Brainstorm)).await;

    let events = h
        .wait_until(WAIT, |events| starts(events).contains(&2))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let events_after = h.snapshot();
    assert_eq!(
        ends(&events_after)
            .into_iter()
            .filter(|(_, end)| *end == SuggestionEnd::Cancelled)
            .count(),
        0,
        "no answer was cancelled"
    );
    assert_eq!(ends(&events_after)[0], (1, SuggestionEnd::Done));
    assert!(
        index_of(&events, |e| matches!(
            e,
            UiEvent::SuggestionEnd { id: 1, .. }
        )) < index_of(&events, |e| matches!(e, UiEvent::SuggestionStart { id: 2 })),
        "the second request starts after the first answer ended"
    );
    assert_eq!(llm.body_count(), 2, "exactly one more request");
    finish(&mut h).await;
}

// ------------------------------------------------------------- switching

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cycling_to_brainstorm_makes_the_next_me_piece_ask_with_the_brainstorm_instruction() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::Delay {
        ms: 400,
        then: Box::new(Respond::text("we could cache the results of the query")),
    });
    let mut h = start(&asr, &llm, Speaker::Me, 1, opts(AssistProfile::Interview)).await;
    assert!(h.wait_state(MeetingState::Running, WAIT).await);
    h.cmd(EngineCommand::CycleProfile);

    assert!(llm.wait_bodies(1, WAIT).await, "one chat request");
    let content = user_content(&llm.bodies()[0]);
    assert!(
        content.contains("I am talking."),
        "Brainstorm instruction: {content}"
    );
    let profiles: Vec<AssistProfile> = h
        .snapshot()
        .into_iter()
        .filter_map(|event| match event {
            UiEvent::Profile(profile) => Some(profile),
            _ => None,
        })
        .collect();
    assert_eq!(
        profiles,
        vec![AssistProfile::Interview, AssistProfile::Brainstorm]
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_profile_chosen_while_idle_applies_to_the_next_meeting() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    let (frames, probs) = utterances(1);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        source(Speaker::Them, frames, 1000.0),
        vec![VadScript::Probs(probs)],
        opts(AssistProfile::Manual),
    )
    .await;
    h.cmd(EngineCommand::CycleProfile);
    h.wait_until(WAIT, |events| {
        events.contains(&UiEvent::Profile(AssistProfile::Interview))
    })
    .await;
    assert_eq!(llm.body_count(), 0);

    h.cmd(EngineCommand::StartMeeting);

    assert!(
        llm.wait_bodies(1, WAIT).await,
        "Interview asks in the new meeting"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn picking_manual_while_a_question_settles_cancels_the_pending_ask() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    let timings = EngineTimings {
        turn_settle: Duration::from_millis(600),
        ..fast_timings()
    };
    let mut h = start(
        &asr,
        &llm,
        Speaker::Them,
        1,
        opts_with(AssistProfile::Interview, timings),
    )
    .await;

    h.wait_until(WAIT, |events| events.iter().any(is_final))
        .await;
    h.cmd(EngineCommand::SetProfile(AssistProfile::Manual));
    tokio::time::sleep(Duration::from_millis(1000)).await;

    assert_eq!(llm.body_count(), 0, "the waiting trigger was dropped");
    assert!(
        h.snapshot()
            .contains(&UiEvent::Profile(AssistProfile::Manual))
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_under_different_profiles_share_the_system_message_and_extend_the_transcript() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("so how would you scale that?"));
    asr.enqueue_final(Respond::Delay {
        ms: 600,
        then: Box::new(Respond::text("and what about the database layer?")),
    });
    let mut h = start(&asr, &llm, Speaker::Them, 2, opts(AssistProfile::Manual)).await;

    h.wait_until(WAIT, |events| events.iter().any(is_final))
        .await;
    h.cmd(EngineCommand::Suggest);
    assert!(llm.wait_bodies(1, WAIT).await, "the manual request");
    h.cmd(EngineCommand::SetProfile(AssistProfile::Interview));
    assert!(llm.wait_bodies(2, WAIT).await, "the automatic request");

    let bodies = llm.bodies();
    assert_eq!(system_content(&bodies[0]), system_content(&bodies[1]));
    let first = transcript_part_of(&user_content(&bodies[0]));
    let second = transcript_part_of(&user_content(&bodies[1]));
    assert!(
        second.starts_with(&first),
        "the second transcript extends the first:\nfirst: {first}\nsecond: {second}"
    );
    finish(&mut h).await;
}

// --------------------------------------------------- lifecycle and ordering

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sources_drained_arrives_after_the_last_automatic_answer_ended() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    llm.enqueue(LlmReply::slow_stream(
        &["a ", "b ", "c"],
        Duration::from_millis(200),
    ));
    let (frames, probs) = utterances(1);
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        ScriptedFactory::new(vec![(
            Speaker::Them,
            vec![OpenPlan::Ok(SourcePlan::new(frames))],
        )]),
        vec![VadScript::Probs(probs)],
        opts(AssistProfile::Interview),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);

    let events = h
        .wait_until(WAIT, |events| events.contains(&UiEvent::SourcesDrained))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let events_after = h.snapshot();

    assert!(
        index_of(&events, |e| matches!(
            e,
            UiEvent::SuggestionEnd {
                id: 1,
                end: SuggestionEnd::Done
            }
        ))
        .expect("the answer ended")
            < index_of(&events, |e| *e == UiEvent::SourcesDrained).expect("drained"),
        "SourcesDrained waits for the answer: {events:?}"
    );
    assert_eq!(
        events_after
            .iter()
            .filter(|e| **e == UiEvent::SourcesDrained)
            .count(),
        1,
        "emitted once"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_meeting_cancels_a_streaming_automatic_answer_before_idle() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    llm.enqueue(LlmReply::slow_stream(
        &["x ", "y ", "z"],
        Duration::from_millis(500),
    ));
    let mut h = start(&asr, &llm, Speaker::Them, 1, opts(AssistProfile::Interview)).await;

    h.wait_until(WAIT, |events| !deltas(events, 1).is_empty())
        .await;
    h.cmd(EngineCommand::StopMeeting);
    assert!(h.wait_state(MeetingState::Idle, WAIT).await);

    assert!(
        h.ordered(
            |e| matches!(
                e,
                UiEvent::SuggestionEnd {
                    id: 1,
                    end: SuggestionEnd::Cancelled
                }
            ),
            |e| *e == UiEvent::MeetingState(MeetingState::Idle),
        ),
        "Cancelled precedes Idle: {:?}",
        h.snapshot()
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggestion_ids_increase_across_manual_and_automatic_requests() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("so how would you scale that?"));
    asr.enqueue_final(Respond::Delay {
        ms: 1000,
        then: Box::new(Respond::text("and what about the database layer?")),
    });
    let mut h = start(&asr, &llm, Speaker::Them, 2, opts(AssistProfile::Interview)).await;

    h.wait_until(WAIT, |events| events.iter().any(is_end)).await;
    h.cmd(EngineCommand::Suggest);
    let events = h.wait_until(WAIT, |events| starts(events).len() >= 3).await;

    assert_eq!(starts(&events), vec![1, 2, 3]);
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interview_asks_right_after_the_settle_time_when_them_is_quiet() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text(QUESTION));
    // A long hard deadline: only a wrongly busy speaker could make the
    // request wait for it.
    let timings = EngineTimings {
        turn_settle: Duration::from_millis(50),
        turn_max_wait: Duration::from_secs(5),
        ..fast_timings()
    };
    let mut h = start(
        &asr,
        &llm,
        Speaker::Them,
        1,
        opts_with(AssistProfile::Interview, timings),
    )
    .await;

    assert!(
        llm.wait_bodies(1, Duration::from_millis(2500)).await,
        "a quiet speaker is asked about after the settle time, not the hard deadline"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starting_a_meeting_that_is_already_running_does_nothing() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    let mut h = start(&asr, &llm, Speaker::Me, 1, opts(AssistProfile::Manual)).await;
    assert!(h.wait_state(MeetingState::Running, WAIT).await);

    h.cmd(EngineCommand::StartMeeting);
    tokio::time::sleep(Duration::from_millis(400)).await;

    assert_eq!(
        h.states(),
        vec![MeetingState::Starting, MeetingState::Running]
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_the_feed_makes_the_model_forget_the_previous_answer() {
    let asr = MockAsr::start().await;
    let llm = MockLlm::start().await;
    asr.enqueue_final(Respond::text("so how would you scale that?"));
    asr.enqueue_final(Respond::Delay {
        ms: 600,
        then: Box::new(Respond::text("and what about the database layer?")),
    });
    llm.enqueue(LlmReply::stream(&["Shard by tenant."]));
    let mut h = start(&asr, &llm, Speaker::Them, 2, opts(AssistProfile::Interview)).await;

    h.wait_until(WAIT, |events| events.iter().any(is_end)).await;
    h.cmd(EngineCommand::ClearSuggestion);
    assert!(llm.wait_bodies(2, WAIT).await, "the second turn asks again");

    let second = user_content(&llm.bodies()[1]);
    assert!(
        !second.contains("YOUR PREVIOUS ANSWER"),
        "a cleared feed leaves nothing to repeat: {second}"
    );
    finish(&mut h).await;
}
