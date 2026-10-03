# clueless

A meeting copilot for macOS that runs entirely on your own infrastructure.
It transcribes your meetings live in a floating overlay - your microphone as `Me`, the computer's audio as `Them` - and on a hotkey asks a language model what you could say next, streaming the answer into the panel.
No account, no telemetry, nothing written to disk: the transcript lives in memory and dies with the app.
A floating panel shows both transcript streams as they finalize, with suggestions streaming in beneath them.

You bring two OpenAI-compatible servers (a chat model and a speech-to-text model, local or hosted) and clueless pipes everything through them.

## How it works

```
microphone ──┐                                  ┌─► transcript store ─► overlay panel
             ├─► ring buffer ─► VAD + segmenter ─┤
system audio ┘        │           (per stream)   └─► ASR server (OpenAI-compatible)
                      │
hotkey ─► prompt builder (transcript + profile) ─► LLM server (streaming) ─► overlay panel
```

Speech detection and utterance cutting run on your Mac; only audio segments and prompt text leave it, straight to the servers you configured.
The full design is in [docs/architecture.md](docs/architecture.md).

## Requirements

- macOS 14.6 or later (Apple Silicon or Intel).
- Rust, installed by rustup from the pinned [rust-toolchain.toml](rust-toolchain.toml) on first build.
- Two HTTP servers with OpenAI-compatible endpoints, or one server offering both:
  - chat: `POST /v1/chat/completions` (streaming)
  - transcription: `POST /v1/audio/transcriptions`
  - health: `GET /v1/models`

Any vLLM or llama.cpp deployment, a local Parakeet/Whisper server, or a hosted API works; the exact contract is [docs/servers.md](docs/servers.md).

## Quick start

```sh
git clone https://github.com/tobrun/clueless
cd clueless
cp .env.example .env    # then edit: the server URLs and model ids are the only must-fills
scripts/make-dev-cert.sh  # once: keeps permission grants across rebuilds
cargo xtask run           # builds, bundles, signs and opens the app
```

Grant Microphone and Screen Recording when macOS asks (Screen Recording needs one app restart after granting; see [docs/troubleshooting.md](docs/troubleshooting.md)).
Then pick "Start Meeting" in the menu bar icon, talk, and press `Cmd+Enter` for a suggestion.

## Configuration

Everything lives in two small files, both documented in [docs/configuration.md](docs/configuration.md):

- `.env` - servers, models, API keys: `LLM_BASE_URL`, `LLM_MODEL`, `ASR_BASE_URL`, `ASR_MODEL` plus optional keys and tuning knobs.
- `~/.config/clueless/config.toml` - optional app behavior: audio backends, voice-detection thresholds, overlay and hotkey settings. A missing file means all defaults; [config.example.toml](config.example.toml) lists every key.

## Hotkeys

| Keys | Action | Active |
| --- | --- | --- |
| `Cmd+Enter` | ask for a suggestion | during a meeting |
| `Cmd+Shift+X` | clear the panel | during a meeting |
| `Cmd+Shift+Arrow` | move the panel | during a meeting |
| `Cmd+Shift+R` | start or stop the meeting | always |
| `Cmd+\` | show or hide the overlay | always |
| `Cmd+Shift+M` | toggle click-through | always |

All of them are remappable, and meeting-only keys are unregistered outside meetings so they do not steal combinations from other apps all day.

## Replay mode

Run the whole pipeline headlessly over WAV files, against the servers in your `.env` - handy for trying a different model without a meeting:

```sh
cargo run -p clueless -- --replay fixtures/conv_me.wav fixtures/conv_them.wav --speed 4 --ask
```

It prints each transcribed utterance as it finalizes and, with `--ask`, a streamed suggestion at the end.

## Permissions

clueless asks for Microphone (input) and Screen Recording (for the system-audio stream).
Grants follow the app's code signature, which is why `scripts/make-dev-cert.sh` exists.
The app needs no network access beyond the servers you point it at.

## Limitations

- Nothing persists: closing the app loses the transcript; there is no history or export yet.
- `hide_from_capture` (keeping the panel out of screen shares) is best effort; macOS 15.4+ ignores it for full-screen shares.
- The shipped bundle is a debug build signed with a self-signed dev certificate; there is no notarized release.
- Speaker diarization inside the Them stream is not attempted - it is one rolling `Them` voice.

## Use it responsibly

You are responsible for following the recording-consent laws that apply where you are, which in several places require informing or getting consent from everyone in the call.
The app shows no indication to other participants.

## Docs

The full index is [docs/README.md](docs/README.md): [configuration](docs/configuration.md), [servers](docs/servers.md), [architecture](docs/architecture.md), [development](docs/development.md), [troubleshooting](docs/troubleshooting.md), [design decisions](docs/decisions.md).

## License

MIT, see [LICENSE](LICENSE).
