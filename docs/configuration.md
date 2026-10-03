# Configuration

Settings are split in two places.

Server endpoints, model ids and API keys come from environment variables, usually supplied by a `.env` file.
App behavior (audio backends, segmenter numbers, overlay, hotkeys) comes from an optional TOML file.
The reference for each is [.env.example](../.env.example) and [config.example.toml](../config.example.toml).

## Environment (.env)

Lookup order for the file, first match wins:

1. The path given by `--env-file`.
2. `./.env` in the working directory.
3. `~/.config/clueless/.env`.

A file is never required.
Every variable can also come straight from the process environment, and the process environment always wins over the file.
An app launched from Finder or with `open` has no shell environment, so in practice the `.env` file is what configures it; `cargo xtask run` passes the repo's `.env` through.

Missing or invalid values are reported together in one error that names every problem and points at `.env.example`, and the app exits with code 2.
The old TOML `[server]` and `[llm]` tables are rejected with an error naming the environment variable that replaced each key.

`BASE_URL` values are bare origins like `http://localhost:8000`; the clients append `/v1/...` themselves, so do not include `/v1` in the value.

### LLM variables

| Variable | Required | Default | Meaning |
| --- | --- | --- | --- |
| `LLM_BASE_URL` | yes | - | Chat server origin, http(s) |
| `LLM_MODEL` | yes | - | Model id sent with every chat request |
| `LLM_API_KEY` | no | unset | Sent as `Authorization: Bearer <key>` when set |
| `LLM_MAX_TOKENS` | no | 220 | Answer length cap |
| `LLM_TEMPERATURE` | no | 0.4 | Sampling temperature |
| `LLM_PROFILE_PATH` | no | unset | Text file placed in the system message, `~` is expanded |
| `LLM_ENABLE_THINKING` | no | unset | `true`/`false`; unset omits the field from the request entirely |

`LLM_ENABLE_THINKING=false` sends `chat_template_kwargs: {"enable_thinking": false}`, which removes the reasoning delay on vLLM-hosted Qwen-family models; hosted APIs generally want it unset.

### ASR variables

| Variable | Required | Default | Meaning |
| --- | --- | --- | --- |
| `ASR_BASE_URL` | yes | - | Transcription server origin, http(s) |
| `ASR_MODEL` | yes | - | Model id sent with every transcription request |
| `ASR_API_KEY` | no | unset | Sent as `Authorization: Bearer <key>` when set |
| `ASR_LANGUAGE` | no | unset | Language hint like `en`; unset lets the server detect |

### Examples

vLLM plus a local OpenAI-compatible ASR server:

```sh
LLM_BASE_URL=http://localhost:8000
LLM_MODEL=Qwen/Qwen3-4B-Instruct-2507
LLM_ENABLE_THINKING=false
ASR_BASE_URL=http://localhost:8097
ASR_MODEL=nvidia/parakeet-tdt-0.6b-v2
```

One hosted API provider:

```sh
LLM_BASE_URL=https://api.openai.com
LLM_MODEL=gpt-4o-mini
LLM_API_KEY=sk-...
ASR_BASE_URL=https://api.openai.com
ASR_MODEL=gpt-4o-transcribe
ASR_API_KEY=sk-...
```

The app only needs the three routes described in [servers.md](servers.md), so any server that implements them works.

## App behavior (config.toml)

The file is optional and every key in it is optional; a missing file or key takes the default.
An unknown key is an error, so typos surface at startup.

Lookup order: the path given by `--config`, else `~/.config/clueless/config.toml`.

### `[audio]`

| Key | Default | Meaning |
| --- | --- | --- |
| `system_audio_backend` | `"sck"` | `"sck"`, `"cpal_loopback"` or `"device:<name>"`, see [system-audio.md](system-audio.md) |
| `mic_device` | default input | Exact coreaudio device name |
| `watchdog_restarts` | 5 | Stream restarts that are allowed without a sample in between; 0 turns the watchdog off |
| `watchdog_silence_secs` | 0 | Restart after this long without samples; 0 turns the timer off |

### `[vad]`

| Key | Default | Meaning |
| --- | --- | --- |
| `start_threshold` | 0.5 | Speech probability that starts an utterance |
| `end_threshold` | 0.35 | Below this a frame counts as silence |
| `end_silence_frames` | 19 | Silent 32 ms frames that end an utterance (608 ms) |
| `max_segment_ms` | 15000 | Utterances are force-cut at this length |

### `[overlay]`

| Key | Default | Meaning |
| --- | --- | --- |
| `hide_from_capture` | true | Sets the hidden overlay panel's sharing type to none; the standard window is never hidden from capture; best effort on macOS 15.4+ |

### `[hotkeys]`

All keys are global while their registration window is active; the syntax is `cmd|ctrl|alt|shift` plus a key name like `cmd+shift+KeyR`.

| Key | Default | Registered |
| --- | --- | --- |
| `suggest` | `cmd+Enter` | during a meeting |
| `clear` | `cmd+shift+KeyX` | during a meeting |
| `move_left` / `move_right` / `move_up` / `move_down` | `cmd+shift+Arrow*` | during a meeting |
| `toggle_meeting` | `cmd+shift+KeyR` | always |
| `toggle_overlay` | `cmd+Backslash` | always |
| `toggle_click_through` | `cmd+shift+KeyM` | always |
| `toggle_mode` | `cmd+shift+Backslash` | always |

## Window modes

The app has two windows and shows at most one at a time: a standard window with a title bar that can be dragged and resized, and a borderless overlay panel that is click-through.
Every launch starts in standard mode; `hotkeys.toggle_mode` (default `cmd+shift+Backslash`) or the status icon menu switches between the two, and the overlay then covers exactly the standard window's content area.
The show/hide menu item names the active window ("Show Window"/"Hide Window" in standard mode, "Show Overlay"/"Hide Overlay" in hidden mode) and the mode item names the destination ("Switch to Hidden Overlay"/"Switch to Standard Window").
The close button and `cmd+W` hide the window and the app keeps running in the menu bar; `cmd+Q` quits.
The four move keys act on whichever window is on screen; dragging and resizing only exist in standard mode.
Screen capture always sees the standard window; `overlay.hide_from_capture` applies to the overlay panel only, and it is best effort: on macOS 15.4 and later ScreenCaptureKit ignores the panel's sharing type, so hidden mode can still show up when a whole screen is shared.
The standard window's position and size are remembered across launches in the user defaults of the app's bundle id (AppKit frame autosave `CluelessMainWindow`); the overlay derives its frame from it.
The bundled app and a bare `cargo run` binary have different bundle ids, so they remember different frames.

## Other inputs

- `LLM_PROFILE_PATH` points at a plain text file with your name, role and anything the answers should know; it is read at meeting start and placed in the system message.
- Transcripts and suggestions are never written to disk; the only files the app writes are its log (`~/Library/Logs/clueless/clueless.log`) and a single-instance lock (`~/Library/Application Support/clueless/lock`).
- The one piece of state that survives a launch is the standard window's frame, kept by AppKit in the user defaults; the app owns no file for it.
