//! Tests for the real `clueless` binary: CLI contract, replay against
//! local mock servers, and the e2e live replay suite (ignored by default;
//! `LIVE_SERVER=1` opts in, without it each test returns early).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

const BIN: &str = env!("CARGO_BIN_EXE_clueless");

// ---------------------------------------------------------------- plumbing

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/../../fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
}

fn expected_lines(name: &str) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(fixture("expected").join(name)).expect("expected file");
    text.lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(speaker, text)| (speaker.to_string(), text.to_string()))
        .collect()
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clueless-app-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An env file pointing both servers at `127.0.0.1` ports, with one shared
/// model id the mocks list.
fn write_mock_env(dir: &TempDir, llm_port: u16, asr_port: u16) -> PathBuf {
    let path = dir.join(".env");
    std::fs::write(
        &path,
        format!(
            "LLM_BASE_URL=http://127.0.0.1:{llm_port}\nLLM_MODEL=test-model\n\
             ASR_BASE_URL=http://127.0.0.1:{asr_port}\nASR_MODEL=test-model\n"
        ),
    )
    .expect("env file written");
    path
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

/// Run the binary with a throwaway log file; never blocks forever. The
/// child runs on a blocking thread so mock servers keep moving.
async fn run(args: &[&Path], limit: std::time::Duration) -> std::process::Output {
    run_from(workspace_root().as_path(), args, limit).await
}

/// The workspace root: the child resolves its relative model paths from it.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/app lives in the workspace")
        .to_path_buf()
}

/// Same, from a specific working directory (the repo root keeps relative
/// model paths resolvable for the child).
async fn run_from(cwd: &Path, args: &[&Path], limit: std::time::Duration) -> std::process::Output {
    let cwd = cwd.to_path_buf();
    let dir = std::env::temp_dir().join(format!(
        "clueless-app-log-{}-{}.log",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let out = run_with_log(&cwd, args, &dir, limit).await;
    let _ = std::fs::remove_file(&dir);
    out
}

async fn run_with_log(
    cwd: &Path,
    args: &[&Path],
    log: &Path,
    limit: std::time::Duration,
) -> std::process::Output {
    let mut command = std::process::Command::new(BIN);
    command
        // Mock tests run from the crate directory; relative paths inside the
        // config (the bundled VAD model) resolve from the repo root.
        .current_dir(cwd)
        .arg("--log-file")
        .arg(log)
        // The repo's own .env (or a stray shell export) must not leak into
        // tests that pass their own --env-file or expect env errors.
        .env_remove("LLM_BASE_URL")
        .env_remove("LLM_MODEL")
        .env_remove("LLM_API_KEY")
        .env_remove("LLM_NOTES_PATH")
        .env_remove("LLM_PROFILE_PATH")
        .env_remove("LLM_ENABLE_THINKING")
        .env_remove("ASR_BASE_URL")
        .env_remove("ASR_MODEL")
        .env_remove("ASR_API_KEY")
        .env_remove("ASR_LANGUAGE")
        .args(args);
    let joined = tokio::task::spawn_blocking(move || command.output());
    tokio::time::timeout(limit, joined)
        .await
        .expect("the binary finishes within its limit")
        .expect("the child waiter does not panic")
        .expect("the binary runs")
}

fn stdout_lines(out: &std::process::Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// True for `[mm:ss] Me: text` / `[mm:ss] Them: text` lines.
fn is_transcript_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    let digits = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    line.len() >= 13
        && bytes[0] == b'['
        && digits(1)
        && digits(2)
        && bytes[3] == b':'
        && digits(4)
        && digits(5)
        && bytes[6] == b']'
        && bytes[7] == b' '
        && (line[8..].starts_with("Me: ") || line[8..].starts_with("Them: "))
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

// ------------------------------------------------------------- mock servers

#[derive(Clone)]
struct AsrMock {
    model: String,
    text: String,
}

async fn asr_models(State(mock): State<AsrMock>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/json")],
        format!(
            "{{\"object\":\"list\",\"data\":[{{\"id\":\"{}\",\"object\":\"model\"}}]}}",
            mock.model
        ),
    )
}

async fn asr_transcribe(State(mock): State<AsrMock>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/json")],
        format!("{{\"text\":\"{}\"}}", mock.text),
    )
}

async fn spawn_asr_mock(text: &str) -> u16 {
    let state = AsrMock {
        model: "test-model".to_string(),
        text: text.to_string(),
    };
    let app = Router::new()
        .route("/v1/models", get(asr_models))
        .route("/v1/audio/transcriptions", post(asr_transcribe))
        .with_state(state);
    spawn_mock(app).await
}

#[derive(Clone)]
struct LlmMock {
    parts: Vec<String>,
    /// How many chat requests this mock served.
    calls: Arc<AtomicUsize>,
}

async fn llm_models() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/json")],
        "{\"object\":\"list\",\"data\":[{\"id\":\"test-model\",\"object\":\"model\"}]}",
    )
}

