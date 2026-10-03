//! Real-server tests: `LIVE_SERVER=1 cargo test -p llm --test live -- --ignored`
//! reads `LLM_*` from the process environment or the repo `.env`.

use futures_util::StreamExt;
use llm::client::LlmClient;
use llm::types::{ChatRequest, Message};

fn client(timeout_secs: u64) -> LlmClient {
    let base_url = dotenvy::var("LLM_BASE_URL").expect("LLM_BASE_URL required for live tests");
    let model = dotenvy::var("LLM_MODEL").expect("LLM_MODEL required for live tests");
    let api_key = dotenvy::var("LLM_API_KEY").ok().filter(|k| !k.is_empty());
    LlmClient::new(
        base_url,
        model,
        api_key,
        std::time::Duration::from_secs(10),
        std::time::Duration::from_secs(timeout_secs),
    )
}

/// The profile text, when `LLM_PROFILE_PATH` points at a readable file.
fn profile() -> Option<String> {
    dotenvy::var("LLM_PROFILE_PATH").ok().map(|path| {
        let path = strip_tilde(&path);
        std::fs::read_to_string(path).expect("LLM_PROFILE_PATH is not readable")
    })
}

fn strip_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.to_owned()
}

fn request(prompt: &str) -> ChatRequest {
    let mut messages = profile()
        .map(Message::system)
        .into_iter()
        .collect::<Vec<_>>();
    messages.push(Message::user(prompt));
    ChatRequest::new("ignored", messages, 220, 0.4, None)
}

#[tokio::test]
#[ignore = "requires a live server: run with LIVE_SERVER=1"]
async fn real_prompt_fills_the_template_and_uses_the_profile() {
    if dotenvy::var("LIVE_SERVER").is_err() {
        eprintln!("skipping: set LIVE_SERVER=1 to run live tests");
        return;
    }
    let mut stream = client(120).stream(
        request("What do you know about Lüdenscheid? say the town name."),
        tokio_util::sync::CancellationToken::new(),
    );
    let mut content = String::new();
    while let Some(item) = stream.next().await {
        content.push_str(&item.expect("live stream failed"));
    }

    println!("---- raw content ----\n{content}");

    // The world knowledge entry for this town must have been used.
    assert!(
        content.to_lowercase().contains("lüdenscheid")
            || content.to_lowercase().contains("ludenscheid"),
        "answer does not mention the entry: {content}"
    );
}

#[tokio::test]
#[ignore = "requires a live server: run with LIVE_SERVER=1"]
async fn empty_prompt_is_rejected_by_the_real_server() {
    if dotenvy::var("LIVE_SERVER").is_err() {
        return;
    }
    let mut stream = client(60).stream(request(""), tokio_util::sync::CancellationToken::new());
    let mut saw_error = false;
    while let Some(item) = stream.next().await {
        let err = item.expect_err("empty user content should be a 400");
        println!("---- error: {err} ----");
        assert!(format!("{err}").contains("HTTP 400"));
        saw_error = true;
    }
    assert!(saw_error, "expected an HTTP 400 error item");
}
