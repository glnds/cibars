# LongIdle AWS Auth Health Check

## Context

AWS SSO token expiration goes undetected during `LongIdle` polling state.
`should_poll_aws()` returns `false` for `LongIdle`, so no AWS API call is made
and the app silently shows stale data. The existing `AuthFailed` detection only
triggers reactively when a pipeline poll fails — but that poll never happens in
`LongIdle`.

**Goal:** detect expired AWS SSO credentials during `LongIdle` with minimal
overhead, using a lightweight STS `GetCallerIdentity` probe.

## Design

### 1. New dependency

Add `aws-sdk-sts = "1"` to `Cargo.toml`.

### 2. STS client initialization

Modify `init_aws_client()` in `main.rs` to return both the `AwsPipelineClient`
and an `aws_sdk_sts::Client`, created from the same `aws_config`. This ensures
shared profile, region, and identity cache.

**Signature change:**

```rust
async fn init_aws_client(config: &Config)
    -> (poller::aws::AwsPipelineClient, aws_sdk_sts::Client)
```

The STS client is stored alongside `aws_client` in the poll orchestrator as
`Option<aws_sdk_sts::Client>`.

### 3. Health check function

Add to `src/poller/mod.rs`:

```rust
pub async fn check_aws_auth(
    app: &Arc<Mutex<App>>,
    sts: &aws_sdk_sts::Client,
    profile: &str,
) {
    match sts.get_caller_identity().send().await {
        Ok(_) => {
            let mut a = app.lock().expect("app mutex poisoned");
            if matches!(a.aws_health, SourceHealth::AuthFailed { .. }) {
                a.aws_health = SourceHealth::Healthy;
                a.warnings.retain(|w| !w.starts_with("AWS:"));
            }
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if is_auth_error(&msg) {
                let mut a = app.lock().expect("app mutex poisoned");
                a.aws_health = SourceHealth::AuthFailed { since: Utc::now() };
                a.push_warning(format!(
                    "AWS: SSO session expired — run \
                     `aws sso login --profile {profile}` then press b"
                ));
            }
        }
    }
}
```

Reuses existing `is_auth_error()`. On success, clears any prior `AuthFailed`
state (handles case where user re-authenticates between checks).

### 4. Integration in poll orchestrator

In `run_poll_orchestrator()` main loop, after the existing poll block:

- When `poll_aws` is `false` (i.e., in `Idle`/`LongIdle`/`Watching`) **and**
  scheduler state is `LongIdle` **and** the STS client exists:
  run `check_aws_auth()`.
- The check runs inside the same `tokio::select!` block alongside
  `poll_actions_tick`, so boost interruption still works.

```rust
// Inside the tokio::select! polling branch:
if let Some(aws) = aws_client.as_ref().filter(|_| poll_aws) {
    tokio::join!(
        poller::poll_actions_tick(&app, &gh_client),
        poller::poll_pipelines_tick(&app, aws, &config.aws_profile, force_aws),
    );
} else if let Some(sts) = sts_client.as_ref()
    .filter(|_| scheduler.state() == PollState::LongIdle)
{
    tokio::join!(
        poller::poll_actions_tick(&app, &gh_client),
        poller::check_aws_auth(&app, sts, &config.aws_profile),
    );
} else {
    poller::poll_actions_tick(&app, &gh_client).await;
}
```

### 5. Scope

- Only `LongIdle` state gets the health check — `Idle` and `Watching` are
  unaffected per user's decision to minimize overhead.
- If the AWS client was never initialized (never reached `Active` state), the
  STS client also won't exist, so the health check is a no-op.

## Files to modify

| File | Change |
|---|---|
| `Cargo.toml` | Add `aws-sdk-sts = "1"` |
| `src/main.rs` | Update `init_aws_client` return type; store STS client; add health check branch |
| `src/poller/mod.rs` | Add `check_aws_auth()` function |

## Verification

1. `cargo test` — all existing tests pass
2. `cargo clippy` — no warnings
3. `cargo fmt` — clean
4. Manual test: run cibars, wait for `LongIdle`, expire SSO session
   (`rm ~/.aws/sso/cache/*`), observe warning appears within 5 minutes
5. Manual test: re-authenticate (`aws sso login`), press `b`, confirm
   warning clears and pipelines refresh
