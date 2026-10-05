//! Integration proofs for the disk sink: unique run directories, the
//! manifest, timeline WAVs and anchors, crash survival, locking, tolerant
//! reading, and writer threads that never block the meeting.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use trace::audio::AudioFile;
use trace::manifest::{
    LlmSettings, MANIFEST_FILE, Manifest, Origin, SessionStart, SpeechSettings, Timings,
    VoiceDetector,
};

use trace::paths::{AUDIO_DIR, EVENTS_FILE, SESSIONS_DIR};
use trace::record::{Body, EndReason, Record, Speaker};
use trace::sink::{FailureSink, TraceOpener};
use trace::writer::DiskOpener;

// --- fixtures and helpers

fn session_start() -> SessionStart {
    SessionStart {
        speakers: vec!["me".into(), "them".into()],
        profile: trace::record::Profile::Manual,
        llm: LlmSettings {
            base_url: "http://host:8000".into(),
            model: "m".into(),
            max_tokens: 220,
            temperature: 0.4,
        },
        speech: SpeechSettings {
            base_url: "http://host:9000".into(),
            model: "w".into(),
            language: None,
        },
        voice_detector: VoiceDetector {
            start_threshold: 0.5,
            end_threshold: 0.35,
            end_silence_frames: 19,
            max_segment_ms: 15_000,
        },
        timings_ms: Timings {
            echo_hold_ms: 700,
            stop_wait_ms: 1500,
            health_timeout_ms: 2000,
            asr_timeout_ms: 15_000,
            llm_connect_ms: 2000,
            llm_stall_ms: 10_000,
        },
        compress_threshold_tokens: 90_000,
    }
}

fn silent_manifest() -> Manifest {
    Manifest {
        schema: trace::manifest::SCHEMA,
        started_at_ms: 1_791_209_002_000,
        origin: Origin::Live,
        speed: 1.0,
        app_version: "0.1.0".into(),
        git_commit: "test".into(),
        audio: false,
        session: session_start(),
    }
}

fn write_manifest(dir: &Path, manifest: &Manifest) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_string(manifest).unwrap(),
    )
    .unwrap();
}

/// One speaker frame: 512 samples at 16 kHz, 32 meeting milliseconds.
fn frame() -> Vec<f32> {
    vec![0.1; 512]
}

/// Scratch directory per test, unique per process run.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "clueless-trace-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn failure_sink() -> (FailureSink, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let seen = Arc::clone(&seen);
        Arc::new(move |message: String| seen.lock().unwrap().push(message)) as FailureSink
    };
    (sink, seen)
}

fn open(data_dir: &Path, audio: bool) -> Arc<dyn trace::sink::TraceSink> {
    let opener = DiskOpener::new(
        data_dir,
        audio,
        Origin::Live,
        1.0,
        "0.1.0".into(),
        "test".into(),
    );
    opener.open(session_start(), failure_sink().0).unwrap()
}

fn event_lines(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(EVENTS_FILE))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn read_events(dir: &Path) -> Vec<Record> {
    event_lines(dir)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "condition never held: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wav_samples(path: &Path) -> Vec<i16> {
    hound::WavReader::open(path)
        .unwrap()
        .samples::<i16>()
        .map(|s| s.unwrap())
        .collect()
}

// --- session directories

#[test]
fn create_unique_appends_dash_two_on_a_name_clash() {
    let scratch = Scratch::new("unique");
    let first = trace::paths::create_unique(scratch.path(), "2026-10-05T14-03-22Z").unwrap();
    let second = trace::paths::create_unique(scratch.path(), "2026-10-05T14-03-22Z").unwrap();
    assert!(first.is_dir());
    assert!(
        second
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-2")
    );
    assert_ne!(first, second);
}

