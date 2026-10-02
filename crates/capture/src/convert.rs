//! Pure conversion helpers: interleaved/planar buffers to mono f32, and the
//! digital-silence detector. No device types here, so every rule is unit-testable.

/// Downmix interleaved multi-channel f32 frames to mono by averaging each frame.
///
/// `channels <= 1` copies the input unchanged. A trailing partial frame (fewer
/// than `channels` samples left over) is ignored.
pub fn downmix_interleaved(frames: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(frames);
        return;
    }
    for frame in frames.chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        out.push(sum / channels as f32);
    }
}

/// Reinterpret a byte buffer as native-endian f32 samples, dropping any
/// leading or trailing bytes that do not form a whole `f32`.
fn bytes_to_f32(bytes: &[u8]) -> &[f32] {
    // Header and trailer are bytes outside whole f32 samples; ignore them.
    let (_header, body, _trailer) = unsafe { bytes.align_to::<f32>() };
    body
}

/// Convert one interleaved byte buffer (f32 samples, `channels` per frame) to
/// mono. Bytes that do not form a whole f32, and a trailing partial frame, are
/// ignored.
pub fn interleaved_bytes_to_mono(bytes: &[u8], channels: usize, out: &mut Vec<f32>) {
    downmix_interleaved(bytes_to_f32(bytes), channels, out);
}

/// Convert planar audio (one byte buffer per channel, f32 samples) to mono by
/// averaging the channels sample-wise. Planes shorter than the longest one
/// limit the frame count; trailing bytes that are not a whole f32 are ignored.
pub fn planar_bytes_to_mono(planes: &[&[u8]], out: &mut Vec<f32>) {
    let planes: Vec<&[f32]> = planes.iter().map(|p| bytes_to_f32(p)).collect();
    let frames = planes.iter().map(|p| p.len()).min().unwrap_or(0);
    for i in 0..frames {
        let sum: f32 = planes.iter().map(|p| p[i]).sum();
        out.push(sum / planes.len() as f32);
    }
}

/// Counts consecutive exactly-zero samples and fires once at a limit of 3
/// seconds of audio at the given sample rate (D-mic-silence: a denied
/// microphone delivers zeros, not an error).
#[derive(Debug, Clone)]
pub struct SilenceDetector {
    limit: u64,
    zeros: u64,
    fired: bool,
}

impl SilenceDetector {
    /// Fires after 3 s of consecutive exactly-zero samples at `sample_rate`.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            limit: 3_u64 * u64::from(sample_rate),
            zeros: 0,
            fired: false,
        }
    }

    /// Feed mono samples; returns `true` exactly once, on the sample that
    /// reaches the limit. Any non-zero sample (even 1e-9) resets the count.
    pub fn feed(&mut self, samples: &[f32]) -> bool {
        if self.fired {
            return false;
        }
        for &s in samples {
            if s == 0.0 {
                self.zeros += 1;
                if self.zeros >= self.limit {
                    self.fired = true;
                    return true;
                }
            } else {
                self.zeros = 0;
            }
        }
        false
    }

    /// Whether the detector has already fired.
    pub fn fired(&self) -> bool {
        self.fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_of_stereo_frames_averages_each_frame() {
        // Spec: stereo frames (1.0, 0.0) downmix to 0.5 per frame.
        let mut out = Vec::new();
        downmix_interleaved(&[1.0, 0.0, 1.0, 0.0], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn downmix_of_mono_input_is_unchanged() {
        let mut out = Vec::new();
        downmix_interleaved(&[0.25, -0.5, 1.0], 1, &mut out);
        assert_eq!(out, vec![0.25, -0.5, 1.0]);
    }

    #[test]
    fn planar_two_buffer_input_averages_channels() {
        // Spec: planar L=[1,1] R=[0,0] -> mono [0.5,0.5].
        let l = [1.0f32, 1.0];
        let r = [0.0f32, 0.0];
        let mut out = Vec::new();
        planar_bytes_to_mono(&[by_bytes(&l), by_bytes(&r)], &mut out);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn interleaved_four_samples_two_channels_gives_two_mono_samples() {
        // Spec: single interleaved buffer of 4 f32 with 2 channels -> 2 mono samples.
        let data = [1.0f32, -1.0, 0.5, 0.25];
        let mut out = Vec::new();
        interleaved_bytes_to_mono(by_bytes(&data), 2, &mut out);
        assert_eq!(out, vec![0.0, 0.375]);
    }

    #[test]
    fn byte_buffer_not_a_multiple_of_four_ignores_trailing_bytes() {
        // Spec: trailing bytes ignored, no panic. 5 bytes = 1 whole f32 + 1 stray byte.
        let mut bytes = vec![0u8; 5];
        bytes[0..4].copy_from_slice(&0.5f32.to_ne_bytes());
        let mut out = Vec::new();
        interleaved_bytes_to_mono(&bytes, 1, &mut out);
        assert_eq!(out, vec![0.5]);
    }

    #[test]
    fn silence_detector_fires_exactly_once_at_three_seconds() {
        // Spec: at 48000 Hz, 144000 zeros fire exactly once.
        let mut d = SilenceDetector::new(48_000);
        assert!(!d.feed(&vec![0.0; 143_999]));
        assert!(d.feed(&[0.0]));
        assert!(!d.feed(&[0.0; 144_000]));
        assert!(d.fired());
    }

    #[test]
    fn silence_detector_never_fires_with_one_nonzero_sample_per_second() {
        // Spec: zeros with one sample of 1e-9 every second never fire: the
        // zero run restarts 47999 samples short of the 144000-sample limit.
        let mut d = SilenceDetector::new(48_000);
        for _ in 0..20 {
            let mut block = vec![0.0f32; 48_000];
            block[7] = 1e-9;
            assert!(!d.feed(&block));
        }
        assert!(!d.fired());
    }

    fn by_bytes(v: &[f32]) -> &[u8] {
        let (h, b, t) = unsafe { v.align_to::<u8>() };
        assert!(h.is_empty() && t.is_empty());
        b
    }
}
