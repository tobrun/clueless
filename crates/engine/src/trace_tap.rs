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
///
/// Statuses additionally go to the log at their level, whatever the trace
/// slot holds: a source that fails to open must leave a line in
/// `~/Library/Logs/clueless/clueless.log` even outside a meeting trace.
pub fn wrap_ui(ui: StatusSink, current: Current) -> StatusSink {
    Arc::new(move |event| {
        if let clueless_types::events::UiEvent::Status {
            source,
            level,
            text,
        } = &event
        {
            match level {
                clueless_types::events::StatusLevel::Error => {
                    tracing::error!(source = ?source, %text, "status")
                }
                clueless_types::events::StatusLevel::Warn => {
                    tracing::warn!(source = ?source, %text, "status")
                }
                clueless_types::events::StatusLevel::Info => {
                    tracing::info!(source = ?source, %text, "status")
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use clueless_types::events::{StatusLevel, StatusSource, UiEvent};
    use std::sync::Mutex;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};

    /// A subscriber that stores `(level, source, text)` for every event.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<(String, String, String)>>>);

    struct VisitStatus {
        level: String,
        source: String,
        text: String,
    }

    impl Visit for VisitStatus {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let rendered = format!("{value:?}");
            match field.name() {
                "source" => self.source = rendered,
                "text" => self.text = rendered,
                _ => {}
            }
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "text" {
                self.text = value.to_owned();
            }
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _meta: &tracing::Metadata) -> bool {
            true
        }
        fn event(&self, event: &Event) {
            let mut v = VisitStatus {
                level: event.metadata().level().as_str().to_owned(),
                source: String::new(),
                text: String::new(),
            };
            event.record(&mut v);
            self.0.lock().unwrap().push((v.level, v.source, v.text));
        }
        fn exit(&self, _span: &tracing::Id) {}
        fn enter(&self, _span: &tracing::Id) {}
        fn new_span(&self, _attrs: &tracing::span::Attributes) -> tracing::Id {
            tracing::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record) {}
        fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}
    }

    fn captured(sub: &Capture) -> Vec<(String, String, String)> {
        sub.0.lock().unwrap().clone()
    }

    #[test]
    fn statuses_reach_the_log_at_their_level_with_source_and_text() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.clone());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let ui_seen = seen.clone();
        let ui: StatusSink = Arc::new(move |e| ui_seen.lock().unwrap().push(e));
        let tap = wrap_ui(ui, Current::new());

        tap(UiEvent::Status {
            source: StatusSource::SystemAudio,
            level: StatusLevel::Error,
            text: "Grant Screen Recording in System Settings, then restart clueless".into(),
        });
        tap(UiEvent::Status {
            source: StatusSource::Mic,
            level: StatusLevel::Warn,
            text: "Microphone delivers silence".into(),
        });
        tap(UiEvent::Status {
            source: StatusSource::Asr,
            level: StatusLevel::Info,
            text: "ASR server reachable".into(),
        });
        tap(UiEvent::ClearSuggestion);

        let lines = captured(&capture);
        assert_eq!(
            lines,
            vec![
                (
                    "ERROR".into(),
                    "SystemAudio".into(),
                    "Grant Screen Recording in System Settings, then restart clueless".into(),
                ),
                (
                    "WARN".into(),
                    "Mic".into(),
                    "Microphone delivers silence".into()
                ),
                ("INFO".into(), "Asr".into(), "ASR server reachable".into()),
            ],
        );
        // The event still reaches the original sink unchanged.
        assert_eq!(seen.lock().unwrap().len(), 4);
    }
}
