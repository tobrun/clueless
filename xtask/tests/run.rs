//! End-to-end tests for `cargo xtask run`: the real xtask binary, with the
//! app build skipped, a stand-in app binary, a throwaway `HOME` (so the log
//! file is ours) and a fake `open` first on `PATH`. `open` is the operating
//! system boundary: the real one starts the app through LaunchServices,
//! which ignores this test's environment and needs a desktop session.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is a direct member of the workspace")
        .to_path_buf()
}

fn scratch(name: &str) -> PathBuf {
    let dir = workspace_root()
        .join("target")
        .join("xtask-run-tests")
        .join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("scratch dir is ours to reset");
    }
    fs::create_dir_all(&dir).expect("can create the scratch dir");
    dir
}

/// Runs `xtask run` with `open_script` as the body of the fake `open`, after
/// putting `seeded_log` (when given) in the log file. Returns the output and
/// the log path.
fn xtask_run(test: &str, seeded_log: Option<&str>, open_script: &str) -> (Output, PathBuf) {
    let dir = scratch(test);
    let log = dir.join("Library/Logs/clueless/clueless.log");
    fs::create_dir_all(log.parent().expect("log dir")).expect("log dir");
    if let Some(seeded) = seeded_log {
        fs::write(&log, seeded).expect("seed log");
    }

    let debug = dir.join("target").join("debug");
    fs::create_dir_all(&debug).expect("debug dir");
    let source = dir.join("app.rs");
    fs::write(&source, "fn main() {}\n").expect("fake source");
    let status = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(debug.join("clueless"))
        .status()
        .expect("rustc is on the path");
    assert!(status.success(), "rustc failed");

    let bin = dir.join("bin");
    fs::create_dir_all(&bin).expect("bin dir");
    let open = bin.join("open");
    fs::write(&open, format!("#!/bin/sh\n{open_script}\n")).expect("fake open");
    fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).expect("chmod");

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").expect("PATH is set")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("run")
        .current_dir(workspace_root())
        .env("PATH", path)
        .env("HOME", &dir)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .env("CLUELESS_XTASK_SKIP_BUILD", "1")
        .output()
        .expect("xtask runs");
    (output, log)
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A fake `open` that appends `line` to the app log, as the app would.
fn appends(line: &str) -> String {
    format!("printf '%s\\n' '{line}' >> \"$HOME/Library/Logs/clueless/clueless.log\"")
}

#[test]
fn run_fails_with_the_startup_error_when_the_app_logs_one() {
    let (output, log) = xtask_run(
        "app-fails",
        None,
        &appends(
            "2026-10-04T07:00:00Z ERROR clueless: startup failed: invalid config: [llm] moved to the environment",
        ),
    );
    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(all.contains("[llm] moved to the environment"), "{all}");
    assert!(all.contains(log.to_str().expect("utf-8 path")), "{all}");
}

#[test]
fn run_succeeds_when_the_app_logs_no_startup_failure() {
    let (output, log) = xtask_run(
        "app-starts",
        None,
        &appends("2026-10-04T07:00:00Z  INFO overlay::ui: engine idle"),
    );
    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    assert!(all.contains(log.to_str().expect("utf-8 path")), "{all}");
}

#[test]
fn a_failure_logged_by_an_earlier_run_does_not_fail_this_launch() {
    let (output, _) = xtask_run(
        "earlier-failure",
        Some("2026-10-03T07:00:00Z ERROR clueless: startup failed: yesterday\n"),
        "exit 0",
    );
    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
}
