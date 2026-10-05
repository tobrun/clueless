//! The meeting lifecycle: the single owner of meeting state. Commands
//! drive one loop that starts and stops the pipeline, runs suggestions,
//! checks server health at start and keeps the transcript store
//! compressed, exactly once per meeting.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use asr::client::AsrClient;
use clueless_types::audio::SourceError;
use clueless_types::config::Config;
use clueless_types::events::{
    EngineCommand, MeetingState, Speaker, StatusLevel, StatusSource, SuggestionEnd, UiEvent,
};
use clueless_types::profile::{AssistProfile, Origin};
use context::assist::{self, AutoPolicy, Decision, Outcome, PolicyTimings};
use context::prompt::{self, Ask};
use context::store::TranscriptStore;
use llm::client::LlmClient;
use llm::types::ChatRequest;
use segmenter::machine::MachineParams;
use trace::manifest::SessionStart;
use trace::record::{Body, EndReason, PolicyOutcome};
use trace::sink::{FailureSink, NoTrace, TraceSink};

use crate::asr_worker::PieceDone;
use crate::clock::MeetingClock;
use crate::compress;
use crate::deps::EngineDeps;
use crate::health;
use crate::pipeline::{self, Pipeline};
use crate::suggest;
use crate::trace_tap::{self, Current};

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
    notes: Option<String>,
    cancel: CancellationToken,
    compress_cancel: CancellationToken,
    compress_task: JoinHandle<()>,
    drained_task: JoinHandle<()>,
    panic_task: Option<JoinHandle<()>>,
    suggestion: Option<RunningSuggestion>,
    last_trigger_line_id: u64,
    /// Finished speech pieces reported by the ASR workers.
    pieces: mpsc::UnboundedReceiver<PieceDone>,
    /// When automatic requests start.
    policy: AutoPolicy,
    /// The last answer that ended `Done` with shown text.
    previous_answer: Option<String>,
    /// Every source ended and every queue is empty.
    drained: bool,
    drained_emitted: bool,
    /// The last suggestion request failed; the next good answer restores the
    /// LLM status.
    llm_failed: bool,
    /// When the engine loop must look at the policy again.
    wake_at: Option<tokio::time::Instant>,
    /// Whether the policy was last seen waiting (logging only).
    was_waiting: bool,
    /// This meeting's trace sink (records nothing when tracing is off).
    sink: Arc<dyn TraceSink>,
    /// The profile in effect, for `policy` records.
    profile: AssistProfile,
}

/// The one suggestion request that is open.
struct RunningSuggestion {
    id: u64,
    task: JoinHandle<()>,
    cancel: CancellationToken,
}

/// The meeting engine. Construct it with the config and its seams, then
/// run the loop over a command receiver; the loop is the only place
/// meeting state changes.
pub struct Engine {
    config: Config,
    deps: EngineDeps,
    /// The tap `deps.ui` records through while a meeting holds a sink.
    trace: Current,
}

enum Next {
    Command(EngineCommand),
    Internal(Internal),
    Finished(suggest::Finished),
    Piece(PieceDone),
    Tick,
    Closed,
}

/// How the policy sees a run that ended this way.
fn end_outcome(end: &SuggestionEnd) -> Outcome {
    match end {
        SuggestionEnd::Done => Outcome::Ok,
        SuggestionEnd::Failed(_) | SuggestionEnd::Interrupted => Outcome::Failed,
        SuggestionEnd::Cancelled => Outcome::Cancelled,
    }
}

