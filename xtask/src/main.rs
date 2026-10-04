//! Dev tasks for the clueless MVP.
//!
//! ```text
//! cargo xtask bundle [--identity ID] [--out DIR]
//! cargo xtask run
//! ```
//!
//! `bundle` builds the app, assembles `clueless.app` and signs it, by default
//! with the self-signed `clueless-dev` identity created once by
//! `scripts/make-dev-cert.sh`, so macOS permission grants survive rebuilds
//! (a stable signature is what keeps the grants attached to the app).
//! `run` bundles, makes sure a root `.env` exists and launches the bundle
//! with `open`, so the app itself is responsible for its permission prompts.
//! `open` neither shows the app's stderr nor passes on its exit code, so
//! `run` reads back what the app appended to its log during the launch and
//! fails with the message when the app logged a startup failure.
//!
//! One environment variable exists for the integration tests, which must not
//! build the sibling crates: `CLUELESS_XTASK_SKIP_BUILD=1` skips
//! `cargo build -p clueless` and bundles the binary that is already in
//! `target/debug`.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_IDENTITY: &str = "clueless-dev";
const ENV_EXAMPLE: &str = ".env.example";
const ENV_FILE: &str = ".env";
/// What the app logs in front of every startup failure message
/// (C-startup-failure-marker in docs/contracts.md).
const STARTUP_FAILURE_MARKER: &str = "startup failed: ";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("bundle") => cmd_bundle(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        _ => {
            eprintln!(
                "usage: cargo xtask bundle [--identity ID] [--out DIR] [--binary PATH] | cargo xtask run"
            );
            eprintln!("  --identity ID   codesign identity to use; \"-\" means ad-hoc");
            Err("no valid subcommand given".to_string())
        }
    };
    if let Err(message) = result {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

/// Which codesign identity `bundle` should use.
enum Signing {
    /// The default identity when it exists, ad-hoc with a warning when not.
    Default,
    /// Exactly this identity; a missing one is an error.
    Identity(String),
    /// `codesign --sign -`, chosen on purpose.
    AdHoc,
}

struct BundleOptions {
    signing: Signing,
    /// Where `clueless.app` is assembled. Defaults to `target/debug`.
    out: Option<PathBuf>,
    /// The app binary to bundle. Defaults to `target/debug/clueless`.
    binary: Option<PathBuf>,
}

fn cmd_bundle(args: &[String]) -> Result<(), String> {
    let options = parse_bundle_args(args)?;
    let identity = resolve_identity(&options.signing)?;
    build_app()?;
    let binary = match &options.binary {
        Some(path) => path.clone(),
        None => target_debug_dir()?.join("clueless"),
    };
    let out = match &options.out {
        Some(dir) => dir.clone(),
        None => target_debug_dir()?,
    };
    let bundle = assemble_bundle(&binary, &out)?;
    sign_bundle(&bundle, identity.as_deref())?;
    println!("bundled {}", bundle.display());
    Ok(())
}

fn cmd_run(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(format!("run takes no arguments, got {args:?}"));
    }
    cmd_bundle(&[])?;
    let root = workspace_root()?;
    let env_file = root.join(ENV_FILE);
    if !env_file.exists() {
        let example = root.join(ENV_EXAMPLE);
        fs::copy(&example, &env_file).map_err(|error| {
            format!(
                "could not copy {} to {}: {error}",
                example.display(),
                env_file.display()
            )
        })?;
        println!("copied {ENV_EXAMPLE} to {ENV_FILE}; edit it to point at your servers");
    }
    // An app launched with `open` inherits neither the shell's cwd nor its
    // environment, so point it at this workspace's files explicitly.
    let config = root.join("config.toml");
    let mut open = Command::new("open");
    open.arg("-W")
        .arg(target_debug_dir()?.join("clueless.app"))
        .arg("--args")
        .arg("--env-file")
        .arg(&env_file);
    if config.is_file() {
        open.arg("--config").arg(&config);
    }
    let log = log_file_path();
    let log_len_before = fs::metadata(&log).map_or(0, |meta| meta.len());
    let status = open
        .status()
        .map_err(|error| format!("could not run open: {error}"))?;
    if !status.success() {
        return Err(format!("open -W clueless.app exited with {status}"));
    }
    if let Some(message) = startup_failure_since(&log, log_len_before) {
        return Err(startup_failure_error(&log, &message));
    }
    println!("log file: {}", log.display());
    Ok(())
}

/// The error `run` ends with when the app logged a startup failure.
fn startup_failure_error(log: &Path, message: &str) -> String {
    format!(
        "the app exited during startup; the log ({}) says:\n{message}",
        log.display()
    )
}

