//! Trigger policy for automatic suggestion requests.
//!
//! A pure state machine: the caller reports finished speech pieces and request
//! starts and ends together with a clock value, and asks [`AutoPolicy::poll`]
//! whether to fire a request, wait until some instant, or stay idle. Nothing
//! here reads the clock or sleeps.

use std::time::{Duration, Instant};

use clueless_types::{AssistProfile, Origin, Speaker};

/// Minimum count of non-whitespace characters for a turn to be worth a request.
const MIN_CHARS: usize = 12;

/// What a profile reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trigger {
    /// Whose finished pieces start a request.
    pub speaker: Speaker,
    /// Wait until the speaker stops being busy (the turn has ended).
    pub wait_for_turn_end: bool,
    /// Multiplier on the minimum gap between automatic starts.
    pub gap_factor: u32,
}

/// The trigger of a profile; `None` for Manual.
pub fn trigger(profile: AssistProfile) -> Option<Trigger> {
    match profile {
        AssistProfile::Manual => None,
        AssistProfile::Interview => Some(Trigger {
            speaker: Speaker::Them,
            wait_for_turn_end: true,
            gap_factor: 1,
        }),
        AssistProfile::Brainstorm => Some(Trigger {
            speaker: Speaker::Me,
            wait_for_turn_end: false,
            gap_factor: 4,
        }),
    }
}

/// The durations the policy works with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyTimings {
    /// Quiet time after the last piece before a turn counts as over.
    pub turn_settle: Duration,
    /// How long a busy speaker can hold a request back, after the settle time.
    pub turn_max_wait: Duration,
    /// Floor between the starts of two automatic requests.
    pub min_gap: Duration,
    /// Pause of automatic requests after a failed request.
    pub failure_pause: Duration,
}

/// How a request ended, as far as the policy cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed,
    Cancelled,
}

/// What the caller should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Idle,
    WaitUntil(Instant),
    Fire,
}

/// True when the text has at least 12 non-whitespace characters or a question
/// mark (ASCII or full-width).
pub fn enough_text(text: &str) -> bool {
    text.contains(['?', '？']) || text.chars().filter(|c| !c.is_whitespace()).count() >= MIN_CHARS
}

/// Fire when `ready` is absent or reached; otherwise wait until it.
fn fire_or_wait(now: Instant, ready: Option<Instant>) -> Decision {
    match ready {
        Some(at) if now < at => Decision::WaitUntil(at),
        _ => Decision::Fire,
    }
}

/// The trigger state of one engine.
#[derive(Debug)]
pub struct AutoPolicy {
    profile: AssistProfile,
    timings: PolicyTimings,
    collected: String,
    waiting: bool,
    settle_deadline: Option<Instant>,
    hard_deadline: Option<Instant>,
    last_auto_start: Option<Instant>,
    running: bool,
    paused_until: Option<Instant>,
}

impl AutoPolicy {
    pub fn new(profile: AssistProfile, timings: PolicyTimings) -> Self {
        Self {
            profile,
            timings,
            collected: String::new(),
            waiting: false,
            settle_deadline: None,
            hard_deadline: None,
            last_auto_start: None,
            running: false,
            paused_until: None,
        }
    }

    /// Switch profile: drops a waiting trigger and the collected text; the gap
    /// clock, the pause and the running mark stay.
    pub fn set_profile(&mut self, profile: AssistProfile) {
        self.profile = profile;
        self.clear_waiting();
    }

    /// A speech piece of `speaker` is finished. `text` is `Some` when it was
    /// committed and `None` when it was dropped.
    pub fn piece_done(&mut self, speaker: Speaker, text: Option<&str>, now: Instant) {
        let Some(trigger) = trigger(self.profile) else {
            return;
        };
        if trigger.speaker != speaker || self.pause_active(now) {
            return;
        }
        if let Some(text) = text {
            self.collect_text(text, now);
        }
        if self.waiting {
            self.settle_deadline = Some(now + self.timings.turn_settle);
        }
    }