/// Wait for the next thing the engine loop has to react to.
async fn next_event(
    internal_rx: &mut mpsc::UnboundedReceiver<Internal>,
    commands: &mut mpsc::UnboundedReceiver<EngineCommand>,
    finished_rx: &mut mpsc::UnboundedReceiver<suggest::Finished>,
    meeting: &mut Option<Meeting>,
) -> Next {
    let wake = meeting.as_ref().and_then(|current| current.wake_at);
    tokio::select! { biased;
        message = internal_rx.recv() => message.map_or(Next::Closed, Next::Internal),
        command = commands.recv() => command.map_or(Next::Closed, Next::Command),
        finished = finished_rx.recv() => finished.map_or(Next::Closed, Next::Finished),
        piece = async {
            match meeting.as_mut() {
                Some(current) => current.pieces.recv().await,
                None => std::future::pending().await,
            }
        } => piece.map_or(Next::Closed, Next::Piece),
        _ = async {
            match wake {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        } => Next::Tick,
    }
}

impl Engine {
    pub fn new(config: Config, mut deps: EngineDeps) -> Self {
        let trace = Current::new();
        let ui = deps.ui.clone();
        deps.ui = trace_tap::wrap_ui(ui, trace.clone());
        Self {
            config,
            deps,
            trace,
        }
    }

    /// Own the meeting state until `Shutdown` or the command sender is
    /// dropped. Must run inside a tokio runtime.
    pub async fn run(self, mut commands: mpsc::UnboundedReceiver<EngineCommand>) {
        let (internal_tx, mut internal_rx) = mpsc::unbounded_channel();
        let (finished_tx, mut finished_rx) = mpsc::unbounded_channel::<suggest::Finished>();
        let mut meeting: Option<Meeting> = None;
        let mut state = MeetingState::Idle;
        let mut next_suggestion_id: u64 = 0;
        let mut profile = self.config.assist.start_profile;
        self.emit(UiEvent::Profile(profile));

        loop {
            let next = next_event(
                &mut internal_rx,
                &mut commands,
                &mut finished_rx,
                &mut meeting,
            )
            .await;
            match next {
                Next::Internal(Internal::Panic(message)) => {
                    tracing::error!(%message, "engine component panicked");
                    self.emit(pipeline::panic_status(&message));
                    if meeting.is_some() {
                        self.stop_meeting(&mut meeting, EndReason::Panic).await;
                        state = MeetingState::Idle;
                    }
                }
                Next::Internal(Internal::Drained) => {
                    if state == MeetingState::Running
                        && let Some(current) = meeting.as_mut()
                    {
                        // Every piece message was queued before the drain
                        // settled; take them in before judging quietness.
                        Self::absorb_pieces(current);
                        current.drained = true;
                    }
                }
                Next::Finished(finished) => self.on_finished(&mut meeting, finished),
                Next::Piece(piece) => Self::on_piece(&mut meeting, piece),
                Next::Tick => {}
                Next::Command(command) => {
                    // Every command received while a meeting exists, also
                    // the ones its state ignores.
                    self.trace.record(Body::command(&command));
                    match command {
                        EngineCommand::StartMeeting if state == MeetingState::Idle => {
                            state = self
                                .start_meeting(&mut meeting, &internal_tx, profile)
                                .await;
                        }
                        EngineCommand::StopMeeting if state == MeetingState::Running => {
                            self.stop_meeting(&mut meeting, EndReason::Stop).await;
                            state = MeetingState::Idle;
                        }
                        EngineCommand::ToggleMeeting => match state {
                            MeetingState::Idle => {
                                state = self
                                    .start_meeting(&mut meeting, &internal_tx, profile)
                                    .await;
                            }
                            MeetingState::Running => {
                                self.stop_meeting(&mut meeting, EndReason::Stop).await;
                                state = MeetingState::Idle;
                            }
                            MeetingState::Starting | MeetingState::Stopping => {}
                        },
                        EngineCommand::Suggest if state == MeetingState::Running => {
                            if let Some(current) = meeting.as_mut() {
                                next_suggestion_id += 1;
                                self.run_suggestion(
                                    current,
                                    next_suggestion_id,
                                    profile,
                                    Origin::Manual,
                                    &finished_tx,
                                )
                                .await;
                            }
                        }
                        EngineCommand::ClearSuggestion if state == MeetingState::Running => {
                            if let Some(current) = meeting.as_mut() {
                                Self::cancel_suggestion(current).await;
                                current.previous_answer = None;
                                self.emit(UiEvent::ClearSuggestion);
                            }
                        }
                        EngineCommand::CycleProfile => {
                            let wanted = profile.next();
                            self.change_profile(&mut profile, wanted, &mut meeting);
                        }
                        EngineCommand::SetProfile(wanted) => {
                            self.set_profile(&mut profile, wanted, &mut meeting);
                        }
                        EngineCommand::Shutdown => {
                            if meeting.is_some() {
                                // The stop sequence itself ends with an Idle event.
                                self.stop_meeting(&mut meeting, EndReason::Shutdown).await;
                            } else {
                                self.emit(UiEvent::MeetingState(MeetingState::Idle));
                            }
                            return;
                        }
                        EngineCommand::StartMeeting
                        | EngineCommand::StopMeeting
                        | EngineCommand::Suggest
                        | EngineCommand::ClearSuggestion => {}
                    }
                }
                Next::Closed => break,
            }
            self.pump_running(
                state,
                &mut meeting,
                &mut next_suggestion_id,
                profile,
                &finished_tx,
            )
            .await;
        }
        // The command sender went away: shut down like `Shutdown` did.
        if meeting.is_some() {
            self.stop_meeting(&mut meeting, EndReason::ChannelClosed)
                .await;
        }
    }

    /// A finished answer only matters while a meeting exists.
    fn on_finished(&self, meeting: &mut Option<Meeting>, finished: suggest::Finished) {
        if let Some(current) = meeting.as_mut() {
            self.suggestion_finished(current, finished);
        }
    }

    /// A finished piece only matters while a meeting exists.
    fn on_piece(meeting: &mut Option<Meeting>, piece: PieceDone) {
        if let Some(current) = meeting.as_mut() {
            current.sink.record(Body::PieceDone {
                speaker: piece.speaker.into(),
                chars: piece.text.as_ref().map(|text| text.chars().count()),
            });
            current
                .policy
                .piece_done(piece.speaker, piece.text.as_deref(), Instant::now());
        }
    }

    /// Switch to `wanted` unless it is already active.
    fn set_profile(
        &self,
        profile: &mut AssistProfile,
        wanted: AssistProfile,
        meeting: &mut Option<Meeting>,
    ) {
        if wanted != *profile {
            self.change_profile(profile, wanted, meeting);
        }
    }

    /// Run the policy after every wake-up, but only for a running meeting.
    async fn pump_running(
        &self,
        state: MeetingState,
        meeting: &mut Option<Meeting>,
        next_suggestion_id: &mut u64,
        profile: AssistProfile,
        finished_tx: &mpsc::UnboundedSender<suggest::Finished>,
    ) {
        if state == MeetingState::Running
            && let Some(current) = meeting.as_mut()
        {
            self.pump(current, next_suggestion_id, profile, finished_tx)
                .await;
        }
    }

    /// Switch the active profile and tell the UI; a running meeting's policy
    /// drops its waiting trigger.
    fn change_profile(
        &self,
        profile: &mut AssistProfile,
        wanted: AssistProfile,
        meeting: &mut Option<Meeting>,
    ) {
        *profile = wanted;
        if let Some(current) = meeting.as_mut() {
            current.policy.set_profile(wanted);
            current.profile = wanted;
        }
        self.emit(UiEvent::Profile(wanted));
    }

    /// Feed every queued piece report to the policy.
    fn absorb_pieces(meeting: &mut Meeting) {
        while let Ok(piece) = meeting.pieces.try_recv() {
            meeting.sink.record(Body::PieceDone {
                speaker: piece.speaker.into(),
                chars: piece.text.as_ref().map(|text| text.chars().count()),
            });
            meeting
                .policy
                .piece_done(piece.speaker, piece.text.as_deref(), Instant::now());
        }
    }

    /// Ask the policy what to do now: start an automatic request, set the
    /// wake-up time, and release `SourcesDrained` once nothing is running or
    /// waiting.
    async fn pump(
        &self,
        meeting: &mut Meeting,
        next_suggestion_id: &mut u64,
        profile: AssistProfile,
        finished_tx: &mpsc::UnboundedSender<suggest::Finished>,
    ) {
        let busy =
            assist::trigger(profile).is_some_and(|trigger| meeting.pipeline.busy(trigger.speaker));
        meeting.wake_at = None;
        match meeting.policy.poll(Instant::now(), busy) {
            Decision::Idle => meeting.was_waiting = false,
            Decision::WaitUntil(at) => {
                if !meeting.was_waiting {
                    tracing::info!(assist_profile = profile.key(), assist_outcome = "waiting");
                    meeting.sink.record(Body::Policy {
                        outcome: PolicyOutcome::Waiting,
                        profile: profile.into(),
                        suggestion: None,
                    });
                    meeting.was_waiting = true;
                }
                meeting.wake_at = Some(tokio::time::Instant::from_std(at));
            }
            Decision::Fire => {
                meeting.was_waiting = false;
                *next_suggestion_id += 1;
                tracing::info!(
                    assist_profile = profile.key(),
                    assist_outcome = "fired",
                    suggestion = *next_suggestion_id,
                );
                meeting.sink.record(Body::Policy {
                    outcome: PolicyOutcome::Fired,
                    profile: profile.into(),
                    suggestion: Some(*next_suggestion_id),
                });
                self.run_suggestion(
                    meeting,
                    *next_suggestion_id,
                    profile,
                    Origin::Auto,
                    finished_tx,
                )
                .await;
            }
        }
        self.release_drained(meeting);
    }

    /// Release `SourcesDrained` once, when the sources are drained and the
    /// policy has nothing running or waiting.
    fn release_drained(&self, meeting: &mut Meeting) {
        if meeting.drained && !meeting.drained_emitted && meeting.policy.is_quiet() {
            meeting.drained_emitted = true;
            self.emit(UiEvent::SourcesDrained);
        }
    }

    /// A suggestion run reported its end: if it is the open request, close it,
    /// tell the policy, remember the answer and update the LLM status.
    fn suggestion_finished(&self, meeting: &mut Meeting, finished: suggest::Finished) {
        if meeting
            .suggestion
            .as_ref()
            .is_none_or(|running| running.id != finished.id)
        {
            return;
        }
        meeting.suggestion = None;
        meeting
            .policy
            .request_finished(end_outcome(&finished.end), Instant::now());
        self.apply_end(meeting, finished);
    }

    /// Automatic requests pause after a failed or interrupted request; say so.
    fn log_pause(&self, meeting: &Meeting, suggestion: u64, reason: &str) {
        tracing::warn!(
            suggestion,
            reason,
            pause_secs = self.deps.timings.auto_failure_pause.as_secs_f64(),
            assist_outcome = "paused",
            "suggestion failed, automatic requests pause"
        );
        meeting.sink.record(Body::Policy {
            outcome: PolicyOutcome::Paused,
            profile: meeting.profile.into(),
            suggestion: Some(suggestion),
        });
    }

    /// Remember the answer and update the LLM status for how the run ended.
    fn apply_end(&self, meeting: &mut Meeting, finished: suggest::Finished) {
        match finished.end {
            SuggestionEnd::Done => {
                if !finished.shown.is_empty() {
                    meeting.previous_answer = Some(finished.shown);
                }
                if meeting.llm_failed {
                    meeting.llm_failed = false;
                    self.emit(health::llm_reachable(meeting.llm.model()));
                }
            }
            SuggestionEnd::Failed(reason) => {
                self.log_pause(meeting, finished.id, &reason);
                meeting.llm_failed = true;
                self.llm_status(reason);
            }
            SuggestionEnd::Interrupted => {
                self.log_pause(meeting, finished.id, "interrupted");
                meeting.llm_failed = true;
                self.llm_status("LLM answer interrupted".to_owned());
            }
            SuggestionEnd::Cancelled => {}
        }
    }

    fn llm_status(&self, text: String) {
        self.emit(UiEvent::Status {
            source: StatusSource::Llm,
            level: StatusLevel::Error,
            text,
        });
    }

    /// The full start sequence; returns the state reached (`Running`, or
    /// `Idle` when no source could be opened).
    async fn start_meeting(
        &self,
        meeting: &mut Option<Meeting>,
        internal_tx: &mpsc::UnboundedSender<Internal>,
        profile: AssistProfile,
    ) -> MeetingState {
        // The trace opens before the first event so the whole meeting,
        // `Starting` included, lands in it (D-trace-tap).
        let speakers = self.deps.factory.speakers();
        let start = trace_tap::session_start(&self.config, &self.deps, &speakers, profile);
        let sink = self.open_trace(start).await;
        self.trace.set(sink.clone());
        self.emit(UiEvent::MeetingState(MeetingState::Starting));
        if let Some(location) = sink.location() {
            // One line telling the user where this meeting is stored
            // (D-audio-indicator); the text comes from the sink, not the config.
            let audio = if location.audio { " (with audio)" } else { "" };
            self.emit(UiEvent::Status {
                source: StatusSource::App,
                level: StatusLevel::Info,
                text: format!("recording to {}{}", location.dir.display(), audio),
            });
        }
        let timings = self.deps.timings;
        let asr = Arc::new(AsrClient::new(
            &self.config.asr.base_url,
            &self.config.asr.model,
            self.config.asr.api_key.clone(),
            self.config.asr.language.clone(),
            timings.asr_timeout,
            timings.asr_backoff,
        ));
        let llm = Arc::new(LlmClient::new(
            &self.config.llm.base_url,
            &self.config.llm.model,
            self.config.llm.api_key.clone(),
            timings.llm_connect,
            timings.llm_stall,
        ));
        for event in health::check(&asr, &self.config.asr.model, &llm, timings.health_timeout).await
        {
            self.emit(event);
        }
        let notes = self.read_notes();
        if let Some(text) = &notes {
            sink.record(Body::Notes { text: text.clone() });
        }

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
            // The meeting ended before it began; the trace says so (D-session-end).
            sink.close(EndReason::StartFailed);
            self.trace.take();
            self.emit(UiEvent::MeetingState(MeetingState::Idle));
            return MeetingState::Idle;
        }

        let clock = MeetingClock::new();
        sink.record(Body::ClockStarted);
        let store = Arc::new(Mutex::new(TranscriptStore::new()));
        let cancel = CancellationToken::new();
        let machine = MachineParams::from(&self.config.vad);
        let mut pipeline = Pipeline::start(
            sources,
            &self.deps,
            asr,
            &machine,
            store.clone(),
            clock,
            cancel.clone(),
            sink.clone(),
        );

        let pieces = pipeline
            .pieces()
            .expect("a fresh pipeline hands out its pieces receiver once");
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
            self.config.llm.enable_thinking,
            self.config.llm.include_usage,
            pipeline.commits(),
            self.deps.ui.clone(),
            compress_cancel.clone(),
        ));

        *meeting = Some(Meeting {
            pipeline,
            store,
            llm,
            notes,
            cancel,
            compress_cancel,
            compress_task,
            drained_task,
            panic_task,
            suggestion: None,
            last_trigger_line_id: 0,
            pieces,
            policy: AutoPolicy::new(
                profile,
                PolicyTimings {
                    turn_settle: timings.turn_settle,
                    turn_max_wait: timings.turn_max_wait,
                    min_gap: timings.auto_min_gap,
                    failure_pause: timings.auto_failure_pause,
                },
            ),
            previous_answer: None,
            drained: false,
            drained_emitted: false,
            llm_failed: false,
            wake_at: None,
            was_waiting: false,
            sink,
            profile,
        });
        self.emit(UiEvent::MeetingState(MeetingState::Running));
        MeetingState::Running
    }

    /// The full stop sequence: cancel the suggestion, the compression
    /// task and the meeting token, flush and join the pipeline, close the
    /// trace, then report `Idle`.
    async fn stop_meeting(&self, meeting: &mut Option<Meeting>, reason: EndReason) {
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
        let sink = std::mem::replace(&mut current.sink, Arc::new(NoTrace));
        *meeting = None;
        self.close_trace(sink, reason).await;
        self.emit(UiEvent::MeetingState(MeetingState::Idle));
    }

    /// Open this meeting's trace off the loop thread: an opener may create
    /// directories, which must not stall the engine. A failed open is one
    /// Warn status and an unrecorded meeting (C-trace-failure-isolated).
    async fn open_trace(&self, start: SessionStart) -> Arc<dyn TraceSink> {
        let opener = self.deps.trace.clone();
        let ui = self.deps.ui.clone();
        let on_failure: FailureSink = Arc::new(move |message| {
            (ui)(UiEvent::Status {
                source: StatusSource::App,
                level: StatusLevel::Warn,
                text: format!("trace: recording stopped: {message}"),
            });
        });
        match tokio::task::spawn_blocking(move || opener.open(start, on_failure)).await {
            Ok(Ok(sink)) => sink,
            Ok(Err(message)) => {
                self.emit(UiEvent::Status {
                    source: StatusSource::App,
                    level: StatusLevel::Warn,
                    text: format!("trace: cannot record this meeting: {message}"),
                });
                Arc::new(NoTrace)
            }
            Err(error) => {
                tracing::warn!(%error, "trace opener task failed");
                Arc::new(NoTrace)
            }
        }
    }

    /// Flush and close a meeting's trace before the loop may report `Idle`
    /// (C-trace-closed-before-idle): the writer thread is joined off the
    /// loop, and the whole close gives up after 3 s so a stuck disk can
    /// never keep `Idle` from being sent.
    async fn close_trace(&self, sink: Arc<dyn TraceSink>, reason: EndReason) {
        // Take the slot first: the `Idle` event must not be recorded and
        // nothing may be added after the `end` record.
        self.trace.take();
        let closing = tokio::task::spawn_blocking(move || sink.close(reason));
        match tokio::time::timeout(Duration::from_secs(3), closing).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "trace close task failed"),
            Err(_) => {
                tracing::warn!(
                    "trace close did not finish in time; the session may lack its end record"
                );
            }
        }
    }

    /// Cancel the running suggestion (its task reports `Cancelled`) and
    /// wait for that end event so later engine events follow it. The policy
    /// hears about the cancellation here, because the run's own report is
    /// ignored once the request is no longer the open one.
    async fn cancel_suggestion(meeting: &mut Meeting) {
        if let Some(mut running) = meeting.suggestion.take() {
            suggest::cancel_and_wait(&mut running.task, &running.cancel).await;
            meeting
                .policy
                .request_finished(Outcome::Cancelled, Instant::now());
        }
    }

    /// Build the prompt from the store plus in-progress text and stream
    /// one new suggestion. A manual request cancels the open one first; an
    /// automatic one is only started when nothing is open.
    async fn run_suggestion(
        &self,
        meeting: &mut Meeting,
        id: u64,
        profile: AssistProfile,
        origin: Origin,
        finished_tx: &mpsc::UnboundedSender<suggest::Finished>,
    ) {
        if origin == Origin::Manual {
            Self::cancel_suggestion(meeting).await;
        }
        self.emit(UiEvent::SuggestionStart { id });
        let in_progress = meeting.pipeline.in_progress();
        let built = {
            let store = meeting.store.lock().expect("store lock");
            prompt::build_for(
                &store,
                meeting.notes.as_deref(),
                &in_progress,
                meeting.last_trigger_line_id,
                &Ask {
                    profile,
                    origin,
                    previous_answer: meeting.previous_answer.as_deref(),
                },
            )
        };
        meeting.last_trigger_line_id = built.last_line_id;
        let request = ChatRequest::new(
            meeting.llm.model(),
            suggest::to_llm_messages(built.messages),
            self.config.llm.max_tokens,
            self.config.llm.temperature as f64,
            self.config.llm.enable_thinking,
            self.config.llm.include_usage,
        );
        let cancel = CancellationToken::new();
        meeting.policy.request_started(origin, Instant::now());
        let task = tokio::spawn(suggest::run(
            id,
            meeting.llm.clone(),
            request,
            cancel.clone(),
            self.deps.ui.clone(),
            origin == Origin::Auto,
            finished_tx.clone(),
        ));
        meeting.suggestion = Some(RunningSuggestion { id, task, cancel });
    }

    /// The notes file is read once per meeting; a missing file is a
    /// Warn status and the meeting runs without it.
    fn read_notes(&self) -> Option<String> {
        let path = self.config.llm.notes_path.as_deref()?;
        match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) => {
                self.emit(UiEvent::Status {
                    source: StatusSource::App,
                    level: StatusLevel::Warn,
                    text: format!("notes file {path} could not be read: {error}"),
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
