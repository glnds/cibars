# Heartbeat LED — Build Detection Indicator

## Context

When monitoring CI pipelines, there's no visual confirmation that the poller is
actively detecting running builds. The status bar shows poll state (`Fast`,
`Slow`, etc.) and tick progress, but nothing says "yes, I see a build right
now." This makes it hard to verify that detected build status is correct —
you have to infer it from bar colors and poll state transitions.

A heartbeat LED solves this: a single `◉` indicator in the status bar that
pulses bright on each poll confirming running builds, then smoothly fades
until the next poll. No builds = stays dim.

## Design

### Symbol and Placement

- **Symbol:** `◉` (`\u{25C9}`, fisheye)
- **Position:** left edge of the status bar, before the poll state label
- **Layout:** `◉ Fast ▮▮▮▯▯ 4s │ e=expand b=boost q=quit │ ...`

### Colors

- **Bright (builds detected):** `STATUS_RUNNING` — `(240, 192, 80)` amber
- **Dim (no builds / faded):** `FG_DIM` — `(85, 85, 85)` gray

### Animation Behavior

**Builds running:**

1. Poll completes and `has_any_running()` returns true
2. LED snaps to full brightness amber `(240, 192, 80)`
3. Each render frame: `lerp_color(bright, dim, elapsed / poll_interval)`
4. LED smoothly fades toward `(85, 85, 85)` over the polling interval
5. Next poll with running builds → snap back to bright, repeat

**No builds running:**

- LED stays at `FG_DIM` `(85, 85, 85)` — no pulsing

**Build starts (idle → running):**

- First poll detecting running builds lights up the LED immediately

**Build ends (running → idle):**

- LED completes its current fade to dim and stays there

### State

Add to `App`:

```rust
/// Instant of last poll that detected running builds (for LED fade)
pub heartbeat_at: Option<Instant>,
```

- Set to `Some(Instant::now())` after each poll where `has_any_running()` is
  true
- Set to `None` when no builds are running (LED stays dim)

### Rendering Logic

In `statusbar.rs`, before the poll state label:

```rust
let led_color = match app.heartbeat_at {
    Some(t) => {
        let elapsed = t.elapsed().as_secs_f32();
        let interval = scheduler.current_interval().as_secs_f32();
        let progress = (elapsed / interval).min(1.0);
        lerp_color(STATUS_RUNNING, FG_DIM, progress)
    }
    None => FG_DIM,
};
```

Reuses existing `lerp_color()` from `ui/bar.rs` (may need to move to
`theme.rs` or a shared util).

### Files to Modify

| File | Change |
|---|---|
| `src/app.rs` | Add `heartbeat_at: Option<Instant>` field |
| `src/ui/statusbar.rs` | Render `◉` with interpolated color |
| `src/ui/theme.rs` | Add `SYMBOL_HEARTBEAT` constant, move `lerp_color()` here |
| `src/poller/mod.rs` | Set `heartbeat_at` after poll completes |
| `src/ui/bar.rs` | Update to use shared `lerp_color()` from theme |

### What This Does NOT Do

- No per-bar LEDs — the global LED consolidates all pipelines
- No "poller alive" signal when idle — status bar ticks already serve that
- No new poll states or interval changes — purely a visual addition

## Verification

1. Start cibars with active builds → LED should pulse amber, fading between
   polls
2. Wait for builds to finish → LED should fade to dim and stay
3. Trigger new build (push + boost) → LED should light up on next poll
4. Verify `lerp_color` refactor doesn't break existing bar gradient rendering
5. Run `cargo test`, `cargo clippy`, `cargo fmt`
