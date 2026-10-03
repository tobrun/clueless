//! Microphone (and cpal loopback) source over cpal 0.18; device changes
//! and the digital-silence check are reasoned about in `docs/decisions.md`.
//!
//! The data callback only downmixes and pushes one sample at a time into the
//! ring (C-audio-callback-realtime). A `DeviceNotAvailable` or
//! `StreamInvalidated` error sets an atomic flag; `read` then starts a rebuild
//! on a helper thread and returns `Empty` until the new stream hands its ring
//! over, when it returns `Reset` with the new rate.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use clueless_types::{
    SampleSource, SourceError, SourceRead, Speaker, StatusLevel, StatusSink, StatusSource, UiEvent,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, ErrorKind, Host, SampleFormat, Stream, StreamConfig, SupportedStreamConfig};

use crate::convert::SilenceDetector;
use crate::ring::{RingReader, ring_pair_with};

/// Ring capacity: 5 s of audio at the stream rate.
const RING_SECS: u64 = 5;
/// Rebuild attempts on the helper thread before giving up.
const REBUILD_ATTEMPTS: u32 = 10;
/// Wait between rebuild attempts.
const REBUILD_WAIT: Duration = Duration::from_millis(500);

/// Which cpal endpoint a source reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// the system default input device
    DefaultInput,
    /// a named input device
    NamedInput(String),
    /// the default output device, captured through the cpal loopback tap
    Loopback,
}

/// Shared between the reader, the cpal error callback and the rebuild thread.
struct Shared {
    endpoint: Endpoint,
    /// set by the cpal error callback for rebuild-worthy errors only
    rebuild: Arc<AtomicBool>,
    /// a helper thread is rebuilding the stream
    rebuilding: AtomicBool,
    /// rebuild failed for good; read returns Empty from then on
    dead: AtomicBool,
    /// sample rate of the live ring
    rate: AtomicU32,
    /// the live cpal stream; the rebuild thread swaps it
    stream: Mutex<Option<Stream>>,
    /// ring handed over by a finished rebuild, picked up by the next read
    new_reader: Mutex<Option<RingReader>>,
    /// drop counter, shared across rebuilds
    dropped: Arc<AtomicU64>,
    status: StatusSink,
    speaker: Speaker,
}

/// A `SampleSource` fed by a cpal input stream.
pub struct CpalSource {
    shared: Arc<Shared>,
    reader: RingReader,
    silence: Option<SilenceDetector>,
}

fn status_source(speaker: Speaker) -> StatusSource {
    match speaker {
        Speaker::Me => StatusSource::Mic,
        Speaker::Them => StatusSource::SystemAudio,
    }
}

/// Human-readable device name, empty when the host cannot say.
pub fn device_name(device: &Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_default()
}

fn select_device(host: &Host, endpoint: &Endpoint) -> Result<Device, SourceError> {
    match endpoint {
        Endpoint::DefaultInput => host
            .default_input_device()
            .ok_or_else(|| SourceError::DeviceNotFound("no default input device".into())),
        Endpoint::Loopback => host
            .default_output_device()
            .ok_or_else(|| SourceError::DeviceNotFound("no default output device".into())),
        Endpoint::NamedInput(want) => {
            let devices = host
                .input_devices()
                .map_err(|e| SourceError::Backend(format!("cannot list input devices: {e}")))?;
            let mut names = Vec::new();
            for device in devices {
                let name = device_name(&device);
                if &name == want {
                    return Ok(device);
                }
                names.push(name);
            }
            Err(SourceError::DeviceNotFound(format!(
                "input device \"{want}\" not found; available input devices: {}",
                names.join(", ")
            )))
        }
    }
}

fn f32_config(device: &Device, endpoint: &Endpoint) -> Result<SupportedStreamConfig, SourceError> {
    let config = match endpoint {
        Endpoint::Loopback => device.default_output_config(),
        _ => device.default_input_config(),
    }
    .map_err(|e| SourceError::Backend(format!("no usable stream config: {e}")))?;
    if config.sample_format() != SampleFormat::F32 {
        return Err(SourceError::Backend(format!(
            "device only supports {} samples, this build needs f32",
            config.sample_format()
        )));
    }
    Ok(config)
}

