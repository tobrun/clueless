//! Config loading: optional TOML defaults, unknown and moved keys,
//! environment building, and the `.env` variable rules.

use clueless_types::config::{Config, SystemAudioBackend};
use clueless_types::profile::AssistProfile;
use std::collections::HashMap;
use std::path::Path;

/// The variables a complete setup needs, minus anything a test overrides.
fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = [
        ("LLM_BASE_URL", "http://llm.test:8000"),
        ("LLM_MODEL", "test-llm"),
        ("ASR_BASE_URL", "http://asr.test:8097"),
        ("ASR_MODEL", "test-asr"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (key, value) in pairs {
        if value.is_empty() {
            map.remove(*key);
        } else {
            map.insert((*key).to_string(), (*value).to_string());
        }
    }
    map
}

fn lookup(map: &HashMap<String, String>) -> impl Fn(&str) -> Option<String> + '_ {
    move |name| map.get(name).cloned()
}

fn load(
    text: &str,
    name: &str,
    pairs: &[(&str, &str)],
) -> Result<Config, clueless_types::ConfigError> {
    let dir = std::env::temp_dir().join(format!("clueless-config-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(&path, text).unwrap();
    let map = env(pairs);
    Config::load(Some(Path::new(&path)), &lookup(&map))
}

#[test]
fn every_toml_key_may_be_absent_and_every_env_default_applies() {
    let cfg = load("", "empty", &[]).unwrap();
    assert_eq!(cfg.llm.base_url, "http://llm.test:8000");
    assert_eq!(cfg.llm.model, "test-llm");
    assert_eq!(cfg.llm.max_tokens, 220);
    assert_eq!(cfg.llm.temperature, 0.4);
    assert_eq!(cfg.llm.notes_path, None);
    assert_eq!(cfg.llm.api_key, None);
    assert_eq!(cfg.llm.enable_thinking, None);
    assert_eq!(cfg.asr.base_url, "http://asr.test:8097");
    assert_eq!(cfg.asr.model, "test-asr");
    assert_eq!(cfg.asr.api_key, None);
    assert_eq!(cfg.asr.language, None);
    assert_eq!(cfg.audio.system_audio_backend, SystemAudioBackend::Sck);
    assert_eq!(cfg.audio.mic_device, None);
    assert_eq!(cfg.audio.watchdog_restarts, 5);
    assert_eq!(cfg.audio.watchdog_silence_secs, 0);
    assert_eq!(cfg.vad.start_threshold, 0.5);
    assert_eq!(cfg.vad.end_threshold, 0.35);
    assert_eq!(cfg.vad.end_silence_frames, 19);
    assert_eq!(cfg.vad.max_segment_ms, 15000);
    assert!(cfg.overlay.hide_from_capture);
    assert_eq!(cfg.hotkeys.suggest, "cmd+Enter");
}

#[test]
fn a_missing_file_only_means_toml_defaults() {
    let map = env(&[]);
    let missing = std::env::temp_dir().join("clueless-absent-optional-config.toml");
    let _ = std::fs::remove_file(&missing);
    let cfg = Config::load(Some(&missing), &lookup(&map)).unwrap_err();
    // An explicit path that does not exist is still an error naming it.
    assert!(
        cfg.to_string()
            .contains("clueless-absent-optional-config.toml")
    );
    let cfg = Config::load(None, &lookup(&map)).unwrap();
    assert_eq!(cfg.vad.max_segment_ms, 15000);
}

#[test]
fn a_missing_required_variable_is_listed_with_the_env_hint() {
    let err = load(
        "",
        "missing-vars",
        &[("LLM_BASE_URL", ""), ("ASR_MODEL", "")],
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("LLM_BASE_URL"), "error was: {msg}");
    assert!(msg.contains("ASR_MODEL"), "error was: {msg}");
    assert!(msg.contains(".env.example"), "error was: {msg}");
    assert!(
        !msg.contains("ASR_BASE_URL"),
        "only the missing ones: {msg}"
    );
}

#[test]
fn invalid_env_values_are_all_reported_at_once() {
    let err = load(
        "",
        "bad-values",
        &[
            ("LLM_BASE_URL", "llm.test:8000"),
            ("LLM_MAX_TOKENS", "many"),
            ("LLM_ENABLE_THINKING", "maybe"),
        ],
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("LLM_BASE_URL"), "error was: {msg}");
    assert!(msg.contains("LLM_MAX_TOKENS"), "error was: {msg}");
    assert!(msg.contains("LLM_ENABLE_THINKING"), "error was: {msg}");
}

#[test]
fn base_urls_keep_their_scheme_and_lose_trailing_slashes() {
    let cfg = load(
        "",
        "urls",
        &[("LLM_BASE_URL", "https://api.example.com/v1//")],
    )
    .unwrap();
    assert_eq!(cfg.llm.base_url, "https://api.example.com/v1");
}

#[test]
fn notes_path_expands_a_leading_tilde() {
    let cfg = load("", "tilde", &[("LLM_NOTES_PATH", "~/notes.txt")]).unwrap();
    let path = cfg.llm.notes_path.expect("set");
    assert!(!path.starts_with('~'), "tilde was not expanded: {path}");
    assert!(path.ends_with("/notes.txt"), "wrong expansion: {path}");
}

#[test]
fn the_old_profile_path_variable_is_an_error_naming_the_new_one() {
    let err = load("", "old-profile", &[("LLM_PROFILE_PATH", "/tmp/p.txt")]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("LLM_PROFILE_PATH was renamed to LLM_NOTES_PATH"),
        "error was: {msg}"
    );
}

#[test]
fn assist_defaults_to_manual_and_cycle_profile_to_ctrl_alt_p() {
    let cfg = load("", "assist-default", &[]).unwrap();
    assert_eq!(cfg.assist.start_profile, AssistProfile::Manual);
    assert_eq!(cfg.hotkeys.cycle_profile, "ctrl+alt+KeyP");
}

#[test]
fn start_profile_can_be_set_in_toml() {
    let cfg = load(
        "[assist]\nstart_profile = \"brainstorm\"\n",
        "assist-brainstorm",
        &[],
    )
    .unwrap();
    assert_eq!(cfg.assist.start_profile, AssistProfile::Brainstorm);
}

#[test]
fn an_unknown_start_profile_names_the_three_valid_ones() {
    let err = load("[assist]\nstart_profile = \"coach\"\n", "assist-coach", &[]).unwrap_err();
    let msg = err.to_string();
    for key in ["manual", "interview", "brainstorm"] {
        assert!(msg.contains(key), "error was: {msg}");
    }
}

#[test]
fn a_misspelled_assist_key_is_reported_as_unknown() {
    let err = load("[assist]\nstart = \"x\"\n", "assist-unknown", &[]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unknown key assist.start"), "error was: {msg}");
}

#[test]
fn enable_thinking_is_a_tristate() {
    let cfg = load("", "think-off", &[("LLM_ENABLE_THINKING", "false")]).unwrap();
    assert_eq!(cfg.llm.enable_thinking, Some(false));
    let cfg = load("", "think-on", &[("LLM_ENABLE_THINKING", "true")]).unwrap();
    assert_eq!(cfg.llm.enable_thinking, Some(true));
}

#[test]
fn the_debug_output_never_contains_the_api_keys() {
    let cfg = load(
        "",
        "keys",
        &[("LLM_API_KEY", "sk-secret"), ("ASR_API_KEY", "asr-secret")],
    )
    .unwrap();
    let text = format!("{cfg:?}");
    assert!(!text.contains("sk-secret"), "{text}");
    assert!(!text.contains("asr-secret"), "{text}");
    assert!(text.contains("[set]"), "{text}");
}

#[test]
fn the_llm_debug_output_masks_only_the_key() {
    let cfg = load("", "llm-debug", &[("LLM_API_KEY", "sk-secret")]).unwrap();
    let text = format!("{:?}", cfg.llm);
    assert!(text.contains("LlmConfig"), "{text}");
    assert!(text.contains("test-llm"), "{text}");
    assert!(text.contains("api_key: Some(\"[set]\")"), "{text}");
    assert!(!text.contains("sk-secret"), "{text}");
}

#[test]
fn unknown_key_names_the_key() {
    let err = load("[vad]\nfoo = 1\n", "unknown-key", &[]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("vad.foo"), "error was: {msg}");
}

#[test]
fn the_moved_server_and_llm_tables_point_at_their_variables() {
    let err = load("[server]\nhost = \"x\"\n", "moved-server", &[]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("LLM_BASE_URL"), "error was: {msg}");
    assert!(msg.contains(".env.example"), "error was: {msg}");
    let err = load("[llm]\nmax_tokens = 1\n", "moved-llm", &[]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("LLM_MAX_TOKENS"), "error was: {msg}");
}

#[test]
fn default_suggest_hotkey_is_a_valid_hotkey_string() {
    let cfg = clueless_types::config::HotkeysConfig::default();
    assert!(!cfg.suggest.is_empty());
    assert!(cfg.suggest.contains('+'));
}

#[test]
fn toggle_mode_defaults_to_cmd_shift_backslash() {
    assert_eq!(
        clueless_types::config::HotkeysConfig::default().toggle_mode,
        "cmd+shift+Backslash"
    );
    let cfg = load("", "toggle-mode-default", &[]).unwrap();
    assert_eq!(cfg.hotkeys.toggle_mode, "cmd+shift+Backslash");
}

#[test]
fn toggle_mode_can_be_set_in_toml() {
    let cfg = load(
        "[hotkeys]\ntoggle_mode = \"ctrl+KeyM\"\n",
        "toggle-mode-set",
        &[],
    )
    .unwrap();
    assert_eq!(cfg.hotkeys.toggle_mode, "ctrl+KeyM");
}

#[test]
fn a_misspelled_toggle_mode_key_is_reported_as_unknown() {
    let err = load(
        "[hotkeys]\ntoggle_modes = \"x\"\n",
        "toggle-mode-unknown",
        &[],
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("hotkeys.toggle_modes"), "error was: {msg}");
}

#[test]
fn device_backend_parses_the_name_after_the_colon() {
    let cfg = load(
        "[audio]\nsystem_audio_backend = \"device:BlackHole 2ch\"\n",
        "dev",
        &[],
    )
    .unwrap();
    assert_eq!(
        cfg.audio.system_audio_backend,
        SystemAudioBackend::Device("BlackHole 2ch".into())
    );
}

#[test]
fn bad_backend_error_lists_the_three_allowed_forms() {
    let err = load(
        "[audio]\nsystem_audio_backend = \"foo\"\n",
        "bad-backend",
        &[],
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("sck"), "error was: {msg}");
    assert!(msg.contains("cpal_loopback"), "error was: {msg}");
    assert!(msg.contains("device:<name>"), "error was: {msg}");
}

#[test]
fn a_config_without_the_trace_table_records_text_but_not_audio() {
    let cfg = load("", "no-trace", &[]).unwrap();
    assert!(cfg.trace.enabled, "recording is on by default");
    assert!(!cfg.trace.audio, "audio is opt-in");
}

#[test]
fn the_trace_table_switches_audio_on() {
    let cfg = load("[trace]\naudio = true\n", "trace-audio", &[]).unwrap();
    assert!(cfg.trace.enabled);
    assert!(cfg.trace.audio);
}

#[test]
fn the_trace_table_can_switch_recording_off() {
    let cfg = load("[trace]\nenabled = false\n", "trace-off", &[]).unwrap();
    assert!(!cfg.trace.enabled);
}

#[test]
fn an_unknown_key_in_the_trace_table_is_named() {
    let err = load("[trace]\naudoi = true\n", "trace-typo", &[]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unknown key trace.audoi"), "error was: {msg}");
}

#[test]
fn trace_enabled_must_be_a_boolean_naming_the_key() {
    let err = load("[trace]\nenabled = \"yes\"\n", "trace-str", &[]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("trace.enabled"), "error was: {msg}");
}

#[test]
fn include_usage_defaults_to_true() {
    let map = env(&[]);
    let cfg = Config::load(None, &lookup(&map)).unwrap();
    assert!(cfg.llm.include_usage);
}

#[test]
fn include_usage_reads_the_variable() {
    let map = env(&[("LLM_INCLUDE_USAGE", "false")]);
    let cfg = Config::load(None, &lookup(&map)).unwrap();
    assert!(!cfg.llm.include_usage);
}

#[test]
fn include_usage_refuses_anything_but_true_or_false_naming_the_variable() {
    let map = env(&[("LLM_INCLUDE_USAGE", "maybe")]);
    let err = Config::load(None, &lookup(&map)).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("LLM_INCLUDE_USAGE"), "error was: {msg}");
}
