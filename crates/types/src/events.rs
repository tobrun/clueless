//! Transcript, suggestion and status events, and the commands the engine accepts.

/// Who produced an utterance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Speaker {
    Me,
    Them,
}

/// Identifies one utterance of one speaker within a meeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UtteranceId {
    pub speaker: Speaker,
    pub seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    Interim,
    Final,
}

/// 16 kHz mono f32 audio for one utterance or utterance-so-far.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub id: UtteranceId,
    pub kind: SegmentKind,
    /// meeting clock
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub pcm: Vec<f32>,
    /// true when a forced cut carried 1.0 s from the previous piece
    pub overlaps_prev: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub id: UtteranceId,
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeetingState {
    Idle,
    Starting,
    Running,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusSource {
    Mic,
    SystemAudio,
    Asr,
    Llm,
    App,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionEnd {
    Done,
    Cancelled,
    Interrupted,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    MeetingState(MeetingState),
    TranscriptInterim {
        id: UtteranceId,
        text: String,
    },
    TranscriptFinal(Utterance),
    TranscriptDropped {
        id: UtteranceId,
    },
    SuggestionStart {
        id: u64,
    },
    SuggestionDelta {
        id: u64,
        text: String,
    },
    SuggestionEnd {
        id: u64,
        end: SuggestionEnd,
    },
    ClearSuggestion,
    Status {
        source: StatusSource,
        level: StatusLevel,
        text: String,
    },
    /// Replay only: every source returned Ended and every queue and hold is empty.
    /// Emitted once per meeting.
    SourcesDrained,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCommand {
    StartMeeting,
    StopMeeting,
    ToggleMeeting,
    Suggest,
    ClearSuggestion,
    Shutdown,
}

/// Sends a UI event; callable from any thread.
pub type StatusSink = std::sync::Arc<dyn Fn(UiEvent) + Send + Sync>;
/// Sends a command to the engine; callable from any thread.
pub type CommandSink = std::sync::Arc<dyn Fn(EngineCommand) + Send + Sync>;
