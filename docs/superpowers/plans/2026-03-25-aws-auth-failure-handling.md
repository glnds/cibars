# AWS Auth Failure Handling — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Gracefully degrade when AWS SSO tokens expire —
stop polling, show persistent header indicator, auto-recover.

**Architecture:** Add `SourceHealth` enum to `App`, extract
`is_auth_error()` helper, add `force` param to
`poll_pipelines_tick` for skip/retry logic, update header to
show red profile when auth fails. Orchestrator checks health
to decide 5-min retry probes.

**Tech Stack:** Rust, ratatui, tokio, chrono, anyhow

**Spec:**
`docs/superpowers/specs/2026-03-25-aws-auth-failure-handling-design.md`

---

## Tasks

### Task 1: SourceHealth enum + App field

**Files:**

- Modify: `src/app.rs:1-54` (add enum, field, default)

- [ ] **Step 1: Write failing test for default aws_health**

In `src/app.rs` test module, add:

```rust
#[test]
fn app_starts_with_healthy_aws() {
    let app = App::new();
    assert_eq!(app.aws_health, SourceHealth::Healthy);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test app_starts_with_healthy_aws`
Expected: FAIL — `SourceHealth` does not exist

- [ ] **Step 3: Add SourceHealth enum and aws_health field**

Note: `src/app.rs` already imports `use chrono::{DateTime, Utc};`
at line 3, so no new import needed.

At top of `src/app.rs`, after existing imports, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceHealth {
    Healthy,
    AuthFailed { since: DateTime<Utc> },
}
```

Add field to `App` struct (after `hook_status`):

```rust
/// AWS authentication health state.
pub aws_health: SourceHealth,
```

Add default in `App::new()`:

```rust
aws_health: SourceHealth::Healthy,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test app_starts_with_healthy_aws`
Expected: PASS

- [ ] **Step 5: Run full suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "add SourceHealth enum and aws_health field to App"
```

---

### Task 2: Extract is_auth_error() helper

**Files:**

- Modify: `src/poller/mod.rs` (add helper fn + tests)

- [ ] **Step 1: Write failing tests for is_auth_error**

In `src/poller/mod.rs` test module, add:

```rust
#[test]
fn is_auth_error_expired_token() {
    assert!(is_auth_error(
        "ExpiredToken: the security token is expired"
    ));
}

#[test]
fn is_auth_error_unauthorized_exception() {
    assert!(is_auth_error(
        "UnauthorizedException: user is not authorized"
    ));
}

#[test]
fn is_auth_error_invalid_identity_token() {
    assert!(is_auth_error("InvalidIdentityToken: bad token"));
}

#[test]
fn is_auth_error_expired_token_exception() {
    assert!(is_auth_error("ExpiredTokenException: token expired"));
}

#[test]
fn is_auth_error_access_denied_with_sso() {
    assert!(is_auth_error(
        "AccessDenied: SSO session has expired"
    ));
}

#[test]
fn is_auth_error_access_denied_with_sts() {
    assert!(is_auth_error(
        "AccessDenied: STS token is invalid"
    ));
}

#[test]
fn is_auth_error_plain_access_denied_is_not_auth() {
    assert!(!is_auth_error(
        "AccessDenied: not authorized to perform action"
    ));
}

#[test]
fn is_auth_error_connection_refused_is_not_auth() {
    assert!(!is_auth_error("connection refused"));
}

#[test]
fn is_auth_error_network_timeout_is_not_auth() {
    assert!(!is_auth_error("network timeout"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test is_auth_error`
Expected: FAIL — `is_auth_error` not found

- [ ] **Step 3: Implement is_auth_error**

In `src/poller/mod.rs`, add above `poll_pipelines_tick`:

```rust
/// Detect AWS auth/credential errors by string matching.
fn is_auth_error(msg: &str) -> bool {
    msg.contains("ExpiredToken")
        || msg.contains("UnauthorizedException")
        || msg.contains("InvalidIdentityToken")
        || msg.contains("ExpiredTokenException")
        || (msg.contains("AccessDenied")
            && (msg.contains("SSO") || msg.contains("STS")))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test is_auth_error`
Expected: all 9 tests PASS

- [ ] **Step 5: Commit**

```bash
git add src/poller/mod.rs
git commit -m "extract is_auth_error() helper with broadened detection"
```

---

### Task 3: Wire is_auth_error into poll_pipelines_tick + set aws_health

**Files:**

- Modify: `src/poller/mod.rs:100-132` (poll_pipelines_tick)

- [ ] **Step 1: Write failing test — auth error sets AuthFailed**

In `src/poller/mod.rs` test module, add:

