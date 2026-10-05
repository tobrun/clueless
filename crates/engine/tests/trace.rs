//! The transcription side of a recorded meeting: every test drives a real
//! `Engine` through `MeetingHarness` with a `MemoryOpener` and asserts on
//! the records the meeting left behind.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;

use clueless_types::UiEvent;
use clueless_types::audio::SourceError;
use clueless_types::events::{EngineCommand, MeetingState, Speaker, StatusLevel, StatusSource};
use clueless_types::profile::AssistProfile;
use segmenter::machine::MachineParams;
use trace::record::{
    AsrOutcome, Body, CommandName, DropReason, EndReason, MeetingState as TraceState,
    PolicyOutcome, Profile, SegmentKind, Speaker as TraceSpeaker,
};
use trace::sink::{Location, MemoryOpener};

use support::*;

/// One closed Me utterance on a single speaker.
fn one_utterance() -> (ScriptedFactory, Vec<VadScript>) {
    let (frames, probs) = pattern(&[(11, 0.0), (10, 0.9), (25, 0.0)]);
    (
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan::new(frames))],
        )]),
        vec![VadScript::Probs(probs)],
    )
}

/// One Me source streaming the `runs` loudness pattern at `speed`, with its
/// vad script.
fn me_source(
    runs: &[(usize, f32)],
    speed: f64,
    never_end: bool,
) -> (ScriptedFactory, Vec<VadScript>) {
    let (frames, probs) = pattern(runs);
    (
        ScriptedFactory::new(vec![(
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan {
                frames,
                speed,
                never_end,
            })],
        )]),
        vec![VadScript::Probs(probs)],
    )
}

/// Run one utterance through a traced meeting until its drop is recorded, stop
/// the meeting, and return the harness with the recorded bodies.
async fn dropped_once(asr: &MockAsr, llm: &MockLlm) -> (MeetingHarness, Vec<Body>) {
    let opener = opener();
    let (factory, vad) = one_utterance();
    let h = running_tracing_meeting(asr, llm, factory, vad, &opener, MeetingOpts::default()).await;
    h.wait_dropped().await;
    stop_and_wait(&h).await;
    (h, last_bodies(&single_trace(&opener)))
}

fn is_state(state: TraceState) -> impl Fn(&Body) -> bool {
    move |body| *body == Body::MeetingState { state }
}

/// A memory opener announcing the session directory, with audio or without.
fn location_opener(audio: bool) -> Arc<MemoryOpener> {
    Arc::new(MemoryOpener::with_location(Location {
        dir: PathBuf::from("/tmp/clueless-session"),
        audio,
    }))
}

/// True when an App Info status announces exactly `text`.
fn announces(statuses: &[(StatusSource, StatusLevel, String)], text: &str) -> bool {
    statuses.iter().any(|(source, level, t)| {
        *source == StatusSource::App && *level == StatusLevel::Info && t == text
    })
}

