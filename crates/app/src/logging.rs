//! One tracing subscriber writing every line to both stderr and the log
//! file, through a `Mutex<File>` writer as decided in D-logging. The
//! latency fields the pipeline logs (utterance `seq`, `vad_end_ms`,
//! `asr_sent_ms`, `asr_done_ms`, `llm_first_delta_ms`) end up in both.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use tracing_subscriber::fmt::MakeWriter;

/// Create the log file (and its parent directories) and install the
/// stderr-plus-file subscriber. Called once from `main`.
pub fn init(path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|error| format!("cannot create log directory {}: {error}", dir.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("cannot open log file {}: {error}", path.display()))?;
    let writer = DualWriter {
        file: Arc::new(Mutex::new(file)),
    };
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(writer)
        .try_init()
        .map_err(|_| "the logging subscriber was already installed".to_string())
}

/// Writes each trace line to stderr and to the shared log file.
#[derive(Clone)]
struct DualWriter {
    file: Arc<Mutex<File>>,
}

struct DualGuard<'a> {
    file: MutexGuard<'a, File>,
}

impl Write for DualGuard<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = io::stderr().write(buffer)?;
        self.file.write_all(buffer)?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()?;
        self.file.flush()
    }
}

impl<'a> MakeWriter<'a> for DualWriter {
    type Writer = DualGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        DualGuard {
            // A panicked writer thread must not take the app down.
            file: self
                .file
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        }
    }
}