```rust
#[tokio::test]
async fn poll_expired_token_sets_auth_failed() {
    use crate::app::SourceHealth;
    let app = Arc::new(Mutex::new(App::new()));
    let pipes = ExpiredTokenClient;
    poll_pipelines_tick(&app, &pipes, "my-profile", false).await;

    let a = app.lock().unwrap();
    assert!(
        matches!(a.aws_health, SourceHealth::AuthFailed { .. }),
        "expected AuthFailed, got: {:?}",
        a.aws_health
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test poll_expired_token_sets_auth_failed`
Expected: FAIL — `poll_pipelines_tick` doesn't accept 4th arg

- [ ] **Step 3: Write failing test — successful poll sets Healthy**

```rust
#[tokio::test]
async fn poll_success_clears_auth_failed() {
    use crate::app::SourceHealth;
    let app = Arc::new(Mutex::new(App::new()));
    {
        let mut a = app.lock().unwrap();
        a.aws_health = SourceHealth::AuthFailed {
            since: Utc::now(),
        };
    }
    let pipes = MockPipelineClient {
        pipelines: vec![mock_pipeline(
            "deploy",
            BuildStatus::Succeeded,
            vec![],
        )],
    };
    poll_pipelines_tick(&app, &pipes, "my-profile", true).await;

    let a = app.lock().unwrap();
    assert_eq!(a.aws_health, SourceHealth::Healthy);
}
```

- [ ] **Step 4: Implement — add force param, set aws_health**

Update `poll_pipelines_tick` signature in `src/poller/mod.rs`:

```rust
pub async fn poll_pipelines_tick(
    app: &Arc<Mutex<App>>,
    client: &dyn PipelineClient,
    profile: &str,
    force: bool,
) {
```

Add skip check at the top (BEFORE warning-clear):

```rust
{
    let a = app.lock().expect("app mutex poisoned");
    if matches!(a.aws_health, SourceHealth::AuthFailed { .. })
        && !force
    {
        return;
    }
}
```

In the `Err` branch, replace the inline auth check with
`is_auth_error`:

```rust
Err(e) => {
    let msg = format!("{e:#}");
    let mut a = app.lock().expect("app mutex poisoned");
    if is_auth_error(&msg) {
        a.aws_health = SourceHealth::AuthFailed {
            since: Utc::now(),
        };
        a.push_warning(format!(
            "AWS: SSO session expired \u{2014} \
             run `aws sso login --profile {profile}` \
             then press b"
        ));
    } else {
        a.push_warning(format!("AWS: {msg}"));
    }
}
```

In the `Ok` branch, add after `update_pipeline_groups`:

```rust
a.aws_health = SourceHealth::Healthy;
```

Add `use crate::app::SourceHealth;` to imports at top.

- [ ] **Step 5: Fix ALL existing callers (4th arg = false)**

In `src/poller/mod.rs`, update these call sites:

- `poll_once` test helper (line ~227):

```rust
poll_pipelines_tick(app, pipeline_client, "test-profile", false)
```

- `poll_expired_token_shows_sso_login_hint` test (line ~728):

```rust
poll_pipelines_tick(&app, &pipes, "my-profile", false).await;
```

- `poll_unauthorized_exception_shows_sso_hint` test (line ~1005):

```rust
poll_pipelines_tick(&app, &pipes, "my-profile", false).await;
```

In `src/main.rs:154`, update the production call:

```rust
poller::poll_pipelines_tick(&app, aws, &config.aws_profile, false),
```

- [ ] **Step 6: Run tests to verify new + existing pass**

Run: `cargo test`
Expected: all tests pass (including the two new ones)

- [ ] **Step 7: Commit**

```bash
git add src/poller/mod.rs src/main.rs
git commit -m "wire is_auth_error into poll_pipelines_tick, set aws_health"
```

---

### Task 4: Verify skip + warning preservation + force bypass

These tests validate behavior already implemented in Task 3.
They are verification tests (not TDD) — confirming the skip
path, warning preservation, and force bypass work correctly
as a combined flow.

**Files:**

- Modify: `src/poller/mod.rs` (add verification tests)

- [ ] **Step 1: Add verification test — skip preserves warning**

```rust
#[tokio::test]
async fn poll_skips_when_auth_failed_and_not_forced() {
    use crate::app::SourceHealth;
    let app = Arc::new(Mutex::new(App::new()));
    {
        let mut a = app.lock().unwrap();
        a.aws_health = SourceHealth::AuthFailed {
            since: Utc::now(),
        };
        a.push_warning(
            "AWS: SSO session expired".to_string(),
        );
        a.loading_pipelines = false;
    }
    let pipes = FailingPipelineClient;
    poll_pipelines_tick(&app, &pipes, "my-profile", false).await;

    let a = app.lock().unwrap();
    // Warning should be preserved (not cleared)
    assert!(
        a.warnings.iter().any(|w| w.contains("SSO")),
        "SSO warning should be preserved, got: {:?}",
        a.warnings
    );
    // loading_pipelines should remain unchanged (skip = no-op)
    assert!(!a.loading_pipelines);
}
```

