//! The clueless binary: parses the command line, then runs either the
//! replay pipeline over WAV files or the GUI (overlay on the main thread,
//! engine with live capture on a background tokio runtime).

use std::io::Write as _;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clueless::cli::{self, Replay};
use clueless::{envfile, lock, logging};
use clueless_types::audio::SourceFactory;
use clueless_types::config::Config;
use clueless_types::events::{
    CommandSink, EngineCommand, Speaker, StatusSink, SuggestionEnd, UiEvent,
};
use clueless_types::profile::AssistProfile;
use engine::deps::EngineDeps;
use engine::meeting;
use engine::replay::WavSources;

/// A replay run may not take longer than this (generous: real-time e2e
/// replays of the longest fixture plus server latency fit inside).
const REPLAY_LIMIT: Duration = Duration::from_secs(900);

fn main() -> ExitCode {
    let parsed = match cli::parse(std::env::args().skip(1)) {
        Ok(cli::Parsed::Cli(cli)) => cli,
        Ok(cli::Parsed::Help) => {
            print!("{}", cli::usage());
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n\n{}", cli::usage());
            return ExitCode::from(2);
        }
    };
    if let Err(message) = logging::init(&parsed.log_file) {
        eprintln!("{message}");
        return ExitCode::from(2);
    }
    let config_path = parsed
        .config
        .clone()
        .unwrap_or_else(|| cli::home().join(".config/clueless/config.toml"));
    let env_vars = match envfile::load(parsed.env_file.as_deref()) {
        Ok(vars) => vars,
        Err(message) => return startup_failure(&message),
    };
    // Process environment first, then the .env file, then defaults inside
    // the config builders.
    let lookup = envfile::overlay(|name| std::env::var(name).ok(), &env_vars);
    // The default config path being absent is normal (config.toml is
    // optional); an explicit --config that does not exist is an error.
    let config_file = match &parsed.config {
        Some(path) => Some(path.as_path()),
        None if config_path.is_file() => Some(config_path.as_path()),
        None => None,
    };
    let config = match Config::load(config_file, &lookup) {
        Ok(config) => config,
        Err(error) => return startup_failure(&error.to_string()),
    };
    match parsed.replay {
        Some(replay) => run_replay(config, replay),
        None => run_gui(config),
    }
}

/// Logs a failure after the logger is open (to stderr and the log file) and
/// returns the exit code 2 for `main` to return. An app started with `open` has no visible stderr, so the
/// message goes to the log file as well; `cargo xtask run` looks for the
/// `startup failed: ` marker in the log (C-startup-failure-marker); the same
/// literal is `STARTUP_FAILURE_MARKER` in xtask.
fn startup_failure(message: &str) -> ExitCode {
    tracing::error!("startup failed: {message}");
    ExitCode::from(2)
}

