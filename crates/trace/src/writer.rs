//! The disk opener and the two writer threads behind [`DiskTrace`].
//!
//! Per session one thread writes records from a queue of 4096 and, when
//! audio is on, a second thread writes frames from its own queue of 4096,
//! so a slow audio write can never push out text records. Callers use
//! `try_send`: nothing here blocks the meeting, and a full queue drops the
//! message and counts it, visible later as a `records_lost` record. The
//! first I/O error stops both threads, reports once through `on_failure`,
//! and the meeting goes on.

use crate::audio::{Anchor, AudioFile};
use crate::manifest::{MANIFEST_FILE, Manifest, Origin, SCHEMA, SessionStart};
use crate::paths::{
    AUDIO_DIR, EVENTS_FILE, RUNS_DIR, SESSIONS_DIR, create_private_dir, create_private_file,
    create_unique, open_private_append, utc_name,
};
use crate::record::{Body, EndReason, Record, Speaker};
use crate::sink::{FailureSink, Location, Stamper, TraceOpener, TraceSink};
use std::collections::BTreeMap;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Depth of each of the two queues (about a minute of audio frames).
const QUEUE_SIZE: usize = 4096;
/// `close` gives the whole shutdown this much time before going on anyway.
const CLOSE_DEADLINE: Duration = Duration::from_secs(2);
/// Pause between `end` record retries and shutdown polls.
const RETRY_PAUSE: Duration = Duration::from_millis(5);

/// Where a [`DiskOpener`] puts its traces: new session directories under
/// `<data_dir>/sessions/`, or one run directory below an existing session.
#[derive(Debug, Clone)]
pub enum Target {
    /// One directory per meeting under `<data_dir>/sessions/`.
    Sessions,
    /// The trace of a re-run of `session_dir`, in its own fresh directory
    /// under `<session_dir>/runs/`.
    Run { session_dir: PathBuf },
}

/// A hook a writer thread calls before each line / frame it processes;
/// tests use it to hold a thread at a known point.
pub type ThreadHook = Arc<dyn Fn() + Send + Sync>;

/// Reports a writer failure once per trace and flips the shared `failed`
/// flag so both threads stop.
pub struct FailureOnce {
    fired: AtomicBool,
    failed: Arc<AtomicBool>,
    on_failure: FailureSink,
}

impl FailureOnce {
    /// Wrap the sink the opener supplied, sharing the stop flag with it.
    pub fn new(failed: Arc<AtomicBool>, on_failure: FailureSink) -> Arc<Self> {
        Arc::new(Self {
            fired: AtomicBool::new(false),
            failed,
            on_failure,
        })
    }

    /// Whether a writer already stopped on an error.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Stop the writers; the sink hears the message exactly once.
    pub fn fire(&self, message: String) {
        self.failed.store(true, Ordering::Relaxed);
        if !self.fired.swap(true, Ordering::SeqCst) {
            (self.on_failure)(message);
        }
    }
}

/// A message on the record queue.
#[derive(Debug)]
pub enum TraceMsg {
    /// One stamped record to append.
    Record(Record),
    /// The `end` record, last of the file; `ack` hears it after the flush.
    End(Record, SyncSender<()>),
}

/// A message on the audio queue: one frame for one speaker.
#[derive(Debug)]
pub struct AudioFrame {
    pub speaker: Speaker,
    pub t_ms: u64,
    pub samples: Vec<f32>,
}

/// Opens the file-backed sink for one meeting or re-run.
pub struct DiskOpener {
    /// The data directory (`~/.clueless` or `--data-dir`).
    pub data_dir: PathBuf,
    /// Whether audio WAVs are written too.
    pub audio: bool,
    pub origin: Origin,
    pub speed: f64,
    pub app_version: String,
    pub git_commit: String,
    pub target: Target,
    /// The run directory a `Target::Run` writes to, made by `for_run`.
    run_dir: Option<PathBuf>,
    run_started_at_ms: u64,
    record_hook: Option<ThreadHook>,
    audio_hook: Option<ThreadHook>,
}

impl DiskOpener {
    /// An opener that writes new session directories under `data_dir`.
    pub fn new(
        data_dir: impl Into<PathBuf>,
        audio: bool,
        origin: Origin,
        speed: f64,
        app_version: String,
        git_commit: String,
    ) -> Self {
        Self {
            data_dir: data_dir.into(),
            audio,
            origin,
            speed,
            app_version,
            git_commit,
            target: Target::Sessions,
            run_dir: None,
            run_started_at_ms: 0,
            record_hook: None,
            audio_hook: None,
        }
    }

