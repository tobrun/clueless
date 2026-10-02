//! Utterance state machine: turns per-frame speech probabilities into segments.
//!
//! The machine takes one speech probability per 32 ms frame (512 samples at
//! 16 kHz) plus the meeting-clock time at which the frame starts, and emits
//! [`Segment`] values: [`SegmentKind::Interim`] while a segment runs and one
//! [`SegmentKind::Final`] per utterance. Audio for the `pcm` field is fed in
//! with [`Machine::push_audio`]; frames that were never fed read as silence,
//! so tests and callers that only care about timing can use [`Machine::push`]
//! alone.

use clueless_types::config::VadConfig;
use clueless_types::{Segment, SegmentKind, Speaker, UtteranceId};
use std::collections::VecDeque;

/// Samples in one VAD frame at 16 kHz (32 ms).
pub const FRAME_SAMPLES: usize = 512;
/// Duration of one VAD frame in milliseconds.
pub const FRAME_MS: u64 = 32;

/// Consecutive frames at or above `start_threshold` needed to start speech.
const START_RUN_FRAMES: u64 = 3;
/// Frames of audio taken before the first speech frame of a segment.
const PRE_ROLL_FRAMES: u64 = 10;
/// Frames after the last frame at or above `end_threshold` included in a segment.
const END_TAIL_FRAMES: u64 = 6;
/// Frames at or above `start_threshold` a segment needs to be kept.
const MIN_SPEECH_FRAMES: u32 = 8;
/// An `Interim` is emitted every this many frames after a segment's first frame.
const INTERIM_PERIOD_FRAMES: u64 = 62;
/// An `Interim` carries at most this many frames of audio (10 s at whole frames).
const INTERIM_MAX_FRAMES: u64 = 160_000 / FRAME_SAMPLES as u64;
/// Frames searched for the lowest probability at a forced cut.
const FORCED_CUT_WINDOW_FRAMES: u64 = 62;
/// Frames of audio (1.0 s) carried into the next piece across a forced cut.
const OVERLAP_CARRY_FRAMES: u64 = 31;
/// How many past frames of probabilities the machine remembers.
const HISTORY_FRAMES: usize = 128;

/// Thresholds and limits of the state machine; defaults are the spec table.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineParams {
    pub start_threshold: f32,
    pub end_threshold: f32,
    pub end_silence_frames: u32,
    pub max_segment_ms: u64,
}

impl Default for MachineParams {
    fn default() -> Self {
        Self {
            start_threshold: 0.5,
            end_threshold: 0.35,
            end_silence_frames: 19,
            max_segment_ms: 15_000,
        }
    }
}

impl From<&VadConfig> for MachineParams {
    fn from(c: &VadConfig) -> Self {
        Self {
            start_threshold: c.start_threshold,
            end_threshold: c.end_threshold,
            end_silence_frames: c.end_silence_frames as u32,
            max_segment_ms: c.max_segment_ms,
        }
    }
}

#[derive(Debug)]
struct OpenSegment {
    /// First frame of the segment, including pre-roll.
    start_frame: u64,
    /// Meeting clock at the start of `start_frame`.
    t0_ms: u64,
    overlaps_prev: bool,
    /// Frames at or above `start_threshold` counted in this segment.
    speech_frames: u32,
    /// Consecutive frames below `end_threshold` ending at the current frame.
    silence_run: u32,
    /// Latest frame seen at or above `end_threshold`.
    last_above_end_frame: u64,
    /// Latest frame pushed into this segment.
    last_frame: u64,
}

/// One stream's utterance state machine. Create one per stream per meeting.
#[derive(Debug)]
pub struct Machine {
    speaker: Speaker,
    params: MachineParams,
    /// Next `seq` to hand out; increases by one per `Final`.
    seq: u64,
    /// A new segment never starts before this frame (end of the previous Final).
    floor_frame: u64,
    /// Consecutive frames at or above `start_threshold` while no segment is open.
    start_run: u32,
    open: Option<OpenSegment>,
    /// Recent (frame, probability) pairs, oldest first, at most `HISTORY_FRAMES`.
    history: VecDeque<(u64, f32)>,
    /// Fed audio, and the absolute sample index of `audio[0]`.
    audio: VecDeque<f32>,
    audio_base: u64,
}