/// Replay mode: WAV sources through a real engine, printing finals to
/// stdout and statuses to stderr; exits 0 when the sources drain and no
/// automatic answer is running or waiting (and, with `--ask`, when the
/// suggestion asked for at the end ends). The profile is the flag's, or
/// Manual: the config file's `start_profile` is for the GUI.
fn run_replay(mut config: Config, replay: Replay) -> ExitCode {
    config.assist.start_profile = replay.profile.unwrap_or(AssistProfile::Manual);
    let mut files = vec![(Speaker::Me, replay.files[0].clone())];
    if let Some(them) = replay.files.get(1) {
        files.push((Speaker::Them, them.clone()));
    }
    let factory = match WavSources::new(files, replay.speed) {
        Ok(factory) => Arc::new(factory),
        Err(error) => {
            eprintln!("replay: {error:?}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("replay: cannot start a tokio runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(replay_loop(config, factory, replay.ask)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("replay: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn replay_loop(
    config: Config,
    factory: Arc<dyn SourceFactory>,
    ask: bool,
) -> Result<(), String> {
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
    let ui: StatusSink = Arc::new(move |event| {
        let _ = events_tx.send(event);
    });
    let (commands, receiver) = meeting::command_channel();
    let engine = meeting::Engine::new(config, EngineDeps::production(factory, ui));
    let running = tokio::spawn(engine.run(receiver));
    commands
        .send(EngineCommand::StartMeeting)
        .map_err(|_| "the engine loop exited before the meeting started".to_string())?;

    let outcome = tokio::time::timeout(REPLAY_LIMIT, async {
        let mut asked = false;
        let mut printer = SuggestionPrinter::default();
        while let Some(event) = events_rx.recv().await {
            match event {
                UiEvent::TranscriptFinal(final_) => {
                    println!(
                        "{} {}: {}",
                        clock_label(final_.t0_ms),
                        speaker_label(final_.id.speaker),
                        final_.text
                    );
                }
                UiEvent::Status {
                    source,
                    level,
                    text,
                } => eprintln!("{source:?} {level:?}: {text}"),
                UiEvent::SuggestionDelta { id, text } => {
                    let mut out = std::io::stdout();
                    printer.delta(id, &text, &mut out);
                    let _ = out.flush();
                }
                UiEvent::SuggestionEnd { id, end } => {
                    printer.end(id, &end, &mut std::io::stdout(), &mut std::io::stderr());
                    // `SourcesDrained` only arrives once nothing is running
                    // or waiting, so after the asked request was sent this
                    // is its end.
                    if asked {
                        break;
                    }
                }
                UiEvent::SourcesDrained => {
                    if ask && !asked {
                        asked = true;
                        let _ = commands.send(EngineCommand::Suggest);
                    } else if !ask {
                        break;
                    }
                }
                _ => {}
            }
        }
    })
    .await;

    let _ = commands.send(EngineCommand::Shutdown);
    let _ = tokio::time::timeout(Duration::from_secs(10), running).await;
    outcome.map_err(|_| format!("replay did not finish within {} s", REPLAY_LIMIT.as_secs()))
}

/// GUI mode: the single-instance lock, the engine with live capture on a
/// background runtime thread, and the overlay on the main thread.
fn run_gui(config: Config) -> ExitCode {
    let lock_path = cli::home().join("Library/Application Support/clueless/lock");
    let _lock = match lock::acquire(&lock_path) {
        Ok(lock) => lock,
        Err(message) => return startup_failure(&message),
    };
    let (commands_tx, commands_rx) = meeting::command_channel();
    let commands: CommandSink = Arc::new(move |command| {
        let _ = commands_tx.send(command);
    });
    let audio = &config.audio;
    let factory = Arc::new(capture::backend::LiveSources::new(
        audio.system_audio_backend.clone(),
        audio.mic_device.clone(),
        audio.watchdog_restarts,
        audio.watchdog_silence_secs,
    ));
    let ui: StatusSink = Arc::new(overlay::ui::post);
    let engine = meeting::Engine::new(config.clone(), EngineDeps::production(factory, ui));
    std::thread::Builder::new()
        .name("engine".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime for the engine");
            runtime.block_on(engine.run(commands_rx));
        })
        .expect("the engine thread starts");
    overlay::ui::run_on_main_thread(config, commands);
    ExitCode::SUCCESS
}

/// Prints suggestion deltas and ends, with the "--- suggestion ---" header
/// once per answer.
#[derive(Default)]
struct SuggestionPrinter {
    /// The id of the answer whose header is out.
    header_for: Option<u64>,
}

impl SuggestionPrinter {
    fn delta(&mut self, id: u64, text: &str, out: &mut impl std::io::Write) {
        if self.header_for != Some(id) {
            let _ = writeln!(out, "--- suggestion ---");
            self.header_for = Some(id);
        }
        let _ = write!(out, "{text}");
    }

    fn end(
        &self,
        id: u64,
        end: &SuggestionEnd,
        out: &mut impl std::io::Write,
        err: &mut impl std::io::Write,
    ) {
        if self.header_for == Some(id) {
            let _ = writeln!(out);
        }
        if let SuggestionEnd::Failed(reason) = end {
            let _ = writeln!(err, "suggestion failed: {reason}");
        }
    }
}

fn speaker_label(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::Me => "Me",
        Speaker::Them => "Them",
    }
}

/// `t0_ms` as `[mm:ss]`, clamping minutes at 99 for absurd offsets.
fn clock_label(t0_ms: u64) -> String {
    let total_seconds = t0_ms / 1000;
    let minutes = (total_seconds / 60).min(99);
    format!("[{minutes:02}:{:02}]", total_seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(buffer: Vec<u8>) -> String {
        String::from_utf8(buffer).expect("utf-8")
    }

    #[test]
    fn the_header_is_printed_once_per_answer_and_the_end_closes_the_line() {
        let mut printer = SuggestionPrinter::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        printer.delta(1, "a", &mut out);
        printer.delta(1, "b", &mut out);
        printer.end(1, &SuggestionEnd::Done, &mut out, &mut err);
        printer.delta(2, "c", &mut out);
        assert_eq!(text(out), "--- suggestion ---\nab\n--- suggestion ---\nc");
        assert!(err.is_empty());
    }

    #[test]
    fn a_failed_end_is_reported_on_stderr_and_an_unseen_id_prints_no_newline() {
        let printer = SuggestionPrinter::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        printer.end(
            7,
            &SuggestionEnd::Failed("boom".to_string()),
            &mut out,
            &mut err,
        );
        assert!(out.is_empty());
        assert_eq!(text(err), "suggestion failed: boom\n");
    }
}
