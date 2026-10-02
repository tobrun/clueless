//! The running pipeline: one stream thread and two worker tasks per opened
//! source, plus the shared state the meeting lifecycle drives.
//!
//! `Pipeline::start` wires everything; `in_progress`, `commits`, `drained`
//! and `panics` are read-only surfaces; `stop` runs the spec's stop steps
//! (flush, release holds, wait, cancel, drop the rest, join).

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use asr::client::AsrClient;
use clueless_types::audio::SampleSource;
use clueless_types::events::{Segment, Speaker, StatusLevel, StatusSource, UiEvent};
use context::prompt::InProgressText;
use context::store::TranscriptStore;
use futures_util::FutureExt;
use segmenter::machine::MachineParams;

use crate::asr_worker::{self, LatestSlot, ProgressEntry, WorkerCtx};
use crate::clock::{MeetingClock, StreamClock};
use crate::deps::EngineDeps;
use crate::stream::{StreamHandle, StreamPipes, spawn_stream};

/// Completion accounting behind `Pipeline::drained` and `stop`'s wait: how
/// many sources ended and how many finals are still queued or in flight.
/// The stream thread registers each final before queueing it and the worker
/// releases it when it commits or drops; `source_ended` comes from the
/// stream thread once its source Ended or after its stop flush.
pub struct Drain {
    state: Mutex<DrainState>,
    changed: watch::Sender<()>,
}

#[derive(Debug, Default)]
struct DrainState {
    expected_sources: usize,
    ended_sources: usize,
    pending_finals: usize,
}

impl Drain {
    pub fn new(expected_sources: usize) -> Self {
        let (changed, _) = watch::channel(());
        Self {
            state: Mutex::new(DrainState {
                expected_sources,
                ended_sources: 0,
                pending_finals: 0,
            }),
            changed,
        }
    }

    pub fn inc(&self) {
        self.state.lock().unwrap().pending_finals += 1;
        self.changed.send_replace(());
    }

    pub fn dec(&self) {
        let mut state = self.state.lock().unwrap();
        state.pending_finals = state.pending_finals.saturating_sub(1);
        drop(state);
        self.changed.send_replace(());
    }

    pub fn source_ended(&self) {
        self.state.lock().unwrap().ended_sources += 1;
        self.changed.send_replace(());
    }

    fn settled(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.ended_sources >= state.expected_sources && state.pending_finals == 0
    }

    pub fn pending_finals(&self) -> usize {
        self.state.lock().unwrap().pending_finals
    }

