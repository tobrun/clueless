//! Single-instance lock: an exclusive `File::try_lock` on a well-known
//! path, so a second GUI instance fails fast with a message naming the
//! path instead of fighting the first one for the microphones.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Seek as _, Write};
use std::path::{Path, PathBuf};

/// The held lock; the second instance's [`acquire`] fails while this lives.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
    _file: File,
}

impl Lock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Create the parent directories if needed and take an exclusive lock on
/// `path`. The error messages always name the path.
pub fn acquire(path: &Path) -> Result<Lock, String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("cannot open lock file {}: {error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {
            let mut locked = &file;
            let _ = locked.rewind();
            let _ = writeln!(locked, "{}", std::process::id());
            Ok(Lock {
                path: path.to_path_buf(),
                _file: file,
            })
        }
        Err(TryLockError::WouldBlock) => Err(format!(
            "another clueless instance already holds the lock at {}",
            path.display()
        )),
        Err(error) => Err(format!("cannot lock {}: {error}", path.display())),
    }
}
