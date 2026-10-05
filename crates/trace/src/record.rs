//! One JSON line of `events.jsonl`.
//!
//! These are the format's own types, not serde derives on the engine's:
//! renaming an engine enum must not silently change what an old trace means.
//! Every `kind` value of the Records table is a variant here, tagged by
//! `kind` in snake_case; a kind a reader does not know parses as
//! [`Body::Unknown`] and unknown fields are ignored.

use serde::{Deserialize, Serialize};

/// One record line: a sequence number, milliseconds since the session
/// opened, and the body flattened beside them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub seq: u64,
    pub at_ms: u64,
    #[serde(flatten)]
    pub body: Body,
}

/// Token counts from the server, each present only when the server sent it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

/// The body of one record: every kind of the Records table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// One engine command received while a session is open, also ignored ones.
    Command {
        command: CommandName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<Profile>,
    },
    /// The meeting notes given to `start_meeting`.
    Notes {
        text: String,
    },
    /// The moment the meeting clock began.
    ClockStarted,
    MeetingState {
        state: MeetingState,
    },
    Profile {
        profile: Profile,
    },
    Status {
        source: StatusSource,
        level: StatusLevel,
        text: String,
    },
    TranscriptInterim {
        speaker: Speaker,
        utterance: u64,
        text: String,
    },
    TranscriptFinal {
        speaker: Speaker,
        utterance: u64,
        t0_ms: u64,
        t1_ms: u64,
        text: String,
    },
    TranscriptDropped {
        speaker: Speaker,
        utterance: u64,
    },
    SuggestionStart {
        suggestion: u64,
    },
    SuggestionDelta {
        suggestion: u64,
        text: String,
    },
    SuggestionEnd {
        suggestion: u64,
        end: SuggestionOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    ClearSuggestion,
    SourcesDrained,
    /// One segment handed from a stream thread to the workers.
    Segment {
        speaker: Speaker,
        utterance: u64,
        segment_kind: SegmentKind,
        t0_ms: u64,
        t1_ms: u64,
        samples: u64,
        overlaps_prev: bool,
    },
    /// One speech-server call with its timing and outcome.
    AsrCall {
        speaker: Speaker,
        utterance: u64,
        segment_kind: SegmentKind,
        started_at_ms: u64,
        duration_ms: u64,
        outcome: AsrOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The echo comparison of one Me final against the Them side.
    EchoCheck {
        utterance: u64,
        held_ms: u64,
        echo: bool,
    },
    /// Why an utterance was dropped, at the reason level the UI event lacks.
    UtteranceDropped {
        speaker: Speaker,
        utterance: u64,
        reason: DropReason,
    },
    /// One finished answer piece; `chars` absent when the piece was dropped.
    PieceDone {
        speaker: Speaker,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chars: Option<usize>,
    },
    /// One trigger policy outcome.
    Policy {
        outcome: PolicyOutcome,
        profile: Profile,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suggestion: Option<u64>,
    },
    /// A chat request as it was sent, with the full JSON body.
    LlmRequest {
        call: u64,
        purpose: Purpose,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suggestion: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<SuggestionOrigin>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<Profile>,
        body: serde_json::Value,
    },
    /// One streamed text piece, content or reasoning.
    LlmDelta {
        call: u64,
        channel: Channel,
        text: String,
    },
    /// The end of one chat call with every fact the client learned.
    LlmEnd {
        call: u64,
        outcome: LlmOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default)]
        raw_text: String,
        #[serde(default)]
        shown_text: String,
        passed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_content_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_reasoning_ms: Option<u64>,
    },
    /// A compression summary replaced this many transcript lines.
    SummaryApplied {
        call: u64,
        replaced: usize,
    },
    /// A place where the audio file's sample index stops following the
    /// meeting timeline, and where it resumes.
    AudioAnchor {
        speaker: Speaker,
        t_ms: u64,
        sample_index: u64,
    },
    /// Messages the full queues dropped before the writer caught up.
    RecordsLost {
        records: u64,
        audio_frames: u64,
    },
    /// The session's last record; a trace without one is cut off.
    End {
        reason: EndReason,
    },
    /// A kind this build does not know.
    #[serde(other)]
    Unknown,
}

// The wire enums: the format's own spellings, converted from the engine's
// types by hand so a refactor cannot move the format underneath old traces.

/// Which side of the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    Me,
    Them,
}

/// The assist profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Manual,
    Interview,
    Brainstorm,
}

