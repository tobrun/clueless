//! The meeting lifecycle: the single owner of meeting state. Commands
//! drive one loop that starts and stops the pipeline, runs suggestions,
//! checks server health at start and keeps the transcript store
//! compressed, exactly once per meeting.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use asr::client::AsrClient;
use clueless_types::audio::SourceError;
use clueless_types::config::Config;
use clueless_types::events::{
    EngineCommand, MeetingState, Speaker, StatusLevel, StatusSource, UiEvent,
};
use context::prompt;
use context::store::TranscriptStore;
use llm::client::LlmClient;
use llm::types::ChatRequest;
use segmenter::machine::MachineParams;

use crate::clock::MeetingClock;
use crate::compress;
use crate::deps::EngineDeps;
use crate::health;
use crate::pipeline::{self, Pipeline};
use crate::suggest;

/// Messages from the engine's own helper tasks back into the loop.
enum Internal {
    Drained,
    Panic(String),
}

/// Everything that lives and dies with one meeting.
struct Meeting {
    pipeline: Pipeline,
    store: Arc<Mutex<TranscriptStore>>,
    llm: Arc<LlmClient>,
    profile: Option<String>,
    cancel: CancellationToken,
    compress_cancel: CancellationToken,
    compress_task: JoinHandle<()>,
    drained_task: JoinHandle<()>,
    panic_task: Option<JoinHandle<()>>,
    suggestion: Option<(JoinHandle<()>, CancellationToken)>,
    last_trigger_line_id: u64,
}

/// The meeting engine. Construct it with the config and its seams, then
/// run the loop over a command receiver; the loop is the only place
/// meeting state changes.
pub struct Engine {
    config: Config,
    deps: EngineDeps,
}

impl Engine {
    pub fn new(config: Config, deps: EngineDeps) -> Self {
        Self { config, deps }
    }

    /// Own the meeting state until `Shutdown` or the command sender is
    /// dropped. Must run inside a tokio runtime.
    pub async fn run(self, mut commands: mpsc::UnboundedReceiver<EngineCommand>) {
        let (internal_tx, mut internal_rx) = mpsc::unbounded_channel();
        let mut meeting: Option<Meeting> = None;
        let mut state = MeetingState::Idle;
        let mut next_suggestion_id: u64 = 0;

        loop {
            enum Next {
                Command(EngineCommand),
                Internal(Internal),
                Closed,
            }
            let next = tokio::select! { biased;
                message = internal_rx.recv() => match message {
                    Some(message) => Next::Internal(message),
                    None => Next::Closed,
                },
                command = commands.recv() => match command {
                    Some(command) => Next::Command(command),
                    None => Next::Closed,
                },
            };
            match next {
                Next::Internal(Internal::Panic(message)) => {
                    tracing::error!(%message, "engine component panicked");
                    self.emit(pipeline::panic_status(&message));
                    if meeting.is_some() {
                        self.stop_meeting(&mut meeting).await;
                        state = MeetingState::Idle;
                    }
                }
                Next::Internal(Internal::Drained) => {
                    if state == MeetingState::Running && meeting.is_some() {
                        self.emit(UiEvent::SourcesDrained);
                    }
                }
                Next::Command(command) => match command {
                    EngineCommand::StartMeeting if state == MeetingState::Idle => {
                        state = self.start_meeting(&mut meeting, &internal_tx).await;
                    }
                    EngineCommand::StopMeeting if state == MeetingState::Running => {
                        self.stop_meeting(&mut meeting).await;
                        state = MeetingState::Idle;
                    }
                    EngineCommand::ToggleMeeting => match state {
                        MeetingState::Idle => {
                            state = self.start_meeting(&mut meeting, &internal_tx).await;
                        }
                        MeetingState::Running => {
                            self.stop_meeting(&mut meeting).await;
                            state = MeetingState::Idle;
                        }
                        MeetingState::Starting | MeetingState::Stopping => {}
                    },
                    EngineCommand::Suggest if state == MeetingState::Running => {
                        if let Some(current) = meeting.as_mut() {
                            next_suggestion_id += 1;
                            self.run_suggestion(current, next_suggestion_id).await;
                        }
                    }
                    EngineCommand::ClearSuggestion if state == MeetingState::Running => {
                        if let Some(current) = meeting.as_mut() {
                            Self::cancel_suggestion(current).await;
                            self.emit(UiEvent::ClearSuggestion);
                        }
                    }
                    EngineCommand::Shutdown => {
                        if meeting.is_some() {
                            // The stop sequence itself ends with an Idle event.
                            self.stop_meeting(&mut meeting).await;
                        } else {
                            self.emit(UiEvent::MeetingState(MeetingState::Idle));
                        }
                        return;
                    }
                    EngineCommand::StartMeeting
                    | EngineCommand::StopMeeting
                    | EngineCommand::Suggest
                    | EngineCommand::ClearSuggestion => {}
                },
                Next::Closed => break,
            }
        }
        // The command sender went away: shut down like `Shutdown` did.
        if meeting.is_some() {
            self.stop_meeting(&mut meeting).await;
        }
    }