#[test]
fn open_lands_in_a_fresh_directory_and_close_writes_four_sequenced_lines() {
    let scratch = Scratch::new("open-close");
    let sink = open(scratch.path(), false);
    let location = sink.location().unwrap();
    assert!(!location.audio);
    assert!(location.dir.starts_with(scratch.path().join(SESSIONS_DIR)));
    assert!(
        location
            .dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("Z")
    );

    sink.record(Body::ClockStarted);
    sink.record(Body::TranscriptFinal {
        speaker: Speaker::Them,
        utterance: 1,
        t0_ms: 0,
        t1_ms: 900,
        text: "hello".into(),
    });
    sink.record(Body::TranscriptFinal {
        speaker: Speaker::Me,
        utterance: 1,
        t0_ms: 1000,
        t1_ms: 1500,
        text: "hi".into(),
    });
    sink.close(EndReason::Stop);

    let lines = event_lines(&location.dir);
    assert_eq!(lines.len(), 4);
    let records = read_events(&location.dir);
    let seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
    assert_eq!(
        records[3].body,
        Body::End {
            reason: EndReason::Stop
        }
    );

    let trace_data = trace::reader::read(&location.dir).unwrap();
    assert!(!trace_data.cut_off);
    assert_eq!(trace_data.manifest.origin, Origin::Live);
    assert_eq!(trace_data.manifest.speed, 1.0);
    assert_eq!(trace_data.manifest.app_version, "0.1.0");
    assert_eq!(trace_data.manifest.git_commit, "test");
    assert!(!trace_data.manifest.audio);
    assert_eq!(trace_data.manifest.session.speakers.len(), 2);
    assert!(trace_data.manifest.started_at_ms > 1_700_000_000_000);

    // A second open of the same data directory is a different directory.
    let second = open(scratch.path(), false);
    let second_dir = second.location().unwrap().dir;
    assert_ne!(second_dir, location.dir);
    second.close(EndReason::Stop);
    assert_eq!(
        std::fs::read_dir(scratch.path().join(SESSIONS_DIR))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn a_data_dir_below_a_regular_file_fails_naming_the_path() {
    let scratch = Scratch::new("bad-dir");
    let blocker = scratch.path().join("a-file");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let opener = DiskOpener::new(
        blocker.join("sub"),
        false,
        Origin::Live,
        1.0,
        "0.1.0".into(),
        "test".into(),
    );
    let error = match opener.open(session_start(), failure_sink().0) {
        Ok(_) => panic!("opening below a regular file must fail"),
        Err(error) => error,
    };
    assert!(
        error.contains(&blocker.join("sub").join(SESSIONS_DIR).display().to_string()),
        "error should name the path: {error}"
    );
}

#[test]
fn every_created_path_is_private() {
    let scratch = Scratch::new("modes");
    let sink = open(scratch.path(), true);
    let dir = sink.location().unwrap().dir;
    sink.record(Body::ClockStarted);
    sink.audio(Speaker::Me, 0, &frame());
    sink.close(EndReason::Stop);

    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join(AUDIO_DIR)), 0o700);
    assert_eq!(mode(&dir.join(MANIFEST_FILE)), 0o600);
    assert_eq!(mode(&dir.join(EVENTS_FILE)), 0o600);
    assert_eq!(mode(&dir.join(AUDIO_DIR).join("me.wav")), 0o600);
}

// --- audio files and anchors

#[test]
fn a_frame_at_zero_anchors_at_sample_zero() {
    let scratch = Scratch::new("anchor-0");
    let path = scratch.path().join("me.wav");
    let mut file = AudioFile::create(&path).unwrap();
    let first = file.push(0, &frame()).unwrap();
    let second = file.push(32, &frame()).unwrap();
    assert_eq!(
        first,
        Some(trace::audio::Anchor {
            t_ms: 0,
            sample_index: 0
        })
    );
    assert_eq!(second, None, "a constant gap is one mapping");
    assert_eq!(file.samples(), 1024);
    file.finalize().unwrap();
    assert_eq!(wav_samples(&path).len(), 1024);
}

