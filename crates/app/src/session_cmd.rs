//! The recorded session commands: `--sessions`, `--show`, `--delete` and
//! the no-judge `--compare` (change set 9, D-inspect-tools).
//!
//! Everything here works from the trace files alone and never touches the
//! engine or a meeting. `--delete` follows D-delete-safety: it previews the
//! path and size and exits 1 without `--yes`, it only removes a directory
//! whose `manifest.json` parses as a trace manifest and which holds an
//! `events.jsonl`, and it refuses while a recorder holds any of those
//! files locked.

use std::fs::{OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clueless_types::config::Config;
use trace::paths::{EVENTS_FILE, RUNS_DIR};
use trace::{compare, list, paths, reader, show};

/// One line per session, newest first (D-inspect-tools). A missing or
/// empty data directory prints nothing; exit 0 either way.
pub fn sessions(data_dir: &Path) -> ExitCode {
    for row in list::list(data_dir) {
        println!("{}", row.render());
    }
    ExitCode::SUCCESS
}

/// One session as transcript with suggestions, through `trace::show`.
pub fn show(data_dir: &Path, session: &str) -> ExitCode {
    let dir = paths::resolve(data_dir, session);
    match reader::read(&dir) {
        Ok(trace) => {
            print!("{}", show::show(&trace));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("show: {error}");
            ExitCode::from(2)
        }
    }
}

/// Delete one session or run (D-delete-safety): without `--yes` print the
/// path and size and exit 1; a directory that is not a trace exits 2; a
/// trace whose `events.jsonl`, or that of any run below it, is locked by a
/// recorder exits 1 naming the session.
pub fn delete(data_dir: &Path, session: &str, yes: bool) -> ExitCode {
    let dir = paths::resolve(data_dir, session);
    if !is_trace(&dir) {
        eprintln!(
            "delete: {} is not a recorded session or run: it needs a readable trace manifest.json and an events.jsonl",
            dir.display()
        );
        return ExitCode::from(2);
    }
    if let Some(locked) = locked_events(&dir) {
        eprintln!(
            "delete: session {} is being recorded ({} is locked); stop the recording first",
            session_name(&dir),
            locked.display()
        );
        return ExitCode::from(1);
    }
    let size = dir_size(&dir);
    println!("{}: {size} bytes", dir.display());
    if !yes {
        println!("nothing was deleted; repeat with --yes to delete it");
        return ExitCode::from(1);
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {
            println!("deleted {}", dir.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("delete: cannot remove {}: {error}", dir.display());
            ExitCode::from(1)
        }
    }
}

/// The report of D-compare-metric: the diff always, the judge only when a
/// config came in (a `judge: false` `--compare` never reads the config or
/// the env file, D-inspect-tools). With judging on and the server
/// unreachable every pair stays `not judged` and the exit code is 0.
pub fn compare(data_dir: &Path, a: &str, b: Option<&str>, judge: Option<&Config>) -> ExitCode {
    let dir_a = paths::resolve(data_dir, a);
    let dir_b = match b {
        Some(b) => paths::resolve(data_dir, b),
        None => match paths::newest_run(&dir_a) {
            Some(run) => run,
            None => {
                eprintln!("compare: session {a} has no runs to compare against");
                return ExitCode::from(2);
            }
        },
    };
    let (trace_a, trace_b) = match (reader::read(&dir_a), reader::read(&dir_b)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("compare: {error}");
            return ExitCode::from(2);
        }
    };
    let mut report = compare::compare(&trace_a, &trace_b);
    if let Some(config) = judge {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("compare: cannot start a tokio runtime: {error}");
                return ExitCode::FAILURE;
            }
        };
        runtime.block_on(engine::judge::judge_report_from_config(
            &config.llm,
            &trace_a,
            &mut report,
        ));
    }
    print!("{}", report.render());
    ExitCode::SUCCESS
}

/// True when `dir` holds a readable trace manifest and an `events.jsonl`,
/// the two files that make a directory a trace (D-delete-safety).
fn is_trace(dir: &Path) -> bool {
    dir.is_dir() && reader::read_manifest(dir).is_ok() && dir.join(EVENTS_FILE).is_file()
}

/// The first `events.jsonl` of `dir` or of a run below it that a recorder
/// holds locked, or `None` when every lock can be taken. Taking a lock we
/// can have and dropping it is the test; the recorder's own `try_lock` in
/// `trace::writer` is what fails here while a meeting records.
fn locked_events(dir: &Path) -> Option<PathBuf> {
    let mut candidates = vec![dir.join(EVENTS_FILE)];
    if let Ok(entries) = std::fs::read_dir(dir.join(RUNS_DIR)) {
        candidates.extend(entries.flatten().map(|e| e.path().join(EVENTS_FILE)));
    }
    candidates
        .into_iter()
        .find(|path| path.is_file() && is_locked(path))
}

fn is_locked(path: &Path) -> bool {
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => matches!(file.try_lock(), Err(TryLockError::WouldBlock)),
        // A file that cannot be reopened is treated as unreadable, not as
        // locked; the removal below reports the real error.
        Err(_) => false,
    }
}

fn session_name(dir: &Path) -> String {
    // For a run, the session name is one level up.
    let dir = dir
        .parent()
        .and_then(|p| {
            (p.file_name() == Some(std::ffi::OsStr::new(RUNS_DIR)))
                .then(|| p.parent())
                .flatten()
        })
        .unwrap_or(dir);
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// The sum of the file sizes below `dir`, the number `--delete` previews.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}
