//! A scripted SSE mock of the LLM server, shared by the integration tests.
//!
//! The script is a list of steps written to the connection as raw frames, so a
//! test can control exact bytes, write gaps and disconnects.

// This harness is compiled into every integration test binary, and no single
// binary exercises every step, reply shape and accessor.
#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::header::{self, HeaderMap};
use axum::response::Response;
use axum::routing::post;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// One scripted action on the response connection.
#[derive(Debug, Clone)]
pub enum Step {
    /// Write this exact byte sequence as one write.
    Raw(String),
    /// Write one `delta.content` chunk for this text.
    Chunk(String),
    /// Write one `delta.reasoning` chunk for this text.
    Reasoning(String),
    /// Write one `delta.reasoning_content` chunk for this text.
    ReasoningContent(String),
    /// Write one chunk whose first choice carries this `finish_reason`.
    Finish(String),
    /// Write one chunk with empty `choices` and this exact `usage` value
    /// (an object like the server's, or anything unreadable on purpose).
    Usage(serde_json::Value),
    /// Write the `data: [DONE]` event.
    Done,
    /// Wait before the next step, keeping the connection open.
    Pause(Duration),
    /// Keep the connection open this long with comment frames, then disconnect.
    Hold(Duration),
}

/// What `POST /v1/chat/completions` answers with.
#[derive(Debug, Clone)]
pub enum Reply {
    /// Scripted SSE steps; the connection closes when the script ends.
    Stream(Vec<Step>),
    /// Answer with this status and body, no SSE.
    Status(u16, String),
}

impl Reply {
    /// A script that streams these content pieces, then sends `[DONE]`.
    pub fn contents(pieces: &[&str]) -> Self {
        Self::stream(
            pieces
                .iter()
                .map(|p| Step::Chunk((*p).to_string()))
                .collect(),
        )
    }

    /// A script from steps, with `[DONE]` and a close appended.
    pub fn stream(steps: Vec<Step>) -> Self {
        let mut steps = steps;
        steps.push(Step::Done);
        Self::Stream(steps)
    }

    /// A JSON error body as vLLM would send it.
    pub fn error(status: u16, message: impl Into<String>) -> Self {
        let body =
            json!({ "error": { "message": message.into(), "type": "invalid_request_error" } });
        Self::Status(status, body.to_string())
    }
}

/// A running mock server. Aborted when dropped.
pub struct Mock {
    pub base_url: String,
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
    auths: Arc<std::sync::Mutex<Vec<Option<String>>>>,
    disconnected: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
}

impl Mock {
    pub async fn start(reply: Reply) -> Self {
        let state = MockState {
            reply,
            bodies: Arc::new(std::sync::Mutex::new(Vec::new())),
            auths: Arc::new(std::sync::Mutex::new(Vec::new())),
            disconnected: Arc::new(AtomicBool::new(false)),
        };
        let app = axum::Router::new()
            .route("/v1/chat/completions", post(handle))
            .route("/v1/models", axum::routing::get(handle_models))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let port = listener.local_addr().expect("mock addr").port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock server");
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            bodies: state.bodies,
            auths: state.auths,
            disconnected: state.disconnected,
            server,
        }
    }

    /// Bodies of chat requests the mock received, in arrival order.
    pub fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("mock bodies lock").clone()
    }

    /// The `Authorization` header of every request the mock saw (chat and
    /// models), in arrival order.
    pub fn authorizations(&self) -> Vec<Option<String>> {
        self.auths.lock().expect("mock auths lock").clone()
    }

    /// Whether the mock noticed the client close the response connection,
    /// checked every 10 ms until `within` has passed.
    pub async fn wait_disconnected(&self, within: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            if self.disconnected.load(Ordering::SeqCst) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[derive(Clone)]
struct MockState {
    reply: Reply,
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
    auths: Arc<std::sync::Mutex<Vec<Option<String>>>>,
    disconnected: Arc<AtomicBool>,
}

fn record_auth(auths: &std::sync::Mutex<Vec<Option<String>>>, headers: &HeaderMap) {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    auths.lock().expect("mock auths lock").push(value);
}

async fn handle(State(state): State<MockState>, headers: HeaderMap, body: String) -> Response {
    record_auth(&state.auths, &headers);
    state.bodies.lock().expect("mock bodies lock").push(body);
    match &state.reply {
        Reply::Status(code, text) => axum::http::Response::builder()
            .status(*code)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(text.clone()))
            .expect("status response"),
        Reply::Stream(steps) => {
            let (tx, rx) = mpsc::channel::<Result<String, std::io::Error>>(64);
            let disconnected = state.disconnected.clone();
            let steps = steps.clone();
            tokio::spawn(async move { run_script(tx, disconnected, steps).await });
            axum::http::Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from_stream(ReceiverStream::new(rx)))
                .expect("sse response")
        }
    }
}

