# Server contract

clueless talks to two OpenAI-compatible HTTP servers: one chat model and one
speech-to-text model.
Both may run on one host or on two; both may be plain HTTP on a trusted network or HTTPS anywhere.
Every setting lives in the environment described in [configuration.md](configuration.md).

## Endpoints the app calls

### `GET {LLM_BASE_URL}/v1/models`

Health check, run when a meeting starts.
The response must list `LLM_MODEL`; anything else puts the LLM indicator in the error state.
With `LLM_API_KEY` set, the request carries `Authorization: Bearer <key>`.

### `POST {LLM_BASE_URL}/v1/chat/completions`

The suggestion and compression calls, both streamed.
The request is a standard chat completion with `stream: true`, `max_tokens` and `temperature` from the environment.
When `LLM_ENABLE_THINKING=false`, the request adds `"chat_template_kwargs": {"enable_thinking": false}`, which vLLM-style servers use to skip the reasoning pass.
The app reads `choices[0].delta.content` from each SSE event, ignores other fields (a `delta.reasoning` field is simply skipped), and stops at `data: [DONE]`.

### `POST {ASR_BASE_URL}/v1/audio/transcriptions`

One multipart upload per finished utterance: the WAV as `file`, the model id as `model`, and `language` when `ASR_LANGUAGE` is set.
The response is read for its `text` field; word timestamps are not used.
`GET {ASR_BASE_URL}/v1/models` is the ASR health check, same rule as above with `ASR_MODEL`.

## Server expectations

- Streaming works end to end (no proxy that buffers the whole SSE body).
- The chat model follows a system prompt closely enough to answer in the requested shape and to answer with the single word PASS when it has nothing to say.
- In Interview the app sends one chat request per turn of the other side; in Brainstorm it sends one at each finished piece of the user's own speech, at least 8 s apart. Never more than one open suggestion request plus one open compression request, and each carries the whole transcript.
- The ASR model is accurate at 16 kHz mono speech and finishes an utterance well under its timeout.
- Responses carry no surprising error shape: non-200 bodies are surfaced (truncated) in the status line, so a readable `{"error": ...}` helps.

## Smoke scripts

Two scripts check a configured server pair from the shell, reading the same environment (or `.env`) as the app:

```
scripts/smoke-llm.sh
scripts/smoke-asr.sh
```

`smoke-llm.sh` lists the models, runs one streamed chat request with thinking off and asserts `[DONE]` arrives with no reasoning text.
`smoke-asr.sh` lists the models and transcribes `fixtures/en_question.wav`, printing the wall time.
Both exit non-zero on any non-200 or unexpected response body.

The ignored live tests (`LIVE_SERVER=1 cargo test --workspace -- --ignored`) exercise the same servers from Rust against the checked-in fixtures.
