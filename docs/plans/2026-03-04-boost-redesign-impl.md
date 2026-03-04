# Boost Redesign Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans
> to implement this plan task-by-task.

**Goal:** Replace the flaky boost mechanism with a 3-tier polling
state machine (Idle/Watching/Active/Cooldown) where boost triggers
fast GitHub-only polling, and AWS polling only activates when GitHub
detects running builds.

**Architecture:** `PollScheduler` gains a `Watching` state and a
`boost()` method. The `watch::channel` boost signal is replaced with
an `Arc<AtomicBool>` that the UI sets and the orchestrator
atomically swaps. The orchestrator loop always polls GitHub, and
only polls AWS when the scheduler says so.

**Tech Stack:** Rust, ratatui, tokio, crossterm

---

## Task 1: Add Watching state + boost() to PollScheduler

**Files:**

- Modify: `src/poll_scheduler.rs`

### Step 1: Write failing tests for Watching state

Add these tests to the existing `#[cfg(test)] mod tests` block in
`src/poll_scheduler.rs` (after line 226):

```rust
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
        s.watching_started =
            Some(Instant::now() - Duration::from_secs(61));
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
```

### Step 2: Run tests to verify they fail

Run: `cargo test -p cibars poll_scheduler 2>&1 | tail -30`

Expected: compilation errors — `PollState::Watching` does not exist,
`boost()` method not found, `watching_started` field not found.

### Step 3: Implement Watching state

Replace the entire `src/poll_scheduler.rs` content with:

```rust
use std::time::{Duration, Instant};

const IDLE_INTERVAL: Duration = Duration::from_secs(30);
const ACTIVE_INTERVAL: Duration = Duration::from_secs(5);
const COOLDOWN_DURATION: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollState {
    Idle,
    Watching,
    Active,
    Cooldown,
}

pub struct PollScheduler {
    state: PollState,
    cooldown_started: Option<Instant>,
    pub(crate) watching_started: Option<Instant>,
    needs_initial_poll: bool,
}

impl PollScheduler {
    pub fn new() -> Self {
        Self {
            state: PollState::Idle,
            cooldown_started: None,
            watching_started: None,
            needs_initial_poll: true,
        }
    }

    pub fn state(&self) -> PollState {
        self.state
    }

    pub fn boost(&mut self) {
        if self.state == PollState::Idle {
            self.state = PollState::Watching;
            self.watching_started = Some(Instant::now());
        }
    }

    pub fn should_poll_aws(&self) -> bool {
        self.needs_initial_poll
            || matches!(self.state, PollState::Active | PollState::Cooldown)
    }

    pub fn interval(&self) -> Duration {
        match self.state {
            PollState::Idle => IDLE_INTERVAL,
            PollState::Watching
            | PollState::Active
            | PollState::Cooldown => ACTIVE_INTERVAL,
        }
    }

    pub fn transition(&mut self, any_running: bool) {
        self.needs_initial_poll = false;
        match self.state {
            PollState::Idle => {
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
```

Keep the existing `#[cfg(test)] mod tests` block unchanged — all
old tests should still pass, plus the new ones.

### Step 4: Run all tests

Run: `cargo test -p cibars poll_scheduler 2>&1 | tail -30`

Expected: all tests pass (old + new).

### Step 5: Commit

```bash
git add src/poll_scheduler.rs
git commit -m "feat: add Watching state + boost() to PollScheduler"
```

---

## Task 2: Update statusbar for Watching state + b=boost

**Files:**

- Modify: `src/ui/statusbar.rs:21-28` (filled_ticks match)
- Modify: `src/ui/statusbar.rs:38-41` (label match)
- Modify: `src/ui/statusbar.rs:52` (help text)
- Modify: `src/ui/statusbar.rs:134-137` (test)

### Step 1: Update existing test + add Watching test

In `src/ui/statusbar.rs`, change the `shows_boost_not_refresh` test
(line 134) to check for `b=boost`:

```rust
    #[test]
    fn shows_boost_not_refresh() {
        let content = render_bar(&PollState::Idle, Duration::ZERO, None);
        assert!(content.contains("b=boost"), "got: {content}");
        assert!(!content.contains("r=boost"), "got: {content}");
    }
```

Add a new test after it:

```rust
    #[test]
    fn watching_shows_fast_polling() {
        let content =
            render_bar(&PollState::Watching, Duration::ZERO, None);
        assert!(
            content.contains("Fast Polling:"),
            "got: {content}"
        );
    }
```

### Step 2: Run tests to verify statusbar failures

