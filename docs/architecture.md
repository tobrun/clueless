# Architecture

Purpose: clueless is a macOS meeting copilot for one user on one Mac. During a meeting it records the microphone ("Me") and the system audio ("Them") as two streams, cuts each into utterances, transcribes each utterance on a speech-to-text server over HTTP, and shows a rolling transcript in a standard window or a hidden overlay panel. On a hotkey it asks an LLM server over HTTP what to say next and streams the answer into whichever window is on screen. Three built-in assist profiles (Manual, Interview, Brainstorm) decide whether the app also asks by itself at the end of a turn; Manual, the default, asks only on the hotkey. While a meeting runs the engine also writes a session trace under the data directory (`~/.clueless` by default): the transcript, commands and every model call as JSON lines, plus the speaker WAVs when audio recording is on; the same binary re-runs a recorded session through the current build and compares the two traces. Audio flows capture -> ring buffer -> engine stream thread (resample, voice detection, segmenter) -> ASR worker -> transcript store -> UI event -> overlay; a suggestion flows hotkey (or, in Interview and Brainstorm, a finished turn) -> engine command -> prompt builder -> LLM stream -> overlay.

Captured: 2026-10-02, updated 2026-10-03 when the server endpoints moved to `.env`, the switchable window modes were added, and continuous assistance with assist profiles was added; updated 2026-10-05 for the trace crate, session recording, re-run and compare.

## Components

| Component | Responsibility | Lives at | Talks to |
| --------- | -------------- | -------- | -------- |
| types | shared data types, config, source traits | `crates/types/` | nothing |
| segmenter | resample, VAD wrapper, utterance state machine, dedup | `crates/segmenter/` | types |
| asr | WAV encoding and transcription client | `crates/asr/` | types |
| llm | streaming chat client | `crates/llm/` | types |
| context | transcript store, prompt builder, assist profiles and trigger policy, echo test, token estimate | `crates/context/` | types |
| trace | record types, sink traits, disk writer, audio files, reader, compare, list, show | `crates/trace/` | types |
| engine | threads, queues, meeting state, suggestions, replay | `crates/engine/` | types, segmenter, asr, llm, context, trace |
| capture | microphone and system-audio sources | `crates/capture/` | types |
| overlay | standard window, hidden overlay panel, mode switch, views, hotkeys, menu-bar item | `crates/overlay/` | types |
| app | binary: wiring, command line, logging | `crates/app/` | all of the above |
| xtask | bundle, sign, run | `xtask/` | nothing |

## Flows

### Transcription

1. A capture source (cpal microphone or ScreenCaptureKit system audio) writes mono samples into a ring buffer (`crates/capture/`).
2. One engine stream thread per source reads the ring, resamples to 16 kHz, scores frames with the VAD and runs the segmenter state machine (`crates/engine/`, `crates/segmenter/`).
3. Segments go to per-stream ASR workers, which call the transcription server (`crates/asr/`) and commit finished utterances to the transcript store (`crates/context/`).
4. Every commit, interim and drop becomes a `UiEvent` sent to the overlay panel (`crates/overlay/`).

### Suggestion

1. A global hotkey in the overlay sends `EngineCommand::Suggest`, which means "answer now" in every profile and cancels a running answer; in Interview and Brainstorm the engine can also start a request by itself, see Automatic assistance (`crates/overlay/`, `crates/engine/`).
2. The engine builds one system and one user message from the transcript store, the notes file and the text in progress, with the active profile's instruction after the transcript (`crates/context/`).
3. The LLM client streams content deltas from the chat server (`crates/llm/`).
4. Each delta is a `UiEvent` with the suggestion id; the overlay drops events with an old id and shows the answer as the newest entry of a feed (`crates/overlay/`).

### Automatic assistance

1. Hotkey `hotkeys.cycle_profile` (default `ctrl+alt+KeyP`) sends `EngineCommand::CycleProfile` and the status icon menu sends `EngineCommand::SetProfile`; the engine keeps the active profile across meetings and sends `UiEvent::Profile`, which the overlay shows at the start of the status line (`crates/overlay/`, `crates/engine/`).
2. The engine hears about every finished piece of either speaker, committed or dropped, plus whether a speaker is still busy (`crates/engine/`).
3. The trigger policy decides per piece whether to fire: Interview waits for the end of a Them turn, Brainstorm fires at every finished piece of Me; it skips short turns, keeps a minimum gap between automatic requests, waits while an answer runs and stays quiet for a while after a failure (`crates/context/`).
4. A fired request follows the Suggestion flow; the stream holds back its first words until it is clear the answer is not the single word PASS, which shows nothing and adds no feed entry (`crates/engine/`).

### Meeting lifecycle

