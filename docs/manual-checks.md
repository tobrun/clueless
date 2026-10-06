# Manual checks

The automated suite covers everything that can be proven headlessly.
These checks need a real desktop session, a real microphone and reachable ASR/LLM servers, so they are done by hand before a release.
Run from the repo root after `cargo xtask bundle` (assembles `target/debug/clueless.app`; `cargo xtask run` bundles and launches it).

## Setup

- [ ] `cp .env.example .env` and fill `ASR_BASE_URL`, `ASR_MODEL`, `LLM_BASE_URL`, `LLM_MODEL` for your servers.
- [ ] `clueless --help` prints the usage text and exits 0.
- [ ] `clueless --config /does/not/exist.toml` prints the path and exits 2.
- [ ] With no `.env` anywhere and no `ASR_*`/`LLM_*` in the environment, startup prints one error naming every missing variable and exits 2.
- [ ] With an `[llm]` table in `config.toml`, `cargo xtask run` exits 1 within seconds and prints the `startup failed:` config error and the log path.
- [ ] With that table removed, `cargo xtask run` starts the app and stays attached until the app quits.

## GUI lifecycle

- [ ] Launch `open target/debug/clueless.app` (or the binary directly): the standard window appears with a title bar, the overlay panel is not on screen, and the menu bar shows the clueless icon.
- [ ] The icon menu shows "LIVE"/"offline" server health within ~2 s of launch.
- [ ] Start a meeting from the icon menu: the panel header turns green and transcripts appear as you speak.
- [ ] A second launch fails with a message naming `~/Library/Application Support/clueless/lock` and exits 2.
- [ ] Toggle the overlay hidden and back; the transcript content is still there.
- [ ] Quit from the icon menu; a relaunch works immediately (the lock is released).

## Audio

- [ ] Speak into the mic: an interim line appears within ~1 s and a final line within ~3 s of stopping.
- [ ] Play a meeting video with system sound: Them lines appear; the mic hearing the speakers does not also produce Me lines (echo filter).
- [ ] Unplug the mic mid-meeting: a warning status appears and capture restarts (watchdog); replugging recovers it.
- [ ] Mute system audio for a minute: no crash, no interim spam.
- [ ] Bluetooth headset as the default input and output, `mic_device` unset, start a meeting: the output rate does not drop (check `system_profiler SPAudioDataType` for the headset before and during the meeting), the status line names the built-in mic ("kept headset audio full quality"), and speaking toward the built-in mic yields Me lines; `mic_device = "default"` reproduces the headset call-mode degradation as the documented override.
- [ ] Revoked Screen Recording (`tccutil reset ScreenCapture be.tobrun.clueless`, restart): starting a meeting shows "Them: Them audio off: Grant Screen Recording..." first in the status line after the profile, the menu bar icon gains a `!`, its menu leads with a disabled item holding the full error text, the log has an ERROR line, and the meeting still works with Me lines; re-granting and restarting clears all of it.

## Suggestions

- [ ] Press the suggest hotkey (default Cmd+Enter) during a live meeting: a suggestion streams into the panel within a few seconds.
- [ ] Press it again mid-stream: the old suggestion stops and the new one starts.
- [ ] Long conversation: once the context passes the compression threshold the suggestion still arrives and `[llm]` history keeps working (check `~/Library/Logs/clueless/clueless.log` for a compress entry).

## Continuous assistance

Needs a real two-sided conversation (a second person or a video call with system sound) and a live LLM server.

- [ ] Fresh launch with no `[assist]` key: the status line starts with "manual" and nothing is asked until `Cmd+Enter` is pressed.
- [ ] Press `ctrl+alt+KeyP` until the status line reads "interview": the other side asks a real question and an answer streams into the feed without a key press.
- [ ] Small talk from the other side ("yeah", "okay, thanks") shows nothing.
- [ ] Switch to brainstorm and talk for a while: ideas as up to 3 dash lines are added to the feed while you keep talking, at least 8 s apart.
- [ ] Older feed entries stay above the newest one and can be scrolled to in both windows.
- [ ] Pick a profile from the status icon menu: the status line shows the new name and the active menu item is marked; switching mid-meeting and while idle both work.
- [ ] `Cmd+Enter` in every profile answers now and cancels a running answer.
- [ ] Stop the LLM server mid-meeting: the status line shows the error, nothing is added to the feed, and automatic requests resume after the 30 s pause once the server is back.
- [ ] Note `llm_first_delta_ms` in `~/Library/Logs/clueless/clueless.log` for automatic requests, and whether the model answers PASS when it has nothing to say (the PASS answers must not show).

