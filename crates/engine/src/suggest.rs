//! One suggestion run: stream deltas for one prompt to the UI and map
//! every way it can end to a `SuggestionEnd`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use tokio_util::sync::CancellationToken;

use clueless_types::events::{StatusSink, SuggestionEnd, UiEvent};
use context::prompt::PromptMessage;
use llm::client::{LlmClient, LlmError};
use llm::types::{ChatRequest, Message};

/// Convert the prompt builder's messages to the LLM client's type.
pub fn to_llm_messages(messages: Vec<PromptMessage>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|message| match message.role.as_str() {
            "system" => Message::system(message.content),
            _ => Message::user(message.content),
        })
        .collect()
}

/// Map a stream error to its end outcome (Scope: connect failure is
/// "LLM offline", an HTTP status keeps its number, and a stall, a closed
/// connection or a decode failure all interrupt the suggestion).
pub fn map_llm_error(error: &LlmError) -> SuggestionEnd {
    match error {
        LlmError::Connect => SuggestionEnd::Failed("LLM offline".to_owned()),
        LlmError::Http { status, .. } => SuggestionEnd::Failed(format!("LLM error {status}")),
        LlmError::Stalled | LlmError::Closed | LlmError::Decode => SuggestionEnd::Interrupted,
    }
}

/// Stream one suggestion until it ends and report the outcome. `cancel`
/// is this suggestion's own token: when it fires the run reports
/// `Cancelled` whatever else was in flight.
pub async fn run(
    id: u64,
    llm: Arc<LlmClient>,
    request: ChatRequest,
    cancel: CancellationToken,
    ui: StatusSink,
) {
    let mut stream = llm.stream(request, cancel.clone());
    let started = Instant::now();
    let mut first_delta = true;
    let end = loop {
        match stream.next().await {
            Some(Ok(text)) => {
                if first_delta {
                    first_delta = false;
                    tracing::info!(
                        suggestion = id,
                        llm_first_delta_ms = started.elapsed().as_millis(),
                        "time to first suggestion delta"
                    );
                }
                if cancel.is_cancelled() {
                    break SuggestionEnd::Cancelled;
                }
                ui(UiEvent::SuggestionDelta { id, text });
            }
            Some(Err(error)) => break map_llm_error(&error),
            None => {
                break if cancel.is_cancelled() {
                    SuggestionEnd::Cancelled
                } else {
                    SuggestionEnd::Done
                };
            }
        }
    };
    ui(UiEvent::SuggestionEnd { id, end });
}

/// A conservative bound so a suggestion that ignores cancellation can
/// never hold the lifecycle hostage.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);

/// Cancel a running suggestion and wait for its `SuggestionEnd`, so the
/// engine's later events (a new start, a stop) always follow it.
pub async fn cancel_and_wait(task: &mut tokio::task::JoinHandle<()>, cancel: &CancellationToken) {
    cancel.cancel();
    let _ = tokio::time::timeout(CANCEL_GRACE, &mut *task).await;
}