Run: `cargo test -p cibars statusbar 2>&1 | tail -20`

Expected: `shows_boost_not_refresh` fails (still says `r=boost`),
`watching_shows_fast_polling` fails (no match arm for Watching).

### Step 3: Update statusbar implementation

In `src/ui/statusbar.rs`, make these changes:

**Line 22-25** — `filled_ticks` match: add `Watching` to the fast
arm:

```rust
fn filled_ticks(elapsed: Duration, state: &PollState) -> usize {
    let tick_duration_ms = match state {
        PollState::Idle => 30_000 / NUM_TICKS,
        PollState::Watching
        | PollState::Active
        | PollState::Cooldown => 5_000 / NUM_TICKS,
    };
    let filled = elapsed.as_millis() as u64 / tick_duration_ms;
    filled.min(NUM_TICKS) as usize
}
```

**Line 38-41** — `render` label match: add `Watching`:

```rust
        let label = match self.poll_state {
            PollState::Idle => "Slow",
            PollState::Watching
            | PollState::Active
            | PollState::Cooldown => "Fast",
        };
```

**Line 52** — help text: change `r=boost` to `b=boost`:

```rust
        spans.push(Span::raw(" | e=expand b=boost q=quit"));
```

### Step 4: Run tests

Run: `cargo test -p cibars statusbar 2>&1 | tail -20`

Expected: all pass.

### Step 5: Commit statusbar changes

```bash
git add src/ui/statusbar.rs
git commit -m "feat: statusbar shows Watching state, b=boost shortcut"
```

---

## Task 3: Replace watch::channel with AtomicBool + update orchestrator

**Files:**

- Modify: `src/main.rs:8` (add Ordering import)
- Modify: `src/main.rs:47-111` (run_poll_orchestrator)
- Modify: `src/main.rs:129-130` (channel → AtomicBool)
- Modify: `src/main.rs:140-141` (pass to orchestrator)
- Modify: `src/main.rs:148-154` (pass to UI)

### Step 1: No new tests needed

