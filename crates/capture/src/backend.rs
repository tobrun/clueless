//! The live [`SourceFactory`]: what the engine opens at meeting start.
//!
//! "Me" is always a cpal input stream (default or named device).
//! "Them" follows `audio.system_audio_backend`: ScreenCaptureKit (default),
//! the cpal loopback tap on the default output device, or a named input
//! device such as BlackHole (the rationale is in `docs/decisions.md`).

use clueless_types::{
    SampleSource, SourceError, SourceFactory, Speaker, StatusSink, SystemAudioBackend,
};

use crate::mic::{CpalSource, Endpoint};
use crate::sck::SckSource;

/// Opens real microphone and system-audio sources.
pub struct LiveSources {
    backend: SystemAudioBackend,
    mic_device: Option<String>,
    watchdog_restarts: u32,
    watchdog_silence_secs: u64,
}

impl LiveSources {
    /// Take the four `audio.*` config values the sources need.
    pub fn new(
        backend: SystemAudioBackend,
        mic_device: Option<String>,
        watchdog_restarts: u32,
        watchdog_silence_secs: u64,
    ) -> Self {
        Self {
            backend,
            mic_device,
            watchdog_restarts,
            watchdog_silence_secs,
        }
    }

    fn mic_endpoint(&self) -> Endpoint {
        match &self.mic_device {
            Some(name) => Endpoint::NamedInput(name.clone()),
            None => Endpoint::DefaultInput,
        }
    }
}

impl SourceFactory for LiveSources {
    fn speakers(&self) -> Vec<Speaker> {
        // The engine opens both and copes with one failing to open:
        // a missing Screen Recording grant keeps Me working.
        vec![Speaker::Me, Speaker::Them]
    }

    fn open(
        &self,
        speaker: Speaker,
        status: StatusSink,
    ) -> Result<Box<dyn SampleSource>, SourceError> {
        match speaker {
            Speaker::Me => Ok(Box::new(CpalSource::open(
                self.mic_endpoint(),
                Speaker::Me,
                status,
            )?)),
            Speaker::Them => match &self.backend {
                SystemAudioBackend::Sck => Ok(Box::new(SckSource::open(
                    status,
                    self.watchdog_restarts,
                    self.watchdog_silence_secs,
                )?)),
                SystemAudioBackend::CpalLoopback => Ok(Box::new(CpalSource::open(
                    Endpoint::Loopback,
                    Speaker::Them,
                    status,
                )?)),
                SystemAudioBackend::Device(name) => Ok(Box::new(CpalSource::open(
                    Endpoint::NamedInput(name.clone()),
                    Speaker::Them,
                    status,
                )?)),
            },
        }
    }
}
