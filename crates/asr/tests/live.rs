//! E2E: the real client against the real LAN ASR server, using the repo's
//! WAV fixtures. Ignored by default; the runner opts in with `LIVE_SERVER=1`,
//! and without it each test returns early as passed.

use std::time::{Duration, Instant};

use asr::client::AsrClient;
use asr::wav::decode_wav;

fn live_enabled() -> bool {
    std::env::var("LIVE_SERVER").is_ok_and(|value| value == "1")
}

fn client() -> AsrClient {
    let base_url =
        std::env::var("ASR_BASE_URL").unwrap_or_else(|_| "http://localhost:8097".to_owned());
    let model = std::env::var("ASR_MODEL")
        .unwrap_or_else(|_| "istupakov/parakeet-tdt-0.6b-v3-onnx".to_owned());
    AsrClient::new(
        base_url,
        model,
        Duration::from_secs(10),
        [Duration::from_millis(250), Duration::from_millis(1000)],
    )
}

fn fixture_samples(name: &str) -> Vec<f32> {
    let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    let (samples, rate) = decode_wav(&bytes).expect("fixture is a readable wav");
    assert_eq!(rate, 16_000, "fixtures are 16 kHz");
    samples
}

fn assert_contains_words(text: &str, words: &[&str]) {
    let lower = text.to_lowercase();
    for word in words {
        assert!(lower.contains(word), "expected {words:?} in {text:?}");
    }
}

#[tokio::test]
#[ignore = "needs the LAN ASR server; run with LIVE_SERVER=1"]
async fn live_english_fixture_transcribes_quickly_with_its_expected_words() {
    if !live_enabled() {
        return;
    }
    let samples = fixture_samples("en_question.wav");

    let started = Instant::now();
    let text = client()
        .transcribe(&samples, 3)
        .await
        .expect("live transcription succeeds")
        .expect("the fixture contains speech");
    let elapsed = started.elapsed();

    // expected/en_question.txt: "Can we ship the new release on friday if all
    // the automated tests and the manual checks pass?"
    assert_contains_words(&text, &["ship", "release", "manual"]);
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}

#[tokio::test]
#[ignore = "needs the LAN ASR server; run with LIVE_SERVER=1"]
async fn live_dutch_fixture_transcribes_with_its_expected_words() {
    if !live_enabled() {
        return;
    }
    let samples = fixture_samples("nl_question.wav");

    let text = client()
        .transcribe(&samples, 3)
        .await
        .expect("live transcription succeeds")
        .expect("the fixture contains speech");

    // expected/nl_question.txt: "Kunnen we de presentatie morgenochtend bespreken"
    assert_contains_words(&text, &["presentatie", "bespreken"]);
}

#[tokio::test]
#[ignore = "needs the LAN ASR server; run with LIVE_SERVER=1"]
async fn live_french_fixture_transcribes_with_its_expected_words() {
    if !live_enabled() {
        return;
    }
    let samples = fixture_samples("fr_question.wav");

    let text = client()
        .transcribe(&samples, 3)
        .await
        .expect("live transcription succeeds")
        .expect("the fixture contains speech");

    // expected/fr_question.txt: "Pouvez vous envoyer le rapport avant la fin de la journee"
    assert_contains_words(&text, &["rapport", "envoyer"]);
}
