//! Tests for the real `clueless` binary: CLI contract, replay against
//! local mock servers, and the e2e live replay suite (ignored by default;
//! `LIVE_SERVER=1` opts in, without it each test returns early).
//!
//! The child-process runners and the mock servers live in `common`; every
//! run through them gets a temporary `HOME` and a `--data-dir` inside it,
//! so no test writes into the real `~/.clueless` (D-test-isolation).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use trace::compare::{word_distance, words};

mod common;

use common::{
    TempDir, closed_port, fixture, mock_home, run, run_with_log, spawn_llm_mock, stdout_lines,
    write_mock_env,
};

const BIN: &str = env!("CARGO_BIN_EXE_clueless");

// ---------------------------------------------------------------- plumbing

fn expected_lines(name: &str) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(fixture("expected").join(name)).expect("expected file");
    text.lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(speaker, text)| (speaker.to_string(), text.to_string()))
        .collect()
}

/// An env file for the production servers, assembled from the test process's
/// own `LLM_*`/`ASR_*` environment (only reached under `LIVE_SERVER=1`).
fn write_live_env(dir: &TempDir) -> PathBuf {
    let mut text = String::new();
    for name in [
        "LLM_BASE_URL",
        "LLM_MODEL",
        "LLM_API_KEY",
        "LLM_NOTES_PATH",
        "LLM_ENABLE_THINKING",
        "ASR_BASE_URL",
        "ASR_MODEL",
        "ASR_API_KEY",
        "ASR_LANGUAGE",
    ] {
        if let Ok(value) = dotenvy::var(name) {
            text.push_str(&format!("{name}={value}\n"));
        }
    }
    for name in ["LLM_BASE_URL", "LLM_MODEL", "ASR_BASE_URL", "ASR_MODEL"] {
        assert!(
            text.contains(&format!("{name}=")),
            "{name} must be set in the environment or the repo .env for live tests"
        );
    }
    let path = dir.join(".env");
    std::fs::write(&path, text).expect("env file written");
    path
}

/// Replay `wavs` with the env file, `extra` flags after the wavs, and the
/// run limit.
async fn run_replay(
    config: &Path,
    wavs: &[&Path],
    extra: &[&Path],
    limit: Duration,
) -> std::process::Output {
    let mut args: Vec<&Path> = vec![Path::new("--env-file"), config, Path::new("--replay")];
    args.extend_from_slice(wavs);
    args.extend_from_slice(extra);
    run(&args, limit).await
}

/// A mock replay at speed 20 with `extra` flags after the wavs, asserted to
/// exit 0 (every mock-server scenario that reaches the end does).
async fn run_replay_fast(config: &Path, wavs: &[&Path], extra: &[&Path]) -> std::process::Output {
    let mut speed: Vec<&Path> = vec![Path::new("--speed"), Path::new("20")];
    speed.extend_from_slice(extra);
    let out = run_replay(config, wavs, &speed, Duration::from_secs(120)).await;
    assert_eq!(out.status.code(), Some(0));
    out
}

/// The binary directly (no temp HOME or data dir), with these arguments plus
/// a throwaway `--log-file` inside `dir`.
fn run_raw(dir: &TempDir, args: &[&Path]) -> std::process::Output {
    let mut command = std::process::Command::new(BIN);
    for arg in args {
        command.arg(arg);
    }
    command
        .arg("--log-file")
        .arg(dir.join("log.txt"))
        .output()
        .expect("binary runs")
}

fn transcript_speaker(line: &str) -> &str {
    if line[8..].starts_with("Me: ") {
        "Me"
    } else {
        "Them"
    }
}

fn transcript_text(line: &str) -> &str {
    line[8..].split_once(": ").map_or("", |(_, text)| text)
}

/// Replay the conversation fixture at speed 10 with `extra` arguments added.
async fn replay_conversation(env_file: &Path, extra: &[&Path]) -> std::process::Output {
    let me = fixture("conv_me.wav");
    let them = fixture("conv_them.wav");
    let mut speed: Vec<&Path> = vec![Path::new("--speed"), Path::new("10")];
    speed.extend_from_slice(extra);
    run_replay(env_file, &[&me, &them], &speed, Duration::from_secs(120)).await
}

// ----------------------------------------------------------- integration

