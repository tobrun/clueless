//! Integration tests for the transcribe path: stream threads, ASR workers,
//! queueing, forced cuts, the echo filter and shutdown, against the scripted
//! sources and the mock ASR server in `support`.

mod support;

use std::time::Duration;

use clueless_types::events::{Speaker, UiEvent};
use engine::deps::EngineTimings;
use segmenter::machine::MachineParams;
use tokio::time::timeout;

use support::*;

const FAST: f64 = 1000.0;

/// Speech with a one-frame dip to 0.2 every `period` frames, so forced cuts
/// find a below-threshold frame to cut at (a flat plateau would cascade).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finals_commit_in_sequence() {
    let mock = MockAsr::start().await;
    mock.enqueue_final(Respond::text("one"));
    mock.enqueue_final(Respond::text("two"));
    let (frames, probs) = pattern(&[(11, 0.0), (10, 0.9), (20, 0.0), (10, 0.9), (20, 0.0)]);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, FAST, probs)],
        &mock,
        Opts::default(),
    );

    let finals = h.wait_finals(2, Duration::from_secs(5)).await;
    assert_eq!(finals.len(), 2, "{:?}", h.snapshot());
    assert_eq!(finals[0].text, "one");
    assert_eq!(finals[1].text, "two");
    assert!(finals[0].id.seq < finals[1].id.seq);
    assert_eq!(finals[0].id.speaker, Speaker::Me);
    assert_eq!(
        h.store_texts(),
        vec![(Speaker::Me, "one".into()), (Speaker::Me, "two".into())]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_first_final_keeps_order_and_single_flight() {
    let mock = MockAsr::start().await;
    mock.enqueue_final(Respond::Delay {
        ms: 800,
        then: Box::new(Respond::text("one")),
    });
    mock.enqueue_final(Respond::text("two"));
    let (frames, probs) = pattern(&[(11, 0.0), (10, 0.9), (20, 0.0), (10, 0.9), (20, 0.0)]);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, FAST, probs)],
        &mock,
        Opts::default(),
    );

    let finals = h.wait_finals(2, Duration::from_secs(5)).await;
    assert_eq!(finals[0].text, "one");
    assert_eq!(finals[1].text, "two");
    assert_eq!(
        mock.max_inflight(Kind::Final),
        1,
        "the mock never sees two finals at once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interim_latest_wins_skips_stale() {
    let mock = MockAsr::start().await;
    // Interims run up to ~8 s of audio; the 267-frame final is ~8.5 s.
    mock.set_final_min_ms(8_200);
    mock.set_default_interim(Respond::Delay {
        ms: 150,
        then: Box::new(Respond::text("slow")),
    });
    mock.set_default_final(Respond::text("done"));
    // 4 interims would be emitted (segment elapsed 62, 124, 186, 248).
    let (frames, probs) = pattern(&[(11, 0.0), (250, 0.9), (20, 0.0)]);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, 20.0, probs)],
        &mock,
        Opts::default(),
    );

    let finals = h.wait_finals(1, Duration::from_secs(8)).await;
    assert_eq!(finals[0].text, "done");
    assert!(
        mock.max_inflight(Kind::Interim) <= 1,
        "at most one interim request in flight"
    );
    let interims = mock.kind_count(Kind::Interim);
    assert!(
        (1..4).contains(&interims),
        "some interims were skipped: {interims} sent"
    );
    let durations: Vec<u64> = mock
        .requests()
        .iter()
        .filter(|r| r.kind == Kind::Interim)
        .map(|r| r.duration_ms)
        .collect();
    for pair in durations.windows(2) {
        assert!(
            pair[1] > pair[0],
            "each sent interim is newer: {durations:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interim_response_after_final_is_ignored() {
    let mock = MockAsr::start().await;
    // Interim audio is 62 frames (1984 ms), the final spans 97 frames (3104 ms).
    mock.set_final_min_ms(2_600);
    mock.block();
    mock.enqueue_interim(Respond::text("first look"));
    mock.enqueue_interim(Respond::Blocked(Box::new(Respond::text("arrives late"))));
    mock.set_default_final(Respond::text("final text"));
    let (frames, probs) = pattern(&[(11, 0.0), (80, 0.9), (20, 0.0)]);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, 20.0, probs)],
        &mock,
        Opts::default(),
    );

    let finals = h.wait_finals(1, Duration::from_secs(6)).await;
    assert_eq!(finals[0].text, "final text");
    let interim_before = h.interim_ids().len();
    assert_eq!(interim_before, 1, "the first interim arrived normally");
    mock.release();
    // The held interim response comes back for a seq the Final already passed.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let events = h.snapshot();
    let final_at = events
        .iter()
        .rposition(|e| matches!(e, UiEvent::TranscriptFinal(_)))
        .expect("a final");
    assert!(
        !events[final_at..]
            .iter()
            .any(|e| matches!(e, UiEvent::TranscriptInterim { .. })),
        "no interim is emitted after the Final: {events:?}"
    );
    assert_eq!(h.interim_ids().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_500s_drop_with_one_error_status() {
    let mock = MockAsr::start().await;
    for _ in 0..3 {
        mock.enqueue_final(Respond::Status(500));
    }
    mock.set_default_final(Respond::text("second"));
    let (frames, probs) = pattern(&[(11, 0.0), (10, 0.9), (20, 0.0), (10, 0.9), (20, 0.0)]);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, FAST, probs)],
        &mock,
        Opts::default(),
    );

    let finals = h.wait_finals(1, Duration::from_secs(6)).await;
    assert_eq!(
        finals[0].text, "second",
        "the next utterance still transcribes"
    );
    assert_eq!(
        mock.kind_count(Kind::Final),
        4,
        "3 attempts + the next utterance"
    );
    assert_eq!(h.dropped().len(), 1);
    assert_eq!(h.asr_error_statuses().len(), 1);
    assert_eq!(h.finals().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_text_drops_without_status() {
    let mock = MockAsr::start().await;
    mock.set_default_final(Respond::text("   "));
    let (frames, probs) = utterance(11, 10, 20);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, FAST, probs)],
        &mock,
        Opts::default(),
    );

    h.wait_until(Duration::from_secs(5), |e| {
        e.iter()
            .any(|e| matches!(e, UiEvent::TranscriptDropped { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(h.dropped().len(), 1);
    assert_eq!(h.finals().len(), 0);
    assert_eq!(h.asr_error_statuses().len(), 0, "no-speech is not an error");
    assert!(h.store_texts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_full_drops_exactly_one() {
    let mock = MockAsr::start().await;
    mock.block();
    mock.enqueue_final(Respond::Blocked(Box::new(Respond::text("first"))));
    mock.set_default_final(Respond::text("q"));
    // 18 utterances: 1 in flight + 16 queued + 1 dropped.
    let mut runs = vec![(11usize, 0.0f32)];
    for _ in 0..18 {
        runs.push((10, 0.9));
        runs.push((20, 0.0));
    }
    let (frames, probs) = pattern(&runs);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, 10.0, probs)],
        &mock,
        Opts::default(),
    );

    let events = h
        .wait_until(Duration::from_secs(8), |e| {
            e.iter()
                .filter(|e| matches!(e, UiEvent::TranscriptDropped { .. }))
                .count()
                >= 1
        })
        .await;
    let dropped: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, UiEvent::TranscriptDropped { .. }))
        .collect();
    assert_eq!(dropped.len(), 1, "exactly one drop at queue full");
    let statuses = events
        .iter()
        .filter(|e| matches!(e, UiEvent::Status { .. }))
        .count();
    assert_eq!(statuses, 1, "one status error with the drop");
    assert_eq!(
        mock.request_count(),
        1,
        "only the blocked request reached the mock"
    );
    assert_eq!(mock.max_inflight(Kind::Final), 1);

    mock.release();
    let finals = h.wait_finals(17, Duration::from_secs(8)).await;
    assert_eq!(finals.len(), 17, "18 emitted - 1 dropped");
    assert_eq!(finals[0].text, "first");
    assert!(finals[1..].iter().all(|f| f.text == "q"));
    assert_eq!(
        h.dropped()[0].seq,
        finals[16].id.seq + 1,
        "the last final was the dropped one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_request_drops_after_one_attempt() {
    let mock = MockAsr::start().await;
    mock.enqueue_final(Respond::Status(400));
    mock.set_default_final(Respond::text("never"));
    let (frames, probs) = utterance(11, 10, 20);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, FAST, probs)],
        &mock,
        Opts::default(),
    );

    h.wait_until(Duration::from_secs(5), |e| {
        e.iter()
            .any(|e| matches!(e, UiEvent::TranscriptDropped { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(mock.request_count(), 1, "a 400 is not retried");
    assert_eq!(h.dropped().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forced_cut_after_dropped_piece_keeps_text() {
    let mock = MockAsr::start().await;
    // Piece 1 (3008 ms of audio) burns all three attempts, piece 2 (2112 ms)
    // then answers; the 1984 ms interims stay on their own queue.
    mock.set_final_min_ms(2_048);
    for _ in 0..3 {
        mock.enqueue_final(Respond::Status(500));
    }
    mock.enqueue_final(Respond::text("beta gamma"));
    let (frames, probs) = pattern(&[(3, 0.0), (120, 0.9), (25, 0.0)]);
    let opts = Opts {
        machine: MachineParams {
            max_segment_ms: 3_000,
            ..MachineParams::default()
        },
        ..Opts::default()
    };
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, FAST, probs)],
        &mock,
        opts,
    );

    let _ = timeout(Duration::from_secs(6), h.pipeline.drained()).await;
    let finals = h.finals();
    assert_eq!(h.dropped().len(), 1, "piece 1 dropped after 3 failures");
    assert_eq!(finals.len(), 1, "{:?}", finals);
    assert_eq!(
        finals[0].text, "beta gamma",
        "the piece after a dropped overlap is committed unchanged"
    );
    assert!(h.dropped()[0].seq < finals[0].id.seq);
    assert_eq!(h.asr_error_statuses().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_progress_lists_interim_until_final_commits() {
    let mock = MockAsr::start().await;
    // Interim audio is 1984 ms, the final spans 3104 ms.
    mock.set_final_min_ms(2_600);
    mock.block();
    mock.enqueue_interim(Respond::text("interim text"));
    mock.enqueue_final(Respond::Blocked(Box::new(Respond::text("full text"))));
    let (frames, probs) = pattern(&[(11, 0.0), (80, 0.9), (20, 0.0)]);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, 20.0, probs)],
        &mock,
        Opts::default(),
    );

    assert!(
        mock.wait_requests(2, Duration::from_secs(6)).await,
        "the interim answered and the final request is blocked: {:?}",
        mock.requests()
    );
    // The final request is in flight; nothing is committed yet.
    let in_progress = h.pipeline.in_progress();
    assert_eq!(in_progress.len(), 1, "{:?}", in_progress);
    assert_eq!(in_progress[0].speaker, Speaker::Me);
    assert_eq!(in_progress[0].text, "interim text");

    mock.release();
    let finals = h.wait_finals(1, Duration::from_secs(6)).await;
    assert_eq!(finals[0].text, "full text");
    assert!(
        h.pipeline.in_progress().is_empty(),
        "cleared after the commit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forced_cut_overlap_strips_repeated_words() {
    let mock = MockAsr::start().await;
    // Flat 0.9 speech means the cut window has no pause, so the cut lands on
    // a speech frame and carries 1 s of audio into the next piece. Interim
    // audio spans 62 frames (1984 ms), the finals 94 (3008 ms) and 66
    // (2112 ms), so 2048 ms separates the kinds.
    mock.set_final_min_ms(2_048);
    mock.enqueue_final(Respond::text("we ship on friday"));
    mock.enqueue_final(Respond::text("on friday if tests pass"));
    let (frames, probs) = pattern(&[(3, 0.0), (120, 0.9), (25, 0.0)]);
    let opts = Opts {
        machine: MachineParams {
            max_segment_ms: 3_000,
            ..MachineParams::default()
        },
        ..Opts::default()
    };
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, FAST, probs)],
        &mock,
        opts,
    );

    let _ = timeout(Duration::from_secs(6), h.pipeline.drained()).await;
    let finals = h.finals();
    assert_eq!(finals.len(), 2, "{:?}", finals);
    assert_eq!(finals[0].text, "we ship on friday");
    assert_eq!(
        finals[1].text, "if tests pass",
        "the overlap with the previous commit is stripped"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gap_closes_segment_and_reanchors() {
    let mock = MockAsr::start().await;
    mock.enqueue_final(Respond::text("first"));
    mock.enqueue_final(Respond::text("second"));
    // The vad script rewinds on the gap's detector reset, so the source
    // feeds 21 frames, the gap, then the same 41-frame script again.
    let (_, probs) = pattern(&[(11, 0.0), (10, 0.9), (20, 0.0)]);
    let source = ScriptedSource::new(62, FAST).with_event(21, FrameEvent::Gap);
    let h = start(
        vec![StreamSpec {
            speaker: Speaker::Me,
            source,
            probs,
        }],
        &mock,
        Opts::default(),
    );

    let finals = h.wait_finals(2, Duration::from_secs(6)).await;
    assert_eq!(
        finals[0].text, "first",
        "the gap closed the speech as a Final"
    );
    assert_eq!(finals[1].text, "second");
    assert!(
        finals[1].t0_ms < finals[0].t1_ms,
        "the next segment is timed from the fresh anchor: {:?}",
        finals
    );
    assert_eq!(h.finals().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_to_24k_keeps_16k_frames() {
    let mock = MockAsr::start().await;
    mock.enqueue_final(Respond::text("pre"));
    mock.enqueue_final(Respond::text("post"));
    // Same rewind logic as the gap test: 21 frames, the reset, then the
    // 46-frame script again at 24 kHz.
    let (_, probs) = pattern(&[(11, 0.0), (10, 0.9), (25, 0.0)]);
    let source = ScriptedSource::new(93, FAST).with_event(21, FrameEvent::Reset(24_000));
    let h = start(
        vec![StreamSpec {
            speaker: Speaker::Me,
            source,
            probs,
        }],
        &mock,
        Opts::default(),
    );

    let finals = h.wait_finals(2, Duration::from_secs(6)).await;
    assert_eq!(finals[0].text, "pre", "the reset closed the open segment");
    assert_eq!(finals[1].text, "post", "audio keeps transcribing at 24 kHz");
    // 16 kHz framing survived: the second utterance spans ~26 machine frames.
    let post = mock
        .requests()
        .last()
        .expect("a request for the post-reset utterance")
        .duration_ms;
    assert!(
        (500..1_300).contains(&post),
        "post-reset segment duration {post} ms"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn me_echo_of_them_final_is_dropped() {
    let mock = MockAsr::start().await;
    mock.set_default_final(Respond::text("hello there"));
    // Both paced at 1x so their audio times line up; Them ends first.
    let (them_frames, them_probs) = pattern(&[(11, 0.0), (15, 0.9), (20, 0.0)]);
    let (me_frames, me_probs) = pattern(&[(11, 0.0), (25, 0.9), (20, 0.0)]);
    let h = start(
        vec![
            StreamSpec::new(Speaker::Me, me_frames, 2.0, me_probs),
            StreamSpec::new(Speaker::Them, them_frames, 2.0, them_probs),
        ],
        &mock,
        Opts::default(),
    );

    let finals = h.wait_finals(1, Duration::from_secs(8)).await;
    assert_eq!(finals[0].id.speaker, Speaker::Them);
    // Me's final comes ~0.4 s after Them's; give it room, then verify.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let finals = h.finals();
    assert_eq!(finals.len(), 1, "Me's copy must not commit: {finals:?}");
    let dropped = h.dropped();
    assert_eq!(dropped.len(), 1);
    assert_eq!(
        dropped[0].speaker,
        Speaker::Me,
        "Me dropped as an echo of Them"
    );
    assert_eq!(h.store_texts(), vec![(Speaker::Them, "hello there".into())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn echo_hold_times_out_and_commits() {
    let mock = MockAsr::start().await;
    // Them's 60-frame final (~2.4 s audio) is a "final"; their 62-frame
    // interim (1984 ms) and Me's 10-frame utterance (~0.9 s) are interims.
    mock.set_final_min_ms(2_048);
    mock.block();
    mock.enqueue_final(Respond::Blocked(Box::new(Respond::text("them text"))));
    mock.enqueue_interim(Respond::Delay {
        ms: 250,
        then: Box::new(Respond::text("me text")),
    });
    let (them_frames, them_probs) = pattern(&[(11, 0.0), (60, 0.9), (20, 0.0)]);
    let (me_frames, me_probs) = pattern(&[(11, 0.0), (10, 0.9), (20, 0.0)]);
    let h = start(
        vec![
            StreamSpec::new(Speaker::Me, me_frames, 10.0, me_probs),
            StreamSpec::new(Speaker::Them, them_frames, 10.0, them_probs),
        ],
        &mock,
        Opts::default(),
    );

    let finals = h.wait_finals(1, Duration::from_secs(8)).await;
    assert_eq!(finals[0].id.speaker, Speaker::Me);
    assert_eq!(
        finals[0].text, "me text",
        "committed after the hold with what is known"
    );
    // Me's request is answered after 250 ms, then the hold waits ~300 ms.
    let me_request = &mock.requests()[0];
    assert_eq!(me_request.kind, Kind::Interim);
    let final_at = h
        .event_time(|e| matches!(e, UiEvent::TranscriptFinal(u) if u.id.speaker == Speaker::Me))
        .expect("Me committed");
    let waited = final_at.saturating_sub(me_request.arrival);
    assert!(
        waited >= Duration::from_millis(300) && waited <= Duration::from_millis(900),
        "Me committed ~300 ms after its transcription, waited {waited:?}"
    );
    assert_eq!(
        h.dropped().len(),
        0,
        "the held Them final is still in flight"
    );
    mock.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn them_empty_never_holds_me() {
    let mock = MockAsr::start().await;
    let opts = Opts {
        timings: EngineTimings {
            echo_hold: Duration::from_secs(5),
            ..fast_timings()
        },
        ..Opts::default()
    };
    let them = ScriptedSource::new(0, FAST).never_end();
    let (me_frames, me_probs) = utterance(11, 10, 20);
    let h = start(
        vec![
            StreamSpec::new(Speaker::Me, me_frames, 1.0, me_probs),
            StreamSpec {
                speaker: Speaker::Them,
                source: them,
                probs: Vec::new(),
            },
        ],
        &mock,
        opts,
    );

    // Me's audio ends at ~1.5 s wall; a 5 s hold would land far after 2.5 s.
    let finals = h.wait_finals(1, Duration::from_millis(2_500)).await;
    assert_eq!(
        finals.len(),
        1,
        "Them's watermark follows wall clock: {finals:?}"
    );
    assert_eq!(finals[0].id.speaker, Speaker::Me);
    h.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn me_echo_of_them_interim_is_dropped() {
    let mock = MockAsr::start().await;
    // Every answer is the same text; Them is still speaking (never ends).
    mock.set_default_final(Respond::text("shared words"));
    mock.set_default_interim(Respond::text("shared words"));
    let them = ScriptedSource::new(411, 5.0).never_end();
    let (_, them_probs) = pattern(&[(11, 0.0), (400, 0.9)]);
    let (me_frames, me_probs) = pattern(&[(55, 0.0), (65, 0.9), (20, 0.0)]);
    let mut h = start(
        vec![
            StreamSpec::new(Speaker::Me, me_frames, 5.0, me_probs),
            StreamSpec {
                speaker: Speaker::Them,
                source: them,
                probs: them_probs,
            },
        ],
        &mock,
        Opts::default(),
    );

    h.wait_until(Duration::from_secs(6), |e| {
        e.iter()
            .any(|e| matches!(e, UiEvent::TranscriptDropped { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let dropped = h.dropped();
    assert_eq!(dropped.len(), 1, "Me's final overlaps Them's interim text");
    assert_eq!(dropped[0].speaker, Speaker::Me);
    assert!(
        h.finals().iter().all(|f| f.id.speaker != Speaker::Me),
        "Me never committed"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_releases_hold_and_flushes() {
    let mock = MockAsr::start().await;
    mock.set_final_min_ms(2_048);
    mock.block();
    mock.enqueue_final(Respond::Blocked(Box::new(Respond::text("them text"))));
    mock.enqueue_interim(Respond::Delay {
        ms: 250,
        then: Box::new(Respond::text("me text")),
    });
    let opts = Opts {
        timings: EngineTimings {
            echo_hold: Duration::from_secs(5),
            stop_wait: Duration::from_millis(300),
            ..fast_timings()
        },
        ..Opts::default()
    };
    let (them_frames, them_probs) = pattern(&[(11, 0.0), (60, 0.9), (20, 0.0)]);
    let (me_frames, me_probs) = pattern(&[(11, 0.0), (10, 0.9), (20, 0.0)]);
    let mut h = start(
        vec![
            StreamSpec::new(Speaker::Me, me_frames, 10.0, me_probs),
            StreamSpec::new(Speaker::Them, them_frames, 10.0, them_probs),
        ],
        &mock,
        opts,
    );

    // Wait until Them's final request is blocked in flight, then stop.
    let mut them_requested = false;
    for _ in 0..600 {
        if mock.requests().iter().any(|r| r.kind == Kind::Final) {
            them_requested = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(them_requested, "Them's final reached the mock and blocked");

    let began = std::time::Instant::now();
    h.stop().await;
    let took = began.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "stop returned in {took:?}, not waiting for the hold"
    );
    let finals = h.finals();
    assert_eq!(
        finals.len(),
        1,
        "Me decided when the stop released the hold"
    );
    assert_eq!(finals[0].id.speaker, Speaker::Me);
    assert_eq!(finals[0].text, "me text");
    let dropped = h.dropped();
    assert_eq!(
        dropped.len(),
        1,
        "the blocked Them final was cancelled and dropped"
    );
    assert_eq!(dropped[0].speaker, Speaker::Them);
    assert_eq!(
        h.pipeline.pending_finals(),
        0,
        "nothing left pending after stop"
    );
    mock.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_them_source_means_no_hold() {
    let mock = MockAsr::start().await;
    let opts = Opts {
        timings: EngineTimings {
            echo_hold: Duration::from_secs(5),
            ..fast_timings()
        },
        ..Opts::default()
    };
    let (frames, probs) = utterance(11, 10, 20);
    let h = start(
        vec![StreamSpec::new(Speaker::Me, frames, FAST, probs)],
        &mock,
        opts,
    );

    let finals = h.wait_finals(1, Duration::from_secs(1)).await;
    assert_eq!(finals.len(), 1, "Me commits without ever holding");
    assert_eq!(finals[0].text, "mock");
}
