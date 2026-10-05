//! Binary tests for the recorded session commands (change set 9): the
//! session re-run writes a run below the session, and `--sessions`,
//! `--show`, `--delete` and `--compare` read, judge and remove what is
//! there. Every scenario first records a real session with a WAV replay
//! at speed 10 (audio on) against the mock servers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use std::os::unix::fs::PermissionsExt as _;

use trace::reader;
use trace::record::{Body, Purpose, SuggestionOrigin};

mod common;

use common::{
    TempDir, fixture, run_with_log, spawn_asr_mock, spawn_counting_llm_mock, spawn_llm_mock,
    workspace_root,
};

// ---------------------------------------------------------------- plumbing

static RUNS: AtomicUsize = AtomicUsize::new(0);

/// The data directory every run of one test shares: the runner passes its
/// own `--data-dir` first, and an explicit one in the args wins after it.
fn data(dir: &TempDir) -> PathBuf {
    dir.join("data")
}

/// One binary run against the shared data directory, with the test's env
/// file when one exists.
async fn clueless(dir: &TempDir, args: &[&Path]) -> std::process::Output {
    clueless_env(dir, args, true).await
}

/// The same, optionally without any env file: the read-only modes must
/// work with no server settings at all (D-inspect-tools).
async fn clueless_env(dir: &TempDir, args: &[&Path], use_env: bool) -> std::process::Output {
    let log = std::env::temp_dir().join(format!(
        "clueless-sessions-log-{}-{}.log",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    let data = data(dir);
    let env = dir.join(".env");
    let mut full: Vec<&Path> = vec![Path::new("--data-dir"), data.as_path()];
    if use_env {
        full.extend([Path::new("--env-file"), env.as_path()]);
    }
    full.extend_from_slice(args);
    let run = run_with_log(&workspace_root(), &full, &log, Duration::from_secs(180)).await;
    let _ = std::fs::remove_file(&log);
    run.output
}

/// A config that switches session audio on.
fn audio_config(dir: &TempDir) -> PathBuf {
    let path = dir.join("audio.toml");
    std::fs::write(&path, "[trace]\naudio = true\n").expect("config written");
    path
}

/// Record one session: a WAV replay of both conversation fixtures at
/// speed 10 with audio on, plus any extra flags (`--ask`, `--profile`).
async fn record_session(dir: &TempDir, extra: &[&Path]) -> PathBuf {
    let config = audio_config(dir);
    let me = fixture("conv_me.wav");
    let them = fixture("conv_them.wav");
    let mut args: Vec<&Path> = vec![
        Path::new("--config"),
        &config,
        Path::new("--replay"),
        &me,
        &them,
        Path::new("--speed"),
        Path::new("10"),
    ];
    args.extend_from_slice(extra);
    let before = session_dirs(&data(dir));
    let out = clueless(dir, &args).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "the recording run succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut fresh: Vec<PathBuf> = session_dirs(&data(dir))
        .into_iter()
        .filter(|path| !before.contains(path))
        .collect();
    assert_eq!(fresh.len(), 1, "one new session dir, found {fresh:?}");
    fresh.remove(0)
}

/// Re-run a recorded session by name at speed 10.
async fn rerun(dir: &TempDir, session: &Path) -> std::process::Output {
    let name = session_name(session);
    clueless(
        dir,
        &[
            Path::new("--replay-session"),
            Path::new(&name),
            Path::new("--speed"),
            Path::new("10"),
        ],
    )
    .await
}

fn session_dirs(data_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("sessions")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn session_name(session: &Path) -> String {
    session
        .file_name()
        .expect("a named session")
        .to_string_lossy()
        .into_owned()
}

fn only_run(session: &Path) -> PathBuf {
    let runs_dir = session.join("runs");
    let entries: Vec<PathBuf> = std::fs::read_dir(&runs_dir)
        .unwrap_or_else(|error| panic!("{}: {error}", runs_dir.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one run, found {entries:?}");
    entries.into_iter().next().expect("checked above")
}

fn manifest_value(dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join("manifest.json")).expect("manifest readable");
    serde_json::from_str(&text).expect("manifest is JSON")
}

fn bodies(dir: &Path) -> Vec<Body> {
    reader::read(dir)
        .expect("the trace reads back")
        .records
        .into_iter()
        .map(|record| record.body)
        .collect()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

// ------------------------------------------------------------ mock servers

/// An SSE chat response with properly escaped parts.
fn sse(parts: &[String]) -> Response {
    let mut body = String::new();
    for part in parts {
        let chunk = serde_json::json!({"choices": [{"index": 0, "delta": {"content": part}}]});
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    ([(CONTENT_TYPE, "text/event-stream")], body).into_response()
}

async fn models() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "application/json")],
        "{\"object\":\"list\",\"data\":[{\"id\":\"test-model\",\"object\":\"model\"}]}",
    )
}

fn get_models<S: Clone + Send + Sync + 'static>() -> axum::routing::MethodRouter<S> {
    axum::routing::get(models)
}

/// A chat mock that keeps every request body and answers with one stream.
async fn spawn_capture_mock(parts: &[&str]) -> (u16, Arc<Mutex<Vec<serde_json::Value>>>) {
    #[derive(Clone)]
    struct Inner {
        bodies: Arc<Mutex<Vec<serde_json::Value>>>,
        parts: Arc<Vec<String>>,
    }
    async fn chat(State(inner): State<Inner>, body: String) -> Response {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
            inner.bodies.lock().expect("not poisoned").push(value);
        }
        sse(&inner.parts)
    }
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/models", get_models())
        .route("/v1/chat/completions", post(chat))
        .with_state(Inner {
            bodies: bodies.clone(),
            parts: Arc::new(parts.iter().map(|p| p.to_string()).collect()),
        });
    (common::spawn_mock(app).await, bodies)
}

