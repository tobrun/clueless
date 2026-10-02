//! Shared helpers for the engine integration tests: a scripted
//! `SpeechProb`, a paced in-memory `SampleSource`, an axum mock of the ASR
//! server that records requests, and a `Harness` that wires a pipeline.

#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};

use asr::client::AsrClient;
use clueless_types::UtteranceId;
use clueless_types::audio::{SampleSource, SourceError, SourceFactory, SourceRead};
use clueless_types::events::{Speaker, StatusLevel, StatusSink, StatusSource, UiEvent, Utterance};
use context::store::TranscriptStore;
use segmenter::machine::MachineParams;
use segmenter::vad::SpeechProb;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use engine::clock::MeetingClock;
use engine::deps::{EngineDeps, EngineTimings};
use engine::pipeline::Pipeline;

// ---------------------------------------------------------------- vad

/// Returns the given probabilities one frame at a time, then silence.
/// `reset` rewinds to the start (each stream gets its own instance).
pub struct FixedVad {
    probs: Vec<f32>,
    index: usize,
}

impl SpeechProb for FixedVad {
    fn prob(&mut self, _frame: &[f32]) -> f32 {
        let p = self.probs.get(self.index).copied().unwrap_or(0.0);
        self.index += 1;
        p
    }

    fn reset(&mut self) {
        self.index = 0;
    }
}

/// A vad factory handing out one probability script per created detector, in
/// creation order (the pipeline creates them in source order).
pub fn vad_factory(
    per_stream: Vec<Vec<f32>>,
) -> Arc<dyn Fn() -> Box<dyn SpeechProb> + Send + Sync> {
    let queue = Arc::new(Mutex::new(VecDeque::from(per_stream)));
    Arc::new(move || {
        let probs = queue.lock().unwrap().pop_front().unwrap_or_default();
        Box::new(FixedVad { probs, index: 0 })
    })
}

/// `(frames, probs)`: `silence` zeros, `speech` values at 0.9, `tail` zeros.
/// The tail must be at least 19 frames to close the utterance.
pub fn utterance(silence: usize, speech: usize, tail: usize) -> (usize, Vec<f32>) {
    let mut probs = vec![0.0f32; silence];
    probs.extend(std::iter::repeat_n(0.9, speech));
    probs.extend(std::iter::repeat_n(0.0, tail));
    let frames = probs.len();
    (frames, probs)
}

/// A speech script from alternating runs: `(frame_count, prob)` pairs.
pub fn pattern(runs: &[(usize, f32)]) -> (usize, Vec<f32>) {
    let mut probs = Vec::new();
    for (count, p) in runs {
        probs.extend(std::iter::repeat_n(*p, *count));
    }
    let frames = probs.len();
    (frames, probs)
}

// ---------------------------------------------------------------- source

/// Something special a scripted source returns at one frame index.
#[derive(Clone, Copy, Debug)]
pub enum FrameEvent {
    Gap,
    Reset(u32),
}

/// A `SampleSource` that feeds silent frames at `rate`, paced by wall time
/// divided by `speed` (1.0 mirrors a live meeting, 40.0 fast-forwards tests).
/// Frame `i` is released once `i * 32 ms / speed` of wall time passed since
/// the first read. Special events fire at their frame index without
/// consuming a vad call.
pub struct ScriptedSource {
    rate: u32,
    frames_total: usize,
    speed: f64,
    events: BTreeMap<usize, FrameEvent>,
    never_end: bool,
    start: Option<Instant>,
    next: usize,
}

impl ScriptedSource {
    pub fn new(frames_total: usize, speed: f64) -> Self {
        Self {
            rate: 16_000,
            frames_total,
            speed: speed.max(f64::EPSILON),
            events: BTreeMap::new(),
            never_end: false,
            start: None,
            next: 0,
        }
    }

    pub fn with_event(mut self, frame: usize, event: FrameEvent) -> Self {
        self.events.insert(frame, event);
        self
    }

    /// After the last frame keep returning `Empty` instead of `Ended`.
    pub fn never_end(mut self) -> Self {
        self.never_end = true;
        self
    }
}