#[test]
fn silence_pads_to_the_frame_time_and_anchors_there() {
    let scratch = Scratch::new("anchor-1000");
    let path = scratch.path().join("me.wav");
    let mut file = AudioFile::create(&path).unwrap();
    file.push(0, &frame()).unwrap();
    let anchor = file.push(1000, &frame()).unwrap();
    assert_eq!(
        anchor,
        Some(trace::audio::Anchor {
            t_ms: 1000,
            sample_index: 16_000
        })
    );
    assert_eq!(file.samples(), 16_512);
    file.finalize().unwrap();
    let samples = wav_samples(&path);
    assert_eq!(samples.len(), 16_512);
    assert!(
        samples[512..16_000].iter().all(|&s| s == 0),
        "the stretch with no audio is zeros"
    );
}

#[test]
fn a_forward_jump_anchors_only_where_the_audio_lands() {
    let scratch = Scratch::new("anchor-jump-once");
    let path = scratch.path().join("me.wav");
    let mut file = AudioFile::create(&path).unwrap();
    file.push(0, &frame()).unwrap();
    let jump = file.push(1000, &frame()).unwrap();
    let next = file.push(1032, &frame()).unwrap();
    assert_eq!(
        jump,
        Some(trace::audio::Anchor {
            t_ms: 1000,
            sample_index: 16_000
        }),
        "the landing frame after the silence gets the anchor"
    );
    assert_eq!(
        next, None,
        "the frame continuing from the landing keeps the same mapping, no second anchor"
    );
    file.finalize().unwrap();
}

#[test]
fn backwards_time_appends_and_anchors_once() {
    let scratch = Scratch::new("anchor-backwards");
    let path = scratch.path().join("me.wav");
    let mut file = AudioFile::create(&path).unwrap();
    let first = file.push(1000, &frame()).unwrap();
    let second = file.push(500, &frame()).unwrap();
    let third = file.push(532, &frame()).unwrap();
    assert_eq!(
        first,
        Some(trace::audio::Anchor {
            t_ms: 1000,
            sample_index: 16_000
        })
    );
    assert_eq!(
        second,
        Some(trace::audio::Anchor {
            t_ms: 500,
            sample_index: 16_512
        })
    );
    assert_eq!(third, None, "the gap is unchanged, no new anchor");
    assert_eq!(file.samples(), 17_536);
    file.finalize().unwrap();
    assert_eq!(wav_samples(&path).len(), 17_536);
}

#[test]
fn a_killed_writer_leaves_a_wav_readable_to_its_last_flush() {
    let scratch = Scratch::new("flush");
    let path = scratch.path().join("me.wav");
    let mut file = AudioFile::create(&path).unwrap();
    file.push(0, &vec![0.25f32; 80_000]).unwrap();
    // Simulate a killed process: no finalize, no drop.
    std::mem::forget(file);
    assert_eq!(trace::audio::wav_sample_count(&path).unwrap(), 80_000);
}

#[test]
fn audio_on_writes_the_wav_and_records_anchors() {
    let scratch = Scratch::new("audio-on");
    let sink = open(scratch.path(), true);
    let dir = sink.location().unwrap().dir;
    assert!(sink.location().unwrap().audio);
    sink.record(Body::ClockStarted);
    for k in 0..10 {
        sink.audio(Speaker::Me, k * 32, &frame());
    }
    sink.close(EndReason::Stop);

    let samples = wav_samples(&dir.join(AUDIO_DIR).join("me.wav"));
    assert_eq!(samples.len(), 5120);
    let spec = hound::WavReader::open(dir.join(AUDIO_DIR).join("me.wav"))
        .unwrap()
        .spec();
    assert_eq!(spec.sample_rate, 16_000);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.bits_per_sample, 16);

    let trace_data = trace::reader::read(&dir).unwrap();
    let anchors: Vec<&Body> = trace_data
        .records
        .iter()
        .map(|r| &r.body)
        .filter(|b| matches!(b, Body::AudioAnchor { .. }))
        .collect();
    assert_eq!(
        anchors.len(),
        1,
        "a steady stream needs one anchor, at the start"
    );
    assert_eq!(
        *anchors[0],
        Body::AudioAnchor {
            speaker: Speaker::Me,
            t_ms: 0,
            sample_index: 0
        }
    );
}