The orchestrator is not unit-tested (it's the async runtime glue).
Scheduler logic is already tested. This task is wiring only.

### Step 2: Update main.rs

**Replace the `run_poll_orchestrator` signature and body**
(lines 47-111):

```rust
async fn run_poll_orchestrator(
    app: Arc<Mutex<App>>,
    config: Config,
    token: String,
    boost_flag: Arc<AtomicBool>,
) -> Result<()> {
    let (owner, repo) = config
        .github_repo
        .split_once('/')
        .context(
            "github-repo missing '/'; should have been validated in config",
        )?;
    let gh_client = poller::github::GitHubActionsClient::new(
        &token,
        owner.to_string(),
        repo.to_string(),
    )?;

    let mut aws_client: Option<poller::aws::AwsPipelineClient> = None;
    let mut scheduler = PollScheduler::new();

    loop {
        // Consume boost flag (atomic swap, no race)
        if boost_flag.swap(false, Ordering::Relaxed) {
            scheduler.boost();
        }

        let need_aws = scheduler.should_poll_aws();

        // Lazy-init AWS on first need
        if need_aws && aws_client.is_none() {
            tracing::info!("initializing AWS client (first active poll)");
            aws_client = Some(init_aws_client(&config).await);
        }

        // Record poll start for UI tick visualization
        {
            let mut a = app.lock().expect("app mutex poisoned");
            a.last_poll_started = Some(Instant::now());
        }

        // Always poll GitHub; poll AWS only when scheduler says so
        if let Some(aws) = aws_client.as_ref().filter(|_| need_aws) {
            tokio::join!(
                poller::poll_actions_tick(&app, &gh_client),
                poller::poll_pipelines_tick(
                    &app,
                    aws,
                    &config.aws_profile
                ),
            );
        } else {
            poller::poll_actions_tick(&app, &gh_client).await;
        }

        // Transition + update App display state
        let any_running =
            app.lock().expect("app mutex poisoned").has_any_running();
        scheduler.transition(any_running);
        {
            let mut a = app.lock().expect("app mutex poisoned");
            a.poll_state = scheduler.state();
            a.cooldown_remaining = scheduler.cooldown_remaining();
        }

        tracing::debug!(
            state = ?scheduler.state(),
            any_running,
            interval = ?scheduler.interval(),
            "poll cycle complete"
        );

        // Sleep, interruptible by boost flag check
        let interval = scheduler.interval();
        let flag = boost_flag.clone();
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                }
            } => {}
        }
    }
}
```

**Replace lines 129-130** (watch channel → AtomicBool):

```rust
    // AtomicBool for boost (manual poll trigger, no race)
    let boost_flag = Arc::new(AtomicBool::new(false));
```

**Replace lines 140-141** (pass to orchestrator):

```rust
    rt.spawn(async move {
        if let Err(e) = run_poll_orchestrator(
            poll_app, poll_config, token, boost_flag.clone(),
        ).await {
            tracing::error!("poll orchestrator failed: {e:#}");
        }
    });
```

**Replace lines 148-154** (pass to UI): the `run_ui` call now takes
`Arc<AtomicBool>` instead of `watch::Sender`:

```rust
    let result = ui::run_ui(
        app.clone(),
        terminal,
        &config.aws_profile,
        &config.region,
        &config.github_repo,
        boost_flag,
        &term_flag,
    );
```

**Add `Ordering` import** — ensure `use std::sync::atomic::Ordering;`
is present (line 8 area). The file already imports `AtomicBool`; add
`Ordering` to the same line:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
```

**Add `Duration` import** — add to the existing `use std::time`
line:

```rust
use std::time::{Duration, Instant};
```

**Remove** the `tokio::sync::watch` usage — no more `watch` import
needed.

### Step 3: Run build

Run: `cargo build 2>&1 | tail -20`

Expected: compiler errors in `ui/mod.rs` because `run_ui` signature
still expects `watch::Sender`. That's fine — Task 4 fixes it.

### Step 4: Commit (partial — will compile after Task 4)

Don't commit yet; proceed to Task 4.

---

## Task 4: Update UI key binding (r→b) + AtomicBool signal

**Files:**

- Modify: `src/ui/mod.rs:62-70` (run_ui signature)
- Modify: `src/ui/mod.rs:250-252` (key handler)

### Step 1: UI wiring only (no new tests)

Key binding is UI glue. The statusbar test already validates
`b=boost` text.

### Step 2: Update run_ui signature

Change `src/ui/mod.rs` line 68 from:

```rust
    refresh_tx: tokio::sync::watch::Sender<()>,
```

to:

```rust
    boost_flag: Arc<AtomicBool>,
```

### Step 3: Update key handler

Change `src/ui/mod.rs` lines 250-252 from:

```rust
                    KeyCode::Char('r') => {
                        let _ = refresh_tx.send(());
                    }
```

to:

```rust
                    KeyCode::Char('b') => {
                        boost_flag.store(true, Ordering::Relaxed);
                    }
```

### Step 4: Build + test

Run: `cargo build 2>&1 | tail -20`

Expected: compiles clean.

Run: `cargo test 2>&1 | tail -30`

Expected: all tests pass.

### Step 5: Run clippy + fmt

Run: `cargo clippy 2>&1 | tail -20`

Expected: no warnings.

Run: `cargo fmt --check 2>&1`

Expected: clean.

### Step 6: Commit both Task 3 + Task 4

```bash
git add src/main.rs src/ui/mod.rs
git commit -m "feat: AtomicBool boost signal, b key, GH-first polling"
```

---

## Task 5: Update warning message (press r → press b)

**Files:**

- Modify: `src/poller/mod.rs:82`

### Step 1: Change the SSO expiry warning text

On line 82 of `src/poller/mod.rs`, change:

```rust
"AWS: SSO session expired \u{2014} run `aws sso login --profile {profile}` then press r"
```

to:

```rust
"AWS: SSO session expired \u{2014} run `aws sso login --profile {profile}` then press b"
```

### Step 2: Run tests

Run: `cargo test 2>&1 | tail -20`

Expected: all pass (no test checks exact wording of this message
beyond "aws sso login" and profile name).

### Step 3: Commit

```bash
git add src/poller/mod.rs
git commit -m "fix: update SSO hint to reference b key"
```

---

## Task 6: Final verification

### Step 1: Run full test suite

Run: `cargo test 2>&1 | tail -30`

Expected: all tests pass.

### Step 2: Run clippy

Run: `cargo clippy 2>&1 | tail -20`

Expected: no warnings.

### Step 3: Run fmt check

Run: `cargo fmt --check 2>&1`

Expected: clean.

### Step 4: Manual smoke test (optional)

Run: `cargo run -- --aws-profile <profile> --region <region> --github-repo <owner/repo>`

Verify:

- Status bar shows `b=boost`
- Pressing `b` switches to "Fast Polling" (Watching)
- If no GH builds running, returns to Slow after ~60s
- If GH builds running, switches to Active (AWS polls too)
- `r` key does nothing
