# Decisions

## Principles

None promoted yet.
A rationale becomes a principle after it decides a third decision.

## Product and scope

D-build-or-adopt: Should this be a new native Rust app, or an existing open-source overlay pointed at the LAN server?
  ✓ build a native Rust app - the request names a native Rust UI and local servers, and the user picked the whole MVP (user 2026-10-02) ⚠ weeks of work before first use
  ✗ adopt Pluely or Thuki - both are Tauri apps with a web UI in WKWebView, and their NSPanel conversion has known abort bugs
  ✗ adopt free-cluely - Electron app, not Rust, not native
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-first-slice: How much of the research plan goes into this spec?
  ✓ phases 0 to 4 in one spec - user chose it over a thin vertical slice (user 2026-10-02) ⚠ twelve change sets land before the first full live meeting test
  ✗ thin vertical slice with mic only - was the recommendation at Confidence: 60%, overruled by the user
  ✗ all eight phases - screenshots, storage, summary, auto-trigger and packaging are not needed to use the tool in a meeting
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-packaging: Should this slice produce a notarized DMG?
  ✗ Developer ID signing and notarization - a personal tool on one Mac has no Gatekeeper problem to solve (user 2026-10-02)
  ⊘ not doing - reopen if the app must run on a Mac that did not build it
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-persistence: Is anything written to disk?
  ✗ sqlite as in the architecture diagram - belongs to Phase 6
  ✗ append-only JSON lines log - useful, but not asked for
  ⊘ not doing - the transcript lives in memory and is lost on quit or crash; reopen when the post-meeting summary is specced
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-screen-context: Are screenshots sent to the model?
  ✗ screenshot hotkey - Phase 5
  ⊘ not doing - reopen as the next slice after this one works in a real meeting
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-auto-trigger: Should a question from Them fire a suggestion automatically?
  ✗ word-list detector or a one-token classifier call - not needed to use the tool
  ⊘ not doing - reopen after the manual flow has been used in real meetings
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Workspace and tooling

D-crate-layout: One crate or a workspace?
  ✓ cargo workspace of ten crates - cargo then refuses a macOS import inside the pure crates, and parallel change sets own disjoint files; auto-applied at Confidence: 80%
  ✗ single crate with modules - nothing stops audio or AppKit code leaking into the logic that must be testable without permissions
  ✗ the nine crates from the research plan as written - it has no home for shared types or for orchestration, and it includes storage and screen crates this slice does not build
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-engine-boundary: Where does the orchestration live (stream workers, ASR queue, meeting state, suggestion requests)?
  ✓ engine crate behind source traits - it has no macOS dependency, so the whole pipeline runs in tests from WAV files against a mock server; auto-applied at Confidence: 80%
  ✗ inside the app binary crate - every pipeline test would link AppKit and ScreenCaptureKit and could not run headless
  ✗ inside the context crate - mixes pure text logic with threads and network calls
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-dependencies-file: Should module edges be written down as rules?
  ✓ allowlist in docs/dependencies.md - ten new modules appear at once, and the purity rule is the main reason for the layout
  ✗ no file - the rule would live only in this spec and soften over time
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-toolchain: Which Rust setup and test runner?
  ✓ edition 2024 on pinned rustc 1.94.1 with cargo test - all probes compiled on it, and nothing extra has to be installed
  ✗ cargo-nextest - not installed on this Mac
  ✗ unpinned stable - a new compiler could add clippy warnings mid-build
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-config: Where do settings live?
  ✓ TOML file - read once at launch from the path given by --config, else ~/.config/clueless/config.toml; auto-applied at Confidence: 85%
  ✗ settings window - Phase 7
  ✗ environment variables - an app started with open does not inherit the shell environment
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-logging: Where do logs go?
  ✓ tracing to stderr and a file - ~/Library/Logs/clueless/clueless.log, with per-utterance latency fields; transcript text only at debug level
  ✗ stderr only - an app started with open has no visible stderr
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-single-instance: What stops two copies from running?
  ✓ file lock - std File::try_lock on a lock file, second copy exits with a message
  ✗ nothing - two copies would both register hotkeys and both record
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Overlay

