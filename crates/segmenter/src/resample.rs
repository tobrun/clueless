//! Convert mono f32 audio at any sample rate to 16 kHz frames of 512 samples.

use std::collections::VecDeque;

use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{Fft, FixedSync, Resampler};

/// Output sample rate every stream is resampled to.
pub const OUT_RATE: u32 = 16_000;

/// One VAD frame is 512 samples, 32 ms at 16 kHz.
pub const FRAME_SAMPLES: usize = 512;

/// Input chunk hint for the rubato resampler. At 48 kHz in, `FixedSync::Both`
/// turns this into exactly 1536 input frames per call and exactly 512 output
/// frames per call (probe 2026-10-02).
const CHUNK_IN: usize = 1536;

/// Push mono f32 samples at the input rate, pop complete 512-sample frames at
/// 16 kHz. Partial input or output is kept between calls; nothing is emitted
/// until a full frame exists.
pub struct Resampler16k {
    /// `None` for 16 kHz input, which needs no resampling.
    fft: Option<Fft<f32>>,
    /// Input samples waiting for a full resampler input chunk.
    in_pending: Vec<f32>,
    /// Resampled samples not yet returned as a frame.
    out_pending: VecDeque<f32>,
}

impl Resampler16k {
    /// Build a resampler from `in_rate` to 16 kHz. `in_rate` of 16000 skips
    /// rubato entirely.
    pub fn new(in_rate: u32) -> Self {
        let fft = if in_rate == OUT_RATE {
            None
        } else {
            let fft = Fft::<f32>::new(
                in_rate as usize,
                OUT_RATE as usize,
                CHUNK_IN,
                1,
                FixedSync::Both,
            )
            .unwrap_or_else(|e| panic!("cannot resample {in_rate} Hz to {OUT_RATE} Hz: {e}"));
            Some(fft)
        };
        Self {
            fft,
            in_pending: Vec::new(),
            out_pending: VecDeque::new(),
        }
    }

    /// Append mono samples at the input rate and resample everything a full
    /// input chunk covers.
    pub fn push(&mut self, samples: &[f32]) {
        self.in_pending.extend_from_slice(samples);
        match &mut self.fft {
            None => {
                while self.in_pending.len() >= FRAME_SAMPLES {
                    let chunk: Vec<f32> = self.in_pending.drain(..FRAME_SAMPLES).collect();
                    self.out_pending.extend(chunk);
                }
            }
            Some(fft) => {
                let need = fft.input_frames_next();
                while self.in_pending.len() >= need {
                    let chunk: Vec<f32> = self.in_pending.drain(..need).collect();
                    let input = InterleavedOwned::<f32>::new_from(chunk, 1, need)
                        .expect("drained exactly `need` frames for 1 channel");
                    let output = fft
                        .process(&input, None)
                        .expect("input buffer has input_frames_next frames");
                    self.out_pending.extend(output.take_data());
                }
            }
        }
    }

    /// Return the next complete 512-sample 16 kHz frame, or `None` when fewer
    /// than 512 resampled samples are buffered.
    pub fn pop_frame(&mut self) -> Option<[f32; FRAME_SAMPLES]> {
        if self.out_pending.len() < FRAME_SAMPLES {
            return None;
        }
        let mut frame = [0.0f32; FRAME_SAMPLES];
        for slot in frame.iter_mut() {
            *slot = self.out_pending.pop_front().expect("length checked above");
        }
        Some(frame)
    }

