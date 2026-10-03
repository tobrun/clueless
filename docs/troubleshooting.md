# Troubleshooting

## The app exits at startup with a config error

The error names every problem at once and exits with code 2.

- "missing required environment variables" means no `.env` was found (or it lacks `LLM_BASE_URL`, `LLM_MODEL`, `ASR_BASE_URL`, `ASR_MODEL`).
  Copy `.env.example` to `./.env` or `~/.config/clueless/.env` and fill the four required variables; see [configuration.md](configuration.md).
- A message about `[server]` or `[llm]` means an old `config.toml` still has endpoint keys; delete those tables, they now live in the environment.
- An "unknown key" message names the offending key in `config.toml`; fix or remove it.
- A message naming `~/Library/Application Support/clueless/lock` means another copy is running; quit it from its menu bar icon.

## The status label shows a server as offline

Health checks run at meeting start against `GET /v1/models` on both servers.

- Check `LLM_BASE_URL` / `ASR_BASE_URL`: they must be bare origins without `/v1`, reachable from this machine, and `LLM_MODEL` / `ASR_MODEL` must be ids the server actually serves.
- Prove the endpoint with `scripts/smoke-llm.sh` and `scripts/smoke-asr.sh`, which use the same variables.
- A server behind a VPN or firewall may need the Mac's local-network permission (System Settings, Privacy & Security).

## No Them lines (system audio)

The first start of the system stream asks for Screen Recording.
macOS does not apply that grant to a running process: quit clueless and start it again after allowing it.
If the panel keeps showing only Me lines, check the grant in System Settings (Privacy & Security, Screen Recording), and watch the menu bar status label for the restart message.
macOS sometimes asks to re-approve this grant after updates; the same restart applies.

## No Me lines (microphone)

- Allow Microphone in System Settings (Privacy & Security); the app warns in the status label when the mic delivers only digital silence.
- Check the input device: `system_audio_backend` covers Them, while `[audio] mic_device` pins the mic (default input when unset).

## Hotkeys do nothing

The suggest, clear and move keys are registered only while a meeting is running, so they are silent outside a meeting.
`cmd+shift+KeyR`, `cmd+Backslash`, `cmd+shift+KeyM`, `cmd+shift+Backslash` (the mode switch) and `ctrl+alt+KeyP` (cycle profile) are always on, which also takes those combinations from every other app; remap them in `[hotkeys]` if they collide with something you need.

## No automatic answers

The log has an info line with the profile and the outcome when the app decides to ask (`fired`) or to hold a request back (`waiting`), and a warning when a failed request starts the pause; none of them carry transcript text. A turn that is too short leaves no line at all. Check in this order.

- The profile is Manual: the status line starts with the profile name; press `ctrl+alt+KeyP` or pick one from the status icon menu, or set `assist.start_profile`.
- The turn was too short: turns under 12 characters without a question mark start no request.
- The gap: automatic requests start at least 2 s apart, 8 s in Brainstorm.
- The pause after a failure: for 30 s after a failed or interrupted request nothing is asked automatically, unless a request has ended successfully since.
- System audio is missing in Interview: with no Them stream it never fires; the source status says system audio is missing, see "No Them lines" above.

## Too many answers

The model is expected to answer with the single word PASS when it has nothing to say, and the app hides that.
If a model ignores PASS and always answers, try a more instruction-following model, switch to Manual, or use Interview instead of Brainstorm.

## The app does not launch from the terminal or Finder

- Use `cargo xtask run` (bundles, signs, opens). Launching the bare binary by hand works too, but permission prompts then name the terminal instead of clueless.
- If signing fails, the dev certificate is missing: run `scripts/make-dev-cert.sh` once.
- macOS treats ad-hoc or rebuilt binaries as new apps and drops earlier permission grants; a stable `clueless-dev` signature keeps them across rebuilds.

## Where to look when something else misbehaves

The log file is `~/Library/Logs/clueless/clueless.log` (transcript text only appears at debug level).
Each transcribed utterance logs its latency fields, which usually tells you whether capture, the segmenter or the ASR server is the slow part.

## The overlay is visible in a screen share

The standard window is always captured; only the hidden overlay panel can be hidden from capture, so switch to hidden mode with `cmd+shift+Backslash` before sharing.
`hide_from_capture = true` is best effort: macOS 15.4 and later ignore the panel's sharing type for full-screen shares ([configuration.md](configuration.md) has the details), so hidden mode can still show up when a whole screen is shared.
Hide the overlay with `cmd+Backslash` before sharing, or move it off the part of the screen you will share.