#[test]
fn config_pointing_at_a_missing_file_exits_2_naming_the_path() {
    let dir = TempDir::new("cfg-missing");
    let out = run_raw(&dir, &[Path::new("--config"), &dir.join("nope.toml")]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nope.toml"), "stderr: {stderr}");
}

#[tokio::test]
async fn replay_with_a_missing_wav_exits_2_naming_the_file() {
    let dir = TempDir::new("wav-missing");
    let config = write_mock_env(&dir, 1, 1);
    let missing = dir.join("missing.wav");
    let out = run_replay(&config, &[&missing], &[], Duration::from_secs(30)).await;
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("missing.wav"), "stderr: {stderr}");
}

#[test]
fn an_unknown_flag_exits_2_and_prints_usage() {
    let out = std::process::Command::new(BIN)
        .arg("--bogus")
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage:"), "stderr: {stderr}");
}

/// Another test thread may be between fork and exec of a child process:
/// the child still shares the open file description (and so the flock)
/// until it execs, so the release can lag by a few milliseconds.
fn acquire_within_two_seconds(path: &std::path::Path) -> clueless::lock::Lock {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match clueless::lock::acquire(path) {
            Ok(lock) => break lock,
            Err(error) if std::time::Instant::now() >= deadline => {
                panic!("the lock is free again after release: {error}")
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

#[test]
fn the_lock_helper_rejects_a_second_holder_naming_the_path() {
    let dir = TempDir::new("lock");
    let path = dir.join("lock");
    let first = clueless::lock::acquire(&path).expect("first acquire succeeds");
    let second = clueless::lock::acquire(&path);
    let error = match second {
        Ok(_) => panic!("the second acquire must fail while the first is held"),
        Err(error) => error,
    };
    assert!(error.contains(&path.display().to_string()), "{error}");
    drop(first);
    let again = acquire_within_two_seconds(&path);
    drop(again);
}

#[tokio::test]
async fn replay_with_only_a_me_file_never_mentions_system_audio() {
    let dir = TempDir::new("me-only");
    let config = write_mock_env(&dir, 1, closed_port().await);
    let me = fixture("conv_me.wav");
    let out = run_replay_fast(&config, &[&me], &[]).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("SystemAudio"),
        "no SystemAudio status expected: {stderr}"
    );
}

#[tokio::test]
async fn replay_of_the_conversation_prints_only_transcript_lines() {
    let m = mock_home("conv", "mock words", &["unused"]).await;
    let me = fixture("conv_me.wav");
    let them = fixture("conv_them.wav");
    let out = run_replay(
        &m.env,
        &[&me, &them],
        &[Path::new("--speed"), Path::new("10")],
        Duration::from_secs(120),
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
    let lines = stdout_lines(&out);
    assert!(
        !lines.is_empty(),
        "the conversation yields transcript lines; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for line in &lines {
        assert!(
            common::is_transcript_line(line),
            "unexpected stdout line: {line:?}"
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Me: "), "Me lines expected: {stdout}");
    assert!(stdout.contains("Them: "), "Them lines expected: {stdout}");
}

#[tokio::test]
async fn ask_appends_the_mock_suggestion_after_its_header() {
    let m = mock_home("ask", "mock words", &["mock ", "answer"]).await;
    let me = fixture("conv_me.wav");
    let out = run_replay_fast(&m.env, &[&me], &[Path::new("--ask")]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.ends_with("--- suggestion ---\nmock answer\n"),
        "stdout ends with the suggestion: {stdout}"
    );
}

/// A transcription answer the Interview profile treats as a real question.
const QUESTION_TEXT: &str = "what is the status of the release?";

#[tokio::test]
async fn profile_interview_prints_automatic_answers_between_the_transcript_lines() {
    let m = mock_home("profile-interview", QUESTION_TEXT, &["mock ", "answer"]).await;
    let out = replay_conversation(&m.env, &[Path::new("--profile"), Path::new("interview")]).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = stdout_lines(&out);
    let headers: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.as_str() == "--- suggestion ---")
        .map(|(index, _)| index)
        .collect();
    assert!(
        !headers.is_empty(),
        "at least one automatic answer: {lines:?}"
    );
    for index in &headers {
        assert_eq!(
            lines.get(index + 1).map(String::as_str),
            Some("mock answer"),
            "every header is followed by the answer: {lines:?}"
        );
    }
    let others: Vec<&String> = lines
        .iter()
        .enumerate()
        .filter(|(index, _)| !headers.contains(index) && !headers.contains(&index.wrapping_sub(1)))
        .map(|(_, line)| line)
        .collect();
    for line in others {
        assert!(
            common::is_transcript_line(line),
            "unexpected stdout line: {line:?}"
        );
    }
    assert_eq!(
        m.llm_calls.load(Ordering::SeqCst),
        headers.len(),
        "one chat request per printed answer"
    );
}

#[tokio::test]
async fn profile_interview_with_a_pass_answer_prints_only_transcript_lines() {
    let m = mock_home("profile-pass", QUESTION_TEXT, &["PASS"]).await;
    let out = replay_conversation(&m.env, &[Path::new("--profile"), Path::new("interview")]).await;
    assert_eq!(out.status.code(), Some(0));
    assert!(
        m.llm_calls.load(Ordering::SeqCst) >= 1,
        "the profile did ask"
    );
    let lines = stdout_lines(&out);
    assert!(!lines.is_empty());
    for line in &lines {
        assert!(
            common::is_transcript_line(line),
            "unexpected stdout line: {line:?}"
        );
    }
}

#[tokio::test]
async fn profile_interview_with_ask_ends_after_the_asked_answer() {
    let m = mock_home("profile-ask", QUESTION_TEXT, &["mock ", "answer"]).await;
    let out = replay_conversation(
        &m.env,
        &[
            Path::new("--profile"),
            Path::new("interview"),
            Path::new("--ask"),
        ],
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.ends_with("--- suggestion ---\nmock answer\n"),
        "stdout ends with the asked answer: {stdout}"
    );
    assert!(
        m.llm_calls.load(Ordering::SeqCst) >= 2,
        "at least one automatic request plus the asked one"
    );
}

#[test]
fn an_unknown_profile_exits_2_and_names_the_three_valid_ones() {
    let dir = TempDir::new("profile-unknown");
    let out = run_raw(
        &dir,
        &[
            Path::new("--replay"),
            fixture("conv_me.wav").as_path(),
            Path::new("--profile"),
            Path::new("coach"),
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    for name in ["manual", "interview", "brainstorm"] {
        assert!(stderr.contains(name), "stderr: {stderr}");
    }
}

#[tokio::test]
async fn the_config_files_start_profile_does_not_apply_to_replay() {
    let m = mock_home("profile-config", QUESTION_TEXT, &["mock answer"]).await;
    let toml = m.dir.join("config.toml");
    std::fs::write(&toml, "[assist]\nstart_profile = \"interview\"\n").expect("config written");
    let out = replay_conversation(&m.env, &[Path::new("--config"), &toml]).await;
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        m.llm_calls.load(Ordering::SeqCst),
        0,
        "replay runs in Manual"
    );
    for line in stdout_lines(&out) {
        assert!(
            common::is_transcript_line(&line),
            "unexpected stdout line: {line:?}"
        );
    }
}

#[tokio::test]
async fn replay_with_the_asr_server_down_exits_zero_with_an_offline_status() {
    let dir = TempDir::new("asr-down");
    let llm = spawn_llm_mock(&["unused"]).await;
    let config = write_mock_env(&dir, llm, closed_port().await);
    let me = fixture("conv_me.wav");
    let them = fixture("conv_them.wav");
    let out = run_replay_fast(&config, &[&me, &them], &[]).await;
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "no transcript lines without ASR"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ASR offline"),
        "offline status expected: {stderr}"
    );
}

// -------------------------------------------------------------------- e2e

fn live_enabled() -> bool {
    std::env::var("LIVE_SERVER").is_ok_and(|value| value == "1")
}

/// Live-replay `wavs`, assert exit 0, and hand back the output.
async fn live_replay(tag: &str, wavs: &[&Path], limit: Duration) -> std::process::Output {
    let dir = TempDir::new(tag);
    let config = write_live_env(&dir);
    let out = run_replay(&config, wavs, &[], limit).await;
    assert_eq!(out.status.code(), Some(0));
    out
}

/// The same for scenarios that also read the binary's log file; returns the
/// output and the log text.
async fn live_replay_logged(
    tag: &str,
    wavs: &[&Path],
    extra: &[&Path],
    limit: Duration,
) -> (std::process::Output, String) {
    let dir = TempDir::new(tag);
    let config = write_live_env(&dir);
    let log = dir.join("clueless.log");
    let mut args: Vec<&Path> = vec![Path::new("--env-file"), &config, Path::new("--replay")];
    args.extend_from_slice(wavs);
    args.extend_from_slice(extra);
    let out = run_with_log(&common::workspace_root(), &args, &log, limit)
        .await
        .output;
    assert_eq!(out.status.code(), Some(0));
    let log_text = std::fs::read_to_string(&log).expect("the log file exists");
    (out, log_text)
}

#[tokio::test]
#[ignore = "needs reachable servers; run with LIVE_SERVER=1"]
async fn live_replay_reproduces_every_expected_conversation_line() {
    if !live_enabled() {
        return;
    }
    let me = fixture("conv_me.wav");
    let them = fixture("conv_them.wav");
    let out = live_replay("e2e-conv", &[&me, &them], Duration::from_secs(420)).await;
    let lines = stdout_lines(&out);
    for (speaker, expected) in expected_lines("conv.txt") {
        let same_speaker: Vec<&String> = lines
            .iter()
            .filter(|line| transcript_speaker(line) == speaker)
            .collect();
        let direct = same_speaker.iter().any(|line| {
            // at most one wrong or duplicated word per line
            word_distance(&words(&expected), &words(transcript_text(line))) <= 1
        });
        let merged = same_speaker.windows(2).any(|pair| {
            let joined = format!("{} {}", transcript_text(pair[0]), transcript_text(pair[1]));
            word_distance(&words(&expected), &words(&joined)) <= 2
        });
        assert!(
            direct || merged,
            "expected {speaker} line {expected:?} not found among:\n{}",
            lines.join("\n")
        );
    }
}

#[tokio::test]
#[ignore = "needs reachable servers; run with LIVE_SERVER=1"]
async fn live_monologue_yields_finals_without_repeated_joins() {
    if !live_enabled() {
        return;
    }
    let mono = fixture("monologue_40s.wav");
    let out = live_replay("e2e-mono", &[&mono], Duration::from_secs(300)).await;
    let texts: Vec<String> = stdout_lines(&out)
        .iter()
        .map(|line| transcript_text(line).to_string())
        .collect();
    assert!(
        texts.len() >= 2,
        "the monologue yields >= 2 finals: {texts:?}"
    );
    for pair in texts.windows(2) {
        let (a, b) = (words(&pair[0]), words(&pair[1]));
        let max_overlap = a.len().min(b.len());
        for k in (3..=max_overlap).rev() {
            assert!(
                a[a.len() - k..] != b[..k],
                "join repeats {k} words: {:?} | {:?}",
                &a[a.len() - k..],
                &b[..k]
            );
        }
    }
}

#[tokio::test]
#[ignore = "needs reachable servers; run with LIVE_SERVER=1"]
async fn live_echo_of_them_never_transcribes_as_me() {
    if !live_enabled() {
        return;
    }
    let echo = fixture("echo_me.wav");
    let them = fixture("conv_them.wav");
    let out = live_replay("e2e-echo", &[&echo, &them], Duration::from_secs(420)).await;
    let lines = stdout_lines(&out);
    assert!(
        lines.iter().any(|line| transcript_speaker(line) == "Them"),
        "Them must be transcribed (guards against a dead pass): {}",
        lines.join("\n")
    );
    for line in &lines {
        assert_ne!(transcript_speaker(line), "Me", "echo leaked as Me: {line}");
    }
}

#[tokio::test]
#[ignore = "needs reachable servers; run with LIVE_SERVER=1"]
async fn live_ask_on_french_answers_and_logs_the_first_delta() {
    if !live_enabled() {
        return;
    }
    let question = fixture("fr_question.wav");
    let (_out, log_text) = live_replay_logged(
        "e2e-fr",
        &[&question],
        &[Path::new("--ask")],
        Duration::from_secs(180),
    )
    .await;
    let out = _out;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let suggestion = stdout
        .split_once("--- suggestion ---\n")
        .unwrap_or_else(|| panic!("no suggestion in: {stdout}"))
        .1
        .trim()
        .to_string();
    assert!(!suggestion.is_empty(), "the suggestion text is non-empty");
    assert!(
        !suggestion.to_lowercase().contains("think"),
        "no thinking text in: {suggestion}"
    );
    assert!(
        log_text.contains("llm_first_delta_ms"),
        "the log shows the first-delta timing"
    );
}

#[tokio::test]
#[ignore = "needs reachable servers; run with LIVE_SERVER=1"]
async fn live_silence_yields_no_lines_and_no_asr_request() {
    if !live_enabled() {
        return;
    }
    let silence = fixture("silence_5s.wav");
    let (out, log_text) =
        live_replay_logged("e2e-silence", &[&silence], &[], Duration::from_secs(120)).await;
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "silence yields no transcript lines"
    );
    assert!(
        !log_text.contains("asr_sent_ms"),
        "silence must never trigger an ASR request"
    );
}
