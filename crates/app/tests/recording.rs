//! Binary tests for recording (change set 8): every recorded mode writes a
//! session directory under `--data-dir`, the `[trace]` switches decide what
//! lands on disk, and the read-only session modes work without any server
//! settings. The mock servers and runners live in `common`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use trace::manifest::Origin;
use trace::reader;
use trace::record::{Body, Record};

mod common;

use common::{
    TempDir, fixture, mock_home, run_with_log, spawn_asr_mock, spawn_counting_llm_mock,
    workspace_root,
};

// ---------------------------------------------------------------- plumbing

static RUNS: AtomicUsize = AtomicUsize::new(0);

/// One binary run against the mock servers; keeps the temporary HOME and
/// the `--data-dir` for the assertions.
async fn replay(dir: &TempDir, args: &[&Path]) -> common::Run {
    let log = std::env::temp_dir().join(format!(
        "clueless-recording-log-{}-{}.log",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    let env = dir.join(".env");
    let mut full: Vec<&Path> = vec![Path::new("--env-file"), env.as_path()];
    full.extend_from_slice(args);
    let run = run_with_log(&workspace_root(), &full, &log, Duration::from_secs(120)).await;
    let _ = std::fs::remove_file(&log);
    run
}

/// Replay the question fixture at speed 10, with `extra` arguments added
/// before the replay flags (so a later `--data-dir` still wins).
async fn replay_question(dir: &TempDir, extra: &[&Path]) -> common::Run {
    let wav = fixture("en_question.wav");
    let mut args: Vec<&Path> = extra.to_vec();
    args.extend([
        Path::new("--replay"),
        &wav,
        Path::new("--speed"),
        Path::new("10"),
    ]);
    replay(dir, &args).await
}

/// The same replay with `--ask` added.
async fn replay_ask(dir: &TempDir) -> common::Run {
    replay_question(dir, &[Path::new("--ask")]).await
}

/// Write a config with this `[trace]` body and replay the question fixture
/// at speed 10 with it.
async fn replay_with_trace(dir: &TempDir, trace_body: &str) -> common::Run {
    let config = dir.join("config.toml");
    std::fs::write(&config, format!("[trace]\n{trace_body}")).expect("config written");
    replay_question(dir, &[Path::new("--config"), &config]).await
}

/// The session directories currently under `data_dir/sessions`.
fn session_dirs(data_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("sessions")) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect()
}

/// The one session directory the run must have produced.
fn only_session(run: &common::Run) -> PathBuf {
    let dirs = session_dirs(&run.data_dir);
    assert_eq!(dirs.len(), 1, "exactly one session dir, found {dirs:?}");
    dirs.into_iter().next().expect("checked above")
}

fn count_finals(records: &[Record]) -> usize {
    records
        .iter()
        .filter(|record| matches!(record.body, Body::TranscriptFinal { .. }))
        .count()
}

/// Every regular file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

// ------------------------------------------------------------------ e2e

/// The headline scenario: a WAV replay records a session with a manifest
/// that pins what ran, and an events file closed by an `end` record.
#[tokio::test]
async fn replay_records_one_session_dir_with_manifest_and_end_record() {
    let m = mock_home("rec-basic", "hello there", &["unused"]).await;

    let run = replay_question(&m.dir, &[]).await;
    let out = &run.output;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("App Info: recording to"),
        "the recording status line is on stderr: {stderr}"
    );

    let session = only_session(&run);
    let trace = reader::read(&session).expect("the session reads back");
    assert_eq!(trace.manifest.schema, trace::manifest::SCHEMA);
    assert_eq!(trace.manifest.origin, Origin::ReplayWav);
    assert_eq!(trace.manifest.speed, 10.0);
    assert!(!trace.manifest.git_commit.is_empty());
    assert_eq!(trace.manifest.app_version, env!("CARGO_PKG_VERSION"));
    assert!(!trace.manifest.audio);
    assert!(!session.join("audio").exists());
    assert!(!trace.cut_off, "the trace ends with its end record");
    let last = trace.records.last().expect("records exist");
    assert!(
        matches!(last.body, Body::End { .. }),
        "last record: {last:?}"
    );
    assert!(
        trace
            .records
            .iter()
            .any(|record| matches!(record.body, Body::ClockStarted)),
        "the meeting clock is recorded"
    );
    let finals = count_finals(&trace.records);
    assert!(
        finals >= 1,
        "the question yields a final; records: {:?}",
        trace.records.len()
    );

    // D-test-isolation: the temp HOME never grew a real `~/.clueless`.
    assert!(
        !run.home.join(".clueless").exists(),
        "HOME holds no .clueless directory"
    );
}