#[derive(Clone)]
struct Judge {
    answer: Arc<String>,
    calls: Arc<AtomicUsize>,
}

/// A chat mock that answers every request with the same completion text.
async fn spawn_judge_mock(answer: &str) -> (u16, Arc<AtomicUsize>) {
    async fn chat(State(judge): State<Judge>) -> Response {
        judge.calls.fetch_add(1, Ordering::SeqCst);
        sse(&[judge.answer.clone().to_string()])
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let judge = Judge {
        answer: Arc::new(answer.to_string()),
        calls: calls.clone(),
    };
    let app = Router::new()
        .route("/v1/models", get_models())
        .route("/v1/chat/completions", post(chat))
        .with_state(judge);
    (common::spawn_mock(app).await, calls)
}

// -------------------------------------------------------------- re-runs

/// The headline re-run: the recorded session re-runs through this build
/// and lands as a run below the session.
#[tokio::test]
async fn rerun_writes_a_run_named_rerun_below_the_session() {
    let dir = TempDir::new("ses-rerun");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = record_session(&dir, &[]).await;

    let out = rerun(&dir, &session).await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Me: "), "stdout has transcript: {stdout}");
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("replaying at") && stderr.contains("10"),
        "the speed warning is on stderr: {stderr}"
    );
    let run = only_run(&session);
    assert!(
        stderr.contains(&format!("run: {}", run.display())),
        "stderr announces the run dir: {stderr}"
    );

    let manifest = manifest_value(&run);
    assert_eq!(manifest["origin"], "rerun");
    assert_eq!(manifest["speed"], 10.0);
    assert_eq!(manifest["source_session"], session.display().to_string());
    assert!(!run.join("audio").exists(), "a run copies no audio");
    assert!(
        session.join("audio").is_dir(),
        "the session keeps its audio"
    );
}