1. A hotkey or menu-bar click sends `StartMeeting` (`crates/overlay/`).
2. The engine checks both servers, reads the notes file, opens each speaker the factory lists and starts one thread per source (`crates/engine/`, `crates/capture/`).
3. `StopMeeting` flushes both segmenters, releases echo holds, waits for queued finals, cancels in-flight requests and joins the threads (`crates/engine/`).

### Mode switch

1. The `toggle_mode` hotkey (default `cmd+shift+Backslash`) or the status icon menu item routes through `handle_action` in the overlay (`crates/overlay/`).
2. The overlay orders out the window on screen, fits the other one to the standard window's content area, and shows it; AppKit frame autosave (`CluelessMainWindow`) remembers the frame in the user defaults of the bundle id (`crates/overlay/`).

### Replay

1. The binary takes one or two WAV files, a speed factor and optionally `--profile NAME` on the command line (`crates/app/`).
2. Paced in-memory sources feed the same engine (`crates/engine/`), which prints each final utterance on stdout and exits when drained.

### Recording

1. `start_meeting` opens a trace directory under the data directory (`~/.clueless` by default) and writes `manifest.json`; the overlay gets one Info status line naming where the meeting is stored and whether audio is included (`crates/engine/`, `crates/trace/`).
2. The engine wraps the UI sink so every `UiEvent` is also a record, and the stream threads, ASR workers, engine loop and LLM tasks record segments, ASR calls, commands, policy decisions and full model requests and answers (`crates/engine/`).
3. A writer thread drains bounded queues into `events.jsonl` and the speaker WAVs; a full queue drops messages and counts them in a `records_lost` record, so recording never blocks or fails the meeting (`crates/trace/`).
4. `stop_meeting` flushes and closes the trace before the engine reports `Idle` (or 2 s pass trying), so the files are complete when the app exits on `Idle` (`crates/engine/`).

### Session re-run

1. The binary reads a session's `events.jsonl`, manifest and WAVs and feeds them to the same engine: per-speaker audio at its timeline position, recorded hotkey presses and profile changes at their recorded times, at `--speed` (`crates/app/`, `crates/engine/`).
2. The re-run writes its own trace plus the session's notes to `<session>/runs/<run id>/` and prints the same stdout as `--replay`, then `run: <path>` on stderr (`crates/trace/`).

### Compare

1. The binary reads two traces - a session and one of its runs, newest by default - and prints the transcript and suggestion differences as plain text (`crates/app/`, `crates/trace/`).
2. Unless `--no-judge`, the judge in the engine asks the chat server per suggestion pair which answer is better, one `POST /v1/chat/completions` at a time, and the verdicts join the report (`crates/engine/`).

## Boundaries

| Boundary | Kind | Owned by | Notes |
| -------- | ---- | -------- | ----- |
| LLM server | external HTTP | external, configured by `LLM_BASE_URL` | OpenAI-compatible /v1/models, /v1/chat/completions |
| ASR server | external HTTP | external, configured by `ASR_BASE_URL` | OpenAI-compatible /v1/models, /v1/audio/transcriptions |
| Microphone | device | capture | cpal input stream, f32 format required |
| System audio | device | capture | ScreenCaptureKit default, cpal loopback or a named device |
| Config file | file | types | `--config` path or ~/.config/clueless/config.toml, TOML, optional |
| Env file | file | app | `--env-file` path, else ./.env, else ~/.config/clueless/.env; LLM_* and ASR_* variables |
| Log file | file | app | ~/Library/Logs/clueless/clueless.log |
| Lock file | file | app | ~/Library/Application Support/clueless/lock, single instance |
| Data directory | file | trace | `--data-dir` path or ~/.clueless; sessions/<name>/ with manifest.json, events.jsonl, audio WAVs and runs/ |

## Cross-cutting

Timing: every timeout is a field of `EngineTimings` (engine) so tests inject 100-300 ms values. Logging: tracing to stderr and the log file, per-utterance latency fields, transcript text only at debug (a meeting's transcript and model text live in its trace files under the data directory, not in the log). Purity: types, segmenter, asr, llm, context, engine and trace compile without any macOS-only crate (docs/dependencies.md). Threading: DSP runs on one std thread per stream, async network work on one tokio runtime, AppKit only on the main thread, UI events hop through the main dispatch queue.

## Entry points

`crates/app/src/main.rs` (the `clueless` binary: GUI mode, WAV replay, session re-run, compare, sessions, show, delete), `xtask/src/main.rs` (`cargo xtask bundle|run`), `scripts/make-fixtures.sh` (test fixtures), `scripts/smoke-llm.sh` and `scripts/smoke-asr.sh` (server checks).