/// With `[trace] audio = true` the WAVs land in the session and the status
/// line says so.
#[tokio::test]
async fn audio_switch_on_writes_speaker_wavs_and_the_status_says_with_audio() {
    let m = mock_home("rec-audio", "hello there", &["unused"]).await;

    let wav = fixture("en_question.wav");
    let run = replay_with_trace(&m.dir, "audio = true\n").await;
    assert_eq!(run.output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    let line = stderr
        .lines()
        .find(|line| line.contains("recording to"))
        .unwrap_or_else(|| panic!("no recording status line in: {stderr}"));
    assert!(line.ends_with("(with audio)"), "status line: {line}");

    let session = only_session(&run);
    let trace = reader::read(&session).expect("the session reads back");
    assert!(trace.manifest.audio);
    assert!(session.join("audio/me.wav").is_file(), "the Me WAV exists");

    // The WAV covers the fixture's audio: its meeting-time length is within
    // one second of the file that was played.
    let fixture_ms = trace::audio::wav_sample_count(&wav).expect("fixture samples") * 1000 / 16000;
    let recorded_ms = trace.audio_ms().expect("the trace reports audio length");
    assert!(
        recorded_ms.abs_diff(fixture_ms) <= 1000,
        "recorded {recorded_ms} ms is not within 1 s of the fixture's {fixture_ms} ms"
    );
}

/// `[trace] enabled = false` records nothing and says nothing about it.
#[tokio::test]
async fn trace_disabled_writes_no_session_and_no_recording_status() {
    let m = mock_home("rec-disabled", "hello there", &["unused"]).await;

    let run = replay_with_trace(&m.dir, "enabled = false\n").await;
    assert_eq!(run.output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    assert!(
        !stderr.contains("recording to"),
        "no recording status with the trace off: {stderr}"
    );
    assert!(
        session_dirs(&run.data_dir).is_empty(),
        "no session directory with the trace off"
    );
    assert!(
        !run.data_dir.join("sessions").exists(),
        "not even a sessions dir is created"
    );
}

/// `--ask` sends exactly one chat request and the trace holds its request
/// and end records under one call id.
#[tokio::test]
async fn ask_records_one_llm_request_and_one_llm_end() {
    let m = mock_home("rec-ask", "hello there", &["mock ", "answer"]).await;

    let run = replay_ask(&m.dir).await;
    assert_eq!(run.output.status.code(), Some(0));
    assert_eq!(
        m.llm_calls.load(Ordering::SeqCst),
        1,
        "one chat request was sent"
    );

    let session = only_session(&run);
    let trace = reader::read(&session).expect("the session reads back");
    let requests: Vec<u64> = trace
        .records
        .iter()
        .filter_map(|record| match &record.body {
            Body::LlmRequest { call, .. } => Some(*call),
            _ => None,
        })
        .collect();
    let ends: Vec<u64> = trace
        .records
        .iter()
        .filter_map(|record| match &record.body {
            Body::LlmEnd { call, .. } => Some(*call),
            _ => None,
        })
        .collect();
    assert_eq!(requests, vec![requests[0]], "exactly one llm_request");
    assert_eq!(
        ends, requests,
        "the request closes with exactly one llm_end"
    );
    let has_deltas = trace
        .records
        .iter()
        .any(|record| matches!(&record.body, Body::LlmDelta { .. }));
    assert!(has_deltas, "the streamed answer pieces are recorded");
}

/// The API keys never reach any file under the data directory (C-trace-no-secrets).
#[tokio::test]
async fn no_trace_file_contains_an_api_key() {
    let dir = TempDir::new("rec-keys");
    let asr = spawn_asr_mock("hello there").await;
    let (llm, _calls) = spawn_counting_llm_mock(&["mock answer"]).await;
    std::fs::write(
        dir.join(".env"),
        format!(
            "LLM_BASE_URL=http://127.0.0.1:{llm}\nLLM_MODEL=test-model\nLLM_API_KEY=sk-test-123\n\
             ASR_BASE_URL=http://127.0.0.1:{asr}\nASR_MODEL=test-model\nASR_API_KEY=sk-asr-456\n"
        ),
    )
    .expect("env file written");

    let run = replay_ask(&dir).await;
    assert_eq!(run.output.status.code(), Some(0));
    let files = files_under(&run.data_dir);
    assert!(!files.is_empty(), "the run wrote files");
    for file in &files {
        let bytes = std::fs::read(file).expect("file readable");
        let text = String::from_utf8_lossy(&bytes).to_string();
        assert!(
            !text.contains("sk-test-123") && !text.contains("sk-asr-456"),
            "{} holds an API key",
            file.display()
        );
    }
}

/// A data directory that cannot be created downgrades recording to one Warn
/// status line; the meeting itself runs unrecorded (C-trace-failure-isolated).
#[tokio::test]
async fn a_data_dir_below_a_regular_file_warns_and_the_replay_still_runs() {
    let m = mock_home("rec-bad-dir", "hello there", &["unused"]).await;
    let blocker = m.dir.join("blocker");
    std::fs::write(&blocker, "not a directory").expect("regular file");
    let bad = blocker.join("sub");

    // After the helper's own --data-dir, so this one wins.
    let run = replay_question(&m.dir, &[Path::new("--data-dir"), &bad]).await;
    assert_eq!(run.output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    assert!(
        stderr.contains("App Warn: trace: cannot record this meeting"),
        "the warn status is on stderr: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&run.output.stdout);
    assert!(
        stdout.contains("hello there"),
        "the transcript still prints: {stdout}"
    );
}

// ---------------------------------------------------- session modes

/// The read-only modes work with no env file and no server variables
/// (D-inspect-tools: they are dispatched before the environment is read).
#[tokio::test]
async fn inspect_modes_run_without_environment() {
    let dir = TempDir::new("rec-inspect");
    for (args, name) in [
        (vec!["--sessions"], "sessions"),
        (vec!["--show", "nope"], "show"),
        (vec!["--delete", "nope"], "delete"),
        (vec!["--compare", "nope", "--no-judge"], "compare"),
    ] {
        let paths: Vec<&Path> = args.iter().map(Path::new).collect();
        let run = replay(&dir, &paths);
        let run = run.await;
        let stderr = String::from_utf8_lossy(&run.output.stderr).to_string();
        let expected = match name {
            // An empty data directory lists nothing and says nothing.
            "sessions" => Some(0),
            // Everything else names a session that is not there, which
            // fails with the path in the message.
            _ => Some(2),
        };
        assert_eq!(run.output.status.code(), expected, "{args:?} exit code");
        assert!(
            !stderr.contains("Missing required environment variables"),
            "{args:?} must not read the environment: {stderr}"
        );
    }
}

/// `--replay-session` on a name with no session behind it fails with the
/// path in the message after the settings loaded.
#[tokio::test]
async fn replay_session_names_the_missing_session() {
    let m = mock_home("rec-rerun-missing", "unused", &["unused"]).await;

    let run = replay(&m.dir, &[Path::new("--replay-session"), Path::new("nope")]).await;
    assert_eq!(run.output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    assert!(
        stderr.contains("nope") && stderr.contains("sessions"),
        "the message names the missing path: {stderr}"
    );
}