#[test]
fn audio_off_keeps_no_audio_directory() {
    let scratch = Scratch::new("audio-off");
    let sink = open(scratch.path(), false);
    let dir = sink.location().unwrap().dir;
    assert!(!sink.location().unwrap().audio);
    sink.record(Body::ClockStarted);
    for k in 0..3 {
        sink.audio(Speaker::Me, k * 32, &frame());
    }
    sink.close(EndReason::Stop);
    assert!(!dir.join(AUDIO_DIR).exists());
}

// --- writer threads never block the meeting

#[test]
fn a_stuck_audio_thread_cannot_push_out_text_records() {
    let scratch = Scratch::new("audio-flood");
    let release = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let (release_for_hook, entered_for_hook) = (release.clone(), entered.clone());
    let opener = DiskOpener::new(
        scratch.path(),
        true,
        Origin::Live,
        1.0,
        "0.1.0".into(),
        "test".into(),
    )
    .with_audio_hook(Arc::new(move || {
        entered_for_hook.store(true, Ordering::SeqCst);
        while !release_for_hook.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }));
    let sink = opener.open(session_start(), failure_sink().0).unwrap();
    let dir = sink.location().unwrap().dir;

    sink.record(Body::ClockStarted);
    sink.audio(Speaker::Me, 0, &[0.1; 16]);
    wait_until("the audio thread entered the hook", || {
        entered.load(Ordering::SeqCst)
    });

    // Overflow the audio queue while its thread is held.
    for k in 0..5000_u64 {
        sink.audio(Speaker::Me, k, &[0.1; 16]);
    }
    // Text records keep flowing on their own queue.
    for k in 0..10_u64 {
        sink.record(Body::TranscriptFinal {
            speaker: Speaker::Them,
            utterance: k,
            t0_ms: k,
            t1_ms: k + 1,
            text: format!("line {k}"),
        });
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let lines = loop {
        let lines = event_lines(&dir);
        let finals = lines
            .iter()
            .filter(|line| line.contains("\"transcript_final\""))
            .count();
        if finals == 10 {
            break lines;
        }
        assert!(
            Instant::now() < deadline,
            "only {finals} of 10 records arrived"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("broken line {line:?}: {e}"));
    }

    release.store(true, Ordering::SeqCst);
    sink.close(EndReason::Stop);
}

#[test]
fn close_while_the_record_thread_is_held_returns_within_its_deadline() {
    let scratch = Scratch::new("close-deadline");
    let release = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let (release_for_hook, entered_for_hook) = (release.clone(), entered.clone());
    let opener = DiskOpener::new(
        scratch.path(),
        false,
        Origin::Live,
        1.0,
        "0.1.0".into(),
        "test".into(),
    )
    .with_record_hook(Arc::new(move || {
        entered_for_hook.store(true, Ordering::SeqCst);
        while !release_for_hook.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }));
    let sink = opener.open(session_start(), failure_sink().0).unwrap();
    sink.record(Body::ClockStarted);
    wait_until("the record thread entered the hook", || {
        entered.load(Ordering::SeqCst)
    });

    let start = Instant::now();
    sink.close(EndReason::Stop);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "close took {elapsed:?} with a held writer"
    );

    release.store(true, Ordering::SeqCst);
}

// --- tolerant reading

