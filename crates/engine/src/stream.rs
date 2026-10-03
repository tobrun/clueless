//! One std thread per opened source: read, resample, score, segment, dispatch.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clueless_types::audio::{SampleSource, SourceRead};
use clueless_types::events::{
    Segment, SegmentKind, Speaker, StatusLevel, StatusSink, StatusSource, UiEvent,
};
use segmenter::machine::{Machine, MachineParams};
use segmenter::resample::Resampler16k;
use segmenter::vad::SpeechProb;
use tokio::sync::mpsc;

use crate::asr_worker::LatestSlot;
use crate::clock::StreamClock;
use crate::pipeline::Drain;

/// What the stream thread writes into: segment channels and the shared state
/// its dispatch decisions touch.
pub struct StreamPipes {
    pub status_source: StatusSource,
    pub ui: StatusSink,
    /// Bounded finals queue (16) consumed in order by the stream's final worker.
    pub final_tx: mpsc::Sender<Segment>,
    /// Latest-wins slot for interims.
    pub interim_slot: Arc<LatestSlot>,
    /// One past the highest `Final` seq the segmenter has already emitted for
    /// this stream (interims at or beyond it are stale).
    pub final_seq_watermark: Arc<AtomicU64>,
    pub drain: Arc<Drain>,
    /// This speaker's busy record, read by the automatic-request logic.
    pub activity: Arc<crate::asr_worker::SpeakerActivity>,
    /// t0 of every queued or in-flight Them final (echo hold reads this).
    pub them_pending: Arc<Mutex<VecDeque<u64>>>,
    /// Start time of Them's currently open segment, or `u64::MAX` when none
    /// is open (echo hold reads this: an open segment is not text yet).
    pub them_open_t0: Arc<AtomicU64>,
    pub panic_tx: tokio::sync::mpsc::UnboundedSender<String>,
    pub shutdown: Arc<AtomicBool>,
    pub idle_poll: Duration,
}

/// A running stream thread with its shutdown flag.
pub struct StreamHandle {
    thread: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl StreamHandle {
    /// Ask the thread to flush its open segment and exit.
    pub fn request_stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    /// Wait for the thread; it returns on its own once `request_stop` was seen.
    pub fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start one stream thread inside `catch_unwind`: a panic sends an internal
/// panic message instead of killing the process, and the source is marked
/// ended so waiters do not hang.
pub fn spawn_stream(
    speaker: Speaker,
    source: Box<dyn SampleSource>,
    vad: Box<dyn SpeechProb>,
    params: MachineParams,
    mut clock: StreamClock,
    pipes: StreamPipes,
) -> StreamHandle {
    let shutdown = pipes.shutdown.clone();
    let panic_tx = pipes.panic_tx.clone();
    let end_watermark = clock.watermark_handle();
    let end_drain = pipes.drain.clone();
    let thread = std::thread::Builder::new()
        .name(format!("clueless-{speaker:?}-stream"))
        .spawn(move || {
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                stream_loop(speaker, source, vad, params, &mut clock, &pipes)
            }));
            if let Err(payload) = result {
                let message = panic_text(&payload);
                let _ = panic_tx.send(format!("{speaker:?} stream thread panicked: {message}"));
                end_watermark.store(u64::MAX, Ordering::Relaxed);
                end_drain.source_ended();
            }
        })
        .expect("spawning a stream thread");
    StreamHandle {
        thread: Some(thread),
        shutdown,
    }
}

