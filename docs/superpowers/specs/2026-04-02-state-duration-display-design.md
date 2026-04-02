# State Duration Display

## Context

The status bar shows the current polling state (Slow, Sleep, Scan:GH, Fast, Cool) with a
progress tick animation, but only Cooldown shows how long it will remain in that state. Users
have no visibility into when Idle will transition to LongIdle, when Watching will expire, or
how long Active has been running. Adding duration indicators to all states (except LongIdle)
gives users better situational awareness.

## Design

### Per-state display

| State | Timer type | What to show | Duration constant |
|---|---|---|---|
| Idle | Countdown | Time until LongIdle | `IDLE_TO_LONG_DURATION` (5min) |
| LongIdle | None | No timer (excluded) | — |
| Watching | Countdown | Time until auto-reset to Idle | `COOLDOWN_DURATION` (60s) |
| Active | Elapsed | Time spent in Active | N/A |
| Cooldown | Countdown | Time until Idle (exists today) | `COOLDOWN_DURATION` (60s) |

### Display format

Timer appears as a separate span after the progress ticks, matching existing cooldown
placement:

```text
Slow ▮▮▯▯▯ │ 3m12s │ e=expand b=boost q=quit
Scan:GH ▮▮▯▯▯ │ 42s │ e=expand b=boost q=quit
Fast ▮▮▯▯▯ │ 1m05s │ e=expand b=boost q=quit
Cool ▮▮▯▯▯ │ 42s │ e=expand b=boost q=quit
Sleep ▮▮▯▯▯ │ e=expand b=boost q=quit
```

Duration formatting: `Xm YYs` when >= 60s, `XXs` when < 60s.

The existing "Cooldown: 42s" text simplifies to just the duration (e.g. "42s") for
consistency with other states — the state label already identifies which state it is.

## Changes

### `src/poll_scheduler.rs`

- Add `active_started: Option<Instant>` field to `PollScheduler`
- Set `active_started = Some(Instant::now())` on every transition **into** Active
  (from Idle, LongIdle, Watching, or Cooldown)
- Clear `active_started = None` when leaving Active
- Add three new public methods mirroring `cooldown_remaining()`:
  - `idle_remaining() -> Option<Duration>` — returns `IDLE_TO_LONG_DURATION - elapsed`
    when in Idle and `idle_started` is set; `None` otherwise
  - `watching_remaining() -> Option<Duration>` — returns `COOLDOWN_DURATION - elapsed`
    when in Watching and `watching_started` is set; `None` otherwise
  - `active_elapsed() -> Option<Duration>` — returns elapsed since `active_started`
    when in Active; `None` otherwise

### `src/app.rs`

- Add three new fields:
  - `idle_remaining: Option<Duration>`
  - `watching_remaining: Option<Duration>`
  - `active_elapsed: Option<Duration>`
- Initialize all to `None` in `App::new()`

### `src/main.rs` (~line 188)

After `scheduler.transition(any_running)`, set:

```rust
a.poll_state = scheduler.state();
a.cooldown_remaining = scheduler.cooldown_remaining();
a.idle_remaining = scheduler.idle_remaining();
a.watching_remaining = scheduler.watching_remaining();
a.active_elapsed = scheduler.active_elapsed();
```

### `src/ui/statusbar.rs`

- Add `idle_remaining`, `watching_remaining`, `active_elapsed` fields to `StatusBar`
- Add `fn format_duration(d: Duration) -> String` helper (private to module)
- Replace the existing cooldown-specific rendering block with a unified timer span:
  compute "the duration to display" from whichever of the four `Option<Duration>` fields
  is `Some`, format it, and render after the ticks
- Update existing tests; add tests for each state's timer rendering

## Verification

1. `cargo test` — all existing + new tests pass
2. `cargo clippy` — no warnings
3. `cargo fmt` — clean
4. Manual: run the app and observe:
   - Idle shows countdown from 5:00 to 0:00 then transitions to Sleep
   - Boost (`b`) enters Watching with 60s countdown
   - When builds start, Active shows elapsed time climbing
   - When builds finish, Cooldown shows 60s countdown
   - Sleep shows no timer
