//! Sample sources: the trait the engine reads from, and the factory that opens them.

use crate::events::{Speaker, StatusSink};

/// What one `SampleSource::read` call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRead {
    /// that many mono samples written to the buffer
    Samples(usize),
    /// nothing available now; never blocks
    Empty,
    /// samples were lost or the stream restarted; caller re-anchors
    Gap,
    /// the device changed; caller rebuilds its resampler
    Reset { sample_rate: u32 },
    /// no more samples will come (replay only)
    Ended,
}

/// Mono f32 samples in [-1, 1] at `sample_rate()`.
pub trait SampleSource: Send {
    fn sample_rate(&self) -> u32;
    fn read(&mut self, out: &mut [f32]) -> SourceRead;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    PermissionMissing(String),
    DeviceNotFound(String),
    Backend(String),
}

pub trait SourceFactory: Send + Sync {
    /// Which speakers this factory can open. The engine opens only these.
    fn speakers(&self) -> Vec<Speaker>;
    fn open(
        &self,
        speaker: Speaker,
        status: StatusSink,
    ) -> Result<Box<dyn SampleSource>, SourceError>;
}
