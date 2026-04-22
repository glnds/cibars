use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_secs(3);
pub const GRACE_DURATION: Duration = Duration::from_secs(90);
pub const COOLDOWN_DURATION: Duration = Duration::from_secs(60);
/// Cap on how long a single watched PR keeps the scheduler out of Sleep.
/// Prevents indefinite polling if a PR is opened but never merged or closed.
pub const PR_WATCH_CAP: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollingPhase {
    Grace,
    Active,
    Cooldown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollState {
    Sleep,
    Polling { phase: PollingPhase, since: Instant },
}

pub struct PollScheduler {
    state: PollState,
}

impl PollScheduler {
    pub fn new() -> Self {
        Self {
            state: PollState::Sleep,
        }
    }

    pub fn state(&self) -> PollState {
        self.state
    }

    pub fn is_polling(&self) -> bool {
        matches!(self.state, PollState::Polling { .. })
    }

    pub fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    pub fn boost(&mut self) {
        match self.state {
            PollState::Sleep => {
                self.state = PollState::Polling {
                    phase: PollingPhase::Grace,
                    since: Instant::now(),
                };
                tracing::info!("boost: Sleep → Polling{{Grace}}");
            }
            PollState::Polling {
                phase: PollingPhase::Cooldown,
                ..
            } => {
                self.state = PollState::Polling {
                    phase: PollingPhase::Grace,
                    since: Instant::now(),
                };
                tracing::info!("boost: Cooldown → Polling{{Grace}} (restart)");
            }
            PollState::Polling {
                phase: PollingPhase::Grace,
                ..
            } => {
                self.state = PollState::Polling {
                    phase: PollingPhase::Grace,
                    since: Instant::now(),
                };
                tracing::info!("boost: Grace → Grace (reset)");
            }
            PollState::Polling {
                phase: PollingPhase::Active,
                ..
            } => {
                tracing::info!("boost: no-op in Active");
            }
        }
    }

    pub fn transition(&mut self, any_running: bool) {
        match self.state {
            PollState::Sleep => {}
            PollState::Polling {
                phase: PollingPhase::Grace,
                since,
            } => {
                if any_running {
                    self.state = PollState::Polling {
                        phase: PollingPhase::Active,
                        since: Instant::now(),
                    };
                } else if since.elapsed() >= GRACE_DURATION {
                    self.state = PollState::Sleep;
                }
            }
            PollState::Polling {
                phase: PollingPhase::Active,
                ..
            } => {
                if !any_running {
                    self.state = PollState::Polling {
                        phase: PollingPhase::Cooldown,
                        since: Instant::now(),
                    };
                }
            }
            PollState::Polling {
                phase: PollingPhase::Cooldown,
                since,
            } => {
                if any_running {
                    self.state = PollState::Polling {
                        phase: PollingPhase::Active,
                        since: Instant::now(),
                    };
                } else if since.elapsed() >= COOLDOWN_DURATION {
                    self.state = PollState::Sleep;
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn force_phase_since(&mut self, phase: PollingPhase, age: Duration) {
        let since = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
        self.state = PollState::Polling { phase, since };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn polling_phase(s: &PollScheduler) -> PollingPhase {
        match s.state() {
            PollState::Polling { phase, .. } => phase,
            other => panic!("expected Polling, got {other:?}"),
        }
    }

    // 1
    #[test]
    fn new_starts_in_sleep() {
        let s = PollScheduler::new();
        assert_eq!(s.state(), PollState::Sleep);
    }

    // 2
    #[test]
    fn sleep_does_not_poll() {
        let s = PollScheduler::new();
        assert!(!s.is_polling());
    }

    // 3
    #[test]
    fn transition_in_sleep_with_running_stays_sleep() {
        let mut s = PollScheduler::new();
        s.transition(true);
        assert_eq!(s.state(), PollState::Sleep);
    }

    // 4
    #[test]
    fn boost_from_sleep_enters_grace() {
        let mut s = PollScheduler::new();
        s.boost();
        assert!(s.is_polling());
        assert_eq!(polling_phase(&s), PollingPhase::Grace);
    }

    // 5
    #[test]
    fn grace_with_running_transitions_to_active() {
        let mut s = PollScheduler::new();
        s.boost();
        s.transition(true);
        assert_eq!(polling_phase(&s), PollingPhase::Active);
    }

    // 6
    #[test]
    fn grace_without_running_stays_grace_within_window() {
        let mut s = PollScheduler::new();
        s.boost();
        s.transition(false);
        assert_eq!(polling_phase(&s), PollingPhase::Grace);
    }

    // 7
    #[test]
    fn grace_expires_to_sleep_after_90s() {
        let mut s = PollScheduler::new();
        s.boost();
        s.force_phase_since(PollingPhase::Grace, GRACE_DURATION + Duration::from_secs(1));
        s.transition(false);
        assert_eq!(s.state(), PollState::Sleep);
    }

    // 8
    #[test]
    fn active_without_running_transitions_to_cooldown() {
        let mut s = PollScheduler::new();
        s.boost();
        s.transition(true); // → Active
        s.transition(false);
        assert_eq!(polling_phase(&s), PollingPhase::Cooldown);
    }

    // 9
    #[test]
    fn active_with_running_stays_active() {
        let mut s = PollScheduler::new();
        s.boost();
        s.transition(true); // → Active
        s.transition(true);
        assert_eq!(polling_phase(&s), PollingPhase::Active);
    }

    // 10
    #[test]
    fn cooldown_with_running_transitions_to_active() {
        let mut s = PollScheduler::new();
        s.boost();
        s.transition(true); // → Active
        s.transition(false); // → Cooldown
        s.transition(true);
        assert_eq!(polling_phase(&s), PollingPhase::Active);
    }

    // 11
    #[test]
    fn cooldown_expires_to_sleep_after_60s() {
        let mut s = PollScheduler::new();
        s.boost();
        s.transition(true); // → Active
        s.transition(false); // → Cooldown
        s.force_phase_since(
            PollingPhase::Cooldown,
            COOLDOWN_DURATION + Duration::from_secs(1),
        );
        s.transition(false);
        assert_eq!(s.state(), PollState::Sleep);
    }

    // 12
    #[test]
    fn boost_in_cooldown_re_enters_grace() {
        let mut s = PollScheduler::new();
        s.boost();
        s.transition(true); // → Active
        s.transition(false); // → Cooldown
        s.boost();
        assert_eq!(polling_phase(&s), PollingPhase::Grace);
    }

    // 13
    #[test]
    fn boost_in_active_is_noop() {
        let mut s = PollScheduler::new();
        s.boost();
        s.transition(true); // → Active
        s.boost();
        assert_eq!(polling_phase(&s), PollingPhase::Active);
    }

    // 14
    #[test]
    fn boost_in_grace_resets_since() {
        let mut s = PollScheduler::new();
        s.boost();
        s.force_phase_since(PollingPhase::Grace, Duration::from_secs(80));
        let before = match s.state() {
            PollState::Polling { since, .. } => since,
            _ => panic!(),
        };
        s.boost();
        let after = match s.state() {
            PollState::Polling { since, .. } => since,
            _ => panic!(),
        };
        assert!(
            after > before,
            "boost in Grace should reset since to a later Instant"
        );
        assert_eq!(polling_phase(&s), PollingPhase::Grace);
    }

    // 15
    #[test]
    fn interval_is_3s_when_polling() {
        let mut s = PollScheduler::new();
        s.boost();
        assert_eq!(s.interval(), Duration::from_secs(3));
        s.transition(true); // → Active
        assert_eq!(s.interval(), Duration::from_secs(3));
        s.transition(false); // → Cooldown
        assert_eq!(s.interval(), Duration::from_secs(3));
    }

    // 16: cooldown countdown is implicit in PollState::Polling{Cooldown, since}.
    // Verify entry sets `since` to roughly Instant::now() so remaining ≈ COOLDOWN_DURATION.
    #[test]
    fn cooldown_entry_since_is_recent() {
        let mut s = PollScheduler::new();
        s.boost();
        s.transition(true); // → Active
        s.transition(false); // → Cooldown
        match s.state() {
            PollState::Polling {
                phase: PollingPhase::Cooldown,
                since,
            } => {
                let elapsed = since.elapsed();
                assert!(
                    elapsed < Duration::from_millis(50),
                    "cooldown since should be very recent, got {elapsed:?}"
                );
            }
            other => panic!("expected Cooldown, got {other:?}"),
        }
    }
}