impl SampleSource for ScriptedSource {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn read(&mut self, out: &mut [f32]) -> SourceRead {
        if self.next >= self.frames_total {
            if self.never_end {
                return SourceRead::Empty;
            }
            return SourceRead::Ended;
        }
        // Special events fire exactly at their frame index; the frame
        // itself is not delivered (a gap or reset loses it).
        if let Some(event) = self.events.remove(&self.next) {
            return match event {
                FrameEvent::Gap => SourceRead::Gap,
                FrameEvent::Reset(rate) => {
                    self.rate = rate;
                    SourceRead::Reset { sample_rate: rate }
                }
            };
        }
        let started = self.start.get_or_insert_with(Instant::now);
        let due = Duration::from_secs_f64(self.next as f64 * 512.0 / self.rate as f64 / self.speed);
        let elapsed = started.elapsed();
        if elapsed < due {
            return SourceRead::Empty;
        }
        let since_due = elapsed - due;
        let mut ready =
            (since_due.as_secs_f64() * self.rate as f64 * self.speed / 512.0).floor() as usize + 1;
        // never batch past the next event frame, so it always fires
        if let Some(&event_frame) = self.events.range(self.next + 1..).next().map(|(k, _)| k) {
            ready = ready.min(event_frame - self.next);
        }
        let frames = ready
            .min(self.frames_total - self.next)
            .min(out.len() / 512)
            .max(1);
        let n = frames * 512;
        out[..n].fill(0.0);
        self.next += frames;
        SourceRead::Samples(n)
    }
}

/// Source factory with no sources: only the lifecycle uses it, pipeline
/// tests pass sources directly.
pub struct EmptyFactory;

impl SourceFactory for EmptyFactory {
    fn speakers(&self) -> Vec<Speaker> {
        Vec::new()
    }

    fn open(
        &self,
        speaker: Speaker,
        _status: StatusSink,
    ) -> Result<Box<dyn SampleSource>, SourceError> {
        Err(SourceError::DeviceNotFound(format!("{speaker:?}")))
    }
}

// ---------------------------------------------------------------- mock asr

/// What the mock does with one transcription request.
#[derive(Clone, Debug)]
pub enum Respond {
    Text(String),
    Status(u16),
    Delay {
        ms: u64,
        then: Box<Respond>,
    },
    /// Hold the request until `release()` clears the block flag.
    Blocked(Box<Respond>),
}

impl Respond {
    pub fn text(s: &str) -> Self {
        Respond::Text(s.to_owned())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Final,
    Interim,
}

#[derive(Clone, Debug)]
pub struct RequestRec {
    pub kind: Kind,
    pub duration_ms: u64,
    pub arrival: Duration,
}

struct MockState {
    script_final: Mutex<VecDeque<Respond>>,
    script_interim: Mutex<VecDeque<Respond>>,
    default_final: Mutex<Respond>,
    default_interim: Mutex<Respond>,
    requests: Mutex<Vec<RequestRec>>,
    inflight: AtomicUsize,
    max_final: AtomicUsize,
    max_interim: AtomicUsize,
    blocked: watch::Sender<bool>,
    started: Instant,
    /// A request whose audio is at least this long counts as a `Final` for
    /// the queues and counters (tests size utterances to be separable).
    final_min_ms: AtomicU64,
}

pub struct MockAsr {
    pub base_url: String,
    state: Arc<MockState>,
}

impl MockAsr {
    pub async fn start() -> Self {
        let state = Arc::new(MockState {
            script_final: Mutex::new(VecDeque::new()),
            script_interim: Mutex::new(VecDeque::new()),
            default_final: Mutex::new(Respond::text("mock")),
            default_interim: Mutex::new(Respond::text("mock interim")),
            requests: Mutex::new(Vec::new()),
            inflight: AtomicUsize::new(0),
            max_final: AtomicUsize::new(0),
            max_interim: AtomicUsize::new(0),
            blocked: watch::channel(false).0,
            started: Instant::now(),
            final_min_ms: AtomicU64::new(0),
        });
        let app = Router::new()
            .route("/v1/audio/transcriptions", any(transcribe))
            .route("/v1/models", get(models))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base_url: format!("http://{addr}"),
            state,
        }
    }

    /// Queue one scripted answer for a final request (arrival order).
    pub fn enqueue_final(&self, response: Respond) {
        self.state.script_final.lock().unwrap().push_back(response);
    }

    /// Queue one scripted answer for an interim request (arrival order).
    pub fn enqueue_interim(&self, response: Respond) {
        self.state
            .script_interim
            .lock()
            .unwrap()
            .push_back(response);
    }

    pub fn set_default_final(&self, response: Respond) {
        *self.state.default_final.lock().unwrap() = response;
    }

    pub fn set_default_interim(&self, response: Respond) {
        *self.state.default_interim.lock().unwrap() = response;
    }

    /// Requests with at least this much audio count as finals.
    pub fn set_final_min_ms(&self, ms: u64) {
        self.state.final_min_ms.store(ms, Ordering::Relaxed);
    }

    pub fn block(&self) {
        self.state.blocked.send_replace(true);
    }

    pub fn release(&self) {
        self.state.blocked.send_replace(false);
    }

    pub fn requests(&self) -> Vec<RequestRec> {
        self.state.requests.lock().unwrap().clone()
    }

    pub fn request_count(&self) -> usize {
        self.state.requests.lock().unwrap().len()
    }