fn panic_text(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

fn stream_loop(
    speaker: Speaker,
    mut source: Box<dyn SampleSource>,
    mut vad: Box<dyn SpeechProb>,
    params: MachineParams,
    clock: &mut StreamClock,
    pipes: &StreamPipes,
) {
    let mut machine = Machine::new(speaker, params);
    let mut resampler = Resampler16k::new(source.sample_rate());
    let mut buf = vec![0f32; 4096];
    let mut frame: u64 = 0;
    let mut empty_since: Option<Instant> = None;
    let mut warned_gap = false;

    loop {
        if pipes.shutdown.load(Ordering::Relaxed) {
            break;
        }
        match source.read(&mut buf) {
            SourceRead::Samples(n) => {
                empty_since = None;
                if !clock.anchored() {
                    clock.anchor_now();
                }
                resampler.push(&buf[..n]);
                while let Some(f) = resampler.pop_frame() {
                    let t_start = clock.current_t();
                    machine.push_audio(frame, &f);
                    let p = vad.prob(&f);
                    let segments = machine.push(p, frame, t_start);
                    clock.advance(512);
                    frame += 1;
                    for segment in segments {
                        send_segment(speaker, segment, pipes);
                    }
                }
                sync_them_open_t0(speaker, &machine, pipes);
            }
            SourceRead::Empty => {
                clock.follow_wall_clock();
                let now = Instant::now();
                match empty_since {
                    None => empty_since = Some(now),
                    Some(started) => {
                        if started.elapsed() > Duration::from_millis(200) {
                            empty_since = Some(now);
                            close_and_reanchor(
                                &mut machine,
                                &mut vad,
                                &mut resampler,
                                clock,
                                speaker,
                                pipes,
                            );
                        }
                    }
                }
                std::thread::sleep(pipes.idle_poll);
            }
            SourceRead::Gap => {
                if !warned_gap {
                    warned_gap = true;
                    (pipes.ui)(UiEvent::Status {
                        source: pipes.status_source,
                        level: StatusLevel::Warn,
                        text: "audio gap: some samples were lost".to_owned(),
                    });
                }
                close_and_reanchor(
                    &mut machine,
                    &mut vad,
                    &mut resampler,
                    clock,
                    speaker,
                    pipes,
                );
            }
            SourceRead::Reset { sample_rate } => {
                close_and_reanchor(
                    &mut machine,
                    &mut vad,
                    &mut resampler,
                    clock,
                    speaker,
                    pipes,
                );
                resampler = Resampler16k::new(sample_rate);
            }
            SourceRead::Ended => {
                if let Some(segment) = machine.close_segment() {
                    send_segment(speaker, segment, pipes);
                }
                sync_them_open_t0(speaker, &machine, pipes);
                clock.set_infinite();
                pipes.drain.source_ended();
                return;
            }
        }
    }

    // Meeting stop: flush the open segment as a Final if it passes the
    // minimum-speech rule, then let the source drop with the thread.
    if let Some(segment) = machine.flush() {
        send_segment(speaker, segment, pipes);
    }
    sync_them_open_t0(speaker, &machine, pipes);
    clock.set_infinite();
    pipes.drain.source_ended();
}

/// A `Gap`, a `Reset` or an `Empty` over 200 ms: close the open segment as a
/// `Final` if it qualifies, reset the detector and resampler state, and take
/// a fresh time anchor.
fn close_and_reanchor(
    machine: &mut Machine,
    vad: &mut Box<dyn SpeechProb>,
    resampler: &mut Resampler16k,
    clock: &mut StreamClock,
    speaker: Speaker,
    pipes: &StreamPipes,
) {
    if let Some(segment) = machine.close_segment() {
        send_segment(speaker, segment, pipes);
    }
    sync_them_open_t0(speaker, machine, pipes);
    vad.reset();
    resampler.reset();
    clock.anchor_now();
}

/// Mirror the segmenter's open-segment state for the Me echo hold: the t0 of
/// Them's open segment, or `u64::MAX` when none is open. Always called after
/// any `send_segment` of a close, so a hold that sees the open t0 cleared
/// already sees the closed final registered in `them_pending`.
fn sync_them_open_t0(speaker: Speaker, machine: &Machine, pipes: &StreamPipes) {
    pipes
        .activity
        .open
        .store(machine.open_t0_ms().is_some(), Ordering::Release);
    if speaker == Speaker::Them {
        let t0 = machine.open_t0_ms().unwrap_or(u64::MAX);
        pipes.them_open_t0.store(t0, Ordering::Release);
    }
}

/// Route one machine segment: interims replace the latest-wins slot, finals
/// are queued in order and become `TranscriptDropped` (with one status error)
/// when the bounded queue is full.
fn send_segment(speaker: Speaker, segment: Segment, pipes: &StreamPipes) {
    match segment.kind {
        SegmentKind::Interim => {
            pipes.interim_slot.push(segment);
        }
        SegmentKind::Final => {
            pipes
                .final_seq_watermark
                .fetch_max(segment.id.seq + 1, Ordering::Relaxed);
            pipes.drain.inc();
            pipes.activity.unresolved.fetch_add(1, Ordering::AcqRel);
            if speaker == Speaker::Them {
                pipes.them_pending.lock().unwrap().push_back(segment.t0_ms);
            }
            match pipes.final_tx.try_send(segment) {
                Ok(()) => {}
                Err(error) => {
                    let segment = error.into_inner();
                    pipes.drain.dec();
                    pipes.activity.unresolved.fetch_sub(1, Ordering::AcqRel);
                    if speaker == Speaker::Them {
                        remove_pending(&pipes.them_pending, segment.t0_ms);
                    }
                    (pipes.ui)(UiEvent::TranscriptDropped { id: segment.id });
                    (pipes.ui)(UiEvent::Status {
                        source: StatusSource::Asr,
                        level: StatusLevel::Error,
                        text: "final queue full: dropped an utterance".to_owned(),
                    });
                }
            }
        }
    }
}

pub(crate) fn remove_pending(pending: &Arc<Mutex<VecDeque<u64>>>, t0_ms: u64) {
    let mut queue = pending.lock().unwrap();
    if let Some(index) = queue.iter().position(|t| *t == t0_ms) {
        queue.remove(index);
    }
}
