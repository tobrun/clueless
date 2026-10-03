//! The two ASR worker tasks per stream: one final worker reading the bounded
//! channel in order, one interim worker on a latest-wins slot.
//!
//! The final worker transcribes with retries, applies the forced-cut overlap
//! strip, holds `Me` finals for the echo filter, commits survivors to the
//! shared store and emits `TranscriptFinal` / `TranscriptDropped` / status
//! events. The interim worker sends at most one request at a time and skips
//! anything an older result was still replacing.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use asr::client::AsrClient;
use clueless_types::events::{
    Segment, Speaker, StatusLevel, StatusSink, StatusSource, UiEvent, Utterance,
};
use context::echo::is_echo;
use context::prompt::InProgressText;
use segmenter::dedup::strip_overlap;

use crate::deps::EngineTimings;
use crate::pipeline::Drain;

/// A finished speech piece, reported to the engine loop: committed (`text` is
/// `Some`) or dropped (`None`).
#[derive(Debug, Clone)]
pub struct PieceDone {
    pub speaker: Speaker,
    pub text: Option<String>,
}

/// Whether a speaker has speech the automatic-request logic should wait for:
/// an open segment in the segmenter, or closed segments whose final is still
/// queued or in flight.
#[derive(Debug, Default)]
pub struct SpeakerActivity {
    /// The segmenter holds an open (not yet closed) segment.
    pub open: AtomicBool,
    /// Finals queued or in flight, not yet committed or dropped.
    pub unresolved: AtomicUsize,
}

impl SpeakerActivity {
    pub fn busy(&self) -> bool {
        self.open.load(Ordering::Acquire) || self.unresolved.load(Ordering::Acquire) > 0
    }
}

/// A single-slot "latest wins" mailbox: an unread segment is replaced, so a
/// slow interim worker always works on the newest audio and never queues
/// stale interims.
pub struct LatestSlot {
    inner: Mutex<Option<Segment>>,
    notify: tokio::sync::Notify,
}

impl Default for LatestSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl LatestSlot {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// Store the segment, dropping any unread one.
    pub fn push(&self, segment: Segment) {
        *self.inner.lock().unwrap() = Some(segment);
        // `Notify` parks permits for absent waiters; extra offers just mean
        // a spurious wakeup that finds the slot empty.
        self.notify.notify_one();
    }

    pub fn take(&self) -> Option<Segment> {
        self.inner.lock().unwrap().take()
    }

    /// Resolve once something has been offered; loop `take` until it yields.
    pub async fn wait(&self) -> Option<Segment> {
        loop {
            if let Some(segment) = self.take() {
                return Some(segment);
            }
            self.notify.notified().await;
        }
    }
}

/// Milliseconds since process start: the shared base of the
/// `asr_sent_ms` / `asr_done_ms` log fields.
fn uptime_ms() -> u128 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis()
}

/// The latest interim text of a not-yet-resolved utterance, per speaker
/// (spec: "text in progress", read by `Pipeline::in_progress`).
#[derive(Debug, Clone)]
pub struct ProgressEntry {
    pub speaker: Speaker,
    pub seq: u64,
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub text: String,
}

/// Echo state a `Me` worker needs; only present when a Them source is open.
pub struct EchoCtx {
    /// The Them stream's audio-time watermark in meeting ms.
    pub them_watermark: Arc<AtomicU64>,
    /// `t0_ms` of every queued or in-flight Them final.
    pub them_pending: Arc<Mutex<VecDeque<u64>>>,
    /// Start of Them's currently open segment, or `u64::MAX` when none is
    /// open: an open segment is Them text not yet on its way to becoming a
    /// final, so the hold keeps waiting while it lies below the Me end time.
    pub them_open_t0: Arc<AtomicU64>,
    pub hold: Duration,
}

/// Everything one worker task touches, shared by both workers of a stream.
pub struct WorkerCtx {
    pub speaker: Speaker,
    pub ui: StatusSink,
    pub asr: Arc<AsrClient>,
    pub store: Arc<Mutex<context::store::TranscriptStore>>,
    /// Bumped after every commit (`Pipeline::commits`).
    pub commits_tx: watch::Sender<u64>,
    /// Text in progress per speaker, shared across both streams.
    pub progress: Arc<Mutex<HashMap<Speaker, ProgressEntry>>>,
    /// Them final `t0_ms`s, shared across both streams; the Them worker
    /// removes its own entries, the Me hold reads them.
    pub them_pending: Arc<Mutex<VecDeque<u64>>>,
    pub drain: Arc<Drain>,
    /// The meeting's cancel token: aborts in-flight requests.
    pub cancel: CancellationToken,
    /// This speaker's busy record, updated before a piece is reported.
    pub activity: Arc<SpeakerActivity>,
    /// Where every finished piece is reported.
    pub pieces: mpsc::UnboundedSender<PieceDone>,
    /// Some only for a Me stream while a Them source is open.
    pub echo: Option<EchoCtx>,
    pub timings: EngineTimings,
}

impl WorkerCtx {
    fn drop_final(&self, segment: &Segment) {
        (self.ui)(UiEvent::TranscriptDropped { id: segment.id });
        self.release(segment, None);
    }

