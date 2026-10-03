//! Settings are read once at launch from two places. The LLM and ASR
//! endpoints come from the environment (normally a `.env` file, see
//! `.env.example`); every required variable without a compiled-in default
//! is named there. App behavior comes from an optional TOML file whose
//! every key is optional.

use serde::{Deserialize, Deserializer};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "config file not found at {path}; config.toml is optional, so drop --config or copy config.example.toml to that path and edit it"
    )]
    Missing { path: String },
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid config: {0}")]
    Parse(String),
    #[error("missing or invalid environment settings, copy .env.example to .env and fill them in:{}", items.iter().map(|i| format!("\n  {i}")).collect::<String>())]
    Env { items: Vec<String> },
}

/// Which backend captures the other side's audio.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SystemAudioBackend {
    /// ScreenCaptureKit (default)
    Sck,
    /// cpal loopback tap on the default output device
    CpalLoopback,
    /// a named input device, written `device:<name>`
    Device(String),
}

impl SystemAudioBackend {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "sck" => Ok(Self::Sck),
            "cpal_loopback" => Ok(Self::CpalLoopback),
            other => match other.strip_prefix("device:") {
                Some(name) if !name.is_empty() => Ok(Self::Device(name.to_string())),
                _ => Err(format!(
                    "audio.system_audio_backend must be \"sck\", \"cpal_loopback\" \
                     or \"device:<name>\", got {other:?}"
                )),
            },
        }
    }
}

fn de_backend<'de, D>(d: D) -> Result<SystemAudioBackend, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    SystemAudioBackend::parse(&s).map_err(serde::de::Error::custom)
}

fn de_default_backend() -> SystemAudioBackend {
    SystemAudioBackend::Sck
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AudioConfig {
    #[serde(deserialize_with = "de_backend", default = "de_default_backend")]
    pub system_audio_backend: SystemAudioBackend,
    pub mic_device: Option<String>,
    pub watchdog_restarts: u32,
    pub watchdog_silence_secs: u64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            system_audio_backend: SystemAudioBackend::Sck,
            mic_device: None,
            watchdog_restarts: 5,
            watchdog_silence_secs: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VadConfig {
    pub start_threshold: f32,
    pub end_threshold: f32,
    pub end_silence_frames: usize,
    pub max_segment_ms: u64,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            start_threshold: 0.5,
            end_threshold: 0.35,
            end_silence_frames: 19,
            max_segment_ms: 15000,
        }
    }
}

/// A variable lookup, normally the process environment overlaid on a parsed
/// `.env` file. Every setting reads through it, so tests never touch the
/// real environment.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The trimmed value of `name`, or `None` when unset or blank.
fn lookup_value(lookup: Lookup, name: &str) -> Option<String> {
    lookup(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn required_value(lookup: Lookup, name: &str, problems: &mut Vec<String>) -> Option<String> {
    match lookup_value(lookup, name) {
        Some(value) => Some(value),
        None => {
            problems.push(format!("{name} is not set"));
            None
        }
    }
}

/// A base URL must be absolute http(s); trailing slashes are dropped so
/// appending `/v1/...` cannot double them.
fn check_base_url(name: &str, value: String, problems: &mut Vec<String>) -> Option<String> {
    let lowered = value.to_ascii_lowercase();
    if lowered.starts_with("http://") || lowered.starts_with("https://") {
        Some(value.trim_end_matches('/').to_string())
    } else {
        problems.push(format!("{name} = \"{value}\" is not an http(s) URL"));
        None
    }
}

fn parse_number<T: std::str::FromStr>(
    name: &str,
    value: &str,
    problems: &mut Vec<String>,
) -> Option<T>
where
    T::Err: std::fmt::Display,
{
    match value.parse() {
        Ok(parsed) => Some(parsed),
        Err(_) => {
            problems.push(format!("{name} = \"{value}\" is not a valid number"));
            None
        }
    }
}

/// `~` and `~/` at the front expand to the home directory.
fn expand_tilde(value: &str) -> String {
    if (value == "~" || value.starts_with("~/"))
        && let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return format!("{home}{}", &value[1..]);
    }
    value.to_string()
}

// Test/dev scaffolding only: an app without a real LLM_* environment fails at
// Config::load, never runs on these defaults.
impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            api_key: None,
            max_tokens: 220,
            temperature: 0.4,
            profile_path: None,
            enable_thinking: None,
        }
    }
}