    /// The full start sequence; returns the state reached (`Running`, or
    /// `Idle` when no source could be opened).
    async fn start_meeting(
        &self,
        meeting: &mut Option<Meeting>,
        internal_tx: &mpsc::UnboundedSender<Internal>,
    ) -> MeetingState {
        self.emit(UiEvent::MeetingState(MeetingState::Starting));
        let timings = self.deps.timings;
        let asr = Arc::new(AsrClient::new(
            format!(
                "http://{}:{}",
                self.config.server.host, self.config.server.asr_port
            ),
            &self.config.server.asr_model,
            timings.asr_timeout,
            timings.asr_backoff,
        ));
        let llm = Arc::new(LlmClient::new(
            format!(
                "http://{}:{}",
                self.config.server.host, self.config.server.llm_port
            ),
            &self.config.server.llm_model,
            timings.llm_connect,
            timings.llm_stall,
        ));
        for event in health::check(
            &asr,
            &self.config.server.asr_model,
            &llm,
            timings.health_timeout,
        )
        .await
        {
            self.emit(event);
        }
        let profile = self.read_profile();

        let mut sources = Vec::new();
        for speaker in self.deps.factory.speakers() {
            match self.deps.factory.open(speaker, self.deps.ui.clone()) {
                Ok(source) => sources.push((speaker, source)),
                Err(error) => self.emit(UiEvent::Status {
                    source: status_source(speaker),
                    level: StatusLevel::Error,
                    text: source_error_text(&error),
                }),
            }
        }
        if sources.is_empty() {
            self.emit(UiEvent::MeetingState(MeetingState::Idle));
            return MeetingState::Idle;
        }

        let store = Arc::new(Mutex::new(TranscriptStore::new()));
        let cancel = CancellationToken::new();
        let machine = MachineParams::from(&self.config.vad);
        let mut pipeline = Pipeline::start(
            sources,
            &self.deps,
            asr,
            &machine,
            store.clone(),
            MeetingClock::new(),
            cancel.clone(),
        );

        let panic_task = pipeline.panics().map(|mut panic_rx| {
            let tx = internal_tx.clone();
            tokio::spawn(async move {
                while let Some(message) = panic_rx.recv().await {
                    if tx.send(Internal::Panic(message)).is_err() {
                        break;
                    }
                }
            })
        });
        let drained_task = {
            let drain = pipeline.drain_handle();
            let tx = internal_tx.clone();
            tokio::spawn(async move {
                drain.settled_wait().await;
                let _ = tx.send(Internal::Drained);
            })
        };
        let compress_cancel = CancellationToken::new();
        let compress_task = tokio::spawn(compress::run(
            store.clone(),
            llm.clone(),
            self.deps.compress_threshold_tokens,
            timings.compress_retry,
            self.config.llm.temperature as f64,
            pipeline.commits(),
            self.deps.ui.clone(),
            compress_cancel.clone(),
        ));

        *meeting = Some(Meeting {
            pipeline,
            store,
            llm,
            profile,
            cancel,
            compress_cancel,
            compress_task,
            drained_task,
            panic_task,
            suggestion: None,
            last_trigger_line_id: 0,
        });
        self.emit(UiEvent::MeetingState(MeetingState::Running));
        MeetingState::Running
    }