impl Machine {
    pub fn new(speaker: Speaker, params: MachineParams) -> Self {
        Self {
            speaker,
            params,
            seq: 0,
            floor_frame: 0,
            start_run: 0,
            open: None,
            history: VecDeque::new(),
            audio: VecDeque::new(),
            audio_base: 0,
        }
    }

    /// The `seq` the next `Final` will carry.
    pub fn next_seq(&self) -> u64 {
        self.seq
    }

    /// Feed 16 kHz mono audio for frame `frame` onward, in frame order.
    /// Frames skipped over read as silence; audio is frame-aligned.
    pub fn push_audio(&mut self, frame: u64, samples: &[f32]) {
        let start = frame * FRAME_SAMPLES as u64;
        let end = self.audio_base + self.audio.len() as u64;
        if start > end {
            self.audio
                .extend(std::iter::repeat_n(0.0, (start - end) as usize));
        }
        self.audio.extend(samples);
        let cap_frames = self.params.max_segment_ms / FRAME_MS + HISTORY_FRAMES as u64 + 64;
        while (self.audio.len() as u64) > cap_frames * FRAME_SAMPLES as u64 {
            self.audio.pop_front();
            self.audio_base += 1;
        }
    }

    /// Push one frame's speech probability. Returns the segments emitted at
    /// this frame: at most one `Interim`, or the `Final` that closes a segment.
    pub fn push(&mut self, prob: f32, frame: u64, t_frame_start_ms: u64) -> Vec<Segment> {
        self.history.push_back((frame, prob));
        if self.history.len() > HISTORY_FRAMES {
            self.history.pop_front();
        }
        let above_start = prob >= self.params.start_threshold;
        let above_end = prob >= self.params.end_threshold;

        if self.open.is_none() {
            self.start_run = if above_start { self.start_run + 1 } else { 0 };
            if self.start_run as u64 >= START_RUN_FRAMES {
                let first_speech = frame + 1 - START_RUN_FRAMES;
                let start = first_speech
                    .saturating_sub(PRE_ROLL_FRAMES)
                    .max(self.floor_frame);
                let t0 = t_frame_start_ms.saturating_sub((frame - start) * FRAME_MS);
                self.open = Some(OpenSegment {
                    start_frame: start,
                    t0_ms: t0,
                    overlaps_prev: false,
                    speech_frames: START_RUN_FRAMES as u32,
                    silence_run: 0,
                    last_above_end_frame: frame,
                    last_frame: frame,
                });
                self.start_run = 0;
            }
            return Vec::new();
        }

        {
            let seg = self.open.as_mut().expect("open checked above");
            seg.last_frame = frame;
            if above_start {
                seg.speech_frames += 1;
            }
            if above_end {
                seg.silence_run = 0;
                seg.last_above_end_frame = frame;
            } else {
                seg.silence_run += 1;
            }
        }

        let mut out = Vec::new();
        let reached_max = {
            let seg = self.open.as_ref().expect("open checked above");
            t_frame_start_ms.saturating_add(FRAME_MS)
                >= seg.t0_ms.saturating_add(self.params.max_segment_ms)
        };
        if reached_max {
            out.extend(self.forced_cut(frame, t_frame_start_ms));
        }

        let snap = self.open.as_ref().map(|s| {
            (
                s.start_frame,
                s.t0_ms,
                s.overlaps_prev,
                s.silence_run,
                s.last_above_end_frame,
                s.speech_frames,
                s.last_frame,
            )
        });
        let Some((start, t0, overlaps, silence_run, last_above, speech, last_frame)) = snap else {
            return out;
        };

        if silence_run >= self.params.end_silence_frames {
            let end = (last_above + END_TAIL_FRAMES).min(last_frame);
            if speech >= MIN_SPEECH_FRAMES {
                out.push(self.emit_final(start, end, t0, overlaps));
                self.floor_frame = end + 1;
            }
            self.open = None;
        } else if (frame - start + 1).is_multiple_of(INTERIM_PERIOD_FRAMES) {
            let elapsed = frame - start + 1;
            let w0 = start + (elapsed - elapsed.min(INTERIM_MAX_FRAMES));
            let wt0 = t0 + (w0 - start) * FRAME_MS;
            out.push(self.make_segment(SegmentKind::Interim, wt0, w0, frame, overlaps));
        }
        out
    }