/// Who started a suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionOrigin {
    Manual,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingState {
    Idle,
    Starting,
    Running,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusSource {
    Mic,
    SystemAudio,
    Asr,
    Llm,
    App,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    Interim,
    Final,
}

/// How a `SuggestionEnd` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionOutcome {
    Done,
    Cancelled,
    Interrupted,
    Failed,
}

/// Why an utterance was dropped; the six reasons behind one UI event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    Cancelled,
    NoSpeech,
    AsrError,
    EmptyAfterOverlap,
    Echo,
    QueueFull,
}

/// How a meeting ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Stop,
    Shutdown,
    Panic,
    ChannelClosed,
    StartFailed,
}

/// The outcome of one speech-server call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsrOutcome {
    Text,
    NoSpeech,
    Error,
    Cancelled,
}

/// The outcome of one chat call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmOutcome {
    Done,
    Cancelled,
    Error,
}

/// What a chat call was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Suggestion,
    Compress,
}

/// Which stream a text piece arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Content,
    Reasoning,
}

/// One engine command, named the way the format names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandName {
    StartMeeting,
    StopMeeting,
    ToggleMeeting,
    Suggest,
    ClearSuggestion,
    CycleProfile,
    SetProfile,
    Shutdown,
}

/// One trigger policy outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    Waiting,
    Fired,
    Paused,
}

impl From<clueless_types::Speaker> for Speaker {
    fn from(s: clueless_types::Speaker) -> Self {
        match s {
            clueless_types::Speaker::Me => Self::Me,
            clueless_types::Speaker::Them => Self::Them,
        }
    }
}

impl From<clueless_types::AssistProfile> for Profile {
    fn from(p: clueless_types::AssistProfile) -> Self {
        match p {
            clueless_types::AssistProfile::Manual => Self::Manual,
            clueless_types::AssistProfile::Interview => Self::Interview,
            clueless_types::AssistProfile::Brainstorm => Self::Brainstorm,
        }
    }
}

impl From<clueless_types::Origin> for SuggestionOrigin {
    fn from(o: clueless_types::Origin) -> Self {
        match o {
            clueless_types::Origin::Manual => Self::Manual,
            clueless_types::Origin::Auto => Self::Auto,
        }
    }
}

impl From<clueless_types::MeetingState> for MeetingState {
    fn from(s: clueless_types::MeetingState) -> Self {
        match s {
            clueless_types::MeetingState::Idle => Self::Idle,
            clueless_types::MeetingState::Starting => Self::Starting,
            clueless_types::MeetingState::Running => Self::Running,
            clueless_types::MeetingState::Stopping => Self::Stopping,
        }
    }
}

impl From<clueless_types::StatusSource> for StatusSource {
    fn from(s: clueless_types::StatusSource) -> Self {
        match s {
            clueless_types::StatusSource::Mic => Self::Mic,
            clueless_types::StatusSource::SystemAudio => Self::SystemAudio,
            clueless_types::StatusSource::Asr => Self::Asr,
            clueless_types::StatusSource::Llm => Self::Llm,
            clueless_types::StatusSource::App => Self::App,
        }
    }
}

impl From<clueless_types::StatusLevel> for StatusLevel {
    fn from(l: clueless_types::StatusLevel) -> Self {
        match l {
            clueless_types::StatusLevel::Info => Self::Info,
            clueless_types::StatusLevel::Warn => Self::Warn,
            clueless_types::StatusLevel::Error => Self::Error,
        }
    }
}

impl From<clueless_types::SegmentKind> for SegmentKind {
    fn from(k: clueless_types::SegmentKind) -> Self {
        match k {
            clueless_types::SegmentKind::Interim => Self::Interim,
            clueless_types::SegmentKind::Final => Self::Final,
        }
    }
}

impl Body {
    /// The `command` record for one engine command; `SetProfile` carries the
    /// profile key.
    pub fn command(command: &clueless_types::EngineCommand) -> Body {
        use clueless_types::EngineCommand as C;
        let (name, profile) = match command {
            C::StartMeeting => (CommandName::StartMeeting, None),
            C::StopMeeting => (CommandName::StopMeeting, None),
            C::ToggleMeeting => (CommandName::ToggleMeeting, None),
            C::Suggest => (CommandName::Suggest, None),
            C::ClearSuggestion => (CommandName::ClearSuggestion, None),
            C::CycleProfile => (CommandName::CycleProfile, None),
            C::SetProfile(p) => (CommandName::SetProfile, Some(Profile::from(*p))),
            C::Shutdown => (CommandName::Shutdown, None),
        };
        Body::Command {
            command: name,
            profile,
        }
    }
}