/// A recorded `--ask` press is replayed after the sources drained, so the
/// run holds exactly one manual suggestion call, closed before `end`.
#[tokio::test]
async fn rerun_replays_the_recorded_suggest_after_the_drain() {
    let dir = TempDir::new("ses-rerun-ask");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["mock answer"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = record_session(&dir, &[Path::new("--ask")]).await;

    let out = rerun(&dir, &session).await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));

    let bodies = bodies(&only_run(&session));
    let manual = bodies
        .iter()
        .filter(|body| {
            matches!(
                body,
                Body::LlmRequest {
                    purpose: Purpose::Suggestion,
                    origin: Some(SuggestionOrigin::Manual),
                    ..
                }
            )
        })
        .count();
    assert_eq!(manual, 1, "exactly one manual suggestion call");
    let request_at = bodies
        .iter()
        .position(|body| {
            matches!(
                body,
                Body::LlmRequest {
                    purpose: Purpose::Suggestion,
                    origin: Some(SuggestionOrigin::Manual),
                    ..
                }
            )
        })
        .expect("request");
    let end_at = bodies
        .iter()
        .rposition(|body| matches!(body, Body::End { .. }))
        .expect("the run ends");
    assert!(
        bodies[request_at + 1..end_at]
            .iter()
            .any(|body| matches!(body, Body::LlmEnd { .. })),
        "the llm_end for the manual call comes before end"
    );
}

/// The start profile of a re-run is the one the session recorded.
#[tokio::test]
async fn rerun_starts_with_the_recorded_start_profile() {
    let dir = TempDir::new("ses-rerun-profile");
    let asr = spawn_asr_mock("what is the status of the release?").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let profile = Path::new("interview");
    let session = record_session(&dir, &[Path::new("--profile"), profile]).await;
    assert_eq!(manifest_value(&session)["session"]["profile"], "interview");

    let out = rerun(&dir, &session).await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        manifest_value(&only_run(&session))["session"]["profile"],
        "interview"
    );
}

/// The recorded notes travel with the re-run: they are staged into the
/// run directory and reach the prompt even with no notes path configured.
#[tokio::test]
async fn rerun_hands_the_recorded_notes_to_the_prompt() {
    let dir = TempDir::new("ses-rerun-notes");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    let notes = dir.join("notes.txt");
    std::fs::write(&notes, "I am Tobrun").expect("notes written");
    std::fs::write(
        dir.join(".env"),
        format!(
            "LLM_BASE_URL=http://127.0.0.1:{llm}\nLLM_MODEL=test-model\n\
             LLM_NOTES_PATH={}\n\
             ASR_BASE_URL=http://127.0.0.1:{asr}\nASR_MODEL=test-model\n",
            notes.display()
        ),
    )
    .expect("env file written");
    let session = record_session(&dir, &[Path::new("--ask")]).await;
    assert!(
        bodies(&session)
            .iter()
            .any(|body| matches!(body, Body::Notes { text } if text == "I am Tobrun")),
        "the recording kept the notes"
    );

    // The re-run has no notes of its own: only the session's copy remains.
    let (capture, request_bodies) = spawn_capture_mock(&["mock answer"]).await;
    common::write_mock_env(&dir, capture, asr);
    let out = rerun(&dir, &session).await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));

    let run = only_run(&session);
    let staged = std::fs::read_to_string(run.join("notes.txt")).expect("notes staged into the run");
    assert_eq!(staged, "I am Tobrun");
    let mode = std::fs::metadata(run.join("notes.txt"))
        .expect("notes.txt")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "the staged notes are 0600");

    let carries_notes = request_bodies
        .lock()
        .expect("not poisoned")
        .iter()
        .any(|body| {
            body["messages"]
                .as_array()
                .expect("a chat request has messages")
                .iter()
                .any(|message| {
                    message["role"] == "system"
                        && message["content"]
                            .as_str()
                            .is_some_and(|text| text.contains("I am Tobrun"))
                })
        });
    assert!(
        carries_notes,
        "the re-run's system message carries the recorded notes"
    );
}

/// A session recorded without audio cannot re-run and says how to fix it.
#[tokio::test]
async fn rerun_of_a_session_without_audio_says_so() {
    let dir = TempDir::new("ses-rerun-no-audio");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let me = fixture("conv_me.wav");
    let out = clueless(
        &dir,
        &[
            Path::new("--replay"),
            &me,
            Path::new("--speed"),
            Path::new("10"),
        ],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let session = session_dirs(&data(&dir)).remove(0);

    let name = session_name(&session);
    let out = clueless(&dir, &[Path::new("--replay-session"), Path::new(&name)]).await;
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("has no audio") && stderr.contains("audio = true"),
        "the message names the audio switch: {stderr}"
    );
}

