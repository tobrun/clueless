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

/// Holds back the start of an automatic answer while it could still turn out
/// to be the single word `PASS` (case-insensitive, optionally followed by
/// `.`, `!` or whitespace), which means "nothing to add" and is never shown.
#[derive(Debug, Default)]
pub struct PassFilter {
    held: String,
}

impl PassFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the next streamed text. `None` while the answer might still be
    /// `PASS`; otherwise everything held so far, to be shown as one delta.
    pub fn push(&mut self, text: &str) -> Option<String> {
        self.held.push_str(text);
        if self.could_be_pass() {
            None
        } else {
            Some(std::mem::take(&mut self.held))
        }
    }

    fn could_be_pass(&self) -> bool {
        let upper = self.held.trim_start().to_uppercase();
        if "PASS".starts_with(&upper) {
            return true;
        }
        upper.strip_prefix("PASS").is_some_and(|rest| {
            rest.chars()
                .all(|c| c == '.' || c == '!' || c.is_whitespace())
        })
    }
}

/// How a suggestion run ended, reported to the engine loop after the UI got
/// its `SuggestionEnd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub id: u64,
    pub end: SuggestionEnd,
    /// Everything that was sent to the UI as deltas.
    pub shown: String,
}

/// Stream one suggestion until it ends and report the outcome. `cancel`
/// is this suggestion's own token: when it fires the run reports
/// `Cancelled` whatever else was in flight. With `hold_pass` the start of
/// the answer is held back while it could still be `PASS`; an answer that
/// ends while held shows nothing.
pub async fn run(
    id: u64,
    llm: Arc<LlmClient>,
    request: ChatRequest,
    cancel: CancellationToken,
    ui: StatusSink,
    hold_pass: bool,
    finished: tokio::sync::mpsc::UnboundedSender<Finished>,
) {
    let mut stream = llm.stream(request, cancel.clone());
    let started = Instant::now();
    let mut first_delta = true;
    let mut filter = hold_pass.then(PassFilter::new);
    let mut shown = String::new();
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
                let text = match filter.as_mut() {
                    None => Some(text),
                    Some(held) => {
                        let released = held.push(&text);
                        if released.is_some() {
                            filter = None;
                        }
                        released
                    }
                };
                if let Some(text) = text {
                    shown.push_str(&text);
                    ui(UiEvent::SuggestionDelta { id, text });
                }
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
    ui(UiEvent::SuggestionEnd {
        id,
        end: end.clone(),
    });
    let _ = finished.send(Finished { id, end, shown });
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

#[cfg(test)]
mod tests {
    use super::PassFilter;

    fn feed(chunks: &[&str]) -> Vec<Option<String>> {
        let mut filter = PassFilter::new();
        chunks.iter().map(|chunk| filter.push(chunk)).collect()
    }

    #[test]
    fn pass_in_any_case_with_trailing_punctuation_stays_held() {
        for chunks in [
            vec!["PASS"],
            vec!["pass"],
            vec!["Pa", "ss", "."],
            vec!["\n  PASS!"],
            vec!["PASS", " \n"],
        ] {
            assert!(
                feed(&chunks).iter().all(Option::is_none),
                "{chunks:?} must stay held"
            );
        }
    }

    #[test]
    fn an_answer_that_leaves_pass_is_released_in_full_at_once() {
        assert_eq!(
            feed(&["Pass", "ing the test"]),
            vec![None, Some("Passing the test".to_owned())]
        );
        assert_eq!(feed(&["Yes."]), vec![Some("Yes.".to_owned())]);
        assert_eq!(
            feed(&["PASS", " - nothing new"]),
            vec![None, Some("PASS - nothing new".to_owned())]
        );
    }

    #[test]
    fn leading_whitespace_before_the_answer_is_released_with_it() {
        assert_eq!(
            feed(&["  ", "Sure thing"]),
            vec![None, Some("  Sure thing".to_owned())]
        );
    }
}