impl From<&clueless_types::UiEvent> for Body {
    fn from(event: &clueless_types::UiEvent) -> Body {
        use clueless_types::UiEvent as E;
        match event {
            E::MeetingState(s) => Body::MeetingState {
                state: MeetingState::from(*s),
            },
            E::TranscriptInterim { id, text } => Body::TranscriptInterim {
                speaker: Speaker::from(id.speaker),
                utterance: id.seq,
                text: text.clone(),
            },
            E::TranscriptFinal(u) => Body::TranscriptFinal {
                speaker: Speaker::from(u.id.speaker),
                utterance: u.id.seq,
                t0_ms: u.t0_ms,
                t1_ms: u.t1_ms,
                text: u.text.clone(),
            },
            E::TranscriptDropped { id } => Body::TranscriptDropped {
                speaker: Speaker::from(id.speaker),
                utterance: id.seq,
            },
            E::SuggestionStart { id } => Body::SuggestionStart { suggestion: *id },
            E::SuggestionDelta { id, text } => Body::SuggestionDelta {
                suggestion: *id,
                text: text.clone(),
            },
            E::SuggestionEnd { id, end } => {
                let (outcome, message) = match end {
                    clueless_types::SuggestionEnd::Done => (SuggestionOutcome::Done, None),
                    clueless_types::SuggestionEnd::Cancelled => {
                        (SuggestionOutcome::Cancelled, None)
                    }
                    clueless_types::SuggestionEnd::Interrupted => {
                        (SuggestionOutcome::Interrupted, None)
                    }
                    clueless_types::SuggestionEnd::Failed(m) => {
                        (SuggestionOutcome::Failed, Some(m.clone()))
                    }
                };
                Body::SuggestionEnd {
                    suggestion: *id,
                    end: outcome,
                    message,
                }
            }
            E::ClearSuggestion => Body::ClearSuggestion,
            E::Profile(p) => Body::Profile {
                profile: Profile::from(*p),
            },
            E::Status {
                source,
                level,
                text,
            } => Body::Status {
                source: StatusSource::from(*source),
                level: StatusLevel::from(*level),
                text: text.clone(),
            },
            E::SourcesDrained => Body::SourcesDrained,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clueless_types::{
        AssistProfile, EngineCommand, MeetingState as EngineMeetingState, Origin,
        Speaker as EngineSpeaker, StatusLevel as EngineStatusLevel,
        StatusSource as EngineStatusSource, SuggestionEnd as EngineSuggestionEnd, UiEvent,
        Utterance, UtteranceId,
    };

    fn id(speaker: EngineSpeaker, seq: u64) -> UtteranceId {
        UtteranceId { speaker, seq }
    }

    /// Serialize a body on its own, the way one line of the file reads.
    fn json(body: &Body) -> serde_json::Value {
        let record = Record {
            seq: 12,
            at_ms: 3481,
            body: body.clone(),
        };
        let line = serde_json::to_string(&record).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// One UI event with its expected `kind` and expected fields.
    type Case = (
        UiEvent,
        &'static str,
        Vec<(&'static str, serde_json::Value)>,
    );

    #[test]
    fn every_ui_event_variant_converts_to_its_record_kind_and_fields() {
        let cases: Vec<Case> = vec![
            (
                UiEvent::MeetingState(EngineMeetingState::Running),
                "meeting_state",
                vec![("state", "running".into())],
            ),
            (
                UiEvent::TranscriptInterim {
                    id: id(EngineSpeaker::Them, 7),
                    text: "hel".into(),
                },
                "transcript_interim",
                vec![
                    ("speaker", "them".into()),
                    ("utterance", 7.into()),
                    ("text", "hel".into()),
                ],
            ),
            (
                UiEvent::TranscriptFinal(Utterance {
                    id: id(EngineSpeaker::Me, 3),
                    t0_ms: 1000,
                    t1_ms: 2500,
                    text: "hello".into(),
                }),
                "transcript_final",
                vec![
                    ("speaker", "me".into()),
                    ("utterance", 3.into()),
                    ("t0_ms", 1000.into()),
                    ("t1_ms", 2500.into()),
                    ("text", "hello".into()),
                ],
            ),
            (
                UiEvent::TranscriptDropped {
                    id: id(EngineSpeaker::Me, 4),
                },
                "transcript_dropped",
                vec![("speaker", "me".into()), ("utterance", 4.into())],
            ),
            (
                UiEvent::SuggestionStart { id: 9 },
                "suggestion_start",
                vec![("suggestion", 9.into())],
            ),
            (
                UiEvent::SuggestionDelta {
                    id: 9,
                    text: "hi".into(),
                },
                "suggestion_delta",
                vec![("suggestion", 9.into()), ("text", "hi".into())],
            ),
            (
                UiEvent::SuggestionEnd {
                    id: 9,
                    end: EngineSuggestionEnd::Done,
                },
                "suggestion_end",
                vec![("suggestion", 9.into()), ("end", "done".into())],
            ),
            (
                UiEvent::SuggestionEnd {
                    id: 9,
                    end: EngineSuggestionEnd::Failed("boom".into()),
                },
                "suggestion_end",
                vec![
                    ("suggestion", 9.into()),
                    ("end", "failed".into()),
                    ("message", "boom".into()),
                ],
            ),
            (UiEvent::ClearSuggestion, "clear_suggestion", vec![]),
            (
                UiEvent::Profile(AssistProfile::Brainstorm),
                "profile",
                vec![("profile", "brainstorm".into())],
            ),
            (
                UiEvent::Status {
                    source: EngineStatusSource::SystemAudio,
                    level: EngineStatusLevel::Warn,
                    text: "quiet".into(),
                },
                "status",
                vec![
                    ("source", "system_audio".into()),
                    ("level", "warn".into()),
                    ("text", "quiet".into()),
                ],
            ),
            (UiEvent::SourcesDrained, "sources_drained", vec![]),
        ];
        for (event, kind, fields) in cases {
            let body = Body::from(&event);
            let value = json(&body);
            assert_eq!(value["kind"], kind, "{event:?}");
            assert_eq!(value["seq"], 12, "{event:?}");
            assert_eq!(value["at_ms"], 3481, "{event:?}");
            for (field, expected) in fields {
                assert_eq!(value[field], expected, "{event:?} field {field}");
            }
        }
    }

    #[test]
    fn every_engine_command_becomes_a_command_record_naming_it() {
        let cases = [
            (EngineCommand::StartMeeting, "start_meeting"),
            (EngineCommand::StopMeeting, "stop_meeting"),
            (EngineCommand::ToggleMeeting, "toggle_meeting"),
            (EngineCommand::Suggest, "suggest"),
            (EngineCommand::ClearSuggestion, "clear_suggestion"),
            (EngineCommand::CycleProfile, "cycle_profile"),
            (
                EngineCommand::SetProfile(AssistProfile::Brainstorm),
                "set_profile",
            ),
            (EngineCommand::Shutdown, "shutdown"),
        ];
        for (command, name) in cases {
            let value = json(&Body::command(&command));
            assert_eq!(value["kind"], "command", "{command:?}");
            assert_eq!(value["command"], name, "{command:?}");
        }
        let value = json(&Body::command(&EngineCommand::SetProfile(
            AssistProfile::Interview,
        )));
        assert_eq!(value["profile"], "interview");
        let value = json(&Body::command(&EngineCommand::Suggest));
        assert!(
            value.get("profile").is_none(),
            "only SetProfile carries one"
        );
    }

    #[test]
    fn a_record_round_trips_through_serde_json_to_the_equal_value() {
        let record = Record {
            seq: 12,
            at_ms: 3481,
            body: Body::LlmEnd {
                call: 3,
                outcome: LlmOutcome::Done,
                error: None,
                finish_reason: Some("stop".into()),
                usage: Some(Usage {
                    prompt_tokens: Some(100),
                    completion_tokens: Some(5),
                    total_tokens: None,
                }),
                raw_text: "Say hello".into(),
                shown_text: "Say hello".into(),
                passed: false,
                first_content_ms: Some(410),
                first_reasoning_ms: None,
            },
        };
        let line = serde_json::to_string(&record).unwrap();
        let back: Record = serde_json::from_str(&line).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn a_kind_from_the_future_parses_as_unknown() {
        let line = r#"{"seq":1,"at_ms":0,"kind":"from_the_future","whatever":true}"#;
        let record: Record = serde_json::from_str(line).unwrap();
        assert_eq!(record.body, Body::Unknown);
        assert_eq!(record.seq, 1);
    }

    #[test]
    fn a_record_with_an_extra_unknown_field_parses_and_ignores_it() {
        let line = r#"{"seq":1,"at_ms":0,"kind":"sources_drained","surprise":42}"#;
        let record: Record = serde_json::from_str(line).unwrap();
        assert_eq!(record.body, Body::SourcesDrained);
    }

    #[test]
    fn every_kind_of_the_records_table_round_trips() {
        let bodies = vec![
            Body::Command {
                command: CommandName::SetProfile,
                profile: Some(Profile::Manual),
            },
            Body::Notes {
                text: "notes".into(),
            },
            Body::ClockStarted,
            Body::MeetingState {
                state: MeetingState::Starting,
            },
            Body::Profile {
                profile: Profile::Manual,
            },
            Body::Status {
                source: StatusSource::App,
                level: StatusLevel::Info,
                text: "t".into(),
            },
            Body::TranscriptInterim {
                speaker: Speaker::Me,
                utterance: 1,
                text: "t".into(),
            },
            Body::TranscriptFinal {
                speaker: Speaker::Them,
                utterance: 2,
                t0_ms: 1,
                t1_ms: 2,
                text: "t".into(),
            },
            Body::TranscriptDropped {
                speaker: Speaker::Me,
                utterance: 3,
            },
            Body::SuggestionStart { suggestion: 4 },
            Body::SuggestionDelta {
                suggestion: 4,
                text: "d".into(),
            },
            Body::SuggestionEnd {
                suggestion: 4,
                end: SuggestionOutcome::Failed,
                message: Some("m".into()),
            },
            Body::ClearSuggestion,
            Body::SourcesDrained,
            Body::Segment {
                speaker: Speaker::Me,
                utterance: 1,
                segment_kind: SegmentKind::Final,
                t0_ms: 10,
                t1_ms: 20,
                samples: 160,
                overlaps_prev: true,
            },
            Body::AsrCall {
                speaker: Speaker::Me,
                utterance: 1,
                segment_kind: SegmentKind::Final,
                started_at_ms: 10,
                duration_ms: 20,
                outcome: AsrOutcome::Text,
                raw_text: Some("hello".into()),
                error: None,
            },
            Body::EchoCheck {
                utterance: 1,
                held_ms: 700,
                echo: true,
            },
            Body::UtteranceDropped {
                speaker: Speaker::Them,
                utterance: 2,
                reason: DropReason::QueueFull,
            },
            Body::PieceDone {
                speaker: Speaker::Me,
                chars: Some(12),
            },
            Body::PieceDone {
                speaker: Speaker::Me,
                chars: None,
            },
            Body::Policy {
                outcome: PolicyOutcome::Fired,
                profile: Profile::Interview,
                suggestion: Some(7),
            },
            Body::LlmRequest {
                call: 1,
                purpose: Purpose::Suggestion,
                suggestion: Some(7),
                origin: Some(SuggestionOrigin::Auto),
                profile: Some(Profile::Interview),
                body: serde_json::json!({"model": "m"}),
            },
            Body::LlmDelta {
                call: 1,
                channel: Channel::Reasoning,
                text: "think".into(),
            },
            Body::LlmEnd {
                call: 1,
                outcome: LlmOutcome::Error,
                error: Some("500 boom".into()),
                finish_reason: None,
                usage: None,
                raw_text: String::new(),
                shown_text: String::new(),
                passed: false,
                first_content_ms: None,
                first_reasoning_ms: Some(3),
            },
            Body::SummaryApplied {
                call: 2,
                replaced: 5,
            },
            Body::AudioAnchor {
                speaker: Speaker::Them,
                t_ms: 1000,
                sample_index: 16000,
            },
            Body::RecordsLost {
                records: 7,
                audio_frames: 3,
            },
            Body::End {
                reason: EndReason::Stop,
            },
        ];
        for body in bodies {
            let record = Record {
                seq: 1,
                at_ms: 0,
                body: body.clone(),
            };
            let line = serde_json::to_string(&record).unwrap();
            let back: Record = serde_json::from_str(&line).unwrap();
            assert_eq!(back, record, "round trip of {body:?}");
        }
    }

    #[test]
    fn the_wire_enums_spell_the_format_names_in_snake_case() {
        // The exact strings the Records table and docs promise.
        assert_eq!(serde_json::to_string(&Speaker::Them).unwrap(), "\"them\"");
        assert_eq!(
            serde_json::to_string(&StatusSource::SystemAudio).unwrap(),
            "\"system_audio\""
        );
        assert_eq!(
            serde_json::to_string(&DropReason::EmptyAfterOverlap).unwrap(),
            "\"empty_after_overlap\""
        );
        assert_eq!(
            serde_json::to_string(&EndReason::ChannelClosed).unwrap(),
            "\"channel_closed\""
        );
        assert_eq!(
            serde_json::to_string(&Purpose::Compress).unwrap(),
            "\"compress\""
        );
        assert_eq!(
            serde_json::to_string(&SuggestionOrigin::from(Origin::Auto)).unwrap(),
            "\"auto\""
        );
    }
}
