//! HTTP client for the ASR server (`/v1/audio/transcriptions`, `/v1/models`).

use std::time::Duration;

use reqwest::multipart;
use serde::Deserialize;
use thiserror::Error;

/// Errors a transcription or model-list request can end with.
///
/// `Timeout` and `Connect` and any `Http` with a 5xx status are retryable;
/// 4xx answers and undecodable bodies are not.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AsrError {
    #[error("ASR request timed out")]
    Timeout,
    #[error("ASR server unreachable")]
    Connect,
    #[error("ASR server answered {status}")]
    Http { status: u16 },
    #[error("ASR response could not be decoded")]
    Decode,
}

impl AsrError {
    /// Spec retry rule: retry on timeout, connection error and 5xx;
    /// never on 4xx (and never on an undecodable body).
    pub fn retryable(&self) -> bool {
        match self {
            AsrError::Timeout | AsrError::Connect => true,
            AsrError::Http { status } => (500..=599).contains(status),
            AsrError::Decode => false,
        }
    }
}

/// The `{"text": "..."}` body of a transcription response. A missing or
/// non-string `text` field is a `Decode` error, not silent no-speech.
#[derive(Deserialize)]
struct TranscriptionBody {
    text: String,
}

#[derive(Deserialize)]
struct ModelsBody {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Client for one ASR server and one model.
///
/// `timeout` bounds a single attempt and `backoff` gives the waits between
/// attempts (`backoff[i]` before attempt `i + 2`); the engine injects both,
/// so tests use short values and wait in real time.
pub struct AsrClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    timeout: Duration,
    backoff: Vec<Duration>,
}

impl AsrClient {
    /// `base_url` is like `http://localhost:8097` (a trailing slash is
    /// ignored). Build it inside a tokio runtime.
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        timeout: Duration,
        backoff: impl IntoIterator<Item = Duration>,
    ) -> Self {
        let base_url = base_url.into();
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            model: model.into(),
            timeout,
            backoff: backoff.into_iter().collect(),
        }
    }

    /// Transcribe mono 16 kHz f32 audio.
    ///
    /// `attempts` is the maximum number of requests for this utterance
    /// (the engine passes 3 for finals and 1 for interims).
    /// `Ok(None)` means the server answered with no speech in it, which is
    /// not an error.
    pub async fn transcribe(
        &self,
        pcm: &[f32],
        attempts: usize,
    ) -> Result<Option<String>, AsrError> {
        let wav = crate::wav::encode_wav(pcm);
        let attempts = attempts.max(1);
        for attempt in 0..attempts {
            if attempt > 0 {
                let index = (attempt - 1).min(self.backoff.len().saturating_sub(1));
                if let Some(wait) = self.backoff.get(index) {
                    tokio::time::sleep(*wait).await;
                }
            }
            match self.one_attempt(&wav).await {
                Ok(text) => return Ok(commit_text(&text)),
                // The last attempt never continues, so this loop only ends
                // through a return or a retry `continue`.
                Err(error) if attempt + 1 < attempts && error.retryable() => continue,
                Err(error) => return Err(error),
            }
        }
        unreachable!("attempts is at least 1")
    }

    async fn one_attempt(&self, wav: &[u8]) -> Result<String, AsrError> {
        let file = multipart::Part::bytes(wav.to_vec())
            .file_name("seg.wav")
            .mime_str("audio/wav")
            .expect("audio/wav is a valid mime type");
        let form = multipart::Form::new()
            .part("file", file)
            .text("model", self.model.clone())
            .text("response_format", "json");
        let request = self
            .http
            .post(format!("{}/v1/audio/transcriptions", self.base_url))
            .multipart(form);
        match tokio::time::timeout(self.timeout, request.send()).await {
            Err(_) => Err(AsrError::Timeout),
            Ok(Err(error)) => Err(classify(error)),
            Ok(Ok(response)) => {
                let status = response.status();
                if !status.is_success() {
                    return Err(AsrError::Http {
                        status: status.as_u16(),
                    });
                }
                let body = match tokio::time::timeout(self.timeout, response.bytes()).await {
                    Err(_) => return Err(AsrError::Timeout),
                    Ok(Err(error)) => return Err(classify(error)),
                    Ok(Ok(bytes)) => bytes,
                };
                serde_json::from_slice(&body)
                    .map(|parsed: TranscriptionBody| parsed.text)
                    .map_err(|_| AsrError::Decode)
            }
        }
    }

    /// Model ids from `GET /v1/models`, for the meeting-start health check.
    pub async fn models(&self) -> Result<Vec<String>, AsrError> {
        let request = self.http.get(format!("{}/v1/models", self.base_url));
        let response = match tokio::time::timeout(self.timeout, request.send()).await {
            Err(_) => return Err(AsrError::Timeout),
            Ok(Err(error)) => return Err(classify(error)),
            Ok(Ok(response)) => response,
        };
        let status = response.status();
        if !status.is_success() {
            return Err(AsrError::Http {
                status: status.as_u16(),
            });
        }
        let body = match tokio::time::timeout(self.timeout, response.bytes()).await {
            Err(_) => return Err(AsrError::Timeout),
            Ok(Err(error)) => return Err(classify(error)),
            Ok(Ok(bytes)) => bytes,
        };
        let parsed: ModelsBody = serde_json::from_slice(&body).map_err(|_| AsrError::Decode)?;
        Ok(parsed.data.into_iter().map(|entry| entry.id).collect())
    }
}

fn classify(error: reqwest::Error) -> AsrError {
    if error.is_timeout() {
        AsrError::Timeout
    } else if error.is_decode() {
        AsrError::Decode
    } else {
        // Connection failures and transport errors (reset mid-body, request
        // build failures) all mean the answer never arrived: treat them as
        // Connect so the retry rule applies.
        AsrError::Connect
    }
}

/// A text with no letter or digit after trimming counts as no speech
/// (dropped without an error); otherwise the text commits, trimmed.
fn commit_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.chars().any(char::is_alphanumeric) {
        Some(trimmed.to_owned())
    } else {
        None
    }
}
