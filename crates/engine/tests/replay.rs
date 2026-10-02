//! Replay-source and pipeline-shutdown tests from change set 10: WavSource
//! pacing and downmix, `drained()` resolution, and the stop-time flush.

mod support;

use std::time::{Duration, Instant};

const FAST: f64 = 1000.0;

use clueless_types::audio::{SampleSource, SourceRead};
use clueless_types::events::Speaker;
use engine::replay::WavSource;
use support::*;

/// Reads a source to the end on a plain thread, counting delivered samples.
fn drain_source(source: &mut WavSource) -> usize {
    let mut buf = vec![0f32; 4096];
    let mut total = 0;
    loop {
        match source.read(&mut buf) {
            SourceRead::Samples(n) => total += n,
            SourceRead::Ended => return total,
            _ => std::thread::sleep(Duration::from_millis(1)),
        }
    }
}

#[test]
fn wav_source_at_speed_10_ends_a_two_second_file_under_1_5s() {
    let path = temp_path("speed");
    write_wav(&path, 1, 16_000, &[500i16; 32_000]);
    let began = Instant::now();
    let mut source = WavSource::open(&path, 10.0).expect("open");
    assert_eq!(source.sample_rate(), 16_000);
    let delivered = drain_source(&mut source);
    let took = began.elapsed();
    assert_eq!(delivered, 32_000, "all samples delivered");
    assert!(took < Duration::from_millis(1_500), "ended in {took:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn wav_source_downmixes_a_stereo_44100_file_to_mono() {
    let path = temp_path("stereo");
    // 0.5 s of interleaved stereo at 44100 Hz: left 100, right 300.
    let mut samples = Vec::new();
    for _ in 0..22_050 {
        samples.extend([100i16, 300]);
    }
    write_wav(&path, 2, 44_100, &samples);
    let mut source = WavSource::open(&path, 1_000.0).expect("open");
    assert_eq!(
        source.sample_rate(),
        44_100,
        "the rate is reported unchanged"
    );
    let delivered = drain_source(&mut source);
    assert_eq!(delivered, 22_050, "interleaved stereo became mono frames");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drained_resolves_once_all_sources_end_and_finals_commit() {
    let mock = MockAsr::start().await;
    // Distinct texts so the echo filter does not drop Me's final.
    mock.enqueue_final(Respond::text("me text"));
    mock.enqueue_final(Respond::text("them text"));
    let (me_frames, me_probs) = utterance(11, 10, 20);
    let (them_frames, them_probs) = utterance(11, 15, 20);
    let h = start(
        vec![
            StreamSpec::new(Speaker::Me, me_frames, FAST, me_probs),
            StreamSpec::new(Speaker::Them, them_frames, FAST, them_probs),
        ],
        &mock,
        Opts::default(),
    );

    let resolved = tokio::time::timeout(Duration::from_secs(5), h.drained()).await;
    assert!(resolved.is_ok(), "drained() did not resolve");
    let again = tokio::time::timeout(Duration::from_secs(1), h.drained()).await;
    assert!(again.is_ok(), "still resolves after it fired once");
    assert_eq!(h.finals().len(), 2);
    assert_eq!(h.pipeline.pending_finals(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_during_speech_flushes_the_open_segment() {
    let mock = MockAsr::start().await;
    mock.set_default_final(Respond::text("flushed"));
    // Speech that never ends: 11 silent frames then an open-ended run, the
    // source never ends either.
    let source = ScriptedSource::new(10_000, 1.0).never_end();
    let (_, probs) = pattern(&[(11, 0.0), (2_000, 0.9)]);
    let mut h = start(
        vec![StreamSpec {
            speaker: Speaker::Me,
            source,
            probs,
        }],
        &mock,
        Opts::default(),
    );

    // Mid-speech at ~700 ms wall (frame ~21: ten speech frames so far).
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        h.finals().is_empty(),
        "nothing committed while speech is open"
    );
    let began = Instant::now();
    h.stop().await;
    let took = began.elapsed();
    assert!(took < Duration::from_secs(1), "stop returned in {took:?}");
    let finals = h.finals();
    assert_eq!(finals.len(), 1, "the open segment was flushed as a Final");
    assert_eq!(finals[0].id.speaker, Speaker::Me);
    assert_eq!(finals[0].text, "flushed");
}
