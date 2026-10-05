//! Helpers shared by the binary-level integration tests: temp directories,
//! the child-process runners, and the mock ASR/LLM servers every mock test
//! drives the binary against.
//!
//! Each test binary compiles its own copy of this module and uses only part
//! of it, so unused helpers are expected.
#![allow(dead_code)]

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fresh directory under Cargo's per-target scratch space, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "clueless-app-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The trace data directory inside a temporary home: the run helpers pass
/// this as `--data-dir` and set `HOME` to `home`, so a test never writes
/// into the real `~/.clueless` (D-test-isolation).
pub fn data_dir(home: &TempDir) -> PathBuf {
    home.join("clueless-data")
}

// ------------------------------------------------------------ child process

/// Everything one binary run produced, keeping the temporary HOME alive so
/// a test can look inside before it is cleaned up.
pub struct Run {
    pub output: std::process::Output,
    /// The temporary `HOME` the child ran with.
    pub home: TempDir,
    /// The `--data-dir` that was passed, inside `home`.
    pub data_dir: PathBuf,
}

/// Run the binary with a throwaway log file; never blocks forever. The
/// child runs on a blocking thread so mock servers keep moving.
pub async fn run(args: &[&Path], limit: std::time::Duration) -> std::process::Output {
    run_from(workspace_root().as_path(), args, limit).await
}

/// The workspace root: the child resolves its relative model paths from it.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/app lives in the workspace")
        .to_path_buf()
}

/// Same, from a specific working directory (the repo root keeps relative
/// model paths resolvable for the child).
pub async fn run_from(
    cwd: &Path,
    args: &[&Path],
    limit: std::time::Duration,
) -> std::process::Output {
    let cwd = cwd.to_path_buf();
    let log = std::env::temp_dir().join(format!(
        "clueless-app-log-{}-{}.log",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let run = run_with_log(&cwd, args, &log, limit).await;
    let _ = std::fs::remove_file(&log);
    run.output
}

/// The same from a specific working directory, reporting the temporary HOME
/// and data directory the child used.
pub async fn run_with_log(
    cwd: &Path,
    args: &[&Path],
    log: &Path,
    limit: std::time::Duration,
) -> Run {
    let home = TempDir::new("home");
    let data = data_dir(&home);
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_clueless"));
    command
        // Mock tests run from the crate directory; relative paths inside the
        // config (the bundled VAD model) resolve from the repo root.
        .current_dir(cwd)
        .arg("--log-file")
        .arg(log)
        // A recorded meeting goes to the data directory inside this throwaway
        // home, never into the real `~/.clueless` (D-test-isolation).
        .env("HOME", home.as_path())
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
        // Before the caller's args, so an explicit `--data-dir` wins.
        .arg("--data-dir")
        .arg(&data)
        .args(args);
    let joined = tokio::task::spawn_blocking(move || command.output());
    let output = tokio::time::timeout(limit, joined)
        .await
        .expect("the binary finishes within its limit")
        .expect("the child waiter does not panic")
        .expect("the binary runs");
    Run {
        output,
        home,
        data_dir: data,
    }
}

pub fn stdout_lines(out: &std::process::Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// True for `[mm:ss] Me: text` / `[mm:ss] Them: text` lines.
pub fn is_transcript_line(line: &str) -> bool {
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

// ----------------------------------------------------------------- fixtures

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/../../fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
}

/// An env file pointing both servers at `127.0.0.1` ports, with one shared
/// model id the mocks list.
pub fn write_mock_env(dir: &TempDir, llm_port: u16, asr_port: u16) -> PathBuf {
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

// ------------------------------------------------------------- mock servers

#[derive(Clone)]
pub struct AsrMock {
    pub model: String,
    pub text: String,
}

pub async fn asr_models(State(mock): State<AsrMock>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/json")],
        format!(
            "{{\"object\":\"list\",\"data\":[{{\"id\":\"{}\",\"object\":\"model\"}}]}}",
            mock.model
        ),
    )
}

pub async fn asr_transcribe(State(mock): State<AsrMock>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/json")],
        format!("{{\"text\":\"{}\"}}", mock.text),
    )
}

/// A test's temp home wired to the mock servers: a fresh directory whose
/// `.env` points at a mock ASR answering `asr_text` and an LLM streaming
/// `llm_parts` (its chat requests are counted in `llm_calls`).
pub struct Mocked {
    pub dir: TempDir,
    /// The mock ASR's port, for re-pointing the env file later.
    pub asr: u16,
    /// The `.env` file inside `dir`.
    pub env: PathBuf,
    /// How many chat requests the LLM mock served.
    pub llm_calls: Arc<AtomicUsize>,
}

pub async fn mock_home(tag: &str, asr_text: &str, llm_parts: &[&str]) -> Mocked {
    let dir = TempDir::new(tag);
    let asr = spawn_asr_mock(asr_text).await;
    let (llm, llm_calls) = spawn_counting_llm_mock(llm_parts).await;
    let env = write_mock_env(&dir, llm, asr);
    Mocked {
        dir,
        asr,
        env,
        llm_calls,
    }
}

pub async fn spawn_asr_mock(text: &str) -> u16 {
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
pub struct LlmMock {
    pub parts: Vec<String>,
    /// How many chat requests this mock served.
    pub calls: Arc<AtomicUsize>,
}

pub async fn llm_models() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/json")],
        "{\"object\":\"list\",\"data\":[{\"id\":\"test-model\",\"object\":\"model\"}]}",
    )
}

pub async fn llm_chat(State(mock): State<LlmMock>) -> Response {
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

pub async fn spawn_llm_mock(parts: &[&str]) -> u16 {
    spawn_counting_llm_mock(parts).await.0
}

/// The same mock, plus the count of chat requests it served.
pub async fn spawn_counting_llm_mock(parts: &[&str]) -> (u16, Arc<AtomicUsize>) {
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

pub async fn spawn_mock(app: Router) -> u16 {
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
pub async fn closed_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("free port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    port
}
