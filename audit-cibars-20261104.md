# cibars review — 2026-11-04

- Repository: `glnds/cibars`, trunk `master`.
- Baseline: `be0cef4c2ea1d6afbde24832b4d626a67c8e3f4e`.
- Scope: Rust runtime, polling, linkage, hooks, UI, manifests, CI and user documentation.
- Method: source review, independent agents, failing regressions before fixes, local checks.
- Source locations below refer to the baseline unless marked as follow-up locations.

## Executive summary

Good foundations: focused modules, extensive unit tests, pinned tools, hardened Actions and
dependency scanning. The largest concrete risk was hook composition bypassing pre-push checks.
Polling also had several paths to incomplete or stale status. Nine findings were addressed;
five follow-ups remain. No Critical findings; two High findings addressed.

| Category | High | Medium | Low | Addressed | Open |
| --- | --- | --- | --- | --- | --- |
| Hook safety | 2 | 0 | 0 | 2 | 0 |
| Resilience and correctness | 0 | 9 | 0 | 6 | 3 |
| Cost efficiency | 0 | 1 | 0 | 0 | 1 |
| Documentation and monitoring | 0 | 0 | 2 | 1 | 1 |
| Total | 2 | 10 | 2 | 9 | 5 |

## Addressed findings

| ID | Severity | Finding and baseline evidence | Result |
| --- | --- | --- | --- |
| H-01 | High | Appending successful boost commands masks a failing existing hook; `src/config.rs:L378-L417`. | Preserve original interpreter, arguments and exit status in a separate executable. Reject shell dispatchers referencing `$0`; refuse backup collisions. |
| H-02 | High | Global hook consumes stdin before local delegation; `src/config.rs:L262-L266,L345-L370`. | Replay complete ref input separately to both hooks; propagate failures; guard self-delegation. |
| M-01 | Medium | Duplicate-only runs page stops discovery before older workflows; `src/poller/github.rs:L46-L60,L171-L189`. | Continue scans, use workflow inventory for safe early stopping, retain incremental page-one polls. |
| M-02 | Medium | Jobs beyond the first 100 silently omitted; `src/poller/github.rs:L217-L242`. | Paginate jobs; test a failing 101st job and its timestamp. |
| M-03 | Medium | One pipeline error discards all successful updates; `src/poller/mod.rs:L413-L417`. | Apply successes, retain failed-fetch state, distinguish listed pipelines from deleted pipelines, name errors. |
| M-04 | Medium | Last action hides earlier running/failed stage actions; `src/poller/mod.rs:L451-L460`. | Aggregate action statuses and use latest completion timestamp. |
| M-05 | Medium | Boost never clears cached auth failure; `src/main.rs:L303-L314`, `src/poller/mod.rs:L163-L165,L335`. | Explicit boost permits retry; SIGUSR1 retains auth gating. |
| M-06 | Medium | Gone groups and bars keep polling active; linkage can turn a live workflow green from a gone pipeline; `src/app.rs:L223-L231`, `src/linkage.rs:L472-L513,L577-L586`. | Exclude gone entities from activity, pending links, completion recording and suppression. |
| L-01 | Low | README says startup makes no calls despite initial boost; `README.md:L208-L209`, `src/main.rs:L80`. | Document initial poll, independent SSO checks and hook behavior. |

Hook tests execute actual scripts in temporary repositories, including `false`, `exit`, `exec`,
stdin consumption, arguments, permissions, symlinks and legacy migration. No application launch or
mutation of the user's real hooks was needed. Unsafe legacy appended snippets expose the install
hint, allowing migration through the existing UI.

## Open findings

### M-07 — Bound external operations and initial discovery

Evidence: `src/poller/github.rs:L73-L78`, `src/main.rs:L33-L44,L85-L115`,
`src/sso_health.rs:L9-L24`. GitHub constructs a default client without explicit timeouts.
The pinned octocrab source defaults connect/read/write timeouts to `None`. AWS config bounds
credential loading but sets no operation deadline. Discovery precedes interruptible polling.
A stalled upstream can prevent status refresh or relink completion indefinitely.

Recommendation: explicit client deadlines plus bounded discovery, with stalled-response tests.
Effort: Small. Difficulty: Medium; requires a stalled network or upstream.
AWS documents operation timeout as unset by default in its
[timeout guidance](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/timeouts.html).