#[test]
fn a_half_written_last_line_reads_as_a_prefix_with_cut_off() {
    let scratch = Scratch::new("cut-off");
    let dir = scratch.path().join("run");
    write_manifest(&dir, &silent_manifest());
    let mut file = std::fs::File::create(dir.join(EVENTS_FILE)).unwrap();
    for seq in 1..=3_u64 {
        let record = Record {
            seq,
            at_ms: seq * 10,
            body: Body::TranscriptFinal {
                speaker: Speaker::Me,
                utterance: seq,
                t0_ms: 0,
                t1_ms: seq * 10,
                text: format!("line {seq}"),
            },
        };
        writeln!(file, "{}", serde_json::to_string(&record).unwrap()).unwrap();
    }
    file.write_all(b"{\"seq\":4,\"kind\":\"transcript_fin")
        .unwrap();
    drop(file);

    let trace_data = trace::reader::read(&dir).unwrap();
    assert!(trace_data.cut_off, "no end record, tail was torn");
    assert_eq!(trace_data.records.len(), 3);
    assert_eq!(trace_data.records[2].seq, 3);
    match &trace_data.records[0].body {
        Body::TranscriptFinal { text, .. } => assert_eq!(text, "line 1"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn records_are_returned_sorted_by_seq() {
    let scratch = Scratch::new("seq-order");
    let dir = scratch.path().join("run");
    write_manifest(&dir, &silent_manifest());
    let line_for = |seq: u64| {
        serde_json::to_string(&Record {
            seq,
            at_ms: 0,
            body: Body::ClockStarted,
        })
        .unwrap()
    };
    std::fs::write(
        dir.join(EVENTS_FILE),
        format!("{}\n{}\n", line_for(2), line_for(1)),
    )
    .unwrap();

    let trace_data = trace::reader::read(&dir).unwrap();
    let seqs: Vec<u64> = trace_data.records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2]);
}

#[test]
fn a_newer_schema_is_refused() {
    let scratch = Scratch::new("schema");
    let dir = scratch.path().join("run");
    let mut manifest = silent_manifest();
    manifest.schema = 2;
    write_manifest(&dir, &manifest);

    let error = trace::reader::read_manifest(&dir).unwrap_err();
    assert!(
        matches!(
            error,
            trace::reader::ReadError::NewerSchema { found: 2, .. }
        ),
        "{error}"
    );
}

// --- re-runs below a session

#[test]
fn for_run_writes_below_the_session_and_locks_its_events() {
    let scratch = Scratch::new("for-run");
    let session = scratch.path().join("session");
    let opener = DiskOpener::for_run(&session, 1.0, "0.1.0".into(), "test".into()).unwrap();
    let run_dir = opener.run_dir().unwrap().to_path_buf();
    assert!(run_dir.starts_with(session.join("runs")));

    let (failures, seen) = failure_sink();
    let sink = opener.open(session_start(), failures).unwrap();
    assert!(!sink.location().unwrap().audio);
    sink.record(Body::ClockStarted);
    sink.audio(Speaker::Me, 0, &frame());

    // A second writer cannot lock the record file while it is open.
    let contender = std::fs::OpenOptions::new()
        .write(true)
        .open(run_dir.join(EVENTS_FILE))
        .unwrap();
    assert!(
        contender.try_lock().is_err(),
        "events.jsonl must stay locked"
    );

    sink.close(EndReason::Stop);
    assert!(seen.lock().unwrap().is_empty(), "no failures expected");

    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join(MANIFEST_FILE)).unwrap())
            .unwrap();
    assert_eq!(raw["origin"], "rerun");
    assert_eq!(raw["source_session"], session.display().to_string());
    assert_eq!(raw["audio"], false);

    let trace_data = trace::reader::read(&run_dir).unwrap();
    assert_eq!(trace_data.manifest.origin, Origin::Rerun);
    assert!(!trace_data.cut_off);
    assert!(
        !run_dir.join(AUDIO_DIR).exists(),
        "a re-run records no audio"
    );
}