    /// Common teardown for one final: leave the pending set, clear text in
    /// progress for this utterance, update the busy record, report the
    /// finished piece, and only then decrement the drain counter - so the
    /// engine has every piece message queued before it hears "drained".
    fn release(&self, segment: &Segment, text: Option<String>) {
        if self.speaker == Speaker::Them {
            let mut pending = self.them_pending.lock().unwrap();
            if let Some(index) = pending.iter().position(|t| *t == segment.t0_ms) {
                pending.remove(index);
            }
        }
        let mut progress = self.progress.lock().unwrap();
        if let Some(entry) = progress.get(&self.speaker)
            && entry.seq <= segment.id.seq
        {
            progress.remove(&self.speaker);
        }
        drop(progress);
        self.activity.unresolved.fetch_sub(1, Ordering::AcqRel);
        let _ = self.pieces.send(PieceDone {
            speaker: self.speaker,
            text,
        });
        self.drain.dec();
    }
}

/// The final worker: reads finals in order until the channel closes.
pub async fn run_final(ctx: Arc<WorkerCtx>, mut finals: mpsc::Receiver<Segment>) {
    let mut previous_committed: Option<String> = None;
    loop {
        let segment = tokio::select! {
            maybe = finals.recv() => match maybe {
                Some(segment) => segment,
                None => break,
            },
            _ = ctx.cancel.cancelled() => {
                finals.close();
                while let Ok(segment) = finals.try_recv() {
                    ctx.drop_final(&segment);
                }
                break;
            }
        };
        if ctx.cancel.is_cancelled() {
            ctx.drop_final(&segment);
            finals.close();
            while let Ok(rest) = finals.try_recv() {
                ctx.drop_final(&rest);
            }
            break;
        }
        previous_committed = process_final(&ctx, segment, previous_committed).await;
    }
    tracing::debug!(speaker = ?ctx.speaker, "final worker exited");
}

/// One final segment end to end; returns the committed text (the strip
/// reference for the next forced-cut piece) or `None` when dropped.
async fn process_final(
    ctx: &Arc<WorkerCtx>,
    segment: Segment,
    previous_committed: Option<String>,
) -> Option<String> {
    // 1. Transcribe with the client's built-in retries, abortable.
    // Latency fields for one utterance, logged at the end of transcribe.
    tracing::info!(
        speaker = ?segment.id.speaker,
        seq = segment.id.seq,
        vad_end_ms = segment.t1_ms,
        asr_sent_ms = uptime_ms(),
        "final transcription request"
    );
    let result = tokio::select! {
        result = ctx.asr.transcribe(&segment.pcm, 3) => result,
        _ = ctx.cancel.cancelled() => {
            ctx.drop_final(&segment);
            return None;
        }
    };
    tracing::info!(
        speaker = ?segment.id.speaker,
        seq = segment.id.seq,
        vad_end_ms = segment.t1_ms,
        asr_done_ms = uptime_ms(),
        "final transcription response"
    );
    let text = match result {
        Ok(Some(text)) => text,
        Ok(None) => {
            tracing::debug!(seq = segment.id.seq, "no speech, dropped");
            ctx.drop_final(&segment);
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, seq = segment.id.seq, "final transcription failed");
            (ctx.ui)(UiEvent::Status {
                source: StatusSource::Asr,
                level: StatusLevel::Error,
                text: format!("ASR failed, dropped an utterance: {error}"),
            });
            ctx.drop_final(&segment);
            return None;
        }
    };

    // 2. Forced-cut overlap: strip repeated words only when the previous
    // piece was committed.
    let text = if segment.overlaps_prev {
        match &previous_committed {
            Some(previous) => strip_overlap(previous, &text),
            None => text,
        }
    } else {
        text
    };
    if !text.chars().any(char::is_alphanumeric) {
        tracing::debug!(seq = segment.id.seq, "empty after overlap strip, dropped");
        ctx.drop_final(&segment);
        return None;
    }

    // 3. Echo filter for Me finals (only when a Them source is open).
    if ctx.speaker == Speaker::Me
        && let Some(echo) = &ctx.echo
    {
        hold_for_them(ctx, echo, segment.t1_ms).await;
        let comparison = comparison_set(ctx);
        let me = Utterance {
            id: segment.id,
            t0_ms: segment.t0_ms,
            t1_ms: segment.t1_ms,
            text: text.clone(),
        };
        if is_echo(&me, &comparison) {
            tracing::info!(seq = segment.id.seq, "Me final dropped as echo");
            ctx.drop_final(&segment);
            return None;
        }
    }

    // 4. Commit.
    if ctx.cancel.is_cancelled() {
        ctx.drop_final(&segment);
        return None;
    }
    let utterance = Utterance {
        id: segment.id,
        t0_ms: segment.t0_ms,
        t1_ms: segment.t1_ms,
        text: text.clone(),
    };
    ctx.store.lock().unwrap().push(utterance.clone());
    ctx.commits_tx.send_modify(|count| *count += 1);
    (ctx.ui)(UiEvent::TranscriptFinal(utterance));
    ctx.release(&segment, Some(text.clone()));
    Some(text)
}

