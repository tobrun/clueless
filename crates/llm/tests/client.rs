//! The LLM client against a scripted local SSE mock (never the real server).

mod support;

use std::time::Duration;

use futures_util::{Stream, StreamExt};
use llm::client::{LlmClient, LlmError};
use llm::types::{ChatRequest, Message, StreamPart};
use serde_json::json;
use support::{Mock, Reply, Step};
use tokio_util::sync::CancellationToken;

const CONNECT: Duration = Duration::from_secs(2);

fn request() -> ChatRequest {
    support::test_request(true)
}

/// Drain a suggestion stream into its content items and the error that ended
/// it, if any; reasoning and finish parts are not content and drop out here.
async fn drain(
    mut stream: impl Stream<Item = Result<StreamPart, LlmError>> + Unpin,
) -> (Vec<String>, Option<LlmError>) {
    let mut items = Vec::new();
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamPart::Content(text)) => items.push(text),
            Ok(StreamPart::Reasoning(_)) | Ok(StreamPart::Finish { .. }) => {}
            Err(err) => {
                error = Some(err);
                break;
            }
        }
    }
    (items, error)
}

/// Stream the standard request against the mock with the default client and
/// drain it into its content items and the error that ended it, if any.
async fn stream_content(mock: &Mock) -> (Vec<String>, Option<LlmError>) {
    let client = support::test_client(&mock.base_url);
    drain(Box::pin(client.stream(request(), CancellationToken::new()))).await
}

/// Stream once and list models, so both endpoints have been seen.
async fn hit_both_endpoints(client: &LlmClient) {
    let _ = drain(Box::pin(client.stream(request(), CancellationToken::new()))).await;
    client.models().await.expect("models request");
}

#[tokio::test]
async fn stream_yields_non_empty_content_deltas_and_ends_at_done() {
    let mock = Mock::start(Reply::contents(&["", "Hel", "lo"])).await;
    let (items, error) = stream_content(&mock).await;
    assert_eq!(items, vec!["Hel".to_string(), "lo".to_string()]);
    assert_eq!(error, None);
    // The thinking-off body really crossed the HTTP boundary.
    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies[0].contains(r#""chat_template_kwargs":{"enable_thinking":false}"#),
        "{}",
        bodies[0]
    );
}

#[tokio::test]
async fn unset_enable_thinking_omits_chat_template_kwargs_from_the_body() {
    let mock = Mock::start(Reply::contents(&["ok"])).await;
    let client = support::test_client(&mock.base_url);
    let request = ChatRequest::new(
        "mock-model",
        vec![Message::user("hi")],
        220,
        0.4,
        None,
        true,
    );
    let _ = drain(Box::pin(client.stream(request, CancellationToken::new()))).await;
    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 1);
    assert!(
        !bodies[0].contains("chat_template_kwargs"),
        "generic servers must not see the kwarg: {}",
        bodies[0]
    );
}

#[tokio::test]
async fn an_api_key_becomes_a_bearer_header_on_both_endpoints() {
    let mock = Mock::start(Reply::contents(&["ok"])).await;
    let client = LlmClient::new(
        &mock.base_url,
        "mock-model",
        Some("sk-test-key".to_string()),
        CONNECT,
        Duration::from_secs(10),
    );
    hit_both_endpoints(&client).await;
    assert_eq!(
        mock.authorizations(),
        vec![
            Some("Bearer sk-test-key".to_string()),
            Some("Bearer sk-test-key".to_string()),
        ]
    );
}

#[tokio::test]
async fn no_api_key_sends_no_authorization_header() {
    let mock = Mock::start(Reply::contents(&["ok"])).await;
    let client = support::test_client(&mock.base_url);
    hit_both_endpoints(&client).await;
    assert_eq!(mock.authorizations(), vec![None, None]);
}

#[tokio::test]
async fn reasoning_deltas_before_content_never_show_up_as_content() {
    let mock = Mock::start(Reply::stream(vec![
        Step::Reasoning("thinking ".into()),
        Step::Reasoning("very hard".into()),
        Step::Chunk("Hello".into()),
        Step::Chunk("!".into()),
    ]))
    .await;
    let (items, error) = stream_content(&mock).await;
    assert_eq!(items, vec!["Hello".to_string(), "!".to_string()]);
    assert_eq!(error, None);
}

#[tokio::test]
async fn reasoning_content_deltas_yield_no_content() {
    let mock = Mock::start(Reply::stream(vec![
        Step::ReasoningContent("hidden ".into()),
        Step::ReasoningContent("still hidden".into()),
    ]))
    .await;
    let (items, error) = stream_content(&mock).await;
    assert!(
        items.is_empty(),
        "reasoning_content must never be yielded, got {items:?}"
    );
    assert_eq!(error, None);
}

