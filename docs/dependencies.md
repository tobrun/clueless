# Dependencies

Module edges for the `clueless` workspace. A crate may depend on the crates
listed after its arrow, and on nothing else inside the workspace. External
crates are declared in each crate's `Cargo.toml`; the seven pure
crates (types, segmenter, asr, llm, context, trace, engine) must not depend
on any macOS-only crate (objc2 family, cpal, screencapturekit, global-hotkey,
dispatch2). `crates/types/tests/purity.rs` checks the pure-crate rule over
the real dependency graph.

```rules
[modules]
types = crates/types/**
segmenter = crates/segmenter/**
asr = crates/asr/**
llm = crates/llm/**
context = crates/context/**
trace = crates/trace/**
engine = crates/engine/**
capture = crates/capture/**
overlay = crates/overlay/**
app = crates/app/**
xtask = xtask/**

[allowed]
segmenter -> types
asr -> types
llm -> types
context -> types
trace -> types
engine -> types, segmenter, asr, llm, context, trace
capture -> types
overlay -> types
app -> types, engine, capture, overlay, trace
```
