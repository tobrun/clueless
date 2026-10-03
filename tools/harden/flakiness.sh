#!/usr/bin/env bash
# flakiness.sh - repo-fitted flakiness detector for the clueless workspace.
#
# Usage: tools/harden/flakiness.sh N <test-name> [<test-name> ...]
#
# Runs each named test N times and compares every repeat against the first
# run. Any repeat that disagrees with the first run marks the test FLAKY.
# A test that fails on every run is reported as FAILING (also a non-zero
# exit). Tests are located by grepping the function name under crates/*/src
# (unit tests, run with --lib) and the top-level files of crates/*/tests
# (integration tests, run with --test <target>), so each repeat is scoped
# to the owning package with `-p` and filtered with `--exact` for speed.
#
# Exit codes:
#   0  every test passed on every repeat
#   1  at least one test was flaky or failed consistently
#   2  usage error (bad args, or a test name could not be found)
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT" || exit 2

usage() {
  cat >&2 <<'EOF'
Usage: tools/harden/flakiness.sh N <test-name> [<test-name> ...]

  N                     number of repeats per test (positive integer, required)
  <test-name>           test function name, e.g. all_ten_default_hotkeys_parse

Runs `<test-name>` N times via cargo test, scoped to the owning crate.
Exit: 0 = all stable+passing, 1 = flaky or consistently failing, 2 = usage error.
EOF
}

if [[ $# -lt 2 ]]; then
  echo "error: expected at least 2 arguments (N and one test name)" >&2
  usage
  exit 2
fi

N="$1"
shift
if ! [[ "$N" =~ ^[0-9]+$ ]] || [[ "$N" -lt 1 ]]; then
  echo "error: N must be a positive integer, got '$N'" >&2
  usage
  exit 2
fi

# package name declared in a crate's Cargo.toml
pkg_name_for_dir() {
  sed -n '/^\[package\]/,/^\[/p' "$1/Cargo.toml" \
    | grep -m1 '^name *=' \
    | sed -E 's/^name *= *"([^"]*)".*/\1/'
}

# Resolve a bare test function name to: package|lib|target|full-test-path
# target is "-" for unit tests (--lib). Prints nothing if not found.
# Uses `cargo test -- --list` to recover the full module path that --exact needs.
resolve_test() {
  local name="$1" hit crate_dir pkg kind target file full

  # Integration tests: top-level files of crates/*/tests/*.rs
  hit="$(grep -rlE "fn[[:space:]]+${name}[[:space:]]*\(" crates/*/tests/*.rs 2>/dev/null | head -n1 || true)"
  kind="test"
  if [[ -n "$hit" ]]; then
    crate_dir="$(dirname "$(dirname "$hit")")"
    target="$(basename "$hit" .rs)"
  else
    # Unit tests: anywhere under crates/*/src
    hit="$(grep -rlE "fn[[:space:]]+${name}[[:space:]]*\(" --include='*.rs' crates/*/src 2>/dev/null | head -n1 || true)"
    [[ -n "$hit" ]] || return 1
    kind="lib"
    crate_dir="$(dirname "$(dirname "$hit")")"
    target="-"
  fi

  pkg="$(pkg_name_for_dir "$crate_dir")"
  [[ -n "$pkg" ]] || return 1

  if [[ "$target" == "-" ]]; then
    full="$(cargo test -p "$pkg" --lib -- --list 2>/dev/null \
      | grep -E "(^|::)${name}: test$" | head -n1 | sed 's/: test$//' || true)"
  else
    full="$(cargo test -p "$pkg" --test "$target" -- --list 2>/dev/null \
      | grep -E "(^|::)${name}: test$" | head -n1 | sed 's/: test$//' || true)"
  fi
  [[ -n "$full" ]] || return 1

  printf '%s|%s|%s|%s\n' "$pkg" "$kind" "$target" "$full"
}

# Run one test once; echo combined output, return its exit status.
run_test_once() {
  local pkg="$1" kind="$2" target="$3" full="$4"
  if [[ "$kind" == "lib" ]]; then
    cargo test -p "$pkg" --lib -- --exact "$full" --nocapture 2>&1
  else
    cargo test -p "$pkg" --test "$target" -- --exact "$full" --nocapture 2>&1
  fi
}

overall=0
for name in "$@"; do
  resolved="$(resolve_test "$name")"
  if [[ -z "$resolved" ]]; then
    echo "error: test '$name' not found under crates/*/src or crates/*/tests" >&2
    exit 2
  fi
  IFS='|' read -r pkg kind target full <<<"$resolved"
  if [[ "$full" != "$name" ]]; then
    echo "==> ${name} -> ${pkg} :: ${full} (running ${N}x)" >&2
  else
    echo "==> ${pkg} :: ${name} (running ${N}x)" >&2
  fi

  results=()
  for ((i = 1; i <= N; i++)); do
    out="$(run_test_once "$pkg" "$kind" "$target" "$full")"
    rc=$?
    if [[ $rc -eq 0 ]]; then
      results+=("pass")
    else
      results+=("fail")
      # keep the failure visible for diagnosis
      printf '%s\n' "$out" >&2
    fi
  done

  passes=0
  for r in "${results[@]}"; do
    [[ "$r" == "pass" ]] && passes=$((passes + 1))
  done

  if [[ $passes -eq $N ]]; then
    echo "PASS(${passes}/${N}) ${name}"
  elif [[ $passes -eq 0 ]]; then
    echo "FAIL(${passes}/${N}) ${name}"
    overall=1
  else
    mismatches=()
    first="${results[0]}"
    for ((i = 0; i < N; i++)); do
      if [[ "${results[$i]}" != "$first" ]]; then
        mismatches+=("run-$((i + 1))-${results[$i]}")
      fi
    done
    echo "FLAKY(${passes}/${N}) ${name} ${mismatches[*]}"
    overall=1
  fi
done

exit "$overall"
