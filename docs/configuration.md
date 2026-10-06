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
| `LLM_NOTES_PATH` | no | unset | Notes file placed in the system message, `~` is expanded |
| `LLM_ENABLE_THINKING` | no | unset | `true`/`false`; unset omits the field from the request entirely |
| `LLM_INCLUDE_USAGE` | no | `true` | `true`/`false`; ask streaming responses for token usage (`stream_options.include_usage`) so session traces carry the server's counts |

`LLM_NOTES_PATH` replaces the older `LLM_PROFILE_PATH`; the word "profile" now means only the switchable assist profile.
A set `LLM_PROFILE_PATH` is a startup error that names `LLM_NOTES_PATH`.

`LLM_ENABLE_THINKING=false` sends `chat_template_kwargs: {"enable_thinking": false}`, which removes the reasoning delay on vLLM-hosted Qwen-family models; hosted APIs generally want it unset.

`LLM_INCLUDE_USAGE=false` leaves the `stream_options` field out of every streaming request, for servers that reject it; usage counts then simply stay absent from `llm_end` trace records and answers are unaffected.

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
| `mic_device` | Auto | unset: system default input, steered onto a built-in or wired input when the default is Bluetooth; `"default"`: always the system default; any other string: that exact input device name |
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

### `[assist]`

| Key | Default | Meaning |
| --- | --- | --- |
| `start_profile` | `"manual"` | Profile active at launch: `"manual"`, `"interview"` or `"brainstorm"`; anything else is a startup error listing the three names |

See Profiles below.

### `[trace]`

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | true | Write a trace directory per meeting under the data directory: transcript, commands, statuses and every model call as JSON lines |
| `audio` | false | Also record both speaker WAVs into the session directory (~115 MB per recorded hour per speaker) |

The data directory defaults to `~/.clueless`; `--data-dir PATH` moves it and is valid in every mode, including `--sessions`, `--show`, `--replay-session`, `--compare` and `--delete`.
One session directory per meeting holds `manifest.json`, `events.jsonl`, the optional `audio/` WAVs and `runs/` for re-runs; the format is [trace-format.md](trace-format.md).
A meeting whose trace cannot be opened or written still runs, with one `trace:` warning in the status line (see [troubleshooting.md](troubleshooting.md)).

The new command line modes read and write this directory:

| Mode | Effect |
| --- | --- |
| `--sessions` | list sessions newest first: length, origin, audio, finals, suggestions, runs |
| `--show SESSION` | print a session's finals and suggestions in meeting time order |
| `--replay-session SESSION [--speed N]` | re-run the session's audio and recorded presses through this build into `<session>/runs/<run id>/` |
| `--compare A [B] [--no-judge]` | diff a session against one of its runs (default: the newest) and judge each suggestion pair on the LLM server |
| `--delete SESSION [--yes]` | delete a session or run; without `--yes` it only prints the path and size |

A `SESSION` value is a session directory name (`2026-10-05T14-03-22Z`) or a path to it.
Only one mode flag per launch; two of them are a parse error.
`--sessions`, `--show`, `--delete` and `--compare --no-judge` never contact a server and work with no `.env` file present.

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
| `cycle_profile` | `ctrl+alt+KeyP` | always |

## Profiles

An assist profile decides when the app asks the LLM for help by itself.
There are three built-in ones, and `assist.start_profile` picks the one active at launch.
The `cycle_profile` hotkey steps through them and the status icon menu lists all three for a direct pick; the active one is marked in the menu and its name opens the status line, followed by " ..." while a request runs.
The active profile is kept across meetings in one session, and switching is allowed while idle.

- Manual (default): the app starts no request by itself; the `suggest` hotkey is the only trigger.
- Interview: asks at the end of a turn of the other side (Them), to help answer what they raised.
  The turn counts as over 400 ms after its last finished piece with Them not speaking, or after at most 4 s of waiting.
  Without a Them stream no automatic request ever fires.
- Brainstorm: asks at every finished piece of your own speech (a pause of about 0.6 s or the 15 s cut), also while you keep talking, and offers ideas as up to 3 dash lines.

The `suggest` hotkey means "answer now" in every profile and cancels a running answer; an automatic request never cancels one and instead waits and fires once when the answer is done.
The model decides whether there is anything worth saying: answering with the single word PASS shows nothing and adds no feed entry.
The previous answer is shown to the model so it does not repeat itself.

Limits on automatic requests:

- At least 2 s between the starts of two automatic requests, 8 s in Brainstorm.
- Turns shorter than 12 characters without a question mark start no request.
- After a failed or interrupted request automatic requests pause for 30 s, unless a request has ended successfully since.
  Connect and HTTP failures (429 included) show in the status line and not in the feed; an answer that stalls mid-stream keeps its text and ends with "[interrupted]".

Answers collect in a feed in both windows, newest at the bottom, older ones stay and can be scrolled to.
Automatic help continues while the overlay is hidden.
The log gets an info line with the profile and the outcome (`assist_outcome`: `fired` when an automatic request starts, `waiting` when one is held back for the settle time, the gap or a running answer), never transcript text.

Cost note: every automatic request sends the whole transcript.
On a paid API without prompt caching that costs up to the full context per turn, and on a server with one cache slot a compression request can push the transcript out of the cache.
The system message and the transcript part do not depend on the profile, so switching profile keeps the server's prompt cache.

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

- `LLM_NOTES_PATH` points at a plain text file (the notes file) with your name, role and anything the answers should know; it is read at meeting start and placed in the system message.
- The app writes its log (`~/Library/Logs/clueless/clueless.log`), a single-instance lock (`~/Library/Application Support/clueless/lock`) and, while `[trace] enabled` holds, a trace directory per meeting under the data directory (`~/.clueless` by default) with the transcript text, the model calls and, with `[trace] audio = true`, the audio; see [trace-format.md](trace-format.md).
- No file under the data directory contains an API key: server URLs are stored redacted and keys are never written.
- The one piece of state that survives a launch is the standard window's frame, kept by AppKit in the user defaults; the app owns no file for it.
