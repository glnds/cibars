use std::time::{Duration, Instant};

const IDLE_INTERVAL: Duration = Duration::from_secs(30);
const LONG_IDLE_INTERVAL: Duration = Duration::from_secs(300);
const ACTIVE_INTERVAL: Duration = Duration::from_secs(5);
const COOLDOWN_DURATION: Duration = Duration::from_secs(60);
const IDLE_TO_LONG_DURATION: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy)]
pub struct StateTimer {
    pub started: Instant,
    pub total: Option<Duration>,
}

impl StateTimer {
    pub fn countdown(started: Instant, total: Duration) -> Self {
        Self {
            started,
            total: Some(total),
        }
    }

    pub fn elapsed(started: Instant) -> Self {
        Self {
            started,
            total: None,
        }
    }

    pub fn display_duration(&self) -> Duration {
        let elapsed = self.started.elapsed();
        match self.total {
            Some(total) => total.saturating_sub(elapsed),
            None => elapsed,
        }
    }

    /// Number of filled tick blocks (0..num_ticks) derived from elapsed time.
    /// Both this and `display_duration` use the same `started` Instant,
    /// keeping blocks and timer visually in sync.
    pub fn tick_filled(&self, poll_interval: Duration, num_ticks: usize) -> usize {
        let interval_secs = poll_interval.as_secs();
        if interval_secs == 0 {
            return 0;
        }
        let elapsed_secs = self.started.elapsed().as_secs();
        let cycle_secs = elapsed_secs % interval_secs;
        (cycle_secs as usize * num_ticks) / interval_secs as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollState {
    Idle,
    LongIdle,
    Watching,
    Active,
    Cooldown,
}

impl PollState {
    pub fn interval(&self) -> Duration {
        match self {
            PollState::Idle => IDLE_INTERVAL,
            PollState::LongIdle => LONG_IDLE_INTERVAL,
            PollState::Watching | PollState::Active | PollState::Cooldown => ACTIVE_INTERVAL,
        }
    }
}

pub struct PollScheduler {
    state: PollState,
    cooldown_started: Option<Instant>,
    watching_started: Option<Instant>,
    idle_started: Option<Instant>,
    active_started: Option<Instant>,
    long_idle_started: Option<Instant>,
    needs_initial_poll: bool,
}

impl PollScheduler {
    pub fn new() -> Self {
        Self {
            state: PollState::Idle,
            cooldown_started: None,
            watching_started: None,
            idle_started: None,
            active_started: None,
            long_idle_started: None,
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
            let prev = self.state;
            self.state = PollState::Watching;
            self.watching_started = Some(Instant::now());
            self.idle_started = None;
            self.long_idle_started = None;
            tracing::info!(?prev, "boost: → Watching");
        } else {
            tracing::info!(state = ?self.state, "boost: no-op (not idle)");
        }
    }

    pub fn interval(&self) -> Duration {
        self.state.interval()
    }

    pub fn transition(&mut self, any_running: bool) {
        self.needs_initial_poll = false;
        match self.state {
            PollState::Idle => {
                if any_running {
                    self.state = PollState::Active;
                    self.idle_started = None;
                    self.active_started = Some(Instant::now());
                } else if self
                    .idle_started
                    .map(|t| t.elapsed() >= IDLE_TO_LONG_DURATION)
                    .unwrap_or(false)
                {
                    self.state = PollState::LongIdle;
                    self.idle_started = None;
                    self.long_idle_started = Some(Instant::now());
                } else if self.idle_started.is_none() {
                    self.idle_started = Some(Instant::now());
                }
            }
            PollState::LongIdle => {
                if any_running {
                    self.state = PollState::Active;
                    self.active_started = Some(Instant::now());
                    self.long_idle_started = None;
                }
            }
            PollState::Watching => {
                if any_running {
                    self.state = PollState::Active;
                    self.watching_started = None;
                    self.active_started = Some(Instant::now());
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
                    self.active_started = None;
                }
            }
            PollState::Cooldown => {
                if any_running {
                    self.state = PollState::Active;
                    self.cooldown_started = None;
                    self.active_started = Some(Instant::now());
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

    pub fn state_timer(&self) -> Option<StateTimer> {
        match self.state {
            PollState::Idle => self
                .idle_started
                .map(|t| StateTimer::countdown(t, IDLE_TO_LONG_DURATION)),
            PollState::Watching => self
                .watching_started
                .map(|t| StateTimer::countdown(t, COOLDOWN_DURATION)),
            PollState::Cooldown => self
                .cooldown_started
                .map(|t| StateTimer::countdown(t, COOLDOWN_DURATION)),
            PollState::Active => self.active_started.map(StateTimer::elapsed),
            PollState::LongIdle => self.long_idle_started.map(StateTimer::elapsed),
        }
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

    #[test]
    fn boost_noop_in_watching() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.boost(); // → Watching
        s.boost(); // should stay Watching
        assert_eq!(s.state(), PollState::Watching);
    }

    // --- PollState::interval() tests ---

    #[test]
    fn poll_state_interval_active() {
        assert_eq!(PollState::Active.interval(), Duration::from_secs(5));
    }

    #[test]
    fn poll_state_interval_idle() {
        assert_eq!(PollState::Idle.interval(), Duration::from_secs(30));
    }

    #[test]
    fn poll_state_interval_long_idle() {
        assert_eq!(PollState::LongIdle.interval(), Duration::from_secs(300));
    }

    #[test]
    fn poll_state_interval_watching() {
        assert_eq!(PollState::Watching.interval(), Duration::from_secs(5));
    }

    #[test]
    fn poll_state_interval_cooldown() {
        assert_eq!(PollState::Cooldown.interval(), Duration::from_secs(5));
    }

    // --- StateTimer tests ---

    #[test]
    fn state_timer_countdown_display() {
        let timer = StateTimer::countdown(
            Instant::now() - Duration::from_secs(10),
            Duration::from_secs(60),
        );
        let d = timer.display_duration();
        assert!(d >= Duration::from_secs(49) && d <= Duration::from_secs(50));
    }

    #[test]
    fn state_timer_countdown_saturates_to_zero() {
        let timer = StateTimer::countdown(
            Instant::now() - Duration::from_secs(120),
            Duration::from_secs(60),
        );
        assert_eq!(timer.display_duration(), Duration::ZERO);
    }

    #[test]
    fn state_timer_elapsed_display() {
        let timer = StateTimer::elapsed(Instant::now() - Duration::from_secs(5));
        let d = timer.display_duration();
        assert!(d >= Duration::from_secs(5) && d < Duration::from_secs(6));
    }

    // --- state_timer() method tests ---

    #[test]
    fn state_timer_none_in_fresh_scheduler() {
        let s = PollScheduler::new();
        assert!(s.state_timer().is_none());
    }

    #[test]
    fn state_timer_countdown_in_idle() {
        let mut s = PollScheduler::new();
        s.transition(false); // Idle, sets idle_started
        let timer = s.state_timer().unwrap();
        assert!(timer.total.is_some());
        assert_eq!(timer.total.unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn state_timer_countdown_in_watching() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.boost(); // → Watching
        let timer = s.state_timer().unwrap();
        assert_eq!(timer.total.unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn state_timer_countdown_in_cooldown() {
        let mut s = PollScheduler::new();
        s.transition(true); // → Active
        s.transition(false); // → Cooldown
        let timer = s.state_timer().unwrap();
        assert_eq!(timer.total.unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn state_timer_elapsed_in_active() {
        let mut s = PollScheduler::new();
        s.transition(true); // → Active
        let timer = s.state_timer().unwrap();
        assert!(timer.total.is_none());
    }

    #[test]
    fn state_timer_elapsed_in_long_idle() {
        let mut s = PollScheduler::new();
        s.transition(false);
        s.force_expire_idle();
        s.transition(false); // → LongIdle
        let timer = s.state_timer().unwrap();
        assert!(
            timer.total.is_none(),
            "LongIdle should be elapsed, not countdown"
        );
    }

    // --- tick_filled() tests ---

    #[test]
    fn tick_filled_zero_at_cycle_start() {
        let timer = StateTimer::elapsed(Instant::now());
        assert_eq!(timer.tick_filled(Duration::from_secs(5), 5), 0);
    }

    #[test]
    fn tick_filled_one_per_second_active() {
        // Active: 5s interval, 5 ticks → 1 tick per second
        let timer = StateTimer::elapsed(Instant::now() - Duration::from_secs(2));
        assert_eq!(timer.tick_filled(Duration::from_secs(5), 5), 2);
    }

    #[test]
    fn tick_filled_wraps_at_interval() {
        // 7 seconds elapsed, 5s interval → cycle_secs = 2
        let timer = StateTimer::elapsed(Instant::now() - Duration::from_secs(7));
        assert_eq!(timer.tick_filled(Duration::from_secs(5), 5), 2);
    }

    #[test]
    fn tick_filled_idle_one_per_six_seconds() {
        // Idle: 30s interval, 5 ticks → 1 tick per 6 seconds
        let timer = StateTimer::elapsed(Instant::now() - Duration::from_secs(12));
        assert_eq!(timer.tick_filled(Duration::from_secs(30), 5), 2);
    }

    #[test]
    fn tick_filled_long_idle_one_per_sixty_seconds() {
        // LongIdle: 300s interval, 5 ticks → 1 tick per 60 seconds
        let timer = StateTimer::elapsed(Instant::now() - Duration::from_secs(180));
        assert_eq!(timer.tick_filled(Duration::from_secs(300), 5), 3);
    }

    #[test]
    fn tick_filled_max_is_four_not_five() {
        // At 4s into 5s cycle: 4*5/5 = 4 (never reaches 5)
        let timer = StateTimer::elapsed(Instant::now() - Duration::from_secs(4));
        assert_eq!(timer.tick_filled(Duration::from_secs(5), 5), 4);
    }

    #[test]
    fn tick_filled_countdown_uses_same_epoch() {
        // Countdown timers also use started for tick computation
        let timer = StateTimer::countdown(
            Instant::now() - Duration::from_secs(3),
            Duration::from_secs(60),
        );
        assert_eq!(timer.tick_filled(Duration::from_secs(5), 5), 3);
    }

    #[test]
    fn tick_filled_zero_interval_returns_zero() {
        let timer = StateTimer::elapsed(Instant::now() - Duration::from_secs(5));
        assert_eq!(timer.tick_filled(Duration::from_secs(0), 5), 0);
    }
}
