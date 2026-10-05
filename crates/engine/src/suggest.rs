//! One suggestion run: stream deltas for one prompt to the UI and map
//! every way it can end to a `SuggestionEnd`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use tokio_util::sync::CancellationToken;

use clueless_types::events::{StatusSink, SuggestionEnd, UiEvent};
use context::prompt::{PASS_TOKEN, PromptMessage};
use llm::client::{LlmClient, LlmError};
use llm::types::{ChatRequest, Message, StreamPart};
use trace::record::{Body, Channel, LlmOutcome, Usage as TraceUsage};
use trace::sink::TraceSink;

/// Copy the client's token counts into the trace record's own type.
pub(crate) fn trace_usage(usage: llm::types::Usage) -> TraceUsage {
    TraceUsage {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
    }
}

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
        if PASS_TOKEN.starts_with(&upper) {
            return true;
        }
        upper.strip_prefix(PASS_TOKEN).is_some_and(|rest| {
            rest.chars()
                .all(|c| c == '.' || c == '!' || c.is_whitespace())
        })
    }
}

/// True when `raw` is exactly the single word `PASS` (case-insensitive,
/// optionally followed by `.`, `!` or whitespace): what an automatic answer
/// that passed leaves behind. Empty or blank text is not a pass, so a stream
/// that produced nothing is never mistaken for one.
fn is_pass_text(raw: &str) -> bool {
    let upper = raw.trim_start().to_uppercase();
    upper.len() >= PASS_TOKEN.len()
        && upper.strip_prefix(PASS_TOKEN).is_some_and(|rest| {
            rest.chars()
                .all(|c| c == '.' || c == '!' || c.is_whitespace())
        })
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

/// Pass one streamed chunk through the optional hold-back filter. Without a
/// filter the chunk is shown as is; with one, the held text is released once
/// the answer can no longer be `PASS`, and the filter is dropped from then on.
fn release_chunk(filter: &mut Option<PassFilter>, text: String) -> Option<String> {
    let Some(held) = filter.as_mut() else {
        return Some(text);
    };
    let released = held.push(&text);
    if released.is_some() {
        *filter = None;
    }
    released
}

/// The outcome when the stream closes without an error.
fn stream_closed_end(cancel: &CancellationToken) -> SuggestionEnd {
    if cancel.is_cancelled() {
        SuggestionEnd::Cancelled
    } else {
        SuggestionEnd::Done
    }
}

/// Stream one suggestion until it ends and report the outcome. `cancel`
/// is this suggestion's own token: when it fires the run reports
/// `Cancelled` whatever else was in flight. With `hold_pass` the start of
/// the answer is held back while it could still be `PASS`; an answer that
/// ends while held shows nothing. Every streamed text piece is recorded
/// on the meeting trace under `call`, and the guard writes the call's
/// `llm_end` when the run ends.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    id: u64,
    llm: Arc<LlmClient>,
    request: ChatRequest,
    cancel: CancellationToken,
    ui: StatusSink,
    hold_pass: bool,
    finished: tokio::sync::mpsc::UnboundedSender<Finished>,
    trace: Arc<dyn TraceSink>,
    call: u64,
) {
    let mut guard = EndGuard {
        id,
        ui: ui.clone(),
        finished,
        done: false,
        trace,
        call,
        hold_pass,
        raw: String::new(),
        finish_reason: None,
        usage: None,
        first_content_ms: None,
        first_reasoning_ms: None,
    };
    let mut stream = llm.stream(request, cancel.clone());
    let started = Instant::now();
    let mut first_delta = true;
    let mut filter = hold_pass.then(PassFilter::new);
    let mut shown = String::new();
    let mut error_detail: Option<String> = None;
    let end = loop {
        match stream.next().await {
            Some(Ok(StreamPart::Content(text))) => {
                if first_delta {
                    first_delta = false;
                    guard.first_content_ms = Some(started.elapsed().as_millis() as u64);
                    tracing::info!(
                        suggestion = id,
                        llm_first_delta_ms = started.elapsed().as_millis(),
                        "time to first suggestion delta"
                    );
                }
                guard.raw.push_str(&text);
                guard.trace.record(Body::LlmDelta {
                    call: guard.call,
                    channel: Channel::Content,
                    text: text.clone(),
                });
                if cancel.is_cancelled() {
                    break SuggestionEnd::Cancelled;
                }
                let text = release_chunk(&mut filter, text);
                if let Some(text) = text {
                    shown.push_str(&text);
                    ui(UiEvent::SuggestionDelta { id, text });
                }
            }
            Some(Ok(StreamPart::Reasoning(text))) => {
                if guard.first_reasoning_ms.is_none() {
                    guard.first_reasoning_ms = Some(started.elapsed().as_millis() as u64);
                }
                guard.trace.record(Body::LlmDelta {
                    call: guard.call,
                    channel: Channel::Reasoning,
                    text,
                });
            }
            Some(Ok(StreamPart::Finish { reason, usage })) => {
                guard.finish_reason = reason.clone();
                guard.usage = usage.clone().map(trace_usage);
            }
            Some(Err(error)) => {
                error_detail = Some(error.detail());
                break map_llm_error(&error);
            }
            None => break stream_closed_end(&cancel),
        }
    };
    guard.finish(end, shown, error_detail);
}