/// Build a playing stream whose callback downmixes into a fresh ring;
/// returns the stream, the read side and the rate. The error callback only
/// touches the atomic rebuild flag (C-audio-callback-realtime).
fn build_stream(
    device: &Device,
    config: &SupportedStreamConfig,
    dropped: Arc<AtomicU64>,
    rebuild: Arc<AtomicBool>,
) -> Result<(Stream, RingReader, u32), SourceError> {
    let channels = usize::from(config.channels()).max(1);
    let rate = config.sample_rate();
    let (mut writer, reader) = ring_pair_with((RING_SECS * u64::from(rate)) as usize, dropped);
    let stream_config = StreamConfig::from(config.to_owned());
    let stream = device
        .build_input_stream::<f32, _, _>(
            stream_config,
            move |data: &[f32], _info| {
                // Realtime: no allocation, no lock, no log. Average each
                // frame by hand and push one sample at a time.
                if channels == 1 {
                    for &s in data {
                        writer.push(s);
                    }
                } else {
                    let divisor = channels as f32;
                    for frame in data.chunks_exact(channels) {
                        let mut sum = 0.0f32;
                        for &s in frame {
                            sum += s;
                        }
                        writer.push(sum / divisor);
                    }
                }
            },
            move |err: cpal::Error| {
                // May run on the audio thread: only touch the atomic.
                // cpal reroutes DeviceChanged by itself;
                // only these two kinds need a rebuild.
                if matches!(
                    err.kind(),
                    ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated
                ) {
                    rebuild.store(true, Ordering::Relaxed);
                }
            },
            None,
        )
        .map_err(|e| SourceError::Backend(format!("cannot open input stream: {e}")))?;
    stream
        .play()
        .map_err(|e| SourceError::Backend(format!("cannot start input stream: {e}")))?;
    Ok((stream, reader, rate))
}

impl CpalSource {
    /// Open the endpoint named by `endpoint` for `speaker`.
    pub fn open(
        endpoint: Endpoint,
        speaker: Speaker,
        status: StatusSink,
    ) -> Result<Self, SourceError> {
        let host = cpal::default_host();
        let device = select_device(&host, &endpoint)?;
        let config = f32_config(&device, &endpoint)?;
        let dropped = Arc::new(AtomicU64::new(0));
        let rebuild = Arc::new(AtomicBool::new(false));
        let (stream, reader, rate) =
            build_stream(&device, &config, Arc::clone(&dropped), Arc::clone(&rebuild))?;
        let shared = Arc::new(Shared {
            endpoint,
            rebuild,
            rebuilding: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            rate: AtomicU32::new(rate),
            stream: Mutex::new(Some(stream)),
            new_reader: Mutex::new(None),
            dropped,
            status,
            speaker,
        });
        Ok(Self {
            shared,
            reader,
            // The digital-silence check applies to the Me source.
            silence: (speaker == Speaker::Me).then(|| SilenceDetector::new(rate)),
        })
    }
}

impl SampleSource for CpalSource {
    fn sample_rate(&self) -> u32 {
        self.shared.rate.load(Ordering::Relaxed)
    }

    fn read(&mut self, out: &mut [f32]) -> SourceRead {
        let shared = &self.shared;
        if shared.dead.load(Ordering::Relaxed) {
            return SourceRead::Empty;
        }
        if shared.rebuild.swap(false, Ordering::Relaxed)
            && !shared.rebuilding.swap(true, Ordering::Relaxed)
        {
            let thread_shared = Arc::clone(shared);
            if let Err(e) = std::thread::Builder::new()
                .name("capture-mic-rebuild".into())
                .spawn(move || rebuild_loop(thread_shared))
            {
                tracing::error!("cannot spawn mic rebuild thread: {e}");
                shared.rebuilding.store(false, Ordering::Relaxed);
            }
        }
        if shared.rebuilding.load(Ordering::Relaxed) {
            let handed_over = shared.new_reader.lock().unwrap().take();
            if let Some(reader) = handed_over {
                self.reader = reader;
                if let Some(detector) = self.silence.as_mut() {
                    *detector = SilenceDetector::new(shared.rate.load(Ordering::Relaxed));
                }
                shared.rebuilding.store(false, Ordering::Relaxed);
                return SourceRead::Reset {
                    sample_rate: shared.rate.load(Ordering::Relaxed),
                };
            }
            return SourceRead::Empty;
        }
        let read = self.reader.read(out);
        if let SourceRead::Samples(n) = read
            && let Some(detector) = self.silence.as_mut()
            && detector.feed(&out[..n])
        {
            (shared.status)(UiEvent::Status {
                source: status_source(shared.speaker),
                level: StatusLevel::Warn,
                text: "Microphone delivers silence - check the Microphone permission".into(),
            });
        }
        read
    }
}

