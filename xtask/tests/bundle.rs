//! Integration tests for the dev bundle tasks (`cargo xtask bundle`).
//!
//! Every test runs the real `cargo xtask` alias from the repository root, so
//! the alias, the CLI, the bundle assembly, the codesign interaction and the
//! plist are all exercised for real. Two things are deliberately not built
//! from this workspace's sources:
//! - the app binary: sibling change sets may be mid-edit while these tests
//!   run, so the bundle gets a compiled stand-in binary via `--binary` and
//!   `CLUELESS_XTASK_SKIP_BUILD=1` instead of `cargo build -p clueless`
//!   (the Validation command `cargo xtask bundle` proves the real build);
//! - the keychain: the signing identity tests pass `--identity` explicitly,
//!   and the certificate script is only ever run with `--dry-run` inside a
//!   throwaway `HOME`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is a direct member of the workspace")
        .to_path_buf()
}

/// A fresh scratch directory under `target`, one per test, so tests running
/// in parallel never share a bundle or a binary.
fn scratch(name: &str) -> PathBuf {
    let dir = workspace_root()
        .join("target")
        .join("xtask-bundle-tests")
        .join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("scratch dir is ours to reset");
    }
    fs::create_dir_all(&dir).expect("can create the scratch dir");
    dir
}

/// Compile a trivial Mach-O to stand in for the app binary.
fn fake_binary(name: &str) -> PathBuf {
    let dir = scratch(name);
    let source = dir.join("app.rs");
    fs::write(&source, "fn main() {}\n").expect("can write the fake source");
    let binary = dir.join("clueless");
    let output = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("rustc is on the path");
    assert!(
        output.status.success(),
        "rustc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

/// Run `cargo xtask <args>` from the repository root with the app build
/// skipped and a stand-in binary, as `bundle` invocation context.
fn cargo_xtask(args: &[&str], fake: &Path, out: &Path) -> Output {
    let mut full = args.to_vec();
    full.push("--binary");
    full.push(fake.to_str().expect("utf-8 scratch path"));
    full.push("--out");
    full.push(out.to_str().expect("utf-8 scratch path"));
    Command::new("cargo")
        .arg("xtask")
        .args(&full)
        .current_dir(workspace_root())
        .env("CLUELESS_XTASK_SKIP_BUILD", "1")
        .output()
        .expect("cargo is on the path")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn bundled_executable(out: &Path) -> PathBuf {
    out.join("clueless.app")
        .join("Contents")
        .join("MacOS")
        .join("clueless")
}

fn bundled_plist(out: &Path) -> PathBuf {
    out.join("clueless.app").join("Contents").join("Info.plist")
}

/// Bundle a stand-in binary with the dash identity and return the bundle dir.
fn bundle_with_identity(test: &str, identity: &str) -> PathBuf {
    let fake = fake_binary(test);
    let out = scratch(&format!("{test}-out"));
    let output = cargo_xtask(&["bundle", "--identity", identity], &fake, &out);
    assert!(
        output.status.success(),
        "cargo xtask bundle --identity {identity} failed: {}",
        text(&output)
    );
    out
}

#[test]
fn bundle_with_dash_identity_produces_an_adhoc_signed_bundle() {
    let out = bundle_with_identity("dash", "-");

    assert!(
        bundled_executable(&out).is_file(),
        "bundle holds the executable"
    );
    assert!(bundled_plist(&out).is_file(), "bundle holds Info.plist");

    let check = Command::new("codesign")
        .arg("-dv")
        .arg(out.join("clueless.app"))
        .output()
        .expect("codesign is on the path");
    let report = text(&check);
    assert!(
        check.status.success(),
        "codesign -dv rejected the bundle: {report}"
    );
    assert!(
        report.contains("Signature=adhoc"),
        "codesign -dv does not report an ad-hoc signature: {report}"
    );
}

#[test]
fn bundle_with_unknown_identity_fails_naming_the_identity() {
    let fake = fake_binary("unknown-id");
    let out = scratch("unknown-id-out");
    let output = cargo_xtask(
        &["bundle", "--identity", "no-such-identity-xyz"],
        &fake,
        &out,
    );
    let report = text(&output);
    assert!(
        !output.status.success(),
        "bundle with a missing identity must not succeed: {report}"
    );
    assert!(
        report.contains("no-such-identity-xyz"),
        "the error must name the identity: {report}"
    );
}

#[test]
fn bundled_info_plist_passes_plutil_lint() {
    let out = bundle_with_identity("lint", "-");
    let lint = Command::new("plutil")
        .arg("-lint")
        .arg(bundled_plist(&out))
        .output()
        .expect("plutil is on the path");
    let report = text(&lint);
    assert!(lint.status.success(), "plutil -lint failed: {report}");
    assert!(report.contains("OK"), "plutil did not say OK: {report}");
}

#[test]
fn bundled_plist_has_the_four_usage_descriptions_and_is_an_agent() {
    let out = bundle_with_identity("keys", "-");
    let plist = bundled_plist(&out);

    let element = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :LSUIElement"])
        .arg(&plist)
        .output()
        .expect("PlistBuddy ships with macOS");
    assert!(
        element.status.success(),
        "LSUIElement is missing: {}",
        text(&element)
    );
    assert_eq!(
        String::from_utf8_lossy(&element.stdout).trim(),
        "true",
        "LSUIElement must be true"
    );

    for key in [
        "NSMicrophoneUsageDescription",
        "NSAudioCaptureUsageDescription",
        "NSScreenCaptureUsageDescription",
        "NSLocalNetworkUsageDescription",
    ] {
        let query = format!("Print :{key}");
        let value = Command::new("/usr/libexec/PlistBuddy")
            .arg("-c")
            .arg(&query)
            .arg(&plist)
            .output()
            .expect("PlistBuddy ships with macOS");
        let value_text = String::from_utf8_lossy(&value.stdout).trim().to_string();
        assert!(
            value.status.success(),
            "{key} is missing from the bundled plist: {}",
            text(&value)
        );
        assert!(
            value_text.split_whitespace().count() >= 5,
            "{key} should be a plain sentence, got {value_text:?}"
        );
    }
}

#[test]
fn make_dev_cert_dry_run_prints_the_commands_without_touching_the_keychain() {
    let home = scratch("dry-run-home");
    let output = Command::new("bash")
        .arg(workspace_root().join("scripts").join("make-dev-cert.sh"))
        .arg("--dry-run")
        .current_dir(&home)
        .env("HOME", &home)
        .output()
        .expect("bash is on the path");
    let report = text(&output);
    assert!(output.status.success(), "dry run failed: {report}");
    assert!(
        report.contains("clueless-dev"),
        "names the identity: {report}"
    );
    assert!(
        report.contains("openssl req"),
        "prints the openssl command: {report}"
    );
    assert!(
        report.contains("security import"),
        "prints the keychain import command: {report}"
    );
    assert!(
        report.contains("security add-trusted-cert"),
        "names the one trust command: {report}"
    );
    assert!(
        fs::read_dir(&home)
            .expect("home is readable")
            .next()
            .is_none(),
        "the dry run left files behind in the throwaway HOME"
    );
}
