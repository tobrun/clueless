//! Live-server ASR round trip against real fixtures.
//!
//! Transcribes WAV fixtures from the repo's `fixtures/` directory through a
//! real OpenAI-compatible ASR server, end to end from WAV bytes to text.
//! Gated behind `LIVE_SERVER=1`; without it every test passes the gate and
//! returns early, so the run stays green without a server. Endpoints come
//! from the `ASR_*` environment or the repo `.env`, exactly like the app.

use std::time::Instant;

use asr::client::AsrClient;

fn live_enabled() -> bool {
    matches!(
        dotenvy::var("LIVE_SERVER").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn client() -> AsrClient {
    let base_url = dotenvy::var("ASR_BASE_URL").expect("ASR_BASE_URL required for live tests");
    let model = dotenvy::var("ASR_MODEL").expect("ASR_MODEL required for live tests");
    let api_key = dotenvy::var("ASR_API_KEY").ok().filter(|k| !k.is_empty());
    let language = dotenvy::var("ASR_LANGUAGE").ok().filter(|v| !v.is_empty());
    AsrClient::new(
        base_url,
        model,
        api_key,
        language,
        std::time::Duration::from_secs(120),
        [],
    )
}

/// Fixture samples at 16 kHz mono, from the WAVs under `fixtures/`.
fn fixture_samples(name: &str) -> Vec<f32> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/").to_string() + name;
    let bytes = std::fs::read(&path).expect("fixture wav exists");
    let samples = hound::WavReader::new(std::io::Cursor::new(bytes))
        .expect("fixture wav opens")
        .into_samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .expect("fixture wav samples");
    samples.iter().map(|s| *s as f32 / 32768.0).collect()
}

#[tokio::test]
#[ignore = "needs a live ASR server; run with LIVE_SERVER=1"]
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
    for word in ["ship", "release", "friday", "tests"] {
        assert!(
            text.to_lowercase().contains(word),
            "{word:?} missing from {text:?}"
        );
    }
    assert!(
        elapsed.as_secs_f64() < 30.0,
        "live transcription took {elapsed:?}"
    );
}

#[tokio::test]
#[ignore = "needs a live ASR server; run with LIVE_SERVER=1"]
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

    // expected/nl_question.txt: "Kunnen we de presentatie morgenochtend
    // bespreken"
    for word in ["presentatie", "morgenochtend"] {
        assert!(
            text.to_lowercase().contains(word),
            "{word:?} missing from {text:?}"
        );
    }
}

#[tokio::test]
#[ignore = "needs a live ASR server; run with LIVE_SERVER=1"]
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

    // expected/fr_question.txt: "Pouvez vous envoyer le rapport avant la fin
    // de la journee"
    for word in ["rapport", "journee"] {
        assert!(
            text.to_lowercase().contains(word),
            "{word:?} missing from {text:?}"
        );
    }
}