D-ui-stack: Which UI technology draws the overlay?
  ✓ objc2 with objc2-app-kit - an NSPanel subclass created with the non-activating style compiled and ran (probe 2026-10-02); auto-applied at Confidence: 90%
  ✗ Tauri with tauri-nspanel - web UI, and it converts a window to a panel at runtime, which is where its crashes come from
  ✗ egui or Slint - the window they create is not an NSPanel, so a click activates the app and takes focus from the meeting
  ✗ gpui or cacao - no official release, or self-described as very early stage
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-overlay-window: Which window settings?
  ✓ status level panel - level NSStatusWindowLevel, joins all Spaces, full-screen auxiliary, never key or main except in interactive mode ? verify: stays above a full-screen app
  ✗ screen-saver level - sits above system dialogs and permission prompts
  ✗ normal floating level - goes behind full-screen apps
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-capture-hiding: What happens with "invisible to screen share"?
  ✓ set sharingType none behind a config flag - one line of code, and it still hides the panel from older capture APIs ⚠ on macOS 15.4 and later ScreenCaptureKit ignores it, so apps like Zoom will show the panel when the whole screen is shared ? verify: share the screen once and look
  ✗ startup self-test with a warning badge plus a test matrix - user dropped it (user 2026-10-02); it needs screenshot code from Phase 5
  ✗ no flag at all - loses the cases where it still works
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-hotkeys: Which hotkeys, and when are they registered?
  ✓ meeting-only registration - suggest, clear and the four move keys are registered only while a meeting runs, because a global hotkey takes the key combination away from every other app (user 2026-10-02) ⚠ Cmd+Enter does not send messages and Cmd+Shift+arrows do not select text in other apps during a meeting, and the three always-on defaults also take Cmd+Shift+R (browser hard reload), Cmd+Backslash and Cmd+Shift+M from other apps all day; every key is configurable
  ✗ register everything at launch - Cmd+Enter would stop working in Slack and Mail all day
  ✗ detect conflicts with other apps - macOS reported success even when another process held the same combination (probe 2026-10-02)
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-quit-path: How does the user quit an app with no Dock icon and no window buttons?
  ✓ menu-bar item - Start or Stop Meeting, Show or Hide Overlay, Quit; pulled forward from Phase 7 (user 2026-10-02)
  ✗ quit hotkey only - one more always-on global key, and no visible sign the app is running
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-suggestion-id: How are late tokens from a cancelled request kept off the screen?
  ✓ id on every suggestion event - the overlay drops any delta whose id is not the newest one
  ✗ rely on cancellation alone - a delta already sent to the main queue would still be drawn
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Capture

D-system-audio: How is the other side's audio captured?
  ✓ ScreenCaptureKit default plus a backend switch - needs only the Screen Recording grant, and the user asked to keep the fallbacks (user 2026-10-02) ⚠ macOS shows a recording indicator and may ask to re-approve the grant periodically ? verify: how often macOS 15.6.1 re-asks
  ✗ ScreenCaptureKit only - was the recommendation at Confidence: 60%, overruled by the user
  ✗ cpal loopback as default - it builds a Core Audio tap, which has reported silent-failure cases after an unclean exit
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-coreaudio-tap: Should the app also carry its own Core Audio process tap built on objc2-core-audio?
  ✗ hand-written tap - cpal 0.18.2 already implements the same tap plus aggregate device in its loopback module (cpal source, loopback.rs)
  ⊘ not doing - the cpal_loopback backend covers this path; reopen if cpal loopback cannot deliver audio on this Mac
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-permissions: How are missing permissions handled?
  ✓ preflight and request - check Screen Recording before starting the system stream, request it once, tell the user to restart, and keep the meeting running with Me only; auto-applied at Confidence: 80%
  ✗ let the stream fail - the error text from the framework does not say what to do
  ✗ onboarding window - Phase 8
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-mic-silence: How is a denied microphone detected?
  ✓ digital-silence check - 3 s of samples from the Me source that are all exactly zero shows a status warning ? verify: a denied microphone delivers zeros instead of an error
  ✗ ask AVFoundation for the authorization status - needs another framework binding for one call
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-device-change: What happens when the microphone changes (AirPods connect)?
  ✓ rely on cpal - cpal 0.18 reroutes a default-device stream by itself, and the app rebuilds the stream only on a DeviceNotAvailable or StreamInvalidated error ? verify: the sample rate after a reroute
  ✗ Core Audio property listener - extra unsafe code for a case cpal claims to handle
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-watchdog: What happens when the system-audio stream stops delivering?
  ✓ restart on stop error - when ScreenCaptureKit reports that the stream stopped, a helper thread restarts it, up to 5 times without a sample in between; a no-samples timer exists but is off by default ⚠ a stream that goes quiet without reporting a stop is not recovered until the timer is switched on
  ✗ no-samples timer of 5 s on by default - nobody has checked that ScreenCaptureKit keeps delivering buffers while nothing plays, and if it does not, every quiet stretch would restart the stream ? verify: play nothing for 30 s and count buffers, then set audio.watchdog_silence_secs to 5
  ✗ no watchdog - a reported macOS bug makes long captures die without notice
  ✗ restart forever - a missing permission would loop
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-sleep-wake: Is sleep or display change handled specially?
  ✗ listen for sleep and display notifications - more AppKit code for a rare case
  ⊘ not doing - the watchdog and the mic rebuild cover a stopped stream; reopen if a meeting breaks after sleep
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-dev-signing: How is the dev app signed so macOS remembers the Microphone and Screen Recording grants?
  ✓ self-signed local certificate - user chose it (user 2026-10-02); free and stable across rebuilds ? verify: codesign accepts the untrusted self-signed identity without a trust step
  ✗ Apple Development certificate - needs an Apple ID set up in Xcode, which this Mac does not have (security find-identity returned 0 identities)
  ✗ ad-hoc signing - the signature changes every build, so macOS can forget the grants
  ✗ run the bare binary from a terminal - the grants then belong to the terminal app
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-app-launch: How is the dev app started?
  ✓ launch the bundle with open - macOS then treats the app as responsible for its own permission prompts ? verify: prompts name clueless, not the terminal
  ✗ execute the binary inside the bundle from a shell - the terminal becomes the responsible process
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Segmenter