/// A name with no session behind it fails with the path in the message.
#[tokio::test]
async fn rerun_of_an_unknown_session_names_the_path() {
    let dir = TempDir::new("ses-rerun-missing");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let out = clueless(&dir, &[Path::new("--replay-session"), Path::new("nope")]).await;
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("nope"),
        "the message names the path: {stderr}"
    );
}

/// A manifest from a newer format is refused, not guessed at.
#[tokio::test]
async fn rerun_refuses_a_newer_schema() {
    let dir = TempDir::new("ses-rerun-schema");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = record_session(&dir, &[]).await;

    let path = session.join("manifest.json");
    let mut value = manifest_value(&session);
    value["schema"] = serde_json::json!(2);
    std::fs::write(&path, serde_json::to_string_pretty(&value).expect("JSON")).expect("written");

    let name = session_name(&session);
    let out = clueless(&dir, &[Path::new("--replay-session"), Path::new(&name)]).await;
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("newer than this build reads"),
        "the message explains the schema: {stderr}"
    );
}

// --------------------------------------------------------------- compare

/// One recorded session plus one re-run, ready to compare.
async fn recorded_and_rerun(dir: &TempDir, extra: &[&Path]) -> PathBuf {
    let session = record_session(dir, extra).await;
    let out = rerun(dir, &session).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "the re-run succeeds: {}",
        stderr_of(&out)
    );
    session
}

/// `--compare A` with no B compares A with its newest run; without
/// judging it reads no env file and asks the servers nothing.
#[tokio::test]
async fn compare_no_judge_prints_the_diff_without_requests() {
    let dir = TempDir::new("ses-compare");
    let asr = spawn_asr_mock("mock words").await;
    let (llm, calls) = spawn_counting_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = recorded_and_rerun(&dir, &[]).await;
    let calls_after_replay = calls.load(Ordering::SeqCst);

    let name = session_name(&session);
    let out = clueless_env(
        &dir,
        &[
            Path::new("--compare"),
            Path::new(&name),
            Path::new("--no-judge"),
        ],
        false,
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let report = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        report.contains("compare "),
        "the report is plain text: {report}"
    );
    let me = report
        .lines()
        .find(|line| line.starts_with("Me "))
        .unwrap_or_else(|| panic!("a Me line with numbers: {report}"));
    let them = report
        .lines()
        .find(|line| line.starts_with("Them "))
        .unwrap_or_else(|| panic!("a Them line with numbers: {report}"));
    for line in [me, them] {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 5, "finals, words, distance and rate: {line}");
        assert!(
            fields[1].contains('/') && fields[2].contains('/'),
            "the line carries counts for both sides: {line}"
        );
        assert!(
            fields[4].parse::<f64>().is_ok(),
            "the line ends with the distance rate: {line}"
        );
    }
    assert!(
        report.contains("Drops"),
        "the drops table is there: {report}"
    );
    assert!(
        report.contains("Suggestions a/b"),
        "the suggestions section is there: {report}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        calls_after_replay,
        "judging off sends no request"
    );
}

/// With judging on, each judgeable pair is sent twice, both orders, and
/// a tie answer lands in the tally.
#[tokio::test]
async fn compare_judge_tallies_a_judged_pair() {
    let dir = TempDir::new("ses-compare-judge");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["mock answer"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = recorded_and_rerun(&dir, &[Path::new("--ask")]).await;

    let (judge_port, judge_calls) =
        spawn_judge_mock("{\"winner\":\"tie\",\"reason\":\"same\"}").await;
    common::write_mock_env(&dir, judge_port, asr);
    let name = session_name(&session);
    let out = clueless(&dir, &[Path::new("--compare"), Path::new(&name)]).await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let report = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        report.contains("tie 1"),
        "the tally counts the judged pair: {report}"
    );
    assert_eq!(
        judge_calls.load(Ordering::SeqCst),
        2,
        "both orders, one at a time"
    );
}

