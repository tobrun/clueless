//! Shared fixtures for the trace tests, exposed only under the `testutil`
//! cargo feature (off by default, so production builds never see it).
//!
//! Trace's own integration tests get the feature through a self
//! dev-dependency in `Cargo.toml`; other crates enable it with
//! `trace = { workspace = true, features = ["testutil"] }` in their
//! `[dev-dependencies]`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::manifest::LlmSettings;
use crate::manifest::MANIFEST_FILE;
use crate::manifest::Manifest;
use crate::manifest::Origin;
use crate::manifest::SessionStart;
use crate::manifest::SpeechSettings;
use crate::manifest::Timings;
use crate::manifest::VoiceDetector;
use crate::reader::Trace;
use crate::record::Body;
use crate::record::Profile;
use crate::record::Record;
use crate::sink::FailureSink;

/// The fixture session settings every trace test starts a sink with.
pub fn session_start() -> SessionStart {
    SessionStart {
        speakers: vec!["me".into(), "them".into()],
        profile: Profile::Manual,
        llm: LlmSettings {
            base_url: "http://host:8000".into(),
            model: "m".into(),
            max_tokens: 220,
            temperature: 0.4,
        },
        speech: SpeechSettings {
            base_url: "http://host:9000".into(),
            model: "w".into(),
            language: None,
        },
        voice_detector: VoiceDetector {
            start_threshold: 0.5,
            end_threshold: 0.35,
            end_silence_frames: 19,
            max_segment_ms: 15_000,
        },
        timings_ms: Timings {
            echo_hold_ms: 700,
            stop_wait_ms: 1500,
            health_timeout_ms: 2000,
            asr_timeout_ms: 15_000,
            llm_connect_ms: 2000,
            llm_stall_ms: 10_000,
        },
        compress_threshold_tokens: 90_000,
    }
}

/// The fixture manifest at the given replay `speed`.
pub fn manifest_with_speed(speed: f64) -> Manifest {
    Manifest {
        schema: crate::manifest::SCHEMA,
        started_at_ms: 1_791_209_002_000,
        origin: Origin::Live,
        speed,
        app_version: "0.1.0".into(),
        git_commit: "test".into(),
        audio: false,
        session: session_start(),
    }
}

/// The fixture manifest with audio off and speed 1.0.
pub fn silent_manifest() -> Manifest {
    manifest_with_speed(1.0)
}

/// Write `manifest` as `manifest.json` into `dir`, created if needed.
pub fn write_manifest(dir: &Path, manifest: &Manifest) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_string(manifest).unwrap(),
    )
    .unwrap();
}

/// Scratch directory per test, unique per process run, removed on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "clueless-trace-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A failing-sink callback collecting its messages, with the collection.
pub fn failure_sink() -> (FailureSink, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let seen = Arc::clone(&seen);
        Arc::new(move |message: String| seen.lock().unwrap().push(message)) as FailureSink
    };
    (sink, seen)
}

/// One speaker frame: 512 samples at 16 kHz, 32 meeting milliseconds.
pub fn frame() -> Vec<f32> {
    vec![0.1; 512]
}

/// Every sample of a PCM16 WAV.
pub fn wav_samples(path: &Path) -> Vec<i16> {
    hound::WavReader::open(path)
        .unwrap()
        .samples::<i16>()
        .map(|s| s.unwrap())
        .collect()
}

/// Append one record to an in-memory trace; `seq` numbers itself from the
/// push order.
pub fn push(records: &mut Vec<Record>, at_ms: u64, body: Body) {
    let seq = records.len() as u64 + 1;
    records.push(Record { seq, at_ms, body });
}

/// An in-memory trace that never touches disk.
pub fn trace_named(name: &str, speed: f64, records: Vec<Record>) -> Trace {
    Trace {
        dir: PathBuf::from("/nonexistent").join(name),
        manifest: manifest_with_speed(speed),
        records,
        cut_off: false,
    }
}