D-dsp-threading: Where do resampling and voice detection run?
  ✓ one std thread per stream - the ring buffer has no async wake-up, and one VAD call takes about 85 microseconds, so a plain loop with a 5 ms sleep is enough (probe 2026-10-02); auto-applied at Confidence: 80%
  ✗ tokio tasks - blocking CPU work and polling on the async runtime delays network tasks
  ✗ inside the audio callback - model inference allocates and can block, which breaks audio
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-vad: Which voice activity detector?
  ✓ voice_activity_detector - bundles the Silero v5 model, links ONNX Runtime statically, and scored silence at 0.04 and speech at 0.95 or more on this Mac (probe 2026-10-02) ⚠ the first build downloads about 68 MB of ONNX Runtime, and it pins ort to exactly 2.0.0-rc.10
  ✗ silero-vad-rust - needs an ONNX Runtime dynamic library installed separately
  ✗ webrtc-vad - unmaintained since 2019
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-resampler: How is audio converted to 16 kHz?
  ✓ rubato Fft - the fixed-ratio resampler, 4 microseconds per call, and with 48 kHz input and chunk 1536 it yields exactly one 512-sample frame per call (probe 2026-10-02)
  ✗ integer decimation by 3 - only works for 48 kHz, and AirPods deliver 16 or 24 kHz
  ✗ rubato 0.x API from online tutorials - does not compile on 5.x
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-segment-params: Which segmenter numbers?
  ✓ research table with corrections - end of utterance is 19 frames (608 ms) because 600 ms is not a whole number of 32 ms frames, pre-roll is 10 frames (320 ms), and minimum speech is 8 frames (256 ms) so that "yes" and "no" survive; auto-applied at Confidence: 75%
  ✗ research table unchanged - 400 ms minimum speech drops one-word answers
  ✗ tune on real recordings first - no real recordings exist yet
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-forced-cut: How is an utterance longer than 15 s cut?
  ✓ overlap only outside a pause - cut at the lowest-probability frame of the last 2 s, and carry 1.0 s of audio into the next piece only when that frame is still speech (p at least 0.35)
  ✗ always carry 1.0 s as the research plan says - when the cut lands in a pause this creates duplicate words that the dedup step then has to remove
  ✗ never overlap - a cut inside a word loses that word
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-dedup: How are duplicate words at a forced cut removed?
  ✓ word-suffix match up to 8 words - compare normalized words of the end of piece A with the start of piece B and drop the match from B ⚠ fails when the server writes the overlap differently in the two pieces
  ✗ ASR word timestamps - the server response has no timestamps (probe 2026-10-02 returned only text, logprobs null, usage null)
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Engine and transcript

