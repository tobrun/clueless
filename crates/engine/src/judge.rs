//! The compare judge: asks the chat server to weigh two suggestions against
//! each other (D-judge, D-judge-home).
//!
//! Every judgeable pair of a compare [`Report`] is sent to the configured
//! LLM server twice, one request at a time, with the two answers swapped
//! between the requests. Both orders naming the same side makes it the
//! winner; anything else (a disagreement, an error, an answer the judge
//! cannot read) is a tie or left unjudged, so a model that prefers one
//! position can never produce a winner.

use clueless_types::config::LlmConfig;
use llm::client::LlmClient;
use llm::types::{ChatRequest, Message};
use trace::compare::{JUDGE_CONTEXT_CHARS, Report, Side, Verdict, transcript_before};
use trace::reader::Trace;

use crate::deps::EngineTimings;

/// The judge's instructions. They live here, never in the user message, so
/// the user message carries only the transcript and the two answers.
pub const JUDGE_SYSTEM: &str = "You compare two answers a meeting assistant could have given at one moment of a conversation. \
The user message holds the meeting transcript so far, then the two candidate answers labelled `Answer A:` and `Answer B:`. \
Decide which answer is more helpful, specific and correct for that moment of the conversation; a tie is the honest answer when they are equally good. \
Answer with exactly one JSON object and nothing else, in the form {\"winner\": \"A\", \"reason\": \"one short sentence\"}, \
where winner is \"A\", \"B\" or \"tie\".";

/// Judge requests run at temperature 0 (D-judge).
const JUDGE_TEMPERATURE: f64 = 0.0;

/// A judge answer is one small JSON object; 200 tokens is generous.
const JUDGE_MAX_TOKENS: u32 = 200;

/// What a judge request sends, fixed except for the two settings that must
/// match the live requests (thinking) and the usage trace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JudgeConfig {
    pub temperature: f64,
    pub max_tokens: u32,
    pub enable_thinking: Option<bool>,
    pub include_usage: bool,
}

impl JudgeConfig {
    /// Fixed judge parameters plus the thinking and usage settings of the
    /// live config, so the judge runs against the server as configured.
    pub fn from_llm_config(config: &LlmConfig) -> Self {
        Self {
            temperature: JUDGE_TEMPERATURE,
            max_tokens: JUDGE_MAX_TOKENS,
            enable_thinking: config.enable_thinking,
            include_usage: config.include_usage,
        }
    }
}

/// Which side of one request the judge named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Winner {
    A,
    B,
    Tie,
}

/// One parsed judge answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub winner: Winner,
    pub reason: String,
}

/// Ask the judge once: `first` shown as answer A, `second` as answer B,
/// under the given transcript context. The error is the text a
/// [`Verdict::NotJudged`] carries.
pub async fn ask(
    llm: &LlmClient,
    cfg: &JudgeConfig,
    context: &str,
    first: &str,
    second: &str,
) -> Result<Choice, String> {
    let request = ChatRequest::new(
        llm.model(),
        vec![
            Message::system(JUDGE_SYSTEM),
            Message::user(user_prompt(context, first, second)),
        ],
        cfg.max_tokens,
        cfg.temperature,
        cfg.enable_thinking,
        cfg.include_usage,
    );
    let completion = llm
        .complete(request)
        .await
        .map_err(|error| error.detail())?;
    parse_choice(&completion.text)
}

/// Judge one pair: the same answers once in each order (D-judge). Both
/// orders naming the same side wins with the first answer's reason; both
/// tying is a tie; orders disagreeing is a tie marked `order-dependent`;
/// any failed or unreadable answer leaves the pair unjudged.
pub async fn judge_pair(
    llm: &LlmClient,
    cfg: &JudgeConfig,
    context: &str,
    a_text: &str,
    b_text: &str,
) -> Verdict {
    let first = match ask(llm, cfg, context, a_text, b_text).await {
        Ok(choice) => choice,
        Err(reason) => return Verdict::NotJudged(reason),
    };
    let second = match ask(llm, cfg, context, b_text, a_text).await {
        Ok(choice) => choice,
        Err(reason) => return Verdict::NotJudged(reason),
    };
    // In the swapped request the labels are exchanged.
    let second = match second.winner {
        Winner::A => Winner::B,
        Winner::B => Winner::A,
        Winner::Tie => Winner::Tie,
    };
    if first.winner != second {
        return Verdict::Tie("order-dependent".to_string());
    }
    match first.winner {
        Winner::A => Verdict::Winner(Side::A, first.reason),
        Winner::B => Verdict::Winner(Side::B, first.reason),
        Winner::Tie => Verdict::Tie(first.reason),
    }
}

/// Judge every judgeable pair of `report` against the baseline's context,
/// in order, one request at a time (C-server-read-only).
pub async fn judge_report(
    llm: &LlmClient,
    cfg: &JudgeConfig,
    baseline: &Trace,
    report: &mut Report,
) {
    for pair in &mut report.pairs {
        if !pair.judgeable {
            continue;
        }
        let context = transcript_before(baseline, pair.a.meeting_ms, JUDGE_CONTEXT_CHARS);
        pair.verdict =
            Some(judge_pair(llm, cfg, &context, &pair.a.shown_text, &pair.b.shown_text).await);
    }
}

/// Judge a report with the server a live meeting would use: the client is
/// built from the LLM config with the production connect and stall timings,
/// so the binary needs no `app -> llm` edge (D-judge-home).
pub async fn judge_report_from_config(llm_cfg: &LlmConfig, baseline: &Trace, report: &mut Report) {
    let cfg = JudgeConfig::from_llm_config(llm_cfg);
    let timings = EngineTimings::production();
    let llm = LlmClient::new(
        &llm_cfg.base_url,
        &llm_cfg.model,
        llm_cfg.api_key.clone(),
        timings.llm_connect,
        timings.llm_stall,
    );
    judge_report(&llm, &cfg, baseline, report).await;
}

/// The user message: transcript and answers only, no instructions; the
/// labels let the caller see which answer stood under which letter.
fn user_prompt(context: &str, first: &str, second: &str) -> String {
    format!("Transcript so far:\n{context}\nAnswer A:\n{first}\n\nAnswer B:\n{second}\n")
}

/// Read the first JSON object of an answer; prose around it is tolerated.
fn parse_choice(text: &str) -> Result<Choice, String> {
    let object = first_object(text).ok_or_else(|| unreadable(text))?;
    let value: serde_json::Value = serde_json::from_str(object).map_err(|_| unreadable(text))?;
    let winner = match value.get("winner").and_then(serde_json::Value::as_str) {
        Some(winner) => match winner.trim().to_ascii_lowercase().as_str() {
            "a" => Winner::A,
            "b" => Winner::B,
            "tie" => Winner::Tie,
            _ => return Err(unreadable(text)),
        },
        None => return Err(unreadable(text)),
    };
    let reason = value
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(Choice { winner, reason })
}

fn unreadable(text: &str) -> String {
    format!("unreadable answer: {text}")
}

/// The text from the first `{` to its matching `}` (string literals and
/// their escapes do not count as braces); `None` when no closed object.
fn first_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in text.as_bytes().iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=offset]);
                }
            }
            _ => {}
        }
    }
    None
}
