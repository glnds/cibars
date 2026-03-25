# AWS Auth Failure Handling

Graceful degradation when AWS SSO tokens expire mid-session.

## Problem

When an AWS SSO token expires, the app continues showing stale pipeline
data with no clear visual distinction from fresh data. A yellow warning
appears in the status bar but is easy to miss. Polling continues
at normal cadence, spamming failed requests.

## Requirements

- Stale AWS data must be visually distinguishable from fresh data
- Subtle persistent indicator in header (not a modal/overlay)
- Stop AWS polling on auth failure; slow retry every 5 min or
  manual boost
- Auto-recover when credentials become valid again
- GitHub polling unaffected by AWS auth state

## Data Model

New enum in `src/app.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceHealth {
    Healthy,
    AuthFailed {
        since: DateTime<Utc>,
        hint: String,
    },
}
```

New field in `App`:

```rust
pub aws_health: SourceHealth,  // default: Healthy
```

`SourceHealth` is orthogonal to `PollState` — the scheduler manages
cadence, source health manages capability.

## Poller Behavior

### Auth error detection

Extract `is_auth_error()` helper in `src/poller/mod.rs`:

```rust
fn is_auth_error(msg: &str) -> bool {
    msg.contains("ExpiredToken")
        || msg.contains("UnauthorizedException")
        || msg.contains("InvalidIdentityToken")
        || msg.contains("ExpiredTokenException")
        || (msg.contains("AccessDenied")
            && (msg.contains("SSO") || msg.contains("STS")))
}
```

String-based matching on `anyhow::Error` display. Typed extraction
would require `downcast_ref` through multiple AWS SDK layers — not
worth the complexity. The `AccessDenied` + SSO/STS guard avoids
false positives from IAM policy errors.

### On auth error

- Set `app.aws_health = SourceHealth::AuthFailed { since, hint }`
- Push warning (status bar visibility, first occurrence)
- Skip further AWS polling

### On successful poll

- Set `app.aws_health = SourceHealth::Healthy`
- Clear `"AWS:"` warnings (existing behavior)

### Skip logic

In `poll_pipelines_tick`, early-exit when `AuthFailed` unless a
boost or slow retry is active:

```rust
if matches!(app.aws_health, SourceHealth::AuthFailed { .. })
    && !boost_requested
{
    return;
}
```

### Slow retry (5 min)

The orchestrator tracks `last_aws_auth_retry: Option<Instant>`.
Every 5 minutes while `AuthFailed`, it attempts one probe poll.
On success: `Healthy`, resume normal schedule. On failure: stay
`AuthFailed`, wait another 5 min.

### Boost recovery

`b` press always triggers a poll regardless of `AuthFailed` state.
Success: `Healthy`. Failure: stays `AuthFailed`.

## UI Changes

### Header (`src/ui/header.rs`)

When `AuthFailed`, the profile span turns red with a suffix:

```text
cibars (v0.1-42) | my-profile ⚠ SSO expired | eu-west-1 | ...
                   ^^^^^^^^^^^^^^^^^^^^^^^^^^
                   red (was default color)
```

When `Healthy`, renders as today (default color).

The header is always visible and the profile name is the natural
place to signal "this AWS connection is broken." The status bar
is already crowded.

### Status bar (`src/ui/statusbar.rs`)

No changes. Warnings still fire as-is.

## Files Changed

| File | Change |
| --- | --- |
| `src/app.rs` | `SourceHealth` enum, `aws_health` field |
| `src/poller/mod.rs` | `is_auth_error()`, set health, skip logic |
| `src/poll_scheduler.rs` | No changes |
| `src/ui/header.rs` | Red profile + suffix when `AuthFailed` |
| `src/ui/statusbar.rs` | No changes |
| Orchestrator (main) | `last_aws_auth_retry`, probe every 5 min |

## Out of Scope

- GitHub auth health (same pattern, add later)
- Typed AWS error extraction (YAGNI)
- Per-bar dimming/graying
- Warning buffer size changes

## Testing Strategy

| Test | Layer |
| --- | --- |
| `is_auth_error()` all variants + edge cases | Unit |
| `SourceHealth` transitions (Healthy/AuthFailed) | Unit |
| `poll_pipelines_tick` sets `aws_health` on error | Unit |
| `poll_pipelines_tick` clears `aws_health` on success | Unit |
| Skip-poll-when-AuthFailed behavior | Unit |
| Header renders red profile when `AuthFailed` | Unit |
| Header renders normal when `Healthy` | Unit |
| Existing 322 tests stay green | Regression |
