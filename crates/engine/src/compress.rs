//! The compression watcher: one task per meeting that keeps the store's
//! transcript part under the token budget by asking the LLM for a summary,
//! keeping the summary request small at 1500 tokens. Every summary call is
//! recorded on the meeting trace like a suggestion call is.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use clueless_types::events::{StatusLevel, StatusSink, StatusSource, UiEvent};
use context::{budget, store::TranscriptStore};
use llm::client::LlmClient;
use llm::types::ChatRequest;
use trace::record::{Body, LlmOutcome, Purpose};
use trace::sink::TraceSink;

use crate::suggest::{to_llm_messages, trace_usage};

/// Max tokens for the summary answer (spec: compression requests use 1500).
pub const SUMMARY_MAX_TOKENS: u32 = 1500;

/// Everything the compression watcher reads: the store it shrinks, the LLM
/// it asks, when to ask, and the trace to record the calls on.
pub struct CompressDeps {
    /// The conversation store whose transcript part is kept under budget.
    pub store: Arc<Mutex<TranscriptStore>>,
    /// The client summary requests go through.
    pub llm: Arc<LlmClient>,
    /// Compress whenever the transcript part outgrows this estimated token count.
    pub threshold: usize,
    /// The shortest wait between two failed attempts.
    pub retry: Duration,
    /// The temperature summary requests are sent with.
    pub temperature: f64,
    /// Whether summary requests ask the backend to think first.
    pub enable_thinking: Option<bool>,
    /// Whether summary requests ask the backend for a usage block.
    pub include_usage: bool,
    /// The meeting trace the summary calls are recorded on.
    pub trace: Arc<dyn TraceSink>,
}

/// Watch `commits` and compress whenever the transcript part outgrows
/// `threshold` estimated tokens. At most one request is in flight (this
/// task awaits each one); after a failure the next attempt waits at
/// least `retry`, and one Warn status is emitted per failed attempt.
pub async fn run(
    deps: CompressDeps,
    mut commits: watch::Receiver<u64>,
    ui: StatusSink,
    cancel: CancellationToken,
) {
    let CompressDeps {
        store,
        llm,
        threshold,
        retry,
        temperature,
        enable_thinking,
        include_usage,
        trace,
    } = deps;
    let mut last_failure: Option<Instant> = None;
    loop {
        let plan = {
            let guard = store.lock().expect("store lock");
            let cooldown_passed = last_failure.is_none_or(|at| at.elapsed() >= retry);
            if cooldown_passed && budget::needs_compression_at(&guard, threshold) {
                let (messages, replaced) = budget::compression_request(&guard);
                // Fewer than two lines leaves nothing to fold (the request
                // is empty); wait for the next commit instead.
                if messages.is_empty() {
                    None
                } else {
                    Some((messages, replaced))
                }
            } else {
                None
            }
        };
        if let Some((messages, replaced)) = plan {
            // One call id covers the request record, the end record and the
            // `summary_applied` record that names what was replaced.
            let call = trace.next_call();
            let request = ChatRequest::new(
                llm.model(),
                to_llm_messages(messages),
                SUMMARY_MAX_TOKENS,
                temperature,
                enable_thinking,
                include_usage,
            );
            trace.record(Body::LlmRequest {
                call,
                purpose: Purpose::Compress,
                suggestion: None,
                origin: None,
                profile: None,
                body: serde_json::to_value(&request).unwrap_or(serde_json::Value::Null),
            });
            let completion = tokio::select! {
                biased;
                completion = llm.complete(request) => completion,
                () = cancel.cancelled() => {
                    trace.record(Body::LlmEnd {
                        call,
                        outcome: LlmOutcome::Cancelled,
                        error: Some("meeting ended mid summary".to_owned()),
                        finish_reason: None,
                        usage: None,
                        raw_text: String::new(),
                        shown_text: String::new(),
                        passed: false,
                        first_content_ms: None,
                        first_reasoning_ms: None,
                    });
                    return;
                }
            };
            match completion {
                Ok(completion) => {
                    last_failure = None;
                    trace.record(Body::LlmEnd {
                        call,
                        outcome: LlmOutcome::Done,
                        error: None,
                        finish_reason: completion.finish_reason,
                        usage: completion.usage.map(trace_usage),
                        raw_text: completion.text.clone(),
                        shown_text: String::new(),
                        passed: false,
                        first_content_ms: None,
                        first_reasoning_ms: None,
                    });
                    store
                        .lock()
                        .expect("store lock")
                        .set_summary(completion.text, replaced);
                    trace.record(Body::SummaryApplied { call, replaced });
                }
                Err(error) => {
                    last_failure = Some(Instant::now());
                    trace.record(Body::LlmEnd {
                        call,
                        outcome: LlmOutcome::Error,
                        error: Some(error.detail()),
                        finish_reason: None,
                        usage: None,
                        raw_text: String::new(),
                        shown_text: String::new(),
                        passed: false,
                        first_content_ms: None,
                        first_reasoning_ms: None,
                    });
                    ui(UiEvent::Status {
                        source: StatusSource::App,
                        level: StatusLevel::Warn,
                        text: format!("transcript compression failed: {error}"),
                    });
                }
            }
        }
        // Wait for the next trigger: a new commit, or the retry cooldown
        // expiring after a failure.
        let cooldown = match last_failure {
            Some(at) => Box::pin(tokio::time::sleep(retry.saturating_sub(at.elapsed())))
                as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
            None => Box::pin(std::future::pending::<()>()),
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            () = cooldown => {}
            changed = commits.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}
