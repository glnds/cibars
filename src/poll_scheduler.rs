use std::time::{Duration, Instant};

const IDLE_INTERVAL: Duration = Duration::from_secs(30);
const ACTIVE_INTERVAL: Duration = Duration::from_secs(5);
const COOLDOWN_DURATION: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollState {
    Idle,
    Active,
    Cooldown,
}

pub struct PollScheduler {
    state: PollState,
    cooldown_started: Option<Instant>,
}

impl PollScheduler {
    pub fn new() -> Self {
        Self {
            state: PollState::Idle,
            cooldown_started: None,
        }
    }

    pub fn state(&self) -> PollState {
        self.state
    }

    pub fn should_poll_aws(&self) -> bool {
        matches!(self.state, PollState::Active | PollState::Cooldown)
    }

    pub fn interval(&self) -> Duration {
        match self.state {
            PollState::Idle => IDLE_INTERVAL,
            PollState::Active | PollState::Cooldown => ACTIVE_INTERVAL,
        }
    }

    pub fn transition(&mut self, any_running: bool) {
        match self.state {
            PollState::Idle => {
                if any_running {
                    self.state = PollState::Active;
                }
            }
            PollState::Active => {
                if !any_running {
                    self.state = PollState::Cooldown;
                    self.cooldown_started = Some(Instant::now());
                }
            }
            PollState::Cooldown => {
                if any_running {
                    self.state = PollState::Active;
                    self.cooldown_started = None;
                } else if self
                    .cooldown_started
                    .map(|t| t.elapsed() >= COOLDOWN_DURATION)
                    .unwrap_or(true)
                {
                    self.state = PollState::Idle;
                    self.cooldown_started = None;
                }
            }
        }
    }

    pub fn cooldown_remaining(&self) -> Option<Duration> {
        if self.state != PollState::Cooldown {
            return None;
        }
        self.cooldown_started.map(|t| {
            let elapsed = t.elapsed();
            COOLDOWN_DURATION.saturating_sub(elapsed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_idle() {
        let s = PollScheduler::new();
        assert_eq!(s.state(), PollState::Idle);
    }

    // --- transition tests ---

    #[test]
    fn idle_to_active_when_running() {
        let mut s = PollScheduler::new();
        s.transition(true);
        assert_eq!(s.state(), PollState::Active);
    }

    #[test]
    fn idle_stays_idle_when_not_running() {
        let mut s = PollScheduler::new();
        s.transition(false);
        assert_eq!(s.state(), PollState::Idle);
    }

    #[test]
    fn active_stays_active_when_running() {
        let mut s = PollScheduler::new();
        s.transition(true); // → Active
        s.transition(true);
        assert_eq!(s.state(), PollState::Active);
    }

    #[test]
    fn active_to_cooldown_when_not_running() {
        let mut s = PollScheduler::new();
        s.transition(true); // → Active
        s.transition(false);
        assert_eq!(s.state(), PollState::Cooldown);
    }

    #[test]
    fn cooldown_to_active_when_running() {
        let mut s = PollScheduler::new();
        s.transition(true); // → Active
        s.transition(false); // → Cooldown
        s.transition(true);
        assert_eq!(s.state(), PollState::Active);
    }

    #[test]
    fn cooldown_stays_when_timer_not_expired() {
        let mut s = PollScheduler::new();
        s.transition(true); // → Active
        s.transition(false); // → Cooldown (timer starts now)
                             // Timer just started, not expired
        s.transition(false);
        assert_eq!(s.state(), PollState::Cooldown);
    }

    #[test]
    fn cooldown_to_idle_when_timer_expired() {
        let mut s = PollScheduler::new();
        s.transition(true); // → Active
        s.transition(false); // → Cooldown
                             // Force expired cooldown
        s.cooldown_started = Some(Instant::now() - Duration::from_secs(61));
        s.transition(false);
        assert_eq!(s.state(), PollState::Idle);
    }

    // --- should_poll_aws tests ---

    #[test]
    fn should_poll_aws_false_in_idle() {
        let s = PollScheduler::new();
        assert!(!s.should_poll_aws());
    }

    #[test]
    fn should_poll_aws_true_in_active() {
        let mut s = PollScheduler::new();
        s.transition(true);
        assert!(s.should_poll_aws());
    }

    #[test]
    fn should_poll_aws_true_in_cooldown() {
        let mut s = PollScheduler::new();
        s.transition(true);
        s.transition(false);
        assert!(s.should_poll_aws());
    }

    // --- interval tests ---

    #[test]
    fn interval_30s_in_idle() {
        let s = PollScheduler::new();
        assert_eq!(s.interval(), Duration::from_secs(30));
    }

    #[test]
    fn interval_5s_in_active() {
        let mut s = PollScheduler::new();
        s.transition(true);
        assert_eq!(s.interval(), Duration::from_secs(5));
    }

    #[test]
    fn interval_5s_in_cooldown() {
        let mut s = PollScheduler::new();
        s.transition(true);
        s.transition(false);
        assert_eq!(s.interval(), Duration::from_secs(5));
    }

    // --- cooldown_remaining tests ---

    #[test]
    fn cooldown_remaining_none_when_not_cooldown() {
        let s = PollScheduler::new();
        assert!(s.cooldown_remaining().is_none());
    }

    #[test]
    fn cooldown_remaining_some_when_cooldown() {
        let mut s = PollScheduler::new();
        s.transition(true);
        s.transition(false);
        let remaining = s.cooldown_remaining().unwrap();
        assert!(remaining <= Duration::from_secs(60));
        assert!(remaining > Duration::from_secs(58));
    }
}
