# Hook Status Indicator & Push Signal Feedback

## Context

The pre-push hook (`pkill -USR1 cibars`) is already implemented but lacks two pieces
of UI feedback:

1. No **positive confirmation** that the hook is installed — only a prompt when it's
   missing
2. No **distinct visual feedback** when SIGUSR1 arrives from a push — it looks
   identical to a manual `b` boost

This design adds both: a static hook-installed indicator and a transient push-received
flash.

## Design

### Feature 1: Static Hook Indicator

In the statusbar, between keybinding hints and `l=relink`:

| HookStatus     | Rendered                    | Color                       |
|----------------|-----------------------------|-----------------------------|
| `Installed`    | `✓hook`                     | `STATUS_SUCCESS` `#00ff7f`  |
| `Missing`      | `h=install pre-push hook`   | `STATUS_RUNNING` `#f0c050`  |
| `Incomplete`   | `h=install pre-push hook`   | `STATUS_RUNNING` `#f0c050`  |
| `NoGitDir`     | *(omitted)*                 | —                           |

`Missing`/`Incomplete` rendering is unchanged from current behavior.

### Feature 2: Push Signal Feedback

When SIGUSR1 is received, the `✓hook` indicator is temporarily replaced:

- **Label:** `⬆ pushed!`
- **Color:** `POLL_SCAN` (`#4897d4`) — scan-blue, btop `temp_start`
- **Duration:** 1.5s (`PUSH_SIGNAL_DURATION`)
- **After expiry:** reverts to `✓hook`
- **No boost flash** on `b=boost` text — only the hook indicator reacts

Manual `b` press continues to flash `b=boost` in `BOOST_FLASH` red as before.

### Data Flow

```text
git push → .git/hooks/pre-push → pkill -USR1 cibars
  → tokio::select! SIGUSR1 branch
    → app.push_signal_at = Some(Instant::now())
    → scheduler.boost()
  → StatusBar render:
    if push_signal_at.elapsed() < 1.5s → "⬆ pushed!" (scan-blue)
    else if hook_status == Installed    → "✓hook" (green)
    else if Missing/Incomplete          → "h=install pre-push hook" (orange)
```

## Changes

### `src/app.rs`

- Add field `push_signal_at: Option<Instant>` to `App`
- Initialize to `None` in `App::new()`

### `src/main.rs`

- In the `tokio::select!` SIGUSR1 branch: set `app.push_signal_at =
  Some(Instant::now())` (in addition to existing `scheduler.boost()`)
- Manual `b` boost branch: unchanged (does NOT set `push_signal_at`)

### `src/ui/statusbar.rs`

- Add `PUSH_SIGNAL_DURATION: Duration = Duration::from_millis(1500)`
- Add `push_signal_at: Option<Instant>` field to `StatusBar` struct
- Render logic for the hook section:
  1. If `push_signal_at` is recent (< 1.5s): render `⬆ pushed!` in `POLL_SCAN`
  2. Else if `Installed`: render `✓hook` in `STATUS_SUCCESS`
  3. Else if `Missing`/`Incomplete`: render `h=install pre-push hook` in
     `STATUS_RUNNING` (unchanged)
  4. `NoGitDir`: omit

### `src/ui/mod.rs`

- Pass `push_signal_at` from `App` to `StatusBar` constructor

## Color Palette (btop-aligned)

| Element             | Constant         | Hex       | btop origin    |
|---------------------|------------------|-----------|----------------|
| `✓hook` (installed) | `STATUS_SUCCESS` | `#00ff7f` | `proc_misc`    |
| `⬆ pushed!` (flash) | `POLL_SCAN`     | `#4897d4` | `temp_start`   |
| `h=install` (warn)  | `STATUS_RUNNING` | `#f0c050` | `cpu_mid`      |

No new theme constants needed — all colors already exist in `theme.rs`.

## Verification

1. `cargo test` — all existing + new tests pass
2. `cargo clippy` — no warnings
3. `cargo fmt` — clean
4. Manual test: run cibars, confirm `✓hook` shows green in statusbar
5. Manual test: `pkill -USR1 cibars` from another terminal, confirm `⬆ pushed!`
   appears in blue for ~1.5s then reverts to `✓hook`
6. Manual test: press `b`, confirm only `b=boost` flashes red (no hook indicator
   change)