### M-08 — Replace obsolete linkage state after successful empty discovery

Evidence: `src/linkage.rs:L418-L449`. Cache writes and assignment replacement happen only with
nonempty links. Removing the last link can leave obsolete job assignments in memory and obsolete
links on disk. Restart reloads them.

Recommendation: distinguish failed discovery from an authoritative empty result; persist the
empty result and clear assignments. Test seeded cache → successful empty discovery → restart.
Effort: Small. Difficulty: Low; normal workflow changes trigger it.

### M-09 — Match literal S3 buckets as well as keys

Evidence: `src/linkage.rs:L374-L385,L639-L645`. Matching ignores bucket identity. Identical keys
in distinct literal staging/production buckets can create false links and assignments.

Recommendation: require equality for literal buckets; handle unresolved workflow expressions
explicitly. Keep expression-based discovery working. Effort: Small. Difficulty: Low.

### M-10 — Preserve the watched-PR lifetime cap across rediscovery

Evidence: `src/app.rs:L162-L183`, `src/main.rs:L152-L160`, `src/poller/mod.rs:L278-L281`.
Expired PR entries are removed, but a still-visible historical run adds the PR back with a fresh
first-seen time. During Cooldown this can return polling to Active indefinitely, defeating the
intended one-hour cap and increasing API usage.

Recommendation: retain expiry identity while that PR remains in the visible run set, with bounded
retention. Test expiry, immediate rediscovery and eventual Sleep together.
Effort: Small. Difficulty: Low; requires an unmerged PR still in the fetched history.

### L-02 — Separate logs for concurrent repositories

Evidence: `src/main.rs:L281-L292,L338-L341`. PID ownership is per repository, but every instance
truncates the same `~/.cibars/cibars.log`. Starting another repository erases the first instance's
diagnostics; concurrent file offsets can also overwrite records.

Recommendation: per-repository log paths or an append-only shared writer with rotation.
Effort: Small. Difficulty: Low; requires concurrent repositories.

## Architecture and operational assessment

| Dimension | Assessment |
| --- | --- |
| Access controls | Satisfactory: uses supplied GitHub/AWS credentials; no service-facing auth layer. |
| Cryptography | Satisfactory: standard TLS clients, no custom cryptography identified. |
| Configuration | Moderate: clear source selection; hook migration has compatibility constraints. |
| Error handling | Improved: independent AWS failures no longer discard every result. |
| Observability | Moderate: visible warnings and structured logs; shared-log collision remains. |
| Resilience | Moderate: missing operation deadlines and stale linkage remain. |
| Dependency hygiene | Strong: lockfile, Renovate, cargo-deny, pinned Actions and tools. |
| Cost posture | Satisfactory: sleep scheduling and incremental polls; PR cap needs repair. |
| Documentation | Satisfactory after correcting startup behavior. |
| Testing | Strong breadth; new regressions cover actual HTTP and shell boundaries. |

No deployed infrastructure or containers are defined here; service health endpoints, cloud
retention policies and production resource sizing are outside this CLI's scope. No new
dependencies were added. Larger modules mostly contain colocated tests; file length alone was
not treated as a defect.

## Validation

Regression tests were observed failing before fixes. Final local check, coverage and push/CI
results are recorded in the completion message. Dependency audit passed advisories, bans,
licenses and sources, with existing duplicate-version warnings. Baseline GitHub CI and Security
Scans were green. Semgrep and Trivy were unavailable locally; CI supplies the configured SAST
gate. No live AWS credentials or production infrastructure were accessed.

## Assumptions

- This is a CLI review; user authorized fixes and direct pushes to `master`.
- Pipeline listing determines existence; a failed state fetch does not prove deletion.
- Stage priority follows existing pipeline behavior: Running, Failed, Succeeded, Idle.
- Existing 1,800-run scan cap remains authoritative. Never-run or filtered workflows can force
  scans to that cap; a workflow older than the cap may still be omitted.
- Existing custom hook-manager delegation remains authoritative; arbitrary scripts are not
  rewritten. Shell hooks using `$0` require manual integration.
- Tests use fixtures and local servers; live GitHub/AWS behavior was not exercised.

## Recommended follow-up

Address operation deadlines and stale linkage first, then bucket identity and PR lifetime.
Keep changes test-driven and focused. Open findings above are recommendations, not implemented
features or claims of live exploitation.
