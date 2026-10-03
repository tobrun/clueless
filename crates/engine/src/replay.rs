//! Paced WAV replay: the sources `--replay` feeds into the same pipeline
//! the live capture uses.

use std::path::{Path, PathBuf};
use std::time::Instant;

use clueless_types::audio::{SampleSource, SourceError, SourceFactory};
use clueless_types::events::{Speaker, StatusSink};

/// A decoded 16-bit PCM WAV played back at wall-clock pace times `speed`.
pub struct WavSource {
    rate: u32,
    /// Mono f32 samples in [-1, 1].
    samples: Vec<f32>,
    /// Index of the next sample to hand out.
    next: usize,
    speed: f64,
    start: Option<Instant>,
    ended: bool,
}

impl WavSource {
    /// Load a WAV file, downmixing to mono. `speed` > 1 plays faster than
    /// real time (10.0 is the test default; 1.0 mirrors a live meeting).
    pub fn open(path: &Path, speed: f64) -> Result<Self, SourceError> {
        let naming = || SourceError::DeviceNotFound(path.display().to_string());
        let mut reader = hound::WavReader::open(path).map_err(|_| naming())?;
        let spec = reader.spec();
        if spec.channels == 0 || spec.sample_rate == 0 {
            return Err(naming());
        }
        let channels = spec.channels as usize;
        // `duration` counts frames, one mixed sample per frame.
        let total = reader.duration() as usize;
        let mut mixed = vec![0.0_f32; total];
        match spec.sample_format {
            hound::SampleFormat::Int => match spec.bits_per_sample {
                16 => {
                    for (index, sample) in reader.samples::<i16>().enumerate() {
                        let value = sample.map_err(|_| naming())? as f32 / 32768.0;
                        mixed[index / channels] += value;
                    }
                }
                other => {
                    return Err(SourceError::Backend(format!(
                        "{}: only 16-bit PCM WAV files are supported, found {other}-bit",
                        path.display()
                    )));
                }
            },
            hound::SampleFormat::Float => {
                if spec.bits_per_sample != 32 {
                    return Err(SourceError::Backend(format!(
                        "{}: only 16-bit PCM or 32-bit float WAV files are supported",
                        path.display()
                    )));
                }
                for (index, sample) in reader.samples::<f32>().enumerate() {
                    let value = sample.map_err(|_| naming())?;
                    mixed[index / channels] += value;
                }
            }
        }
        if channels > 1 {
            for sample in &mut mixed {
                *sample /= channels as f32;
            }
        }
        Ok(Self {
            rate: spec.sample_rate,
            samples: mixed,
            next: 0,
            speed: speed.max(f64::EPSILON),
            start: None,
            ended: false,
        })
    }
}

impl SampleSource for WavSource {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn read(&mut self, out: &mut [f32]) -> clueless_types::audio::SourceRead {
        use clueless_types::audio::SourceRead;
        if self.ended {
            return SourceRead::Ended;
        }
        let started = self.start.get_or_insert_with(Instant::now);
        // Release samples at wall pace times the speed factor.
        let elapsed = started.elapsed().as_secs_f64();
        let target = ((elapsed * self.rate as f64 * self.speed) as usize).min(self.samples.len());
        let available = (target - self.next).min(out.len());
        if available > 0 {
            let end = self.next + available;
            out[..available].copy_from_slice(&self.samples[self.next..end]);
            self.next = end;
            return SourceRead::Samples(available);
        }
        if self.next >= self.samples.len() {
            self.ended = true;
            SourceRead::Ended
        } else {
            SourceRead::Empty
        }
    }
}

/// A `SourceFactory` over one or two WAV files; it lists only the speakers
/// that actually have a file (replay with just a Me file opens just Me).
pub struct WavSources {
    files: Vec<(Speaker, PathBuf)>,
    speed: f64,
}

impl WavSources {
    /// Paths are validated up front so a missing file fails at once.
    pub fn new(files: Vec<(Speaker, PathBuf)>, speed: f64) -> Result<Self, SourceError> {
        for (_, path) in &files {
            if !path.exists() {
                return Err(SourceError::DeviceNotFound(path.display().to_string()));
            }
        }
        Ok(Self { files, speed })
    }
}

impl SourceFactory for WavSources {
    fn speakers(&self) -> Vec<Speaker> {
        self.files.iter().map(|(speaker, _)| *speaker).collect()
    }

    fn open(
        &self,
        speaker: Speaker,
        _status: StatusSink,
    ) -> Result<Box<dyn SampleSource>, SourceError> {
        let (_, path) = self
            .files
            .iter()
            .find(|(found, _)| *found == speaker)
            .ok_or_else(|| {
                SourceError::DeviceNotFound(format!("no replay file for {speaker:?}"))
            })?;
        Ok(Box::new(WavSource::open(path, self.speed)?))
    }
}
