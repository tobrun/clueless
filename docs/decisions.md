# Decisions

Design records from building this project. Each record states the question, the
option chosen with its rationale and date, and the options rejected with theirs.

## Principles

None promoted yet.
A rationale becomes a principle after it decides a third decision.

## Product and scope

D-build-or-adopt: Should this be a new native Rust app, or an existing open-source overlay pointed at a local inference server?
  ✓ build a native Rust app - the goal names a native Rust UI and local servers, and the full MVP was taken on (2026-10-02) ⚠ weeks of work before first use
  ✗ adopt an existing overlay app - the open-source candidates are Electron or Tauri apps with a web UI in WKWebView, none native Rust
  (2026-10-02)

D-first-slice: How much of the initial research goes into the first release?
  ✓ phases 0 to 4 in one spec - chosen over a thin vertical slice (2026-10-02) ⚠ a large amount of code lands before the first full live meeting test
  ✗ thin vertical slice with mic only - was the initial recommendation
  ✗ all eight phases - screenshots, storage, summary, auto-trigger and packaging are not needed to use the tool in a meeting
  (2026-10-02)

D-packaging: Should this slice produce a notarized DMG?
  ✗ Developer ID signing and notarization - a personal tool on one Mac has no Gatekeeper problem to solve (2026-10-02)
  ⊘ not doing - reopen if the app must run on a Mac that did not build it
  (2026-10-02)

D-persistence: Is anything written to disk?
  ✗ sqlite as in the architecture diagram - belongs to Phase 6
  ✗ append-only JSON lines log - useful, but not asked for
  ⊘ not doing - the transcript lives in memory and is lost on quit or crash; reopen when the post-meeting summary is specced
  The one thing the app does store is the window frame, in the user defaults; see D-frame-store.
  (2026-10-02)
  superseded 2026-10-05 by D-recording-default and D-trace-format (recorded-session-traces/spec.md) - the user asked for persistence directly; the reopen condition above was not what triggered it

D-screen-context: Are screenshots sent to the model?
  ✗ screenshot hotkey - Phase 5
  ⊘ not doing - reopen as the next slice after this one works in a real meeting
  (2026-10-02)

D-auto-trigger: Should a question from Them fire a suggestion automatically?
  ✗ word-list detector or a one-token classifier call - not needed to use the tool
  ⊘ not doing - reopen after the manual flow has been used in real meetings
  (2026-10-02)

## Workspace and tooling

D-crate-layout: One crate or a workspace?
  ✓ cargo workspace of ten crates - cargo then refuses a macOS import inside the pure crates, and parallel work streams own disjoint files
  ✗ single crate with modules - nothing stops audio or AppKit code leaking into the logic that must be testable without permissions
  ✗ the nine-crate layout from the initial research as written - it has no home for shared types or for orchestration, and it includes storage and screen crates this slice does not build
  (2026-10-02)

D-engine-boundary: Where does the orchestration live (stream workers, ASR queue, meeting state, suggestion requests)?
  ✓ engine crate behind source traits - it has no macOS dependency, so the whole pipeline runs in tests from WAV files against a mock server
  ✗ inside the app binary crate - every pipeline test would link AppKit and ScreenCaptureKit and could not run headless
  ✗ inside the context crate - mixes pure text logic with threads and network calls
  (2026-10-02)

D-dependencies-file: Should module edges be written down as rules?
  ✓ allowlist in docs/dependencies.md - ten new modules appear at once, and the purity rule is the main reason for the layout
  ✗ no file - the rule would live only in prose and soften over time
  (2026-10-02)

D-toolchain: Which Rust setup and test runner?
  ✓ edition 2024 on pinned rustc 1.94.1 with cargo test - all probes compiled on it, and nothing extra has to be installed
  ✗ cargo-nextest - not installed
  ✗ unpinned stable - a new compiler could add clippy warnings mid-build
  (2026-10-02)

D-config: Where do settings live?
  ✓ a split - server endpoints, model ids and API keys come from LLM_* and ASR_* environment variables read from a .env file, and app behavior from an optional TOML file; reading the .env file at startup is what makes the environment work for an app launched with open, which does not inherit the shell environment
  ✗ everything in TOML - endpoints and model ids change per server and API keys would sit in a config whose example is committed; the split landed 2026-10-03
  ✗ environment variables only, no file - an app launched from Finder or with open has no shell environment, so a file to read is needed either way
  ✗ settings window - planned for later
  (2026-10-02, revised 2026-10-03)

D-logging: Where do logs go?
  ✓ tracing to stderr and a file - ~/Library/Logs/clueless/clueless.log, with per-utterance latency fields; transcript text only at debug level
  ✗ stderr only - an app started with open has no visible stderr
  (2026-10-02)

D-single-instance: What stops two copies from running?
  ✓ file lock - std File::try_lock on a lock file, second copy exits with a message
  ✗ nothing - two copies would both register hotkeys and both record
  (2026-10-02)

D-failure-channel: How does the app report a startup failure so a user launched through `open` can see it? (2026-10-04, loud-startup-failures/spec.md)
  ✓ one helper in `main.rs` logs the message at error level, then returns exit code 2 - the log file is the one place a user of the bundled app already looks (docs/troubleshooting.md names it), and the logger writes to stderr too, so terminal runs lose nothing
  ✗ stderr only (today) - `open` discards it, which is the bug
  ✗ a macOS alert or notification - a new AppKit path before the run loop exists, for a problem the log and xtask already cover

D-detect-failure: How does xtask learn that the app died at launch? (2026-10-04, loud-startup-failures/spec.md)
  ✓ read the new bytes of the log file after `open -W` returns and look for the marker - works because the app's failure is now in the log, and the log path is already known to xtask (`log_file_path`)
  ✗ time the launch and treat a short run as a failure - a user who quits within seconds would be reported as a failure, and a slow failure would be missed
  ✗ launch the binary directly to get its exit code - permission prompts then name the terminal instead of clueless (docs/troubleshooting.md, `cargo xtask run` entry), and the point of `open` is to avoid that
  ✗ `open --stderr FILE` capture - gets all log lines, not only failures, and `open` still drops the exit code, so a marker is needed anyway

D-marker-string: What exactly does xtask look for? (2026-10-04, loud-startup-failures/spec.md)
  ✓ the substring `startup failed: ` - the formatter prints `ERROR clueless: startup failed: ...` so a plain substring match works with no log parsing ⚠ the string is written in two crates (app and xtask) and nothing but tests and the contract keeps them equal; a log line from some other source containing the same words would be a false positive, which is unlikely because transcript text is logged only at debug level (docs/architecture.md, Cross-cutting)
  ✗ a structured field or special target - the log is plain text, so xtask would have to parse the formatter's output
  ✗ share a constant through a crate both depend on - xtask must not build or link the app crates (its tests skip the build on purpose, see `CLUELESS_XTASK_SKIP_BUILD`)

D-sites: Which exits use the loud path? (2026-10-04, loud-startup-failures/spec.md)
  ✓ env file load failure, config load failure and lock failure in `main` and `run_gui`, in both GUI and replay mode - these are the failures that happen after the log is open and before the first window, and replay shares the env and config code (replay tests only check that stderr contains the file name, which stays true) ⚠ replay's stderr lines now carry the log formatter's timestamp and level prefix
  ✗ argument parse errors and log-open errors - they happen before the logger exists, so there is no log to write to; `open` is started by xtask with fixed valid arguments, so a user cannot hit them through `cargo xtask run`
  ✗ every `eprintln!` in replay mode - those run in a terminal where stderr is visible

D-existing-instance: Should `xtask run` handle an instance that is already running (including one still shutting down)? (2026-10-04, loud-startup-failures/spec.md)
  ⊘ not doing - not the reported symptom, and the observed behaviour is unproven: without `-n`, `open -W` seems to attach to the running instance instead of starting a new one ? verify: start the app, rebuild, run `cargo xtask run`, and see whether the old build keeps running; reopen if it does
  ✗ add `open -n` or kill the running copy - changes how the single-instance lock is used and could drop a user's running meeting

D-config-migration: Should the app or xtask upgrade an old `config.toml` for the user? (2026-10-04, loud-startup-failures/spec.md)
  ✗ migrate the file automatically - rewrites a user-owned file and needs a mapping for every moved key
  ⊘ not doing - the user chose loud failures only (user 2026-10-04); the error text already names the exact replacement variables; reopen if a second config key moves

