//! The health check run once at meeting start: one `models()`
//! call per server under a shared timeout, mapped to one status each.

use std::time::Duration;

use clueless_types::events::{StatusLevel, StatusSource, UiEvent};

use asr::client::AsrClient;
use llm::client::LlmClient;

/// Check both servers and return the status events to emit, ASR first.
/// A server that answers `models()` within `timeout` and offers the
/// configured model is ok; one that names a different model list is a
/// model-missing error naming the model; anything else is offline.
pub async fn check(
    asr: &AsrClient,
    asr_model: &str,
    llm: &LlmClient,
    timeout: Duration,
) -> Vec<UiEvent> {
    let mut events = Vec::new();
    match tokio::time::timeout(timeout, asr.models()).await {
        Ok(Ok(models)) if models.iter().any(|id| id == asr_model) => {
            events.push(reachable(StatusSource::Asr, "ASR", asr_model));
        }
        Ok(Ok(_)) => events.push(missing_model(StatusSource::Asr, "ASR", asr_model)),
        Ok(Err(_)) | Err(_) => events.push(offline(StatusSource::Asr, "ASR")),
    }
    let llm_model = llm.model().to_owned();
    match tokio::time::timeout(timeout, llm.models()).await {
        Ok(Ok(models)) if models.iter().any(|id| id == &llm_model) => {
            events.push(reachable(StatusSource::Llm, "LLM", &llm_model));
        }
        Ok(Ok(_)) => events.push(missing_model(StatusSource::Llm, "LLM", &llm_model)),
        Ok(Err(_)) | Err(_) => events.push(offline(StatusSource::Llm, "LLM")),
    }
    events
}

fn status(source: StatusSource, level: StatusLevel, text: String) -> UiEvent {
    UiEvent::Status {
        source,
        level,
        text,
    }
}

fn reachable(source: StatusSource, label: &str, model: &str) -> UiEvent {
    status(
        source,
        StatusLevel::Info,
        format!("{label} server reachable, model {model}"),
    )
}

fn missing_model(source: StatusSource, label: &str, model: &str) -> UiEvent {
    status(
        source,
        StatusLevel::Error,
        format!("{label} server does not offer model {model}"),
    )
}

fn offline(source: StatusSource, label: &str) -> UiEvent {
    status(source, StatusLevel::Error, format!("{label} offline"))
}
