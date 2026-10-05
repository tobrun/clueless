//! The session re-run (`--replay-session`): feeding a recorded session's
//! audio and recorded commands through this build and writing the result as
//! a run below the session (D-replay-meaning, D-replay-output).
//!
//! Stub implementation: change set 9 fills this in over
//! `trace::reader`, `engine::replay::WavSources` and `trace::writer::DiskOpener::for_run`.

use std::path::Path;
use std::process::ExitCode;

use clueless_types::config::Config;

/// Re-run the session `session` (a name under the data directory or a path,
/// D-session-arg) at `speed`, printing through `replay_print` and announcing
/// the run directory on stderr.
pub fn run(config: Config, data_dir: &Path, session: &str, speed: f64) -> ExitCode {
    let _ = (config, data_dir, session, speed);
    eprintln!("replay-session: not implemented");
    ExitCode::from(2)
}
