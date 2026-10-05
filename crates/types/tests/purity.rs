//! The purity rule from docs/dependencies.md: the six pure crates must not
//! depend on any macOS-only crate, checked over the real dependency graph.

use std::process::Command;

const PURE_CRATES: &[&str] = &[
    "clueless-types",
    "segmenter",
    "asr",
    "llm",
    "context",
    "trace",
    "engine",
];

fn is_forbidden(name: &str) -> bool {
    name.starts_with("objc2")
        || matches!(
            name,
            "cpal" | "screencapturekit" | "global-hotkey" | "dispatch2" | "block2"
        )
}

/// Names of every package in `cargo tree` output for one crate (all targets,
/// normal + build edges; dev edges are excluded with --no-dev-dependencies).
fn tree_names(krate: &str) -> Vec<String> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace = workspace_root(manifest_dir);
    let out = Command::new("cargo")
        .args([
            "tree",
            "-p",
            krate,
            "-e",
            "normal,build",
            "--prefix",
            "none",
        ])
        .current_dir(&workspace)
        .output()
        .expect("cargo tree should run");
    assert!(
        out.status.success(),
        "cargo tree failed for {krate}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.split(' ')
                .next()
                .unwrap_or("")
                .trim_end_matches("(*)")
                .to_string()
        })
        .filter(|n| !n.is_empty())
        .collect()
}

fn workspace_root(manifest_dir: &str) -> std::path::PathBuf {
    // crates/types -> workspace root
    std::path::Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn pure_crates_have_no_macos_dependency() {
    for krate in PURE_CRATES {
        let names = tree_names(krate);
        let bad: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|n| is_forbidden(n))
            .collect();
        assert!(
            bad.is_empty(),
            "pure crate {krate} depends on macOS-only crates: {bad:?}"
        );
        assert!(
            names.iter().any(|n| n == *krate),
            "cargo tree for {krate} listed nothing"
        );
    }
}
