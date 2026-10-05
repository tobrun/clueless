//! The `--sessions` listing over a data directory (D-inspect-tools).
//!
//! One row per session directory, newest first: name, start time,
//! duration, origin, audio, finals, suggestions, runs, and `cut off` for
//! a session without an `end` record. Counts come from `reader::scan`,
//! so no session is ever fully held in memory; a directory whose
//! manifest cannot be read still gets a row, marked unreadable.

use std::path::Path;

use crate::manifest::Origin;
use crate::paths::{RUNS_DIR, SESSIONS_DIR, utc_name};
use crate::reader::{read_manifest, scan};
use crate::record::Body;

/// Everything the listing shows about one session directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The session directory's name.
    pub name: String,
    /// True when the manifest is missing or unreadable; the counts below
    /// are then all zero.
    pub unreadable: bool,
    /// Wall-clock start from the manifest, Unix milliseconds.
    pub started_at_ms: u64,
    pub origin: Option<Origin>,
    pub audio: bool,
    /// Meeting milliseconds between `clock_started` and the last record.
    pub duration_ms: u64,
    pub finals: usize,
    pub suggestions: usize,
    pub runs: usize,
    /// True when the records hold no `end` record.
    pub cut_off: bool,
}

impl Row {
    /// The one-line rendering for `--sessions`.
    pub fn render(&self) -> String {
        if self.unreadable {
            return format!("{}  unreadable", self.name);
        }
        let origin = match self.origin {
            Some(origin) => origin_name(origin),
            None => "unknown",
        };
        let audio = if self.audio { "yes" } else { "no" };
        let cut = if self.cut_off { "  cut off" } else { "" };
        format!(
            "{}  start {}  duration {}  {origin}  audio {audio}  finals {}  suggestions {}  runs {}{cut}",
            self.name,
            utc_name(self.started_at_ms),
            render_duration(self.duration_ms),
            self.finals,
            self.suggestions,
            self.runs,
        )
    }
}

fn origin_name(origin: Origin) -> &'static str {
    match origin {
        Origin::Live => "live",
        Origin::ReplayWav => "replay_wav",
        Origin::Rerun => "rerun",
    }
}

fn render_duration(ms: u64) -> String {
    let (m, s) = (ms / 60_000, (ms % 60_000) / 1000);
    if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// List the sessions of `data_dir`, newest first. A missing or empty data
/// directory gives no rows; a session whose manifest cannot be read gives
/// a row marked unreadable.
pub fn list(data_dir: &Path) -> Vec<Row> {
    let Ok(entries) = std::fs::read_dir(data_dir.join(SESSIONS_DIR)) else {
        return Vec::new();
    };
    let mut rows: Vec<Row> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| row_for(&entry.path()))
        .collect();
    rows.sort_by(|a, b| {
        b.unreadable
            .cmp(&a.unreadable)
            .then(b.started_at_ms.cmp(&a.started_at_ms))
            .then(b.name.cmp(&a.name))
    });
    rows
}

fn row_for(dir: &Path) -> Row {
    let name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let Ok(manifest) = read_manifest(dir) else {
        return Row {
            name,
            unreadable: true,
            started_at_ms: 0,
            origin: None,
            audio: false,
            duration_ms: 0,
            finals: 0,
            suggestions: 0,
            runs: count_runs(dir),
            cut_off: true,
        };
    };
    let mut counts = ScanCounts::default();
    if scan(dir, |record| counts.observe(record)).is_err() {
        // No record file at all: a manifest-only session counts as zero
        // and stays cut off.
        counts.saw_end = false;
    }
    let clock = counts.clock_at_ms.unwrap_or(0);
    let elapsed = counts.last_at_ms.unwrap_or(0).saturating_sub(clock);
    Row {
        name,
        unreadable: false,
        started_at_ms: manifest.started_at_ms,
        origin: Some(manifest.origin),
        audio: manifest.audio,
        duration_ms: (elapsed as f64 * manifest.speed).round() as u64,
        finals: counts.finals,
        suggestions: counts.suggestions,
        runs: count_runs(dir),
        cut_off: !counts.saw_end,
    }
}

#[derive(Default)]
struct ScanCounts {
    finals: usize,
    suggestions: usize,
    saw_end: bool,
    clock_at_ms: Option<u64>,
    last_at_ms: Option<u64>,
}

impl ScanCounts {
    fn observe(&mut self, record: &crate::record::Record) {
        match &record.body {
            Body::TranscriptFinal { .. } => self.finals += 1,
            Body::SuggestionStart { .. } => self.suggestions += 1,
            Body::ClockStarted => {
                self.clock_at_ms.get_or_insert(record.at_ms);
            }
            Body::End { .. } => self.saw_end = true,
            _ => {}
        }
        self.last_at_ms = Some(record.at_ms.max(self.last_at_ms.unwrap_or(0)));
    }
}

/// The number of run directories below a session.
fn count_runs(session: &Path) -> usize {
    std::fs::read_dir(session.join(RUNS_DIR))
        .map(|entries| entries.flatten().filter(|e| e.path().is_dir()).count())
        .unwrap_or(0)
}