async fn handle_models(State(state): State<MockState>, headers: HeaderMap) -> Response {
    record_auth(&state.auths, &headers);
    axum::http::Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "object": "list", "data": [{ "id": "mock-model", "object": "model" }] })
                .to_string(),
        ))
        .expect("models response")
}

/// Writes the script to the body channel; records a disconnect when the
/// receiving side is gone (the client dropped the connection).
async fn run_script(
    tx: mpsc::Sender<Result<String, std::io::Error>>,
    disconnected: Arc<AtomicBool>,
    steps: Vec<Step>,
) {
    for step in steps {
        let frame = match step {
            Step::Pause(duration) => {
                tokio::time::sleep(duration).await;
                continue;
            }
            Step::Hold(duration) => {
                // Comment frames keep write failures (and so disconnect detection)
                // quick while the connection is otherwise idle.
                let start = tokio::time::Instant::now();
                while start.elapsed() < duration {
                    if send(&tx, &disconnected, ": ping\n\n".to_string()).await {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                continue;
            }
            Step::Raw(frame) => frame,
            Step::Chunk(content) => chunk_frame(json!({"content": content})),
            Step::Reasoning(text) => chunk_frame(json!({"reasoning": text})),
            Step::ReasoningContent(text) => chunk_frame(json!({"reasoning_content": text})),
            Step::Finish(reason) => finish_frame(&reason),
            Step::Usage(usage) => usage_frame(usage),
            Step::Done => "data: [DONE]\n\n".to_string(),
        };
        if send(&tx, &disconnected, frame).await {
            return;
        }
    }
    // Script ended: drop the sender so the body ends and the connection closes.
    drop(tx);
}

/// Returns true when the send failed, meaning the client is gone.
async fn send(
    tx: &mpsc::Sender<Result<String, std::io::Error>>,
    disconnected: &Arc<AtomicBool>,
    frame: String,
) -> bool {
    if tx.send(Ok(frame)).await.is_err() {
        disconnected.store(true, Ordering::SeqCst);
        return true;
    }
    false
}

/// A chat chunk shaped like the real server's (extra fields, null token ids).
pub fn chunk_frame(delta: serde_json::Value) -> String {
    let chunk = json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 1759400000u64,
        "model": "mock-model",
        "system_fingerprint": null,
        "choices": [{
            "index": 0,
            "logprobs": null,
            "finish_reason": null,
            "token_ids": null,
            "delta": delta,
        }],
    });
    format!("data: {chunk}\n\n")
}

/// A chunk whose only payload is the choice's `finish_reason` (empty delta).
pub fn finish_frame(reason: &str) -> String {
    let chunk = json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 1759400000u64,
        "model": "mock-model",
        "system_fingerprint": null,
        "choices": [{
            "index": 0,
            "logprobs": null,
            "finish_reason": reason,
            "token_ids": null,
            "delta": {},
        }],
    });
    format!("data: {chunk}\n\n")
}

/// A usage-only chunk: empty `choices`, `usage` written exactly as given.
pub fn usage_frame(usage: serde_json::Value) -> String {
    let chunk = json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 1759400000u64,
        "model": "mock-model",
        "system_fingerprint": null,
        "usage": usage,
        "choices": [],
    });
    format!("data: {chunk}\n\n")
}
