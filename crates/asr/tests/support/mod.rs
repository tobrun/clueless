//! A local axum mock of the ASR server, shared by the integration tests.
//!
//! The transcription route consumes the real multipart request, records what
//! it saw, and answers with the next scripted reply (after its delay), so
//! tests exercise real HTTP with injectable durations.

// Every test binary uses only part of this module.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Multipart, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

/// One scripted answer for the transcription route.
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}

impl Reply {
    /// A 200 with a raw body string (tests pass JSON text to it).
    pub fn body(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// What the mock saw on one transcription request.
#[derive(Clone, Debug, PartialEq)]
pub struct SeenRequest {
    /// Name and value of every text (non-file) multipart field.
    pub text_fields: Vec<(String, String)>,
    /// Name of the file field, if one arrived.
    pub file_field: Option<String>,
    pub file_name: Option<String>,
    pub file_content_type: Option<String>,
    pub file_len: usize,
}

#[derive(Clone)]
struct MockState {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    script: VecDeque<Reply>,
    fallback: Reply,
    models_body: String,
    requests: Vec<SeenRequest>,
}

/// Handle to a running mock server; the server lives until the test runtime
/// is dropped.
pub struct MockAsr {
    /// Something like `http://127.0.0.1:53124`.
    pub base_url: String,
    state: MockState,
}

impl MockAsr {
    /// Start the mock on a free loopback port (call inside a tokio runtime).
    pub async fn start() -> MockAsr {
        let state = MockState {
            inner: Arc::new(Mutex::new(Inner {
                script: VecDeque::new(),
                fallback: Reply::body(r#"{"text":"ok"}"#),
                models_body: r#"{"object":"list","data":[]}"#.to_owned(),
                requests: Vec::new(),
            })),
        };
        let app = axum::Router::new()
            .route("/v1/audio/transcriptions", post(transcribe))
            .route("/v1/models", get(models))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("can bind a loopback port");
        let addr = listener.local_addr().expect("bound address is known");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock server runs until the runtime ends");
        });
        MockAsr {
            base_url: format!("http://{addr}"),
            state,
        }
    }

    /// Queue one answer, consumed in order; when the script runs out the
    /// fallback `{"text":"ok"}` answers.
    pub fn enqueue(&self, reply: Reply) {
        self.lock().script.push_back(reply);
    }

    /// Set the raw body `GET /v1/models` will answer with.
    pub fn set_models(&self, body: impl Into<String>) {
        self.lock().models_body = body.into();
    }

    /// Number of transcription requests seen so far.
    pub fn request_count(&self) -> usize {
        self.lock().requests.len()
    }

    /// The last transcription request the mock saw, if any.
    pub fn last_request(&self) -> Option<SeenRequest> {
        self.lock().requests.last().cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.state
            .inner
            .lock()
            .expect("mock lock is never poisoned")
    }
}

async fn transcribe(State(state): State<MockState>, mut mp: Multipart) -> Response {
    let mut seen = SeenRequest {
        text_fields: Vec::new(),
        file_field: None,
        file_name: None,
        file_content_type: None,
        file_len: 0,
    };
    while let Some(field) = mp.next_field().await.expect("multipart parses") {
        let name = field.name().unwrap_or_default().to_owned();
        match field.file_name() {
            Some(file_name) => {
                seen.file_field = Some(name);
                seen.file_name = Some(file_name.to_owned());
                seen.file_content_type = field.content_type().map(str::to_owned);
                seen.file_len = field.bytes().await.expect("field body arrives").len();
            }
            None => seen
                .text_fields
                .push((name, field.text().await.expect("field text arrives"))),
        }
    }
    let reply = {
        let mut inner = state.inner.lock().expect("mock lock is never poisoned");
        inner.requests.push(seen);
        inner.script.pop_front().unwrap_or_else(|| Reply {
            delay: inner.fallback.delay,
            status: inner.fallback.status,
            body: inner.fallback.body.clone(),
        })
    };
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    let status = StatusCode::from_u16(reply.status).expect("tests only script valid http statuses");
    let mut response = (status, reply.body).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

async fn models(State(state): State<MockState>) -> Response {
    let body = state
        .inner
        .lock()
        .expect("mock lock is never poisoned")
        .models_body
        .clone();
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// A loopback URL with nothing listening behind it.
pub async fn dead_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("can bind a loopback port");
    let addr = listener.local_addr().expect("bound address is known");
    drop(listener);
    format!("http://{addr}")
}
