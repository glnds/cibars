# Fast startup

## Intent

Implement the approved recommendations in `docs/2026-11-04-startup-investigation.md`.
Known workflow/job/pipeline names appear before network work. Fresh execution results arrive
progressively, without waiting for unrelated history or jobs.

## Behavior

Persist a separate `.cibars-topology.toml` snapshot of workflow names/jobs and pipeline names/stages.
Persist structure only: never restore status, run IDs, timestamps, animation, or scheduler activity.
Hydrate matching cached structure before starting async clients. Missing, corrupt, incompatible,
or differently scoped caches fall back to discovery; report corruption without aborting startup.
Use atomic replacement. Exclude gone entities, retain structure after failed/partial refresh, and
remove obsolete entities only after an authoritative successful inventory/detail response.

Scope each source independently: GitHub repository and branch; AWS profile and region. Disabled
sources cannot create rows. Classify restored workflows using current configuration. Cache linkage
separately, and accept its legacy format; names in a loaded assignment must render even when no
live job exists. A missing live bar displays a neutral placeholder, never a success/failure claim.

On first use, publish workflow inventory and recent runs without waiting for complete history.
Publish each workflow's jobs when available. Retain sparse workflows, existing branch/bot/skipped
filters, PR harvesting before those filters, rate-limit behavior, and interrupted-poll safety.
Use workflow-specific queries for missing workflows and stale active runs when inventory is known;
bound requests and preserve partial progress on failure. Never infer workflow deletion merely from
absence on a run page. A successful empty inventory is authoritative; a failed inventory is not.

GitHub polling must proceed while AWS initializes or linkage discovery runs. Cache hydration must
also work offline and in either single-source mode. Existing boost, push signal, and terminal
cleanup behavior remain intact. Bound external waits, make failures visible, and do not silently
turn partial discovery into an empty authoritative result.

Successful empty linkage discovery clears saved and in-memory links/assignments. Failed or partial
discovery retains the prior successful mapping. The relink key still requests fresh discovery.
Historical live results remain tied to their run ID, with explicit latest-run wording where shown;
they must not imply that the local HEAD passed. No execution results are persisted.

## Constraints and assumptions

- Work directly on master; no branches, worktrees, PRs, dependencies, or infrastructure changes.
- Tests fail before implementation; full check and coverage floor remain required.
- Existing Rust, tokio, serde/TOML, futures and ratatui dependencies suffice.
- Assumption: topology usually changes slowly, but every successful refresh can revise it.
- Assumption: AWS profile/region identifies the configured local source before credentials load.
  Account identity is unavailable offline; remapping a profile can temporarily show old neutral
  names until live refresh. Never reuse cached execution state across accounts.
- Assumption: existing workflow-name identity is retained; redesigning duplicate-name handling is
  outside scope. Preserve IDs in live discovery where needed for targeted API queries.
- No startup latency promise without production measurement; deterministic tests prove ordering.

## Acceptance

Warm startup displays neutral names before any API response. First use displays an inventory
workflow with no eligible run. Fast jobs appear while another request is held pending. A late
history failure retains already published data. Sparse workflows and stale running executions
still refresh. Source/config changes cannot restore unrelated rows. Empty discovery removes old
structure; failed refresh preserves it. Existing checks, coverage floor and terminal behavior pass.
