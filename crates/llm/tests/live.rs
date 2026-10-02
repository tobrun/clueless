//! E2E against the real LLM server on the LAN.
//!
//! Ignored by default; the e2e pass runs them with `LIVE_SERVER=1`.
//! Without that variable they return early as passed.

use std::time::Duration;

use futures_util::StreamExt;
use llm::client::LlmClient;
use llm::types::{ChatRequest, Message};
use tokio_util::sync::CancellationToken;

const HOST: &str = "localhost";
const MODEL: &str = "your-model-id";

/// The live target, or `None` when this run is not the live pass.
fn live_target() -> Option<(String, String)> {
    match std::env::var("LIVE_SERVER").as_deref() {
        Ok("1") => Some((format!("http://{HOST}:8000"), MODEL.to_string())),
        _ => None,
    }
}

fn client(base_url: &str, model: &str) -> LlmClient {
    // Spec timings: connect 2 s, stall 10 s.
    LlmClient::new(
        base_url,
        model,
        Duration::from_secs(2),
        Duration::from_secs(10),
    )
}

#[tokio::test]
#[ignore = "hits the LAN server at localhost; run with LIVE_SERVER=1"]
async fn live_thinking_off_streams_plain_content_fast() {
    let Some((base_url, model)) = live_target() else {
        return;
    };
    let client = client(&base_url, &model);
    let request = ChatRequest::new(
        model,
        vec![Message::user("Say hello in five words.")],
        220,
        0.4,
    );

    let started = std::time::Instant::now();
    let mut stream = Box::pin(client.stream(request, CancellationToken::new()));
    let mut deltas = Vec::new();
    let mut first_delta = None;
    let mut errors = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(text) => {
                first_delta.get_or_insert_with(|| started.elapsed());
                deltas.push(text);
            }
            Err(err) => errors.push(err),
        }
    }
    assert!(errors.is_empty(), "live stream failed: {errors:?}");
    assert!(!deltas.is_empty(), "expected at least one content delta");
    let first_delta = first_delta.expect("non-empty deltas have a first one");
    assert!(
        first_delta < Duration::from_secs(5),
        "first content delta took {first_delta:?}"
    );
    let text: String = deltas.concat();
    assert!(
        !text.contains("<think>"),
        "thinking text leaked into content: {text:?}"
    );
}

#[tokio::test]
#[ignore = "hits the LAN server at localhost; run with LIVE_SERVER=1"]
async fn live_models_lists_the_copilot_model() {
    let Some((base_url, model)) = live_target() else {
        return;
    };
    let models = client(&base_url, &model)
        .models()
        .await
        .expect("models request");
    assert!(
        models.iter().any(|m| m == MODEL),
        "models() returned {models:?}"
    );
}
