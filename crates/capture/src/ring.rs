//! Lock-free ring buffering between real-time audio callbacks and consumers.
//!
//! Audio callbacks must never block, so capture sources push samples one at a
//! time into an `rtrb` single-producer/single-consumer ring; the engine's
//! stream threads drain it. When the consumer falls behind, pushes hit a full
//! ring, are dropped, and increment an atomic counter; the next `read` reports
//! `Gap` exactly once after the counter changed so the consumer re-anchors
//! instead of hallucinating words across the discontinuity.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clueless_types::SourceRead;
use rtrb::{Consumer, Producer, RingBuffer};

/// Producer half, held by the audio callback (behind a `Mutex` for the
/// ScreenCaptureKit handler, plain for the cpal callback which owns it).
pub struct RingWriter {
    producer: Producer<f32>,
    dropped: Arc<AtomicU64>,
}

/// Consumer half, held by the side calling [`RingReader::read`].
pub struct RingReader {
    consumer: Consumer<f32>,
    dropped: Arc<AtomicU64>,
    seen: u64,
}

/// Create a connected writer/reader pair over a ring of `capacity` samples.
pub fn ring_pair(capacity: usize) -> (RingWriter, RingReader) {
    ring_pair_with(capacity, Arc::new(AtomicU64::new(0)))
}

/// Like [`ring_pair`] but shares an existing drop counter, so a stream
/// rebuild can keep counting into the same total.
pub fn ring_pair_with(capacity: usize, dropped: Arc<AtomicU64>) -> (RingWriter, RingReader) {
    let (producer, consumer) = RingBuffer::<f32>::new(capacity.max(1));
    (
        RingWriter {
            producer,
            dropped: Arc::clone(&dropped),
        },
        RingReader {
            consumer,
            dropped,
            seen: 0,
        },
    )
}

impl RingWriter {
    /// Push one mono sample; when the ring is full the sample is dropped and
    /// the shared counter increases. Never blocks, allocates or locks.
    pub fn push(&mut self, sample: f32) {
        if self.producer.push(sample).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The shared drop counter.
    pub fn dropped(&self) -> &Arc<AtomicU64> {
        &self.dropped
    }
}

impl RingReader {
    /// Wrap an existing consumer half, counting drops into `dropped`.
    pub fn new(consumer: Consumer<f32>, dropped: Arc<AtomicU64>) -> Self {
        let seen = dropped.load(Ordering::Relaxed);
        Self {
            consumer,
            dropped,
            seen,
        }
    }

    /// The shared drop counter.
    pub fn dropped(&self) -> &Arc<AtomicU64> {
        &self.dropped
    }

    /// Fill `out` with pending samples: `Gap` once after the drop counter
    /// changed, else `Samples(n)` or `Empty`. Never blocks.
    pub fn read(&mut self, out: &mut [f32]) -> SourceRead {
        let count = self.dropped.load(Ordering::Relaxed);
        if count != self.seen {
            self.seen = count;
            return SourceRead::Gap;
        }
        if self.consumer.slots() == 0 || out.is_empty() {
            return SourceRead::Empty;
        }
        let (popped, _remainder) = self.consumer.pop_partial_slice(out);
        SourceRead::Samples(popped.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_writer_of_capacity_8_counts_two_drops_and_surfaces_gap_once() {
        // Spec: capacity 8, 10 pushes and no reads -> drop counter 2; first
        // read returns Gap, second returns Samples(8), third returns Empty.
        let (mut w, mut r) = ring_pair(8);
        for i in 0..10 {
            w.push(i as f32);
        }
        assert_eq!(w.dropped().load(Ordering::Relaxed), 2);

        let mut buf = [0.0f32; 16];
        assert_eq!(r.read(&mut buf), SourceRead::Gap);
        match r.read(&mut buf) {
            SourceRead::Samples(n) => assert_eq!(n, 8),
            other => panic!("expected Samples(8), got {other:?}"),
        }
        assert_eq!(r.read(&mut buf), SourceRead::Empty);
    }

    #[test]
    fn a_second_overflow_episode_reports_a_second_gap() {
        let (mut w, mut r) = ring_pair(4);
        w.push(1.0);
        let mut buf = [0.0f32; 8];
        assert_eq!(r.read(&mut buf), SourceRead::Samples(1));
        for i in 0..8 {
            w.push(i as f32); // overflows
        }
        assert_eq!(r.read(&mut buf), SourceRead::Gap);
        assert_eq!(r.read(&mut buf), SourceRead::Samples(4));
        assert_eq!(r.read(&mut buf), SourceRead::Empty);
    }
}