## Overlay

D-ui-stack: Which UI technology draws the overlay?
  ✓ objc2 with objc2-app-kit - an NSPanel subclass created with the non-activating style compiled and ran (probe 2026-10-02)
  ✗ Tauri with tauri-nspanel - web UI, and it converts a window to a panel at runtime, which is where its crashes come from
  ✗ egui or Slint - the window they create is not an NSPanel, so a click activates the app and takes focus from the meeting
  ✗ gpui or cacao - no official release, or self-described as very early stage
  (2026-10-02)

D-overlay-window: Which window settings?
  ✓ status level panel - level NSStatusWindowLevel, joins all Spaces, full-screen auxiliary, never key or main except in interactive mode verify: stays above a full-screen app
  ✗ screen-saver level - sits above system dialogs and permission prompts
  ✗ normal floating level - goes behind full-screen apps
  (2026-10-02)

D-capture-hiding: What happens with "invisible to screen share"?
  ✓ set sharingType none behind a config flag - one line of code, and it still hides the panel from older capture APIs ⚠ on macOS 15.4 and later ScreenCaptureKit ignores it, so apps like Zoom will show the panel when the whole screen is shared verify: share the screen once and look
  ✗ startup self-test with a warning badge plus a test matrix - dropped during the build (2026-10-02); it needs screenshot code from Phase 5
  ✗ no flag at all - loses the cases where it still works
  (2026-10-02)

D-hotkeys: Which hotkeys, and when are they registered?
  ✓ meeting-only registration - suggest, clear and the four move keys are registered only while a meeting runs, because a global hotkey takes the key combination away from every other app (2026-10-02) ⚠ Cmd+Enter does not send messages and Cmd+Shift+arrows do not select text in other apps during a meeting, and the four always-on defaults also take Cmd+Shift+R (browser hard reload), Cmd+Backslash, Cmd+Shift+Backslash (mode switch, added 2026-10-03) and Cmd+Shift+M from other apps all day; every key is configurable; the move keys act on the active window, see D-ui-modes
  ✗ register everything at launch - Cmd+Enter would stop working in Slack and Mail all day
  ✗ detect conflicts with other apps - macOS reported success even when another process held the same combination (probe 2026-10-02)
  (2026-10-02)

D-quit-path: How does the user quit an app with no Dock icon and no window buttons?
  ✓ menu-bar item - pulled forward from a later phase (2026-10-02) ⚠ item list grew with the modes (2026-10-03): Start Meeting / Stop Meeting, Hide Window / Show Window, Switch to Hidden Overlay / Switch to Standard Window, Quit (see D-ui-modes)
  ✗ quit hotkey only - one more always-on global key, and no visible sign the app is running
  (2026-10-02)

D-move-keys: What do the four move hotkeys do, and when are they registered?
  ✓ registered only while a meeting runs, matching D-hotkeys; they move the active window by 40 points, clamped with the window's real size, and the result is written to the shared frame - a global hotkey takes cmd+shift+arrows (text selection) away from every other app, and dragging the standard window covers placement outside a meeting (promoted from the switchable-ui-modes spec 2026-10-03) ⚠ the hidden overlay cannot be nudged outside a meeting; switch to standard mode and drag
  ✗ registered whenever hidden mode is active - text selection breaks in other apps for as long as hidden mode is on
  ✗ always on - contradicts D-hotkeys' cost rule for combinations other apps need
  ✗ disabled in standard mode - one more rule to explain, no benefit
  (2026-10-03)

D-suggestion-id: How are late tokens from a cancelled request kept off the screen?
  ✓ id on every suggestion event - the overlay drops any delta whose id is not the newest one
  ✗ rely on cancellation alone - a delta already sent to the main queue would still be drawn
  (2026-10-02)

D-ui-modes: How do we remove the fixed size and placement of the hidden overlay?
  ✓ two modes, a standard window and the hidden overlay, sharing one frame - gives a normal everyday UI and a familiar way to place and size the overlay (2026-10-03)
  ✗ placement tool only - the user wants the standard window as a real UI (2026-10-03)
  ✗ make the overlay itself draggable and resizable - gives no standard app UI (2026-10-03)
  ✗ `overlay.width` / `overlay.x` config keys - needs an app restart for every try, no visual feedback
  (2026-10-03)

D-window-structure: How are the two modes built?
  ✓ two windows, each with its own view tree, both rendered from the one `UiModel` on every change - each window keeps the styling that fits it, no view is moved between windows, switching is only show and hide ⚠ every repaint runs twice, and scroll position is per window
  ✗ two windows, one view tree moved with `setContentView` - the root view is a HUD card with rounded corners made for a borderless panel, so it would need restyling on every move
  ✗ one window whose style mask, level and sharing type change at runtime - the non-activating panel flag is fixed when the window is created verify: AppKit ignores `NonactivatingPanel` changes after init
  (2026-10-03)

D-app-presence: Does the app show a Dock icon or a top-left app menu in standard mode?
  ✓ always menu-bar-only (`Accessory` policy, `LSUIElement` true) in both modes - most discreet, mode switch lives in the status icon menu (2026-10-03) ⚠ the standard window does not appear in the Dock or in `cmd+Tab`
  ✗ regular app in standard mode only - Dock icon appearing and vanishing during a meeting (2026-10-03)
  ✗ regular app in both modes - Dock icon visible in screen shares while the overlay is hidden (2026-10-03)
  (2026-10-03)

D-launch-mode: Which mode does the app start in?
  ✓ always standard - behaves like a normal app at launch (2026-10-03) ⚠ a launch during a screen share shows the window to viewers until the user switches
  ✗ last used mode - turned down by the user (2026-10-03)
  ✗ always hidden - turned down by the user (2026-10-03)
  (2026-10-03)

D-frame-mapping: What exactly does the hidden overlay copy from the standard window?
  ✓ the content area (the part below the title bar), same origin and same size - the text sits in the same screen position in both modes, so nothing jumps on a switch
  ✗ the whole window frame including the title bar - the overlay has no title bar, so its content would shift up by the title bar height on every switch
  (2026-10-03)

D-frame-store: Where does the shared frame live, and is it remembered across launches?
  ✓ the standard window is the only store: it uses AppKit frame autosave (`setFrameAutosaveName`), and every change made in hidden mode is written into the standard window too, followed by `saveFrameUsingName` - no file format of our own, no window delegate, no save-on-quit hook (the app exits through `std::process::exit` in `apply_event` and `request_quit`) ⚠ the frame lives in the user defaults of the bundle id, so the dev bundle and an unbundled `cargo run` remember different frames verify: frame autosave restores a saved frame onto a screen that still exists
  ✗ JSON state file in `~/Library/Application Support/clueless/` - needs a window delegate or notifications to know when to save, plus parsing and error handling for a few numbers
  ✗ keys in `config.toml` - the app never writes its config; the user would edit numbers by hand
  ✗ not remembered - the user would place the window again on every launch, which is the complaint this change exists to fix
  (2026-10-03)

D-standard-style: What does the standard window look like?
  ✓ titled, closable, resizable, title "clueless", normal level, default Spaces behaviour, opaque with shadow, root view using the window background material with square corners - the plain macOS look (2026-10-03)
  ✗ miniaturizable - a menu-bar-only app has no Dock icon to restore from, so a minimized window is easy to lose
  ✗ always on top - turned down by the user (2026-10-03)
  (2026-10-03)

D-space: On which Space does the standard window appear when the user switches to it from another Space?
  ✓ the Space the user is on (`MoveToActiveSpace` collection behaviour) - the switch must not throw the user to a different Space in the middle of a meeting
  ✗ the Space where the window was last shown (AppKit default) - `makeKeyAndOrderFront` would change Space under the user
  (2026-10-03)

D-capture-scope: Is the standard window hidden from screen capture?
  ✓ never; it always uses the default sharing type, and `overlay.hide_from_capture` keeps applying to the hidden overlay only - the two modes exist so one is visible and one is not
  ✗ apply `hide_from_capture` to both - makes the modes differ only in chrome
  (2026-10-03)

