//! The seam between the engine and one meeting's trace sink.
//!
//! [`Current`] is the slot the engine loop fills when a meeting opens its
//! trace and empties when the meeting ends; [`wrap_ui`] taps the shared UI
//! sink so every `UiEvent` is recorded while the slot holds a sink, and
//! [`session_start`] collects the manifest facts from the config and deps.

use std::sync::{Arc, Mutex};

use clueless_types::Speaker;
use clueless_types::config::Config;
use clueless_types::events::StatusSink;
use clueless_types::profile::AssistProfile;
use trace::manifest::{
    LlmSettings, SessionStart, SpeechSettings, Timings, VoiceDetector, redact_url,
};
use trace::record::{Body, Profile};
use trace::sink::TraceSink;

use crate::deps::EngineDeps;

/// The engine's current meeting sink: `Some` between trace open and close,
/// `None` between meetings. Cheap to clone; every clone sees the same slot.
#[derive(Clone, Default)]
pub struct Current(Arc<Mutex<Option<Arc<dyn TraceSink>>>>);

impl Current {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the sink for the meeting that is starting.
    pub fn set(&self, sink: Arc<dyn TraceSink>) {
        *self.0.lock().unwrap() = Some(sink);
    }

    /// Take the sink out (the meeting is ending); `None` when nothing
    /// is installed.
    pub fn take(&self) -> Option<Arc<dyn TraceSink>> {
        self.0.lock().unwrap().take()
    }

    /// A clone of the installed sink, or `None`.
    pub fn sink(&self) -> Option<Arc<dyn TraceSink>> {
        self.0.lock().unwrap().clone()
    }

    /// Record `body` when a meeting sink is installed, do nothing otherwise.
    pub fn record(&self, body: Body) {
        if let Some(sink) = self.sink() {
            sink.record(body);
        }
    }
}

/// Wrap a UI sink so every event is also recorded while [`Current`] holds a
/// sink; the event always reaches the original sink.
pub fn wrap_ui(ui: StatusSink, current: Current) -> StatusSink {
    Arc::new(move |event| {
        current.record(Body::from(&event));
        (ui)(event);
    })
}

/// The manifest facts for one meeting, from the config and the deps the
/// meeting runs with (server URLs redacted, keys never present).
pub fn session_start(
    config: &Config,
    deps: &EngineDeps,
    speakers: &[Speaker],
    profile: AssistProfile,
) -> SessionStart {
    SessionStart {
        speakers: speakers
            .iter()
            .map(|speaker| match speaker {
                Speaker::Me => "me".to_owned(),
                Speaker::Them => "them".to_owned(),
            })
            .collect(),
        profile: Profile::from(profile),
        llm: LlmSettings {
            base_url: redact_url(&config.llm.base_url),
            model: config.llm.model.clone(),
            max_tokens: config.llm.max_tokens,
            temperature: config.llm.temperature,
        },
        speech: SpeechSettings {
            base_url: redact_url(&config.asr.base_url),
            model: config.asr.model.clone(),
            language: config.asr.language.clone(),
        },
        voice_detector: VoiceDetector {
            start_threshold: config.vad.start_threshold,
            end_threshold: config.vad.end_threshold,
            end_silence_frames: config.vad.end_silence_frames,
            max_segment_ms: config.vad.max_segment_ms,
        },
        timings_ms: Timings {
            echo_hold_ms: deps.timings.echo_hold.as_millis() as u64,
            stop_wait_ms: deps.timings.stop_wait.as_millis() as u64,
            health_timeout_ms: deps.timings.health_timeout.as_millis() as u64,
            asr_timeout_ms: deps.timings.asr_timeout.as_millis() as u64,
            llm_connect_ms: deps.timings.llm_connect.as_millis() as u64,
            llm_stall_ms: deps.timings.llm_stall.as_millis() as u64,
        },
        compress_threshold_tokens: deps.compress_threshold_tokens as u64,
    }
}