/// Hold a Me final until the Them watermark passed `t1_ms`, no Them segment
/// is open across it, and nothing of theirs before it is still uncommitted -
/// or the hold timed out (decide with what is known). A meeting stop does
/// NOT cut this short: the pipeline's stop waits inside `stop_wait` for the
/// decision, so a tail echo can never commit while the Them final covering
/// it is still in flight. Only the meeting's cancel token releases early.
async fn hold_for_them(ctx: &WorkerCtx, echo: &EchoCtx, t1_ms: u64) {
    let deadline = tokio::time::Instant::now() + echo.hold;
    let mut ticker = tokio::time::interval(Duration::from_millis(10));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if ctx.cancel.is_cancelled() {
            return;
        }
        if echo_released(echo, t1_ms) {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::debug!("echo hold timed out, deciding with what is known");
            return;
        }
        tokio::select! {
            _ = ticker.tick() => {}
            _ = ctx.cancel.cancelled() => return,
        }
    }
}

/// The hold's release condition, checked without any waiting.
fn echo_released(echo: &EchoCtx, t1_ms: u64) -> bool {
    if echo.them_watermark.load(Ordering::Acquire) < t1_ms {
        return false;
    }
    // A Them segment still open across this Me end has spoken into the span
    // but produced no text yet: it closes as a final shortly and could hold
    // the echo, so the hold keeps waiting.
    if echo.them_open_t0.load(Ordering::Acquire) < t1_ms {
        return false;
    }
    // Watermark passed and no open segment: everything Them will say that
    // could be an echo of this is registered unless still queued or in
    // flight.
    !echo
        .them_pending
        .lock()
        .unwrap()
        .iter()
        .any(|t0| *t0 < t1_ms)
}

/// Committed Them utterances plus the Them text in progress, if any.
fn comparison_set(ctx: &WorkerCtx) -> Vec<Utterance> {
    let mut set: Vec<Utterance> = ctx
        .store
        .lock()
        .unwrap()
        .lines()
        .iter()
        .filter(|line| line.utterance.id.speaker == Speaker::Them)
        .map(|line| line.utterance.clone())
        .collect();
    let in_progress = in_progress_entry(ctx, Speaker::Them);
    if let Some(entry) = in_progress {
        set.push(Utterance {
            id: clueless_types::UtteranceId {
                speaker: Speaker::Them,
                seq: entry.seq,
            },
            t0_ms: entry.t0_ms,
            t1_ms: entry.t1_ms,
            text: entry.text,
        });
    }
    set
}

fn in_progress_entry(ctx: &WorkerCtx, speaker: Speaker) -> Option<ProgressEntry> {
    ctx.progress.lock().unwrap().get(&speaker).cloned()
}

/// The interim worker: one request at a time, latest segment wins, results
/// for a seq whose `Final` the segmenter already emitted are ignored.
pub async fn run_interim(
    ctx: Arc<WorkerCtx>,
    slot: Arc<LatestSlot>,
    final_seq_watermark: Arc<AtomicU64>,
) {
    'outer: loop {
        let pending = tokio::select! {
            maybe = slot.wait() => maybe,
            _ = ctx.cancel.cancelled() => break 'outer,
        };
        let Some(segment) = pending else {
            continue 'outer;
        };
        let seq = segment.id.seq;
        if seq < final_seq_watermark.load(Ordering::Acquire) {
            continue;
        }
        tracing::info!(
            speaker = ?ctx.speaker,
            seq,
            vad_end_ms = segment.t1_ms,
            asr_sent_ms = uptime_ms(),
            "interim transcription request"
        );
        let result = tokio::select! {
            result = ctx.asr.transcribe(&segment.pcm, 1) => result,
            _ = ctx.cancel.cancelled() => break,
        };
        tracing::info!(
            speaker = ?ctx.speaker,
            seq,
            vad_end_ms = segment.t1_ms,
            asr_done_ms = uptime_ms(),
            "interim transcription response"
        );
        // The Final arrived while this request was in flight.
        if seq < final_seq_watermark.load(Ordering::Acquire) {
            continue;
        }
        let Ok(Some(text)) = result else { continue };
        (ctx.ui)(UiEvent::TranscriptInterim {
            id: segment.id,
            text: text.clone(),
        });
        let mut progress = ctx.progress.lock().unwrap();
        match progress.get_mut(&ctx.speaker) {
            Some(entry) if entry.seq > seq => {}
            _ => {
                progress.insert(
                    ctx.speaker,
                    ProgressEntry {
                        speaker: ctx.speaker,
                        seq,
                        t0_ms: segment.t0_ms,
                        t1_ms: segment.t1_ms,
                        text,
                    },
                );
            }
        }
    }
    tracing::debug!(speaker = ?ctx.speaker, "interim worker exited");
}

impl From<&ProgressEntry> for InProgressText {
    fn from(entry: &ProgressEntry) -> Self {
        Self {
            speaker: entry.speaker,
            t0_ms: entry.t0_ms,
            t1_ms: entry.t1_ms,
            text: entry.text.clone(),
        }
    }
}
