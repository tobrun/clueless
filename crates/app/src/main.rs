//! The clueless binary: parses the command line, then runs one mode - the
//! GUI (overlay on the main thread, engine with live capture on a background
//! tokio runtime), a headless replay over WAV files or a recorded session,
//! or one of the session commands (`--sessions`, `--show`, `--delete`,
//! `--compare`), which read files only and therefore run before the server
//! settings are read (D-inspect-tools).

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clueless::cli::{self, Cli, Mode, Replay};
use clueless::{envfile, lock, logging, recording, replay_print, rerun, session_cmd};
use clueless_types::audio::SourceFactory;
use clueless_types::config::Config;
use clueless_types::events::{CommandSink, EngineCommand, Speaker, StatusSink, UiEvent};
use clueless_types::profile::AssistProfile;
use engine::deps::EngineDeps;
use engine::meeting;
use engine::replay::WavSources;
use trace::manifest::Origin;
use trace::sink::TraceOpener;

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
    // Reading files must not need a server address, so the inspect modes
    // are dispatched before the env file and the config are touched.
    match &parsed.mode {
        Mode::Sessions => return session_cmd::sessions(&parsed.data_dir),
        Mode::Show { session } => return session_cmd::show(&parsed.data_dir, session),
        Mode::Delete { session, yes } => {
            return session_cmd::delete(&parsed.data_dir, session, *yes);
        }
        Mode::Compare { a, b, judge: false } => {
            return session_cmd::compare(&parsed.data_dir, a, b.as_deref(), None);
        }
        _ => {}
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
    match parsed.mode.clone() {
        Mode::Replay(replay) => run_replay(config, &parsed, replay),
        Mode::ReplaySession { session, speed } => {
            rerun::run(config, &parsed.data_dir, &session, speed)
        }
        Mode::Compare { a, b, judge: true } => {
            session_cmd::compare(&parsed.data_dir, &a, b.as_deref(), Some(&config))
        }
        Mode::Gui => run_gui(config, &parsed),
        Mode::Sessions | Mode::Show { .. } | Mode::Delete { .. } | Mode::Compare { .. } => {
            unreachable!("the read-only modes returned before the config was read")
        }
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
fn run_replay(mut config: Config, cli: &Cli, replay: Replay) -> ExitCode {
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
    // A WAV replay records a session like a live meeting, with origin
    // `replay_wav` (D-wav-replay-records).
    let trace = recording::opener(&config, cli, Origin::ReplayWav, replay.speed);
    let runtime = match rerun::replay_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match runtime.block_on(replay_loop(config, factory, trace, replay.ask)) {
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
    trace_opener: Arc<dyn TraceOpener>,
    ask: bool,
) -> Result<(), String> {
    let rerun::RunningMeeting {
        commands,
        mut events,
        running,
    } = rerun::start_meeting(config, factory, trace_opener)?;

    let outcome = tokio::time::timeout(REPLAY_LIMIT, async {
        let mut asked = false;
        let mut printer = replay_print::SuggestionPrinter::default();
        while let Some(event) = events.recv().await {
            rerun::print_meeting_event(&mut printer, &event);
            match event {
                UiEvent::SuggestionEnd { .. } => {
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

    rerun::finish_meeting(
        &commands,
        running,
        outcome,
        &format!("replay did not finish within {} s", REPLAY_LIMIT.as_secs()),
    )
    .await
}

/// GUI mode: the single-instance lock, the engine with live capture on a
/// background runtime thread, and the overlay on the main thread.
fn run_gui(config: Config, cli: &Cli) -> ExitCode {
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
    let mut deps = EngineDeps::production(factory, ui);
    deps.trace = recording::opener(&config, cli, Origin::Live, 1.0);
    let engine = meeting::Engine::new(config.clone(), deps);
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
