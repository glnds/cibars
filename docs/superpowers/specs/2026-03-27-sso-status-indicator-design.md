# AWS SSO Session Status Indicator

## Context

When AWS SSO is expired, the header shows a red `⚠ SSO expired` warning — but
when SSO is valid, there's no positive indication. The profile name just appears
in default color, which is ambiguous: is it healthy, or has it not been checked
yet? This change adds a visible "valid" indicator so users can confirm at a
glance that their SSO session is working.

## Design

### Three-State `SourceHealth` Enum

Extend the existing enum with an `Unknown` variant:

```rust
pub enum SourceHealth {
    Unknown,                            // before first AWS poll
    Healthy,                            // AWS poll succeeded
    AuthFailed { since: DateTime<Utc> },// auth error detected
}
```

App initializes with `Unknown` (currently starts as `Healthy`).

### Visual States in Header

| State        | Profile display                    | Color           |
|--------------|------------------------------------|-----------------|
| `Unknown`    | `my-profile ✓`                     | dim gray        |
| `Healthy`    | `my-profile ✓`                     | green           |
| `AuthFailed` | `my-profile ⚠ SSO expired`        | red (unchanged) |

The `✓` uses Unicode U+2713 (CHECK MARK).

### State Transitions

```text
Unknown ──[first AWS poll succeeds]──▶ Healthy
Unknown ──[first AWS poll auth error]──▶ AuthFailed { since }
Healthy ──[subsequent auth error]──▶ AuthFailed { since }
AuthFailed ──[retry succeeds]──▶ Healthy
```

No active polling for session validity — the existing poll cycle handles it.
The status is set as a side effect of the normal AWS pipeline poll, which
already runs at startup.

### Timing

The `Unknown` state lasts only until the first AWS poll completes (typically
< 2 seconds after startup). After that, the indicator is either green or red.

## Files to Modify

| File | Change |
|------|--------|
| `src/app.rs` | Add `Unknown` variant, change default to `Unknown` |
| `src/ui/header.rs` | Render green `✓` for Healthy, dim `✓` for Unknown |
| `src/ui/theme.rs` | (optional) add `CHECK_MARK` constant |
| `src/poller/mod.rs` | Set `Healthy` on successful AWS poll |
| `src/main.rs` | Update match arms for `Unknown` (treat like `Healthy` for polling) |

## Existing Code to Reuse

- `SourceHealth` enum in `src/app.rs:10-13`
- Header auth rendering in `src/ui/header.rs:39-49`
- `theme::STATUS_SUCCESS` (green) and `theme::FG_DIM` (gray) colors
- `is_auth_error()` in `src/poller/mod.rs:99-107`

## Verification

1. `cargo test` — all existing + new tests pass
2. `cargo clippy` — no warnings
3. `cargo fmt` — clean
4. Manual: launch with valid SSO → green ✓ appears after ~1s
5. Manual: launch with expired SSO → red ⚠ SSO expired (as before)
6. Manual: observe dim ✓ briefly at startup before first poll