D-mode-switch: What happens on a mode switch?
  ✓ always show the new mode's window at the shared frame, also when the UI was toggled hidden; going to hidden mode orders out the standard window, resets the overlay to click-through and gives keyboard focus back to the app that had it before; going to standard mode orders out the overlay and brings the window to the front with `makeKeyAndOrderFront` plus `NSApplication::activate` - asking for a mode means wanting to see it, and a hotkey press in the middle of typing must not leave the keyboard pointed at an invisible app verify: `-[NSApplication deactivate]` returns focus to the previous app; fallback is `hide:` followed by `orderFrontRegardless` on the overlay
  ✗ keep the hidden state across a switch - a switch that shows nothing looks broken
  (2026-10-03)

D-close: What do the red close button and `cmd+W` do in standard mode?
  ✓ hide the window through the same path as the show/hide action, so the menu title stays correct; the app keeps running in the menu bar - a menu-bar app must not quit when its window closes
  ✗ quit the app - ends a running meeting by accident
  ✗ let AppKit close it with no hook - the menu would still say "Hide Window"
  (2026-10-03)

D-key-equivalents: How do `cmd+C`, `cmd+A`, `cmd+W` and `cmd+Q` work in the standard window without an app menu?
  ✓ install a main menu that is never drawn (an `Accessory` app shows no menu bar) with Copy, Select All, Close Window and Quit items; Quit goes through `request_quit` - AppKit takes these shortcuts from the main menu verify: an `Accessory` app with a key window handles main-menu key equivalents
  ✗ no shortcuts - a standard window where copy and close do not work feels broken
  ✗ local key event monitor - reimplements what the main menu already does
  (2026-10-03)

D-mode-hotkey: Which hotkey switches modes, and when is it registered?
  ✓ new config key `hotkeys.toggle_mode`, default `cmd+shift+Backslash`, always registered - sits next to `toggle_overlay` (`cmd+Backslash`) verify: no common macOS app claims this combination
  ✗ meeting-only registration - the user places the window before a meeting starts
  (2026-10-03)

D-screen-change: What if displays change while the app runs?
  ✗ observe screen-change notifications and refit the overlay - needs notification plumbing the crate does not have, for a case nobody has hit
  ⊘ not doing - no screen-change handling exists today, AppKit moves the titled window by itself, and the overlay is fitted again on the next switch to hidden mode (D-fit); reopen if the overlay is reported stranded off screen after unplugging a display
  (2026-10-03)

D-status-order: How do several statuses share the one status line?
  ✓ problems first - status_line puts capture-source Errors right after the profile name, then Warn, then Info, each group in the fixed source order SystemAudio, Mic, Asr, Llm, App, and the menu-bar label truncates at the front so the tail never hides an error verify: the Oct 5 status sequence test in model.rs
  ✓ menu-bar mark and disabled menu items - a ! on the circle and one disabled menu item per down source carry the full error text even with both windows closed
  ✓ clear statuses when a meeting starts - a stale Error from the previous meeting would keep the line red in a healthy one
  ✗ first-seen order - on 2026-10-05 the healthy App/ASR/LLM Info texts pushed the Screen Recording error past the right edge of the label, and a Them-less meeting of 73 minutes went by unnoticed
  (2026-10-06)

## Capture

D-system-audio: How is the other side's audio captured?
  ✓ ScreenCaptureKit default plus a backend switch - needs only the Screen Recording grant, and the fallbacks were kept on purpose (2026-10-02) ⚠ macOS shows a recording indicator and may ask to re-approve the grant periodically verify: how often macOS 15.6.1 re-asks
  ✗ ScreenCaptureKit only - was the initial recommendation
  ✗ cpal loopback as default - it builds a Core Audio tap, which has reported silent-failure cases after an unclean exit
  (2026-10-02)

D-coreaudio-tap: Should the app also carry its own Core Audio process tap built on objc2-core-audio?
  ✗ hand-written tap - cpal 0.18.2 already implements the same tap plus aggregate device in its loopback module (cpal source, loopback.rs)
  ⊘ not doing - the cpal_loopback backend covers this path; reopen if the cpal loopback backend cannot deliver audio
  (2026-10-02)

D-permissions: How are missing permissions handled?
  ✓ preflight and request - check Screen Recording before starting the system stream, request it once, tell the user to restart, and keep the meeting running with Me only
  ✗ let the stream fail - the error text from the framework does not say what to do
  ✗ onboarding window - Phase 8
  (2026-10-02)

D-mic-silence: How is a denied microphone detected?
  ✓ digital-silence check - 3 s of samples from the Me source that are all exactly zero shows a status warning verify: a denied microphone delivers zeros instead of an error
  ✗ ask AVFoundation for the authorization status - needs another framework binding for one call
  (2026-10-02)

D-device-change: What happens when the microphone changes (AirPods connect)?
  ✓ rebuild on stream errors - a DeviceNotAvailable or StreamInvalidated error sets an atomic flag and a helper thread selects the device afresh, so the mic_select rules run again after the change
  ✗ rely on cpal rerouting - corrected 2026-10-06: cpal 0.18.2 does not move an input stream to a new default device by itself, the rebuild is the only reroute path; a stream on a device that merely stopped being the default keeps streaming until it errors
  ✗ Core Audio property listener - extra unsafe code for the rarer half of the case
  (2026-10-02, updated 2026-10-06)

D-mic-bluetooth: Which input does the Me source open?
  ✓ three-state mic_device - absent means Auto (system default, unless its transport is Bluetooth, then prefer BuiltIn over Usb over other wired; Warn if only Bluetooth remains), "default" always follows the system default, any other string opens that exact name
  ✓ transport by device UID through CoreAudio - cpal exposes no transport, so enumerate kAudioHardwarePropertyDevices and match kAudioDevicePropertyDeviceUID; the translate-uid property answers 'what' on current macOS
  ✓ never auto-pick Virtual/Aggregate/Continuity/AirPlay/Unknown - those inputs would silently record silence, so Auto only steers onto BuiltIn/Usb/Other and names the choice in the Mic status
  ✗ always open the system default - opening a Bluetooth headset mic drags the whole device from A2DP into HFP, so every output on the headset drops to ~16 kHz for the rest of the meeting (reproduced live 2026-10-06)
  ✗ steer whenever the device name looks like a headset - name matching breaks on renamed devices; the transport is the ground truth
  (2026-10-06)

D-watchdog: What happens when the system-audio stream stops delivering?
  ✓ restart on stop error - when ScreenCaptureKit reports that the stream stopped, a helper thread restarts it, up to 5 times without a sample in between; a no-samples timer exists but is off by default ⚠ a stream that goes quiet without reporting a stop is not recovered until the timer is switched on
  ✗ no-samples timer of 5 s on by default - nobody has checked that ScreenCaptureKit keeps delivering buffers while nothing plays, and if it does not, every quiet stretch would restart the stream verify: play nothing for 30 s and count buffers, then set audio.watchdog_silence_secs to 5
  ✗ no watchdog - a reported macOS bug makes long captures die without notice
  ✗ restart forever - a missing permission would loop
  (2026-10-02)

D-sleep-wake: Is sleep or display change handled specially?
  ✗ listen for sleep and display notifications - more AppKit code for a rare case
  ⊘ not doing - the watchdog and the mic rebuild cover a stopped stream; reopen if a meeting breaks after sleep
  (2026-10-02)

D-dev-signing: How is the dev app signed so macOS remembers the Microphone and Screen Recording grants?
  ✓ self-signed local certificate - chosen deliberately (2026-10-02); free and stable across rebuilds verify: codesign accepts the untrusted self-signed identity without a trust step
  ✓ dedicated build keychain with a root CA (2026-10-06) - scripts/make-dev-cert.sh keeps the identity in ~/Library/Keychains/clueless-build.keychain-db (passphrase file next to it, 0600) instead of the login keychain, so Security Agent dialogs never interrupt development, and signs a "Clueless Dev Root CA" leaf with an 825-day validity (the maximum macOS accepts); set-key-partition-list runs unattended because the keychain password is on disk ⚠ grants belong to the leaf's stable subject, so renewing must reuse the CA
  ✗ Apple Development certificate - needs an Apple ID set up in Xcode on the build machine
  ✗ ad-hoc signing - the signature changes every build, so macOS can forget the grants
  ✗ run the bare binary from a terminal - the grants then belong to the terminal app
  ✗ the leaf alone in the login keychain - every unattended set-key-partition-list prompted for a password, and a bare self-signed leaf is not accepted as an Authority by codesign's default policy (2026-10-06)
  (2026-10-02, updated 2026-10-06)