D-timebase: How do the two streams get comparable timestamps?
  ✓ sample count on a meeting clock - each stream adds processed samples to an anchor taken from one shared monotonic clock, and takes a new anchor after any gap; auto-applied at Confidence: 75%
  ✗ sample count only - drifts apart after a ring overflow or a stream restart
  ✗ host timestamps from cpal and ScreenCaptureKit - two different clock sources to convert and no benefit at the 100 ms accuracy the echo filter needs
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-final-order: How do finished utterances reach the transcript in a stable order?
  ✓ one final worker per stream - finals of one speaker are sent one at a time in sequence order, so they commit in order with no hold-back buffer; auto-applied at Confidence: 80% ⚠ lines of the two speakers commit in arrival order, so a Me line can appear a line early or late relative to a Them line, and one final that fails all three attempts delays that speaker's later lines by up to about 31 s
  ✗ two concurrent requests per stream as the research plan says - a retry lets utterance 2 finish before utterance 1, and the prompt transcript must only ever grow at the end
  ✗ sort by start time on every insert - changes earlier lines of the prompt and destroys the server's prefix cache
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-interim: How do live partial transcripts work?
  ✓ latest-wins interim slot - each stream keeps one pending interim request, and a newer one replaces an unsent older one; user kept the live ticker (user 2026-10-02) ⚠ roughly doubles ASR requests while someone talks
  ✗ no interims - was an option, user kept them
  ✗ queue every interim - a slow server would show stale text and delay finals
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-asr-failure: What happens when a final utterance cannot be transcribed?
  ✓ drop the utterance - after three attempts the line is removed and the status label shows the error; auto-applied at Confidence: 75% ⚠ that part of the meeting is missing from the transcript
  ✗ keep audio in memory and retry when the server returns - that is the Phase 7 queue, not in this slice
  ✗ insert a placeholder line - puts text into the prompt that nobody said
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-backpressure: What happens when audio or utterances arrive faster than they are consumed?
  ✓ bounded final queue - 16 finals per stream, and when it is full the newest final is dropped with a status error, while a full ring buffer drops new samples and counts them; auto-applied at Confidence: 75%
  ✗ unbounded queue - memory grows without limit while the ASR server is down
  ✗ block the capture thread - the ring then overflows anyway and the timeline breaks silently
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-echo-filter: How is the other side's voice kept out of the "Me" lines when the user is on speakers?
  ✓ hold Me finals - a Me final waits until the Them stream has processed past its end time, or 3 s, and is dropped when it overlaps Them audio by 70 percent and shares 60 percent of its words with the committed Them lines plus the Them text still in progress; user kept the filter (user 2026-10-02) ⚠ Me lines appear up to 3 s later, and an echo is missed when no Them text for that stretch exists yet after 3 s
  ✗ no filter, headphones only - was the recommendation, overruled by the user
  ✗ acoustic echo cancellation - cpal does not expose Apple's voice processing unit
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-health: How does the user know the servers are reachable?
  ✓ one check at meeting start - GET /v1/models on both servers with a 2 s timeout, and the status label names a server that is offline or a model that is missing
  ✗ poll every 10 s with backoff - Phase 7
  ✗ no check - the first failure would only show after someone spoke
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-replay: How is the pipeline run without a microphone or permissions?
  ✓ paced headless replay - the binary takes one or two WAV files, feeds them at a chosen speed multiple, prints the transcript and exits
  ✗ unpaced replay - the two streams would race, and the echo filter compares their progress
  ✗ a separate test-only binary - a second entry point to keep in sync
  (2026-10-02, meeting-copilot-mvp/spec.md)

## LLM and prompt

