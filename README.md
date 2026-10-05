# clueless

A meeting copilot for macOS that runs entirely on your own infrastructure.
It transcribes your meetings live - your microphone as `Me`, the computer's audio as `Them` - and on a hotkey asks a language model what you could say next, streaming the answer into the window.
No account, no telemetry: the app talks only to the two servers you configure.
Every meeting is kept on disk as a trace under `~/.clueless` - the transcript and every model call, plus both audio tracks when you switch `[trace] audio` on - so you can inspect it, re-run it through a newer build and compare the two; turn this off with `[trace] enabled = false` or delete a session with `clueless --delete NAME --yes`.
The window shows both transcript streams as they finalize, with suggestions streaming in beneath them.

You bring two OpenAI-compatible servers (a chat model and a speech-to-text model, local or hosted) and clueless pipes everything through them.

## How it works

```
microphone ──┐                                  ┌─► transcript store ─► overlay panel
             ├─► ring buffer ─► VAD + segmenter ─┤
system audio ┘        │           (per stream)   └─► ASR server (OpenAI-compatible)
                      │
hotkey or turn end ─► prompt builder (transcript + notes + profile) ─► LLM server (streaming) ─► overlay panel
```

Speech detection and utterance cutting run on your Mac; only audio segments and prompt text leave it, straight to the servers you configured.
The full design is in [docs/architecture.md](docs/architecture.md).

## Assist profiles

Three built-in profiles decide when the model helps.
Manual, the default, asks only when you press `Cmd+Enter`.
Interview asks at the end of a turn of the other side and helps you answer what they raised.
Brainstorm asks at every finished piece of your own speech and offers ideas while you talk.
The model answers PASS when it has nothing to say and the app shows nothing then.
Answers collect in a feed with the newest at the bottom.
Switch with `Ctrl+Alt+P` or the menu bar icon, and pick the start profile with `[assist] start_profile` in the config; see [Profiles in docs/configuration.md](docs/configuration.md#profiles).

## Window modes

The app shows one of two windows: a standard window with a title bar that you can drag and resize and that appears in screen shares, or a borderless overlay panel that is click-through and that screen capture tries not to see.
Every launch starts in the standard window; `Cmd+Shift+\` or the menu bar icon switches modes, and the overlay takes the position and size of the window's content area, which the app remembers across launches.

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
- `~/.config/clueless/config.toml` - optional app behavior: audio backends, session recording, voice-detection thresholds, overlay and hotkey settings. A missing file means all defaults; [config.example.toml](config.example.toml) lists every key.

## Hotkeys

| Keys | Action | Active |
| --- | --- | --- |
| `Cmd+Enter` | ask for a suggestion | during a meeting |
| `Cmd+Shift+X` | clear the panel | during a meeting |
| `Cmd+Shift+Arrow` | move the window on screen | during a meeting |
| `Cmd+Shift+R` | start or stop the meeting | always |
| `Cmd+\` | show or hide the window on screen | always |
| `Cmd+Shift+M` | toggle click-through | always |
| `Cmd+Shift+\` | switch between the standard window and the hidden overlay | always |
| `Ctrl+Alt+P` | cycle the assist profile | always |

All of them are remappable, and meeting-only keys are unregistered outside meetings so they do not steal combinations from other apps all day.

## Replay mode

Run the whole pipeline headlessly over WAV files, against the servers in your `.env` - handy for trying a different model without a meeting:

```sh
cargo run -p clueless -- --replay fixtures/conv_me.wav fixtures/conv_them.wav --speed 4 --ask
```

It prints each transcribed utterance as it finalizes and, with `--ask`, a streamed suggestion at the end.
Replay stays hotkey-only unless you pass `--profile`:

```sh
cargo run -p clueless -- --replay fixtures/conv_me.wav fixtures/conv_them.wav --speed 4 --profile interview
```

## Recorded sessions

Every meeting writes a trace to the data directory (default `~/.clueless`, override with `--data-dir PATH`): the transcript, the commands and hotkey presses, and every model call as JSON lines, plus the two speaker WAVs when `[trace] audio = true`.
The format is [docs/trace-format.md](docs/trace-format.md).

```sh
cargo run -p clueless -- --sessions                              # list recorded sessions, newest first
cargo run -p clueless -- --show 2026-10-05T14-03-22Z             # print one session's transcript and suggestions
cargo run -p clueless -- --replay-session 2026-10-05T14-03-22Z --speed 4
cargo run -p clueless -- --compare 2026-10-05T14-03-22Z          # the session against its newest re-run
cargo run -p clueless -- --delete 2026-10-05T14-03-22Z --yes
```

`--replay-session` feeds a recorded session's audio and recorded hotkey presses through the current build and writes the result as a run under the session's `runs/` directory (it needs a session recorded with `[trace] audio = true`).
`--compare` then diffs the two traces and asks the LLM server to judge each pair of suggestions, so a change to the segmenter, the prompt or the model can be measured against real meetings; `--no-judge` prints only the differences.

## Permissions

clueless asks for Microphone (input) and Screen Recording (for the system-audio stream).
Grants follow the app's code signature, which is why `scripts/make-dev-cert.sh` exists.
The app needs no network access beyond the servers you point it at.
With `[trace] audio = true` the microphone and system audio are additionally stored on this Mac under the data directory.

## Limitations

- Recorded sessions are kept until you delete them: there is no size limit, rotation or cleanup, and audio adds about 115 MB per recorded hour per speaker. List what you have with `clueless --sessions` and remove one with `clueless --delete NAME --yes`. Besides traces, the window's position also survives a restart (see the Window modes section above).
- `hide_from_capture` (keeping the panel out of screen shares) is best effort; macOS 15.4+ ignores it for full-screen shares.
- The shipped bundle is a debug build signed with a self-signed dev certificate; there is no notarized release.
- Speaker diarization inside the Them stream is not attempted - it is one rolling `Them` voice.

## Use it responsibly

You are responsible for following the recording-consent laws that apply where you are, which in several places require informing or getting consent from everyone in the call.
The app shows no indication to other participants, and with `[trace] audio = true` it keeps the audio of the conversation on this Mac.

## Docs

The full index is [docs/README.md](docs/README.md): [configuration](docs/configuration.md), [servers](docs/servers.md), [architecture](docs/architecture.md), [trace format](docs/trace-format.md), [development](docs/development.md), [troubleshooting](docs/troubleshooting.md), [design decisions](docs/decisions.md).

## License

MIT, see [LICENSE](LICENSE).
