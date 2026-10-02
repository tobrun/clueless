//! Pure restart policy for the system-audio watchdog (D-watchdog).
//!
//! The policy is decided by three inputs - `on_stop_error`, `on_sample` and
//! `tick` - against an injected clock, so the whole rule set is unit-testable
//! without ScreenCaptureKit.

use std::time::{Duration, Instant};

/// What the watchdog wants done after an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogAction {
    /// start a new stream
    Restart,
    /// too many restarts without a sample; stop trying and report
    GiveUp,
    /// keep going
    Nothing,
}

/// Restart on stop error, at most `max_restarts` times without a sample in
/// between (the count resets on the first sample after a restart). A silence
/// timer additionally asks for a restart when no samples arrive for
/// `silence_secs`; it is active only when `silence_secs` is above 0.
#[derive(Debug, Clone)]
pub struct RestartPolicy {
    max_restarts: u32,
    silence: Option<Duration>,
    restarts_without_sample: u32,
    last_activity: Instant,
}

impl RestartPolicy {
    pub fn new(max_restarts: u32, silence_secs: u64) -> Self {
        let now = Instant::now();
        Self {
            max_restarts,
            silence: if silence_secs > 0 {
                Some(Duration::from_secs(silence_secs))
            } else {
                None
            },
            restarts_without_sample: 0,
            last_activity: now,
        }
    }

    /// The stream reported that it stopped with an error.
    pub fn on_stop_error(&mut self) -> WatchdogAction {
        self.try_restart()
    }

    /// A sample arrived; the restart budget is full again.
    pub fn on_sample(&mut self, now: Instant) {
        self.last_activity = now;
        self.restarts_without_sample = 0;
    }

    /// Periodic check for the silence timer.
    pub fn tick(&mut self, now: Instant) -> WatchdogAction {
        match self.silence {
            Some(d) if now.duration_since(self.last_activity) >= d => self.try_restart(),
            _ => WatchdogAction::Nothing,
        }
    }

    fn try_restart(&mut self) -> WatchdogAction {
        self.restarts_without_sample += 1;
        if self.restarts_without_sample > self.max_restarts {
            WatchdogAction::GiveUp
        } else {
            WatchdogAction::Restart
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_errors_restart_until_the_limit_then_give_up() {
        // Spec: limit 5 - stop error restarts; five more stop errors with no
        // sample in between, and the sixth returns GiveUp.
        let mut p = RestartPolicy::new(5, 0);
        assert_eq!(p.on_stop_error(), WatchdogAction::Restart);
        for _ in 0..4 {
            assert_eq!(p.on_stop_error(), WatchdogAction::Restart);
        }
        assert_eq!(p.on_stop_error(), WatchdogAction::GiveUp);
    }

    #[test]
    fn a_sample_between_stop_errors_resets_the_restart_budget() {
        // Spec: stop error, sample, stop error repeated 10 times restarts every time.
        let mut p = RestartPolicy::new(5, 0);
        let mut t = Instant::now();
        for _ in 0..10 {
            assert_eq!(p.on_stop_error(), WatchdogAction::Restart);
            t += Duration::from_millis(100);
            p.on_sample(t);
        }
    }

    #[test]
    fn limit_zero_gives_up_on_the_first_stop_error() {
        // Spec: limit 0 (restarting off) - stop error returns GiveUp.
        let mut p = RestartPolicy::new(0, 0);
        assert_eq!(p.on_stop_error(), WatchdogAction::GiveUp);
    }

    #[test]
    fn silence_timer_off_never_ticks_a_restart() {
        // Spec: timer 0 - a tick 60 s after the last sample returns Nothing.
        let start = Instant::now();
        let mut p = RestartPolicy::new(5, 0);
        p.on_sample(start);
        assert_eq!(
            p.tick(start + Duration::from_secs(60)),
            WatchdogAction::Nothing
        );
    }

    #[test]
    fn silence_timer_restarts_when_no_samples_arrive() {
        // Spec: timer 5 s - a tick 6 s after the last sample returns Restart.
        let start = Instant::now();
        let mut p = RestartPolicy::new(5, 5);
        p.on_sample(start);
        assert_eq!(
            p.tick(start + Duration::from_secs(6)),
            WatchdogAction::Restart
        );
    }
}
