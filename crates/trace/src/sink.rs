//! The taps the engine records through.
//!
//! The engine holds an [`TraceOpener`] in its deps and opens a [`TraceSink`]
//! per meeting; between meetings it records nothing. The production sink
//! (change set 2) writes files; [`MemoryTrace`] serves tests and [`NoTrace`]
//! serves sessions with recording switched off.

use crate::manifest::SessionStart;
use crate::record::{Body, EndReason, Record, Speaker};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Where a recording sink writes, reported to the user at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The session directory holding manifest, records and audio.
    pub dir: PathBuf,
    /// Whether audio WAVs are written too.
    pub audio: bool,
}

/// Everything one open meeting records through. Every method is cheap and
/// never blocks the meeting on disk: implementations hand work to a writer
/// or keep it in memory.
pub trait TraceSink: Send + Sync {
    /// Stamp `body` with the next sequence number and meeting-relative
    /// time, and keep it.
    fn record(&self, body: Body);
    /// One audio frame for `speaker`, starting at `t_start_ms` on the
    /// meeting timeline.
    fn audio(&self, speaker: Speaker, t_start_ms: u64, frame: &[f32]);
    /// Milliseconds since the meeting clock started.
    fn now_ms(&self) -> u64;
    /// The id for the next LLM call, counting from 1 per meeting.
    fn next_call(&self) -> u64;
    /// Where this sink writes, or `None` when it records nothing.
    fn location(&self) -> Option<Location>;
    /// End the session: the `end` record is the last thing kept. After a
    /// close every method is a no-op.
    fn close(&self, reason: EndReason);
}

/// Called once with a message when the writer stops on an I/O error; the
/// app shows it as a warning and the meeting continues.
pub type FailureSink = Arc<dyn Fn(String) + Send + Sync>;

/// Opens the sink for one meeting. The engine calls this on the meeting
/// thread and drops the sink when the meeting ends.
pub trait TraceOpener: Send + Sync {
    /// Open a sink for a meeting that starts with `start`. The `Err`
    /// message is what the user sees when recording cannot start; the
    /// meeting proceeds either way.
    fn open(
        &self,
        start: SessionStart,
        on_failure: FailureSink,
    ) -> Result<Arc<dyn TraceSink>, String>;
}

/// The counters and clock one trace shares between its sink and its
/// writer thread: one sequence, one call counter, one meeting clock.
#[derive(Debug)]
pub struct Stamper {
    seq: AtomicU64,
    calls: AtomicU64,
    start: std::time::Instant,
}

impl Stamper {
    /// Start a fresh meeting clock; counters begin at zero and count up.
    pub fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            calls: AtomicU64::new(0),
            start: std::time::Instant::now(),
        }
    }

    /// The next sequence number, counting from 1.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The next LLM call id, counting from 1.
    pub fn next_call(&self) -> u64 {
        self.calls.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Milliseconds since this stamper was created.
    pub fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Stamp a body with the next sequence number and the clock now.
    pub fn stamp(&self, body: Body) -> Record {
        Record {
            seq: self.next_seq(),
            at_ms: self.now_ms(),
            body,
        }
    }
}

impl Default for Stamper {
    fn default() -> Self {
        Self::new()
    }
}

/// The [`TraceSink`] clock and call counter methods, for sinks that keep
/// their shared [`Stamper`] in a `stamper` field and answer both questions
/// straight from it.
macro_rules! stamper_forwarding {
    () => {
        fn now_ms(&self) -> u64 {
            self.stamper.now_ms()
        }

        fn next_call(&self) -> u64 {
            self.stamper.next_call()
        }
    };
}

/// The sink for sessions with recording switched off, and the fallback
/// when opening a trace failed: keeps nothing, reports no location.
pub struct NoTrace;

impl TraceSink for NoTrace {
    fn record(&self, _body: Body) {}
    fn audio(&self, _speaker: Speaker, _t_start_ms: u64, _frame: &[f32]) {}
    fn now_ms(&self) -> u64 {
        0
    }
    fn next_call(&self) -> u64 {
        0
    }
    fn location(&self) -> Option<Location> {
        None
    }
    fn close(&self, _reason: EndReason) {}
}

impl TraceOpener for NoTrace {
    fn open(
        &self,
        _start: SessionStart,
        _on_failure: FailureSink,
    ) -> Result<Arc<dyn TraceSink>, String> {
        Ok(Arc::new(NoTrace))
    }
}

/// A sink that keeps every stamped record in memory for tests to assert
/// on, sharing one [`Stamper`] like the disk sink does.
pub struct MemoryTrace {
    stamper: Stamper,
    records: Mutex<Vec<Record>>,
    /// Audio frame start times per speaker, in call order.
    audio_times: Mutex<std::collections::BTreeMap<Speaker, Vec<u64>>>,
    session_start: SessionStart,
    location: Option<Location>,
    closed: Mutex<Option<EndReason>>,
}

impl MemoryTrace {
    fn new(session_start: SessionStart, location: Option<Location>) -> Self {
        Self {
            stamper: Stamper::new(),
            records: Mutex::new(Vec::new()),
            audio_times: Mutex::new(std::collections::BTreeMap::new()),
            session_start,
            location,
            closed: Mutex::new(None),
        }
    }

    /// Every record kept, in the order it was stamped.
    pub fn records(&self) -> Vec<Record> {
        self.records.lock().unwrap().clone()
    }

    /// The `t_start_ms` of every audio frame kept for `speaker`.
    pub fn audio_times(&self, speaker: Speaker) -> Vec<u64> {
        self.audio_times
            .lock()
            .unwrap()
            .get(&speaker)
            .cloned()
            .unwrap_or_default()
    }