    pub fn max_inflight(&self, kind: Kind) -> usize {
        match kind {
            Kind::Final => self.state.max_final.load(Ordering::Relaxed),
            Kind::Interim => self.state.max_interim.load(Ordering::Relaxed),
        }
    }

    pub fn kind_count(&self, kind: Kind) -> usize {
        self.requests().iter().filter(|r| r.kind == kind).count()
    }

    /// Wait until at least `count` requests have arrived.
    pub async fn wait_requests(&self, count: usize, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.request_count() < count {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        true
    }

    /// Wall offset of the first recorded request of `kind` that started
    /// waiting (arrival time).
    pub fn arrival(&self, index: usize) -> Option<Duration> {
        self.requests().get(index).map(|r| r.arrival)
    }
}

async fn transcribe(State(state): State<Arc<MockState>>, mut multipart: Multipart) -> Response {
    let mut file_bytes = 0usize;
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file")
            && let Ok(bytes) = field.bytes().await
        {
            file_bytes += bytes.len();
        }
    }
    // 16 kHz mono 16-bit WAV plus a 44-byte header.
    let duration_ms = (file_bytes.saturating_sub(44) as u64) * 1000 / 32_000;
    let kind = if duration_ms >= state.final_min_ms.load(Ordering::Relaxed) {
        Kind::Final
    } else {
        Kind::Interim
    };
    state.requests.lock().unwrap().push(RequestRec {
        kind,
        duration_ms,
        arrival: state.started.elapsed(),
    });
    let now_inflight = state.inflight.fetch_add(1, Ordering::SeqCst) + 1;
    match kind {
        Kind::Final => {
            state.max_final.fetch_max(now_inflight, Ordering::Relaxed);
        }
        Kind::Interim => {
            state.max_interim.fetch_max(now_inflight, Ordering::Relaxed);
        }
    }
    let scripted = match kind {
        Kind::Final => state.script_final.lock().unwrap().pop_front(),
        Kind::Interim => state.script_interim.lock().unwrap().pop_front(),
    };
    let response = scripted.unwrap_or_else(|| match kind {
        Kind::Final => state.default_final.lock().unwrap().clone(),
        Kind::Interim => state.default_interim.lock().unwrap().clone(),
    });
    let response = play(&state, response).await;
    state.inflight.fetch_sub(1, Ordering::SeqCst);
    response
}

async fn play(state: &Arc<MockState>, mut response: Respond) -> Response {
    loop {
        match response {
            Respond::Text(text) => {
                return Json(serde_json::json!({ "text": text })).into_response();
            }
            Respond::Status(code) => {
                return StatusCode::from_u16(code)
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
                    .into_response();
            }
            Respond::Delay { ms, then } => {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                response = *then;
            }
            Respond::Blocked(then) => {
                let mut blocked = state.blocked.subscribe();
                while *blocked.borrow_and_update() {
                    if blocked.changed().await.is_err() {
                        break;
                    }
                }
                response = *then;
            }
        }
    }
}

async fn models() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "object": "list",
        "data": [{ "id": "mock-model", "object": "model" }],
    }))
}

// ---------------------------------------------------------------- harness

pub fn fast_timings() -> EngineTimings {
    EngineTimings {
        echo_hold: Duration::from_millis(300),
        stop_wait: Duration::from_millis(500),
        health_timeout: Duration::from_secs(1),
        asr_timeout: Duration::from_secs(2),
        asr_backoff: [Duration::from_millis(10), Duration::from_millis(20)],
        llm_connect: Duration::from_secs(1),
        llm_stall: Duration::from_secs(1),
        compress_retry: Duration::from_secs(1),
        idle_poll: Duration::from_millis(1),
    }
}

pub struct Opts {
    pub timings: EngineTimings,
    pub machine: MachineParams,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            timings: fast_timings(),
            machine: MachineParams::default(),
        }
    }
}

pub struct StreamSpec {
    pub speaker: Speaker,
    pub source: ScriptedSource,
    pub probs: Vec<f32>,
}

impl StreamSpec {
    pub fn new(speaker: Speaker, frames: usize, speed: f64, probs: Vec<f32>) -> Self {
        Self {
            speaker,
            source: ScriptedSource::new(frames, speed),
            probs,
        }
    }
}

/// One event with the wall time it arrived.
#[derive(Clone)]
pub struct Timed {
    pub at: Instant,
    pub event: UiEvent,
}

pub struct Harness {
    events: Arc<Mutex<Vec<Timed>>>,
    pub store: Arc<Mutex<TranscriptStore>>,
    pub pipeline: Pipeline,
    pub cancel: CancellationToken,
    pub started: Instant,
    stop_wait: Duration,
}

