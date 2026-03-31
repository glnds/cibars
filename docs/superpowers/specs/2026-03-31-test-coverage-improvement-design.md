# Test Coverage Improvement Design

**Date:** 2026-03-31
**Baseline:** 507 tests passing, all inline `#[cfg(test)]`
**Target:** ~557 tests (+50 new tests across 3 layers)

## Approach: Layered (Unit gaps -> E2E scenarios -> Signal/lifecycle)

## Layer 1: Unit Test Gap Fill

### poller/mod.rs (~20 new tests)

**`is_auth_error()` direct coverage (9 tests):**

- `is_auth_error_expired_token` — `"ExpiredToken"` match
- `is_auth_error_unauthorized_exception` — `"UnauthorizedException"` match
- `is_auth_error_invalid_identity_token` — `"InvalidIdentityToken"` match
- `is_auth_error_expired_token_exception` — `"ExpiredTokenException"` match
- `is_auth_error_access_denied_sso` — `"AccessDenied"` + `"SSO"` combo
- `is_auth_error_access_denied_sts` — `"AccessDenied"` + `"STS"` combo
- `is_auth_error_loading_credentials` — `"error occurred while loading credentials"`
- `is_auth_error_access_denied_alone_not_auth` — `"AccessDenied"` without SSO/STS = false
- `is_auth_error_unrelated_message` — random error = false

**`stage_status_and_timestamp` edge cases (4 tests):**

- `stage_status_and_timestamp_empty_actions` — empty slice -> (Idle, None)
- `stage_status_and_timestamp_single_running` — last action Running -> no timestamp
- `stage_status_and_timestamp_succeeded_with_time` — Succeeded -> returns timestamp
- `stage_status_and_timestamp_failed_with_time` — Failed -> returns timestamp

**`check_aws_auth` transitions (3 tests):**

- `check_aws_auth_recovery_clears_warning` — AuthFailed -> Healthy clears warnings
- `check_aws_auth_healthy_stays_healthy` — no-op when already Healthy
- `check_aws_auth_non_auth_error_ignored` — non-auth error doesn't set AuthFailed

**Other gaps (4 tests):**

- `classify_workflows_applies_config_category` — config -> category mapping
- `update_workflow_jobs_missing_group_noop` — no panic when workflow not found
- `poll_pipelines_tick_skips_when_auth_failed` — early return without force
- `poll_pipelines_tick_forces_when_auth_failed_and_force` — polls despite AuthFailed

### ui/mod.rs (~5 new tests)

- `maybe_advance_tick_advances_when_interval_elapsed` — tick after interval
- `maybe_advance_tick_no_advance_before_interval` — tick stays if too early
- `maybe_advance_tick_advances_on_first_call` — no prior advance -> advances
- `maybe_advance_tick_adapts_to_state_change` — interval changes with poll state
- `build_pipeline_centric_has_correct_group_count` — correct section grouping

### linkage.rs (~6 new tests)

- `topo_sort_diamond_dependency` — A->B, A->C, B->D, C->D
- `topo_sort_linear_chain` — A->B->C strict order
- `topo_sort_independent_jobs` — no deps = stable order
- `topo_sort_missing_dep_skipped` — dep not in map -> no crash
- `assign_jobs_complex_shared_deps` — shared deps across pipelines
- `assign_jobs_all_orphans` — no S3 uploads -> all shared

### app.rs (~4 new tests)

- `clear_warnings_by_prefix_removes_matching` — removes "AWS:" prefixed
- `clear_warnings_by_prefix_keeps_others` — "GitHub:" survives "AWS:" clear
- `clear_warnings_by_prefix_no_match_noop` — empty result when no match
- `clear_warnings_by_prefix_multiple_matches` — all matching removed

## Layer 2: Multi-poll E2E Scenarios (~10 new tests)

All in `poller/mod.rs`, reusing `MockPipelineClient`/`MockActionsClient`.

- `e2e_pipeline_lifecycle_idle_running_succeeded` — 3 polls: Idle -> Running ->
  Succeeded. Verify summary_status and stage statuses at each step.
- `e2e_pipeline_lifecycle_running_to_failed` — Running -> Failed. Verify
  `has_any_running()` transitions true -> false.
- `e2e_workflow_appears_then_goes_gone` — Poll 1: CI exists. Poll 2: CI gone.
  Verify `gone=true` but group still in list.
- `e2e_workflow_reappears_after_gone` — 3 polls: present -> gone -> present.
  Verify `gone` resets to false.
- `e2e_auth_recovery_flow` — Poll 1: ExpiredToken -> AuthFailed. Poll 2:
  `check_aws_auth` succeeds -> Healthy, warnings cleared. Poll 3: normal poll.
- `e2e_rate_limit_backoff_and_recovery` — Poll 1: 403 -> rate_limited_until set.
  Poll 2: within backoff -> skipped. Expire. Poll 3: normal poll succeeds.
- `e2e_linkage_gh_completes_cp_starts` — GH "CI" Running, CP "deploy" Idle with
  matching S3 keys. Apply linkage. GH completes, CP starts -> linked jobs marked.
- `e2e_multi_pipeline_independent_lifecycle` — Two pipelines evolve independently.
  Verify each tracked correctly.
- `e2e_classify_workflows_across_polls` — Categories persist across poll cycles
  and new workflow appearances.
- `e2e_concurrent_pipeline_and_workflow_updates` — Both sources return data.
  Verify no interference between pipeline and workflow groups.

## Layer 3: Signal/Lifecycle E2E (~5 new tests)

All in `main.rs`.

- `e2e_double_boost_stays_watching` — Two consecutive Boost interrupts. Verify
  scheduler stays Watching, `push_signal_at` still None.
- `e2e_sigusr1_then_boost_both_set_watching` — SIGUSR1 then Boost. Both work,
  state stays Watching.
- `e2e_boost_does_not_set_push_signal` — Boost through full scheduler cycle never
  touches `push_signal_at`.
- `e2e_pid_file_overwritten_on_restart` — Write PID twice. Content is current PID.
- `e2e_pid_file_creates_parent_dirs` — Nested non-existent path succeeds.

## Implementation Notes

- All tests are inline `#[cfg(test)] mod tests` — no separate `tests/` dir
- Reuse existing mock infrastructure (`MockPipelineClient`, `MockActionsClient`,
  `MockAuthHealthClient`)
- Add new mock variants only when needed (e.g., `MockAuthHealthClient` for
  `check_aws_auth` tests if not already present)
- E2E signal tests use `tempfile::tempdir()` for PID file isolation
- TDD: write failing test first, then verify it fails for the right reason
