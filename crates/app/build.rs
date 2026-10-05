//! Link flag so the binary finds the Swift runtime libraries at run time
//! (ScreenCaptureKit's async bridges pull in Swift symbols), plus the git
//! commit the trace manifest pins the build to (D-manifest-contents).

fn main() {
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    emit_git_commit();
}

/// Exports `CLUELESS_GIT_COMMIT` as the short hash, with `-dirty` when the
/// tree has changes and `unknown` when git cannot answer. Re-runs whenever
/// HEAD or the index move, so a commit or a staged edit refreshes the value.
fn emit_git_commit() {
    if let Some(git_dir) = git_output(&["rev-parse", "--git-dir"]) {
        for watch in ["HEAD", "index"] {
            println!("cargo:rerun-if-changed={git_dir}/{watch}");
        }
    }
    let commit = match git_output(&["rev-parse", "--short", "HEAD"]) {
        Some(hash) => {
            let dirty = git_output(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
            if dirty { format!("{hash}-dirty") } else { hash }
        }
        None => "unknown".to_string(),
    };
    println!("cargo:rustc-env=CLUELESS_GIT_COMMIT={commit}");
}

/// Runs git in the package directory and returns trimmed stdout, or `None`
/// when git is missing, fails or prints nothing.
fn git_output(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}