D-llm-client: Which HTTP client code talks to the LLM?
  ✓ reqwest plus eventsource-stream - parsed the real server's stream correctly with default features off (probe 2026-10-02)
  ✗ async-openai - needs its bring-your-own-types feature to send chat_template_kwargs, so it adds a dependency without removing code
  ✗ reqwest-eventsource 0.6 - requires an older reqwest
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-thinking: How is model reasoning handled on the live path?
  ✓ off per request - chat_template_kwargs enable_thinking false worked on the real server (probe 2026-10-02); auto-applied at Confidence: 95%
  ✗ leave it on and hide it - seconds of delay before the first visible token
  ✗ filter think tags out of the content on the client - the server already delivers reasoning in a separate field (probe 2026-10-02)
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-prompt-shape: How are the messages arranged so the server can reuse its cache?
  ✓ one system and one user message - the user message is the transcript followed by the task, so the text up to the end of the transcript only ever grows; auto-applied at Confidence: 80%
  ✗ several user messages in a row as the research plan says - a chat template may reject or merge consecutive user messages ? verify: not tested on this template
  ✗ task first, transcript last - the changing part would come first and nothing could be cached
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-output-format: What answer shape does the system prompt ask for?
  ✓ one sentence then bullets - a direct answer sentence, then at most 3 short lines starting with a dash, at most 60 words, plain text, in the language of the conversation
  ✗ 2 to 3 bullets only - the research plan states both; the sentence-first form reads faster
  ✗ markdown - the text view shows plain text
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-trigger: What starts a suggestion?
  ✓ manual hotkey - no false positives and no surprise load on the server; auto-applied at Confidence: 85%
  ✗ automatic question detection - Phase 7 item, and a wrong trigger covers the screen at the wrong moment
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-trigger-pending: What does a trigger use when Them is still talking or the final is still being transcribed?
  ✓ interim text in the tail - the latest interim text goes into the last part of the user message, after the stable transcript; auto-applied at Confidence: 75%
  ✗ wait for the final - adds the 608 ms silence wait plus ASR time to every trigger
  ✗ finals only - the answer would ignore the question that was just asked
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-llm-failure: What does the user see when the LLM request fails?
  ✓ show the error and keep partial text - connect failure shows "LLM offline", an HTTP error shows the status code, and a stream that stalls for 10 s ends with the text so far marked as interrupted
  ✗ retry automatically - the moment has passed, and the user can press the hotkey again
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-compression: What happens when the transcript gets too long for the context window?
  ✓ summarize the oldest half - above an estimated 90000 transcript tokens one LLM call turns the oldest half of the lines into a summary block; user kept the research plan's behaviour (user 2026-10-02) ⚠ the cache is rebuilt once after each compression, and the path only runs after roughly 6 hours of speech, so only tests will exercise it
  ✗ drop the oldest half without a summary - was the recommendation at Confidence: 85%, overruled by the user
  ✗ exact token counting - an estimate of characters divided by 3.5 is enough at this margin
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-profile: How do the user's own notes get into the prompt?
  ✓ one text file - an optional path in the config, read at meeting start and placed in the system message
  ✗ document upload UI - no settings window in this slice
  ✗ no profile - the answers would not know who the user is
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Testing

D-test-levels: What is tested at which level?
  ✓ three levels - unit tests for pure logic, integration tests against a local mock server, and end-to-end replay against the real LAN server, with AppKit, hotkeys and live capture on a manual checklist because macOS permission prompts cannot be answered by a test
  ✗ UI automation - needs Accessibility and Screen Recording grants for the test runner
  ✗ mock everything - the real server's field names and timing are part of what can go wrong
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-test-timing: How do tests exercise timeouts (ASR 10 s, stall 10 s, echo hold 3 s, stop wait 5 s, compression retry 60 s)?
  ✓ injectable durations - every timeout is a field of a timings struct or a client constructor argument, production uses the values in this spec, and tests pass values of 100 to 300 ms and wait in real time
  ✗ tokio paused time - it auto-advances while real socket I/O is pending, and the stream threads use std time, so results would be wrong or flaky
  ✗ real production durations in tests - a single run would take minutes
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-mock-server: Which mock HTTP server?
  ✓ axum - it delivered server-sent events with real gaps between them (probe 2026-10-02); auto-applied at Confidence: 85%
  ✗ wiremock - it only sends a fully buffered body, so stalls and mid-stream drops cannot be tested (probe 2026-10-02)
  (2026-10-02, meeting-copilot-mvp/spec.md)

D-fixtures: Which audio do the tests use?
  ✓ synthetic fixtures - a script generates English, Dutch and French WAV files with the macOS say command, so the build runs unattended (user 2026-10-02) ⚠ synthetic speech is cleaner than a real meeting
  ✗ recordings made by the user - blocks the build on manual work
  ✗ downloaded speech corpus - licensing and size
  (2026-10-02, meeting-copilot-mvp/spec.md)