D-app-launch: How is the dev app started?
  ✓ launch the bundle with open - macOS then treats the app as responsible for its own permission prompts verify: prompts name clueless, not the terminal
  ✗ execute the binary inside the bundle from a shell - the terminal becomes the responsible process
  (2026-10-02)

## Segmenter

D-dsp-threading: Where do resampling and voice detection run?
  ✓ one std thread per stream - the ring buffer has no async wake-up, and one VAD call takes about 85 microseconds, so a plain loop with a 5 ms sleep is enough (probe 2026-10-02)
  ✗ tokio tasks - blocking CPU work and polling on the async runtime delays network tasks
  ✗ inside the audio callback - model inference allocates and can block, which breaks audio
  (2026-10-02)

D-vad: Which voice activity detector?
  ✓ voice_activity_detector - bundles the Silero v5 model, links ONNX Runtime statically, and scored silence at 0.04 and speech at 0.95 or more in a local probe (probe 2026-10-02) ⚠ the first build downloads about 68 MB of ONNX Runtime, and it pins ort to exactly 2.0.0-rc.10
  ✗ silero-vad-rust - needs an ONNX Runtime dynamic library installed separately
  ✗ webrtc-vad - unmaintained since 2019
  (2026-10-02)

D-resampler: How is audio converted to 16 kHz?
  ✓ rubato Fft - the fixed-ratio resampler, 4 microseconds per call, and with 48 kHz input and chunk 1536 it yields exactly one 512-sample frame per call (probe 2026-10-02)
  ✗ integer decimation by 3 - only works for 48 kHz, and AirPods deliver 16 or 24 kHz
  ✗ rubato 0.x API from online tutorials - does not compile on 5.x
  (2026-10-02)

D-segment-params: Which segmenter numbers?
  ✓ research table with corrections - end of utterance is 19 frames (608 ms) because 600 ms is not a whole number of 32 ms frames, pre-roll is 10 frames (320 ms), and minimum speech is 8 frames (256 ms) so that "yes" and "no" survive
  ✗ research table unchanged - 400 ms minimum speech drops one-word answers
  ✗ tune on real recordings first - no real recordings exist yet
  (2026-10-02)

D-forced-cut: How is an utterance longer than 15 s cut?
  ✓ overlap only outside a pause - cut at the lowest-probability frame of the last 2 s, and carry 1.0 s of audio into the next piece only when that frame is still speech (p at least 0.35)
  ✗ always carry 1.0 s as originally planned - when the cut lands in a pause this creates duplicate words that the dedup step then has to remove
  ✗ never overlap - a cut inside a word loses that word
  (2026-10-02)

D-dedup: How are duplicate words at a forced cut removed?
  ✓ word-suffix match up to 8 words - compare normalized words of the end of piece A with the start of piece B and drop the match from B ⚠ fails when the server writes the overlap differently in the two pieces
  ✗ ASR word timestamps - the server response has no timestamps (probe 2026-10-02 returned only text, logprobs null, usage null)
  (2026-10-02)

## Engine and transcript

D-timebase: How do the two streams get comparable timestamps?
  ✓ sample count on a meeting clock - each stream adds processed samples to an anchor taken from one shared monotonic clock, and takes a new anchor after any gap
  ✗ sample count only - drifts apart after a ring overflow or a stream restart
  ✗ host timestamps from cpal and ScreenCaptureKit - two different clock sources to convert and no benefit at the 100 ms accuracy the echo filter needs
  (2026-10-02)

D-final-order: How do finished utterances reach the transcript in a stable order?
  ✓ one final worker per stream - finals of one speaker are sent one at a time in sequence order, so they commit in order with no hold-back buffer ⚠ lines of the two speakers commit in arrival order, so a Me line can appear a line early or late relative to a Them line, and one final that fails all three attempts delays that speaker's later lines by up to about 31 s
  ✗ two concurrent requests per stream as originally planned - a retry lets utterance 2 finish before utterance 1, and the prompt transcript must only ever grow at the end
  ✗ sort by start time on every insert - changes earlier lines of the prompt and destroys the server's prefix cache
  (2026-10-02)

D-interim: How do live partial transcripts work?
  ✓ latest-wins interim slot - each stream keeps one pending interim request, and a newer one replaces an unsent older one; the live ticker was kept (2026-10-02) ⚠ roughly doubles ASR requests while someone talks
  ✗ no interims - considered, and rejected because a silent panel feels broken
  ✗ queue every interim - a slow server would show stale text and delay finals
  (2026-10-02)

D-asr-failure: What happens when a final utterance cannot be transcribed?
  ✓ drop the utterance - after three attempts the line is removed and the status label shows the error ⚠ that part of the meeting is missing from the transcript
  ✗ keep audio in memory and retry when the server returns - that is an offline backlog feature, not in the MVP
  ✗ insert a placeholder line - puts text into the prompt that nobody said
  (2026-10-02)

D-backpressure: What happens when audio or utterances arrive faster than they are consumed?
  ✓ bounded final queue - 16 finals per stream, and when it is full the newest final is dropped with a status error, while a full ring buffer drops new samples and counts them
  ✗ unbounded queue - memory grows without limit while the ASR server is down
  ✗ block the capture thread - the ring then overflows anyway and the timeline breaks silently
  (2026-10-02)

