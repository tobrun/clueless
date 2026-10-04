# Contracts

All entries below were drafted before the code existed.
Each entry names what guarantees it, how it was verified against the code, and what relies on it.

## Capture

C-source-nonblocking: SampleSource::read never blocks and never allocates on the caller's behalf beyond the given buffer
  guaranteed by: capture sources (stream restarts and rebuilds run on a helper thread, and read returns Empty meanwhile) and the replay source verified 2026-10-02 (capture watchdog + ring tests, engine/tests/replay.rs pacing tests)
  relied on by: engine stream threads
  (2026-10-02)

C-audio-callback-realtime: the microphone callback does not allocate, lock or log
  guaranteed by: capture mic callback (per-sample push into the ring) verified 2026-10-02 (mic.rs + ring.rs tests; the callback only writes into the pre-allocated ring)
  relied on by: Core Audio
  (2026-10-02)

## Context

C-prompt-prefix-stable: between two compressions, the user message built after N+1 committed lines starts with the exact bytes of the transcript part built after N lines; the system message does not depend on the active profile
  guaranteed by: context prompt builder (append-only store, tail always last) verified 2026-10-02 (context/src/prompt.rs::transcript_prefix_is_stable_when_a_line_is_committed; context/src/prompt.rs::system_message_and_transcript_part_are_identical_across_profiles)
  relied on by: LLM server prefix cache, latency budget
  (2026-10-02)

## Engine

C-finals-in-order: for one speaker, TranscriptFinal events are emitted in increasing seq order, and each seq at most once
  guaranteed by: engine final worker (one request at a time per stream) verified 2026-10-02 (engine/tests/transcribe.rs::slow_first_final_keeps_order_and_single_flight)
  relied on by: context transcript store, overlay ticker
  (2026-10-02)

C-suggestion-id: every SuggestionDelta and SuggestionEnd carries the id of the SuggestionStart that began it, and ids increase
  guaranteed by: engine suggestion task verified 2026-10-02 (engine/tests/meeting.rs::suggest_while_running_streams_deltas_and_ends_done, engine/tests/meeting.rs::second_suggest_cancels_first_before_starting_second)
  relied on by: overlay (drops events with an old id)
  (2026-10-02)

C-server-read-only: the app sends only GET /v1/models, POST /v1/audio/transcriptions and POST /v1/chat/completions, never more than one final plus one interim transcription per stream at a time; never more than one open suggestion request plus one open compression request; and suggestion requests the app starts by itself begin at least auto_min_gap (2 s) apart
  guaranteed by: asr and llm clients, engine workers (asr_worker, Engine::run, context::assist policy) verified 2026-10-02, extended 2026-10-03 (clients hit only those routes; mock servers in engine/tests/support/mod.rs serve nothing else and every suite passes through them; engine/tests/transcribe.rs::slow_first_final_keeps_order_and_single_flight; engine/tests/meeting.rs::compression_fires_once_and_summary_leads_the_transcript; engine/tests/assist.rs::brainstorm_spaces_requests_by_four_times_the_gap_and_never_overlaps asserts one request in flight and the spacing; engine/tests/assist.rs::brainstorm_does_not_cancel_a_running_answer_and_asks_once_after_it asserts an automatic request waits for the open one)
  relied on by: the shared inference server (other users of it)
  (2026-10-02)

## App startup

C-startup-failure-marker: a startup failure after the logger is open (env file, config or lock setup) appears in the log file as a line containing `startup failed: ` followed by the full message, before the process exits with code 2
  guaranteed by: the startup_failure helper in crates/app/src/main.rs verified 2026-10-04 (crates/app/tests/startup.rs, one test per failure site)
  relied on by: cargo xtask run (reads the log bytes appended during the launch and fails with the message)
  (2026-10-04, D-marker-string)