    /// Append committed text; start waiting once there is enough of it.
    fn collect_text(&mut self, text: &str, now: Instant) {
        self.collected.push_str(text);
        if !self.waiting && enough_text(&self.collected) {
            self.waiting = true;
            self.hard_deadline = Some(now + self.timings.turn_settle + self.timings.turn_max_wait);
        }
    }

    /// A suggestion request started.
    pub fn request_started(&mut self, origin: Origin, now: Instant) {
        self.clear_waiting();
        self.running = true;
        if origin == Origin::Auto {
            self.last_auto_start = Some(now);
        }
    }

    /// The running request ended.
    pub fn request_finished(&mut self, outcome: Outcome, now: Instant) {
        self.running = false;
        match outcome {
            Outcome::Ok => self.paused_until = None,
            Outcome::Failed => {
                self.paused_until = Some(now + self.timings.failure_pause);
                self.clear_waiting();
            }
            Outcome::Cancelled => {}
        }
    }

    /// Whether to start a request now. `trigger_speaker_busy` tells whether
    /// the trigger speaker has an open piece or pieces still being transcribed.
    pub fn poll(&self, now: Instant, trigger_speaker_busy: bool) -> Decision {
        let Some(trigger) = trigger(self.profile) else {
            return Decision::Idle;
        };
        if !self.waiting || self.running {
            return Decision::Idle;
        }
        let gate = self.gate_time(&trigger);
        if !trigger.wait_for_turn_end {
            return fire_or_wait(now, gate);
        }
        self.turn_end_decision(now, gate, trigger_speaker_busy)
    }

    /// The earliest time the gap since the last automatic start allows.
    fn gate_time(&self, trigger: &Trigger) -> Option<Instant> {
        self.last_auto_start
            .map(|start| start + self.timings.min_gap * trigger.gap_factor)
    }

    /// The decision for a trigger that waits for the end of the turn.
    fn turn_end_decision(
        &self,
        now: Instant,
        gate: Option<Instant>,
        trigger_speaker_busy: bool,
    ) -> Decision {
        let hard = self.hard_deadline.unwrap_or(now);
        if trigger_speaker_busy && now < hard {
            return Decision::WaitUntil((now + self.timings.turn_settle).min(hard));
        }
        let settle = if now >= hard {
            None
        } else {
            self.settle_deadline
        };
        fire_or_wait(now, settle.into_iter().chain(gate).max())
    }

    /// True when nothing runs and nothing waits.
    pub fn is_quiet(&self) -> bool {
        !self.running && !self.waiting
    }

    fn pause_active(&self, now: Instant) -> bool {
        self.paused_until.is_some_and(|until| now < until)
    }

    fn clear_waiting(&mut self) {
        self.collected.clear();
        self.waiting = false;
        self.settle_deadline = None;
        self.hard_deadline = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTLE: Duration = Duration::from_millis(400);
    const MAX_WAIT: Duration = Duration::from_secs(4);
    const GAP: Duration = Duration::from_secs(2);
    const PAUSE: Duration = Duration::from_secs(30);

    fn timings() -> PolicyTimings {
        PolicyTimings {
            turn_settle: SETTLE,
            turn_max_wait: MAX_WAIT,
            min_gap: GAP,
            failure_pause: PAUSE,
        }
    }

    fn policy(profile: AssistProfile) -> AutoPolicy {
        AutoPolicy::new(profile, timings())
    }

    fn secs(base: Instant, s: u64) -> Instant {
        base + Duration::from_secs(s)
    }

    fn millis(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn manual_profile_is_always_idle() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Manual);
        for i in 0..3 {
            p.piece_done(
                Speaker::Them,
                Some("how would you scale that?"),
                secs(t0, i),
            );
            assert_eq!(p.poll(secs(t0, i + 10), false), Decision::Idle);
        }
        assert!(p.is_quiet());
    }