- [ ] **Step 2: Add verification test — force bypasses skip**

```rust
#[tokio::test]
async fn poll_force_bypasses_auth_failed_skip() {
    use crate::app::SourceHealth;
    let app = Arc::new(Mutex::new(App::new()));
    {
        let mut a = app.lock().unwrap();
        a.aws_health = SourceHealth::AuthFailed {
            since: Utc::now(),
        };
    }
    let pipes = MockPipelineClient {
        pipelines: vec![mock_pipeline(
            "deploy",
            BuildStatus::Succeeded,
            vec![],
        )],
    };
    poll_pipelines_tick(&app, &pipes, "my-profile", true).await;

    let a = app.lock().unwrap();
    // Should have recovered
    assert_eq!(a.aws_health, SourceHealth::Healthy);
    assert_eq!(a.pipeline_groups.len(), 1);
}
```

- [ ] **Step 3: Run all tests**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add src/poller/mod.rs
git commit -m "add verification tests for skip + force bypass behavior"
```

---

### Task 5: Header — red profile when AuthFailed

**Files:**

- Modify: `src/ui/header.rs:1-39` (add aws_health field, render)
- Modify: `src/ui/mod.rs:209-216` (pass aws_health to Header)

- [ ] **Step 1: Write failing test — header shows SSO expired**

In `src/ui/header.rs` test module, add:

```rust
use crate::app::SourceHealth;

fn render_header_with_health(
    profile: &str,
    region: &str,
    repo: &str,
    aws_health: &SourceHealth,
) -> Buffer {
    let header = Header {
        profile,
        region,
        repo,
        aws_health,
    };
    let area = Rect::new(0, 0, 100, 1);
    let mut buf = Buffer::empty(area);
    header.render(area, &mut buf);
    buf
}

#[test]
fn renders_sso_expired_when_auth_failed() {
    let health = SourceHealth::AuthFailed {
        since: chrono::Utc::now(),
    };
    let buf = render_header_with_health(
        "my-profile", "eu-west-1", "owner/repo", &health,
    );
    let content = buffer_text(&buf);
    assert!(
        content.contains("SSO expired"),
        "got: {content}"
    );
}

#[test]
fn renders_profile_red_when_auth_failed() {
    let health = SourceHealth::AuthFailed {
        since: chrono::Utc::now(),
    };
    let buf = render_header_with_health(
        "my-profile", "eu-west-1", "owner/repo", &health,
    );
    let content = buffer_text(&buf);
    let profile_pos = content.find("my-profile")
        .expect("profile not found");
    let cell = &buf.content()[profile_pos];
    assert_eq!(cell.fg, Color::Red);
}

#[test]
fn renders_profile_default_when_healthy() {
    let health = SourceHealth::Healthy;
    let buf = render_header_with_health(
        "my-profile", "eu-west-1", "owner/repo", &health,
    );
    let content = buffer_text(&buf);
    assert!(
        !content.contains("SSO expired"),
        "got: {content}"
    );
    let profile_pos = content.find("my-profile")
        .expect("profile not found");
    let cell = &buf.content()[profile_pos];
    assert_eq!(cell.fg, Color::Reset);
}
```

**Note on test approach:** The `buf.content()[pos]` indexing
works because all prior header content is single-width ASCII.
The warning icon (U+26A0) appears after "my-profile" so it
doesn't affect the profile position lookup. This matches the
existing test pattern in header.rs (see `renders_version_in_cyan_bold`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test renders_sso_expired`
Expected: FAIL — `Header` has no `aws_health` field

- [ ] **Step 3: Implement header changes**

In `src/ui/header.rs`, update struct:

```rust
use crate::app::SourceHealth;

pub struct Header<'a> {
    pub profile: &'a str,
    pub region: &'a str,
    pub repo: &'a str,
    pub aws_health: &'a SourceHealth,
}
```

In the `render` method, replace the plain profile span:

```rust
// Profile span: red with suffix when auth failed
let profile_spans = match self.aws_health {
    SourceHealth::AuthFailed { .. } => vec![
        Span::styled(
            self.profile,
            Style::default().fg(Color::Red),
        ),
        Span::styled(
            " \u{26A0} SSO expired",
            Style::default().fg(Color::Red),
        ),
    ],
    SourceHealth::Healthy => vec![
        Span::raw(self.profile),
    ],
};
```

Update the `Line::from` to use `profile_spans`:

