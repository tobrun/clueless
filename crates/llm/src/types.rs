//! Request and response types for the OpenAI-compatible chat endpoint.

use serde::{Deserialize, Serialize};

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
}

/// One message in a chat request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
}

/// vLLM-specific template options, sent only when `LLM_ENABLE_THINKING`
/// is set (D-thinking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ChatTemplateKwargs {
    pub enable_thinking: bool,
}

/// The body of `POST /v1/chat/completions`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
    pub max_tokens: u32,
    pub temperature: f64,
    /// `None` omits the field entirely so generic OpenAI servers never see
    /// a vLLM-specific kwarg.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<ChatTemplateKwargs>,
}

impl ChatRequest {
    /// A streaming request; `enable_thinking` comes from `LLM_ENABLE_THINKING`
    /// (D-thinking): `None` omits the field, `Some(false)` sends thinking off.
    pub fn new(
        model: impl Into<String>,
        messages: Vec<Message>,
        max_tokens: u32,
        temperature: f64,
        enable_thinking: Option<bool>,
    ) -> Self {
        Self {
            model: model.into(),
            messages,
            stream: true,
            max_tokens,
            temperature,
            chat_template_kwargs: enable_thinking
                .map(|enable_thinking| ChatTemplateKwargs { enable_thinking }),
        }
    }
}

/// One SSE chunk from a streaming chat response.
///
/// Every optional field defaults so a partial chunk from any server stage parses.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct ChatChunk {
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
}

impl ChatChunk {
    /// The content delta of the first choice, or `None` when this chunk carries none
    /// (role-only chunks, reasoning chunks, finish chunks).
    pub fn content(&self) -> Option<&str> {
        self.choices.first()?.delta.as_ref()?.content.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct ChunkChoice {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub delta: Option<Delta>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// The incremental piece of a choice. `reasoning` and `reasoning_content` are
/// the two field names servers use for thinking text; the client ignores both.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct Delta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_serializes_chat_template_kwargs_when_thinking_is_set() {
        let base = |thinking| {
            ChatRequest::new(
                "mock-model",
                vec![Message::system("rules"), Message::user("hello")],
                220,
                0.4,
                thinking,
            )
        };
        let with = serde_json::to_string(&base(Some(false))).unwrap();
        // The exact bytes a thinking-off request must carry, from probe 2026-10-02.
        assert!(
            with.contains(r#""chat_template_kwargs":{"enable_thinking":false}"#),
            "{with}"
        );
        let without = serde_json::to_string(&base(None)).unwrap();
        assert!(
            !without.contains("chat_template_kwargs"),
            "unset thinking must omit the field: {without}"
        );
        let value: serde_json::Value = serde_json::to_value(base(Some(false))).unwrap();
        assert_eq!(value["stream"], serde_json::json!(true));
        assert_eq!(value["model"], serde_json::json!("mock-model"));
        assert_eq!(value["max_tokens"], serde_json::json!(220));
        assert_eq!(value["temperature"], serde_json::json!(0.4));
        assert_eq!(value["messages"][0]["role"], serde_json::json!("system"));
        assert_eq!(value["messages"][1]["role"], serde_json::json!("user"));
    }

    #[test]
    fn chunk_with_unknown_fields_and_null_token_ids_parses() {
        // A realistic vLLM content delta with extra and null fields from probe 2026-10-02.
        let raw = r#"{
            "id": "cmpl-a1b2c3",
            "object": "chat.completion.chunk",
            "created": 1759400000,
            "model": "mock-model",
            "system_fingerprint": null,
            "usage": null,
            "choices": [{
                "index": 0,
                "logprobs": null,
                "finish_reason": null,
                "token_ids": null,
                "stop_reason": null,
                "delta": {"role": "assistant", "content": "Hel"}
            }]
        }"#;
        let chunk: ChatChunk = serde_json::from_str(raw).unwrap();
        assert_eq!(chunk.content(), Some("Hel"));
        assert_eq!(chunk.choices[0].delta.as_ref().unwrap().reasoning, None);
    }

    #[test]
    fn chunk_without_content_yields_no_delta() {
        // Reasoning arrives in its own field (probe 2026-10-02), not in content.
        for raw in [
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"reasoning":"thinking hard"}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"reasoning_content":"thinking hard"}}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[]}"#,
            r#"{}"#,
        ] {
            let chunk: ChatChunk = serde_json::from_str(raw).unwrap();
            assert_eq!(chunk.content().unwrap_or(""), "", "for {raw}");
        }
    }
}
