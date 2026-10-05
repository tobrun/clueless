//! Streaming chat client for the OpenAI-compatible LLM server.

use std::fmt;
use std::time::Duration;

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::types::{ChatRequest, Completion, StreamPart, Usage};

const CHAT_PATH: &str = "/v1/chat/completions";
const MODELS_PATH: &str = "/v1/models";

/// How many leading bytes of an error body the `Http` variant keeps.
const BODY_START_BYTES: usize = 200;

/// Why a chat request failed. Maps onto the overlay's LLM error statuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    /// Could not reach the server at all (or the connect timed out).
    #[error("LLM offline")]
    Connect,
    /// The server answered with a non-success status.
    #[error("LLM error {status}")]
    Http { status: u16, body_start: String },
    /// No event arrived within the stall timeout; the text so far is usable (Interrupted).
    #[error("LLM stream stalled")]
    Stalled,
    /// The connection ended before the `[DONE]` event.
    #[error("LLM stream closed before done")]
    Closed,
    /// An event or body could not be decoded.
    #[error("LLM response could not be decoded")]
    Decode,
}

type EvStream =
    futures_util::stream::BoxStream<'static, Result<Event, EventStreamError<reqwest::Error>>>;

enum State {
    /// Send the request on the first poll.
    Init(ChatRequest),
    /// Read parts from the opened event stream, remembering the last seen
    /// finish reason and usage for the `Finish` part at `[DONE]`.
    Streaming {
        events: EvStream,
        finish_reason: Option<String>,
        usage: Option<Usage>,
        /// Parts of the last chunk not yielded yet (a chunk can carry
        /// content and reasoning at once).
        pending: std::collections::VecDeque<StreamPart>,
    },
    /// A terminal error was yielded; end the stream.
    Done,
}

impl LlmError {
    /// The display text plus, for `Http`, the first bytes of the server's
    /// answer. Only for logs and traces; text shown to users stays `Display`.
    pub fn detail(&self) -> String {
        match self {
            LlmError::Http { body_start, .. } => {
                format!("{self}; server answer started with: {body_start}")
            }
            other => other.to_string(),
        }
    }
}

/// Client for `POST /v1/chat/completions` (streaming) and `GET /v1/models`.
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    stall_timeout: Duration,
}

