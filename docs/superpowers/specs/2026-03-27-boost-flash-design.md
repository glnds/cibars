# Boost Key Visual Feedback — Color Flash

## Context

When pressing `b` to boost polling, there is no immediate visual feedback in
the TUI. The only indication is an indirect state label change (Slow → Fast)
which can feel delayed. Users need instant confirmation that their keypress
was received.

## Design

Flash the "b=boost" hint text gold for 750ms on every `b` press.

### Behavior

- Every `b` press triggers the flash, regardless of poll state (even if
  boost is a no-op in Active/Cooldown/Watching)
- Flash color: `STATUS_RUNNING` (RGB 240, 192, 80) — existing theme color
- Duration: 750ms — ~3 redraw frames at 250ms tick rate
- Flash resets on repeated presses (re-stamps the timer)

### State

Add to `App` struct:

```rust
pub boost_pressed_at: Option<Instant>,
```

### Render Logic

In `StatusBar` widget, check elapsed time since `boost_pressed_at`:

- If `Some(t)` and `t.elapsed() < 750ms` → render "b=boost" with
  `STATUS_RUNNING` foreground color
- Otherwise → render "b=boost" with default color (as today)

The `StatusBar` widget gets a new `boost_pressed_at: Option<Instant>` field
passed from the UI render loop.

### Key Press Handling

In the `b` key handler (ui/mod.rs), after `boost_notify.notify_one()`:

```rust
app.boost_pressed_at = Some(Instant::now());
```

### Files to Modify

| File | Change |
|---|---|
| `src/app.rs` | Add `boost_pressed_at: Option<Instant>` field + Default |
| `src/ui/statusbar.rs` | Add field to widget, conditional color in render |
| `src/ui/mod.rs` | Set timestamp on `b` press, pass to StatusBar |

### Verification

1. `cargo test` — all existing + new tests pass
2. `cargo clippy` — no warnings
3. `cargo fmt` — clean
4. Manual: run app, press `b`, observe gold flash on "b=boost"
5. Manual: press `b` while already in Active state — still flashes
6. Manual: rapid `b` presses — flash resets cleanly each time