    /// Drop every buffered sample and the resampler's internal state, as if
    /// nothing had been pushed.
    pub fn reset(&mut self) {
        self.in_pending.clear();
        self.out_pending.clear();
        if let Some(fft) = &mut self.fft {
            fft.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(samples: usize, rate: u32, freq: f32, amplitude: f32) -> Vec<f32> {
        (0..samples)
            .map(|n| amplitude * (2.0 * std::f32::consts::PI * freq * n as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn input_at_48k_of_15360_samples_yields_exactly_ten_frames_of_512() {
        let mut r = Resampler16k::new(48_000);
        r.push(&sine(15_360, 48_000, 440.0, 0.5));
        for i in 0..10 {
            assert!(r.pop_frame().is_some(), "expected frame {i}");
        }
        assert_eq!(
            r.pop_frame(),
            None,
            "15360 samples at 48k is exactly 10 frames"
        );
    }

    #[test]
    fn one_second_at_44k1_yields_31_frames_and_keeps_the_leftover_for_the_next_push() {
        let signal = sine(88_200, 44_100, 440.0, 0.5);
        // Push in two one-second halves and once as a single two-second
        // stream; the frame sequence must be identical, which proves the
        // leftover samples of the first push are carried, not dropped.
        let mut split = Resampler16k::new(44_100);
        split.push(&signal[..44_100]);
        let mut frames: Vec<[f32; FRAME_SAMPLES]> = Vec::new();
        while let Some(f) = split.pop_frame() {
            frames.push(f);
        }
        assert_eq!(
            frames.len(),
            31,
            "44100 samples at 44.1k -> 16000 out -> 31 frames"
        );
        split.push(&signal[44_100..]);
        while let Some(f) = split.pop_frame() {
            frames.push(f);
        }
        let mut whole = Resampler16k::new(44_100);
        whole.push(&signal);
        let mut expected: Vec<[f32; FRAME_SAMPLES]> = Vec::new();
        while let Some(f) = whole.pop_frame() {
            expected.push(f);
        }
        assert_eq!(
            expected.len(),
            62,
            "88200 samples at 44.1k -> 32000 out -> 62 frames"
        );
        assert_eq!(
            frames, expected,
            "split push must continue where the first push stopped"
        );
    }

    #[test]
    fn input_at_16k_returns_frames_identical_to_the_input() {
        let input: Vec<f32> = (0..1024).map(|n| (n as f32 / 1024.0 - 0.5) * 0.8).collect();
        let mut r = Resampler16k::new(16_000);
        r.push(&input);
        let f0 = r.pop_frame().expect("first frame");
        let f1 = r.pop_frame().expect("second frame");
        assert_eq!(f0.to_vec(), &input[..512]);
        assert_eq!(f1.to_vec(), &input[512..1024]);
        assert_eq!(r.pop_frame(), None);
    }

    #[test]
    fn a_440hz_sine_survives_resampling_with_amplitude_within_two_percent() {
        let mut r = Resampler16k::new(48_000);
        r.push(&sine(48_000, 48_000, 440.0, 1.0));
        let mut out: Vec<f32> = Vec::new();
        while let Some(f) = r.pop_frame() {
            out.extend_from_slice(&f);
        }
        // The first 256 samples cover the resampler's startup delay.
        let tail = &out[256..];
        assert!(
            tail.len() > 15_000,
            "1 s of 48k input should yield ~16000 samples, got {}",
            out.len()
        );
        let peak = tail.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        // Sampling a 440 Hz sine at 16 kHz passes within 0.4% of the true peak.
        assert!(
            (0.98..=1.02).contains(&peak),
            "440 Hz sine peak amplitude {peak} outside [0.98, 1.02]"
        );
    }

    #[test]
    fn after_a_reset_the_next_frame_contains_no_samples_from_before_it() {
        let mut r = Resampler16k::new(48_000);
        // A partial push: less than one 1536-sample input chunk, so this loud
        // sine still sits in the buffers when the reset lands.
        r.push(&sine(1_000, 48_000, 1_000.0, 1.0));
        r.reset();
        r.push(&vec![0.0f32; 6_144]);
        let mut frames = 0;
        while let Some(f) = r.pop_frame() {
            frames += 1;
            let peak = f.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(
                peak < 0.01,
                "frame {frames} after reset carries old audio (peak {peak})"
            );
        }
        assert!(
            frames >= 3,
            "6144 silent samples at 48k should yield >= 3 frames, got {frames}"
        );
    }
}