    /// Meeting stop: close the open segment as a `Final` if it passes the
    /// minimum-speech rule, then reset the detector state.
    pub fn flush(&mut self) -> Option<Segment> {
        self.close_segment()
    }

    /// A `Gap` or `Reset` from the source, an `Empty` longer than 200 ms, or a
    /// meeting stop: close the open segment as a `Final` if it passes the
    /// minimum-speech rule, then reset the detector state. `seq` and the audio
    /// history carry over; the caller re-anchors `t_frame_start_ms`.
    pub fn close_segment(&mut self) -> Option<Segment> {
        let seg = self.open.take()?;
        self.start_run = 0;
        let end = (seg.last_above_end_frame + END_TAIL_FRAMES).min(seg.last_frame);
        if seg.speech_frames < MIN_SPEECH_FRAMES {
            return None;
        }
        let out = self.emit_final(seg.start_frame, end, seg.t0_ms, seg.overlaps_prev);
        self.floor_frame = end + 1;
        Some(out)
    }

    fn forced_cut(&mut self, frame: u64, t_frame_start_ms: u64) -> Vec<Segment> {
        let old = match self.open.take() {
            Some(old) => old,
            None => return Vec::new(),
        };
        // lowest-probability frame of the segment's last 62 frames, latest on a tie
        let window_start = frame.saturating_sub(FORCED_CUT_WINDOW_FRAMES - 1);
        let mut best: Option<(u64, f32)> = None;
        for &(f, p) in &self.history {
            // `<=` keeps the latest frame when probabilities tie
            if f >= window_start && f <= frame && best.is_none_or(|(_, bp)| p <= bp) {
                best = Some((f, p));
            }
        }
        let (cut_frame, cut_prob) = best.unwrap_or((frame, 0.0));

        let mut out = Vec::new();
        let after_cut = self.count_above_start(cut_frame + 1, old.last_frame);
        let piece_speech = old.speech_frames.saturating_sub(after_cut);
        if piece_speech >= MIN_SPEECH_FRAMES {
            out.push(self.emit_final(old.start_frame, cut_frame, old.t0_ms, old.overlaps_prev));
            self.floor_frame = cut_frame + 1;
        }

        // carry 1.0 s into the next piece only when the cut frame is still speech
        let overlap = cut_prob >= self.params.end_threshold;
        let next_start = if overlap {
            (cut_frame + 1).saturating_sub(OVERLAP_CARRY_FRAMES)
        } else {
            cut_frame + 1
        };
        let speech_frames = self.count_above_start(next_start, frame);
        let (silence_run, last_above) = self.trailing_silence_state(next_start, frame);
        self.open = Some(OpenSegment {
            start_frame: next_start,
            t0_ms: t_frame_start_ms.saturating_sub((frame - next_start) * FRAME_MS),
            overlaps_prev: overlap,
            speech_frames,
            silence_run,
            last_above_end_frame: last_above,
            last_frame: frame,
        });
        out
    }

    /// Frames at or above `start_threshold` in `[lo, hi]`, from the history.
    fn count_above_start(&self, lo: u64, hi: u64) -> u32 {
        self.history
            .iter()
            .filter(|&&(f, p)| f >= lo && f <= hi && p >= self.params.start_threshold)
            .count() as u32
    }

    /// `(silence_run, last_above_end_frame)` of `[lo, hi]` ending at `hi`,
    /// scanning the history backwards; `lo` itself counts when in range.
    fn trailing_silence_state(&self, lo: u64, hi: u64) -> (u32, u64) {
        for &(f, p) in self.history.iter().rev() {
            if f > hi {
                continue;
            }
            if f < lo {
                break;
            }
            if p >= self.params.end_threshold {
                return (0, f);
            }
        }
        ((hi - lo + 1) as u32, lo)
    }

    fn emit_final(&mut self, start: u64, end: u64, t0_ms: u64, overlaps_prev: bool) -> Segment {
        let out = self.make_segment(SegmentKind::Final, t0_ms, start, end, overlaps_prev);
        self.seq += 1;
        out
    }

