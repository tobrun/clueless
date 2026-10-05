//! The timeline-positioned WAV writer over `hound`.
//!
//! One file per speaker, 16 kHz mono 16-bit. Frame `f` with meeting time
//! `t_start_ms` belongs at sample index `t_start_ms * 16`; where no audio
//! arrived the file holds zeros, so the WAV replays through the existing
//! `WavSource` unchanged and stays aligned with the meeting timeline. When
//! the position implied by the time and the place in the file drift apart
//! (a `StreamClock` re-anchor moves time backwards), frames are appended
//! anyway and an [`Anchor`] records where the mapping resumes.

use crate::paths::SAMPLES_PER_MS;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;

/// The sample rate of every written WAV.
pub const SAMPLE_RATE: u32 = 16_000;
/// The WAV header is rewritten and flushed after this many samples, so a
/// file whose writer is killed mid-meeting still reads up to its last flush.
pub const FLUSH_EVERY_SAMPLES: u64 = 80_000;
/// Silence is padded in chunks of at most this many samples.
const ZERO_CHUNK: usize = 16_000;

/// A place where the sample index stops following the meeting time, and
/// the time and index where it resumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Anchor {
    /// Meeting milliseconds of the frame that resumed the mapping.
    pub t_ms: u64,
    /// Sample index in the file where that frame was written.
    pub sample_index: u64,
}

/// A 16 kHz mono 16-bit WAV on disk, written frame by frame on the meeting
/// timeline.
pub struct AudioFile {
    writer: hound::WavWriter<BufWriter<File>>,
    /// Samples written so far; the file's length and current write position.
    written: u64,
    /// `written - target` of the previous frame; a change means the frame
    /// no longer sits where its time says it should.
    last_gap: Option<i64>,
    next_flush: u64,
}

impl AudioFile {
    /// Create `path` as a private (0600) empty 16 kHz mono 16-bit WAV.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = crate::paths::create_private_file(path)?;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer =
            hound::WavWriter::new(BufWriter::new(file), spec).map_err(hound_error_to_io)?;
        Ok(Self {
            writer,
            written: 0,
            last_gap: None,
            next_flush: FLUSH_EVERY_SAMPLES,
        })
    }

    /// Place `frame`, starting at meeting time `t_start_ms`. A target beyond
    /// the end is reached through zeros; a target at or before it (time moved
    /// backwards) is ignored and the frame is appended at the end. Returns an
    /// [`Anchor`] at the first frame and whenever the frame stops sitting
    /// where its time says it should.
    pub fn push(&mut self, t_start_ms: u64, frame: &[f32]) -> io::Result<Option<Anchor>> {
        let target = t_start_ms * SAMPLES_PER_MS;
        let gap = self.written as i64 - target as i64;
        let position = if target > self.written {
            self.pad_zeros(target - self.written)?;
            target
        } else {
            self.written
        };
        let anchor = if self.last_gap != Some(gap) {
            Some(Anchor {
                t_ms: t_start_ms,
                sample_index: position,
            })
        } else {
            None
        };
        for &sample in frame {
            let scaled = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
            self.writer
                .write_sample(scaled)
                .map_err(hound_error_to_io)?;
        }
        self.written += frame.len() as u64;
        self.last_gap = Some(gap);
        while self.written >= self.next_flush {
            self.flush()?;
            self.next_flush += FLUSH_EVERY_SAMPLES;
        }
        Ok(anchor)
    }

    /// The number of samples written so far.
    pub fn samples(&self) -> u64 {
        self.written
    }

    /// Rewrite the header with the current length and flush the data, so the
    /// file reads completely up to here even if the process dies now.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush().map_err(hound_error_to_io)
    }

    /// Finish the file: a valid complete WAV.
    pub fn finalize(self) -> io::Result<()> {
        self.writer.finalize().map_err(hound_error_to_io)
    }
}

static ZEROS: [i16; ZERO_CHUNK] = [0; ZERO_CHUNK];

impl AudioFile {
    fn pad_zeros(&mut self, samples: u64) -> io::Result<()> {
        let mut left = samples;
        while left > 0 {
            let n = left.min(ZERO_CHUNK as u64) as usize;
            for &zero in &ZEROS[..n] {
                self.writer.write_sample(zero).map_err(hound_error_to_io)?;
            }
            self.written += n as u64;
            left -= n as u64;
        }
        Ok(())
    }
}

fn hound_error_to_io(error: hound::Error) -> io::Error {
    match error {
        hound::Error::IoError(io_error) => io_error,
        other => io::Error::other(other),
    }
}

/// The number of samples a written WAV holds, for reports over traces.
pub fn wav_sample_count(path: &Path) -> io::Result<u64> {
    let reader =
        hound::WavReader::open(path).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(u64::from(reader.len()))
}