#[tokio::test]
async fn silent_connection_past_the_stall_timeout_ends_as_stalled() {
    let mock = Mock::start(Reply::Stream(vec![
        Step::Chunk("one".into()),
        Step::Chunk("two".into()),
        Step::Hold(Duration::from_secs(1)),
    ]))
    .await;
    // Spec timing replaced for the test: stall at 200 ms.
    let client = LlmClient::new(
        &mock.base_url,
        "mock-model",
        None,
        CONNECT,
        Duration::from_millis(200),
    );
    let started = std::time::Instant::now();
    let (items, error) = drain(Box::pin(client.stream(request(), CancellationToken::new()))).await;
    assert_eq!(items, vec!["one".to_string(), "two".to_string()]);
    assert_eq!(error, Some(LlmError::Stalled));
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "stalled too late: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn connection_closed_before_done_ends_as_closed() {
    let mock = Mock::start(Reply::Stream(vec![Step::Chunk("Only piece.".into())])).await;
    let (items, error) = stream_content(&mock).await;
    assert_eq!(items, vec!["Only piece.".to_string()]);
    assert_eq!(error, Some(LlmError::Closed));
}

#[tokio::test]
async fn event_data_that_is_not_json_ends_as_decode() {
    let mock = Mock::start(Reply::Stream(vec![Step::Raw(
        "data: <<not json>>\n\n".into(),
    )]))
    .await;
    let (items, error) = stream_content(&mock).await;
    assert!(items.is_empty());
    assert_eq!(error, Some(LlmError::Decode));
}

#[tokio::test]
async fn cancelling_after_a_delta_ends_the_stream_and_closes_the_connection() {
    let run = support::cancel_run(true).await;
    let mut stream = run.stream;

    let started = std::time::Instant::now();
    run.cancel.cancel();
    assert_eq!(
        stream.next().await,
        None,
        "stream must end without an item on cancel"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cancel too late: {:?}",
        started.elapsed()
    );
    drop(stream);

    assert!(
        run.mock.wait_disconnected(Duration::from_secs(2)).await,
        "the mock never saw the connection close"
    );
}

#[tokio::test]
async fn http_400_with_json_error_body_reports_status_and_body_start() {
    let message = "The requested maximum length is longer than the model context window allows; reduce max_tokens and send the request again with a smaller value, or start a shorter conversation.";
    let body =
        json!({ "error": { "message": message, "type": "invalid_request_error" } }).to_string();
    assert!(
        body.len() > 200,
        "test body must exceed the 200-byte prefix, is {}",
        body.len()
    );
    let mock = Mock::start(Reply::error(400, message)).await;
    let (items, error) = stream_content(&mock).await;
    assert!(items.is_empty());
    let Some(LlmError::Http { status, body_start }) = error else {
        panic!("expected an Http error, got {error:?}");
    };
    assert_eq!(status, 400);
    assert_eq!(body_start, &body[..200]);
    assert_eq!(body_start.len(), 200);
}

#[tokio::test]
async fn nothing_listening_ends_as_connect() {
    // Bind and release a port so something recent is definitely not listening.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("free port");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    let client = support::test_client(&format!("http://{addr}"));
    let (items, error) = drain(Box::pin(client.stream(request(), CancellationToken::new()))).await;
    assert!(items.is_empty());
    assert_eq!(error, Some(LlmError::Connect));
}

#[tokio::test]
async fn complete_returns_the_concatenated_content() {
    let mock = Mock::start(Reply::contents(&["Hello", ", ", "world"])).await;
    let client = support::test_client(&mock.base_url);
    let completion = client.complete(request()).await.expect("complete succeeds");
    assert_eq!(completion.text, "Hello, world");
    assert_eq!(completion.reasoning, "");
}

#[tokio::test]
async fn one_event_split_across_two_tcp_writes_is_one_delta() {
    let frame = support::chunk_frame(json!({"content": "SplitHello"}));
    let cut = frame.find("SplitHello").expect("content in frame") + "SplitHel".len();
    let (first, second) = frame.split_at(cut);
    let mock = Mock::start(Reply::Stream(vec![
        Step::Raw(first.to_string()),
        Step::Pause(Duration::from_millis(100)),
        Step::Raw(second.to_string()),
        Step::Done,
    ]))
    .await;
    let (items, error) = stream_content(&mock).await;
    assert_eq!(items, vec!["SplitHello".to_string()]);
    assert_eq!(error, None);
}
