# Manual checks

The automated suite covers everything that can be proven headlessly.
These checks need a real desktop session, a real microphone and reachable ASR/LLM servers, so they are done by hand before a release.
Run from the repo root after `cargo xtask bundle` (assembles `target/debug/clueless.app`; `cargo xtask run` bundles and launches it).

## Setup

- [ ] `cp .env.example .env` and fill `ASR_BASE_URL`, `ASR_MODEL`, `LLM_BASE_URL`, `LLM_MODEL` for your servers.
- [ ] `clueless --help` prints the usage text and exits 0.
- [ ] `clueless --config /does/not/exist.toml` prints the path and exits 2.
- [ ] With no `.env` anywhere and no `ASR_*`/`LLM_*` in the environment, startup prints one error naming every missing variable and exits 2.

## GUI lifecycle

- [ ] Launch `open target/debug/clueless.app` (or the binary directly): the overlay panel appears, click-through by default, and the menu bar shows the clueless icon.
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

## Suggestions

- [ ] Press the suggest hotkey (default Cmd+Enter) during a live meeting: a suggestion streams into the panel within a few seconds.
- [ ] Press it again mid-stream: the old suggestion stops and the new one starts.
- [ ] Long conversation: once the context passes the compression threshold the suggestion still arrives and `[llm]` history keeps working (check `~/Library/Logs/clueless/clueless.log` for a compress entry).

## Overlay panel

- [ ] Move the panel with the hotkeys (or drag); it stays where left and survives overlay toggles.
- [ ] The panel does not appear in screen-share or screenshot captures (`hide_from_capture = true`).
- [ ] Click-through on: clicks pass to the app beneath; toggle click-through off and the panel accepts text selection.

## Failure modes

- [ ] Turn a server off before launch: the health status goes offline, no transcripts, and the app stays responsive; turning it back recovers transcripts without a restart.
- [ ] Kill the ASR server mid-meeting: statuses go offline and interim lines stop; restart recovers within a few utterances.