/// The text from the startup failure marker to the end of what the app
/// appended to `log` after it had `offset` bytes; `None` when nothing
/// appended carries the marker or the log cannot be read. A log shorter
/// than `offset` was replaced, so all of it counts as new.
fn startup_failure_since(log: &Path, offset: u64) -> Option<String> {
    let bytes = fs::read(log).ok()?;
    let start = usize::try_from(offset)
        .ok()
        .filter(|start| *start <= bytes.len())
        .unwrap_or(0);
    let appended = String::from_utf8_lossy(&bytes[start..]);
    let at = appended.find(STARTUP_FAILURE_MARKER)?;
    Some(appended[at..].trim_end().to_string())
}

fn parse_bundle_args(args: &[String]) -> Result<BundleOptions, String> {
    let mut signing = Signing::Default;
    let mut out: Option<PathBuf> = None;
    let mut binary: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        let value = |name: &str| -> Result<String, String> {
            args.get(index + 1)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match args[index].as_str() {
            "--identity" => {
                let id = value("--identity")?;
                signing = if id == "-" {
                    Signing::AdHoc
                } else {
                    Signing::Identity(id)
                };
                index += 2;
            }
            "--out" => {
                out = Some(PathBuf::from(value("--out")?));
                index += 2;
            }
            "--binary" => {
                binary = Some(PathBuf::from(value("--binary")?));
                index += 2;
            }
            other => return Err(format!("unknown bundle argument {other:?}")),
        }
    }
    Ok(BundleOptions {
        signing,
        out,
        binary,
    })
}

/// Decide the identity string for `codesign --sign`, or `None` for ad-hoc.
fn resolve_identity(signing: &Signing) -> Result<Option<String>, String> {
    match signing {
        Signing::AdHoc => Ok(None),
        Signing::Identity(id) => {
            let names = codesigning_identities()?;
            if names.iter().any(|name| name == id) {
                Ok(Some(id.clone()))
            } else {
                Err(format!(
                    "codesigning identity {id:?} was not found in the login keychain; \
                     create it with scripts/make-dev-cert.sh or pass --identity - for ad-hoc \
                     signing"
                ))
            }
        }
        Signing::Default => {
            let names = codesigning_identities()?;
            if names.iter().any(|name| name == DEFAULT_IDENTITY) {
                return Ok(Some(DEFAULT_IDENTITY.to_string()));
            }
            eprintln!(
                "warning: codesigning identity {DEFAULT_IDENTITY:?} not found in the login \
                 keychain; signing ad-hoc instead.\n\
                 warning: an ad-hoc signature changes every build, so macOS permission grants \
                 will not survive rebuilds.\n\
                 warning: run scripts/make-dev-cert.sh once to fix this."
            );
            Ok(None)
        }
    }
}

/// The names of the valid codesigning identities in the keychain.
fn codesigning_identities() -> Result<Vec<String>, String> {
    let output = Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .map_err(|error| format!("could not run security find-identity: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "security find-identity failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut names = Vec::new();
    for line in stdout.lines() {
        // A real entry looks like: `  1) ABCDEF...1234 "Clueless Dev"`.
        let Some(rest) = line.split(") ").nth(1) else {
            continue;
        };
        let Some(start) = rest.find('"') else {
            continue;
        };
        let Some(end) = rest.rfind('"') else { continue };
        if start < end {
            names.push(rest[start + 1..end].to_string());
        }
    }
    Ok(names)
}

fn build_app() -> Result<(), String> {
    if env::var_os("CLUELESS_XTASK_SKIP_BUILD").is_some() {
        println!("skipping cargo build -p clueless (CLUELESS_XTASK_SKIP_BUILD is set)");
        return Ok(());
    }
    let status = Command::new(cargo_bin())
        .args(["build", "-p", "clueless"])
        .current_dir(workspace_root()?)
        .status()
        .map_err(|error| format!("could not run cargo: {error}"))?;
    if !status.success() {
        return Err("cargo build -p clueless failed".to_string());
    }
    Ok(())
}

/// Copy the binary and the plist into `out/clueless.app`.
fn assemble_bundle(binary: &Path, out: &Path) -> Result<PathBuf, String> {
    if !binary.is_file() {
        return Err(format!(
            "{} does not exist; run cargo build -p clueless first",
            binary.display()
        ));
    }
    let plist_source = workspace_root()?.join("macos").join("Info.plist");
    let bundle = out.join("clueless.app");
    let macos_dir = bundle.join("Contents").join("MacOS");
    fs::create_dir_all(&macos_dir)
        .map_err(|error| format!("could not create {}: {error}", macos_dir.display()))?;
    fs::copy(binary, macos_dir.join("clueless"))
        .map_err(|error| format!("could not copy the binary into the bundle: {error}"))?;
    fs::copy(&plist_source, bundle.join("Contents").join("Info.plist"))
        .map_err(|error| format!("could not copy Info.plist into the bundle: {error}"))?;
    Ok(bundle)
}

fn sign_bundle(bundle: &Path, identity: Option<&str>) -> Result<(), String> {
    let executable = bundle.join("Contents").join("MacOS").join("clueless");
    let label = identity.unwrap_or("-");
    let status = Command::new("codesign")
        .args(["--force", "--sign"])
        .arg(label)
        .arg(&executable)
        .status()
        .map_err(|error| format!("could not run codesign: {error}"))?;
    if !status.success() {
        let mut message = format!("codesign --sign {label} failed on {}", executable.display());
        if let Some(id) = identity {
            let _ = write!(
                message,
                "; if the identity {id:?} is rejected as untrusted, trust its certificate \
                 once with: security add-trusted-cert -r trustRoot -k \
                 \"$HOME/Library/Keychains/login.keychain-db\" \
                 \"$HOME/Library/Application Support/clueless-dev-cert/clueless-dev.pem\""
            );
        }
        return Err(message);
    }
    Ok(())
}

fn cargo_bin() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// The repository root, from xtask's location in the workspace.
fn workspace_root() -> Result<PathBuf, String> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is a direct member of the workspace")
        .to_path_buf())
}

