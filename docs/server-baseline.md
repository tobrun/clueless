# Server baseline

Facts measured from this Mac against the LAN inference server, and how to re-measure them.
The spec (`docs/decisions.md`) quotes these as "probe 2026-10-02".

## LLM server - http://localhost:8000

- Serves model `your-model-id`, context 131072 tokens.
- A streaming chat request with `"chat_template_kwargs": {"enable_thinking": false}` returns HTTP 200 with no template error and no reasoning text.
- With thinking on, reasoning arrives in `choices[0].delta.reasoning`, separate from `delta.content`.
- The first content delta of a stream is an empty string.
- Re-measure: `scripts/smoke-llm.sh localhost` (checks the model list, a streaming request with thinking off, `[DONE]`, and that no `reasoning` field or `<think>` text appears).

## ASR server - http://localhost:8097

- Lists three models; `istupakov/parakeet-tdt-0.6b-v3-onnx` supports en, nl, fr and 22 more languages.
- A 4.8 s WAV was transcribed in 0.35 to 0.43 s; a 5.3 s WAV in 0.46 s (2026-10-02).
- The response has no word timestamps: only `text`, with `logprobs` and `usage` null.
- Re-measure: `scripts/smoke-asr.sh localhost` (checks the model list and transcribes `fixtures/en_question.wav` with the model named explicitly, printing the wall time).

## Local toolchain (2026-10-02)

- Xcode 26.3, rustc 1.94.1, no code-signing identity (`security find-identity` returns 0), no `cargo-nextest`.
- Crate versions confirmed compiled together on rustc 1.94.1: objc2 0.6.4, objc2-app-kit 0.3.2, objc2-foundation 0.3.2, block2 0.6.2, dispatch2 0.3.1, global-hotkey 0.8.0, cpal 0.18.2, screencapturekit 11.0.0, rtrb 0.4.0, voice_activity_detector 0.2.1, rubato 5.0.1, hound 3.5.1, reqwest 0.13.5, eventsource-stream 0.2.3, tokio 1.53.1, axum 0.8.9.

## Re-running everything

```
scripts/make-fixtures.sh          # regenerate fixtures (needs the say voices Daniel, Ellen, Flo)
scripts/smoke-llm.sh localhost
scripts/smoke-asr.sh localhost
```

Both scripts exit non-zero on any non-200 or unexpected response body.
The ignored live tests (`LIVE_SERVER=1 cargo test --workspace -- --ignored`) exercise the same servers from Rust.
