//! Choosing the trace opener for this launch (D-trace-switches): the text
//! trace is on unless `[trace] enabled = false`, the audio tier is off
//! unless `[trace] audio = true`, and the data directory is the CLI's
//! (`--data-dir` or `~/.clueless`).

use std::sync::Arc;

use clueless_types::config::Config;
use trace::manifest::Origin;
use trace::sink::{NoTrace, TraceOpener};
use trace::writer::DiskOpener;

use crate::cli::Cli;

/// The opener the engine records through: nothing when the trace is
/// switched off, one session directory per meeting under the data
/// directory otherwise. The manifest pins the app version and the git
/// commit of this build (D-manifest-contents); `CLUELESS_GIT_COMMIT` comes
/// from `build.rs` and is `unknown` when git could not answer.
pub fn opener(config: &Config, cli: &Cli, origin: Origin, speed: f64) -> Arc<dyn TraceOpener> {
    if !config.trace.enabled {
        return Arc::new(NoTrace);
    }
    Arc::new(DiskOpener::new(
        cli.data_dir.clone(),
        config.trace.audio,
        origin,
        speed,
        env!("CARGO_PKG_VERSION").to_string(),
        env!("CLUELESS_GIT_COMMIT").to_string(),
    ))
}