/// Judging without a reachable server marks every pair and still exits 0.
#[tokio::test]
async fn compare_without_a_reachable_llm_marks_pairs_not_judged() {
    let dir = TempDir::new("ses-compare-offline");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["mock answer"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = recorded_and_rerun(&dir, &[Path::new("--ask")]).await;

    common::write_mock_env(&dir, common::closed_port().await, asr);
    let name = session_name(&session);
    let out = clueless(&dir, &[Path::new("--compare"), Path::new(&name)]).await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let report = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        report.contains("not judged"),
        "the pairs are not judged: {report}"
    );
    assert!(
        report.contains("pair 1"),
        "the pair itself still prints: {report}"
    );
}

/// With no run to compare against the command says so and fails.
#[tokio::test]
async fn compare_without_runs_says_so() {
    let dir = TempDir::new("ses-compare-empty");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = record_session(&dir, &[]).await;

    let name = session_name(&session);
    let out = clueless_env(
        &dir,
        &[
            Path::new("--compare"),
            Path::new(&name),
            Path::new("--no-judge"),
        ],
        false,
    )
    .await;
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("has no runs"),
        "the message is about runs: {stderr}"
    );
}

/// Paths work like names (D-session-arg): the report is the same.
#[tokio::test]
async fn compare_by_path_prints_the_same_report() {
    let dir = TempDir::new("ses-compare-path");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = recorded_and_rerun(&dir, &[]).await;
    let run = only_run(&session);

    let name = session_name(&session);
    let by_name = clueless_env(
        &dir,
        &[
            Path::new("--compare"),
            Path::new(&name),
            Path::new("--no-judge"),
        ],
        false,
    )
    .await;
    let session_arg = session.display().to_string();
    let run_arg = run.display().to_string();
    let by_path = clueless_env(
        &dir,
        &[
            Path::new("--compare"),
            Path::new(&session_arg),
            Path::new(&run_arg),
            Path::new("--no-judge"),
        ],
        false,
    )
    .await;
    assert_eq!(by_name.status.code(), Some(0));
    assert_eq!(by_path.status.code(), Some(0));
    assert_eq!(by_name.stdout, by_path.stdout, "the same report by path");
}

// ----------------------------------------------------- sessions and show

/// Two recordings and one re-run: two lines, newest first, runs counted.
#[tokio::test]
async fn sessions_lists_two_sessions_newest_first_with_runs() {
    let dir = TempDir::new("ses-list");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let first = record_session(&dir, &[]).await;
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let second = record_session(&dir, &[]).await;
    let out = rerun(&dir, &second).await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));

    let out = clueless_env(&dir, &[Path::new("--sessions")], false).await;
    assert_eq!(out.status.code(), Some(0));
    let lines = common::stdout_lines(&out);
    assert_eq!(lines.len(), 2, "one line per session: {lines:?}");
    assert!(
        lines[0].starts_with(&session_name(&second)),
        "newest first: {lines:?}"
    );
    assert!(
        lines[0].contains("runs 1"),
        "the re-run counts: {}",
        lines[0]
    );
    assert!(
        lines[1].starts_with(&session_name(&first)),
        "the older session below: {lines:?}"
    );
}

/// An empty data directory lists nothing, quietly.
#[tokio::test]
async fn sessions_on_an_empty_data_dir_prints_nothing() {
    let dir = TempDir::new("ses-list-empty");
    let out = clueless_env(&dir, &[Path::new("--sessions")], false).await;
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "no output for no sessions");
}

/// `--show` prints the recorded finals with speaker and time.
#[tokio::test]
async fn show_prints_the_finals_of_a_session() {
    let dir = TempDir::new("ses-show");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = record_session(&dir, &[]).await;

    let name = session_name(&session);
    let out = clueless_env(&dir, &[Path::new("--show"), Path::new(&name)], false).await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let lines = common::stdout_lines(&out);
    assert!(
        lines.iter().all(|line| line.starts_with("[00:")),
        "every line carries a meeting time: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Me: ")),
        "the Me finals: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Them: ")),
        "the Them finals: {lines:?}"
    );
}

