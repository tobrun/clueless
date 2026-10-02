//! Per-frame speech probability, backed by the bundled Silero v5 model.

use voice_activity_detector::{Error as VadError, VoiceActivityDetector};

/// Scores one 512-sample frame of 16 kHz mono audio.
pub trait SpeechProb: Send {
    /// Probability in [0, 1] that the frame contains speech.
    fn prob(&mut self, frame: &[f32]) -> f32;

    /// Drop the detector's internal state, as if no frame had been scored.
    fn reset(&mut self);
}

/// `SpeechProb` on the Silero v5 model bundled in `voice_activity_detector`.
pub struct SileroVad {
    vad: VoiceActivityDetector,
}

impl SileroVad {
    /// Load the bundled model for 16 kHz audio in 512-sample chunks.
    pub fn new() -> Result<Self, VadError> {
        let vad = VoiceActivityDetector::builder()
            .sample_rate(16_000i64)
            .chunk_size(512usize)
            .build()?;
        Ok(Self { vad })
    }
}

impl SpeechProb for SileroVad {
    fn prob(&mut self, frame: &[f32]) -> f32 {
        self.vad.predict(frame.iter().copied())
    }

    fn reset(&mut self) {
        self.vad.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<f32> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name);
        let mut wav = hound::WavReader::open(&path)
            .unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
        let spec = wav.spec();
        assert_eq!(spec.sample_rate, 16_000, "fixture {name} must be 16 kHz");
        assert_eq!(spec.channels, 1, "fixture {name} must be mono");
        wav.samples::<i16>()
            .map(|s| s.expect("16-bit fixture") as f32 / 32_768.0)
            .collect()
    }

    fn frame_probs(vad: &mut dyn SpeechProb, samples: &[f32]) -> Vec<f32> {
        samples
            .chunks(512)
            .filter(|c| c.len() == 512)
            .map(|frame| vad.prob(frame))
            .collect()
    }

    #[test]
    fn fifty_frames_of_digital_silence_all_score_below_zero_point_two() {
        let mut vad = SileroVad::new().expect("silero loads");
        for i in 0..50 {
            let p = vad.prob(&[0.0f32; 512]);
            assert!(p < 0.2, "silence frame {i} scored {p}");
        }
    }

    #[test]
    fn the_en_question_fixture_scores_speech_on_at_least_half_its_frames() {
        let samples = fixture("en_question.wav");
        let mut vad = SileroVad::new().expect("silero loads");
        let probs = frame_probs(&mut vad, &samples);
        let voiced = probs.iter().filter(|p| **p > 0.5).count();
        assert!(
            voiced * 2 >= probs.len(),
            "only {voiced} of {} frames above 0.5 on speech audio",
            probs.len()
        );
    }

    #[test]
    fn after_a_reset_the_first_frame_of_silence_scores_below_zero_point_two() {
        let speech = fixture("en_question.wav");
        let silence = fixture("silence_5s.wav");
        let mut vad = SileroVad::new().expect("silero loads");
        // Drive the state deep into speech first.
        for frame in speech.chunks(512).filter(|c| c.len() == 512) {
            vad.prob(frame);
        }
        vad.reset();
        let p = vad.prob(&silence[..512]);
        assert!(p < 0.2, "first silence frame after reset scored {p}");
    }
}
