//! The session re-run (`--replay-session`): feeding a recorded session's
//! audio and recorded commands through this build against the live servers
//! and writing the result as a run below the session (D-replay-meaning,
//! D-replay-output).
//!
//! The audio, the notes, the start profile and the hotkey commands come
//! from the session; the servers, models and prompt code come from the
//! current setup (D-replay-inputs). Everything the meeting prints goes
//! through `replay_print`, so stdout looks exactly like `--replay`, and the
//! run directory is announced on stderr once its trace is written.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::replay_print;
use clueless_types::config::Config;
use clueless_types::events::{EngineCommand, MeetingState, Speaker, UiEvent};
use clueless_types::profile::AssistProfile;
use engine::deps::EngineDeps;
use engine::meeting;
use engine::replay::WavSources;
use trace::paths::{self, NOTES_FILE, create_private_file};
use trace::reader::{self, Trace};
use trace::record::{Body, CommandName, Profile, Speaker as TraceSpeaker};
use trace::sink::TraceOpener;
use trace::writer::DiskOpener;

/// A recorded command with the re-run seconds (relative wall time after the
/// meeting started) it is due at: its meeting time divided by the speed
/// (D-replay-inputs).
type Scheduled = (f64, EngineCommand);

/// Re-run the session `session` (a name under the data directory or a path,
/// D-session-arg) at `speed`, printing through `replay_print` and announcing
/// the run directory on stderr.
pub fn run(config: Config, data_dir: &Path, session: &str, speed: f64) -> ExitCode {
    let dir = paths::resolve(data_dir, session);
    let session_trace = match reader::read(&dir) {
        Ok(session_trace) => session_trace,
        Err(error) => {
            eprintln!("replay: {error}");
            return ExitCode::from(2);
        }
    };
    let audio = session_trace.audio_paths();
    if audio.is_empty() {
        let name = dir_name(&dir);
        eprintln!("replay: session {name} has no audio; record with [trace] audio = true");
        return ExitCode::from(2);
    }
    if speed != 1.0 {
        eprintln!(
            "replay: warning: replaying at {speed}x; the echo hold, turn settling and minimum gap run on the real clock"
        );
    }
    let files = audio
        .into_iter()
        .map(|(speaker, path)| {
            let speaker = match speaker {
                TraceSpeaker::Me => Speaker::Me,
                TraceSpeaker::Them => Speaker::Them,
            };
            (speaker, path)
        })
        .collect();
    let factory = match WavSources::new(files, speed) {
        Ok(factory) => Arc::new(factory),
        Err(error) => {
            eprintln!("replay: {error:?}");
            return ExitCode::from(2);
        }
    };
    // The run directory exists from here on, even if a later step fails.
    let opener = match DiskOpener::for_run(
        &dir,
        speed,
        env!("CARGO_PKG_VERSION").to_string(),
        env!("CLUELESS_GIT_COMMIT").to_string(),
    ) {
        Ok(opener) => opener,
        Err(error) => {
            eprintln!("replay: cannot open the run trace: {error}");
            return ExitCode::from(1);
        }
    };
    let run_dir = opener
        .run_dir()
        .expect("for_run creates the run directory")
        .to_path_buf();
    let config = match stage_notes(config, &run_dir, session_trace.notes()) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("replay: {message}");
            return ExitCode::from(1);
        }
    };
    let mut config = config;
    config.assist.start_profile = assist_profile(session_trace.manifest.session.profile);
    let commands = recorded_commands(&session_trace, speed);
    // D-replay-limits: the audio's meeting length over the speed, plus two
    // minutes for server latency and the commands recorded after the drain.
    let audio_ms = session_trace.audio_ms().unwrap_or(0);
    let limit = Duration::from_millis((audio_ms as f64 / speed) as u64 + 120_000);
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("replay: cannot start a tokio runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(rerun_loop(
        config,
        factory,
        Arc::new(opener) as Arc<dyn TraceOpener>,
        commands,
        limit,
    )) {
        Ok(()) => {
            eprintln!("run: {}", run_dir.display());
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("replay: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Write the recorded notes into the run directory as `notes.txt` (0600,
/// D-file-permissions) and point the config at that copy, so the re-run's
/// prompt carries the same notes without the original file (D-replay-inputs).
/// A session without recorded notes clears the configured path instead.
fn stage_notes(
    mut config: Config,
    run_dir: &Path,
    notes: Option<String>,
) -> Result<Config, String> {
    let Some(text) = notes else {
        config.llm.notes_path = None;
        return Ok(config);
    };
    let path = run_dir.join(NOTES_FILE);
    let mut file = create_private_file(&path)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    std::io::Write::write_all(&mut file, text.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    config.llm.notes_path = Some(path.display().to_string());
    Ok(config)
}

/// The recorded commands a re-run replays, with their due seconds at
/// `speed`; everything the recorded commands cannot express (servers,
/// prompt code) comes from the current setup (D-replay-inputs).
fn recorded_commands(session_trace: &Trace, speed: f64) -> VecDeque<Scheduled> {
    session_trace
        .records
        .iter()
        .filter_map(|record| {
            let Body::Command { command, profile } = &record.body else {
                return None;
            };
            let command = match (command, profile) {
                (CommandName::Suggest, _) => EngineCommand::Suggest,
                (CommandName::ClearSuggestion, _) => EngineCommand::ClearSuggestion,
                (CommandName::CycleProfile, _) => EngineCommand::CycleProfile,
                (CommandName::SetProfile, Some(profile)) => {
                    EngineCommand::SetProfile(assist_profile(*profile))
                }
                _ => return None,
            };
            let meeting_ms = session_trace.at_meeting_ms(record.at_ms);
            Some((meeting_ms as f64 / speed / 1000.0, command))
        })
        .collect()
}

fn assist_profile(profile: Profile) -> AssistProfile {
    match profile {
        Profile::Manual => AssistProfile::Manual,
        Profile::Interview => AssistProfile::Interview,
        Profile::Brainstorm => AssistProfile::Brainstorm,
    }
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// The meeting loop: same printing as `--replay`, plus the recorded
/// commands at their due seconds. Ends per D-replay-end: the sources
/// drained, every recorded command sent, and no suggestion open; commands
/// still waiting when the sources drain go at once, in order.
async fn rerun_loop(
    config: Config,
    factory: Arc<dyn clueless_types::audio::SourceFactory>,
    trace_opener: Arc<dyn TraceOpener>,
    mut pending: VecDeque<Scheduled>,
    limit: Duration,
) -> Result<(), String> {
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
    let ui = Arc::new(move |event| {
        let _ = events_tx.send(event);
    });
    let (commands, receiver) = meeting::command_channel();
    let mut deps = EngineDeps::production(factory, ui);
    deps.trace = trace_opener;
    let engine = meeting::Engine::new(config, deps);
    let running = tokio::spawn(engine.run(receiver));
    commands
        .send(EngineCommand::StartMeeting)
        .map_err(|_| "the engine loop exited before the meeting started".to_string())?;

    let outcome = tokio::time::timeout(limit, async {
        let mut printer = replay_print::SuggestionPrinter::default();
        let mut started: Option<Instant> = None;
        let mut drained = false;
        let mut open_suggestions = 0usize;
        // D-replay-end's three conditions can hold for one instant while a
        // just-sent Suggest has not reached the engine loop yet; require
        // them to hold quietly for a moment before calling it an end.
        let mut quiet_since: Option<Instant> = None;
        loop {
            while let Ok(event) = events_rx.try_recv() {
                match event {
                    UiEvent::MeetingState(MeetingState::Running) if started.is_none() => {
                        started = Some(Instant::now());
                    }
                    UiEvent::TranscriptFinal(final_) => replay_print::print_final(&final_),
                    UiEvent::Status {
                        source,
                        level,
                        text,
                    } => replay_print::print_status(source, level, &text),
                    UiEvent::SuggestionStart { .. } => open_suggestions += 1,
                    UiEvent::SuggestionDelta { id, text } => {
                        let mut out = std::io::stdout();
                        printer.delta(id, &text, &mut out);
                        let _ = out.flush();
                    }
                    UiEvent::SuggestionEnd { id, end } => {
                        printer.end(id, &end, &mut std::io::stdout(), &mut std::io::stderr());
                        open_suggestions = open_suggestions.saturating_sub(1);
                    }
                    UiEvent::SourcesDrained => {
                        drained = true;
                        // Whatever is still waiting belongs to the tail of
                        // the meeting (an `--ask` suggest lands here): send
                        // it at once, in order (D-replay-end).
                        while let Some((_, command)) = pending.pop_front() {
                            let _ = commands.send(command);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(started) = started {
                let elapsed = started.elapsed().as_secs_f64();
                while let Some((due, _)) = pending.front() {
                    if *due > elapsed {
                        break;
                    }
                    let (_, command) = pending.pop_front().expect("checked front");
                    let _ = commands.send(command);
                }
            }
            if drained && pending.is_empty() && open_suggestions == 0 && started.is_some() {
                if quiet_since.is_some_and(|since| since.elapsed() >= Duration::from_millis(500)) {
                    break;
                }
                quiet_since.get_or_insert_with(Instant::now);
            } else {
                quiet_since = None;
            }
            // The event queue is only drained by polling here: a recorded
            // command can become due while no event arrives at all, and the
            // real clock rules the meeting either way (D-replay-pacing).
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    let _ = commands.send(EngineCommand::Shutdown);
    let _ = tokio::time::timeout(Duration::from_secs(10), running).await;
    outcome.map_err(|_| format!("the re-run did not finish within {} s", limit.as_secs()))
}