// ---------------------------------------------------------------- delete

/// Without `--yes` the command only previews path and size (D-delete-safety).
#[tokio::test]
async fn delete_previews_path_and_size_without_yes() {
    let dir = TempDir::new("ses-delete-preview");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = record_session(&dir, &[]).await;

    let name = session_name(&session);
    let out = clueless_env(&dir, &[Path::new("--delete"), Path::new(&name)], false).await;
    assert_eq!(out.status.code(), Some(1), "no removal without --yes");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains(&session.display().to_string()) && stdout.contains("bytes"),
        "path and size are previewed: {stdout}"
    );
    assert!(session.is_dir(), "nothing was deleted");
}

/// With `--yes` the session is gone and the list no longer shows it.
#[tokio::test]
async fn delete_yes_removes_the_session_and_it_leaves_the_list() {
    let dir = TempDir::new("ses-delete-yes");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = record_session(&dir, &[]).await;

    let name = session_name(&session);
    let out = clueless_env(
        &dir,
        &[Path::new("--delete"), Path::new(&name), Path::new("--yes")],
        false,
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert!(!session.exists(), "the session is gone");

    let out = clueless_env(&dir, &[Path::new("--sessions")], false).await;
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "the list no longer shows it");
}

/// A run addresses itself as `<session>/runs/<run>` and deleting it
/// leaves the session standing.
#[tokio::test]
async fn delete_yes_removes_only_a_run() {
    let dir = TempDir::new("ses-delete-run");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = recorded_and_rerun(&dir, &[]).await;
    let run = only_run(&session);

    let target = format!("{}/runs/{}", session_name(&session), common_os(&run));
    let out = clueless_env(
        &dir,
        &[
            Path::new("--delete"),
            Path::new(&target),
            Path::new("--yes"),
        ],
        false,
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert!(!run.exists(), "the run is gone");
    assert!(session.is_dir(), "the session stays");
    assert!(session.join("audio").is_dir(), "and its audio with it");
}

fn common_os(path: &Path) -> String {
    path.file_name()
        .expect("a named run")
        .to_string_lossy()
        .into_owned()
}

/// A directory that is not a trace is refused and left alone: a plain
/// `manifest.json` is a common file name (D-delete-safety).
#[tokio::test]
async fn delete_refuses_a_directory_that_is_not_a_trace() {
    let dir = TempDir::new("ses-delete-nontrace");
    let stranger = dir.join("somewhere");
    std::fs::create_dir(&stranger).expect("dir");
    std::fs::write(stranger.join("manifest.json"), "{\"app\":\"mine\"}").expect("manifest");

    let path = stranger.display().to_string();
    let out = clueless_env(
        &dir,
        &[Path::new("--delete"), Path::new(&path), Path::new("--yes")],
        false,
    )
    .await;
    assert_eq!(out.status.code(), Some(2));
    assert!(stranger.is_dir(), "the directory is still there");
    assert!(stranger.join("manifest.json").is_file(), "and untouched");
}

/// A recorder holds `events.jsonl` locked; deleting the session while it
/// records is refused.
#[tokio::test]
async fn delete_refuses_a_session_being_recorded() {
    let dir = TempDir::new("ses-delete-locked");
    let asr = spawn_asr_mock("mock words").await;
    let llm = spawn_llm_mock(&["unused"]).await;
    common::write_mock_env(&dir, llm, asr);
    let session = record_session(&dir, &[]).await;

    let events = session.join("events.jsonl");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&events)
        .expect("the events file opens");
    held.try_lock()
        .expect("no recorder holds it after the meeting");

    let name = session_name(&session);
    let out = clueless_env(
        &dir,
        &[Path::new("--delete"), Path::new(&name), Path::new("--yes")],
        false,
    )
    .await;
    drop(held);
    assert_eq!(out.status.code(), Some(1), "a locked session stays");
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("is being recorded"),
        "the message says why: {stderr}"
    );
    assert!(session.is_dir(), "nothing was deleted");
}