// ------------------------------------------------------------------ happy path

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recorded_meeting_leaves_the_transcription_side_in_order() {
    let (asr, llm) = start_mocks().await;
    asr.set_default_final(Respond::text("mock words"));
    let opener = opener();
    let (factory, vad) = one_utterance();
    let h =
        running_tracing_meeting(&asr, &llm, factory, vad, &opener, MeetingOpts::default()).await;
    h.wait_final().await;
    stop_and_wait(&h).await;
    let trace = single_trace(&opener);
    let bodies = last_bodies(&trace);

    let final_text = bodies
        .iter()
        .find_map(|b| match b {
            Body::TranscriptFinal { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("a transcript_final record");
    assert_eq!(final_text, "mock words");

    let in_order = ordered(
        &bodies,
        &[
            &is_state(TraceState::Starting),
            &|b| *b == Body::ClockStarted,
            &is_state(TraceState::Running),
            &|b| {
                matches!(
                    b,
                    Body::Segment {
                        segment_kind: SegmentKind::Final,
                        ..
                    }
                )
            },
            &|b| {
                matches!(
                    b,
                    Body::AsrCall {
                        outcome: AsrOutcome::Text,
                        ..
                    }
                )
            },
            &|b| matches!(b, Body::TranscriptFinal { text, .. } if text == "mock words"),
            &|b| {
                matches!(
                    b,
                    Body::PieceDone {
                        speaker: TraceSpeaker::Me,
                        chars: Some(_)
                    }
                )
            },
            &is_state(TraceState::Stopping),
        ],
    );
    assert!(in_order, "records in order: {bodies:#?}");
    assert_eq!(
        bodies.last(),
        Some(&Body::End {
            reason: EndReason::Stop
        }),
        "the last record is the end with reason stop"
    );

    finish(&mut { h }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_trace_is_closed_by_the_time_idle_arrives() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let (factory, vad) = one_utterance();
    let h =
        running_tracing_meeting(&asr, &llm, factory, vad, &opener, MeetingOpts::default()).await;
    stop_and_wait(&h).await;
    // The harness has received `Idle`; the trace was closed before it.
    let trace = single_trace(&opener);
    assert_eq!(trace.closed(), Some(EndReason::Stop));
    let bodies = last_bodies(&trace);
    assert!(
        !bodies.contains(&Body::MeetingState {
            state: TraceState::Idle,
        }),
        "no idle record: {bodies:#?}"
    );
    finish(&mut { h }).await;
}

// ------------------------------------------------------------------ drop reasons

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_speech_records_its_call_and_drop_reason() {
    let (asr, llm) = start_mocks().await;
    asr.set_default_final(Respond::text("   "));
    let (mut h, bodies) = dropped_once(&asr, &llm).await;
    assert!(
        ordered(
            &bodies,
            &[
                &|b| matches!(
                    b,
                    Body::AsrCall {
                        outcome: AsrOutcome::NoSpeech,
                        raw_text: None,
                        ..
                    }
                ),
                &|b| matches!(
                    b,
                    Body::UtteranceDropped {
                        reason: DropReason::NoSpeech,
                        ..
                    }
                ),
            ],
        ),
        "no_speech call and drop: {bodies:#?}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_permanently_failing_asr_records_error_and_drop_reason() {
    let (asr, llm) = start_mocks().await;
    // Three 500s exhaust the final worker's retries in one call.
    for _ in 0..3 {
        asr.enqueue_final(Respond::Status(500));
    }
    let (mut h, bodies) = dropped_once(&asr, &llm).await;
    let call = bodies
        .iter()
        .find_map(|b| match b {
            Body::AsrCall {
                outcome: AsrOutcome::Error,
                error,
                ..
            } => Some(error.clone()),
            _ => None,
        })
        .expect("one errored asr_call");
    assert!(
        call.as_deref().is_some_and(|t| t.contains("500")),
        "the error carries the status: {call:?}"
    );
    assert!(
        bodies.iter().any(|b| matches!(
            b,
            Body::UtteranceDropped {
                reason: DropReason::AsrError,
                ..
            }
        )),
        "asr_error drop: {bodies:#?}"
    );
    assert_eq!(
        bodies
            .iter()
            .filter(|b| matches!(b, Body::AsrCall { .. }))
            .count(),
        1,
        "one record for the whole call, not one per retry"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_echo_me_final_records_the_verdict_and_the_drop() {
    let (asr, llm) = start_mocks().await;
    asr.set_default_final(Respond::text("hello there"));
    let opener = opener();
    // Same shape as the pipeline-level echo test: Them ends first, Me's copy
    // of the same words arrives inside the echo window.
    let (them_frames, them_probs) = pattern(&[(11, 0.0), (15, 0.9), (20, 0.0)]);
    let (me_frames, me_probs) = pattern(&[(11, 0.0), (25, 0.9), (20, 0.0)]);
    let factory = ScriptedFactory::new(vec![
        (
            Speaker::Me,
            vec![OpenPlan::Ok(SourcePlan {
                frames: me_frames,
                speed: 2.0,
                never_end: false,
            })],
        ),
        (
            Speaker::Them,
            vec![OpenPlan::Ok(SourcePlan {
                frames: them_frames,
                speed: 2.0,
                never_end: false,
            })],
        ),
    ]);
    let h = running_tracing_meeting(
        &asr,
        &llm,
        factory,
        vec![VadScript::Probs(me_probs), VadScript::Probs(them_probs)],
        &opener,
        MeetingOpts::default(),
    )
    .await;
    h.wait_until(WAIT, |e| {
        matches!(
            e.iter().find(|e| matches!(e, UiEvent::TranscriptFinal(_))),
            Some(UiEvent::TranscriptFinal(u)) if u.id.speaker == Speaker::Them
        )
    })
    .await;
    // Me's final lands ~0.4 s later; give it room, then stop.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let bodies = stopped_bodies(&h, &opener).await;
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, Body::EchoCheck { echo: true, .. })),
        "echo verdict recorded: {bodies:#?}"
    );
    assert!(
        bodies.iter().any(|b| matches!(
            b,
            Body::UtteranceDropped {
                speaker: TraceSpeaker::Me,
                reason: DropReason::Echo,
                ..
            }
        )),
        "Me dropped with reason echo: {bodies:#?}"
    );
    assert!(
        !bodies.iter().any(|b| matches!(
            b,
            Body::TranscriptFinal {
                speaker: TraceSpeaker::Me,
                ..
            }
        )),
        "the echoed Me final never commits: {bodies:#?}"
    );
    finish(&mut { h }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_me_final_without_them_overlap_records_echo_false() {
    let (asr, llm) = start_mocks().await;
    asr.set_default_final(Respond::text("only me"));
    let opener = opener();
    let (frames, probs) = pattern(&[(11, 0.0), (10, 0.9), (25, 0.0)]);
    let factory = ScriptedFactory::new(vec![
        (Speaker::Me, vec![OpenPlan::Ok(SourcePlan::new(frames))]),
        (Speaker::Them, vec![OpenPlan::Ok(SourcePlan::endless(200))]),
    ]);
    // The Them detector scores the endless silence as silence forever.
    let h = running_tracing_meeting(
        &asr,
        &llm,
        factory,
        vec![VadScript::Probs(probs), VadScript::Probs(Vec::new())],
        &opener,
        MeetingOpts::default(),
    )
    .await;
    h.wait_final().await;
    let bodies = stopped_bodies(&h, &opener).await;
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, Body::EchoCheck { echo: false, .. })),
        "echo false on the commit path: {bodies:#?}"
    );
    finish(&mut { h }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_final_queue_records_the_queue_full_drop() {
    let (asr, llm) = start_mocks().await;
    asr.block();
    asr.enqueue_final(Respond::Blocked(Box::new(Respond::text("first"))));
    asr.set_default_final(Respond::text("q"));
    let opener = opener();
    // 18 utterances: 1 in flight + 16 queued + 1 dropped.
    let mut runs = vec![(11usize, 0.0f32)];
    for _ in 0..18 {
        runs.push((10, 0.9));
        runs.push((20, 0.0));
    }
    let (factory, vad) = me_source(&runs, 10.0, true);
    let h =
        running_tracing_meeting(&asr, &llm, factory, vad, &opener, MeetingOpts::default()).await;
    h.wait_dropped().await;
    let bodies = last_bodies(&single_trace(&opener));
    assert!(
        bodies.iter().any(|b| matches!(
            b,
            Body::UtteranceDropped {
                reason: DropReason::QueueFull,
                ..
            }
        )),
        "queue_full recorded: {bodies:#?}"
    );
    asr.release();
    stop_and_wait(&h).await;
    finish(&mut { h }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_while_a_final_is_blocked_records_the_cancelled_drop() {
    let (asr, llm) = start_mocks().await;
    asr.block();
    asr.enqueue_final(Respond::Blocked(Box::new(Respond::text("late"))));
    let opener = opener();
    let (factory, vad) = me_source(&[(11, 0.0), (10, 0.9), (25, 0.0)], 20.0, true);
    let h =
        running_tracing_meeting(&asr, &llm, factory, vad, &opener, MeetingOpts::default()).await;
    assert!(
        asr.wait_requests(1, WAIT).await,
        "the final request reaches the mock"
    );
    stop_and_wait(&h).await;
    asr.release();
    let bodies = last_bodies(&single_trace(&opener));
    assert!(
        ordered(
            &bodies,
            &[
                &|b| matches!(
                    b,
                    Body::AsrCall {
                        outcome: AsrOutcome::Cancelled,
                        ..
                    }
                ),
                &|b| matches!(
                    b,
                    Body::UtteranceDropped {
                        reason: DropReason::Cancelled,
                        ..
                    }
                ),
                &|b| *b
                    == Body::End {
                        reason: EndReason::Stop
                    },
            ],
        ),
        "cancelled call, drop and stop end: {bodies:#?}"
    );
    finish(&mut { h }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overlap_strip_leaving_no_text_records_empty_after_overlap() {
    let (asr, llm) = start_mocks().await;
    // The forced-cut second piece repeats the first one whole, so the strip
    // removes everything.
    asr.set_final_min_ms(2_048);
    asr.enqueue_final(Respond::text("we ship on friday"));
    asr.enqueue_final(Respond::text("we ship on friday"));
    let opener = opener();
    let (_, probs) = pattern(&[(3, 0.0), (120, 0.9), (25, 0.0)]);
    let factory = ScriptedFactory::new(vec![(
        Speaker::Me,
        vec![OpenPlan::Ok(SourcePlan {
            frames: 148,
            speed: 1000.0,
            never_end: false,
        })],
    )]);
    let opts = MeetingOpts {
        machine: MachineParams {
            max_segment_ms: 3_000,
            ..MachineParams::default()
        },
        ..MeetingOpts::default()
    };
    let h = running_tracing_meeting(
        &asr,
        &llm,
        factory,
        vec![VadScript::Probs(probs)],
        &opener,
        opts,
    )
    .await;
    h.wait_dropped().await;
    let bodies = stopped_bodies(&h, &opener).await;
    assert!(
        bodies.iter().any(|b| matches!(
            b,
            Body::UtteranceDropped {
                reason: DropReason::EmptyAfterOverlap,
                ..
            }
        )),
        "empty_after_overlap recorded: {bodies:#?}"
    );
    finish(&mut { h }).await;
}

// ------------------------------------------------------------------ interims & audio

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answered_interim_records_its_call_and_text() {
    let (asr, llm) = start_mocks().await;
    // At 20x speed the 1984 ms interim is sent and answered ~3.1 s in, long
    // before the final segment queues at ~4.6 s (their transcribe.rs twin);
    // the final itself is held at the mock so its watermark cannot pass the
    // interim while its response is in flight.
    asr.set_final_min_ms(2_600);
    asr.block();
    asr.enqueue_interim(Respond::text("hel"));
    asr.enqueue_final(Respond::Blocked(Box::new(Respond::text("hello"))));
    let opener = opener();
    let (factory, vad) = me_source(&[(11, 0.0), (80, 0.9), (20, 0.0)], 20.0, false);
    let h =
        running_tracing_meeting(&asr, &llm, factory, vad, &opener, MeetingOpts::default()).await;
    h.wait_interim().await;
    asr.release();
    h.wait_final().await;
    let bodies = stopped_bodies(&h, &opener).await;
    assert_eq!(
        bodies
            .iter()
            .filter(|b| matches!(
                b,
                Body::TranscriptInterim { text, .. } if text == "hel"
            ))
            .count(),
        1,
        "exactly one interim record: {bodies:#?}"
    );
    assert!(
        bodies.iter().any(|b| matches!(
            b,
            Body::AsrCall {
                segment_kind: SegmentKind::Interim,
                outcome: AsrOutcome::Text,
                ..
            }
        )),
        "interim asr_call recorded: {bodies:#?}"
    );
    finish(&mut { h }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_seconds_of_audio_record_a_frame_time_every_32_ms() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    // 62 frames of 512 samples at 16 kHz = 1.984 s of meeting audio.
    let factory =
        ScriptedFactory::new(vec![(Speaker::Me, vec![OpenPlan::Ok(SourcePlan::new(62))])]);
    let h = running_tracing_meeting(
        &asr,
        &llm,
        factory,
        silence_vad(),
        &opener,
        MeetingOpts::default(),
    )
    .await;
    stop_and_wait(&h).await;
    let trace = single_trace(&opener);
    let times = trace.audio_times(TraceSpeaker::Me);
    assert!(
        (60..=63).contains(&times.len()),
        "about two seconds of frames: {times:?}"
    );
    for pair in times.windows(2) {
        assert_eq!(pair[1] - pair[0], 32, "frames sit 32 ms apart: {times:?}");
    }
    finish(&mut { h }).await;
}

// ------------------------------------------------------------------ commands & lifecycle

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_received_while_running_are_recorded_in_order() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let opts = MeetingOpts {
        start_profile: AssistProfile::Manual,
        ..trace_opts(&opener)
    };
    let mut h = running_me_with(&asr, &llm, opts).await;
    h.cmd(EngineCommand::Suggest);
    assert!(
        llm.wait_bodies(1, WAIT).await,
        "the suggestion reached the llm"
    );
    h.cmd(EngineCommand::CycleProfile);
    h.cmd(EngineCommand::SetProfile(AssistProfile::Brainstorm));
    h.cmd(EngineCommand::StopMeeting);
    h.wait_state(MeetingState::Idle, WAIT).await;
    let bodies = last_bodies(&single_trace(&opener));
    let commands: Vec<_> = bodies
        .iter()
        .filter_map(|b| match b {
            Body::Command { command, profile } => Some((*command, *profile)),
            _ => None,
        })
        .collect();
    assert_eq!(
        commands,
        vec![
            (CommandName::Suggest, None),
            (CommandName::CycleProfile, None),
            (CommandName::SetProfile, Some(Profile::Brainstorm)),
            (CommandName::StopMeeting, None),
        ],
        "the four commands in arrival order"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_suggestion_while_idle_opens_no_trace() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let mut h = idle_tracing_meeting(&asr, &llm, &opener, MeetingOpts::default()).await;
    h.cmd(EngineCommand::Suggest);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(opener.traces().is_empty(), "no trace for an idle suggest");
    assert_eq!(llm.body_count(), 0, "no request left the engine");
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_meetings_in_one_run_each_get_their_own_trace() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let mut h = tracing_meeting(
        &asr,
        &llm,
        endless_me_factory(2),
        silence_vad(),
        &opener,
        MeetingOpts::default(),
    )
    .await;
    for round in 0..2 {
        h.cmd(EngineCommand::StartMeeting);
        assert!(
            h.wait_state(MeetingState::Running, WAIT).await,
            "meeting {round} runs"
        );
        h.cmd(EngineCommand::StopMeeting);
        // Wait for one more Idle than seen so far (this round's).
        let seen = h
            .states()
            .iter()
            .filter(|s| **s == MeetingState::Idle)
            .count();
        h.wait_until(WAIT, |e| {
            e.iter()
                .filter(|e| **e == UiEvent::MeetingState(MeetingState::Idle))
                .count()
                > seen
        })
        .await;
        assert_eq!(
            h.states()
                .iter()
                .filter(|s| **s == MeetingState::Idle)
                .count(),
            seen + 1,
            "meeting {round} ends in exactly one Idle"
        );
    }
    let traces = opener.traces();
    assert_eq!(traces.len(), 2, "one trace per meeting");
    for trace in traces.iter() {
        let records = trace.records();
        assert_eq!(records.first().map(|r| r.seq), Some(1), "seq starts at 1");
        assert_eq!(
            records.last().map(|r| r.body.clone()),
            Some(Body::End {
                reason: EndReason::Stop
            })
        );
    }
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_meeting_with_no_openable_source_ends_the_trace_start_failed() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let factory = ScriptedFactory::new(vec![(
        Speaker::Me,
        vec![OpenPlan::Fail(SourceError::DeviceNotFound(
            "no such input".into(),
        ))],
    )]);
    let mut h = tracing_meeting(
        &asr,
        &llm,
        factory,
        silence_vad(),
        &opener,
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);
    assert!(
        h.wait_state(MeetingState::Idle, WAIT).await,
        "the failed start returns to idle"
    );
    let trace = single_trace(&opener);
    assert_eq!(trace.closed(), Some(EndReason::StartFailed));
    let bodies = last_bodies(&trace);
    assert_eq!(
        bodies.last(),
        Some(&Body::End {
            reason: EndReason::StartFailed
        })
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_component_ends_the_trace_with_panic() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let mut h = tracing_meeting(
        &asr,
        &llm,
        endless_me_factory(1),
        vec![VadScript::PanicsAt(3)],
        &opener,
        MeetingOpts::default(),
    )
    .await;
    h.cmd(EngineCommand::StartMeeting);
    assert!(
        h.wait_state(MeetingState::Idle, WAIT).await,
        "the panic stops the meeting"
    );
    let trace = single_trace(&opener);
    assert_eq!(trace.closed(), Some(EndReason::Panic));
    let bodies = last_bodies(&trace);
    assert_eq!(
        bodies.last(),
        Some(&Body::End {
            reason: EndReason::Panic
        })
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_a_meeting_ends_the_trace_with_shutdown() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let mut h = running_me_with(&asr, &llm, trace_opts(&opener)).await;
    finish(&mut h).await;
    let trace = single_trace(&opener);
    assert_eq!(trace.closed(), Some(EndReason::Shutdown));
    let bodies = last_bodies(&trace);
    assert_eq!(
        bodies.last(),
        Some(&Body::End {
            reason: EndReason::Shutdown
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_command_channel_ends_the_trace_channel_closed() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let h = running_me_with(&asr, &llm, trace_opts(&opener)).await;
    let engine = h.engine;
    let MeetingHarness {
        events,
        commands,
        started,
        engine: _,
    } = h;
    let _ = started;
    drop(events);
    drop(commands);
    timeout(WAIT, engine)
        .await
        .expect("the loop finishes")
        .expect("no panic");
    let trace = single_trace(&opener);
    assert_eq!(trace.closed(), Some(EndReason::ChannelClosed));
    let bodies = last_bodies(&trace);
    assert_eq!(
        bodies.last(),
        Some(&Body::End {
            reason: EndReason::ChannelClosed
        })
    );
}

// ------------------------------------------------------------------ notes, policy, opener failures

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_notes_file_is_recorded_once_per_meeting() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let path = temp_path("notes");
    std::fs::write(&path, "I am Tobrun").expect("write notes");
    let opts = MeetingOpts {
        notes_path: Some(path.display().to_string()),
        ..trace_opts(&opener)
    };
    let mut h = running_me_with(&asr, &llm, opts).await;
    let bodies = stopped_bodies(&h, &opener).await;
    assert_eq!(
        bodies
            .iter()
            .filter(|b| matches!(b, Body::Notes { .. }))
            .count(),
        1
    );
    assert!(
        bodies.iter().any(|b| matches!(
            b,
            Body::Notes { text } if text == "I am Tobrun"
        )),
        "the notes text: {bodies:#?}"
    );
    std::fs::remove_file(&path).ok();
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_notes_path_there_is_no_notes_record() {
    let (asr, llm) = start_mocks().await;
    let opener = opener();
    let mut h = running_me_with(&asr, &llm, trace_opts(&opener)).await;
    let bodies = stopped_bodies(&h, &opener).await;
    assert!(
        !bodies.iter().any(|b| matches!(b, Body::Notes { .. })),
        "no notes record: {bodies:#?}"
    );
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_policy_records_its_waiting_and_fired_outcomes() {
    let (asr, llm) = start_mocks().await;
    asr.enqueue_final(Respond::text("what happens to the budget next quarter?"));
    llm.enqueue(LlmReply::stream(&["It doubles."]));
    let opener = opener();
    let (factory, vad) = one_them_turn();
    let opts = MeetingOpts {
        start_profile: AssistProfile::Interview,
        ..MeetingOpts::default()
    };
    let mut h = running_tracing_meeting(&asr, &llm, factory, vad, &opener, opts).await;
    assert!(
        llm.wait_bodies(1, WAIT).await,
        "the automatic request fires"
    );
    let bodies = loop_bodies_until(&opener, |bodies| {
        bodies.iter().any(|b| {
            matches!(
                b,
                Body::Policy {
                    outcome: PolicyOutcome::Fired,
                    suggestion: Some(1),
                    ..
                }
            )
        })
    })
    .await;
    let in_order = ordered(
        &bodies,
        &[
            &|b| {
                matches!(
                    b,
                    Body::PieceDone {
                        speaker: TraceSpeaker::Them,
                        ..
                    }
                )
            },
            &|b| {
                matches!(
                    b,
                    Body::Policy {
                        outcome: PolicyOutcome::Waiting,
                        profile: Profile::Interview,
                        suggestion: None,
                    }
                )
            },
            &|b| {
                matches!(
                    b,
                    Body::Policy {
                        outcome: PolicyOutcome::Fired,
                        profile: Profile::Interview,
                        suggestion: Some(1),
                    }
                )
            },
        ],
    );
    assert!(in_order, "piece, waiting, fired in order: {bodies:#?}");
    stop_and_wait(&h).await;
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_opener_warns_and_the_meeting_runs_unrecorded() {
    let (asr, llm) = start_mocks().await;
    asr.set_default_final(Respond::text("unrecorded words"));
    let (_, probs) = pattern(&[(11, 0.0), (10, 0.9), (25, 0.0)]);
    let opts = MeetingOpts {
        trace: Some(Arc::new(FailingOpener::new("disk full"))),
        ..MeetingOpts::default()
    };
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        ScriptedFactory::new(vec![(Speaker::Me, vec![OpenPlan::Ok(SourcePlan::new(46))])]),
        vec![VadScript::Probs(probs)],
        opts,
    )
    .await;
    start_running(&h).await;
    h.wait_final().await;
    let statuses = h.statuses();
    let warns: Vec<_> = statuses
        .iter()
        .filter(|(_, level, text)| {
            *level == StatusLevel::Warn && text.starts_with("trace: cannot record this meeting")
        })
        .collect();
    assert_eq!(warns.len(), 1, "exactly one opening Warn: {statuses:?}");
    assert!(
        warns[0].2.contains("disk full"),
        "the opener message reaches the user: {:?}",
        warns[0].2
    );
    assert!(
        !statuses
            .iter()
            .any(|(_, _, text)| text.contains("recording to")),
        "nothing is recorded, so nothing announces a directory: {statuses:?}"
    );
    // The meeting still works: a final committed above.
    stop_and_wait(&h).await;
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_location_with_audio_announces_the_directory_with_audio() {
    let (asr, llm) = start_mocks().await;
    let opener = location_opener(true);
    let mut h = running_me_with(&asr, &llm, trace_opts(&opener)).await;
    let statuses = h.statuses();
    assert!(
        announces(&statuses, "recording to /tmp/clueless-session (with audio)"),
        "the Info line names the directory and the audio: {statuses:?}"
    );
    stop_and_wait(&h).await;
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_location_without_audio_announces_the_directory_alone() {
    let (asr, llm) = start_mocks().await;
    let opener = location_opener(false);
    let mut h = running_me_with(&asr, &llm, trace_opts(&opener)).await;
    let statuses = h.statuses();
    assert!(
        announces(&statuses, "recording to /tmp/clueless-session"),
        "the Info line names only the directory: {statuses:?}"
    );
    assert!(
        !statuses
            .iter()
            .any(|(_, _, text)| text.contains("audio") && text.contains("recording")),
        "no audio words in the announcement: {statuses:?}"
    );
    stop_and_wait(&h).await;
    finish(&mut h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_harness_never_announces_a_recording() {
    let (asr, llm) = start_mocks().await;
    let mut h = MeetingHarness::start(
        &asr,
        &llm,
        endless_me_factory(1),
        silence_vad(),
        MeetingOpts::default(),
    )
    .await;
    start_running(&h).await;
    assert!(
        !h.statuses()
            .iter()
            .any(|(_, _, text)| text.contains("recording to")),
        "no trace installed, no announcement: {:?}",
        h.statuses()
    );
    finish(&mut h).await;
}