/// Reports the end of a run exactly once. If the task unwinds (a panic) before
/// `finish`, dropping the guard reports an interrupted answer, so the engine
/// never keeps a request open that no task is serving. The guard also carries
/// what the stream produced (raw text, finish reason, usage, first-piece
/// times) so either path can write the call's `llm_end` record.
struct EndGuard {
    id: u64,
    ui: StatusSink,
    finished: tokio::sync::mpsc::UnboundedSender<Finished>,
    done: bool,
    trace: Arc<dyn TraceSink>,
    call: u64,
    hold_pass: bool,
    raw: String,
    finish_reason: Option<String>,
    usage: Option<TraceUsage>,
    first_content_ms: Option<u64>,
    first_reasoning_ms: Option<u64>,
}

impl EndGuard {
    fn finish(mut self, end: SuggestionEnd, shown: String, error: Option<String>) {
        self.done = true;
        self.record_end(&end, &shown, error.as_deref());
        tracing::info!(
            suggestion = self.id,
            end = ?end,
            shown_chars = shown.chars().count(),
            "suggestion ended"
        );
        (self.ui)(UiEvent::SuggestionEnd {
            id: self.id,
            end: end.clone(),
        });
        let _ = self.finished.send(Finished {
            id: self.id,
            end,
            shown,
        });
    }

    /// Write this call's `llm_end`. `passed` marks the automatic answer
    /// that held back as `PASS` to the very end (D-suggestion-detail).
    fn record_end(&self, end: &SuggestionEnd, shown: &str, error: Option<&str>) {
        let outcome = match end {
            SuggestionEnd::Done => LlmOutcome::Done,
            SuggestionEnd::Cancelled => LlmOutcome::Cancelled,
            SuggestionEnd::Failed(_) | SuggestionEnd::Interrupted => LlmOutcome::Error,
        };
        let passed = self.hold_pass
            && *end == SuggestionEnd::Done
            && shown.is_empty()
            && is_pass_text(&self.raw);
        self.trace.record(Body::LlmEnd {
            call: self.call,
            outcome,
            error: error.map(str::to_owned),
            finish_reason: self.finish_reason.clone(),
            usage: self.usage.clone(),
            raw_text: self.raw.clone(),
            shown_text: shown.to_owned(),
            passed,
            first_content_ms: self.first_content_ms,
            first_reasoning_ms: self.first_reasoning_ms,
        });
    }
}