    /// The full stop sequence: cancel the suggestion, the compression
    /// task and the meeting token, flush and join the pipeline, then
    /// report `Idle`.
    async fn stop_meeting(&self, meeting: &mut Option<Meeting>) {
        let Some(current) = meeting.as_mut() else {
            return;
        };
        self.emit(UiEvent::MeetingState(MeetingState::Stopping));
        Self::cancel_suggestion(current).await;
        current.compress_cancel.cancel();
        let _ = tokio::time::timeout(suggest::CANCEL_GRACE, &mut current.compress_task).await;
        current.pipeline.stop(self.deps.timings.stop_wait).await;
        current.cancel.cancel();
        // The drained notification is meaningless once we are stopping,
        // and a stream that never ends would keep this task alive.
        current.drained_task.abort();
        if let Some(task) = current.panic_task.as_mut() {
            task.abort();
        }
        *meeting = None;
        self.emit(UiEvent::MeetingState(MeetingState::Idle));
    }

    /// Cancel the running suggestion (its task reports `Cancelled`) and
    /// wait for that end event so later engine events follow it.
    async fn cancel_suggestion(meeting: &mut Meeting) {
        if let Some((mut task, cancel)) = meeting.suggestion.take() {
            suggest::cancel_and_wait(&mut task, &cancel).await;
        }
    }

    /// Build the prompt from the store plus in-progress text and stream
    /// one new suggestion; at most one suggestion runs at a time.
    async fn run_suggestion(&self, meeting: &mut Meeting, id: u64) {
        Self::cancel_suggestion(meeting).await;
        self.emit(UiEvent::SuggestionStart { id });
        let in_progress = meeting.pipeline.in_progress();
        let messages = {
            let store = meeting.store.lock().expect("store lock");
            prompt::build(
                &store,
                meeting.profile.as_deref(),
                &in_progress,
                meeting.last_trigger_line_id,
            )
        };
        meeting.last_trigger_line_id = meeting.store.lock().expect("store lock").last_line_id();
        let request = ChatRequest::new(
            meeting.llm.model(),
            suggest::to_llm_messages(messages),
            self.config.llm.max_tokens,
            self.config.llm.temperature as f64,
        );
        let cancel = CancellationToken::new();
        let task = tokio::spawn(suggest::run(
            id,
            meeting.llm.clone(),
            request,
            cancel.clone(),
            self.deps.ui.clone(),
        ));
        meeting.suggestion = Some((task, cancel));
    }

    /// The profile file is read once per meeting; a missing file is a
    /// Warn status and the meeting runs without it.
    fn read_profile(&self) -> Option<String> {
        let path = self.config.llm.profile_path.as_deref()?;
        match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) => {
                self.emit(UiEvent::Status {
                    source: StatusSource::App,
                    level: StatusLevel::Warn,
                    text: format!("profile file {path} could not be read: {error}"),
                });
                None
            }
        }
    }

    fn emit(&self, event: UiEvent) {
        (self.deps.ui)(event);
    }
}

fn status_source(speaker: Speaker) -> StatusSource {
    match speaker {
        Speaker::Me => StatusSource::Mic,
        Speaker::Them => StatusSource::SystemAudio,
    }
}

fn source_error_text(error: &SourceError) -> String {
    match error {
        SourceError::PermissionMissing(text)
        | SourceError::DeviceNotFound(text)
        | SourceError::Backend(text) => text.clone(),
    }
}

/// The command channel pair the app (and tests) hand to `Engine::run`.
pub fn command_channel() -> (
    tokio::sync::mpsc::UnboundedSender<EngineCommand>,
    tokio::sync::mpsc::UnboundedReceiver<EngineCommand>,
) {
    tokio::sync::mpsc::unbounded_channel()
}