/// Re-open the stream on a helper thread (never on the read path), up to
/// `REBUILD_ATTEMPTS` times; on success hand the new ring reader over.
fn rebuild_loop(shared: Arc<Shared>) {
    for attempt in 1..=REBUILD_ATTEMPTS {
        std::thread::sleep(REBUILD_WAIT);
        match rebuild_once(&shared) {
            Ok(()) => {
                tracing::info!("microphone stream rebuilt (attempt {attempt})");
                return;
            }
            Err(e) => tracing::warn!("microphone rebuild attempt {attempt} failed: {e:?}"),
        }
    }
    shared.dead.store(true, Ordering::Relaxed);
    (shared.status)(UiEvent::Status {
        source: status_source(shared.speaker),
        level: StatusLevel::Error,
        text: "Microphone stream could not be rebuilt".into(),
    });
}

fn rebuild_once(shared: &Shared) -> Result<(), SourceError> {
    // Drop the old stream first: it may be holding the device.
    *shared.stream.lock().unwrap() = None;
    let host = cpal::default_host();
    let device = select_device(&host, &shared.endpoint)?;
    let config = f32_config(&device, &shared.endpoint)?;
    let (stream, reader, rate) = build_stream(
        &device,
        &config,
        Arc::clone(&shared.dropped),
        Arc::clone(&shared.rebuild),
    )?;
    shared.rate.store(rate, Ordering::Relaxed);
    *shared.new_reader.lock().unwrap() = Some(reader);
    *shared.stream.lock().unwrap() = Some(stream);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clueless_types::SourceFactory;
    use cpal::traits::HostTrait;

    fn null_status() -> StatusSink {
        Arc::new(|_| {})
    }

    #[test]
    fn named_device_backend_error_lists_the_available_input_names() {
        // Spec: LiveSources with backend device:NoSuchDevice -> DeviceNotFound
        // whose message lists input names. Opened through a real host, but no
        // stream is built: selection fails before any device is touched.
        let sources = crate::backend::LiveSources::new(
            clueless_types::SystemAudioBackend::Device("NoSuchDevice".into()),
            None,
            5,
            0,
        );
        let err = match sources.open(Speaker::Them, null_status()) {
            Ok(_) => panic!("NoSuchDevice cannot exist"),
            Err(e) => e,
        };
        let SourceError::DeviceNotFound(message) = err else {
            panic!("expected DeviceNotFound, got {err:?}");
        };
        let host = cpal::default_host();
        let count = host.input_devices().map(|d| d.count()).unwrap_or(0);
        assert!(message.contains("NoSuchDevice"));
        assert!(message.contains("available input devices"));
        if count > 0 {
            let first = device_name(
                &host
                    .input_devices()
                    .unwrap()
                    .next()
                    .expect("a listed input device"),
            );
            assert!(message.contains(&first), "message: {message}");
        }
    }

    #[test]
    fn unknown_mic_device_names_the_mic_and_lists_inputs() {
        let err = match CpalSource::open(
            Endpoint::NamedInput("NoSuchMic".into()),
            Speaker::Me,
            null_status(),
        ) {
            Ok(_) => panic!("NoSuchMic cannot exist"),
            Err(e) => e,
        };
        assert!(matches!(err, SourceError::DeviceNotFound(_)));
    }
}