impl Drop for EndGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.record_end(
            &SuggestionEnd::Interrupted,
            "",
            Some("task ended without a report"),
        );
        tracing::error!(
            suggestion = self.id,
            "suggestion task ended without a report"
        );
        (self.ui)(UiEvent::SuggestionEnd {
            id: self.id,
            end: SuggestionEnd::Interrupted,
        });
        let _ = self.finished.send(Finished {
            id: self.id,
            end: SuggestionEnd::Interrupted,
            shown: String::new(),
        });
    }
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
    use clueless_types::events::{StatusSink, SuggestionEnd};

    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use clueless_types::events::UiEvent;
    use serde_json::json;

    use super::{
        EndGuard, Finished, PassFilter, is_pass_text, release_chunk, run, stream_closed_end,
    };

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

    #[test]
    fn release_chunk_passes_text_through_without_a_filter() {
        let mut filter = None;
        assert_eq!(
            release_chunk(&mut filter, "hi".to_owned()),
            Some("hi".to_owned())
        );
        assert!(filter.is_none());
    }

    #[test]
    fn release_chunk_drops_the_filter_once_text_is_released() {
        let mut filter = Some(PassFilter::new());
        assert_eq!(release_chunk(&mut filter, "PA".to_owned()), None);
        assert!(filter.is_some());
        assert_eq!(
            release_chunk(&mut filter, "SSY".to_owned()),
            Some("PASSY".to_owned())
        );
        assert!(filter.is_none());
    }

    #[test]
    fn a_closed_stream_is_done_unless_cancelled() {
        let cancel = tokio_util::sync::CancellationToken::new();
        assert_eq!(stream_closed_end(&cancel), SuggestionEnd::Done);
        cancel.cancel();
        assert_eq!(stream_closed_end(&cancel), SuggestionEnd::Cancelled);
    }

    fn guard() -> (
        EndGuard,
        Arc<Mutex<Vec<UiEvent>>>,
        tokio::sync::mpsc::UnboundedReceiver<Finished>,
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let guard = EndGuard {
            id: 7,
            ui: Arc::new(move |event| sink.lock().unwrap().push(event)),
            finished: tx,
            done: false,
            trace: Arc::new(trace::sink::NoTrace),
            call: 1,
            hold_pass: false,
            raw: String::new(),
            finish_reason: None,
            usage: None,
            first_content_ms: None,
            first_reasoning_ms: None,
        };
        (guard, events, rx)
    }

    #[test]
    fn only_the_whole_word_pass_is_a_pass_text_and_empty_is_not() {
        assert!(is_pass_text("PASS"));
        assert!(is_pass_text("pass."));
        assert!(is_pass_text("  Pass!\n"));
        assert!(!is_pass_text(""), "an empty answer is not a pass");
        assert!(!is_pass_text("   "), "blank text is not a pass");
        assert!(!is_pass_text("PASSPORT"));
        assert!(!is_pass_text("PAS"));
    }

    #[test]
    fn a_guard_dropped_without_finishing_reports_an_interrupted_answer() {
        let (guard, events, mut rx) = guard();
        drop(guard);
        assert_eq!(
            *events.lock().unwrap(),
            vec![UiEvent::SuggestionEnd {
                id: 7,
                end: SuggestionEnd::Interrupted
            }]
        );
        let finished = rx.try_recv().expect("the engine hears about it");
        assert_eq!((finished.id, finished.end), (7, SuggestionEnd::Interrupted));
    }

    #[test]
    fn a_finished_guard_reports_once_with_the_real_end() {
        let (guard, events, mut rx) = guard();
        guard.finish(SuggestionEnd::Done, "text".to_owned(), None);
        assert_eq!(events.lock().unwrap().len(), 1);
        let finished = rx.try_recv().expect("one report");
        assert_eq!(finished.shown, "text");
        assert!(rx.try_recv().is_err(), "no second report from the drop");
    }

    /// An SSE mock answering one chat request with reasoning pieces (under
    /// both field names) and then content pieces, then `[DONE]`.
    async fn reasoning_then_content_mock(pieces: &[&str]) -> String {
        use axum::body::Body;
        use axum::http::header;
        use axum::response::Response;
        use axum::routing::post;
        use tokio::net::TcpListener;

        let mut sse = String::new();
        sse.push_str(&format!(
            "data: {}\n\n",
            json!({"choices": [{"index": 0, "delta": {"reasoning": "thinking hard"}}]})
        ));
        sse.push_str(&format!(
            "data: {}\n\n",
            json!({"choices": [{"index": 0, "delta": {"reasoning_content": " still going"}}]})
        ));
        for piece in pieces {
            sse.push_str(&format!(
                "data: {}\n\n",
                json!({"choices": [{"index": 0, "delta": {"content": piece}}]})
            ));
        }
        sse.push_str("data: [DONE]\n\n");
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let sse = sse.clone();
                async move {
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from(sse))
                        .expect("sse response")
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock server");
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn reasoning_pieces_never_reach_the_ui_deltas() {
        let base_url = reasoning_then_content_mock(&["The ", "answer."]).await;
        let llm = Arc::new(llm::client::LlmClient::new(
            base_url,
            "mock-model",
            None,
            Duration::from_secs(2),
            Duration::from_secs(10),
        ));
        let request = llm::types::ChatRequest::new(
            "mock-model",
            vec![llm::types::Message::user("hi")],
            220,
            0.4,
            Some(false),
            true,
        );
        let events: Arc<Mutex<Vec<UiEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let ui: StatusSink = Arc::new(move |event| sink.lock().unwrap().push(event));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        run(
            9,
            llm,
            request,
            tokio_util::sync::CancellationToken::new(),
            ui,
            false,
            tx,
            Arc::new(trace::sink::NoTrace),
            1,
        )
        .await;

        let deltas: Vec<String> = events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                UiEvent::SuggestionDelta { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["The ".to_string(), "answer.".to_string()]);
        let shown = events.lock().unwrap().iter().any(|event| {
            matches!(
                event,
                UiEvent::SuggestionEnd {
                    end: SuggestionEnd::Done,
                    ..
                }
            )
        });
        assert!(shown, "the run ends Done: {:?}", events.lock().unwrap());
        let finished = rx.recv().await.expect("the engine hears the end");
        assert_eq!(finished.shown, "The answer.");
    }
}