    fn make_segment(
        &self,
        kind: SegmentKind,
        t0_ms: u64,
        start_frame: u64,
        end_frame: u64,
        overlaps_prev: bool,
    ) -> Segment {
        let frames = end_frame - start_frame + 1;
        let base_sample = start_frame * FRAME_SAMPLES as u64;
        let mut pcm = Vec::with_capacity((frames as usize) * FRAME_SAMPLES);
        for i in 0..frames * FRAME_SAMPLES as u64 {
            pcm.push(self.sample_at(base_sample + i));
        }
        Segment {
            id: UtteranceId {
                speaker: self.speaker,
                seq: self.seq,
            },
            kind,
            t0_ms,
            t1_ms: t0_ms + frames * FRAME_MS,
            pcm,
            overlaps_prev,
        }
    }

    /// Sample at an absolute 16 kHz sample index; unfed audio reads as silence.
    fn sample_at(&self, sample: u64) -> f32 {
        if sample < self.audio_base {
            return 0.0;
        }
        let idx = (sample - self.audio_base) as usize;
        self.audio.get(idx).copied().unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds frames with increasing indices; each frame's audio samples all
    /// carry the value `frame + 100`, so a segment's first and last sample
    /// value proves exactly which frames its audio covers.
    struct Sim {
        m: Machine,
        next_frame: u64,
        emitted: Vec<(u64, Segment)>,
    }

    impl Sim {
        fn new() -> Self {
            Self {
                m: Machine::new(Speaker::Me, MachineParams::default()),
                next_frame: 0,
                emitted: Vec::new(),
            }
        }

        fn frame(&mut self, prob: f32) {
            let f = self.next_frame;
            self.next_frame += 1;
            self.m.push_audio(f, &[f as f32 + 100.0; FRAME_SAMPLES]);
            for s in self.m.push(prob, f, f * FRAME_MS) {
                self.emitted.push((f, s));
            }
        }

        fn run(&mut self, prob: f32, count: u64) {
            for _ in 0..count {
                self.frame(prob);
            }
        }

        fn finish(&mut self) -> Option<Segment> {
            self.m.flush()
        }

        fn finals(&self) -> Vec<&Segment> {
            self.emitted
                .iter()
                .map(|(_, s)| s)
                .filter(|s| s.kind == SegmentKind::Final)
                .collect()
        }

        fn interims(&self) -> Vec<&Segment> {
            self.emitted
                .iter()
                .map(|(_, s)| s)
                .filter(|s| s.kind == SegmentKind::Interim)
                .collect()
        }
    }

    fn assert_audio_covers(seg: &Segment, first_frame: u64, last_frame: u64) {
        let frames = last_frame - first_frame + 1;
        assert_eq!(seg.pcm.len(), frames as usize * FRAME_SAMPLES);
        assert_eq!(seg.pcm.first().copied(), Some(first_frame as f32 + 100.0));
        assert_eq!(seg.pcm.last().copied(), Some(last_frame as f32 + 100.0));
    }

    #[test]
    fn two_speech_frames_then_silence_produce_no_segment() {
        let mut sim = Sim::new();
        sim.run(0.9, 2);
        sim.run(0.0, 40);
        assert!(sim.emitted.is_empty());
        assert!(sim.finish().is_none());
    }

    #[test]
    fn final_audio_covers_pre_roll_and_six_frame_tail() {
        let mut sim = Sim::new();
        sim.run(0.0, 12);
        sim.run(0.9, 23); // frames 12..=34
        sim.run(0.1, 19); // ends at frame 53
        let finals = sim.finals();
        assert_eq!(finals.len(), 1);
        let f = finals[0];
        assert_eq!(
            f.id,
            UtteranceId {
                speaker: Speaker::Me,
                seq: 0
            }
        );
        // starts 10 frames before frame 12, ends 6 frames after frame 34
        assert_audio_covers(f, 2, 40);
        assert_eq!(f.t0_ms, 2 * FRAME_MS);
        assert_eq!(f.t1_ms, 41 * FRAME_MS);
        assert!(!f.overlaps_prev);
    }

    #[test]
    fn speech_at_frame_two_rolls_pre_roll_back_to_frame_zero() {
        let mut sim = Sim::new();
        sim.run(0.0, 2);
        sim.run(0.9, 10); // speech starts at frame 2
        sim.run(0.1, 19);
        let finals = sim.finals();
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0].t0_ms, 0);
        assert_audio_covers(finals[0], 0, 17);
    }

    #[test]
    fn utterances_five_silent_frames_apart_merge_into_one_final() {
        let mut sim = Sim::new();
        sim.run(0.0, 5);
        sim.run(0.9, 10); // 5..=14
        sim.run(0.1, 5); // 15..=19, fewer than 19 silent frames
        sim.run(0.9, 10); // 20..=29
        sim.run(0.1, 19); // 30..=48
        let finals = sim.finals();
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0].id.seq, 0);
        assert_audio_covers(finals[0], 0, 35);
    }

    #[test]
    fn frames_between_thresholds_reset_the_silence_count_and_keep_the_segment() {
        let mut sim = Sim::new();
        sim.run(0.9, 5); // 0..=4
        sim.run(0.4, 3); // 5..=7, between 0.35 and 0.5
        assert!(sim.emitted.is_empty());
        sim.run(0.9, 5); // 8..=12
        assert!(sim.emitted.is_empty());
        sim.run(0.1, 19); // 13..=31
        let finals = sim.finals();
        assert_eq!(finals.len(), 1);
        assert_audio_covers(finals[0], 0, 18);
    }

    #[test]
    fn eighteen_silent_frames_continue_and_nineteen_end_the_segment() {
        let mut sim = Sim::new();
        sim.run(0.9, 9); // 0..=8
        sim.run(0.1, 18); // 9..=26
        assert!(sim.emitted.is_empty());
        sim.frame(0.1); // frame 27, the nineteenth
        let finals = sim.finals();
        assert_eq!(finals.len(), 1);
        assert_audio_covers(finals[0], 0, 14); // 8 + 6 tail
        assert_eq!(finals[0].t1_ms, 15 * FRAME_MS);
    }

    #[test]
    fn segment_with_seven_speech_frames_is_discarded_and_seq_is_not_used() {
        let mut sim = Sim::new();
        sim.run(0.9, 7);
        sim.run(0.1, 19);
        assert!(sim.emitted.is_empty());
        sim.run(0.9, 10);
        sim.run(0.1, 19);
        let finals = sim.finals();
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0].id.seq, 0);
    }

    #[test]
    fn interim_emitted_every_62_frames_shares_the_final_seq() {
        let mut sim = Sim::new();
        sim.run(0.9, 200); // segment starts at frame 0 (pre-roll clamped)
        let emitted_frames: Vec<u64> = sim.emitted.iter().map(|(f, _)| *f).collect();
        assert_eq!(emitted_frames, vec![61, 123, 185]);
        let interims = sim.interims();
        assert_eq!(interims.len(), 3);
        for i in &interims {
            assert_eq!(i.kind, SegmentKind::Interim);
            assert_eq!(
                i.id,
                UtteranceId {
                    speaker: Speaker::Me,
                    seq: 0
                }
            );
        }
        // all three still fit inside 10 s, so each carries all audio so far
        assert_audio_covers(interims[0], 0, 61);
        assert_audio_covers(interims[1], 0, 123);
        assert_audio_covers(interims[2], 0, 185);
        assert_eq!(interims[0].t0_ms, 0);
        assert_eq!(interims[0].t1_ms, 62 * FRAME_MS);
        assert_eq!(interims[2].t1_ms, 186 * FRAME_MS);
        let f = sim.finish().expect("final");
        assert_eq!(f.id.seq, 0);
        assert_eq!(f.kind, SegmentKind::Final);
    }

    #[test]
    fn interim_audio_never_exceeds_ten_seconds() {
        let mut sim = Sim::new();
        sim.run(0.9, 400);
        let interims = sim.interims();
        assert_eq!(interims.len(), 6); // frames 61, 123, 185, 247, 309, 371
        for i in &interims {
            assert!(
                (i.pcm.len() as u64) <= 10 * 16_000,
                "interim at t0 {} carries {} samples",
                i.t0_ms,
                i.pcm.len()
            );
        }
        // the last interims sit exactly at the cap of whole frames in 10 s
        assert_audio_covers(interims[5], 60, 371);
        assert_eq!(interims[5].t0_ms, 60 * FRAME_MS);
        let f = sim.finish().expect("final");
        assert_audio_covers(&f, 0, 399);
        assert_eq!(f.id.seq, 0);
    }

    #[test]
    fn forced_cut_at_a_silent_frame_does_not_carry_overlap() {
        let mut sim = Sim::new();
        for f in 0..500 {
            sim.frame(if f == 450 { 0.2 } else { 0.9 });
        }
        let finals = sim.finals();
        assert_eq!(
            finals.len(),
            1,
            "the meeting stop flush gives the second final"
        );
        let a = finals[0].clone();
        assert_eq!(a.id.seq, 0);
        assert!(!a.overlaps_prev);
        // cut at the only low-probability frame inside the last 62 at 15000 ms
        assert_audio_covers(&a, 0, 450);
        assert_eq!(a.t1_ms, 451 * FRAME_MS);
        let b = sim.finish().expect("flush final");
        assert_eq!(b.id.seq, 1);
        assert!(!b.overlaps_prev);
        // the next piece starts right at the cut, no audio lost or doubled
        assert_eq!(b.t0_ms, a.t1_ms);
        assert_audio_covers(&b, 451, 499);
    }

    #[test]
    fn forced_cut_on_speech_carries_31_frames_of_overlap() {
        let mut sim = Sim::new();
        sim.run(0.9, 500);
        let finals = sim.finals();
        assert_eq!(finals.len(), 1);
        let a = finals[0].clone();
        assert_eq!(a.id.seq, 0);
        // all probabilities tie, so the cut takes the latest frame of the window:
        // the first frame whose end reaches 15000 ms is frame 468
        assert_audio_covers(&a, 0, 468);
        assert_eq!(a.t1_ms, 469 * FRAME_MS);
        let b = sim.finish().expect("flush final");
        assert_eq!(b.id.seq, 1);
        assert!(b.overlaps_prev);
        assert_eq!(b.t0_ms, (468 + 1 - 31) * FRAME_MS);
        assert_audio_covers(&b, 438, 499);
        // the carried audio is byte-identical to the tail of piece A
        let carried = 31 * FRAME_SAMPLES;
        assert_eq!(&b.pcm[..carried], &a.pcm[a.pcm.len() - carried..]);
    }

    #[test]
    fn flush_after_eight_speech_frames_gives_one_final() {
        let mut sim = Sim::new();
        sim.run(0.9, 8);
        let f = sim.finish().expect("final");
        assert_eq!(f.kind, SegmentKind::Final);
        assert_eq!(f.id.seq, 0);
        assert_audio_covers(&f, 0, 7);
        assert_eq!(f.t1_ms, 8 * FRAME_MS);
    }

    #[test]
    fn flush_with_four_speech_frames_gives_none() {
        let mut sim = Sim::new();
        sim.run(0.9, 4);
        assert!(sim.finish().is_none());
    }

    #[test]
    fn after_flush_seq_continues_and_timestamps_come_from_the_new_clock() {
        let mut m = Machine::new(Speaker::Them, MachineParams::default());
        for f in 0..10 {
            let segs = m.push(0.9, f, f * FRAME_MS);
            assert!(segs.is_empty());
        }
        let first = m.flush().expect("final");
        assert_eq!(
            first.id,
            UtteranceId {
                speaker: Speaker::Them,
                seq: 0
            }
        );
        assert_eq!(first.t1_ms, 10 * FRAME_MS);
        // the source re-anchors: new frames, new clock base
        let base = 50_000;
        for f in 10..20 {
            assert!(m.push(0.9, f, base + (f - 10) * FRAME_MS).is_empty());
        }
        for f in 20..38 {
            assert!(m.push(0.1, f, base + (f - 10) * FRAME_MS).is_empty());
        }
        let mut segs = m.push(0.1, 38, base + (38 - 10) * FRAME_MS);
        assert_eq!(segs.len(), 1);
        let second = segs.pop().expect("final");
        assert_eq!(
            second.id,
            UtteranceId {
                speaker: Speaker::Them,
                seq: 1
            }
        );
        // pre-roll clamps to the end of the flushed segment (frame 9 + 1)
        assert_eq!(second.t0_ms, base);
        assert_eq!(second.t1_ms, base + 16 * FRAME_MS);
    }
}
