# Poll State Color Display

## Context

The status bar currently collapses 5 `PollState` variants into 3 plain-text labels
(Slow/Long/Fast) with no color. This loses information: `Watching`, `Active`, and
`Cooldown` all display as "Fast" despite being semantically different. Users can't
tell at a glance whether the app is actively polling AWS, scanning GitHub only, or
winding down.

## Design

Give each `PollState` a unique label and color, using btop-palette colors that
convey urgency (dim=dormant, warm=active).

### State Mapping

| PollState  | Label   | Color const | RGB             | btop source   |
|------------|---------|-------------|-----------------|---------------|
| `LongIdle` | Sleep   | `POLL_SLEEP`| `(85, 85, 85)`  | `inactive_fg` |
| `Idle`     | Slow    | `POLL_SLOW` | `(135, 135, 95)` | `mem_box`    |
| `Watching` | Scan:GH | `POLL_SCAN` | `(72, 151, 212)` | `temp_start` |
| `Active`   | Fast    | `POLL_FAST` | `(240, 80, 80)`  | `cpu_end`    |
| `Cooldown` | Cool    | `POLL_COOL` | `(95, 135, 135)` | `cpu_box`    |

### Rendering

The label **and** filled ticks are colored with the state color. Empty ticks stay
`FG_DIM`. This replaces the current unstyled `Span::raw`.

```text
Before:  Fast ▮▮▮▯▯   (plain text, all states look alike)
After:   Fast ▮▮▮▯▯   (red-orange label+ticks when Active)
         Cool ▮▮▯▯▯   (cyan label+ticks when Cooldown)
         Scan:GH ▮▯▯▯▯ (blue label+ticks when Watching)
```

## Files to Modify

- `src/ui/theme.rs` -- add 5 `POLL_*` color constants
- `src/ui/statusbar.rs` -- expand label match to 5 arms, apply color to label
  and filled ticks

## Verification

1. `cargo test` -- all existing + new tests pass
2. `cargo clippy` -- no warnings
3. `cargo fmt` -- clean
4. Manual: run app and observe each state:
   - Start idle -> "Slow" in muted yellow
   - Wait 5 min -> "Sleep" in dim gray
   - Press `b` -> "Scan:GH" in blue
   - Pipeline starts -> "Fast" in red-orange
   - Pipeline finishes -> "Cool" in cyan, with countdown
   - Cooldown expires -> back to "Slow"
