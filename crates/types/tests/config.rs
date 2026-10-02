//! Config loading: defaults, unknown keys, missing file, backend parsing.

use clueless_types::config::{Config, SystemAudioBackend};
use std::path::Path;

fn load(text: &str, name: &str) -> Result<Config, clueless_types::ConfigError> {
    let dir = std::env::temp_dir().join(format!("clueless-config-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(&path, text).unwrap();
    Config::load(Path::new(&path))
}

#[test]
fn only_server_host_set_means_every_other_key_takes_its_default() {
    let cfg = load("[server]\nhost = \"10.0.0.5\"\n", "host-only").unwrap();
    assert_eq!(cfg.server.host, "10.0.0.5");
    assert_eq!(cfg.server.llm_port, 8000);
    assert_eq!(cfg.server.asr_port, 8097);
    assert_eq!(cfg.server.llm_model, "your-model-id");
    assert_eq!(cfg.server.asr_model, "istupakov/parakeet-tdt-0.6b-v3-onnx");
    assert_eq!(cfg.audio.system_audio_backend, SystemAudioBackend::Sck);
    assert_eq!(cfg.audio.mic_device, None);
    assert_eq!(cfg.audio.watchdog_restarts, 5);
    assert_eq!(cfg.audio.watchdog_silence_secs, 0);
    assert_eq!(cfg.vad.start_threshold, 0.5);
    assert_eq!(cfg.vad.end_threshold, 0.35);
    assert_eq!(cfg.vad.end_silence_frames, 19);
    assert_eq!(cfg.vad.max_segment_ms, 15000);
    assert_eq!(cfg.llm.max_tokens, 220);
    assert_eq!(cfg.llm.temperature, 0.4);
    assert_eq!(cfg.llm.profile_path, None);
    assert!(cfg.overlay.hide_from_capture);
    assert_eq!(cfg.hotkeys.suggest, "cmd+Enter");
}

#[test]
fn unknown_key_names_the_key() {
    let err = load("[vad]\nfoo = 1\n", "unknown-key").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("vad.foo"), "error was: {msg}");
}

#[test]
fn missing_file_error_names_the_path_and_points_at_the_example() {
    let path = std::env::temp_dir().join("clueless-definitely-absent-config.toml");
    let _ = std::fs::remove_file(&path);
    let err = Config::load(Path::new(&path)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("clueless-definitely-absent-config.toml"),
        "error was: {msg}"
    );
    assert!(msg.contains("config.example.toml"), "error was: {msg}");
}

#[test]
fn default_suggest_hotkey_is_a_valid_hotkey_string() {
    let cfg = Config::default();
    assert!(!cfg.hotkeys.suggest.is_empty());
    assert!(cfg.hotkeys.suggest.contains('+'));
}

#[test]
fn device_backend_parses_the_name_after_the_colon() {
    let cfg = load(
        "[audio]\nsystem_audio_backend = \"device:BlackHole 2ch\"\n",
        "dev",
    )
    .unwrap();
    assert_eq!(
        cfg.audio.system_audio_backend,
        SystemAudioBackend::Device("BlackHole 2ch".into())
    );
}

#[test]
fn bad_backend_error_lists_the_three_allowed_forms() {
    let err = load("[audio]\nsystem_audio_backend = \"foo\"\n", "bad-backend").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("sck"), "error was: {msg}");
    assert!(msg.contains("cpal_loopback"), "error was: {msg}");
    assert!(msg.contains("device:<name>"), "error was: {msg}");
}