    /// Resolves once every source has ended and no final is queued or in
    /// flight; cheap to await repeatedly.
    pub async fn settled_wait(&self) {
        let mut changed = self.changed.subscribe();
        loop {
            if self.settled() {
                return;
            }
            if changed.changed().await.is_err() {
                return;
            }
        }
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

/// Spawn a worker task inside `catch_unwind`; a panic becomes an internal
/// panic message instead of killing the runtime.
fn spawn_worker(
    name: &str,
    future: impl std::future::Future<Output = ()> + Send + 'static,
    panic_tx: mpsc::UnboundedSender<String>,
) -> tokio::task::JoinHandle<()> {
    let name = name.to_owned();
    tokio::spawn(async move {
        if let Err(payload) = AssertUnwindSafe(future).catch_unwind().await {
            let message = panic_text(&payload);
            tracing::error!(%message, worker = %name, "worker task panicked");
            let _ = panic_tx.send(format!("{name} panicked: {message}"));
        }
    })
}

/// One stream's assembled parts.
struct StreamParts {
    handle: StreamHandle,
    worker_tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// The running capture/transcription pipeline of one meeting.
pub struct Pipeline {
    streams: Vec<StreamParts>,
    drain: Arc<Drain>,
    cancel: CancellationToken,
    progress: Arc<Mutex<HashMap<Speaker, ProgressEntry>>>,
    commits_tx: watch::Sender<u64>,
    panic_rx: Option<mpsc::UnboundedReceiver<String>>,
    stop_done: bool,
}

impl Pipeline {
    /// Start one stream thread and two worker tasks per opened source.
    ///
    /// Must be called inside a tokio runtime. `asr` and `machine` come in
    /// beside `deps` (the lifecycle builds them from the config; tests
    /// script them directly), a small widening of the spec signature.
    pub fn start(
        sources: Vec<(Speaker, Box<dyn SampleSource>)>,
        deps: &EngineDeps,
        asr: Arc<AsrClient>,
        machine: &MachineParams,
        store: Arc<Mutex<TranscriptStore>>,
        clock: MeetingClock,
        cancel: CancellationToken,
    ) -> Self {
        let (panic_tx, panic_rx) = mpsc::unbounded_channel();
        let has_them = sources.iter().any(|(speaker, _)| *speaker == Speaker::Them);
        let drain = Arc::new(Drain::new(sources.len()));
        let (commits_tx, _) = watch::channel(0_u64);
        let progress: Arc<Mutex<HashMap<Speaker, ProgressEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let them_pending: Arc<Mutex<VecDeque<u64>>> = Arc::new(Mutex::new(VecDeque::new()));
        let them_open_t0 = Arc::new(AtomicU64::new(u64::MAX));

        // Build every clock first and hand out the shared watermark handles:
        // the Me echo hold reads the Them stream's watermark.
        let clocks: Vec<(Speaker, StreamClock)> = sources
            .iter()
            .map(|(speaker, _)| (*speaker, StreamClock::new(clock)))
            .collect();
        let watermarks: HashMap<Speaker, Arc<AtomicU64>> = clocks
            .iter()
            .map(|(speaker, stream_clock)| (*speaker, stream_clock.watermark_handle()))
            .collect();

        let mut streams = Vec::new();
        for ((speaker, stream_clock), (_speaker, source)) in
            clocks.into_iter().zip(sources.into_iter())
        {
            let status_source = match speaker {
                Speaker::Me => StatusSource::Mic,
                Speaker::Them => StatusSource::SystemAudio,
            };
            let (final_tx, final_rx) = mpsc::channel::<Segment>(16);
            let slot = Arc::new(LatestSlot::new());
            let final_seq_watermark = Arc::new(AtomicU64::new(0));
            let shutdown = Arc::new(AtomicBool::new(false));

            let pipes = StreamPipes {
                status_source,
                ui: deps.ui.clone(),
                final_tx,
                interim_slot: slot.clone(),
                final_seq_watermark: final_seq_watermark.clone(),
                drain: drain.clone(),
                them_pending: them_pending.clone(),
                them_open_t0: them_open_t0.clone(),
                panic_tx: panic_tx.clone(),
                shutdown: shutdown.clone(),
                idle_poll: deps.timings.idle_poll,
            };
            let handle = spawn_stream(
                speaker,
                source,
                (deps.vad)(),
                machine.clone(),
                stream_clock,
                pipes,
            );

            // Only a Me stream with a Them source open goes through the
            // echo hold.
            let echo = match (speaker, has_them) {
                (Speaker::Me, true) => Some(asr_worker::EchoCtx {
                    them_watermark: watermarks[&Speaker::Them].clone(),
                    them_pending: them_pending.clone(),
                    them_open_t0: them_open_t0.clone(),
                    hold: deps.timings.echo_hold,
                }),
                _ => None,
            };
            let ctx = Arc::new(WorkerCtx {
                speaker,
                ui: deps.ui.clone(),
                asr: asr.clone(),
                store: store.clone(),
                commits_tx: commits_tx.clone(),
                progress: progress.clone(),
                them_pending: them_pending.clone(),
                drain: drain.clone(),
                cancel: cancel.clone(),
                echo,
                timings: deps.timings,
            });
            let worker_tasks = vec![
                spawn_worker(
                    &format!("{speaker:?} final worker"),
                    asr_worker::run_final(ctx.clone(), final_rx),
                    panic_tx.clone(),
                ),
                spawn_worker(
                    &format!("{speaker:?} interim worker"),
                    asr_worker::run_interim(ctx, slot, final_seq_watermark),
                    panic_tx.clone(),
                ),
            ];
            streams.push(StreamParts {
                handle,
                worker_tasks,
            });
        }

        Self {
            streams,
            drain,
            cancel,
            progress,
            commits_tx,
            panic_rx: Some(panic_rx),
            stop_done: false,
        }
    }

    /// The latest uncommitted interim text per speaker with its time range;
    /// it stays listed from the first interim until the commit or drop.
    pub fn in_progress(&self) -> Vec<InProgressText> {
        let mut entries: Vec<InProgressText> = self
            .progress
            .lock()
            .unwrap()
            .values()
            .map(InProgressText::from)
            .collect();
        entries.sort_by_key(|entry| match entry.speaker {
            Speaker::Me => 0,
            Speaker::Them => 1,
        });
        entries
    }

    /// Changes after every committed utterance; the value is the count.
    pub fn commits(&self) -> watch::Receiver<u64> {
        self.commits_tx.subscribe()
    }

    /// Resolves when every source returned `Ended` and all queues and holds
    /// are empty (replay mode's finish signal).
    pub async fn drained(&self) {
        self.drain.settled_wait().await;
    }

    /// A shared handle to the drain state, so the meeting loop can await
    /// settlement in a task without borrowing the pipeline.
    pub fn drain_handle(&self) -> Arc<Drain> {
        self.drain.clone()
    }

    /// The receiver of internal panic messages from stream threads and
    /// worker tasks; taken once by the meeting loop.
    pub fn panics(&mut self) -> Option<mpsc::UnboundedReceiver<String>> {
        self.panic_rx.take()
    }

    /// The number of finals still queued or in flight (diagnostics).
    pub fn pending_finals(&self) -> usize {
        self.drain.pending_finals()
    }

    /// The spec's stop steps: flush both stream threads, wait up to `wait`
    /// for queued finals and pending echo decisions to land (an echo hold is
    /// never cut short by the stop itself, so a tail echo cannot commit while
    /// its Them final is still in flight), cancel what is still in flight,
    /// drop the rest with `TranscriptDropped`, and join every thread and task.
    pub async fn stop(&mut self, wait: Duration) {
        if self.stop_done {
            return;
        }
        self.stop_done = true;
        // 1. Flush: each stream thread closes its open segment as a Final.
        for parts in &self.streams {
            parts.handle.request_stop();
        }
        // 2. Wait for queued and flushed finals to finish, bounded. Echo
        //    holds keep waiting for their Them finals inside this window.
        let _ = tokio::time::timeout(wait, self.drain.settled_wait()).await;
        // 3. Cancel: every request still in flight aborts, and the workers
        //    emit TranscriptDropped for whatever did not commit.
        self.cancel.cancel();
        let joined = async {
            for parts in &mut self.streams {
                for task in &mut parts.worker_tasks {
                    let _ = task.await;
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(2), joined).await;
        // 4. Join the stream threads (they exit right after their flush; a
        //    wedged source would block here and is a manual-check item).
        for parts in &mut self.streams {
            parts.handle.join();
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // Safety net: an abandoned pipeline (panic path) still shuts its
        // threads down; the workers die with the meeting's cancel token.
        for parts in &mut self.streams {
            parts.handle.request_stop();
        }
    }
}

/// Format an internal panic message as an `App` error status event.
pub fn panic_status(text: &str) -> UiEvent {
    UiEvent::Status {
        source: StatusSource::App,
        level: StatusLevel::Error,
        text: format!("internal error: {text}"),
    }
}

/// Read a watermark without caring which stream it belongs to.
pub fn watermark_value(watermark: &AtomicU64) -> u64 {
    watermark.load(Ordering::Acquire)
}