## Overlay panel

- [ ] Move the panel with the move hotkeys during a meeting; it stays where left and survives show/hide.
- [ ] The panel does not appear in screen-share or screenshot captures (`hide_from_capture = true`, best effort on macOS 15.4+, see the Window modes block).
- [ ] Click-through on: clicks pass to the app beneath; toggle click-through off and the panel accepts text selection.

## Window modes

- [ ] With fresh user defaults, launch: the standard window appears with a title bar at top centre, content 560 by 320, the overlay is not on screen, and the status icon menu reads "Hide Window" and "Switch to Hidden Overlay".
- [ ] Drag and resize the window, press `cmd+shift+Backslash`: the window is gone, the overlay covers exactly the old content area, and clicks pass through it.
- [ ] In hidden mode, type in the app that was in front before the switch: the keystrokes reach that app without clicking it first.
- [ ] Press `cmd+shift+Backslash` again: the standard window is back at the same frame and in front.
- [ ] The menu item "Switch to Standard Window" / "Switch to Hidden Overlay" gives the same result as the hotkey.
- [ ] Resize the window to its smallest: it stops at 320 by 200 content and no views overlap.
- [ ] Resize wide while a suggestion is shown: the text re-wraps to the new width in both modes.
- [ ] Click the red close button: the window hides, the app is still in the menu bar, and the menu reads "Show Window".
- [ ] `cmd+W` with the window key: same as the close button.
- [ ] Select suggestion text and press `cmd+C`: the text is on the clipboard.
- [ ] `cmd+Q` with the window key: the app quits through the normal quit path.
- [ ] Hide with `cmd+Backslash` in hidden mode, then switch mode: the standard window shows.
- [ ] Move, resize, quit, relaunch: the window opens at the saved frame in standard mode.
- [ ] During a meeting in hidden mode press a move key, switch to standard: the window sits at the moved position.
- [ ] Outside a meeting press `cmd+shift+ArrowRight` in a text editor: the editor extends its selection and the window does not move.
- [ ] The click-through hotkey in standard mode: nothing changes.
- [ ] Switch to standard mode from a different Space than the one the window was last on: the window appears on the current Space.
- [ ] Save a frame on an external display, unplug it, launch and switch to hidden: the overlay is fully on the remaining screen.
- [ ] Screenshot with `screencapture` in each mode: the standard window is in the image; the overlay is not (macOS below 15.4) or is noted as visible (15.4 and later).

## Session traces

- [ ] With `[trace] audio = true` in `config.toml`, start a live meeting: the status line shows one `recording to` line naming the session path and that audio is on, and `~/.clueless/sessions/<name>/` appears with `manifest.json`, `events.jsonl` and `audio/me.wav` plus `audio/them.wav` (mode 0600).
- [ ] Speak a few sentences and press `Cmd+Enter` for one suggestion, then quit the app from the icon menu.
- [ ] `clueless --sessions` lists the session with audio yes, the finals and suggestion counted, and no `cut off` (quitting wrote the `end` record); `tail -1 events.jsonl` shows `"kind":"end"`.
- [ ] `clueless --show <name>` prints the finals as `[mm:ss] Me: text` with the suggestion under its `--- suggestion 1 (manual, manual) ---` line.
- [ ] Both WAV files play and their content lines up with the `--show` times: your speech is heard in `me.wav` at its printed time.
- [ ] With `[trace] enabled = false`, a meeting shows no `recording to` line and adds no session directory.

## Failure modes

- [ ] Turn a server off before launch: the health status goes offline, no transcripts, and the app stays responsive; turning it back recovers transcripts without a restart.
- [ ] Kill the ASR server mid-meeting: statuses go offline and interim lines stop; restart recovers within a few utterances.
