//! The tolerant trace reader: manifest plus records sorted by `seq`.
//!
//! A reader must keep working on traces written by later builds: the
//! manifest's `schema` refuses only higher versions, unknown record kinds
//! parse as [`Body::Unknown`], unknown fields are ignored, and a last line
//! that does not parse (an interrupted write) is skipped and shows as
//! `cut_off`.

use crate::audio::wav_sample_count;
use crate::manifest::{MANIFEST_FILE, Manifest, SCHEMA};
use crate::paths::{AUDIO_DIR, EVENTS_FILE, NOTES_FILE, SAMPLES_PER_MS};
use crate::record::{Body, Record, Speaker};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Why a trace directory cannot be read.
#[derive(Debug, Error)]
pub enum ReadError {
    /// A file of the trace could not be opened or read; the message names
    /// the path.
    #[error("{path}: {error}", path = .path.display())]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    /// The manifest's `manifest.json` is not readable as a manifest.
    #[error("{path}: {error}", path = .path.display())]
    Manifest {
        path: PathBuf,
        error: serde_json::Error,
    },
    /// The manifest asks for a newer format than this build understands.
    #[error("{path}: trace schema {found} is newer than this build reads ({SCHEMA})")]
    NewerSchema { path: PathBuf, found: u32 },
}

/// A whole trace read from disk: where it lives, what it ran with, its
/// records sorted by `seq`, and whether it ends without an `end` record.
#[derive(Debug, Clone)]
pub struct Trace {
    /// The directory holding the manifest and the record file.
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub records: Vec<Record>,
    /// True when no `end` record was found: the session was cut off.
    pub cut_off: bool,
}

impl Trace {
    /// The audio WAVs present in the trace, per speaker.
    pub fn audio_paths(&self) -> Vec<(Speaker, PathBuf)> {
        [Speaker::Me, Speaker::Them]
            .into_iter()
            .filter_map(|speaker| {
                let name = match speaker {
                    Speaker::Me => "me",
                    Speaker::Them => "them",
                };
                let path = self.dir.join(AUDIO_DIR).join(format!("{name}.wav"));
                path.exists().then_some((speaker, path))
            })
            .collect()
    }

    /// The notes of the trace: the re-run's `notes.txt` copy when present,
    /// else the text of the `notes` record.
    pub fn notes(&self) -> Option<String> {
        let path = self.dir.join(NOTES_FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some(text);
        }
        self.records.iter().find_map(|record| match &record.body {
            Body::Notes { text } => Some(text.clone()),
            _ => None,
        })
    }

    /// The length of the longest speaker WAV in meeting milliseconds, or
    /// `None` when the trace holds no audio.
    pub fn audio_ms(&self) -> Option<u64> {
        self.audio_paths()
            .iter()
            .filter_map(|(_, path)| wav_sample_count(path).ok())
            .max()
            .map(|samples| samples / SAMPLES_PER_MS)
    }

    /// The meeting time of a record: milliseconds since `clock_started`,
    /// scaled by the manifest's replay speed (D-timestamps).
    pub fn meeting_ms(&self, record: &Record) -> u64 {
        self.at_meeting_ms(record.at_ms)
    }

    /// Meeting milliseconds for a raw `at_ms` value.
    pub fn at_meeting_ms(&self, at_ms: u64) -> u64 {
        let clock = self
            .records
            .iter()
            .find(|record| record.body == Body::ClockStarted)
            .map_or(0, |record| record.at_ms);
        ((at_ms.saturating_sub(clock)) as f64 * self.manifest.speed).round() as u64
    }
}

/// Read a whole trace: manifest, records sorted by `seq`, and whether an
/// `end` record closes it.
pub fn read(dir: &Path) -> Result<Trace, ReadError> {
    let manifest = read_manifest(dir)?;
    let mut records = Vec::new();
    let mut cut_off = true;
    scan_records(dir, &mut |record| {
        if matches!(record.body, Body::End { .. }) {
            cut_off = false;
        }
        records.push(record.clone());
    })?;
    records.sort_by_key(|record| record.seq);
    Ok(Trace {
        dir: dir.to_path_buf(),
        manifest,
        records,
        cut_off,
    })
}

/// Read only the manifest of a trace directory, with its schema check.
pub fn read_manifest(dir: &Path) -> Result<Manifest, ReadError> {
    let path = dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path).map_err(|error| ReadError::Io {
        path: path.clone(),
        error,
    })?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|error| ReadError::Manifest {
        path: path.clone(),
        error,
    })?;
    if manifest.schema > SCHEMA {
        return Err(ReadError::NewerSchema {
            path,
            found: manifest.schema,
        });
    }
    Ok(manifest)
}

/// Call `f` for every readable record of a trace without keeping them, for
/// reports that only count. Records arrive in file order; a last line that
/// does not parse is skipped.
pub fn scan<F: FnMut(&Record)>(dir: &Path, mut f: F) -> Result<(), ReadError> {
    read_manifest(dir)?;
    scan_records(dir, &mut f)
}

/// The record file of `dir`, line by line, skipping a tail line that does
/// not parse (an interrupted write).
fn scan_records<F: FnMut(&Record)>(dir: &Path, f: &mut F) -> Result<(), ReadError> {
    let path = dir.join(EVENTS_FILE);
    let file = BufReader::new(std::fs::File::open(&path).map_err(|error| ReadError::Io {
        path: path.clone(),
        error,
    })?);
    // Every line is attempted; one that does not parse is skipped. Lines
    // are flushed as written, so in a file the writer left behind only the
    // last line can be broken, and skipping it is the whole tolerance.
    for line in file.lines() {
        let Ok(text) = line else { continue };
        if let Ok(record) = serde_json::from_str::<Record>(&text) {
            f(&record);
        }
    }
    Ok(())
}
