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
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
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
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    match parsed.replay {
        Some(replay) => run_replay(config, replay),
        None => run_gui(config),
    }
}

/// Replay mode: WAV sources through a real engine, printing finals to
/// stdout and statuses to stderr; exits 0 when the sources drain (and,
/// with `--ask`, when the suggestion ends).
fn run_replay(config: Config, replay: Replay) -> ExitCode {
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
                UiEvent::SuggestionStart { .. } => println!("--- suggestion ---"),
                UiEvent::SuggestionDelta { text, .. } => {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                }
                UiEvent::SuggestionEnd { end, .. } => {
                    println!();
                    if let SuggestionEnd::Failed(reason) = end {
                        eprintln!("suggestion failed: {reason}");
                    }
                    break;
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
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
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
