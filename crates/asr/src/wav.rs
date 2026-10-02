//! In-memory WAV encoding for utterance audio sent to the ASR server.

use std::io::Cursor;

/// The sample rate every transmitted segment is encoded at, per spec.
pub const SAMPLE_RATE: u32 = 16_000;

/// Encode mono f32 samples as a 16-bit PCM mono 16 kHz WAV held in memory.
/// Samples are clamped to [-1, 1] before scaling, so no value wraps.
pub fn encode_wav(pcm: &[f32]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .expect("in-memory WAV header write cannot fail");
        for &s in pcm {
            let scaled = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
            writer
                .write_sample(scaled)
                .expect("in-memory sample write cannot fail");
        }
        writer
            .finalize()
            .expect("in-memory WAV finalize cannot fail");
    }
    cursor.into_inner()
}

/// Decode a WAV file into mono f32 samples in [-1, 1] plus its sample rate.
/// Multi-channel input is averaged to mono; 16-bit and 32-bit int and 32-bit
/// float samples are supported (everything the fixtures and replay produce).
/// Used by replay-side tooling and the live tests; the client itself only
/// encodes.
pub fn decode_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32), hound::Error> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes.to_vec()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mut samples: Vec<f32> = Vec::with_capacity(reader.duration() as usize / channels);
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => {
            for value in reader.samples::<i16>() {
                samples.push(value? as f32 / i16::MAX as f32);
            }
        }
        (hound::SampleFormat::Int, 32) => {
            for value in reader.samples::<i32>() {
                samples.push(value? as f32 / i32::MAX as f32);
            }
        }
        (hound::SampleFormat::Float, 32) => {
            for value in reader.samples::<f32>() {
                samples.push(value?);
            }
        }
        _ => return Err(hound::Error::Unsupported),
    }
    if channels > 1 {
        samples = samples
            .chunks(channels)
            .map(|chunk| chunk.iter().sum::<f32>() / channels as f32)
            .collect();
    }
    Ok((samples, spec.sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_back(wav: &[u8]) -> (hound::WavSpec, Vec<i16>) {
        let mut reader = hound::WavReader::new(Cursor::new(wav.to_vec())).unwrap();
        let spec = reader.spec();
        let samples = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        (spec, samples)
    }

    #[test]
    fn encoding_n_samples_yields_44_plus_2n_bytes() {
        // Spec: WAV encoding is 16-bit PCM mono 16 kHz in memory; a canonical
        // header is 44 bytes and each sample is 2 bytes.
        let pcm = vec![0.0f32; 100];
        let wav = encode_wav(&pcm);
        assert_eq!(wav.len(), 44 + 2 * 100);
    }

    #[test]
    fn encoded_wav_reads_back_as_mono_16000_16bit() {
        let pcm = vec![0.25f32; 320];
        let wav = encode_wav(&pcm);
        let (spec, samples) = read_back(&wav);
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);
        assert_eq!(samples.len(), 320);
        // 0.25 * 32767 = 8191.75 -> rounds to 8192, worked by hand.
        assert_eq!(samples[0], 8192);
    }

    #[test]
    fn out_of_range_samples_clamp_to_the_i16_extremes_without_wrapping() {
        // Spec: samples are clamped to [-1, 1] before scaling. A naive cast of
        // 1.5 would wrap; clamping keeps the extremes.
        let wav = encode_wav(&[1.5, -1.5, 1.0, -1.0]);
        let (_, samples) = read_back(&wav);
        assert_eq!(samples[0], i16::MAX);
        assert_eq!(samples[1], -32767); // -1.5 clamps to -1.0, which scales to -32767
        assert_eq!(samples[2], i16::MAX);
        assert_eq!(samples[3], -32767);
    }

    #[test]
    fn decode_wav_recovers_encoded_samples() {
        let pcm = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        let wav = encode_wav(&pcm);
        let (decoded, rate) = decode_wav(&wav).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(decoded.len(), pcm.len());
        for (got, want) in decoded.iter().zip(&pcm) {
            assert!((got - want).abs() < 0.001, "{got} vs {want}");
        }
    }
}