pub fn start(specs: Vec<StreamSpec>, mock: &MockAsr, opts: Opts) -> Harness {
    let events: Arc<Mutex<Vec<Timed>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let ui: StatusSink = Arc::new(move |event| {
        sink.lock().unwrap().push(Timed {
            at: Instant::now(),
            event,
        });
    });
    let vad = vad_factory(specs.iter().map(|spec| spec.probs.clone()).collect());
    let deps = EngineDeps {
        factory: Arc::new(EmptyFactory),
        ui,
        vad,
        timings: opts.timings,
        compress_threshold_tokens: 1_000,
    };
    let asr = Arc::new(AsrClient::new(
        mock.base_url.clone(),
        "mock-model",
        opts.timings.asr_timeout,
        opts.timings.asr_backoff,
    ));
    let store = Arc::new(Mutex::new(TranscriptStore::new()));
    let cancel = CancellationToken::new();
    let sources = specs
        .into_iter()
        .map(|spec| {
            let source: Box<dyn SampleSource> = Box::new(spec.source);
            (spec.speaker, source)
        })
        .collect();
    let pipeline = Pipeline::start(
        sources,
        &deps,
        asr,
        &opts.machine,
        store.clone(),
        MeetingClock::new(),
        cancel.clone(),
    );
    Harness {
        events,
        store,
        pipeline,
        cancel,
        started: Instant::now(),
        stop_wait: opts.timings.stop_wait,
    }
}

impl Harness {
    pub fn snapshot(&self) -> Vec<UiEvent> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.event.clone())
            .collect()
    }

    pub fn finals(&self) -> Vec<Utterance> {
        self.snapshot()
            .into_iter()
            .filter_map(|event| match event {
                UiEvent::TranscriptFinal(utterance) => Some(utterance),
                _ => None,
            })
            .collect()
    }

    pub fn interim_ids(&self) -> Vec<UtteranceId> {
        self.snapshot()
            .into_iter()
            .filter_map(|event| match event {
                UiEvent::TranscriptInterim { id, .. } => Some(id),
                _ => None,
            })
            .collect()
    }

    pub fn dropped(&self) -> Vec<UtteranceId> {
        self.snapshot()
            .into_iter()
            .filter_map(|event| match event {
                UiEvent::TranscriptDropped { id } => Some(id),
                _ => None,
            })
            .collect()
    }

    pub fn asr_error_statuses(&self) -> Vec<String> {
        self.snapshot()
            .into_iter()
            .filter_map(|event| match event {
                UiEvent::Status {
                    source: StatusSource::Asr,
                    level: StatusLevel::Error,
                    text,
                } => Some(text),
                _ => None,
            })
            .collect()
    }

    pub fn store_texts(&self) -> Vec<(Speaker, String)> {
        self.store
            .lock()
            .unwrap()
            .lines()
            .iter()
            .map(|line| (line.utterance.id.speaker, line.utterance.text.clone()))
            .collect()
    }

    /// Poll until `pred` sees the event list, up to `timeout`.
    pub async fn wait_until(
        &self,
        timeout: Duration,
        pred: impl Fn(&Vec<UiEvent>) -> bool,
    ) -> Vec<UiEvent> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let events = self.snapshot();
            if pred(&events) {
                return events;
            }
            if tokio::time::Instant::now() >= deadline {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    pub async fn wait_finals(&self, count: usize, timeout: Duration) -> Vec<Utterance> {
        let events = self
            .wait_until(timeout, |events| {
                events
                    .iter()
                    .filter(|event| matches!(event, UiEvent::TranscriptFinal(_)))
                    .count()
                    >= count
            })
            .await;
        events
            .into_iter()
            .filter_map(|event| match event {
                UiEvent::TranscriptFinal(utterance) => Some(utterance),
                _ => None,
            })
            .collect()
    }

    /// Wall time offset of the first event matching `pred`.
    pub fn event_time(&self, pred: impl Fn(&UiEvent) -> bool) -> Option<Duration> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .find(|timed| pred(&timed.event))
            .map(|timed| timed.at - self.started)
    }

    pub async fn stop(&mut self) {
        self.pipeline.stop(self.stop_wait).await;
    }

    pub async fn drained(&self) {
        self.pipeline.drained().await;
    }
}

// ---------------------------------------------------------------- wav

/// Write a PCM WAV (`samples.len() == channels * frames`, interleaved).
pub fn write_wav(path: &Path, channels: u16, rate: u32, samples: &[i16]) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create wav");
    for sample in samples {
        writer.write_sample(*sample).expect("write sample");
    }
    writer.finalize().expect("finalize wav");
}

/// A unique file path under the temp dir for one test.
pub fn temp_path(name: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "clueless-test-{}-{}-{}.wav",
        name,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    path
}