fn target_debug_dir() -> Result<PathBuf, String> {
    let from_env = env::var_os("CARGO_TARGET_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let root = match from_env {
        Some(dir) if dir.is_absolute() => dir,
        Some(dir) => workspace_root()?.join(dir),
        None => workspace_root()?.join("target"),
    };
    Ok(root.join("debug"))
}

/// Where the app writes its log file (the default the app itself uses).
fn log_file_path() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Logs")
        .join("clueless")
        .join("clueless.log")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory under the workspace `target`, one per process.
    fn unit_scratch() -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("xtask-unit-tests")
            .join(std::process::id().to_string());
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn log_with(name: &str, content: &str) -> PathBuf {
        let dir = unit_scratch();
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        fs::write(&path, content).expect("log file");
        path
    }

    #[test]
    fn a_failure_appended_after_the_offset_is_returned_from_the_marker() {
        let old = "2026-10-03T15:50:12Z  INFO overlay::ui: engine idle\n";
        let new = "2026-10-04T07:00:00Z ERROR clueless: startup failed: invalid config: x\n";
        let log = log_with("appended.log", &format!("{old}{new}"));
        assert_eq!(
            startup_failure_since(&log, old.len() as u64).as_deref(),
            Some("startup failed: invalid config: x")
        );
    }

    #[test]
    fn a_failure_from_an_earlier_run_does_not_fail_this_launch() {
        let old = "2026-10-03T15:50:12Z ERROR clueless: startup failed: old\n";
        let log = log_with("earlier.log", old);
        assert_eq!(startup_failure_since(&log, old.len() as u64), None);
    }

    #[test]
    fn a_log_shorter_than_the_offset_is_read_from_the_start() {
        let log = log_with("replaced.log", "ERROR clueless: startup failed: fresh\n");
        assert_eq!(
            startup_failure_since(&log, 10_000).as_deref(),
            Some("startup failed: fresh")
        );
    }

    #[test]
    fn the_error_names_the_log_path_and_the_message() {
        let error = startup_failure_error(Path::new("/x/clueless.log"), "startup failed: boom");
        assert!(error.contains("/x/clueless.log"), "{error}");
        assert!(error.contains("startup failed: boom"), "{error}");
    }

    #[test]
    fn a_missing_log_is_not_a_failure() {
        let missing = unit_scratch().join("no-such.log");
        assert_eq!(startup_failure_since(&missing, 0), None);
    }

    #[test]
    fn a_multi_line_message_is_returned_whole() {
        let log = log_with(
            "multi.log",
            "2026-10-04T07:00:00Z ERROR clueless: startup failed: invalid config:\n  first problem\n  second problem\n",
        );
        assert_eq!(
            startup_failure_since(&log, 0).as_deref(),
            Some("startup failed: invalid config:\n  first problem\n  second problem")
        );
    }

    #[test]
    fn a_normal_start_with_many_runtime_lines_is_not_a_failure() {
        let noise =
            "2026-10-04T07:00:00Z  INFO ort::logging: Saving initialized tensors.\n".repeat(2_000);
        let log = log_with("normal.log", &noise);
        assert_eq!(startup_failure_since(&log, 0), None);
    }
}
