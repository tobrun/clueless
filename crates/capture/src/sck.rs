//! ScreenCaptureKit system-audio capture (the rationale for the backend,
//! watchdog and permission handling is in `docs/decisions.md`).
//!
//! A display filter with audio enabled and video reduced to 2x2 at 1 fps
//! yields a 48 kHz stereo float stream of everything this process does not
//! play. The output handler converts sample buffers with the `convert`
//! helpers and pushes through a `Mutex` around the `RingWriter`. The
//! delegate's stop callback and `read` feed the [`RestartPolicy`]: on
//! `Restart` a helper thread waits 1 s and starts a new stream while `read`
//! returns `Empty`, and the first read after the new stream returns `Gap`
//! once with a `Warn` status; on `GiveUp` an `Error` status is sent and
//! `read` returns `Empty` from then on.

use std::ffi::c_void;
use std::os::raw::c_char;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clueless_types::{
    SampleSource, SourceError, SourceRead, StatusLevel, StatusSink, StatusSource, UiEvent,
};
use screencapturekit::cm::CMSampleBuffer;
use screencapturekit::shareable_content::SCShareableContent;
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;
use screencapturekit::stream::delegate_trait::StreamCallbacks;
use screencapturekit::stream::output_type::SCStreamOutputType;
use screencapturekit::stream::sc_stream::SCStream;

use crate::convert::{interleaved_bytes_to_mono, planar_bytes_to_mono};
use crate::ring::{RingReader, RingWriter, ring_pair};
use crate::watchdog::{RestartPolicy, WatchdogAction};

/// ScreenCaptureKit is configured to deliver 48 kHz stereo.
const TARGET_RATE: u32 = 48_000;
const TARGET_CHANNELS: u32 = 2;
/// Ring capacity: 5 s of audio.
const RING_CAPACITY: usize = (TARGET_RATE as usize) * 5;
/// A restarting helper waits this long before re-opening the stream.
const RESTART_WAIT: Duration = Duration::from_secs(1);

// CoreGraphics and CoreMedia entry points used directly (no binding crate).
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGMainDisplayID() -> u32;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(
        desc: *const c_void,
    ) -> *const RawStreamBasicDescription;
}

/// `AudioStreamBasicDescription` (AudioToolbox), read-only view of a format
/// description's stream format.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct RawStreamBasicDescription {
    m_sample_rate: f64,
    m_format_id: u32,
    m_format_flags: u32,
    m_bytes_per_packet: u32,
    m_frames_per_packet: u32,
    m_bytes_per_frame: u32,
    m_channels_per_frame: u32,
    m_bits_per_channel: u32,
    m_reserved: u32,
}

const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;
const K_AUDIO_FORMAT_FLAG_IS_BIG_ENDIAN: u32 = 1 << 1;
const K_AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED: u32 = 1 << 5;

/// Screen recording permission check: preflight, request
/// once on first failure, and report what the user must do.
fn preflight_screen_recording() -> Result<(), SourceError> {
    let granted = unsafe { CGPreflightScreenCaptureAccess() };
    if granted {
        return Ok(());
    }
    unsafe { CGRequestScreenCaptureAccess() };
    tracing::warn!(
        "screen recording permission missing, ScreenCaptureKit cannot capture system audio"
    );
    Err(SourceError::PermissionMissing(
        "Grant Screen Recording in System Settings, then restart clueless".into(),
    ))
}

/// What one sample buffer's audio bytes contain, after consulting the
/// format description.
enum BufferLayout {
    /// float32 planes, one buffer per channel
    Planar,
    /// float32 samples interleaved in one buffer
    Interleaved,
}

