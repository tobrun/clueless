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
  ✓ rely on cpal - cpal 0.18 reroutes a default-device stream by itself, and the app rebuilds the stream only on a DeviceNotAvailable or StreamInvalidated error verify: the sample rate after a reroute
  ✗ Core Audio property listener - extra unsafe code for a case cpal claims to handle
  (2026-10-02)

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
  ✗ Apple Development certificate - needs an Apple ID set up in Xcode on the build machine
  ✗ ad-hoc signing - the signature changes every build, so macOS can forget the grants
  ✗ run the bare binary from a terminal - the grants then belong to the terminal app
  (2026-10-02)

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
