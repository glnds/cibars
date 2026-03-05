use std::time::{Duration, Instant};

const IDLE_INTERVAL: Duration = Duration::from_secs(30);
const LONG_IDLE_INTERVAL: Duration = Duration::from_secs(300);
const ACTIVE_INTERVAL: Duration = Duration::from_secs(5);
const COOLDOWN_DURATION: Duration = Duration::from_secs(60);
const IDLE_TO_LONG_DURATION: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollState {
    Idle,
    LongIdle,
    Watching,
    Active,
    Cooldown,
}

pub struct PollScheduler {
    state: PollState,
    cooldown_started: Option<Instant>,
    watching_started: Option<Instant>,
    idle_started: Option<Instant>,
    needs_initial_poll: bool,
}

impl PollScheduler {
    pub fn new() -> Self {
        Self {
            state: PollState::Idle,
            cooldown_started: None,
            watching_started: None,
            idle_started: None,
            needs_initial_poll: true,
        }
    }

    pub fn state(&self) -> PollState {
        self.state
    }

    pub fn should_poll_aws(&self) -> bool {
        self.needs_initial_poll || matches!(self.state, PollState::Active | PollState::Cooldown)
    }

    pub fn boost(&mut self) {
        if matches!(self.state, PollState::Idle | PollState::LongIdle) {
            self.state = PollState::Watching;
            self.watching_started = Some(Instant::now());
            self.idle_started = None;
        }
    }

    pub fn interval(&self) -> Duration {
        match self.state {
            PollState::Idle => IDLE_INTERVAL,
            PollState::LongIdle => LONG_IDLE_INTERVAL,
            PollState::Watching | PollState::Active | PollState::Cooldown => ACTIVE_INTERVAL,
        }
    }