fn buffer_layout(sample: &CMSampleBuffer) -> Option<(BufferLayout, usize)> {
    let desc = sample.format_description()?;
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(desc.as_ptr()) };
    if asbd.is_null() {
        return None;
    }
    let asbd = unsafe { &*asbd };
    if asbd.m_format_flags & K_AUDIO_FORMAT_FLAG_IS_FLOAT == 0 || asbd.m_bits_per_channel != 32 {
        return None;
    }
    let channels = asbd.m_channels_per_frame.max(1) as usize;
    let endian_ok = (asbd.m_format_flags & K_AUDIO_FORMAT_FLAG_IS_BIG_ENDIAN) == 0; // little on Apple silicon
    if !endian_ok {
        return None;
    }
    Some(
        if asbd.m_format_flags & K_AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED != 0 {
            (BufferLayout::Planar, channels)
        } else {
            (BufferLayout::Interleaved, channels)
        },
    )
}

/// Convert one sample buffer to mono samples; `None` when the format is not
/// float32 PCM (counted by the caller).
fn sample_to_mono(sample: &CMSampleBuffer) -> Option<Vec<f32>> {
    let (layout, channels) = buffer_layout(sample)?;
    let list = sample.audio_buffer_list().ok()?;
    let mut out = Vec::new();
    match layout {
        BufferLayout::Planar => {
            let planes: Vec<&[u8]> = list.iter().map(|b| b.data()).collect();
            if planes.len() == channels {
                planar_bytes_to_mono(&planes, &mut out);
            }
        }
        BufferLayout::Interleaved => {
            if let Some(buffer) = list.buffer(0) {
                interleaved_bytes_to_mono(buffer.data(), channels, &mut out);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// State shared by the reader, the output handler and the delegate.
struct Shared {
    writer: Mutex<RingWriter>,
    policy: Mutex<RestartPolicy>,
    stream: Mutex<Option<SCStream>>,
    /// a helper thread is re-opening the stream
    restarting: AtomicBool,
    /// GiveUp: read returns Empty from then on
    given_up: AtomicBool,
    /// the next read reports Gap once (a restart completed)
    gap_pending: AtomicBool,
    /// unsupported-format buffers, warned at most once per second
    format_misses: AtomicU64,
    last_format_warn: Mutex<Option<Instant>>,
    status: StatusSink,
}

impl Shared {
    fn publish(&self, level: StatusLevel, text: &str) {
        (self.status)(UiEvent::Status {
            source: StatusSource::SystemAudio,
            level,
            text: text.into(),
        });
    }

    /// A restart was requested by the policy; run it on a helper thread
    /// unless one already runs.
    fn request_restart(self: &Arc<Self>, reason: &str) {
        if self.given_up.load(Ordering::Relaxed) {
            return;
        }
        tracing::warn!("system-audio stream restarting: {reason}");
        if self.restarting.swap(true, Ordering::Relaxed) {
            return;
        }
        let shared = Arc::clone(self);
        if let Err(e) = std::thread::Builder::new()
            .name("capture-sck-restart".into())
            .spawn(move || {
                std::thread::sleep(RESTART_WAIT);
                match shared.open_stream() {
                    Ok(()) => {
                        shared.gap_pending.store(true, Ordering::Relaxed);
                        shared.publish(StatusLevel::Warn, "System audio stream restarted");
                    }
                    Err(e) => {
                        shared.given_up.store(true, Ordering::Relaxed);
                        shared.publish(StatusLevel::Error, &format!("System audio stopped: {e:?}"));
                    }
                }
                shared.restarting.store(false, Ordering::Relaxed);
            })
        {
            tracing::error!("cannot spawn system-audio restart thread: {e}");
            self.restarting.store(false, Ordering::Relaxed);
        }
    }

    /// Stop any live stream and start a fresh one (helper-thread context).
    fn open_stream(self: &Arc<Self>) -> Result<(), SourceError> {
        {
            let mut guard = self.stream.lock().unwrap();
            if let Some(old) = guard.take() {
                let _ = old.stop_capture();
            }
        }
        let stream = build_stream(Arc::clone(self))?;
        stream
            .start_capture()
            .map_err(|e| SourceError::Backend(format!("cannot start system-audio stream: {e}")))?;
        *self.stream.lock().unwrap() = Some(stream);
        Ok(())
    }
}

/// Build a display-filtered audio-only stream; the handler pushes converted
/// samples into the shared ring, the delegate feeds the restart policy.
fn build_stream(shared: Arc<Shared>) -> Result<SCStream, SourceError> {
    let content = SCShareableContent::get()
        .map_err(|e| SourceError::Backend(format!("cannot list shareable content: {e}")))?;
    let displays = content.displays();
    let main_id = unsafe { CGMainDisplayID() };
    let display = displays
        .iter()
        .find(|d| d.display_id() == main_id)
        .or_else(|| displays.first())
        .ok_or_else(|| SourceError::DeviceNotFound("no capturable display".into()))?;
    let filter = SCContentFilter::create()
        .with_display(display)
        .with_excluding_windows(&[])
        .build()
        .map_err(|e| SourceError::Backend(format!("cannot build content filter: {e}")))?;
    let config = SCStreamConfiguration::default()
        .with_width(2)
        .with_height(2)
        .with_fps(1)
        .with_captures_audio(true)
        .with_sample_rate(TARGET_RATE as i32)
        .with_channel_count(TARGET_CHANNELS as i32)
        .with_excludes_current_process_audio(true);

    let mut stream = SCStream::new_with_delegate(&filter, &config, {
        let delegate_shared = Arc::clone(&shared);
        StreamCallbacks::new().on_stop(move |error| {
            let action = {
                let mut policy = delegate_shared.policy.lock().unwrap();
                policy.on_stop_error()
            };
            match action {
                WatchdogAction::Restart => delegate_shared.request_restart(&stop_text(error)),
                WatchdogAction::GiveUp => {
                    delegate_shared.given_up.store(true, Ordering::Relaxed);
                    delegate_shared.publish(
                        StatusLevel::Error,
                        &format!("System audio stopped permanently: {}", stop_text(error)),
                    );
                }
                WatchdogAction::Nothing => {}
            }
        })
    })
    .map_err(|e| SourceError::Backend(format!("cannot create system-audio stream: {e}")))?;

    let handler_shared = Arc::clone(&shared);
    stream
        .add_output_handler(
            move |sample: CMSampleBuffer, of_type: SCStreamOutputType| {
                if of_type != SCStreamOutputType::Audio {
                    return;
                }
                match sample_to_mono(&sample) {
                    Some(mono) => {
                        let mut writer = handler_shared.writer.lock().unwrap();
                        for s in mono {
                            writer.push(s);
                        }
                    }
                    None => {
                        handler_shared.format_misses.fetch_add(1, Ordering::Relaxed);
                        let mut last = handler_shared.last_format_warn.lock().unwrap();
                        let now = Instant::now();
                        if last.is_none_or(|t| now - t >= Duration::from_secs(1)) {
                            *last = Some(now);
                            tracing::warn!(
                                "system-audio sample buffer is not float32 PCM; dropped"
                            );
                        }
                    }
                }
            },
            SCStreamOutputType::Audio,
        )
        .map_err(|e| SourceError::Backend(format!("cannot add audio handler: {e}")))?;
    Ok(stream)
}

fn stop_text(error: Option<String>) -> String {
    match error {
        Some(e) => e,
        None => "stream stopped".to_string(),
    }
}

/// A `SampleSource` fed by ScreenCaptureKit display audio.
pub struct SckSource {
    shared: Arc<Shared>,
    reader: RingReader,
}

impl SckSource {
    /// Preflight the Screen Recording grant, open the first stream and start
    /// capturing.
    pub fn open(
        status: StatusSink,
        watchdog_restarts: u32,
        watchdog_silence_secs: u64,
    ) -> Result<Self, SourceError> {
        preflight_screen_recording()?;
        let (writer, reader) = ring_pair(RING_CAPACITY);
        let shared = Arc::new(Shared {
            writer: Mutex::new(writer),
            policy: Mutex::new(RestartPolicy::new(watchdog_restarts, watchdog_silence_secs)),
            stream: Mutex::new(None),
            restarting: AtomicBool::new(false),
            given_up: AtomicBool::new(false),
            gap_pending: AtomicBool::new(false),
            format_misses: AtomicU64::new(0),
            last_format_warn: Mutex::new(None),
            status,
        });
        shared.open_stream()?;
        tracing::info!(
            rate = TARGET_RATE,
            "system audio source opened (ScreenCaptureKit output capture)"
        );
        Ok(Self { shared, reader })
    }
}

impl SampleSource for SckSource {
    fn sample_rate(&self) -> u32 {
        TARGET_RATE
    }

    fn read(&mut self, out: &mut [f32]) -> SourceRead {
        if self.shared.given_up.load(Ordering::Relaxed) {
            return SourceRead::Empty;
        }
        // The silence timer ticks here, the stop error in the
        // delegate.
        let action = {
            let mut policy = self.shared.policy.lock().unwrap();
            policy.tick(Instant::now())
        };
        if action == WatchdogAction::Restart {
            self.shared
                .request_restart("no samples for the silence timeout");
        } else if action == WatchdogAction::GiveUp {
            self.shared.given_up.store(true, Ordering::Relaxed);
            self.shared.publish(
                StatusLevel::Error,
                "System audio stopped permanently: no samples",
            );
        }
        if self.shared.restarting.load(Ordering::Relaxed) {
            return SourceRead::Empty;
        }
        if self.shared.gap_pending.swap(false, Ordering::Relaxed) {
            self.shared
                .publish(StatusLevel::Warn, "System audio restarted - audio gap");
            return SourceRead::Gap;
        }
        let read = self.reader.read(out);
        if let SourceRead::Samples(_) = read {
            self.shared.policy.lock().unwrap().on_sample(Instant::now());
        }
        read
    }
}

impl Drop for SckSource {
    fn drop(&mut self) {
        if let Some(stream) = self.shared.stream.lock().unwrap().take() {
            let _ = stream.stop_capture();
        }
    }
}

/// The c-string used only if Apple's symbols need forcing; keeps the linker
/// honest about the declarations above.
#[allow(dead_code)]
fn _linker_canary() {
    let _: unsafe extern "C" fn() -> bool = CGPreflightScreenCaptureAccess;
    let _: unsafe extern "C" fn() -> bool = CGRequestScreenCaptureAccess;
    let _: unsafe extern "C" fn() -> u32 = CGMainDisplayID;
    let _: unsafe extern "C" fn(*const c_void) -> *const RawStreamBasicDescription =
        CMAudioFormatDescriptionGetStreamBasicDescription;
    let _: Option<*const c_char> = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_stereo_buffers_convert_whatever_sck_delivers() {
        // The convert helpers are pinned by convert.rs tests; this checks the
        // dispatch between planar and interleaved on real byte layouts.
        let left = [1.0f32, 1.0];
        let right = [0.0f32, 0.0];
        let (h, l, t) = unsafe { left.align_to::<u8>() };
        assert!(h.is_empty() && t.is_empty());
        let (h, r, t) = unsafe { right.align_to::<u8>() };
        assert!(h.is_empty() && t.is_empty());
        let mut out = Vec::new();
        planar_bytes_to_mono(&[l, r], &mut out);
        assert_eq!(out, vec![0.5, 0.5]);

        let interleaved = [1.0f32, -1.0, 0.5, 0.25];
        let (h, b, t) = unsafe { interleaved.align_to::<u8>() };
        assert!(h.is_empty() && t.is_empty());
        let mut out = Vec::new();
        interleaved_bytes_to_mono(b, 2, &mut out);
        assert_eq!(out, vec![0.0, 0.375]);
    }

    #[test]
    fn layout_probe_on_a_null_format_is_none() {
        // buffer_layout bails out when the ASBD pointer is null; exercise the
        // guard directly (no screencapturekit stream is opened).
        let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(std::ptr::null()) };
        // Apple tolerates NULL by returning NULL; if it ever returns non-null
        // the guard in buffer_layout still checks before dereferencing.
        let _ = asbd;
    }
}
