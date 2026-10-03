//! Injectable timings and the constructor seam the engine starts from.
//!
//! Production values are fixed by the spec; tests replace them with short
//! durations and scripted factories through `EngineDeps`.

use std::sync::Arc;
use std::time::Duration;

use clueless_types::audio::SourceFactory;
use clueless_types::events::StatusSink;
use segmenter::vad::SpeechProb;

/// Every wait and timeout inside the engine, in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineTimings {
    /// How long a Me final may wait for the Them watermark at most.
    pub echo_hold: Duration,
    /// How long a stop waits for queued finals before cancelling.
    pub stop_wait: Duration,
    /// Per-server timeout of the health check at meeting start.
    pub health_timeout: Duration,
    /// Per-attempt timeout of an ASR request.
    pub asr_timeout: Duration,
    /// Waits before the second and third final attempts.
    pub asr_backoff: [Duration; 2],
    /// LLM connect timeout.
    pub llm_connect: Duration,
    /// LLM stream stall timeout (no event for this long ends the suggestion).
    pub llm_stall: Duration,
    /// Minimum gap between compression attempts after a failure.
    pub compress_retry: Duration,
    /// The stream thread's sleep when a source returns `Empty`.
    pub idle_poll: Duration,
    /// How long the trigger speaker must stay quiet before an automatic
    /// request starts.
    pub turn_settle: Duration,
    /// How long a busy trigger speaker can hold an automatic request back.
    pub turn_max_wait: Duration,
    /// Minimum gap between the starts of two automatic requests (Brainstorm
    /// uses a multiple of it).
    pub auto_min_gap: Duration,
    /// How long automatic requests pause after a failed request.
    pub auto_failure_pause: Duration,
}

impl EngineTimings {
    /// The fixed production values from the spec.
    pub fn production() -> Self {
        Self {
            echo_hold: Duration::from_secs(3),
            stop_wait: Duration::from_secs(5),
            health_timeout: Duration::from_secs(2),
            asr_timeout: Duration::from_secs(10),
            asr_backoff: [Duration::from_millis(250), Duration::from_secs(1)],
            llm_connect: Duration::from_secs(2),
            llm_stall: Duration::from_secs(10),
            compress_retry: Duration::from_secs(60),
            idle_poll: Duration::from_millis(5),
            turn_settle: Duration::from_millis(400),
            turn_max_wait: Duration::from_secs(4),
            auto_min_gap: Duration::from_secs(2),
            auto_failure_pause: Duration::from_secs(30),
        }
    }
}

/// Everything the engine constructs per meeting, injected for tests.
pub struct EngineDeps {
    pub factory: Arc<dyn SourceFactory>,
    pub ui: StatusSink,
    /// One fresh detector per stream.
    pub vad: Arc<dyn Fn() -> Box<dyn SpeechProb> + Send + Sync>,
    pub timings: EngineTimings,
    pub compress_threshold_tokens: usize,
}

impl EngineDeps {
    /// Production deps over a source factory and the UI sink, with the
    /// bundled Silero model behind the vad seam.
    pub fn production(factory: Arc<dyn SourceFactory>, ui: StatusSink) -> Self {
        Self {
            factory,
            ui,
            vad: Arc::new(|| {
                Box::new(
                    segmenter::vad::SileroVad::new()
                        .expect("the bundled Silero model loads at construction"),
                )
            }),
            timings: EngineTimings::production(),
            compress_threshold_tokens: 90_000,
        }
    }
}