    #[test]
    fn interview_waits_for_the_settle_time_then_fires() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.piece_done(Speaker::Them, Some("how would you scale that?"), t0);
        assert_eq!(p.poll(t0, false), Decision::WaitUntil(millis(t0, 400)));
        assert_eq!(
            p.poll(millis(t0, 399), false),
            Decision::WaitUntil(millis(t0, 400))
        );
        assert_eq!(p.poll(millis(t0, 400), false), Decision::Fire);
    }

    #[test]
    fn interview_piece_done_while_busy_waits_a_settle_time_from_now() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.piece_done(Speaker::Them, Some("how would you scale that?"), t0);
        assert_eq!(p.poll(t0, true), Decision::WaitUntil(millis(t0, 400)));
        assert_eq!(
            p.poll(millis(t0, 1000), true),
            Decision::WaitUntil(millis(t0, 1400))
        );
        assert_eq!(p.poll(millis(t0, 1500), false), Decision::Fire);
    }

    #[test]
    fn interview_busy_for_the_whole_max_wait_fires_at_the_hard_deadline() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.piece_done(Speaker::Them, Some("how would you scale that?"), t0);
        // hard deadline: t0 + 400 ms + 4 s = t0 + 4400 ms
        assert_eq!(
            p.poll(millis(t0, 4000), true),
            Decision::WaitUntil(millis(t0, 4400))
        );
        assert_eq!(
            p.poll(millis(t0, 4399), true),
            Decision::WaitUntil(millis(t0, 4400))
        );
        assert_eq!(p.poll(millis(t0, 4400), true), Decision::Fire);
    }

    #[test]
    fn interview_short_filler_turn_stays_idle() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.piece_done(Speaker::Them, Some("right ok"), t0);
        assert_eq!(p.poll(secs(t0, 10), false), Decision::Idle);
        assert!(p.is_quiet());
    }

    #[test]
    fn interview_collects_short_pieces_until_enough_text() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.piece_done(Speaker::Them, Some("so"), t0);
        assert_eq!(p.poll(secs(t0, 5), false), Decision::Idle);
        p.piece_done(Speaker::Them, Some("tell me"), secs(t0, 1));
        assert_eq!(p.poll(secs(t0, 5), false), Decision::Idle);
        // "so tell me about that" holds 17 characters that are not spaces
        p.piece_done(Speaker::Them, Some("about that"), secs(t0, 2));
        assert_eq!(
            p.poll(secs(t0, 2), false),
            Decision::WaitUntil(millis(t0, 2400))
        );
    }

    #[test]
    fn interview_short_question_waits() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.piece_done(Speaker::Them, Some("why?"), t0);
        assert_eq!(p.poll(t0, false), Decision::WaitUntil(millis(t0, 400)));
        assert!(!p.is_quiet());
    }

    #[test]
    fn enough_text_counts_characters_not_words() {
        assert!(enough_text("給与の希望は？"));
        assert!(!enough_text("はい"));
        assert!(enough_text("abcdefghijkl"));
        assert!(!enough_text("abcdefghijk"));
        assert!(!enough_text("abcdef   ghi\n k"));
        assert!(enough_text("abcdef   ghijk\n l"));
        assert!(enough_text("?"));
    }

    #[test]
    fn interview_ignores_me_pieces() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.piece_done(Speaker::Me, Some("we could cache the results"), t0);
        assert_eq!(p.poll(secs(t0, 10), false), Decision::Idle);
        assert!(p.is_quiet());
    }

    #[test]
    fn a_dropped_piece_moves_the_settle_deadline() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.piece_done(Speaker::Them, Some("how would you scale that?"), t0);
        p.piece_done(Speaker::Them, None, millis(t0, 300));
        assert_eq!(
            p.poll(millis(t0, 300), false),
            Decision::WaitUntil(millis(t0, 700))
        );
        assert_eq!(p.poll(millis(t0, 700), false), Decision::Fire);
    }

    #[test]
    fn brainstorm_fires_at_once_even_while_me_is_busy() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Brainstorm);
        p.piece_done(Speaker::Me, Some("we could cache the results"), t0);
        assert_eq!(p.poll(t0, true), Decision::Fire);
    }

    #[test]
    fn brainstorm_dropped_piece_alone_stays_idle() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Brainstorm);
        p.piece_done(Speaker::Me, None, t0);
        assert_eq!(p.poll(secs(t0, 10), false), Decision::Idle);
        assert!(p.is_quiet());
    }

    #[test]
    fn brainstorm_gap_is_four_times_min_gap() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Brainstorm);
        p.request_started(Origin::Auto, t0);
        p.request_finished(Outcome::Ok, millis(t0, 500));
        p.piece_done(Speaker::Me, Some("we could cache the results"), secs(t0, 1));
        assert_eq!(p.poll(secs(t0, 1), true), Decision::WaitUntil(secs(t0, 8)));
        assert_eq!(
            p.poll(millis(t0, 7999), true),
            Decision::WaitUntil(secs(t0, 8))
        );
        assert_eq!(p.poll(secs(t0, 8), true), Decision::Fire);
    }

    #[test]
    fn interview_gap_holds_a_settled_question_until_two_seconds_after_the_last_start() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.request_started(Origin::Auto, t0);
        p.request_finished(Outcome::Ok, millis(t0, 300));
        p.piece_done(
            Speaker::Them,
            Some("how would you scale that?"),
            secs(t0, 1),
        );
        // settled at t0 + 1.4 s, gate at t0 + 2 s
        assert_eq!(
            p.poll(millis(t0, 1400), false),
            Decision::WaitUntil(secs(t0, 2))
        );
        assert_eq!(p.poll(secs(t0, 2), false), Decision::Fire);
    }

    #[test]
    fn a_trigger_during_a_running_request_fires_after_it_ends() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.request_started(Origin::Auto, t0);
        p.piece_done(
            Speaker::Them,
            Some("how would you scale that?"),
            secs(t0, 3),
        );
        assert_eq!(p.poll(secs(t0, 10), false), Decision::Idle);
        assert!(!p.is_quiet());
        p.request_finished(Outcome::Ok, secs(t0, 11));
        assert_eq!(p.poll(secs(t0, 11), false), Decision::Fire);
    }

    #[test]
    fn three_triggers_during_a_running_request_fire_once() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.request_started(Origin::Auto, t0);
        for i in 1..=3 {
            p.piece_done(
                Speaker::Them,
                Some("how would you scale that?"),
                secs(t0, i),
            );
        }
        p.request_finished(Outcome::Ok, secs(t0, 5));
        assert_eq!(p.poll(secs(t0, 6), false), Decision::Fire);
        p.request_started(Origin::Auto, secs(t0, 6));
        assert_eq!(p.poll(secs(t0, 7), false), Decision::Idle);
        p.request_finished(Outcome::Ok, secs(t0, 8));
        assert_eq!(p.poll(secs(t0, 20), false), Decision::Idle);
        assert!(p.is_quiet());
    }

    #[test]
    fn a_failure_drops_the_waiting_trigger_and_pauses_for_thirty_seconds() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.request_started(Origin::Auto, t0);
        p.piece_done(
            Speaker::Them,
            Some("how would you scale that?"),
            secs(t0, 1),
        );
        p.request_finished(Outcome::Failed, secs(t0, 2));
        assert_eq!(p.poll(secs(t0, 3), false), Decision::Idle);
        assert!(p.is_quiet());
        // inside the pause (until t0 + 32 s): ignored
        p.piece_done(
            Speaker::Them,
            Some("how would you scale that?"),
            secs(t0, 3),
        );
        assert_eq!(p.poll(secs(t0, 20), false), Decision::Idle);
        // after the pause: waits and fires
        p.piece_done(
            Speaker::Them,
            Some("how would you scale that?"),
            secs(t0, 32),
        );
        assert_eq!(
            p.poll(secs(t0, 32), false),
            Decision::WaitUntil(millis(t0, 32_400))
        );
        assert_eq!(p.poll(millis(t0, 32_400), false), Decision::Fire);
    }

    #[test]
    fn a_successful_manual_request_ends_the_pause() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.request_started(Origin::Auto, t0);
        p.request_finished(Outcome::Failed, secs(t0, 1));
        p.request_started(Origin::Manual, secs(t0, 5));
        p.request_finished(Outcome::Ok, secs(t0, 6));
        p.piece_done(
            Speaker::Them,
            Some("how would you scale that?"),
            secs(t0, 7),
        );
        assert_eq!(
            p.poll(secs(t0, 7), false),
            Decision::WaitUntil(millis(t0, 7400))
        );
        assert_eq!(p.poll(millis(t0, 7400), false), Decision::Fire);
    }

    #[test]
    fn a_cancelled_request_does_not_pause() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.request_started(Origin::Manual, t0);
        p.request_finished(Outcome::Cancelled, secs(t0, 1));
        p.piece_done(
            Speaker::Them,
            Some("how would you scale that?"),
            secs(t0, 2),
        );
        assert_eq!(p.poll(millis(t0, 2400), false), Decision::Fire);
    }

    #[test]
    fn a_manual_start_drops_the_waiting_trigger_and_keeps_the_gap_clock() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.request_started(Origin::Auto, t0);
        p.request_finished(Outcome::Ok, millis(t0, 100));
        p.piece_done(
            Speaker::Them,
            Some("how would you scale that?"),
            millis(t0, 200),
        );
        assert!(!p.is_quiet());
        p.request_started(Origin::Manual, secs(t0, 1));
        assert!(!p.is_quiet());
        assert_eq!(p.poll(secs(t0, 1), false), Decision::Idle);
        p.request_finished(Outcome::Ok, millis(t0, 1500));
        assert!(p.is_quiet());
        // the gap clock still says t0 + 2 s, not the manual start at t0 + 1 s
        p.piece_done(
            Speaker::Them,
            Some("how would you scale that?"),
            millis(t0, 1500),
        );
        assert_eq!(
            p.poll(millis(t0, 1500), false),
            Decision::WaitUntil(secs(t0, 2))
        );
    }

    #[test]
    fn switching_profile_drops_the_waiting_trigger() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.piece_done(Speaker::Them, Some("how would you scale that?"), t0);
        p.set_profile(AssistProfile::Brainstorm);
        assert_eq!(p.poll(secs(t0, 10), false), Decision::Idle);
        assert!(p.is_quiet());
        p.set_profile(AssistProfile::Interview);
        assert_eq!(p.poll(secs(t0, 10), false), Decision::Idle);
    }

    #[test]
    fn switching_profile_keeps_the_gap_clock() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        p.request_started(Origin::Auto, t0);
        p.request_finished(Outcome::Ok, millis(t0, 100));
        p.set_profile(AssistProfile::Brainstorm);
        p.piece_done(Speaker::Me, Some("we could cache the results"), secs(t0, 1));
        assert_eq!(p.poll(secs(t0, 1), false), Decision::WaitUntil(secs(t0, 8)));
    }

    #[test]
    fn is_quiet_tracks_waiting_and_running() {
        let t0 = Instant::now();
        let mut p = policy(AssistProfile::Interview);
        assert!(p.is_quiet());
        p.piece_done(Speaker::Them, Some("how would you scale that?"), t0);
        assert!(!p.is_quiet());
        p.request_started(Origin::Auto, millis(t0, 400));
        assert!(!p.is_quiet());
        p.request_finished(Outcome::Ok, secs(t0, 3));
        assert!(p.is_quiet());
    }

    #[test]
    fn trigger_table_matches_the_profiles() {
        assert_eq!(trigger(AssistProfile::Manual), None);
        assert_eq!(
            trigger(AssistProfile::Interview),
            Some(Trigger {
                speaker: Speaker::Them,
                wait_for_turn_end: true,
                gap_factor: 1
            })
        );
        assert_eq!(
            trigger(AssistProfile::Brainstorm),
            Some(Trigger {
                speaker: Speaker::Me,
                wait_for_turn_end: false,
                gap_factor: 4
            })
        );
    }
}
