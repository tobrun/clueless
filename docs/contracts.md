# Contracts

All entries below were drafted before the code existed.
Each guarantee carries a verify mark until the change set that implements it has landed and been checked.

## Capture

C-source-nonblocking: SampleSource::read never blocks and never allocates on the caller's behalf beyond the given buffer
  guaranteed by: capture sources (stream restarts and rebuilds run on a helper thread, and read returns Empty meanwhile) and the replay source ? verify: the code does not exist yet; check when its change set lands
  relied on by: engine stream threads
  (2026-10-02, meeting-copilot-mvp/spec.md)

C-audio-callback-realtime: the microphone callback does not allocate, lock or log
  guaranteed by: capture mic callback (per-sample push into the ring) ? verify: the code does not exist yet; check when its change set lands
  relied on by: Core Audio
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Context

C-prompt-prefix-stable: between two compressions, the user message built after N+1 committed lines starts with the exact bytes of the transcript part built after N lines
  guaranteed by: context prompt builder (append-only store, tail always last) ? verify: the code does not exist yet; check when its change set lands
  relied on by: LLM server prefix cache, latency budget
  (2026-10-02, meeting-copilot-mvp/spec.md)

## Engine

C-finals-in-order: for one speaker, TranscriptFinal events are emitted in increasing seq order, and each seq at most once
  guaranteed by: engine final worker (one request at a time per stream) ? verify: the code does not exist yet; check when its change set lands
  relied on by: context transcript store, overlay ticker
  (2026-10-02, meeting-copilot-mvp/spec.md)

C-suggestion-id: every SuggestionDelta and SuggestionEnd carries the id of the SuggestionStart that began it, and ids increase
  guaranteed by: engine suggestion task ? verify: the code does not exist yet; check when its change set lands
  relied on by: overlay (drops events with an old id)
  (2026-10-02, meeting-copilot-mvp/spec.md)

C-server-read-only: the app sends only GET /v1/models, POST /v1/audio/transcriptions and POST /v1/chat/completions, and never more than one final plus one interim transcription per stream at a time
  guaranteed by: asr and llm clients, engine workers ? verify: the code does not exist yet; check when its change set lands
  relied on by: the shared inference server (other users of it)
  (2026-10-02, meeting-copilot-mvp/spec.md)
