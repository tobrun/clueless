# Development

## Requirements

macOS 14.6 or later (the ScreenCaptureKit path and the process-tap API).
Rust is pinned in `rust-toolchain.toml` (1.94.1 with rustfmt and clippy); rustup installs it on first build.

## Build and run

```sh
cargo build                          # workspace
scripts/make-dev-cert.sh             # once: self-signed cert so permission grants survive rebuilds
cargo xtask run                      # bundle + codesign + open the dev app
```

`cargo xtask run` exits 1 and prints the startup error when the app dies during startup, for example on an invalid `config.toml`.

`cargo xtask bundle` assembles `target/debug/clueless.app` (binary, `Info.plist`, icon) and signs it with the `clueless-dev` identity.
Options: `--identity ID` (`-` for ad-hoc signing), `--out DIR`, `--binary PATH`.
Ad-hoc signing works but macOS treats each rebuild as a new app and drops permission grants.

## Validation commands

These are the gates, also run in CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask bundle --identity -
```

Checks that need a real desktop session (permissions, overlay behavior, live audio) are a checklist: [manual-checks.md](manual-checks.md).

## Test layout

- Unit tests live next to the code in `src` modules.
- Integration tests per crate in `crates/*/tests/`, run against a local axum mock server that speaks the OpenAI-compatible routes.
- `crates/app/tests/replay.rs` drives the full pipeline from WAV fixtures through mock servers (unit level, no network) and holds the live e2e replay suite.
- `crates/types/tests/purity.rs` fails the build when a pure crate (types, segmenter, asr, llm, context, engine, trace) pulls in a macOS-only dependency; the rules are in [dependencies.md](dependencies.md).
- Binary tests never record into the real `~/.clueless`: without `--data-dir` the trace sink is a no-op in tests, and tests that exercise recording pass `--data-dir` a temp directory.

### Live tests

The e2e suites in `crates/app/tests/replay.rs`, `crates/llm/tests/live.rs` and `crates/asr/tests/live.rs` run against real servers and are `#[ignore]`d by default.
To run them, put working `LLM_*` and `ASR_*` values in `.env` (or the environment) and:

```sh
LIVE_SERVER=1 cargo test --workspace -- --ignored
```

`LIVE_SERVER` unset means each live test returns early or stays ignored; CI never sets it.

## Fixtures

`fixtures/` holds generated 16 kHz mono WAVs (English, Dutch, French sentences plus conversation mixes) and `fixtures/expected/` the words the tests assert on.
Regenerate with `scripts/make-fixtures.sh` (uses macOS voices, no network).

## Scripts

| Script | Purpose |
| --- | --- |
| `scripts/make-dev-cert.sh` | create the `clueless-dev` codesigning identity |
| `scripts/make-fixtures.sh` | regenerate `fixtures/` |
| `scripts/smoke-llm.sh` | hit the configured `LLM_*` server once and show the streamed reply |
| `scripts/smoke-asr.sh` | transcribe a WAV through the configured `ASR_*` server |

## Session traces from fixtures

Every mode run writes (and reads) its traces in one data directory, so you can build a corpus of fixture sessions and measure a change against it without touching `~/.clueless`:

```sh
TMP=/tmp/clueless-traces
# record a session by replaying the fixtures through the current build
cargo run -p clueless -- --replay fixtures/conv_me.wav fixtures/conv_them.wav --speed 4 --ask --data-dir $TMP
# after changing the segmenter, prompt or model: re-run the session's audio and recorded presses
SESSION=$(basename "$(ls -d $TMP/sessions/* | tail -1)")
cargo run -p clueless -- --replay-session "$SESSION" --speed 4 --data-dir $TMP
# diff the two traces and let the server judge each suggestion pair
cargo run -p clueless -- --compare "$SESSION" --data-dir $TMP
```

The re-run lands in `$TMP/sessions/$SESSION/runs/`; `--sessions`, `--show` and `--delete` list, print and clean up there too.
The format is [trace-format.md](trace-format.md).

## Repo layout

Eleven crates in one cargo workspace; the dependency edges and the pure-crate rule are in [dependencies.md](dependencies.md), the reasoning for the layout in [decisions.md](decisions.md).
The newest member is `crates/trace/`, which owns the session trace format: record types, sink traits, disk writer, reader, compare, list and show; it depends only on types.

## CI

`.github/workflows/ci.yml` runs the validation commands on `macos-15` for pushes and pull requests.
Live tests stay ignored there.