// The chat endpoint the app talks to, entirely from `LLM_*` variables.
// There is no compiled-in default for the endpoint or model: the app
// cannot guess them.
#[derive(Clone, PartialEq)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub profile_path: Option<String>,
    /// `None` omits `chat_template_kwargs` from requests entirely;
    /// `Some(false)` sends `enable_thinking: false` (vLLM/Qwen).
    pub enable_thinking: Option<bool>,
}

impl LlmConfig {
    /// Collects every problem into one `Err(Vec)` so a half-filled `.env`
    /// gets one actionable message, not a one-variable-at-a-time drill.
    pub fn from_lookup(lookup: Lookup) -> Result<Self, Vec<String>> {
        let mut problems = Vec::new();
        let base_url = required_value(lookup, "LLM_BASE_URL", &mut problems)
            .and_then(|v| check_base_url("LLM_BASE_URL", v, &mut problems));
        let model = required_value(lookup, "LLM_MODEL", &mut problems);
        let max_tokens = match lookup_value(lookup, "LLM_MAX_TOKENS") {
            Some(raw) => parse_number("LLM_MAX_TOKENS", &raw, &mut problems),
            None => Some(220),
        };
        let temperature = match lookup_value(lookup, "LLM_TEMPERATURE") {
            Some(raw) => parse_number("LLM_TEMPERATURE", &raw, &mut problems),
            None => Some(0.4),
        };
        let enable_thinking = match lookup_value(lookup, "LLM_ENABLE_THINKING") {
            Some(raw) => match raw.as_str() {
                "true" => Some(Some(true)),
                "false" => Some(Some(false)),
                _ => {
                    problems.push(format!(
                        "LLM_ENABLE_THINKING = \"{raw}\" is not \"true\" or \"false\""
                    ));
                    None
                }
            },
            None => Some(None),
        };
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(Self {
            base_url: base_url.expect("checked above"),
            model: model.expect("checked above"),
            api_key: lookup_value(lookup, "LLM_API_KEY"),
            max_tokens: max_tokens.expect("checked above"),
            temperature: temperature.expect("checked above"),
            profile_path: lookup_value(lookup, "LLM_PROFILE_PATH").map(|p| expand_tilde(&p)),
            enable_thinking: enable_thinking.expect("checked above"),
        })
    }
}

// The api key must never reach a log line through a `{:?}`.
impl std::fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_deref().map(|_| "[set]"))
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("profile_path", &self.profile_path)
            .field("enable_thinking", &self.enable_thinking)
            .finish()
    }
}

// Test/dev scaffolding only, like `LlmConfig::default`.
// The transcription endpoint the app talks to, entirely from `ASR_*`
// variables.
#[derive(Clone, Default, PartialEq)]
pub struct AsrConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// `None` lets the server detect the language.
    pub language: Option<String>,
}

impl AsrConfig {
    pub fn from_lookup(lookup: Lookup) -> Result<Self, Vec<String>> {
        let mut problems = Vec::new();
        let base_url = required_value(lookup, "ASR_BASE_URL", &mut problems)
            .and_then(|v| check_base_url("ASR_BASE_URL", v, &mut problems));
        let model = required_value(lookup, "ASR_MODEL", &mut problems);
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(Self {
            base_url: base_url.expect("checked above"),
            model: model.expect("checked above"),
            api_key: lookup_value(lookup, "ASR_API_KEY"),
            language: lookup_value(lookup, "ASR_LANGUAGE"),
        })
    }
}

impl std::fmt::Debug for AsrConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsrConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_deref().map(|_| "[set]"))
            .field("language", &self.language)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OverlayConfig {
    pub hide_from_capture: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            hide_from_capture: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HotkeysConfig {
    pub suggest: String,
    pub clear: String,
    pub move_left: String,
    pub move_right: String,
    pub move_up: String,
    pub move_down: String,
    pub toggle_meeting: String,
    pub toggle_overlay: String,
    pub toggle_click_through: String,
}

impl Default for HotkeysConfig {
    fn default() -> Self {
        Self {
            suggest: "cmd+Enter".into(),
            clear: "cmd+shift+KeyX".into(),
            move_left: "cmd+shift+ArrowLeft".into(),
            move_right: "cmd+shift+ArrowRight".into(),
            move_up: "cmd+shift+ArrowUp".into(),
            move_down: "cmd+shift+ArrowDown".into(),
            toggle_meeting: "cmd+shift+KeyR".into(),
            toggle_overlay: "cmd+Backslash".into(),
            toggle_click_through: "cmd+shift+KeyM".into(),
        }
    }
}

/// The deserialize target for the TOML file: the app-behavior tables only.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct TomlTuning {
    audio: AudioConfig,
    vad: VadConfig,
    overlay: OverlayConfig,
    hotkeys: HotkeysConfig,
}

