//! The `StreamPart` sequence the client yields from a scripted SSE stream:
//! content, reasoning, and the terminal `Finish` with finish reason and usage.

mod support;

use std::time::Duration;

use futures_util::StreamExt;
use llm::client::{LlmClient, LlmError};
use llm::types::{ChatRequest, Message, StreamPart, Usage};
use serde_json::json;
use support::{Mock, Reply, Step};
use tokio_util::sync::CancellationToken;

const CONNECT: Duration = Duration::from_secs(2);
const STALL: Duration = Duration::from_secs(10);

fn client_for(base_url: &str) -> LlmClient {
    LlmClient::new(base_url, "mock-model", None, CONNECT, STALL)
}

fn request(include_usage: bool) -> ChatRequest {
    ChatRequest::new(
        "mock-model",
        vec![Message::user("hi")],
        220,
        0.4,
        Some(false),
        include_usage,
    )
}

/// Drain a stream into its parts and the error that ended it, if any.
async fn drain_parts(
    stream: futures_util::stream::BoxStream<'static, Result<StreamPart, LlmError>>,
) -> (Vec<StreamPart>, Option<LlmError>) {
    let mut stream = Box::pin(stream);
    let mut parts = Vec::new();
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(part) => parts.push(part),
            Err(err) => {
                error = Some(err);
                break;
            }
        }
    }
    (parts, error)
}

#[tokio::test]
async fn content_chunks_then_done_yield_content_parts_and_an_empty_finish() {
    let mock = Mock::start(Reply::contents(&["Hel", "lo"])).await;
    let client = client_for(&mock.base_url);
    let (parts, error) = drain_parts(client.stream(request(false), CancellationToken::new())).await;
    assert_eq!(
        parts,
        vec![
            StreamPart::Content("Hel".to_string()),
            StreamPart::Content("lo".to_string()),
            StreamPart::Finish {
                reason: None,
                usage: None,
            },
        ]
    );
    assert_eq!(error, None);
}

#[tokio::test]
async fn reasoning_content_chunks_arrive_as_reasoning_parts_before_the_content() {
    let mock = Mock::start(Reply::stream(vec![
        Step::ReasoningContent("thinking ".into()),
        Step::ReasoningContent("very hard".into()),
        Step::Chunk("Hello".into()),
    ]))
    .await;
    let client = client_for(&mock.base_url);
    let (parts, error) = drain_parts(client.stream(request(false), CancellationToken::new())).await;
    assert_eq!(
        parts,
        vec![
            StreamPart::Reasoning("thinking ".to_string()),
            StreamPart::Reasoning("very hard".to_string()),
            StreamPart::Content("Hello".to_string()),
            StreamPart::Finish {
                reason: None,
                usage: None,
            },
        ],
        "reasoning parts must keep their server order and precede the content"
    );
    assert_eq!(error, None);
}

#[tokio::test]
async fn reasoning_field_arrives_as_a_reasoning_part() {
    let mock = Mock::start(Reply::stream(vec![Step::Reasoning("pondering".into())])).await;
    let client = client_for(&mock.base_url);
    let (parts, error) = drain_parts(client.stream(request(false), CancellationToken::new())).await;
    assert_eq!(
        parts,
        vec![
            StreamPart::Reasoning("pondering".to_string()),
            StreamPart::Finish {
                reason: None,
                usage: None,
            },
        ]
    );
    assert_eq!(error, None);
}

#[tokio::test]
async fn finish_reason_and_usage_chunks_are_carried_in_the_finish_part() {
    let mock = Mock::start(Reply::stream(vec![
        Step::Chunk("Answer.".into()),
        Step::Finish("length".into()),
        Step::Usage(json!({"prompt_tokens": 12, "completion_tokens": 8, "total_tokens": 20})),
    ]))
    .await;
    let client = client_for(&mock.base_url);
    let (parts, error) = drain_parts(client.stream(request(true), CancellationToken::new())).await;
    assert_eq!(
        parts.last(),
        Some(&StreamPart::Finish {
            reason: Some("length".to_string()),
            usage: Some(Usage {
                prompt_tokens: Some(12),
                completion_tokens: Some(8),
                total_tokens: Some(20),
            }),
        }),
        "parts: {parts:?}"
    );
    assert_eq!(error, None);
}

#[tokio::test]
async fn a_usage_without_total_tokens_keeps_the_two_counts_that_are_present() {
    let mock = Mock::start(Reply::stream(vec![Step::Usage(json!({
        "prompt_tokens": 12,
        "completion_tokens": 8,
    }))]))
    .await;
    let client = client_for(&mock.base_url);
    let (parts, error) = drain_parts(client.stream(request(true), CancellationToken::new())).await;
    assert_eq!(
        parts.last(),
        Some(&StreamPart::Finish {
            reason: None,
            usage: Some(Usage {
                prompt_tokens: Some(12),
                completion_tokens: Some(8),
                total_tokens: None,
            }),
        }),
        "parts: {parts:?}"
    );
    assert_eq!(error, None, "a partial usage must not error the stream");
}

