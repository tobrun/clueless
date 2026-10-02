//! TOML config read once at launch, every key optional with the default from the spec table.

use serde::{Deserialize, Deserializer};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found at {path}; copy config.example.toml to that path and edit it")]
    Missing { path: String },
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid config: {0}")]
    Parse(String),
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    pub host: String,
    pub llm_port: u16,
    pub asr_port: u16,
    pub llm_model: String,
    pub asr_model: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            llm_port: 8000,
            asr_port: 8097,
            llm_model: "your-model-id".into(),
            asr_model: "istupakov/parakeet-tdt-0.6b-v3-onnx".into(),
        }
    }
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LlmConfig {
    pub max_tokens: u32,
    pub temperature: f32,
    pub profile_path: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            max_tokens: 220,
            temperature: 0.4,
            profile_path: None,
        }
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

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub server: ServerConfig,
    pub audio: AudioConfig,
    pub vad: VadConfig,
    pub llm: LlmConfig,
    pub overlay: OverlayConfig,
    pub hotkeys: HotkeysConfig,
}

/// Known key names per table, so an unknown key is reported as `table.key`.
const KNOWN_KEYS: &[(&str, &[&str])] = &[
    (
        "server",
        &["host", "llm_port", "asr_port", "llm_model", "asr_model"],
    ),
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
    ("llm", &["max_tokens", "temperature", "profile_path"]),
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

fn check_unknown_keys(text: &str) -> Result<(), ConfigError> {
    let doc: toml::Value = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let tables = doc.as_table().unwrap();
    for (name, value) in tables {
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
    /// Reads the file at `path`; a missing file is an error naming the path.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
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
        toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))
    }
}