impl LlmClient {
    /// With an `api_key` every request (both endpoints) carries
    /// `Authorization: Bearer <key>`; the key never appears in `Debug`.
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
        connect_timeout: Duration,
        stall_timeout: Duration,
    ) -> Self {
        let mut builder = reqwest::Client::builder().connect_timeout(connect_timeout);
        if let Some(key) = api_key {
            let mut headers = reqwest::header::HeaderMap::new();
            let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                .expect("api keys with visible ascii work here; invalid ones fail the request");
            headers.insert(reqwest::header::AUTHORIZATION, value);
            builder = builder.default_headers(headers);
        }
        let http = builder
            .build()
            .expect("reqwest client builds with the rustls backend");
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            model: model.into(),
            stall_timeout,
        }
    }

    /// The model this client is configured for.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Stream the parts of the response: non-empty `delta.content` pieces as
    /// `Content`, non-empty `delta.reasoning` / `delta.reasoning_content`
    /// pieces as `Reasoning`, and one `Finish` at `[DONE]` carrying the last
    /// seen finish reason and usage.
    ///
    /// The stream yields items until the first error (then ends), ends after
    /// the `Finish` at `[DONE]`, and ends without an item as soon as `cancel`
    /// fires (then no `Finish` is sent).
    pub fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> futures_util::stream::BoxStream<'static, Result<StreamPart, LlmError>> {
        let http = self.http.clone();
        let url = format!("{}{}", self.base_url, CHAT_PATH);
        let stall = self.stall_timeout;
        futures_util::stream::unfold(State::Init(request), move |state| {
            let http = http.clone();
            let url = url.clone();
            let cancel = cancel.clone();
            async move {
                let mut state = state;
                loop {
                    state = match state {
                        State::Init(request) => {
                            let opened = tokio::select! {
                                biased;
                                _ = cancel.cancelled() => return None,
                                r = open_chat(&http, &url, &request, stall) => r,
                            };
                            match opened {
                                Ok(events) => State::Streaming {
                                    events,
                                    finish_reason: None,
                                    usage: None,
                                    pending: std::collections::VecDeque::new(),
                                },
                                Err(err) => return Some((Err(err), State::Done)),
                            }
                        }
                        State::Streaming {
                            mut events,
                            mut finish_reason,
                            mut usage,
                            mut pending,
                        } => {
                            loop {
                                if let Some(part) = pending.pop_front() {
                                    return Some((
                                        Ok(part),
                                        State::Streaming {
                                            events,
                                            finish_reason,
                                            usage,
                                            pending,
                                        },
                                    ));
                                }
                                let next = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => return None,
                                    r = tokio::time::timeout(stall, events.next()) => r,
                                };
                                let event = match next {
                                    Err(_elapsed) => {
                                        return Some((Err(LlmError::Stalled), State::Done));
                                    }
                                    // Body ended without `[DONE]`.
                                    Ok(None) => {
                                        return Some((Err(LlmError::Closed), State::Done));
                                    }
                                    Ok(Some(Err(err))) => {
                                        let err = match err {
                                            EventStreamError::Transport(_) => LlmError::Closed,
                                            EventStreamError::Utf8(_)
                                            | EventStreamError::Parser(_) => LlmError::Decode,
                                        };
                                        return Some((Err(err), State::Done));
                                    }
                                    Ok(Some(Ok(event))) => event,
                                };
                                if event.data == "[DONE]" {
                                    return Some((
                                        Ok(StreamPart::Finish {
                                            reason: finish_reason,
                                            usage,
                                        }),
                                        State::Done,
                                    ));
                                }
                                let Ok(chunk) =
                                    serde_json::from_str::<crate::types::ChatChunk>(&event.data)
                                else {
                                    return Some((Err(LlmError::Decode), State::Done));
                                };
                                // A chunk may carry only a finish reason, only
                                // usage (empty `choices`), or both; record them
                                // for the `Finish` at `[DONE]`.
                                finish_reason =
                                    chunk.finish_reason().map(str::to_owned).or(finish_reason);
                                usage = chunk.usage().or(usage);
                                // Ignore empty pieces; the spec says so. A
                                // chunk carrying content and reasoning at once
                                // queues both, content first.
                                if let Some(text) = chunk.content()
                                    && !text.is_empty()
                                {
                                    pending.push_back(StreamPart::Content(text.to_string()));
                                }
                                if let Some(text) = chunk.reasoning()
                                    && !text.is_empty()
                                {
                                    pending.push_back(StreamPart::Reasoning(text.to_string()));
                                }
                            }
                        }
                        State::Done => return None,
                    };
                }
            }
        })
        .boxed()
    }

    /// Collect a whole response into one `Completion` (used for compression).
    ///
    /// Fails on the first stream error, keeping nothing of the partial text.
    pub async fn complete(&self, request: ChatRequest) -> Result<Completion, LlmError> {
        let mut stream = self.stream(request, CancellationToken::new());
        let mut completion = Completion::default();
        while let Some(item) = stream.next().await {
            match item? {
                StreamPart::Content(text) => completion.text.push_str(&text),
                StreamPart::Reasoning(text) => completion.reasoning.push_str(&text),
                StreamPart::Finish { reason, usage } => {
                    completion.finish_reason = reason;
                    completion.usage = usage;
                }
            }
        }
        Ok(completion)
    }

    /// The ids from `GET /v1/models`.
    pub async fn models(&self) -> Result<Vec<String>, LlmError> {
        let url = format!("{}{}", self.base_url, MODELS_PATH);
        let body = match self.http.get(&url).send().await {
            Ok(response) => {
                let status = response.status();
                let text = match tokio::time::timeout(self.stall_timeout, response.text()).await {
                    Ok(Ok(text)) => text,
                    Ok(Err(_)) => return Err(LlmError::Decode),
                    Err(_elapsed) => return Err(LlmError::Stalled),
                };
                if !status.is_success() {
                    return Err(LlmError::Http {
                        status: status.as_u16(),
                        body_start: body_start(&text),
                    });
                }
                text
            }
            Err(_) => return Err(LlmError::Connect),
        };
        let parsed: ModelsResponse = serde_json::from_str(&body).map_err(|_| LlmError::Decode)?;
        Ok(parsed.data.into_iter().map(|entry| entry.id).collect())
    }
}

/// Send the request and wrap the response body in an SSE event stream.
async fn open_chat(
    http: &reqwest::Client,
    url: &str,
    request: &ChatRequest,
    stall_timeout: Duration,
) -> Result<EvStream, LlmError> {
    let response = match http.post(url).json(request).send().await {
        Ok(response) => response,
        // Everything before a response exists is unreachable-server territory:
        // refused, unresolvable, or past the connect timeout.
        Err(_) => return Err(LlmError::Connect),
    };
    let status = response.status();
    if !status.is_success() {
        let text = match tokio::time::timeout(stall_timeout, response.text()).await {
            Ok(Ok(text)) => text,
            _ => String::new(),
        };
        return Err(LlmError::Http {
            status: status.as_u16(),
            body_start: body_start(&text),
        });
    }
    Ok(response.bytes_stream().eventsource().boxed())
}

/// The first 200 bytes of a body, cut at a character boundary.
fn body_start(body: &str) -> String {
    if body.len() <= BODY_START_BYTES {
        return body.to_string();
    }
    let mut end = BODY_START_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].to_string()
}

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    #[serde(default)]
    id: String,
}

impl fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("stall_timeout", &self.stall_timeout)
            .finish()
    }
}
