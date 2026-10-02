# Contracts

All entries below were drafted before the code existed.
Each guarantee carries a verify mark until the change set that implements it has landed and been checked.

## Capture

C-source-nonblocking: SampleSource::read never blocks and never allocates on the caller's behalf beyond the given buffer
  guaranteed by: capture sources (stream restarts and rebuilds run on a helper thread, and read returns Empty meanwhile) and the replay source ? verified 2026-10-02 (CS7/CS10: capture watchdog + ring tests, engine/tests/replay.rs pacing tests)
  relied on by: engine stream threads
  (2026-10-02, meeting-copilot-mvp/spec.md)

C-audio-callback-realtime: the microphone callback does not allocate, lock or log
  guaranteed by: capture mic callback (per-sample push into the ring) ? verified 2026-10-02 (CS7: mic.rs + ring.rs tests; the callback only writes into the pre-allocated ring)
  relied on by: Core Audio
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Context

C-prompt-prefix-stable: between two compressions, the user message built after N+1 committed lines starts with the exact bytes of the transcript part built after N lines
  guaranteed by: context prompt builder (append-only store, tail always last) ? verified 2026-10-02 (CS6: context/src/prompt.rs::transcript_prefix_is_stable_when_a_line_is_committed)
  relied on by: LLM server prefix cache, latency budget
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Engine

C-finals-in-order: for one speaker, TranscriptFinal events are emitted in increasing seq order, and each seq at most once
  guaranteed by: engine final worker (one request at a time per stream) ? verified 2026-10-02 (CS10: engine/tests/transcribe.rs::slow_first_final_keeps_order_and_single_flight)
  relied on by: context transcript store, overlay ticker
  (2026-10-02, meeting-copilot-mvp/spec.md)

C-suggestion-id: every SuggestionDelta and SuggestionEnd carries the id of the SuggestionStart that began it, and ids increase
  guaranteed by: engine suggestion task ? verified 2026-10-02 (CS11: engine/tests/meeting.rs::suggest_while_running_streams_deltas_and_ends_done, engine/tests/meeting.rs::second_suggest_cancels_first_before_starting_second)
  relied on by: overlay (drops events with an old id)
  (2026-10-02, meeting-copilot-mvp/spec.md)

C-server-read-only: the app sends only GET /v1/models, POST /v1/audio/transcriptions and POST /v1/chat/completions, and never more than one final plus one interim transcription per stream at a time
  guaranteed by: asr and llm clients, engine workers ? verified 2026-10-02 (CS5/CS6 clients hit only those routes; mock servers in engine/tests/support/mod.rs serve nothing else and every suite passes through them; CS10: engine/tests/transcribe.rs::slow_first_final_keeps_order_and_single_flight; CS11: engine/tests/meeting.rs::compression_fires_once_and_summary_leads_the_transcript)
  relied on by: the shared inference server (other users of it)
  (2026-10-02, meeting-copilot-mvp/spec.md)
