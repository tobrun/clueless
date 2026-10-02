//! The compression watcher: one task per meeting that keeps the store's
//! transcript part under the token budget by asking the LLM for a summary
//! (D-segment-params keeps the summary request small at 1500 tokens).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use clueless_types::events::{StatusLevel, StatusSink, StatusSource, UiEvent};
use context::{budget, store::TranscriptStore};
use llm::client::LlmClient;
use llm::types::ChatRequest;

use crate::suggest::to_llm_messages;

/// Max tokens for the summary answer (spec: compression requests use 1500).
pub const SUMMARY_MAX_TOKENS: u32 = 1500;

/// Watch `commits` and compress whenever the transcript part outgrows
/// `threshold` estimated tokens. At most one request is in flight (this
/// task awaits each one); after a failure the next attempt waits at
/// least `retry`, and one Warn status is emitted per failed attempt.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    store: Arc<Mutex<TranscriptStore>>,
    llm: Arc<LlmClient>,
    threshold: usize,
    retry: Duration,
    temperature: f64,
    mut commits: watch::Receiver<u64>,
    ui: StatusSink,
    cancel: CancellationToken,
) {
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
            let request = ChatRequest::new(
                llm.model(),
                to_llm_messages(messages),
                SUMMARY_MAX_TOKENS,
                temperature,
            );
            let outcome = tokio::select! {
                biased;
                outcome = llm.complete(request) => outcome,
                () = cancel.cancelled() => return,
            };
            match outcome {
                Ok(text) => {
                    last_failure = None;
                    store
                        .lock()
                        .expect("store lock")
                        .set_summary(text, replaced);
                }
                Err(error) => {
                    last_failure = Some(Instant::now());
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