/// Everything the app needs: the app-behavior tables from the (optional)
/// TOML file plus the two server configs built from the environment.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    pub audio: AudioConfig,
    pub vad: VadConfig,
    pub overlay: OverlayConfig,
    pub hotkeys: HotkeysConfig,
    pub llm: LlmConfig,
    pub asr: AsrConfig,
}

/// Known key names per table, so an unknown key is reported as `table.key`.
const KNOWN_KEYS: &[(&str, &[&str])] = &[
    (
        "audio",
        &[
            "system_audio_backend",
            "mic_device",
            "watchdog_restarts",
            "watchdog_silence_secs",
        ],
    ),
    (
        "vad",
        &[
            "start_threshold",
            "end_threshold",
            "end_silence_frames",
            "max_segment_ms",
        ],
    ),
    ("overlay", &["hide_from_capture"]),
    (
        "hotkeys",
        &[
            "suggest",
            "clear",
            "move_left",
            "move_right",
            "move_up",
            "move_down",
            "toggle_meeting",
            "toggle_overlay",
            "toggle_click_through",
        ],
    ),
];

/// Tables that moved to environment variables, with the variables that
/// replaced them, so an old config.toml says where each key went.
const MOVED_TABLES: &[(&str, &str)] = &[
    (
        "server",
        "[server] moved to the environment: use LLM_BASE_URL, LLM_MODEL, ASR_BASE_URL \
         and ASR_MODEL (see .env.example)",
    ),
    (
        "llm",
        "[llm] moved to the environment: use LLM_MAX_TOKENS, LLM_TEMPERATURE, \
         LLM_PROFILE_PATH and LLM_ENABLE_THINKING (see .env.example)",
    ),
];

fn check_unknown_keys(text: &str) -> Result<(), ConfigError> {
    let doc: toml::Value = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let tables = doc.as_table().unwrap();
    for (name, value) in tables {
        if let Some((_, hint)) = MOVED_TABLES.iter().find(|(n, _)| n == name) {
            return Err(ConfigError::Parse(hint.to_string()));
        }
        match KNOWN_KEYS.iter().find(|(n, _)| n == name) {
            None => return Err(ConfigError::Parse(format!("unknown key {name}"))),
            Some((_, keys)) => {
                if let Some(t) = value.as_table() {
                    for key in t.keys() {
                        if !keys.contains(&key.as_str()) {
                            return Err(ConfigError::Parse(format!("unknown key {name}.{key}")));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

impl Config {
    /// Reads the app-behavior TOML (when `path` is `Some`) and builds the
    /// LLM and ASR configs from `lookup`. Every environment problem is
    /// collected into one [`ConfigError::Env`].
    pub fn load(path: Option<&Path>, lookup: Lookup) -> Result<Self, ConfigError> {
        let tuning = match path {
            None => TomlTuning::default(),
            Some(path) => {
                let text = match std::fs::read_to_string(path) {
                    Ok(t) => t,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Err(ConfigError::Missing {
                            path: path.display().to_string(),
                        });
                    }
                    Err(source) => {
                        return Err(ConfigError::Io {
                            path: path.display().to_string(),
                            source,
                        });
                    }
                };
                check_unknown_keys(&text)?;
                toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?
            }
        };
        match (
            LlmConfig::from_lookup(lookup),
            AsrConfig::from_lookup(lookup),
        ) {
            (Ok(llm), Ok(asr)) => Ok(Self {
                audio: tuning.audio,
                vad: tuning.vad,
                overlay: tuning.overlay,
                hotkeys: tuning.hotkeys,
                llm,
                asr,
            }),
            (llm, asr) => {
                let mut items = llm.err().unwrap_or_default();
                items.extend(asr.err().unwrap_or_default());
                Err(ConfigError::Env { items })
            }
        }
    }
}