    /// An opener whose trace becomes a run of `session_dir`: it creates
    /// `<session_dir>/runs/<utc name>` right away, never writes audio, and
    /// names the source session in its manifest.
    pub fn for_run(
        session_dir: impl Into<PathBuf>,
        speed: f64,
        app_version: String,
        git_commit: String,
    ) -> io::Result<Self> {
        let session_dir = session_dir.into();
        let started_at_ms = wall_ms();
        let runs = session_dir.join(RUNS_DIR);
        create_private_dir(&runs)
            .map_err(|error| io::Error::other(format!("{}: {error}", runs.display())))?;
        let run_dir = create_unique(&runs, &utc_name(started_at_ms))?;
        Ok(Self {
            data_dir: session_dir.clone(),
            audio: false,
            origin: Origin::Rerun,
            speed,
            app_version,
            git_commit,
            target: Target::Run { session_dir },
            run_dir: Some(run_dir),
            run_started_at_ms: started_at_ms,
            record_hook: None,
            audio_hook: None,
        })
    }

    /// The directory a `for_run` opener writes its trace into.
    pub fn run_dir(&self) -> Option<&Path> {
        self.run_dir.as_deref()
    }

    /// Hold the record thread at a known point (for tests).
    pub fn with_record_hook(mut self, hook: ThreadHook) -> Self {
        self.record_hook = Some(hook);
        self
    }

    /// Hold the audio thread at a known point (for tests).
    pub fn with_audio_hook(mut self, hook: ThreadHook) -> Self {
        self.audio_hook = Some(hook);
        self
    }
}

impl TraceOpener for DiskOpener {
    fn open(
        &self,
        start: SessionStart,
        on_failure: FailureSink,
    ) -> Result<Arc<dyn TraceSink>, String> {
        let (dir, started_at_ms, audio_on) = match &self.target {
            Target::Sessions => {
                let sessions = self.data_dir.join(SESSIONS_DIR);
                create_private_dir(&sessions)
                    .map_err(|error| format!("cannot create {}: {error}", sessions.display()))?;
                let started_at_ms = wall_ms();
                let dir = create_unique(&sessions, &utc_name(started_at_ms))
                    .map_err(|error| format!("cannot create {}: {error}", sessions.display()))?;
                (dir, started_at_ms, self.audio)
            }
            Target::Run { .. } => {
                let dir = self
                    .run_dir
                    .clone()
                    .ok_or_else(|| "run directory was not created".to_string())?;
                (dir, self.run_started_at_ms, false)
            }
        };

        write_manifest(&dir, self, &start, started_at_ms, audio_on)?;

        let events_path = dir.join(EVENTS_FILE);
        let events = open_private_append(&events_path)
            .map_err(|error| format!("cannot open {}: {error}", events_path.display()))?;
        events
            .try_lock()
            .map_err(|error| format!("cannot lock {}: {error}", events_path.display()))?;

        let stamper = Arc::new(Stamper::new());
        let lost_records = Arc::new(AtomicU64::new(0));
        let lost_audio = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let on_error = FailureOnce::new(failed.clone(), on_failure);
        let audio_done = Arc::new(AtomicBool::new(false));

        let (record_tx, record_rx) = sync_channel::<TraceMsg>(QUEUE_SIZE);
        let out: Box<dyn Write + Send> = Box::new(BufWriter::new(events));
        {
            let stamper = Arc::clone(&stamper);
            let lost_records = Arc::clone(&lost_records);
            let lost_audio = Arc::clone(&lost_audio);
            let on_error = Arc::clone(&on_error);
            let hook = self.record_hook.clone();
            // Detached: the thread ends by itself once the last sender is
            // dropped, which `close` makes happen.
            thread::Builder::new()
                .name("clueless-trace".into())
                .spawn(move || {
                    write_records(
                        out,
                        record_rx,
                        &stamper,
                        &lost_records,
                        &lost_audio,
                        &on_error,
                        hook.as_ref(),
                    )
                })
                .map_err(|error| format!("cannot start the trace writer: {error}"))?;
        }

        let audio_tx = if audio_on {
            let (tx, rx) = sync_channel::<AudioFrame>(QUEUE_SIZE);
            let worker = AudioWorker {
                audio_dir: dir.join(AUDIO_DIR),
                rx,
                anchors: record_tx.clone(),
                lost_records: Arc::clone(&lost_records),
                stamper: Arc::clone(&stamper),
                on_error: Arc::clone(&on_error),
                done: Arc::clone(&audio_done),
                hook: self.audio_hook.clone(),
            };
            // Detached: it drains its queue when `close` drops the sender,
            // finalizes the WAVs and reports through `audio_done`.
            thread::Builder::new()
                .name("clueless-trace-audio".into())
                .spawn(move || run_audio_worker(worker))
                .map_err(|error| format!("cannot start the audio writer: {error}"))?;
            Some(tx)
        } else {
            None
        };

        Ok(Arc::new(DiskTrace {
            dir,
            audio: audio_on,
            stamper,
            record_tx: Mutex::new(Some(record_tx)),
            audio_tx: Mutex::new(audio_tx),
            lost_records,
            lost_audio,
            failed,
            audio_done,
            closed: AtomicBool::new(false),
        }))
    }
}