async fn llm_chat(State(mock): State<LlmMock>) -> Response {
    mock.calls.fetch_add(1, Ordering::SeqCst);
    let mut body = String::new();
    for part in &mock.parts {
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{part}\"}}}}]}}\n\n"
        ));
    }
    body.push_str("data: [DONE]\n\n");
    ([(CONTENT_TYPE, "text/event-stream")], body).into_response()
}

async fn spawn_llm_mock(parts: &[&str]) -> u16 {
    spawn_counting_llm_mock(parts).await.0
}

/// The same mock, plus the count of chat requests it served.
async fn spawn_counting_llm_mock(parts: &[&str]) -> (u16, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = LlmMock {
        parts: parts.iter().map(|p| p.to_string()).collect(),
        calls: calls.clone(),
    };
    let app = Router::new()
        .route("/v1/models", get(llm_models))
        .route("/v1/chat/completions", post(llm_chat))
        .with_state(state);
    (spawn_mock(app).await, calls)
}

async fn spawn_mock(app: Router) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock binds");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock serves");
    });
    port
}

/// A port nothing listens on.
async fn closed_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("free port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    port
}

// ----------------------------------------------------------- integration

#[test]
fn config_pointing_at_a_missing_file_exits_2_naming_the_path() {
    let dir = TempDir::new("cfg-missing");
    let out = std::process::Command::new(BIN)
        .arg("--config")
        .arg(dir.join("nope.toml"))
        .arg("--log-file")
        .arg(dir.join("log.txt"))
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nope.toml"), "stderr: {stderr}");
}

#[tokio::test]
async fn replay_with_a_missing_wav_exits_2_naming_the_file() {
    let dir = TempDir::new("wav-missing");
    let config = write_mock_env(&dir, 1, 1);
    let out = run(
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &dir.join("missing.wav"),
        ],
        std::time::Duration::from_secs(30),
    )
    .await;
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
    let out = run(
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &fixture("conv_me.wav"),
            std::path::Path::new("--speed"),
            std::path::Path::new("20"),
        ],
        std::time::Duration::from_secs(120),
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("SystemAudio"),
        "no SystemAudio status expected: {stderr}"
    );
}

#[tokio::test]
async fn replay_of_the_conversation_prints_only_transcript_lines() {
    let dir = TempDir::new("conv");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    let config = write_mock_env(&dir, llm, asr);
    let out = run(
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &fixture("conv_me.wav"),
            &fixture("conv_them.wav"),
            std::path::Path::new("--speed"),
            std::path::Path::new("10"),
        ],
        std::time::Duration::from_secs(120),
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
        assert!(is_transcript_line(line), "unexpected stdout line: {line:?}");
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Me: "), "Me lines expected: {stdout}");
    assert!(stdout.contains("Them: "), "Them lines expected: {stdout}");
}

#[tokio::test]
async fn ask_appends_the_mock_suggestion_after_its_header() {
    let dir = TempDir::new("ask");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["mock ", "answer"]).await;
    let config = write_mock_env(&dir, llm, asr);
    let out = run(
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &fixture("conv_me.wav"),
            std::path::Path::new("--speed"),
            std::path::Path::new("20"),
            std::path::Path::new("--ask"),
        ],
        std::time::Duration::from_secs(120),
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.ends_with("--- suggestion ---\nmock answer\n"),
        "stdout ends with the suggestion: {stdout}"
    );
}

/// A transcription answer the Interview profile treats as a real question.
const QUESTION_TEXT: &str = "what is the status of the release?";

/// Replay the conversation fixture at speed 10 with `extra` arguments added.
async fn replay_conversation(env_file: &Path, extra: &[&Path]) -> std::process::Output {
    let mut args: Vec<&Path> = vec![Path::new("--env-file"), env_file, Path::new("--replay")];
    let me = fixture("conv_me.wav");
    let them = fixture("conv_them.wav");
    args.push(&me);
    args.push(&them);
    args.push(Path::new("--speed"));
    args.push(Path::new("10"));
    args.extend_from_slice(extra);
    run(&args, std::time::Duration::from_secs(120)).await
}

#[tokio::test]
async fn profile_interview_prints_automatic_answers_between_the_transcript_lines() {
    let dir = TempDir::new("profile-interview");
    let asr = spawn_asr_mock(QUESTION_TEXT).await;
    let (llm, calls) = spawn_counting_llm_mock(&["mock ", "answer"]).await;
    let config = write_mock_env(&dir, llm, asr);
    let out = replay_conversation(&config, &[Path::new("--profile"), Path::new("interview")]).await;
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
        assert!(is_transcript_line(line), "unexpected stdout line: {line:?}");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        headers.len(),
        "one chat request per printed answer"
    );
}