D-echo-filter: How is the other side's voice kept out of the "Me" lines when the user is on speakers?
  ✓ hold Me finals - a Me final waits until the Them stream has processed past its end time, or 3 s, and is dropped when it overlaps Them audio by 70 percent and shares 60 percent of its words with the committed Them lines plus the Them text still in progress; the filter was kept (2026-10-02) ⚠ Me lines appear up to 3 s later, and an echo is missed when no Them text for that stretch exists yet after 3 s
  ✗ no filter, headphones only - was the recommendation, overruled by the user
  ✗ acoustic echo cancellation - cpal does not expose Apple's voice processing unit
  (2026-10-02, build refinements during implementation:  the 60 percent word rule counts a word as reappearing on soundex equality when both words carry at least four letters, because the live ASR heard "the final chains" on the Them track and "Final change" on the echoing mic and the exact-identity rule leaked the echo in the e2e replay; the hold also waits while a Them segment is still open across the Me end time, and a meeting stop no longer cuts a hold short - the stop's drain window waits for the decision and a still-held final drops on cancel, because releasing holds at stop leaked tail echoes like "Have a nice weekend." as Me lines at replay end-of-file)

D-health: How does the user know the servers are reachable?
  ✓ one check at meeting start - GET /v1/models on both servers with a 2 s timeout, and the status label names a server that is offline or a model that is missing
  ✗ poll every 10 s with backoff - planned for later
  ✗ no check - the first failure would only show after someone spoke
  (2026-10-02)

D-replay: How is the pipeline run without a microphone or permissions?
  ✓ paced headless replay - the binary takes one or two WAV files, feeds them at a chosen speed multiple, prints the transcript and exits
  ✗ unpaced replay - the two streams would race, and the echo filter compares their progress
  ✗ a separate test-only binary - a second entry point to keep in sync
  (2026-10-02)

## LLM and prompt

D-llm-client: Which HTTP client code talks to the LLM?
  ✓ reqwest plus eventsource-stream - parsed the real server's stream correctly with default features off (probe 2026-10-02)
  ✗ async-openai - needs its bring-your-own-types feature to send chat_template_kwargs, so it adds a dependency without removing code
  ✗ reqwest-eventsource 0.6 - requires an older reqwest
  (2026-10-02)

D-thinking: How is model reasoning handled on the live path?
  ✓ off per request - chat_template_kwargs enable_thinking false worked on the real server (probe 2026-10-02)
  ✗ leave it on and hide it - seconds of delay before the first visible token
  ✗ filter think tags out of the content on the client - the server already delivers reasoning in a separate field (probe 2026-10-02)
  (2026-10-02)

D-prompt-shape: How are the messages arranged so the server can reuse its cache?
  ✓ one system and one user message - the user message is the transcript followed by the task, so the text up to the end of the transcript only ever grows
  ✗ several user messages in a row as originally planned - a chat template may reject or merge consecutive user messages verify: not tested on this template
  ✗ task first, transcript last - the changing part would come first and nothing could be cached
  (2026-10-02)

D-output-format: What answer shape does the system prompt ask for?
  ✓ one sentence then bullets - a direct answer sentence, then at most 3 short lines starting with a dash, at most 60 words, plain text, in the language of the conversation
  ✗ 2 to 3 bullets only - the initial research stated both; the sentence-first form reads faster
  ✗ markdown - the text view shows plain text
  (2026-10-02)

D-trigger: What starts a suggestion?
  ✓ manual hotkey - no false positives and no surprise load on the server
  ✗ automatic question detection - planned for later, and a wrong trigger covers the screen at the wrong moment
  (2026-10-02)

D-trigger-pending: What does a trigger use when Them is still talking or the final is still being transcribed?
  ✓ interim text in the tail - the latest interim text goes into the last part of the user message, after the stable transcript
  ✗ wait for the final - adds the 608 ms silence wait plus ASR time to every trigger
  ✗ finals only - the answer would ignore the question that was just asked
  (2026-10-02)

D-llm-failure: What does the user see when the LLM request fails?
  ✓ show the error and keep partial text - connect failure shows "LLM offline", an HTTP error shows the status code, and a stream that stalls for 10 s ends with the text so far marked as interrupted
  ✗ retry automatically - the moment has passed, and the user can press the hotkey again
  (2026-10-02)

D-compression: What happens when the transcript gets too long for the context window?
  ✓ summarize the oldest half - above an estimated 90000 transcript tokens one LLM call turns the oldest half of the lines into a summary block; the originally planned behaviour was kept (2026-10-02) ⚠ the cache is rebuilt once after each compression, and the path only runs after roughly 6 hours of speech, so only tests will exercise it
  ✗ drop the oldest half without a summary - was the initial recommendation
  ✗ exact token counting - an estimate of characters divided by 3.5 is enough at this margin
  (2026-10-02)

D-profile: How do the user's own notes get into the prompt?
  ✓ one text file - an optional path in the config, read at meeting start and placed in the system message
  ✗ document upload UI - no settings window in this slice
  ✗ no profile - the answers would not know who the user is
  (2026-10-02)

## Session traces

Promoted from recorded-session-traces/spec.md on 2026-10-05, before the change was built.

D-measure-approach: How do we get a way to tell whether a change made the product better? (2026-10-05, recorded-session-traces/spec.md)
  ✓ record real sessions, re-run them on the current build, compare the two traces - the request, and the only option that uses real meetings (user 2026-10-05) ⚠ a re-run talks to live servers, so two re-runs of the same session differ a little; compare a run against a second run to see how much
  ✗ grow the hand-made WAV fixtures - eight scripted files exist (`fixtures/README.md`) and the ledger already records that synthetic speech is cleaner than a real meeting (`docs/decisions.md`, D-fixtures)
  ✗ send traces to an outside observability tool - the product promise is that data stays on the user's own machines (`README.md`, "runs entirely on your own infrastructure"), and such tools do not store audio for a re-run

D-recording-default: What is recorded without the user doing anything? (2026-10-05, recorded-session-traces/spec.md)
  ✓ text always, audio opt-in: text trace always, audio only with `[trace] audio = true` - the user's answer in their own words (user 2026-10-05) ⚠ a session without audio cannot be re-run in this slice, so only meetings where audio was switched on feed the comparison
  ✗ everything on by default - rejected by the user; stores other people's voices without a decision per meeting
  ✗ everything off by default - rejected by the user; the corpus would only hold meetings chosen in advance

D-trace-tap: Where does the engine hand data to the trace? (2026-10-05, recorded-session-traces/spec.md)
  ✓ opener in EngineDeps: a trace opener in `EngineDeps`; the engine opens one sink per meeting, wraps the UI sink once so every `UiEvent` is also recorded, and calls the sink at the seams the UI never sees - `EngineDeps` is the existing injection point (`EngineDeps::production`), and every UI emitter reads `deps.ui` (`Engine::emit`, `StreamPipes`, `WorkerCtx`, `suggest::run`, `compress::run`) ⚠ an answer piece appears twice, once as it arrived and once as it was shown
  ✗ wrap the UI sink only - the prompt, the unfiltered answer, drop reasons, policy outcomes, speech call timings and audio never pass through it (`UiEvent` has no such variants)
  ✗ a tracing-subscriber layer - ties the file format to the wording of log lines, and log fields cannot carry audio

D-trace-module: Which crate owns the record types, the writer, the reader and the compare? (2026-10-05, recorded-session-traces/spec.md)
  ✓ new pure crate: new pure crate `trace`, with edges `trace -> types`, `engine -> trace`, `app -> trace` - one owner for the on-disk format, usable by engine tests and by the binary; auto-applied at Confidence: 80% ⚠ the crate must be added to `docs/dependencies.md`, the purity test and the dependency checker
  ✗ inside `engine` - the binary's list, show, delete and compare modes would pull the whole engine to read a file, and the format would sit next to thread code
  ✗ record types in `types`, writer in `app` - engine tests could not write or read a trace without the binary

D-trace-format: What is the on-disk format of one session? (2026-10-05, recorded-session-traces/spec.md)
  ✓ directory with JSON lines and WAV: a directory with `manifest.json`, `events.jsonl` (one JSON record per line, append only) and `audio/me.wav`, `audio/them.wav` - readable with `jq` and a text editor, an interrupted write damages at most the last line, and `serde_json` and `hound` are already workspace dependencies (`Cargo.toml`)
  ✗ one SQLite file - a new dependency and a binary file for data that is only appended and read front to back
  ✗ one binary container - needs a tool to read every time someone wants to look

D-audio-content: Which audio is stored, and how is it laid out in time? (2026-10-05, recorded-session-traces/spec.md)
  ✓ 16 kHz on the meeting timeline: the 512-sample 16 kHz frames that come out of the resampler, one WAV per speaker, each frame placed at sample index `t_start_ms * 16`, zeros where no audio arrived - the file replays through the existing `WavSource` unchanged, both speakers stay aligned, a 16 kHz file passes through the resampler bit for bit (`Resampler16k::new`, test `input_at_16k_returns_frames_identical_to_the_input`), and the time range of any utterance can be cut from the file; auto-applied at Confidence: 78% ⚠ a change to the resampler cannot be evaluated; ⚠ a stretch with no samples becomes zeros, so on a re-run it takes the normal silence path instead of the `Empty` path that re-anchors the clock
  ✗ raw device-rate samples plus anchor records - a device change gives a new sample rate mid-file (`SourceRead::Reset`), and a new replay source that re-creates gaps and resets would be needed
  ✗ one WAV per segment sent to the speech server - the segmenter and voice detector could not be re-run, and audio between utterances is lost

D-audio-backwards-time: What does the audio writer do when a frame's time is earlier than what is already written? (2026-10-05, recorded-session-traces/spec.md)
  ✓ append and anchor: append the frame anyway, never overwrite, and write an `audio_anchor` record when the distance between the frame's time and its place in the file changes - `StreamClock::anchor_now` can move time back after a backlog (`close_and_reanchor`), and the anchor records keep the exact mapping ⚠ after such a jump that speaker's file runs ahead of the other speaker's by the size of the jump, and the re-run does not correct for it
  ✗ overwrite earlier samples - destroys audio that was fed to the pipeline
  ✗ an anchor for every frame that is off - after one backwards jump every later frame is off, which would write 31 records per second

D-audio-sample-format: 16-bit integers or 32-bit floats on disk? (2026-10-05, recorded-session-traces/spec.md)
  ✓ 16-bit: 16-bit - half the size (about 115 MB per hour per speaker), and the speech server already receives 16-bit audio (`encode_wav`); auto-applied at Confidence: 80% ⚠ the voice detector on a re-run sees samples rounded to 16 bits, not the exact floats of the live run
  ✗ 32-bit float - exact, at about 230 MB per hour per speaker

D-client-detail: How deep into the HTTP clients does the trace go? (2026-10-05, recorded-session-traces/spec.md)
  ✓ LLM full, speech per call: LLM in full: request body, every content piece and reasoning piece with its arrival time, finish reason, token usage, the unfiltered answer, the real error. Speech: one record per call with timing, raw text and outcome - the user's choice (user 2026-10-05) ⚠ `LlmClient::stream` and `LlmClient::complete` change their return types; ⚠ speech retries stay invisible
  ✗ engine level only - rejected by the user; reasoning, finish reason and usage are thrown away inside `LlmClient::stream`
  ✗ every speech attempt with status and raw body - rejected by the user for this slice

D-llm-usage: How does token usage get into the stream? (2026-10-05, recorded-session-traces/spec.md)
  ✓ include_usage: requests send `stream_options: {"include_usage": true}` unless the new variable `LLM_INCLUDE_USAGE=false` is set; usage is read leniently, so a missing or odd usage object gives no usage and never an error - the configured server accepts the option and returns a last chunk with `usage` and an empty `choices` list (one request sent to `LLM_BASE_URL` on 2026-10-05: HTTP 200, usage chunk present); a server setting belongs in `.env`, next to `LLM_ENABLE_THINKING`, which is omitted for the same reason (`ChatRequest::new`); auto-applied at Confidence: 85% ⚠ a server that rejects the unknown field fails every request until the variable is set ? verify: no second server was tried
  ✗ send it always with no switch - a strict server would break suggestions with no way out
  ✗ retry once without the field on HTTP 400 - a second request the user did not ask for, and a 400 has other causes

D-replay-meaning: What is a re-run in this slice? (2026-10-05, recorded-session-traces/spec.md)
  ✓ live re-run: the session's audio goes through the current build against the live servers and a new trace is written - the user's choice (user 2026-10-05) ⚠ not repeatable: server answers and thread timing differ between runs
  ✗ offline replay from recorded server answers - see D-deterministic-replay (⊘ not doing)

D-replay-inputs: Which recorded inputs does a re-run reuse, and which come from the current setup? (2026-10-05, recorded-session-traces/spec.md)
  ✓ audio, notes, profile, commands: from the session: audio, notes text, start profile, and the commands Suggest, ClearSuggestion, CycleProfile and SetProfile at their recorded meeting times. From the current setup: servers, models, prompt code, tuning - Manual is the default profile and asks only on the hotkey (`AssistProfile::default`, `run_replay`), so without the recorded presses a re-run has no suggestions to compare; auto-applied at Confidence: 85%
  ✗ audio only - no suggestions in Manual sessions, and the prompt would miss the notes

D-replay-end: When does a re-run end? (2026-10-05, recorded-session-traces/spec.md)
  ✓ drained, sent, closed: when the sources are drained, every recorded command has been sent, and no suggestion is open; commands still waiting when the sources drain are sent at once, in order - the existing `--ask` replay sends its Suggest after `SourcesDrained` (`replay_loop`), so that command is recorded after the end of the audio and a re-run that stopped at the drain would never send it
  ✗ stop at `SourcesDrained` - loses every command recorded after the last audio

D-replay-pacing: How fast does a session re-run go? (2026-10-05, recorded-session-traces/spec.md)
  ✓ real time, speed allowed: real time by default; `--speed N` is accepted and prints a warning - the echo hold, turn settling and minimum gap run on the real clock (`EngineTimings`), so only speed 1 keeps their meaning ⚠ a one hour meeting takes one hour to re-run
  ✗ a virtual clock through the engine - see D-deterministic-replay (⊘ not doing)

D-replay-limits: The replay stops after 900 s and loads each WAV into memory. What changes? (2026-10-05, recorded-session-traces/spec.md)
  ✓ limit from duration: a session re-run gets a limit of the longest audio file's length divided by speed plus 120 s; memory use is accepted - `REPLAY_LIMIT` is 900 s and `WavSource::open` decodes the whole file ⚠ about 230 MB of memory per speaker per recorded hour
  ✗ a streaming replay source - no meeting longer than two hours has been recorded yet; reopen when a re-run uses more than 1 GB of memory

D-replay-output: Where does the trace of a re-run go? (2026-10-05, recorded-session-traces/spec.md)
  ✓ runs directory: `<session>/runs/<run id>/` with its own `manifest.json` and `events.jsonl`, no audio copy - runs stay attached to the session they came from and the sessions list only shows real meetings; auto-applied at Confidence: 80%
  ✗ a new top-level session naming its source - mixes re-runs into the corpus

D-compare-metric: What does compare report? (2026-10-05, recorded-session-traces/spec.md)
  ✓ diff plus judge: per speaker: number of finals, number of words, word distance between the two transcripts and that distance divided by the baseline word count; drops by reason; speech call time and time to first answer piece (median and 95th percentile); suggestions paired and printed side by side with a judge verdict per pair and a tally - the user picked diff plus judge (user 2026-10-05) ⚠ transcript numbers say how different, not which is right
  ✗ exact event equality - two runs of the same build already differ
  ✗ a hand-corrected reference transcript per session - the user picked the judge instead (user 2026-10-05); reopen when a transcript difference cannot be explained by reading both sides

D-judge: How are two suggestions judged? (2026-10-05, recorded-session-traces/spec.md)
  ✓ two requests per pair, both orders: each pair is sent twice to the configured LLM server through `LlmClient::complete`, one request at a time, temperature 0, with the same thinking setting as live requests; the judge sees the last 6000 characters of the baseline's transcript up to the suggestion (built from its `transcript_final` records as `Me:` and `Them:` lines) and the two answers labelled A and B, once in each order, and must answer with JSON `{"winner": "A" | "B" | "tie", "reason": "..."}`; the same winner in both orders is the verdict, anything else is a tie marked `order-dependent` - a model that prefers a position cannot produce a winner this way; one at a time stays inside the server's concurrency limit (C-server-read-only) ⚠ twice the requests; ⚠ the judge is the same model that wrote the answers ? verify: the judge agrees with the user's own pick on a sample of pairs; ⚠ the candidate's answer was written from the re-run's transcript but is judged against the baseline's, so a better transcript in the candidate can count against it; ⚠ compare needs the LLM server unless `--no-judge` is given
  ✗ one request per pair with the order swapped on every second pair - balances a position preference only over many pairs, not for one pair
  ✗ judge in parallel - breaks the concurrency cap of C-server-read-only
  ✗ a numeric score per answer - scores from one model are not stable across calls; a forced choice is easier to check by hand
  ✗ the tail of the recorded user message as context - it ends with the app's own instruction text, not the conversation (`prompt::build_for`)

D-judge-home: Which crate holds the judge? (2026-10-05, recorded-session-traces/spec.md)
  ✓ engine: `engine`, as `engine::judge` - the engine already depends on `llm`, and no new edge is needed beyond `engine -> trace`; auto-applied at Confidence: 75%
  ✗ add the edge `app -> llm` - the binary has no edge to a client crate today (`docs/dependencies.md`)
  ✗ a new `eval` crate - a second new crate for one function

D-inspect-tools: Which commands look at and manage sessions? (2026-10-05, recorded-session-traces/spec.md)
  ✓ list, show, delete: `--sessions` lists, `--show SESSION` prints a session as transcript with suggestions, `--delete SESSION --yes` removes one; they run before the config and the server settings are read - pulled in by the user (user 2026-10-05); `main` loads the env file and config before it looks at the mode today, and reading files must not need a server address ⚠ `--sessions` scans every `events.jsonl` line by line to count; reopen with a summary file written at close when listing takes more than 2 s
  ✗ export to other formats - `events.jsonl` is already plain text; reopen when a second tool needs a different format

D-delete-safety: What stops `--delete` from removing the wrong thing? (2026-10-05, recorded-session-traces/spec.md)
  ✓ `--yes`, manifest, lock: without `--yes` it prints the path and size and exits 1; it only removes a session or a run directory whose `manifest.json` parses as a trace manifest and which holds an `events.jsonl`; it refuses when that file, or the `events.jsonl` of any run below it, is locked by a running recorder - deleting is not reversible, and `manifest.json` alone is a common file name; auto-applied at Confidence: 85%
  ✗ delete at once - one typo removes a meeting

D-cli-surface: How are the new modes started? (2026-10-05, recorded-session-traces/spec.md)
  ✓ flags on the binary: flags on the `clueless` binary: `--data-dir PATH`, `--replay-session SESSION`, `--compare A [B] [--no-judge]`, `--sessions`, `--show SESSION`, `--delete SESSION [--yes]` - the binary already has a hand-written parser and a replay mode (`cli::parse`) ⚠ `Cli.replay: Option<Replay>` becomes a `Mode` enum, which changes the existing parser tests
  ✗ xtask subcommands - xtask has no dependencies and talks to no crate (`docs/architecture.md` Components)
  ✗ let `--replay` detect a directory - `--replay` takes up to two positional WAV paths (`take_wav_paths`); a directory there is ambiguous

D-writer-threading: How do records get to disk without stalling audio threads? (2026-10-05, recorded-session-traces/spec.md)
  ✓ two bounded queues, visible drop: per session one thread writes records from a queue of 4096 and a second thread writes audio frames from a queue of 4096; callers use `try_send`; a full queue drops the message and counts it, and the record thread writes a `records_lost` record with both counts - the existing rule is never block the audio path and make the loss visible (`RingWriter::push`, `send_segment`); with separate queues a slow audio write cannot push out text records, which are the tier that is always on; 4096 frames are about one minute of audio for two speakers
  ✗ one shared queue - 62 audio frames per second would fill it and text records would be dropped first
  ✗ unbounded queue - memory grows without limit when the disk stalls
  ✗ write on the calling thread - a slow disk would stall a stream thread and overflow the 5 s ring

D-timestamps: Which clocks stamp a record? (2026-10-05, recorded-session-traces/spec.md)
  ✓ at_ms plus one wall-clock start: `at_ms` since the session opened on every record; a `clock_started` record marks when the meeting clock began; the manifest holds one wall-clock start as Unix milliseconds and the replay `speed` (1 for a live meeting); the meeting time of a record is `(at_ms - at_ms of clock_started) * speed`; utterance times stay in meeting milliseconds as the engine made them - the engine has no wall clock (`MeetingClock`), and a replay releases audio `speed` times faster than the wall clock (`WavSource::read`), so without the speed the two kinds of time cannot be compared
  ✗ wall clock on every record - a clock adjustment mid-meeting would reorder records
  ✗ a second time field on every record - the conversion is one multiplication

D-crash-safety: What is left on disk when the app is killed mid-meeting? (2026-10-05, recorded-session-traces/spec.md)
  ✓ flush lines and header: every JSON line is flushed when written; the WAV header is flushed every 80,000 samples (5 s) - `WavWriter::flush` writes a valid header so the file is readable up to the last flush; a WAV never flushed reads as zero samples ⚠ no `fsync`, so a power loss can lose the last seconds
  ✗ finalize at stop only - a kill would leave every audio file unreadable

D-close-ordering: When is the trace closed? (2026-10-05, recorded-session-traces/spec.md)
  ✓ close before Idle: in `stop_meeting`, after the pipeline has stopped and before `MeetingState::Idle` is sent; the whole close, sending included, gives up after 2 s - the overlay exits the process once it sees `Idle`, with `std::process::exit(0)`, which runs no destructors; a close without a deadline could keep the engine from ever reporting `Idle` ⚠ the `Idle` event of a meeting is not in its trace; the `end` record stands for it; ⚠ on quit the overlay exits after 6 s whatever happens (`request_quit`, `QUIT_DEADLINE_SECS`), and a slow stop can take longer, so that session loses its `end` record and up to 5 s of audio and is shown as cut off
  ✗ close in a destructor - never runs on a normal quit
  ✗ write the `end` record at the start of `stop_meeting` - the finals flushed during the stop would come after the end

D-session-end: What marks a session as finished cleanly? (2026-10-05, recorded-session-traces/spec.md)
  ✓ end record: a last `end` record with a reason: `stop`, `shutdown`, `panic`, `channel_closed` or `start_failed`; a trace without one is shown as cut off - there are five ways a meeting ends (`stop_meeting` callers and the no-sources return in `start_meeting`)
  ✗ a marker file - a second thing to keep in step with the record file

D-write-failure: What happens when the trace cannot be written? (2026-10-05, recorded-session-traces/spec.md)
  ✓ warn and continue: at open: one Warn status from `StatusSource::App`, and the meeting runs unrecorded. Mid-session: one Warn status, the writer stops, the meeting goes on. For `--replay-session` a trace that cannot be opened ends the command with exit code 1 - a meeting must not die because recording did (the notes file follows the same rule, `read_notes`); a re-run without a trace has no purpose
  ✗ fail the meeting - loses the meeting to save its recording

D-serde-types: How do engine types reach the file? (2026-10-05, recorded-session-traces/spec.md)
  ✓ own record types: the `trace` crate has its own record types with serde derives and converts from `UiEvent` and `EngineCommand`; a stored version 1 trace under `fixtures/trace/v1/` must keep parsing in a test - renaming an engine enum cannot change the file format without a failing test; auto-applied at Confidence: 78% ⚠ about 150 lines of conversion code to keep in step with `UiEvent`
  ✗ serde derives on the types in `crates/types` - every refactor of an engine enum would silently change old traces' meaning

D-schema-version: How does a later build read an old trace? (2026-10-05, recorded-session-traces/spec.md)
  ✓ version, tolerant reader: `schema: 1` in the manifest; the reader refuses a higher number, ignores unknown fields, maps an unknown record kind to `unknown`, and ignores a last line that does not parse - new record kinds can be added without breaking old readers
  ✗ no version - the first format change makes old sessions unreadable without a way to tell

D-drop-reason: How is the reason for a dropped utterance recorded? (2026-10-05, recorded-session-traces/spec.md)
  ✓ trace-only record: a trace-only `utterance_dropped` record with a reason: `cancelled`, `no_speech`, `asr_error`, `empty_after_overlap`, `echo` or `queue_full` - nine `drop_final` call sites and one in `send_segment` emit the same `TranscriptDropped` today
  ✗ add the reason to `UiEvent::TranscriptDropped` - changes the overlay for data it does not show

D-policy-trace: How much of the trigger policy is recorded? (2026-10-05, recorded-session-traces/spec.md)
  ✓ pieces and three outcomes: a `piece_done` record per finished piece and a `policy` record at the three outcomes the engine already logs (`waiting`, `fired`, `paused`) - these are computed today in `Engine::pump` and `log_pause`
  ✗ a reason for every skip - `AutoPolicy::poll` returns no reason and changing it is a separate design; reopen when a missing automatic suggestion cannot be explained from the trace

D-suggestion-detail: What extra facts are stored per LLM call? (2026-10-05, recorded-session-traces/spec.md)
  ✓ call facts: a call id, the purpose (`suggestion`, `compress`), the suggestion id, origin and profile on the request record; on the end record the outcome, the full error text with the first bytes of the server's answer, finish reason, usage, the raw text, the shown text, whether the answer was a PASS, the time to the first content piece and the time to the first reasoning piece - `suggest::run` knows neither origin nor profile, and `map_llm_error` folds three errors into `Interrupted`; the error detail goes to the trace only, the text shown to the user stays as it is
  ✗ the `SuggestionEnd` event only - loses the PASS text and the real error
  ✗ put the server's answer into the `LlmError` display text - the compression failure status would then show server text in the window

D-compress-trace: What is recorded when the transcript is summarized? (2026-10-05, recorded-session-traces/spec.md)
  ✓ request, end, lines replaced: the request and end records of the call plus a `summary_applied` record with the number of lines replaced - `compress::run` emits nothing on success, and `set_summary` removes the old lines for good
  ✗ nothing - a later prompt in the same trace could not be explained

D-prompt-storage: Is the full LLM request stored on every call? (2026-10-05, recorded-session-traces/spec.md)
  ✓ full body: the full JSON body every time - each record can be read alone, and it is the exact body sent ⚠ in Brainstorm the transcript is stored again with every request, so the record file grows with the square of the meeting length ? verify: size of `events.jsonl` after a one hour Brainstorm meeting
  ✗ store only what was added since the last request - C-prompt-prefix-stable would allow it; reopen when one session's `events.jsonl` passes 200 MB

D-manifest-contents: What does the manifest hold? (2026-10-05, recorded-session-traces/spec.md)
  ✓ settings and commit: schema, session id, start as Unix milliseconds, origin (`live`, `replay_wav`, `rerun`), replay speed, source session for a re-run, app version, git commit, audio on or off, the speakers the source factory lists, start profile, LLM settings (base URL, model, max tokens, temperature, thinking switch, usage switch), speech settings (base URL, model, language), voice detector settings, every `EngineTimings` value in milliseconds, the compression threshold - a trace cannot be read correctly without the settings it ran with, and the commit pins the prompt text compiled into the build ⚠ a commit ending in `-dirty` does not pin anything
  ✗ a `Debug` dump of `Config` - not parseable, and it changes whenever a field is renamed

D-secrets: How are API keys kept out of the trace? (2026-10-05, recorded-session-traces/spec.md)
  ✓ no key field: the manifest type has no key field, request bodies never contain the key (it travels in a header set by `LlmClient::new`), user and password and the query string of a base URL are replaced by `***`, and a test searches every written file for both keys - the repo already forbids the key in `Debug` output (`LlmConfig` Debug impl)
  ✗ rely on the redacting `Debug` impls - they protect log lines, not serde output

D-session-identity: What is one session and how is its directory named? (2026-10-05, recorded-session-traces/spec.md)
  ✓ one meeting, UTC name: one meeting; `sessions/<UTC start as 2026-10-05T14-03-22Z>`, made with `create_dir`, with `-2`, `-3` appended when the name exists - sorts by time, needs no new dependency, and two processes cannot share a directory (replay mode takes no lock, `run_gui` does); auto-applied at Confidence: 85% ⚠ UTC, not local time, because local time needs a time zone library
  ✗ one directory per app run - utterance numbers and line ids restart per meeting (`Machine`, `TranscriptStore::new`)
  ✗ a uuid - a new dependency and names that do not sort

D-trace-switches: Which switches exist? (2026-10-05, recorded-session-traces/spec.md)
  ✓ two TOML keys and a flag: `[trace] enabled = true` and `[trace] audio = false` in the TOML, and `--data-dir PATH` on the command line (default `~/.clueless`) - app behaviour belongs in TOML (`docs/configuration.md`), and a path override is a launch detail like `--log-file`; auto-applied at Confidence: 80%
  ✗ environment variables - the `.env` file is reserved for servers, models and secrets

D-audio-indicator: Does the user see that the meeting is being stored? (2026-10-05, recorded-session-traces/spec.md)
  ✓ one Info status: one Info status from `StatusSource::App` at every recorded meeting start, `recording to <session directory>`, with ` (with audio)` added when audio is written; the text comes from what the sink reports, not from the config - the user is responsible for consent (`README.md`, recording notice), and the text trace holds other people's words and the user's notes just as the audio holds their voices; auto-applied at Confidence: 80% ⚠ one more status line at each meeting start
  ✗ a status only when audio is on - the text trace starts on the first launch after the upgrade with no sign at all
  ✗ nothing - a forgotten config key would record every meeting unnoticed

D-file-permissions: Who can read the trace files? (2026-10-05, recorded-session-traces/spec.md)
  ✓ 0700 and 0600: every directory the app creates is 0700 and every file 0600, through one helper used for the manifest, the record file, the audio files and the notes copy of a re-run - the files hold other people's words and voices; auto-applied at Confidence: 85% ⚠ a data directory that already exists with wider permissions is left as it is
  ✗ process default - readable by every account on the Mac

D-wav-replay-records: Does the existing `--replay ME.wav` mode write a session too? (2026-10-05, recorded-session-traces/spec.md)
  ✓ yes, origin replay_wav: yes, with origin `replay_wav` - it is the same engine, it lets a session be built from the fixtures, and it is how the binary tests reach the recorder
  ✗ no - a second code path that behaves differently from a live meeting

D-test-isolation: How do existing tests avoid writing into the real `~/.clueless`? (2026-10-05, recorded-session-traces/spec.md)
  ✓ no-op by default, `--data-dir` in tests: `EngineDeps::production` installs an opener that records nothing and the binary replaces it; engine tests record only when a test asks for the in-memory trace; the one helper that starts the binary in `crates/app/tests/replay.rs` sets `HOME` to a temp directory and passes `--data-dir` inside it, for the ignored live tests too - that helper sets neither today (`run_with_log`), and `EngineDeps` has three struct literals
  ✗ switch recording off in tests through the config file - one forgotten test writes into the user's corpus

D-health-trace: What is recorded for the server checks at meeting start? (2026-10-05, recorded-session-traces/spec.md)
  ✓ the status events they already produce - `health::check` returns `UiEvent::Status` values that pass through the wrapped sink
  ✗ the model list and the check latency - no question so far needed them; reopen when a session with no transcript cannot be explained

D-retention: Is old data ever deleted by the app? (2026-10-05, recorded-session-traces/spec.md)
  ✗ a size limit with oldest-first cleanup - left out by the user (user 2026-10-05)
  ⊘ not doing - audio is off by default and `--delete` exists; reopen when `~/.clueless` passes 20 GB

D-discard-meeting: Is there a menu item to throw away the running session? (2026-10-05, recorded-session-traces/spec.md)
  ✗ a status menu item - left out by the user (user 2026-10-05)
  ⊘ not doing - `--delete` covers it after the meeting; reopen when a session has to be removed while the app keeps running

D-tool-calls: What is recorded for tool calls? (2026-10-05, recorded-session-traces/spec.md)
  ✗ a tool call record kind - `ChatRequest` has no `tools` field and `Role` is only system and user
  ⊘ not doing - there is nothing to record; the full request body is stored, so a future `tools` field appears in the trace by itself; reopen when the LLM client gains tool calls

D-deterministic-replay: Is offline, repeatable replay in this slice? (2026-10-05, recorded-session-traces/spec.md)
  ✗ serve recorded server answers and run on a virtual clock - needs a clock passed through the engine (about ten direct `Instant::now()` calls) and raw speech answers
  ⊘ not doing - the user picked the smaller slice (user 2026-10-05); reopen when run-to-run differences hide the effect of a change

D-llm-only-rerun: Is re-asking the LLM from a recorded transcript in this slice? (2026-10-05, recorded-session-traces/spec.md)
  ✗ a mode that rebuilds prompts from recorded transcript events - a third replay path
  ⊘ not doing - the user picked the smaller slice (user 2026-10-05); reopen when most recorded sessions have no audio and a prompt change needs measuring

D-vad-probabilities: Are the per-frame voice scores stored? (2026-10-05, recorded-session-traces/spec.md)
  ✗ a side file of scores - about 31 values per second per speaker
  ⊘ not doing - they can be computed again from the audio; reopen when two voice detector versions must be compared frame by frame

D-ui-actions: Are overlay-only actions (mode switch, hide, move) recorded? (2026-10-05, recorded-session-traces/spec.md)
  ✗ a path from the overlay to the recorder - the overlay may depend only on `types` (`docs/dependencies.md`)
  ⊘ not doing - these actions never reach the engine and do not change transcript or suggestions; reopen when a UI study needs them

D-plist-strings: Do the permission texts in `macos/Info.plist` change? (2026-10-05, recorded-session-traces/spec.md)
  ✓ add storing: the microphone, audio capture and screen capture texts add "and, when you switch audio recording on, to store it on this Mac" - the current texts say the audio is used only to show the transcript, and Them audio arrives via ScreenCaptureKit by default (`audio.system_audio_backend = "sck"`, `docs/system-audio.md`), whose prompt is `NSScreenCaptureUsageDescription`
  ✗ leave them - they would be untrue with `[trace] audio = true`

## Testing

D-test-levels: What is tested at which level?
  ✓ three levels - unit tests for pure logic, integration tests against a local mock server, and end-to-end replay against a real inference server, with AppKit, hotkeys and live capture on a manual checklist because macOS permission prompts cannot be answered by a test
  ✗ UI automation - needs Accessibility and Screen Recording grants for the test runner
  ✗ mock everything - the real server's field names and timing are part of what can go wrong
  (2026-10-02)

D-test-timing: How do tests exercise timeouts (ASR 10 s, stall 10 s, echo hold 3 s, stop wait 5 s, compression retry 60 s)?
  ✓ injectable durations - every timeout is a field of a timings struct or a client constructor argument, production uses the values documented in the code, and tests pass values of 100 to 300 ms and wait in real time
  ✗ tokio paused time - it auto-advances while real socket I/O is pending, and the stream threads use std time, so results would be wrong or flaky
  ✗ real production durations in tests - a single run would take minutes
  (2026-10-02)

D-mock-server: Which mock HTTP server?
  ✓ axum - it delivered server-sent events with real gaps between them (probe 2026-10-02)
  ✗ wiremock - it only sends a fully buffered body, so stalls and mid-stream drops cannot be tested (probe 2026-10-02)
  (2026-10-02)

D-fixtures: Which audio do the tests use?
  ✓ synthetic fixtures - a script generates English, Dutch and French WAV files with the macOS say command, so the build runs unattended (2026-10-02) ⚠ synthetic speech is cleaner than a real meeting
  ✗ recordings made by the user - blocks the build on manual work
  ✗ downloaded speech corpus - licensing and size
  (2026-10-02)
