//! Session and run directory naming, argument resolution and the private
//! file and directory helpers.
//!
//! A session directory is named by its UTC start time (`2026-10-05T14-03-22Z`)
//! so listings sort by time without a time zone dependency, and every path
//! the trace writes is created through the private helpers here: directories
//! 0700, files 0600, because the files hold other people's words and voices.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Directory under the data directory holding all sessions.
pub const SESSIONS_DIR: &str = "sessions";
/// Directory under a session holding its re-runs.
pub const RUNS_DIR: &str = "runs";
/// Name of the record file inside a trace directory.
pub const EVENTS_FILE: &str = "events.jsonl";
/// Directory inside a session holding the speaker WAVs.
pub const AUDIO_DIR: &str = "audio";
/// The notes copy a re-run is given, inside a run directory.
pub const NOTES_FILE: &str = "notes.txt";

/// Seconds and milliseconds units of the meeting/timeline math.
pub const MS_PER_S: u64 = 1000;
/// Samples per millisecond at the stored 16 kHz sample rate.
pub const SAMPLES_PER_MS: u64 = 16;

/// The session directory name for a wall-clock Unix millisecond: a UTC
/// timestamp like `2026-10-05T14-03-22Z`, computed with a days-to-civil
/// calculation so no time zone dependency is needed.
pub fn utc_name(unix_ms: u64) -> String {
    let secs = unix_ms / MS_PER_S;
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}Z",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    )
}

/// Days since the Unix epoch to a proleptic Gregorian (year, month, day),
/// after Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let shifted_month = (5 * day_of_year + 2) / 153; // [0, 11], March first
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32; // [1, 31]
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Create `parent/name` as a private directory; when the name exists, try
/// `name-2`, `name-3`, and so on. Returns the directory actually created.
pub fn create_unique(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let mut suffix = 1;
    loop {
        let candidate = if suffix == 1 {
            parent.join(name)
        } else {
            parent.join(format!("{name}-{suffix}"))
        };
        match create_private_dir_single(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => suffix += 1,
            Err(error) => return Err(error),
        }
    }
}

/// Resolve a command line SESSION value to a directory, per D-session-arg:
/// a value starting with `/` or `.` is a path; anything else names a session
/// (or a run below one) inside `<data_dir>/sessions/`.
pub fn resolve(data_dir: &Path, arg: &str) -> PathBuf {
    if arg.starts_with('/') || arg.starts_with('.') {
        PathBuf::from(arg)
    } else {
        data_dir.join(SESSIONS_DIR).join(arg)
    }
}

/// The run of `session` whose manifest has the highest start time, or `None`
/// when it has no readable runs.
pub fn newest_run(session: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(session.join(RUNS_DIR)).ok()?;
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        let Ok(manifest) = crate::reader::read_manifest(&dir) else {
            continue;
        };
        let better = match &best {
            Some((started, _)) => manifest.started_at_ms > *started,
            None => true,
        };
        if better {
            best = Some((manifest.started_at_ms, dir));
        }
    }
    best.map(|(_, dir)| dir)
}

/// Create `path` and its missing parents as private directories (0700).
/// Directories that already exist are left as they are, wider modes included.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

/// Create (or truncate) `path` as a private file (0600).
pub fn create_private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Open `path` for appending as a private file (0600), created when missing;
/// the record file's append-only line.
pub fn open_private_append(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
}

fn create_private_dir_single(path: &Path) -> io::Result<()> {
    DirBuilder::new().mode(0o700).create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_name_of_the_epoch_is_new_year_1970() {
        assert_eq!(utc_name(0), "1970-01-01T00-00-00Z");
    }

    #[test]
    fn utc_name_formats_a_current_millisecond_timestamp() {
        assert_eq!(utc_name(1_791_209_002_000), "2026-10-05T14-03-22Z");
    }

    #[test]
    fn utc_name_handles_the_leap_day_of_a_leap_year() {
        assert_eq!(utc_name(1_835_438_400_000), "2028-02-29T12-00-00Z");
    }

    #[test]
    fn resolve_treats_a_bare_name_as_a_session_below_the_data_dir() {
        let data = Path::new("/data");
        assert_eq!(
            resolve(data, "2026-10-05T14-03-22Z"),
            PathBuf::from("/data/sessions/2026-10-05T14-03-22Z")
        );
    }

    #[test]
    fn resolve_treats_dot_and_slash_prefixed_values_as_paths() {
        let data = Path::new("/data");
        assert_eq!(resolve(data, "./x/y"), PathBuf::from("./x/y"));
        assert_eq!(resolve(data, "/abs/s"), PathBuf::from("/abs/s"));
    }

    #[test]
    fn resolve_treats_a_name_with_a_run_as_a_run_below_the_session() {
        let data = Path::new("/data");
        assert_eq!(
            resolve(data, "S/runs/R"),
            PathBuf::from("/data/sessions/S/runs/R")
        );
    }
}
