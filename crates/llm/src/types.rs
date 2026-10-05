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
/// is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ChatTemplateKwargs {
    pub enable_thinking: bool,
}

/// OpenAI-compatible stream options, sent only when the caller asked for
/// token usage (`LLM_INCLUDE_USAGE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// Token counts from the trailing usage chunk. Each count is optional so a
/// partial usage object still yields the counts it carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
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
    /// `None` omits the field entirely; `Some` asks the server for a final
    /// usage chunk (`stream_options.include_usage`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
}

impl ChatRequest {
    /// A streaming request; `enable_thinking` comes from `LLM_ENABLE_THINKING`
    /// `None` omits the field, `Some(false)` sends thinking off.
    /// `include_usage` comes from `LLM_INCLUDE_USAGE` and asks the server to
    /// end the stream with a token-usage chunk.
    pub fn new(
        model: impl Into<String>,
        messages: Vec<Message>,
        max_tokens: u32,
        temperature: f64,
        enable_thinking: Option<bool>,
        include_usage: bool,
    ) -> Self {
        Self {
            model: model.into(),
            messages,
            stream: true,
            max_tokens,
            temperature,
            chat_template_kwargs: enable_thinking
                .map(|enable_thinking| ChatTemplateKwargs { enable_thinking }),
            stream_options: include_usage.then_some(StreamOptions {
                include_usage: true,
            }),
        }
    }
}

/// One piece the client yields from a streamed chat response: answer text,
/// reasoning text, or the terminal summary carried at `[DONE]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamPart {
    Content(String),
    Reasoning(String),
    /// The last `finish_reason` and the last readable `usage` seen in the
    /// stream; both are `None` when the server never sent them.
    Finish {
        reason: Option<String>,
        usage: Option<Usage>,
    },
}

/// A whole non-streaming answer collected by `LlmClient::complete`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Completion {
    pub text: String,
    pub reasoning: String,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

/// One SSE chunk from a streaming chat response.
///
/// Every optional field defaults so a partial chunk from any server stage parses.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct ChatChunk {
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    /// Kept as raw JSON so a usage object this build cannot read costs
    /// nothing instead of failing the chunk.
    #[serde(default)]
    pub usage: Option<serde_json::Value>,
}

impl ChatChunk {
    /// The content delta of the first choice, or `None` when this chunk carries none
    /// (role-only chunks, reasoning chunks, finish chunks).
    pub fn content(&self) -> Option<&str> {
        self.choices.first()?.delta.as_ref()?.content.as_deref()
    }

    /// The reasoning delta of the first choice, under either of the two field
    /// names servers use (`reasoning`, `reasoning_content`).
    pub fn reasoning(&self) -> Option<&str> {
        let delta = self.choices.first()?.delta.as_ref()?;
        delta
            .reasoning
            .as_deref()
            .or(delta.reasoning_content.as_deref())
    }

    /// The finish reason of the first choice that carries one.
    pub fn finish_reason(&self) -> Option<&str> {
        self.choices
            .iter()
            .find_map(|choice| choice.finish_reason.as_deref())
    }

    /// The token usage of this chunk; an absent or unreadable usage object
    /// yields `None` and never an error.
    pub fn usage(&self) -> Option<Usage> {
        self.usage
            .as_ref()
            .and_then(|raw| serde_json::from_value(raw.clone()).ok())
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
                false,
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
    fn include_usage_adds_stream_options_and_false_omits_the_key() {
        let build = |include_usage| {
            ChatRequest::new(
                "mock-model",
                vec![Message::user("hi")],
                220,
                0.4,
                None,
                include_usage,
            )
        };
        let with = serde_json::to_string(&build(true)).unwrap();
        assert!(
            with.contains(r#""stream_options":{"include_usage":true}"#),
            "{with}"
        );
        let without = serde_json::to_string(&build(false)).unwrap();
        assert!(
            !without.contains("stream_options"),
            "include_usage=false must omit the field: {without}"
        );
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

    #[test]
    fn a_usage_object_is_read_leniently() {
        let full: ChatChunk = serde_json::from_str(
            r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}}"#,
        )
        .unwrap();
        assert_eq!(
            full.usage(),
            Some(Usage {
                prompt_tokens: Some(12),
                completion_tokens: Some(8),
                total_tokens: Some(20),
            })
        );
        // A partial object keeps the counts it carries.
        let partial: ChatChunk = serde_json::from_str(
            r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":8}}"#,
        )
        .unwrap();
        assert_eq!(partial.usage().unwrap().total_tokens, None);
        // A usage the client cannot read is no usage, and never an error.
        for raw in [
            r#"{"choices":[]}"#,
            r#"{"choices":[],"usage":null}"#,
            r#"{"choices":[],"usage":"n/a"}"#,
        ] {
            let chunk: ChatChunk = serde_json::from_str(raw).unwrap();
            assert_eq!(chunk.usage(), None, "for {raw}");
        }
    }
}