    pub fn transition(&mut self, any_running: bool) {
        self.needs_initial_poll = false;
        match self.state {
            PollState::Idle => {
                if any_running {
                    self.state = PollState::Active;
                    self.idle_started = None;
                } else if self
                    .idle_started
                    .map(|t| t.elapsed() >= IDLE_TO_LONG_DURATION)
                    .unwrap_or(false)
                {
                    self.state = PollState::LongIdle;
                    self.idle_started = None;
                } else if self.idle_started.is_none() {
                    self.idle_started = Some(Instant::now());
                }
            }
            PollState::LongIdle => {
                if any_running {
                    self.state = PollState::Active;
                }
            }
            PollState::Watching => {
                if any_running {
                    self.state = PollState::Active;
                    self.watching_started = None;
                } else if self
                    .watching_started
                    .map(|t| t.elapsed() >= COOLDOWN_DURATION)
                    .unwrap_or(true)
                {
                    self.state = PollState::Idle;
                    self.watching_started = None;
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

    #[cfg(test)]
    pub(crate) fn force_expire_watching(&mut self) {
        self.watching_started = Some(Instant::now() - COOLDOWN_DURATION - Duration::from_secs(1));
    }

    #[cfg(test)]
    pub(crate) fn force_expire_idle(&mut self) {
        self.idle_started = Some(Instant::now() - IDLE_TO_LONG_DURATION - Duration::from_secs(1));
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
    fn should_poll_aws_true_on_initial() {
        let s = PollScheduler::new();
        assert!(s.should_poll_aws());
    }

    #[test]
    fn should_poll_aws_false_in_idle_after_initial() {
        let mut s = PollScheduler::new();
        s.transition(false); // clears initial, stays Idle
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

    // --- boost tests ---

    #[test]
    fn boost_idle_to_watching() {
        let mut s = PollScheduler::new();
        s.transition(false); // clear initial
        s.boost();
        assert_eq!(s.state(), PollState::Watching);
    }

    #[test]
    fn boost_noop_in_active() {
        let mut s = PollScheduler::new();
        s.transition(true); // → Active
        s.boost();
        assert_eq!(s.state(), PollState::Active);
    }

    #[test]
    fn boost_noop_in_cooldown() {
        let mut s = PollScheduler::new();
        s.transition(true); // → Active
        s.transition(false); // → Cooldown
        s.boost();
        assert_eq!(s.state(), PollState::Cooldown);
    }

    // --- watching transition tests ---

    #[test]
    fn watching_to_active_when_running() {
        let mut s = PollScheduler::new();
        s.transition(false); // clear initial
        s.boost(); // → Watching
        s.transition(true);
        assert_eq!(s.state(), PollState::Active);
    }

    #[test]
    fn watching_stays_when_timer_not_expired() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.boost(); // → Watching
        s.transition(false); // timer just started
        assert_eq!(s.state(), PollState::Watching);
    }

    #[test]
    fn watching_to_idle_when_timer_expired() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.boost(); // → Watching
        s.force_expire_watching();
        s.transition(false);
        assert_eq!(s.state(), PollState::Idle);
    }

    // --- watching: should_poll_aws false ---

    #[test]
    fn should_poll_aws_false_in_watching() {
        let mut s = PollScheduler::new();
        s.transition(false); // clear initial
        s.boost(); // → Watching
        assert!(!s.should_poll_aws());
    }

    // --- watching: interval is 5s ---

    #[test]
    fn interval_5s_in_watching() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.boost();
        assert_eq!(s.interval(), Duration::from_secs(5));
    }

    // --- cooldown_remaining None in Watching ---

    #[test]
    fn cooldown_remaining_none_in_watching() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.boost();
        assert!(s.cooldown_remaining().is_none());
    }

    // --- long idle tests ---

    #[test]
    fn idle_to_long_idle_after_timer_expired() {
        let mut s = PollScheduler::new();
        s.transition(false); // Idle, sets idle_started
        s.force_expire_idle();
        s.transition(false);
        assert_eq!(s.state(), PollState::LongIdle);
    }

    #[test]
    fn idle_stays_idle_when_timer_not_expired() {
        let mut s = PollScheduler::new();
        s.transition(false); // Idle, sets idle_started
                             // Timer just started, not expired
        s.transition(false);
        assert_eq!(s.state(), PollState::Idle);
    }

    #[test]
    fn long_idle_to_active_when_running() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.force_expire_idle();
        s.transition(false); // → LongIdle
        s.transition(true);
        assert_eq!(s.state(), PollState::Active);
    }

    #[test]
    fn long_idle_stays_when_not_running() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.force_expire_idle();
        s.transition(false); // → LongIdle
        s.transition(false);
        assert_eq!(s.state(), PollState::LongIdle);
    }

    #[test]
    fn boost_long_idle_to_watching() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.force_expire_idle();
        s.transition(false); // → LongIdle
        s.boost();
        assert_eq!(s.state(), PollState::Watching);
    }

    #[test]
    fn interval_300s_in_long_idle() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.force_expire_idle();
        s.transition(false); // → LongIdle
        assert_eq!(s.interval(), Duration::from_secs(300));
    }

    #[test]
    fn should_poll_aws_false_in_long_idle() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.force_expire_idle();
        s.transition(false); // → LongIdle
        assert!(!s.should_poll_aws());
    }

    #[test]
    fn cooldown_remaining_none_in_long_idle() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.force_expire_idle();
        s.transition(false); // → LongIdle
        assert!(s.cooldown_remaining().is_none());
    }

    #[test]
    fn idle_to_active_clears_idle_timer() {
        let mut s = PollScheduler::new();
        s.transition(false); // sets idle_started
        s.transition(true); // → Active
        s.transition(false); // → Cooldown
        s.cooldown_started = Some(Instant::now() - Duration::from_secs(61));
        s.transition(false); // → Idle
                             // idle_started should be None, gets set fresh
                             // Should not jump to LongIdle immediately
        s.transition(false);
        assert_eq!(s.state(), PollState::Idle);
    }
}
