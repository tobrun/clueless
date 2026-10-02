//! Integration tests: the real ASR client against a local axum mock of the
//! ASR server, with injected short timeouts and backoffs.

mod support;

use std::time::{Duration, Instant};

use asr::client::{AsrClient, AsrError};
use support::{MockAsr, Reply, dead_url};

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

fn client_at(base_url: &str) -> AsrClient {
    AsrClient::new(base_url, "test-model", ms(2_000), [ms(20), ms(20)])
}

fn silence(n: usize) -> Vec<f32> {
    vec![0.0f32; n]
}

#[tokio::test]
async fn a_transcription_answer_returns_the_text_and_posts_the_specced_multipart() {
    let mock = MockAsr::start().await;
    mock.enqueue(Reply::body(r#"{"text":"hello"}"#));

    let result = client_at(&mock.base_url)
        .transcribe(&silence(16_000), 1)
        .await;

    assert_eq!(result, Ok(Some("hello".to_owned())));
    let seen = mock.last_request().expect("the mock saw the request");
    assert_eq!(seen.file_field.as_deref(), Some("file"));
    assert_eq!(seen.file_name.as_deref(), Some("seg.wav"));
    assert_eq!(seen.file_content_type.as_deref(), Some("audio/wav"));
    assert_eq!(
        seen.file_len,
        44 + 2 * 16_000,
        "the WAV body is 16-bit mono"
    );
    let mut fields = seen.text_fields.clone();
    fields.sort();
    assert_eq!(
        fields,
        vec![
            ("model".to_owned(), "test-model".to_owned()),
            ("response_format".to_owned(), "json".to_owned()),
        ]
    );
}

#[tokio::test]
async fn an_empty_text_is_no_speech_not_an_error() {
    let mock = MockAsr::start().await;
    mock.enqueue(Reply::body(r#"{"text":""}"#));

    let result = client_at(&mock.base_url)
        .transcribe(&silence(1_600), 1)
        .await;

    assert_eq!(result, Ok(None));
}

#[tokio::test]
async fn a_punctuation_only_text_is_no_speech_not_an_error() {
    let mock = MockAsr::start().await;
    mock.enqueue(Reply::body(r#"{"text":" . "}"#));

    let result = client_at(&mock.base_url)
        .transcribe(&silence(1_600), 1)
        .await;

    assert_eq!(result, Ok(None));
}

#[tokio::test]
async fn server_errors_are_retried_until_the_answer_arrives() {
    let mock = MockAsr::start().await;
    mock.enqueue(Reply::status(500, "boom"));
    mock.enqueue(Reply::status(503, "later"));
    mock.enqueue(Reply::body(r#"{"text":"at last"}"#));

    let started = Instant::now();
    let result = client_at(&mock.base_url)
        .transcribe(&silence(1_600), 3)
        .await;

    assert_eq!(result, Ok(Some("at last".to_owned())));
    assert_eq!(mock.request_count(), 3, "two failures then the answer");
    // Two waits of 20 ms each sit between the three attempts.
    assert!(started.elapsed() >= ms(40), "backoff waits were skipped");
}

#[tokio::test]
async fn a_client_error_fails_at_once_without_retrying() {
    let mock = MockAsr::start().await;
    mock.enqueue(Reply::status(400, "bad request"));

    let result = client_at(&mock.base_url)
        .transcribe(&silence(1_600), 3)
        .await;

    assert_eq!(result, Err(AsrError::Http { status: 400 }));
    assert_eq!(mock.request_count(), 1, "4xx is never retried");
}

#[tokio::test]
async fn a_server_answer_slowed_past_the_attempt_timeout_is_a_timeout() {
    let mock = MockAsr::start().await;
    mock.enqueue(Reply::body(r#"{"text":"late"}"#).delayed(ms(600)));
    let client = AsrClient::new(&mock.base_url, "test-model", ms(200), [ms(20), ms(20)]);

    let result = client.transcribe(&silence(1_600), 1).await;

    assert_eq!(result, Err(AsrError::Timeout));
}

#[tokio::test]
async fn a_port_with_nothing_listening_is_a_connect_error() {
    let dead = dead_url().await;
    let client = client_at(&dead);

    let result = client.transcribe(&silence(1_600), 1).await;

    assert_eq!(result, Err(AsrError::Connect));
}

#[tokio::test]
async fn a_success_body_that_is_not_the_expected_json_is_a_decode_error() {
    let mock = MockAsr::start().await;
    mock.enqueue(Reply::body("not json"));

    let result = client_at(&mock.base_url)
        .transcribe(&silence(1_600), 1)
        .await;

    assert_eq!(result, Err(AsrError::Decode));
}

#[tokio::test]
async fn models_lists_the_ids_from_the_data_array() {
    let mock = MockAsr::start().await;
    mock.set_models(
        r#"{"object":"list","data":[{"id":"alpha"},{"id":"beta","created":1}],"first_id":"alpha"}"#,
    );

    let ids = client_at(&mock.base_url).models().await.unwrap();

    assert_eq!(ids, vec!["alpha".to_owned(), "beta".to_owned()]);
}
