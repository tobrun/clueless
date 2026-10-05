//! The session inspection modes: `--sessions`, `--show`, `--delete` and
//! `--compare`. They read files under the data directory only (plus the
//! judge for `--compare`), so `main` dispatches them before the server
//! settings are read (D-inspect-tools).
//!
//! Stub implementations: change set 9 fills these in over
//! `trace::{list, show}` and the compare report of change set 4.

use std::path::Path;
use std::process::ExitCode;

use clueless_types::config::Config;

/// Print one line per recorded session, newest first (`--sessions`).
pub fn sessions(data_dir: &Path) -> ExitCode {
    let _ = data_dir;
    not_implemented("sessions")
}

/// Print one session as transcript with suggestions (`--show`).
pub fn show(data_dir: &Path, session: &str) -> ExitCode {
    let _ = (data_dir, session);
    not_implemented("show")
}

/// Delete one session or run (`--delete`); without `yes` this prints the
/// path and size and deletes nothing (D-delete-safety).
pub fn delete(data_dir: &Path, session: &str, yes: bool) -> ExitCode {
    let _ = (data_dir, session, yes);
    not_implemented("delete")
}

/// Compare two traces (`--compare`); `config` is present when judging is on,
/// because the judge needs the server settings and the no-judge report must
/// run without them.
pub fn compare(data_dir: &Path, a: &str, b: Option<&str>, config: Option<&Config>) -> ExitCode {
    let _ = (data_dir, a, b, config);
    not_implemented("compare")
}

/// The fixed stub line and exit code every mode carries until change set 9
/// implements it.
fn not_implemented(what: &str) -> ExitCode {
    eprintln!("{what}: not implemented");
    ExitCode::from(2)
}
