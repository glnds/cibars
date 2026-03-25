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
    AuthFailed { since: DateTime<Utc> },
}
```

The `since` field is used by the orchestrator to decide when to
attempt the next 5-min retry probe.

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

- Set `app.aws_health = SourceHealth::AuthFailed { since }`
- Push warning with SSO login hint (every time auth error occurs,
  including retries — warnings are cleared per cycle anyway)
- Skip further AWS polling

### On successful poll

- Set `app.aws_health = SourceHealth::Healthy`
- Clear `"AWS:"` warnings (existing behavior)

### Skip logic and parameter threading

Add a `force: bool` parameter to `poll_pipelines_tick`. When
`AuthFailed` and `!force`, early-exit **before** the warning-clear
step (so existing SSO warning is preserved):

```rust
pub async fn poll_pipelines_tick(
    app: &Arc<Mutex<App>>,
    client: &dyn PipelineClient,
    profile: &str,
    force: bool,  // true on boost or 5-min retry
) {
    // Skip check BEFORE warning-clear to preserve SSO warning
    {
        let a = app.lock().expect("app mutex poisoned");
        if matches!(a.aws_health, SourceHealth::AuthFailed { .. })
            && !force
        {
            return;
        }
    }

    // Existing warning-clear (only reached on actual poll attempt)
    {
        let mut a = app.lock().expect("app mutex poisoned");
        a.warnings.retain(|w| !w.starts_with("AWS:"));
    }
    // ... rest of poll logic
}
```

Normal callers pass `force: false`. Boost and retry pass
`force: true`.

### Slow retry (5 min) — orchestrator integration

In `run_poll_orchestrator` (src/main.rs), before the poll call:

```rust
let force_aws = {
    let a = app.lock().expect("app mutex poisoned");
    match &a.aws_health {
        SourceHealth::AuthFailed { since } => {
            // Retry if 5 min elapsed since last failure
            since.signed_duration_since(Utc::now()).num_seconds()
                .abs() >= 300
        }
        SourceHealth::Healthy => false,
    }
};
```

On retry: call `poll_pipelines_tick` with `force: true`. On
success, `aws_health` is set to `Healthy` inside the tick fn.
On failure, `since` is updated to `Utc::now()` (resets the
5-min timer).

Lock duration is minimal — only read `aws_health`, release,
then call the tick function.

### Boost recovery

Boost (via existing `AtomicBool` flag) passes `force: true` to
`poll_pipelines_tick`. Success: `Healthy`. Failure: stays
`AuthFailed` with updated `since`.

## UI Changes

### Header (`src/ui/header.rs`)

When `AuthFailed`, the profile span turns red with a hardcoded
suffix (not from the enum — the enum only stores `since`):

```text
cibars (v0.1-42) | my-profile ⚠ SSO expired | eu-west-1 | ...
                   ^^^^^^^^^^^^^^^^^^^^^^^^^^
                   red (was default color)
```

When `Healthy`, renders as today (default color). The `Header`
widget receives `&SourceHealth` as a new field.

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
| Orchestrator (main) | `force` flag, 5-min retry check |

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
| Skip preserves SSO warning (no clear before return) | Unit |
| Force=true bypasses skip (boost/retry path) | Unit |
| 5-min retry triggers after `since` threshold | Unit |
| Boost when AuthFailed → success → Healthy (e2e) | Unit |
| Header renders red profile when `AuthFailed` | Unit |
| Header renders normal when `Healthy` | Unit |
| Existing tests stay green | Regression |