```rust
let mut spans = vec![
    Span::styled(
        format!(
            "cibars (v{}-{})",
            env!("CARGO_PKG_VERSION"),
            env!("VERGEN_GIT_COMMIT_COUNT"),
        ),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ),
    Span::raw(" | "),
];
spans.extend(profile_spans);
spans.extend([
    Span::raw(" | "),
    Span::raw(self.region),
    Span::raw(" | "),
    Span::raw(self.repo),
    Span::raw(" | "),
    Span::styled(
        format!("{time}"),
        Style::default().fg(Color::DarkGray),
    ),
]);
let line = Line::from(spans);
```

- [ ] **Step 4: Fix existing header tests**

Update `render_header` helper to pass `&SourceHealth::Healthy`:

```rust
fn render_header(
    profile: &str,
    region: &str,
    repo: &str,
) -> Buffer {
    render_header_with_health(
        profile,
        region,
        repo,
        &SourceHealth::Healthy,
    )
}
```

- [ ] **Step 5: Update Header construction in ui/mod.rs**

In `src/ui/mod.rs:209-215`, update:

```rust
frame.render_widget(
    Header {
        profile,
        region,
        repo,
        aws_health: &app.aws_health,
    },
    areas[idx],
);
```

- [ ] **Step 6: Run all tests**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add src/ui/header.rs src/ui/mod.rs
git commit -m "header: show red profile with SSO expired when AuthFailed"
```

---

### Task 6: Orchestrator — 5-min retry + force on boost

**Files:**

- Modify: `src/main.rs:111-198` (run_poll_orchestrator)

- [ ] **Step 1: Update orchestrator to compute force flag**

In `run_poll_orchestrator`, before the poll block
(around line 150), add:

```rust
// Check if AWS needs a forced retry (5-min probe or boost)
let force_aws = {
    let a = app.lock().expect("app mutex poisoned");
    match &a.aws_health {
        SourceHealth::AuthFailed { since } => {
            let elapsed = Utc::now()
                .signed_duration_since(*since)
                .num_seconds()
                .unsigned_abs();
            elapsed >= 300  // 5 minutes
        }
        SourceHealth::Healthy => false,
    }
};
```

Add `use crate::app::SourceHealth;` and `use chrono::Utc;`
to the imports at the top of `main.rs`.

- [ ] **Step 2: Update poll guard to include force_aws**

The existing guard `filter(|_| need_aws)` would skip the AWS
poll when `force_aws` is true but `need_aws` is false (e.g.,
GH-only Idle cycle). Fix by including `force_aws`:

```rust
let poll_aws = need_aws || force_aws;
if let Some(aws) = aws_client.as_ref().filter(|_| poll_aws) {
    tokio::join!(
        poller::poll_actions_tick(&app, &gh_client),
        poller::poll_pipelines_tick(
            &app, aws, &config.aws_profile, force_aws,
        ),
    );
} else {
    poller::poll_actions_tick(&app, &gh_client).await;
}
```

- [ ] **Step 3: Force AWS on next cycle after boost**

Do NOT call `poll_pipelines_tick` inline from `select!` —
that would race with the next loop iteration. Instead, use
a `force_next_aws` flag that the boost branch sets, consumed
at the top of the next iteration.

Add before the loop:

```rust
let mut force_next_aws = false;
```

Update `force_aws` computation to include it:

```rust
let force_aws = force_next_aws || {
    let a = app.lock().expect("app mutex poisoned");
    match &a.aws_health {
        SourceHealth::AuthFailed { since } => {
            let elapsed = Utc::now()
                .signed_duration_since(*since)
                .num_seconds()
                .unsigned_abs();
            elapsed >= 300
        }
        SourceHealth::Healthy => false,
    }
};
force_next_aws = false; // consumed
```

In the boost branch, set the flag:

```rust
_ = boost_notify.notified() => {
    scheduler.boost();
    force_next_aws = true;
}
```

Same for SIGUSR1:

```rust
_ = sigusr1.recv() => {
    scheduler.boost();
    force_next_aws = true;
    tracing::info!("boost triggered by SIGUSR1");
}
```

This is clean: boost sets a flag, next loop iteration picks
it up and forces the AWS poll. No race.

- [ ] **Step 4: Run full suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 5: Run clippy and fmt**

Run: `cargo clippy`
Run: `cargo fmt`
Expected: no warnings, clean format

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "orchestrator: 5-min retry probe + force on boost when AuthFailed"
```

---

### Task 7: Final verification + push

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 2: Run clippy**

Run: `cargo clippy`
Expected: no warnings

- [ ] **Step 3: Run fmt check**

Run: `cargo fmt -- --check`
Expected: clean

- [ ] **Step 4: Manual smoke test (optional)**

Run with an expired AWS profile to verify:

```bash
cargo run -- --aws-profile expired-profile \
    --region eu-west-1 --github-repo owner/repo
```

Verify: header shows red profile with "SSO expired".

- [ ] **Step 5: Push to master**

```bash
git push origin master
```
