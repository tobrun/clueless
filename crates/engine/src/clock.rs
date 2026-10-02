//! Meeting-wide and per-stream time bases.
//!
//! All event timestamps are meeting milliseconds from one
//! `MeetingClock::new()`. Each stream maps its processed 16 kHz sample count
//! onto that clock through a `StreamClock` anchor, re-anchored on the first
//! samples and after every `Gap` or `Reset`. The per-stream watermark tells
//! the echo filter how far this stream's audio time has advanced; while the
//! source returns `Empty` it follows the meeting clock, it never decreases,
//! and it becomes infinite when the source ended or failed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Monotonic millisecond clock started when a meeting starts.
#[derive(Debug, Clone, Copy)]
pub struct MeetingClock {
    start: Instant,
}

impl MeetingClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Milliseconds since this clock started.
    pub fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

impl Default for MeetingClock {
    fn default() -> Self {
        Self::new()
    }
}

/// The watermark value once a source has Ended or failed: everything the
/// stream could produce is already accounted for.
pub const WATERMARK_INFINITE: u64 = u64::MAX;

/// Maps processed 16 kHz samples to meeting milliseconds for one stream.
#[derive(Debug)]
pub struct StreamClock {
    meeting: MeetingClock,
    anchor_ms: u64,
    /// 16 kHz samples processed since the current anchor.
    fed: u64,
    anchored: bool,
    watermark: Arc<AtomicU64>,
}

impl StreamClock {
    pub fn new(meeting: MeetingClock) -> Self {
        Self {
            meeting,
            anchor_ms: 0,
            fed: 0,
            anchored: false,
            watermark: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The shared watermark this clock advances, for other threads to read.
    pub fn watermark_handle(&self) -> Arc<AtomicU64> {
        self.watermark.clone()
    }

    pub fn watermark(&self) -> u64 {
        self.watermark.load(Ordering::Relaxed)
    }

    /// True once the first samples anchored this clock.
    pub fn anchored(&self) -> bool {
        self.anchored
    }

    /// Re-anchor at an explicit meeting time (split out for tests).
    pub fn anchor_at(&mut self, anchor_ms: u64) {
        self.anchor_ms = anchor_ms;
        self.fed = 0;
        self.anchored = true;
    }

    /// Anchor on the first samples and after a `Gap` or `Reset`: the stream's
    /// audio time restarts from the wall clock now.
    pub fn anchor_now(&mut self) {
        let now = self.meeting.now_ms();
        self.anchor_at(now);
    }

    /// Meeting time of `processed` 16 kHz samples after the anchor.
    pub fn t(&self, processed_16k_samples: u64) -> u64 {
        self.anchor_ms + processed_16k_samples * 1000 / 16_000
    }

    /// Meeting time of the end of everything processed so far.
    pub fn current_t(&self) -> u64 {
        self.t(self.fed)
    }

    /// Account for freshly processed 16 kHz samples and return the new
    /// audio-time end; the watermark never decreases.
    pub fn advance(&mut self, processed_16k_samples: u64) -> u64 {
        self.fed += processed_16k_samples;
        let t = self.current_t();
        self.bump(t);
        t
    }

    /// While the source returns `Empty` the watermark follows the meeting
    /// clock, so a reader never waits on audio time that cannot advance.
    pub fn follow_wall_clock(&self) {
        let now = self.meeting.now_ms();
        self.bump(now);
    }

    /// The source Ended or failed: the watermark becomes infinite.
    pub fn set_infinite(&self) {
        self.watermark.store(WATERMARK_INFINITE, Ordering::Relaxed);
    }

    fn bump(&self, t: u64) {
        self.watermark.fetch_max(t, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_thousand_processed_samples_after_an_anchor_of_2000_ms_are_3000_ms() {
        let mut clock = StreamClock::new(MeetingClock::new());
        clock.anchor_at(2000);
        assert_eq!(clock.current_t(), 2000);
        assert_eq!(clock.advance(16_000), 3000);
        assert_eq!(clock.t(8_000), 2500);
    }

    #[test]
    fn after_a_gap_the_new_anchor_comes_from_the_meeting_clock_and_the_watermark_never_decreases() {
        let meeting = MeetingClock::new();
        let mut clock = StreamClock::new(meeting);
        std::thread::sleep(std::time::Duration::from_millis(50));
        clock.anchor_now();
        clock.advance(16_000);
        let before = clock.watermark();
        assert!(
            before >= meeting.now_ms().saturating_sub(50),
            "anchor came from the meeting clock"
        );
        // The wall clock keeps moving while a source returns Empty.
        clock.follow_wall_clock();
        let wall = clock.watermark();
        assert!(wall >= before);
        // Re-anchoring at the gap restarts audio time, but the watermark,
        // which readers observe, never moves backwards.
        clock.anchor_now();
        clock.advance(512);
        assert!(clock.watermark() >= wall);
        clock.set_infinite();
        assert_eq!(clock.watermark(), WATERMARK_INFINITE);
        clock.advance(512);
        assert_eq!(clock.watermark(), WATERMARK_INFINITE);
    }
}