#[tokio::test]
async fn profile_interview_with_a_pass_answer_prints_only_transcript_lines() {
    let dir = TempDir::new("profile-pass");
    let asr = spawn_asr_mock(QUESTION_TEXT).await;
    let (llm, calls) = spawn_counting_llm_mock(&["PASS"]).await;
    let config = write_mock_env(&dir, llm, asr);
    let out = replay_conversation(&config, &[Path::new("--profile"), Path::new("interview")]).await;
    assert_eq!(out.status.code(), Some(0));
    assert!(calls.load(Ordering::SeqCst) >= 1, "the profile did ask");
    let lines = stdout_lines(&out);
    assert!(!lines.is_empty());
    for line in &lines {
        assert!(is_transcript_line(line), "unexpected stdout line: {line:?}");
    }
}

#[tokio::test]
async fn profile_interview_with_ask_ends_after_the_asked_answer() {
    let dir = TempDir::new("profile-ask");
    let asr = spawn_asr_mock(QUESTION_TEXT).await;
    let (llm, calls) = spawn_counting_llm_mock(&["mock ", "answer"]).await;
    let config = write_mock_env(&dir, llm, asr);
    let out = replay_conversation(
        &config,
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
        calls.load(Ordering::SeqCst) >= 2,
        "at least one automatic request plus the asked one"
    );
}

#[test]
fn an_unknown_profile_exits_2_and_names_the_three_valid_ones() {
    let dir = TempDir::new("profile-unknown");
    let out = std::process::Command::new(BIN)
        .arg("--replay")
        .arg(fixture("conv_me.wav"))
        .arg("--profile")
        .arg("coach")
        .arg("--log-file")
        .arg(dir.join("log.txt"))
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    for name in ["manual", "interview", "brainstorm"] {
        assert!(stderr.contains(name), "stderr: {stderr}");
    }
}

#[tokio::test]
async fn the_config_files_start_profile_does_not_apply_to_replay() {
    let dir = TempDir::new("profile-config");
    let asr = spawn_asr_mock(QUESTION_TEXT).await;
    let (llm, calls) = spawn_counting_llm_mock(&["mock answer"]).await;
    let env = write_mock_env(&dir, llm, asr);
    let toml = dir.join("config.toml");
    std::fs::write(&toml, "[assist]\nstart_profile = \"interview\"\n").expect("config written");
    let out = replay_conversation(&env, &[Path::new("--config"), &toml]).await;
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "replay runs in Manual");
    for line in stdout_lines(&out) {
        assert!(
            is_transcript_line(&line),
            "unexpected stdout line: {line:?}"
        );
    }
}

#[tokio::test]
async fn replay_with_the_asr_server_down_exits_zero_with_an_offline_status() {
    let dir = TempDir::new("asr-down");
    let llm = spawn_llm_mock(&["unused"]).await;
    let config = write_mock_env(&dir, llm, closed_port().await);
    let out = run(
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &fixture("conv_me.wav"),
            &fixture("conv_them.wav"),
            std::path::Path::new("--speed"),
            std::path::Path::new("20"),
        ],
        std::time::Duration::from_secs(120),
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
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

/// Word-level Levenshtein distance.
fn word_distance<T: AsRef<str>>(a: &[T], b: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, wa) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, wb) in b.iter().enumerate() {
            let cost = usize::from(wa.as_ref() != wb.as_ref());
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '\'')
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
#[ignore = "needs reachable servers; run with LIVE_SERVER=1"]
async fn live_replay_reproduces_every_expected_conversation_line() {
    if !live_enabled() {
        return;
    }
    let dir = TempDir::new("e2e-conv");
    let config = write_live_env(&dir);
    let out = run(
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &fixture("conv_me.wav"),
            &fixture("conv_them.wav"),
        ],
        std::time::Duration::from_secs(420),
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
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
    let dir = TempDir::new("e2e-mono");
    let config = write_live_env(&dir);
    let out = run(
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &fixture("monologue_40s.wav"),
        ],
        std::time::Duration::from_secs(300),
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
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
    let dir = TempDir::new("e2e-echo");
    let config = write_live_env(&dir);
    let out = run(
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &fixture("echo_me.wav"),
            &fixture("conv_them.wav"),
        ],
        std::time::Duration::from_secs(420),
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
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
    let dir = TempDir::new("e2e-fr");
    let config = write_live_env(&dir);
    let log = dir.join("clueless.log");
    let out = run_with_log(
        &workspace_root(),
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &fixture("fr_question.wav"),
            std::path::Path::new("--ask"),
        ],
        &log,
        std::time::Duration::from_secs(180),
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
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
    let log_text = std::fs::read_to_string(&log).expect("the log file exists");
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
    let dir = TempDir::new("e2e-silence");
    let config = write_live_env(&dir);
    let log = dir.join("clueless.log");
    let out = run_with_log(
        &workspace_root(),
        &[
            std::path::Path::new("--env-file"),
            &config,
            std::path::Path::new("--replay"),
            &fixture("silence_5s.wav"),
        ],
        &log,
        std::time::Duration::from_secs(120),
    )
    .await;
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "silence yields no transcript lines"
    );
    let log_text = std::fs::read_to_string(&log).expect("the log file exists");
    assert!(
        !log_text.contains("asr_sent_ms"),
        "silence must never trigger an ASR request"
    );
}
