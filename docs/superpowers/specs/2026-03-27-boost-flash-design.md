# Boost Key Visual Feedback — Color Flash

## Context

When pressing `b` to boost polling, there is no immediate visual feedback in
the TUI. The only indication is an indirect state label change (Slow → Fast)
which can feel delayed. Users need instant confirmation that their keypress
was received.

## Design

Flash the "b=boost" hint text for 750ms on every `b` press.

### Behavior

- Every `b` press triggers the flash, regardless of poll state (even if
  boost is a no-op in Active/Cooldown/Watching)
- Flash color: new `BOOST_FLASH` theme constant (RGB 255, 85, 85) — matches
  btop's `hi_fg` (#ff5555), the designated keyboard-shortcut highlight color
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
  `BOOST_FLASH` foreground color
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
| `src/ui/theme.rs` | Add `BOOST_FLASH` color constant (RGB 255, 85, 85) |
| `src/app.rs` | Add `boost_pressed_at: Option<Instant>` field + Default |
| `src/ui/statusbar.rs` | Add field to widget, conditional color in render |
| `src/ui/mod.rs` | Set timestamp on `b` press, pass to StatusBar |

### TDD Order

1. Add `BOOST_FLASH` constant to theme + test for correct RGB value
2. Test: `App::default()` has `boost_pressed_at: None`
3. Test: StatusBar renders "b=boost" in `BOOST_FLASH` color when
   `boost_pressed_at` is within 750ms
4. Test: StatusBar renders "b=boost" in default color when
   `boost_pressed_at` is `None` or expired
5. Test: StatusBar renders "b=boost" in default color when
   `boost_pressed_at` is > 750ms ago
6. Implement each piece to make the corresponding test pass
7. Wire up `b` key handler in ui/mod.rs

### Verification

1. `cargo test` — all existing + new tests pass
2. `cargo clippy` — no warnings
3. `cargo fmt` — clean
4. Manual: run app, press `b`, observe flash on "b=boost"
5. Manual: press `b` while already in Active state — still flashes
6. Manual: rapid `b` presses — flash resets cleanly each time
