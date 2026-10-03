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
- `crates/types/tests/purity.rs` fails the build when a pure crate (types, segmenter, asr, llm, context, engine) pulls in a macOS-only dependency; the rules are in [dependencies.md](dependencies.md).

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

## Repo layout

Ten crates in one cargo workspace; the dependency edges and the pure-crate rule are in [dependencies.md](dependencies.md), the reasoning for the layout in [decisions.md](decisions.md).

## CI

`.github/workflows/ci.yml` runs the validation commands on `macos-15` for pushes and pull requests.
Live tests stay ignored there.
