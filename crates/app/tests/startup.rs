//! The real `clueless` binary failing at startup: the failure must reach
//! the log file as well as stderr, because an app started with `open` has
//! no visible stderr and `cargo xtask run` reads the log to report it
//! (C-startup-failure-marker).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_clueless");
const MARKER: &str = "startup failed: ";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clueless-startup-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs the binary with an empty environment, `dir` as home and working
/// directory, and the log in `dir/log.txt`.
fn run(dir: &TempDir, args: &[&Path]) -> Output {
    Command::new(BIN)
        .env_clear()
        .env("HOME", &dir.0)
        .current_dir(&dir.0)
        .arg("--log-file")
        .arg(dir.join("log.txt"))
        .args(args)
        .output()
        .expect("binary runs")
}

fn write_valid_env(dir: &TempDir) -> PathBuf {
    let path = dir.join("valid.env");
    std::fs::write(
        &path,
        "LLM_BASE_URL=http://127.0.0.1:1\nLLM_MODEL=m\nASR_BASE_URL=http://127.0.0.1:2\nASR_MODEL=a\n",
    )
    .expect("env file");
    path
}

fn log_text(dir: &TempDir) -> String {
    std::fs::read_to_string(dir.join("log.txt")).expect("the log file exists")
}

#[test]
fn legacy_llm_table_in_the_config_is_reported_in_the_log_and_on_stderr() {
    let dir = TempDir::new("legacy-llm");
    let env = write_valid_env(&dir);
    let config = dir.join("config.toml");
    std::fs::write(&config, "[llm]\nmax_tokens = 100\n").expect("config");
    let out = run(
        &dir,
        &[
            Path::new("--env-file"),
            &env,
            Path::new("--config"),
            &config,
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    let log = log_text(&dir);
    assert!(log.contains(MARKER), "log: {log}");
    assert!(log.contains("[llm] moved to the environment"), "log: {log}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[llm] moved to the environment"),
        "stderr: {stderr}"
    );
}

#[test]
fn missing_required_variables_are_reported_in_the_log() {
    let dir = TempDir::new("no-env");
    let out = run(&dir, &[]);
    assert_eq!(out.status.code(), Some(2));
    let log = log_text(&dir);
    assert!(log.contains(MARKER), "log: {log}");
    assert!(log.contains("LLM_BASE_URL"), "log: {log}");
}

#[test]
fn a_held_instance_lock_is_reported_in_the_log_with_its_path() {
    let dir = TempDir::new("lock-held");
    let env = write_valid_env(&dir);
    let lock_dir = dir.join("Library/Application Support/clueless");
    std::fs::create_dir_all(&lock_dir).expect("lock dir");
    let lock_path = lock_dir.join("lock");
    let holder = std::fs::File::create(&lock_path).expect("lock file");
    holder.lock().expect("the test takes the lock first");
    let out = run(&dir, &[Path::new("--env-file"), &env]);
    assert_eq!(out.status.code(), Some(2));
    let log = log_text(&dir);
    assert!(log.contains(MARKER), "log: {log}");
    assert!(log.contains("another clueless instance"), "log: {log}");
    assert!(
        log.contains(lock_path.to_str().expect("utf-8 path")),
        "log: {log}"
    );
}

#[test]
fn an_unknown_flag_prints_usage_and_writes_no_log() {
    let dir = TempDir::new("bad-flag");
    let out = Command::new(BIN)
        .env_clear()
        .env("HOME", &dir.0)
        .arg("--no-such-flag")
        .arg("--log-file")
        .arg(dir.join("log.txt"))
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage:"));
    assert!(
        !dir.join("log.txt").exists(),
        "no logger before the arguments parse"
    );
}