#[tokio::test]
async fn an_unreadable_usage_object_yields_no_usage_and_no_error() {
    let mock = Mock::start(Reply::stream(vec![Step::Usage(json!("n/a"))])).await;
    let client = client_for(&mock.base_url);
    let (parts, error) = drain_parts(client.stream(request(true), CancellationToken::new())).await;
    assert_eq!(
        parts.last(),
        Some(&StreamPart::Finish {
            reason: None,
            usage: None,
        }),
        "parts: {parts:?}"
    );
    assert_eq!(error, None, "an odd usage must not error the stream");
}

#[tokio::test]
async fn include_usage_sends_stream_options_in_the_body() {
    let mock = Mock::start(Reply::contents(&["ok"])).await;
    let client = client_for(&mock.base_url);
    let _ = drain_parts(client.stream(request(true), CancellationToken::new())).await;
    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies[0].contains(r#""stream_options":{"include_usage":true}"#),
        "{}",
        bodies[0]
    );
}

#[tokio::test]
async fn without_include_usage_the_body_has_no_stream_options_key() {
    let mock = Mock::start(Reply::contents(&["ok"])).await;
    let client = client_for(&mock.base_url);
    let _ = drain_parts(client.stream(request(false), CancellationToken::new())).await;
    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 1);
    assert!(
        !bodies[0].contains("stream_options"),
        "the switch is off, the key must be absent: {}",
        bodies[0]
    );
}

#[tokio::test]
async fn cancelling_mid_stream_ends_without_further_items_and_without_finish() {
    let mock = Mock::start(Reply::Stream(vec![
        Step::Chunk("first".into()),
        Step::Hold(Duration::from_secs(10)),
    ]))
    .await;
    let client = client_for(&mock.base_url);
    let cancel = CancellationToken::new();
    let mut stream = Box::pin(client.stream(request(false), cancel.clone()));
    assert_eq!(
        stream.next().await,
        Some(Ok(StreamPart::Content("first".into())))
    );
    cancel.cancel();
    assert_eq!(
        stream.next().await,
        None,
        "the cancelled stream must end without a Finish or any item"
    );
}

#[tokio::test]
async fn a_body_without_done_ends_as_closed_without_finish() {
    let mock = Mock::start(Reply::Stream(vec![
        Step::Chunk("Half an".into()),
        Step::Chunk(" answer.".into()),
    ]))
    .await;
    let client = client_for(&mock.base_url);
    let (parts, error) = drain_parts(client.stream(request(false), CancellationToken::new())).await;
    assert_eq!(
        parts,
        vec![
            StreamPart::Content("Half an".to_string()),
            StreamPart::Content(" answer.".to_string()),
        ],
        "a Closed stream must not carry a Finish"
    );
    assert_eq!(error, Some(LlmError::Closed));
}

#[tokio::test]
async fn complete_returns_joined_text_and_the_usage() {
    let mock = Mock::start(Reply::stream(vec![
        Step::Chunk("Hello".into()),
        Step::Chunk(", world".into()),
        Step::Finish("stop".into()),
        Step::Usage(json!({"prompt_tokens": 30, "completion_tokens": 4, "total_tokens": 34})),
    ]))
    .await;
    let client = client_for(&mock.base_url);
    let completion = client
        .complete(request(true))
        .await
        .expect("complete succeeds");
    assert_eq!(completion.text, "Hello, world");
    assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        completion.usage,
        Some(Usage {
            prompt_tokens: Some(30),
            completion_tokens: Some(4),
            total_tokens: Some(34),
        })
    );
}

#[tokio::test]
async fn http_500_display_is_unchanged_and_detail_carries_the_body_start() {
    let mock = Mock::start(Reply::Status(500, "boom".to_string())).await;
    let client = client_for(&mock.base_url);
    let (_parts, error) =
        drain_parts(client.stream(request(false), CancellationToken::new())).await;
    let error = error.expect("a 500 is an error item");
    // The text shown to users is exactly what it was before this change.
    assert_eq!(error.to_string(), "LLM error 500");
    let detail = error.detail();
    assert!(detail.contains("500"), "{detail}");
    assert!(detail.contains("boom"), "{detail}");
}

/// The recorded live stream (fixtures/llm/answer-stream.jsonl) replays through
/// the same parser without a live server: content parts, a Finish with usage,
/// and [DONE]. This recorded fallback is what the non-live mode runs on.
#[tokio::test]
async fn the_recorded_live_stream_replays_with_content_usage_and_done() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/llm/answer-stream.jsonl");
    let text = std::fs::read_to_string(&fixture)
        .expect("fixtures/llm/answer-stream.jsonl is committed with the repo");
    let steps: Vec<Step> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Step::Raw(format!("data: {line}\n\n")))
        .collect();
    assert!(!steps.is_empty(), "the recorded fixture is empty");

    let mock = Mock::start(Reply::Stream(steps)).await;
    let client = client_for(&mock.base_url);
    let (parts, error) = drain_parts(client.stream(request(true), CancellationToken::new())).await;
    assert!(error.is_none(), "replay failed: {error:?}");

    let content: String = parts
        .iter()
        .filter_map(|p| match p {
            StreamPart::Content(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(!content.trim().is_empty(), "replayed answer is empty");
    let finish_usage = parts.iter().find_map(|p| match p {
        StreamPart::Finish { usage, .. } => Some(usage.clone()),
        _ => None,
    });
    assert!(
        matches!(finish_usage, Some(Some(ref u)) if u.total_tokens.is_some_and(|t| t > 0)),
        "the replayed Finish carries no usage: {finish_usage:?}"
    );
}
