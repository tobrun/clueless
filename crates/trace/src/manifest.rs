//! The manifest sidecar: what the meeting ran with, written once when the
//! trace opens so tooling can interpret the files without the app.

use crate::record::Profile;
use serde::{Deserialize, Serialize};

/// Trace format schema version. Readers refuse manifests above it.
pub const SCHEMA: u32 = 1;

/// Name of the manifest file inside a trace directory.
pub const MANIFEST_FILE: &str = "manifest.json";

/// What produced the audio behind a trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A live meeting with microphones and system audio.
    Live,
    /// A WAV replay driving the engine.
    ReplayWav,
    /// A re-run of a recorded session's audio against the LLM.
    Rerun,
}

/// The settings the engine supplied when it opened the trace. Server URLs
/// are stored redacted; api keys are never part of this struct.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStart {
    /// Speakers the meeting listed, e.g. `["me", "them"]`.
    pub speakers: Vec<String>,
    /// The profile active at start.
    pub profile: Profile,
    pub llm: LlmSettings,
    pub speech: SpeechSettings,
    pub voice_detector: VoiceDetector,
    /// Engine timings in whole milliseconds.
    pub timings_ms: Timings,
    /// Transcript tokens at which compression kicks in.
    pub compress_threshold_tokens: u64,
}

/// The chat server in effect (redacted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmSettings {
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

/// The speech server in effect (redacted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeechSettings {
    pub base_url: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// The voice activity detector settings in effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceDetector {
    pub start_threshold: f32,
    pub end_threshold: f32,
    pub end_silence_frames: usize,
    pub max_segment_ms: u64,
}

/// Engine timings in whole milliseconds, the ones that shape a meeting's
/// behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timings {
    pub echo_hold_ms: u64,
    pub stop_wait_ms: u64,
    pub health_timeout_ms: u64,
    pub asr_timeout_ms: u64,
    pub llm_connect_ms: u64,
    pub llm_stall_ms: u64,
}

/// Everything the manifest records about a trace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Format version, [`SCHEMA`] for traces this build writes.
    pub schema: u32,
    /// Wall-clock Unix milliseconds when the meeting started.
    pub started_at_ms: u64,
    pub origin: Origin,
    /// Playback speed the trace was produced at (1 for live meetings).
    pub speed: f64,
    /// App version, e.g. the crate version.
    pub app_version: String,
    /// Short git commit, `unknown` when it could not be determined.
    pub git_commit: String,
    /// Whether companion audio WAVs were written.
    pub audio: bool,
    /// The settings the engine supplied.
    pub session: SessionStart,
}

/// Strip credentials and query strings from a URL for storage.
///
/// `http://bob:secret@host:8000` becomes `http://***@host:8000`; a query
/// string (where keys hide in plain sight) becomes `?***`.
pub fn redact_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return strip_query(url);
    };
    let (scheme, rest) = url.split_at(scheme_end + 3);
    // The authority runs to the first path or query separator.
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);
    let authority = match authority.rfind('@') {
        Some(at) => format!("***@{}", &authority[at + 1..]),
        None => authority.to_string(),
    };
    strip_query(&format!("{scheme}{authority}{tail}"))
}

/// Replace a query string or fragment with `***`.
fn strip_query(url: &str) -> String {
    match url.find(['?', '#']) {
        Some(i) => {
            let marker = if url.as_bytes()[i] == b'#' { "#" } else { "?" };
            format!("{}{}***", &url[..i], marker)
        }
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::session_start;

    #[test]
    fn redact_url_replaces_userinfo_with_stars() {
        assert_eq!(
            redact_url("http://bob:secret@host:8000"),
            "http://***@host:8000"
        );
    }

    #[test]
    fn redact_url_replaces_the_query_string_with_stars() {
        assert_eq!(
            redact_url("http://host:8000/v1?api_key=abc"),
            "http://host:8000/v1?***"
        );
    }

    #[test]
    fn redact_url_leaves_a_plain_url_unchanged() {
        assert_eq!(redact_url("http://host:8000"), "http://host:8000");
    }

    #[test]
    fn manifest_round_trips_and_locks_the_schema() {
        // The shared fixture with an Interview profile and a redacted
        // base url, so round-tripping exercises those spellings too.
        let mut session = session_start();
        session.profile = Profile::Interview;
        session.llm.base_url = "http://***@host:8000".into();
        session.llm.model = "gpt-test".into();
        session.speech.model = "whisper-1".into();
        let manifest = Manifest {
            schema: SCHEMA,
            started_at_ms: 1791209002000,
            origin: Origin::ReplayWav,
            speed: 10.0,
            app_version: "0.1.0".into(),
            git_commit: "abc1234".into(),
            audio: true,
            session,
        };
        let text = serde_json::to_string(&manifest).unwrap();
        let back: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back, manifest);
        assert_eq!(back.origin, Origin::ReplayWav);
        assert!(text.contains("\"replay_wav\""));
        assert!(text.contains("\"schema\":1"));
    }
}