fn write_manifest(
    dir: &Path,
    opener: &DiskOpener,
    start: &SessionStart,
    started_at_ms: u64,
    audio_on: bool,
) -> Result<(), String> {
    let manifest = Manifest {
        schema: SCHEMA,
        started_at_ms,
        origin: opener.origin,
        speed: opener.speed,
        app_version: opener.app_version.clone(),
        git_commit: opener.git_commit.clone(),
        audio: audio_on,
        session: start.clone(),
    };
    let path = dir.join(MANIFEST_FILE);
    let mut file = create_private_file(&path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    let mut value = serde_json::to_value(&manifest).map_err(|error| error.to_string())?;
    if let Target::Run { session_dir } = &opener.target {
        value["source_session"] = session_dir.display().to_string().into();
    }
    serde_json::to_writer_pretty(&mut file, &value)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.flush()
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(())
}

/// Append one record as a JSON line and flush it, so a killed process loses
/// at most a partial last line.
fn write_line(out: &mut dyn Write, record: &Record) -> io::Result<()> {
    serde_json::to_writer(&mut *out, record).map_err(io::Error::other)?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Write the `records_lost` line for what the full queues dropped, if any.
fn write_lost(
    out: &mut dyn Write,
    stamper: &Stamper,
    lost_records: &AtomicU64,
    lost_audio: &AtomicU64,
) -> io::Result<()> {
    let records = lost_records.swap(0, Ordering::Relaxed);
    let audio_frames = lost_audio.swap(0, Ordering::Relaxed);
    if records == 0 && audio_frames == 0 {
        return Ok(());
    }
    write_line(
        out,
        &stamper.stamp(Body::RecordsLost {
            records,
            audio_frames,
        }),
    )
}

/// The record thread: one flushed line per message, `records_lost` before
/// the next record whenever a drop counter is above zero, and one failure
/// report after which later messages are discarded.
pub fn write_records(
    mut out: Box<dyn Write + Send>,
    rx: Receiver<TraceMsg>,
    stamper: &Stamper,
    lost_records: &AtomicU64,
    lost_audio: &AtomicU64,
    on_error: &FailureOnce,
    hook: Option<&ThreadHook>,
) {
    for message in rx.iter() {
        if let Some(hook) = &hook {
            hook();
        }
        let (record, ack) = match message {
            TraceMsg::Record(record) => (record, None),
            TraceMsg::End(record, ack) => (record, Some(ack)),
        };
        let result = write_lost(&mut out, stamper, lost_records, lost_audio)
            .and_then(|()| write_line(&mut out, &record));
        let written = match result {
            Ok(()) => true,
            Err(error) => {
                on_error.fire(error.to_string());
                false
            }
        };
        if let Some(ack) = ack {
            let _ = ack.send(());
            break;
        }
        if !written {
            break;
        }
    }
}

/// Everything the audio thread owns.
struct AudioWorker {
    audio_dir: PathBuf,
    rx: Receiver<AudioFrame>,
    anchors: SyncSender<TraceMsg>,
    lost_records: Arc<AtomicU64>,
    stamper: Arc<Stamper>,
    on_error: Arc<FailureOnce>,
    done: Arc<AtomicBool>,
    hook: Option<ThreadHook>,
}

/// The audio thread: one `AudioFile` per speaker, created on its first
/// frame, anchors handed to the record queue, WAVs finalized on the way
/// out so a stopped session still has readable files. It drains its queue
/// when the last sender is dropped (`close`), so frames queued before a
/// stop are still written.
fn run_audio_worker(worker: AudioWorker) {
    let mut files: BTreeMap<Speaker, AudioFile> = BTreeMap::new();
    for frame in worker.rx.iter() {
        if worker.on_error.failed() {
            continue;
        }
        if let Some(hook) = &worker.hook {
            hook();
        }
        let pushed = push_frame(&worker, &mut files, &frame);
        match pushed {
            Ok(Some(anchor)) => {
                let record = worker.stamper.stamp(Body::AudioAnchor {
                    speaker: frame.speaker,
                    t_ms: anchor.t_ms,
                    sample_index: anchor.sample_index,
                });
                if let Err(TrySendError::Full(_)) =
                    worker.anchors.try_send(TraceMsg::Record(record))
                {
                    // A record dropped here is a lost record; the counter
                    // makes it visible in the file.
                    worker.lost_records.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(None) => {}
            Err(error) => {
                worker.on_error.fire(error.to_string());
                break;
            }
        }
    }
    for (_, file) in files {
        if let Err(error) = file.finalize() {
            worker.on_error.fire(error.to_string());
        }
    }
    worker.done.store(true, Ordering::Release);
}

fn push_frame(
    worker: &AudioWorker,
    files: &mut BTreeMap<Speaker, AudioFile>,
    frame: &AudioFrame,
) -> io::Result<Option<Anchor>> {
    let file = match files.get_mut(&frame.speaker) {
        Some(file) => file,
        None => {
            create_private_dir(&worker.audio_dir)?;
            let name = match frame.speaker {
                Speaker::Me => "me",
                Speaker::Them => "them",
            };
            let file = AudioFile::create(&worker.audio_dir.join(format!("{name}.wav")))?;
            files.entry(frame.speaker).or_insert(file)
        }
    };
    file.push(frame.t_ms, &frame.samples)
}

/// The sink one meeting records through: two bounded queues, no blocking,
/// no-op after a failure or a close.
pub struct DiskTrace {
    dir: PathBuf,
    audio: bool,
    stamper: Arc<Stamper>,
    record_tx: Mutex<Option<SyncSender<TraceMsg>>>,
    audio_tx: Mutex<Option<SyncSender<AudioFrame>>>,
    lost_records: Arc<AtomicU64>,
    lost_audio: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    audio_done: Arc<AtomicBool>,
    closed: AtomicBool,
}

impl DiskTrace {
    fn stopped(&self) -> bool {
        self.closed.load(Ordering::Relaxed) || self.failed.load(Ordering::Relaxed)
    }
}

impl TraceSink for DiskTrace {
    fn record(&self, body: Body) {
        if self.stopped() {
            return;
        }
        let record = self.stamper.stamp(body);
        let guard = self.record_tx.lock().unwrap();
        if let Some(tx) = guard.as_ref() {
            match tx.try_send(TraceMsg::Record(record)) {
                Err(TrySendError::Full(_)) => {
                    self.lost_records.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => {}
                Ok(()) => {}
            }
        }
    }

    fn audio(&self, speaker: Speaker, t_start_ms: u64, frame: &[f32]) {
        if self.stopped() {
            return;
        }
        let guard = self.audio_tx.lock().unwrap();
        if let Some(tx) = guard.as_ref() {
            let message = AudioFrame {
                speaker,
                t_ms: t_start_ms,
                samples: frame.to_vec(),
            };
            match tx.try_send(message) {
                Err(TrySendError::Full(_)) => {
                    self.lost_audio.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => {}
                Ok(()) => {}
            }
        }
    }

    stamper_forwarding!();

    fn location(&self) -> Option<Location> {
        Some(Location {
            dir: self.dir.clone(),
            audio: self.audio,
        })
    }

    fn close(&self, reason: EndReason) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let deadline = Instant::now() + CLOSE_DEADLINE;
        // Drop the audio sender so its thread drains the queue, finalizes
        // the WAV files and reports through `audio_done`; give up at the
        // deadline if that thread is stuck.
        if self.audio_tx.lock().unwrap().take().is_some() {
            while !self.audio_done.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
        }
        // Send the `end` record, retrying while a full queue drains.
        let (ack_tx, ack_rx) = sync_channel::<()>(1);
        let end = self.stamper.stamp(Body::End { reason });
        let mut sent = false;
        loop {
            let guard = self.record_tx.lock().unwrap();
            let Some(tx) = guard.as_ref() else { break };
            match tx.try_send(TraceMsg::End(end.clone(), ack_tx.clone())) {
                Ok(()) => {
                    sent = true;
                    break;
                }
                Err(TrySendError::Full(_)) => {
                    drop(guard);
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(RETRY_PAUSE);
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        drop(self.record_tx.lock().unwrap().take());
        // Wait for the record thread's reply until the deadline passes.
        if sent {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let _ = ack_rx.recv_timeout(remaining);
        }
    }
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Body;
    use std::sync::atomic::AtomicU32;

    /// A `Write` that commits to the shared buffer only when a flush
    /// succeeds, and fails from its second flush on; the first line lands.
    struct FlakyOut {
        buffer: Arc<Mutex<Vec<u8>>>,
        pending: Vec<u8>,
        flushes: Arc<AtomicU32>,
    }

    impl FlakyOut {
        fn new(buffer: Arc<Mutex<Vec<u8>>>, flushes: Arc<AtomicU32>) -> Self {
            Self {
                buffer,
                pending: Vec::new(),
                flushes,
            }
        }
    }

    impl Write for FlakyOut {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.pending.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.flushes.fetch_add(1, Ordering::SeqCst) >= 1 {
                return Err(io::Error::other("disk gone"));
            }
            let pending = std::mem::take(&mut self.pending);
            self.buffer.lock().unwrap().extend_from_slice(&pending);
            Ok(())
        }
    }

    struct SharedOut {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for SharedOut {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buffer.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn counters() -> (Arc<AtomicU64>, Arc<AtomicU64>) {
        (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)))
    }

    #[test]
    fn a_write_failure_reports_once_and_later_records_are_discarded() {
        let calls = Arc::new(AtomicU64::new(0));
        let calls2 = Arc::clone(&calls);
        let on_error = FailureOnce::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |_msg: String| {
                calls2.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let flushes = Arc::new(AtomicU32::new(0));
        let out = Box::new(FlakyOut::new(Arc::clone(&buffer), Arc::clone(&flushes)));
        let (tx, rx) = sync_channel::<TraceMsg>(8);
        for _ in 0..4 {
            tx.send(TraceMsg::Record(Record {
                seq: 0,
                at_ms: 0,
                body: Body::ClockStarted,
            }))
            .unwrap();
        }
        drop(tx);
        let stamper = Stamper::new();
        let (lost_records, lost_audio) = counters();
        write_records(
            out,
            rx,
            &stamper,
            &lost_records,
            &lost_audio,
            &on_error,
            None,
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "reported exactly once");
        assert!(on_error.failed());
        let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        assert_eq!(
            text.lines().count(),
            1,
            "only the line before the failure is on disk"
        );
        assert_eq!(
            flushes.load(Ordering::SeqCst),
            2,
            "the two records after the failure never reach the disk"
        );
    }

    #[test]
    fn dropped_messages_are_reported_as_records_lost_before_the_next_record() {
        let on_error = FailureOnce::new(Arc::new(AtomicBool::new(false)), Arc::new(|_msg| {}));
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let out = Box::new(SharedOut {
            buffer: Arc::clone(&buffer),
        });
        let (tx, rx) = sync_channel::<TraceMsg>(8);
        // A real caller stamps on enqueue, like `DiskTrace::record` does;
        // the `records_lost` line is stamped when the writer reaches it.
        let stamper = Stamper::new();
        let record = stamper.stamp(Body::ClockStarted);
        tx.send(TraceMsg::Record(record)).unwrap();
        drop(tx);
        let (lost_records, lost_audio) = counters();
        lost_records.store(7, Ordering::Relaxed);
        lost_audio.store(3, Ordering::Relaxed);
        write_records(
            out,
            rx,
            &stamper,
            &lost_records,
            &lost_audio,
            &on_error,
            None,
        );
        let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["kind"], "records_lost");
        assert_eq!(lines[0]["records"], 7);
        assert_eq!(lines[0]["audio_frames"], 3);
        assert_eq!(
            lines[0]["seq"], 2,
            "the lost line is stamped from the shared stamper"
        );
        assert_eq!(lines[1]["kind"], "clock_started");
        assert_eq!(lines[1]["seq"], 1);
        assert_eq!(
            lost_records.load(Ordering::Relaxed),
            0,
            "the counters reset after the line"
        );
    }
}