    /// The settings the trace was opened with.
    pub fn session_start(&self) -> &SessionStart {
        &self.session_start
    }

    /// The reason passed to `close`, once the session ended.
    pub fn closed(&self) -> Option<EndReason> {
        *self.closed.lock().unwrap()
    }

    /// Whether this trace has been closed.
    pub fn is_closed(&self) -> bool {
        self.closed().is_some()
    }
}

impl TraceSink for MemoryTrace {
    fn record(&self, body: Body) {
        if self.is_closed() {
            return;
        }
        let record = self.stamper.stamp(body);
        self.records.lock().unwrap().push(record);
    }

    fn audio(&self, speaker: Speaker, t_start_ms: u64, _frame: &[f32]) {
        if self.is_closed() {
            return;
        }
        self.audio_times
            .lock()
            .unwrap()
            .entry(speaker)
            .or_default()
            .push(t_start_ms);
    }

    stamper_forwarding!();

    fn location(&self) -> Option<Location> {
        self.location.clone()
    }

    fn close(&self, reason: EndReason) {
        let mut closed = self.closed.lock().unwrap();
        if closed.is_some() {
            return;
        }
        let record = self.stamper.stamp(Body::End { reason });
        self.records.lock().unwrap().push(record);
        *closed = Some(reason);
    }
}

/// An opener that hands out [`MemoryTrace`]s and keeps them all for
/// assertions, the way the disk opener hands out file-backed sinks.
pub struct MemoryOpener {
    traces: Mutex<Vec<Arc<MemoryTrace>>>,
    location: Option<Location>,
}

impl MemoryOpener {
    /// An opener whose traces report no location.
    pub fn new() -> Self {
        Self {
            traces: Mutex::new(Vec::new()),
            location: None,
        }
    }

    /// An opener whose traces report this location, as if they wrote there.
    pub fn with_location(location: Location) -> Self {
        Self {
            traces: Mutex::new(Vec::new()),
            location: Some(location),
        }
    }

    /// Every trace opened so far, oldest first.
    pub fn traces(&self) -> Vec<Arc<MemoryTrace>> {
        self.traces.lock().unwrap().clone()
    }

    /// The most recently opened trace.
    pub fn last(&self) -> Option<Arc<MemoryTrace>> {
        self.traces.lock().unwrap().last().cloned()
    }
}

impl Default for MemoryOpener {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceOpener for MemoryOpener {
    fn open(
        &self,
        start: SessionStart,
        _on_failure: FailureSink,
    ) -> Result<Arc<dyn TraceSink>, String> {
        let trace = Arc::new(MemoryTrace::new(start, self.location.clone()));
        self.traces.lock().unwrap().push(trace.clone());
        Ok(trace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::session_start;

    fn failure_sink() -> FailureSink {
        Arc::new(|_msg| {})
    }

    #[test]
    fn two_threads_record_a_thousand_each_and_every_seq_appears_once() {
        let trace = Arc::new(MemoryTrace::new(session_start(), None));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let trace = Arc::clone(&trace);
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    trace.record(Body::ClockStarted);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let records = trace.records();
        assert_eq!(records.len(), 2000);
        let mut seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
        seqs.sort_unstable();
        assert_eq!(seqs.first().copied(), Some(1));
        assert_eq!(seqs.last().copied(), Some(2000));
        seqs.dedup();
        assert_eq!(seqs.len(), 2000, "every seq 1..=2000 exactly once");
    }

    #[test]
    fn no_trace_records_nothing_and_reports_no_location() {
        let sink = NoTrace;
        sink.record(Body::ClockStarted);
        sink.audio(Speaker::Me, 0, &[0.0; 16]);
        sink.close(EndReason::Stop);
        assert_eq!(sink.location(), None);
        // and as an opener it hands out a sink that stores nothing
        let opener = NoTrace;
        let opened = opener.open(session_start(), failure_sink()).unwrap();
        opened.record(Body::ClockStarted);
        assert_eq!(opened.location(), None);
    }

    #[test]
    fn memory_trace_counts_calls_from_one() {
        let trace = MemoryTrace::new(session_start(), None);
        assert_eq!(trace.next_call(), 1);
        assert_eq!(trace.next_call(), 2);
    }

    #[test]
    fn memory_opener_hands_out_fresh_traces_starting_at_seq_one() {
        let opener = MemoryOpener::new();
        let first = opener.open(session_start(), failure_sink()).unwrap();
        first.record(Body::ClockStarted);
        let second = opener.open(session_start(), failure_sink()).unwrap();
        second.record(Body::ClockStarted);
        let traces = opener.traces();
        assert_eq!(traces.len(), 2);
        for t in traces {
            let records = t.records();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].seq, 1, "each trace starts at seq 1");
        }
    }

    #[test]
    fn closing_records_end_as_the_last_record_and_remember_the_reason() {
        let trace = MemoryTrace::new(session_start(), None);
        trace.record(Body::ClockStarted);
        trace.close(EndReason::Stop);
        assert_eq!(trace.closed(), Some(EndReason::Stop));
        let records = trace.records();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[1].body,
            Body::End {
                reason: EndReason::Stop
            }
        );
        // after close nothing more is kept
        trace.record(Body::ClockStarted);
        assert_eq!(trace.records().len(), 2);
    }

    #[test]
    fn memory_opener_can_report_a_location() {
        let opener = MemoryOpener::with_location(Location {
            dir: PathBuf::from("/tmp/where"),
            audio: true,
        });
        let sink = opener.open(session_start(), failure_sink()).unwrap();
        let location = sink.location().expect("configured");
        assert_eq!(location.dir, PathBuf::from("/tmp/where"));
        assert!(location.audio);
    }
}
