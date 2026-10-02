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
//! `run` bundles, makes sure a root config exists and launches the bundle
//! with `open`, so the app itself is responsible for its permission prompts.
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
const CONFIG_EXAMPLE: &str = "config.example.toml";
const CONFIG_FILE: &str = "config.toml";

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
    let config = root.join(CONFIG_FILE);
    if !config.exists() {
        let example = root.join(CONFIG_EXAMPLE);
        fs::copy(&example, &config).map_err(|error| {
            format!(
                "could not copy {} to {}: {error}",
                example.display(),
                config.display()
            )
        })?;
        println!("copied {CONFIG_EXAMPLE} to {CONFIG_FILE}");
    }
    let bundle = target_debug_dir()?.join("clueless.app");
    let status = Command::new("open")
        .arg("-W")
        .arg(&bundle)
        .arg("--args")
        .arg("--config")
        .arg(&config)
        .status()
        .map_err(|error| format!("could not run open: {error}"))?;
    if !status.success() {
        return Err(format!("open -W {} exited with {status}", bundle.display()));
    }
    println!("log file: {}", log_file_path().display());
    Ok(())
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
    let state_dir = env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home_dir().join(".local").join("state"), PathBuf::from);
    state_dir.join("clueless").join("clueless.log")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}
