//! Shared helpers for the engine integration tests: a scripted
//! `SpeechProb`, a paced in-memory `SampleSource`, an axum mock of the ASR
//! server that records requests, and a `Harness` that wires a pipeline.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};

use asr::client::AsrClient;
use clueless_types::UtteranceId;
use clueless_types::audio::{SampleSource, SourceError, SourceFactory, SourceRead};
use clueless_types::config::{AsrConfig, AssistConfig, Config, LlmConfig, VadConfig};
use clueless_types::events::{
    EngineCommand, MeetingState, Speaker, StatusLevel, StatusSink, StatusSource, UiEvent, Utterance,
};
use clueless_types::profile::AssistProfile;
use context::store::TranscriptStore;
use segmenter::machine::MachineParams;
use segmenter::vad::SpeechProb;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use engine::clock::MeetingClock;
use engine::deps::{EngineDeps, EngineTimings};
use engine::meeting::Engine;
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
    models: Mutex<Vec<String>>,
}

pub struct MockAsr {
    pub base_url: String,
    pub port: u16,
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
            models: Mutex::new(vec!["mock-model".to_owned()]),
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
            port: addr.port(),
            state,
        }
    }

    /// Replace the ids `GET /v1/models` lists (health-check tests).
    pub fn set_models(&self, models: Vec<String>) {
        *self.state.models.lock().unwrap() = models;
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

async fn models(State(state): State<Arc<MockState>>) -> Json<serde_json::Value> {
    let data: Vec<serde_json::Value> = state
        .models
        .lock()
        .unwrap()
        .iter()
        .map(|id| serde_json::json!({ "id": id, "object": "model" }))
        .collect();
    Json(serde_json::json!({ "object": "list", "data": data }))
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
        turn_settle: Duration::from_millis(50),
        turn_max_wait: Duration::from_millis(400),
        auto_min_gap: Duration::from_millis(150),
        auto_failure_pause: Duration::from_millis(800),
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
        None,
        None,
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

// ------------------------------------------------------- meeting support

/// Vad script for the meeting harness: a probability script, or a
/// detector that panics at its Nth scored frame.
pub enum VadScript {
    Probs(Vec<f32>),
    PanicsAt(usize),
}

pub struct PanickingVad {
    calls: usize,
    at: usize,
}

impl SpeechProb for PanickingVad {
    fn prob(&mut self, _frame: &[f32]) -> f32 {
        self.calls += 1;
        if self.calls == self.at {
            panic!("scripted vad panic");
        }
        0.9
    }

    fn reset(&mut self) {}
}

/// Vad factory handing out one script per created detector, in creation
/// order (the pipeline creates detectors in source-open order).
pub fn vad_factory_scripts(
    scripts: Vec<VadScript>,
) -> Arc<dyn Fn() -> Box<dyn SpeechProb> + Send + Sync> {
    let queue = Arc::new(Mutex::new(VecDeque::from(scripts)));
    Arc::new(move || match queue.lock().unwrap().pop_front() {
        Some(VadScript::Probs(probs)) => Box::new(FixedVad { probs, index: 0 }),
        Some(VadScript::PanicsAt(at)) => Box::new(PanickingVad { calls: 0, at }),
        None => Box::new(FixedVad {
            probs: Vec::new(),
            index: 0,
        }),
    })
}

/// What `ScriptedFactory::open` hands out for one open call.
#[derive(Clone)]
pub struct SourcePlan {
    pub frames: usize,
    pub speed: f64,
    pub never_end: bool,
}

impl SourcePlan {
    pub fn new(frames: usize) -> Self {
        Self {
            frames,
            speed: 1000.0,
            never_end: false,
        }
    }

    /// Runs out of frames but never returns `Ended` (a live-like source).
    pub fn endless(frames: usize) -> Self {
        Self {
            never_end: true,
            ..Self::new(frames)
        }
    }
}

#[derive(Clone)]
pub enum OpenPlan {
    Ok(SourcePlan),
    Fail(SourceError),
}

/// Source factory with a per-speaker queue of open outcomes; opens pop
/// in order, so a second meeting gets the second plan.
pub struct ScriptedFactory {
    speakers: Vec<Speaker>,
    plans: Mutex<HashMap<Speaker, VecDeque<OpenPlan>>>,
}

impl ScriptedFactory {
    pub fn new(entries: Vec<(Speaker, Vec<OpenPlan>)>) -> Self {
        let mut speakers = Vec::new();
        let mut plans = HashMap::new();
        for (speaker, list) in entries {
            if !speakers.contains(&speaker) {
                speakers.push(speaker);
            }
            plans.insert(speaker, VecDeque::from(list));
        }
        Self {
            speakers,
            plans: Mutex::new(plans),
        }
    }
}

impl SourceFactory for ScriptedFactory {
    fn speakers(&self) -> Vec<Speaker> {
        self.speakers.clone()
    }

    fn open(
        &self,
        speaker: Speaker,
        _status: StatusSink,
    ) -> Result<Box<dyn SampleSource>, SourceError> {
        let plan = self
            .plans
            .lock()
            .unwrap()
            .get_mut(&speaker)
            .and_then(|queue| queue.pop_front());
        match plan {
            Some(OpenPlan::Ok(plan)) => {
                let source = ScriptedSource::new(plan.frames, plan.speed);
                Ok(Box::new(if plan.never_end {
                    source.never_end()
                } else {
                    source
                }))
            }
            Some(OpenPlan::Fail(error)) => Err(error),
            None => Err(SourceError::DeviceNotFound(format!(
                "no scripted source for {speaker:?}"
            ))),
        }
    }
}

// ---------------------------------------------------------------- mock llm

/// One SSE production step of a scripted chat stream.
#[derive(Clone, Debug)]
pub enum Step {
    Chunk(String),
    Sleep(Duration),
}

impl Step {
    pub fn chunk(text: &str) -> Self {
        Step::Chunk(text.to_owned())
    }

    pub fn sleep_ms(ms: u64) -> Self {
        Step::Sleep(Duration::from_millis(ms))
    }
}

#[derive(Clone, Debug)]
pub enum LlmReply {
    /// Delta steps, then `data: [DONE]`.
    Stream(Vec<Step>),
    /// Delta steps, then the body ends without `[DONE]` (client: Closed).
    CloseMidStream(Vec<Step>),
    /// HTTP error status with a JSON error body.
    Http { status: u16, body: String },
}

impl LlmReply {
    pub fn stream(deltas: &[&str]) -> Self {
        LlmReply::Stream(
            deltas
                .iter()
                .map(|d| Step::Chunk((*d).to_owned()))
                .collect(),
        )
    }

    /// Deltas with `gap` between them (for cancellation races).
    pub fn slow_stream(deltas: &[&str], gap: Duration) -> Self {
        let mut steps = Vec::new();
        for (index, delta) in deltas.iter().enumerate() {
            if index > 0 {
                steps.push(Step::Sleep(gap));
            }
            steps.push(Step::Chunk((*delta).to_owned()));
        }
        LlmReply::Stream(steps)
    }
}

struct LlmState {
    replies: Mutex<VecDeque<LlmReply>>,
    models: Mutex<Vec<String>>,
    bodies: Mutex<Vec<serde_json::Value>>,
    started: Instant,
    arrivals: Mutex<Vec<Instant>>,
    inflight: std::sync::atomic::AtomicUsize,
    max_inflight: std::sync::atomic::AtomicUsize,
}

pub struct MockLlm {
    pub base_url: String,
    pub port: u16,
    state: Arc<LlmState>,
}

impl MockLlm {
    pub async fn start() -> Self {
        let state = Arc::new(LlmState {
            replies: Mutex::new(VecDeque::new()),
            models: Mutex::new(vec!["mock-model".to_owned()]),
            bodies: Mutex::new(Vec::new()),
            started: Instant::now(),
            arrivals: Mutex::new(Vec::new()),
            inflight: std::sync::atomic::AtomicUsize::new(0),
            max_inflight: std::sync::atomic::AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/v1/chat/completions", post(llm_chat))
            .route("/v1/models", get(llm_models))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base_url: format!("http://{addr}"),
            port: addr.port(),
            state,
        }
    }

    /// Queue one scripted answer (arrival order; default is a one-delta stream).
    pub fn enqueue(&self, reply: LlmReply) {
        self.state.replies.lock().unwrap().push_back(reply);
    }

    pub fn set_models(&self, models: Vec<String>) {
        *self.state.models.lock().unwrap() = models;
    }

    pub fn bodies(&self) -> Vec<serde_json::Value> {
        self.state.bodies.lock().unwrap().clone()
    }

    pub fn body_count(&self) -> usize {
        self.state.bodies.lock().unwrap().len()
    }

    /// The arrival time of every chat request, in arrival order.
    pub fn arrivals(&self) -> Vec<Instant> {
        self.state.arrivals.lock().unwrap().clone()
    }

    /// The most chat requests that were open at the same moment.
    pub fn max_inflight(&self) -> usize {
        self.state
            .max_inflight
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn last_body(&self) -> Option<serde_json::Value> {
        self.state.bodies.lock().unwrap().last().cloned()
    }

    pub async fn wait_bodies(&self, count: usize, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.body_count() >= count {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

async fn llm_models(State(state): State<Arc<LlmState>>) -> Json<serde_json::Value> {
    let data: Vec<serde_json::Value> = state
        .models
        .lock()
        .unwrap()
        .iter()
        .map(|id| serde_json::json!({ "id": id, "object": "model" }))
        .collect();
    Json(serde_json::json!({ "object": "list", "data": data }))
}

async fn llm_chat(State(state): State<Arc<LlmState>>, body: axum::body::Bytes) -> Response {
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    state.bodies.lock().unwrap().push(parsed);
    state.arrivals.lock().unwrap().push(Instant::now());
    let now_inflight = state
        .inflight
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    state
        .max_inflight
        .fetch_max(now_inflight, std::sync::atomic::Ordering::SeqCst);
    let guard = InflightGuard(state.clone());
    let reply = state
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| LlmReply::stream(&["mock answer"]));
    match reply {
        LlmReply::Stream(steps) => sse_response(steps, false, guard),
        LlmReply::CloseMidStream(steps) => sse_response(steps, true, guard),
        LlmReply::Http { status, body } => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
    }
}

fn sse_chunk(text: &str) -> axum::body::Bytes {
    let event = serde_json::json!({ "choices": [{ "index": 0, "delta": { "content": text } }] });
    axum::body::Bytes::from(format!("data: {event}\n\n"))
}

/// Counts one chat request as open until its response is finished or dropped.
struct InflightGuard(Arc<LlmState>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0
            .inflight
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn sse_response(steps: Vec<Step>, close_early: bool, guard: InflightGuard) -> Response {
    struct SseState {
        steps: Vec<Step>,
        index: usize,
        finished: bool,
        close_early: bool,
        _guard: InflightGuard,
    }
    let stream = futures_util::stream::unfold(
        SseState {
            steps,
            index: 0,
            finished: false,
            close_early,
            _guard: guard,
        },
        |mut state| async move {
            if state.finished {
                return None;
            }
            while let Some(Step::Sleep(duration)) = state.steps.get(state.index) {
                tokio::time::sleep(*duration).await;
                state.index += 1;
            }
            match state.steps.get(state.index).cloned() {
                Some(Step::Chunk(text)) => {
                    state.index += 1;
                    Some((Ok::<_, std::io::Error>(sse_chunk(&text)), state))
                }
                _ => {
                    state.finished = true;
                    if state.close_early {
                        None
                    } else {
                        Some((
                            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                                b"data: [DONE]\n\n",
                            )),
                            state,
                        ))
                    }
                }
            }
        },
    );
    axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .body(axum::body::Body::from_stream(stream))
        .expect("valid sse response")
        .into_response()
}

// ---------------------------------------------------------------- meeting harness

pub struct MeetingOpts {
    pub timings: EngineTimings,
    pub machine: MachineParams,
    pub compress_threshold_tokens: usize,
    pub notes_path: Option<String>,
    /// The assist profile active at engine start.
    pub start_profile: AssistProfile,
    /// Override the asr model name in the config (model-missing tests).
    pub asr_model: Option<String>,
    /// Override the llm port (closed-port tests pass a freed one).
    pub llm_port: Option<u16>,
}

impl Default for MeetingOpts {
    fn default() -> Self {
        Self {
            timings: fast_timings(),
            machine: MachineParams::default(),
            compress_threshold_tokens: 1_000,
            notes_path: None,
            start_profile: AssistProfile::Manual,
            asr_model: None,
            llm_port: None,
        }
    }
}

/// Runs a real `Engine` over scripted sources and both mocks, recording
/// every UI event with its arrival time.
pub struct MeetingHarness {
    pub events: Arc<Mutex<Vec<Timed>>>,
    pub commands: mpsc::UnboundedSender<EngineCommand>,
    pub engine: tokio::task::JoinHandle<()>,
    pub started: Instant,
}

impl MeetingHarness {
    pub async fn start(
        asr: &MockAsr,
        llm: &MockLlm,
        factory: ScriptedFactory,
        vad: Vec<VadScript>,
        opts: MeetingOpts,
    ) -> Self {
        let events: Arc<Mutex<Vec<Timed>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let started = Instant::now();
        let ui: StatusSink = Arc::new(move |event| {
            sink.lock().unwrap().push(Timed {
                at: Instant::now(),
                event,
            });
        });
        let config = Config {
            vad: VadConfig {
                start_threshold: opts.machine.start_threshold,
                end_threshold: opts.machine.end_threshold,
                end_silence_frames: opts.machine.end_silence_frames as usize,
                max_segment_ms: opts.machine.max_segment_ms,
            },
            llm: LlmConfig {
                base_url: format!("http://127.0.0.1:{}", opts.llm_port.unwrap_or(llm.port)),
                model: "mock-model".into(),
                notes_path: opts.notes_path.clone(),
                // what the app sends when LLM_ENABLE_THINKING is unset
                enable_thinking: Some(false),
                ..Default::default()
            },
            assist: AssistConfig {
                start_profile: opts.start_profile,
            },
            asr: AsrConfig {
                base_url: format!("http://127.0.0.1:{}", asr.port),
                model: opts
                    .asr_model
                    .clone()
                    .unwrap_or_else(|| "mock-model".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let deps = EngineDeps {
            factory: Arc::new(factory),
            ui,
            vad: vad_factory_scripts(vad),
            timings: opts.timings,
            compress_threshold_tokens: opts.compress_threshold_tokens,
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let engine = tokio::spawn(Engine::new(config, deps).run(rx));
        Self {
            events,
            commands: tx,
            engine,
            started,
        }
    }

    pub fn cmd(&self, command: EngineCommand) {
        self.commands.send(command).expect("engine loop alive");
    }

    pub fn snapshot(&self) -> Vec<UiEvent> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|timed| timed.event.clone())
            .collect()
    }

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

    pub fn states(&self) -> Vec<MeetingState> {
        self.snapshot()
            .into_iter()
            .filter_map(|event| match event {
                UiEvent::MeetingState(state) => Some(state),
                _ => None,
            })
            .collect()
    }

    pub async fn wait_state(&self, wanted: MeetingState, timeout: Duration) -> bool {
        self.wait_until(timeout, |events| {
            events.contains(&UiEvent::MeetingState(wanted))
        })
        .await
        .contains(&UiEvent::MeetingState(wanted))
    }

    pub fn statuses(&self) -> Vec<(StatusSource, StatusLevel, String)> {
        self.snapshot()
            .into_iter()
            .filter_map(|event| match event {
                UiEvent::Status {
                    source,
                    level,
                    text,
                } => Some((source, level, text)),
                _ => None,
            })
            .collect()
    }

    /// Index of the first event matching `pred`.
    pub fn index_where(&self, pred: impl Fn(&UiEvent) -> bool) -> Option<usize> {
        self.snapshot().iter().position(pred)
    }

    /// True when the first event matching `before` precedes the first
    /// matching `after` (both must exist).
    pub fn ordered(
        &self,
        before: impl Fn(&UiEvent) -> bool,
        after: impl Fn(&UiEvent) -> bool,
    ) -> bool {
        match (self.index_where(before), self.index_where(after)) {
            (Some(first), Some(second)) => first < second,
            _ => false,
        }
    }

    pub async fn shutdown(self) {
        self.cmd(EngineCommand::Shutdown);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.engine).await;
    }
}

/// A TCP port that was bound and released, so connecting to it is refused.
pub async fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

/// Build `(frames, probs)` runs into one concatenated source script:
/// consecutive `(frames, prob)` runs of a pattern.
pub fn concat_utterances(parts: &[(usize, Vec<f32>)]) -> (usize, Vec<f32>) {
    let mut frames = 0usize;
    let mut probs = Vec::new();
    for (count, run) in parts {
        frames += count;
        probs.extend(run.iter().copied());
    }
    (frames, probs)
}
