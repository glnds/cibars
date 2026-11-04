# Fast Startup Implementation Plan

> **For agentic workers:** Use superpowers:subagent-driven-development task by task.

**Goal:** Display known structure immediately and publish live status progressively.

**Architecture:** Keep persistent topology separate from execution observations and linkage.
Hydrate the app synchronously; stream GitHub discovery and job results into existing app state.
Initialize external sources independently and reconcile only authoritative successful results.

**Tech Stack:** Existing Rust, tokio, futures, serde/TOML, ratatui and octocrab.

**Spec:** `docs/superpowers/specs/2026-11-04-fast-startup-design.md`.

## Global Constraints

- Master only; no new dependencies, branches, worktrees, PRs or infrastructure operations.
- GPT-6.1 Sol implementation agents. No worker-spawned subagents.
- TDD for every behavior; record observed red/green commands in task reports.
- Keep `mise run check` and the 88% coverage floor green. Fish shell commands.
- No production `unwrap()`. Keep I/O out of UI; avoid holding app locks across awaits.
- Names from cache are neutral; cached execution state is forbidden.
- Follow spec assumptions, including offline AWS profile/region scope.

## Review Focus

- Corrupt or mismatched cache cannot leak unrelated rows or block startup: Task 1 tests.
- Partial/failed inventory cannot delete known workflows: Tasks 1 and 2 tests.
- New run IDs cannot inherit completed job statuses from another run: Task 2 tests.
- Cancellation cannot strand detached mutation tasks or overwrite newer status: Tasks 2 and 3 tests.
- Empty successful discovery must differ from failed discovery: Task 3 tests.

## Task 1: Persist neutral structure and render cached names

**Files:** Create `src/topology.rs`; modify `src/main.rs`, `src/ui/mod.rs`, `.gitignore`.
Tests reside in those Rust modules. Use existing app/model types without a broad model redesign.

**Interfaces:** `TopologyCache::capture(app: &App, config: &Config) -> Self`,
`TopologyCache::hydrate(&self, app: &mut App, config: &Config)`,
`load_topology(path: &Path) -> Result<Option<TopologyCache>>`, and
`save_topology(path: &Path, cache: &TopologyCache) -> Result<()>` in `topology`.
These APIs remain usable by Task 3 after startup orchestration changes.

- [ ] Add failing round-trip, neutral-status, source-mismatch/disabled-source, corrupt/unknown-schema,
  empty-inventory, and offline-hydration tests. Assert names/order survive; run IDs, status,
  timestamps and running activity do not. Add a UI test where assignment names have no live bars;
  assert those labels are visible and no success/failure marker appears.
- [ ] Run targeted tests and record expected failures before production edits.
- [ ] Implement versioned source-scoped topology serialization and atomic replacement. Capture only
  non-gone groups/bars. Preserve data from failed refresh by capturing retained app state. Hydrate
  before creating async clients, and classify using current config. Save after successful cycle
  processing; avoid rewriting an unchanged snapshot. Corruption warns and falls back to discovery.
- [ ] Load legacy link assignments early enough for the first render when both sources are enabled.
  Render a neutral fallback bar for each assigned name lacking a live bar; use existing styling.
  Keep legacy cache backward compatibility. Ignore `.cibars-topology.toml` and its temp file.
- [ ] Run targeted tests and full tests; format, self-review, and commit only Task 1 files.

## Task 2: Publish inventory, runs and jobs progressively

**Files:** Modify `src/poller/github.rs`, `src/poller/mod.rs`; adjust app/model/UI only if required
for explicit run identity or neutral inventory rows. Preserve Task 1 public cache interface.

**Interfaces:** Add `WorkflowInventoryEntry { id: u64, name: String }` and
`ActionsUpdate::{Inventory(Vec<WorkflowInventoryEntry>), Runs(RunsPage)}` in `poller`.
Add `ActionsClient::stream_latest_runs(&self, updates:
tokio::sync::mpsc::UnboundedSender<ActionsUpdate>) -> Result<()>` with a default adapter to
`list_latest_runs` for existing mocks. Production GitHub overrides it using shared discovery code;
`list_latest_runs` remains a collecting wrapper, not a second divergent discovery algorithm.
Inventory events carry only complete authoritative inventory; run events are additive updates.

- [ ] Add failing gated-future/local HTTP tests: first-page jobs appear while later discovery waits;
  fast jobs appear while another job stalls; never-run workflow gets a neutral row; late error
  preserves partial progress; successful empty inventory marks cached rows gone; failed inventory
  does not. Assert a newer run resets old terminal job state and stale results cannot overwrite it.
- [ ] Add targeted-lookup tests for sparse workflows, stale active runs, filtered-only/never-run
  workflows, pagination, branch escaping, PR harvesting and bounded inventory/history fallback.
  Preserve behavioral coverage of current pagination tests; update route-specific fixtures if needed.
- [ ] Observe failing tests, then implement streaming updates. `poll_actions_tick` consumes discovery
  and `FuturesUnordered` job completions concurrently, keeps app locks short, and updates each job
  result immediately. Keep futures owned by the tick so cancelling it cancels unfinished work.
  Reconcile deletion only from complete inventory, never from partial run pages. Reset jobs to
  neutral when a workflow's run ID changes; reject results for superseded run IDs.
- [ ] Discover workflow names/IDs independently of runs. Publish recent-page runs immediately;
  use workflow-specific run queries for missing inventory entries or known active runs absent
  from the page. Keep bot/skipped/branch filtering. A workflow-specific empty result finishes
  that lookup rather than forcing repository-history exhaustion. Cap each targeted scan at the
  existing 60-page limit, with bounded concurrency (4). Retain bounded repository scanning only
  as fallback when inventory is unavailable. Never replace good cache with incomplete results.
  Inventory refresh retains the existing twentieth-call cadence; recent page stays the normal poll.
- [ ] Bound GitHub HTTP operations to 15 seconds using tokio timeout and descriptive errors.
  Surface job/discovery failures in warnings; preserve existing rate-limit backoff. Use explicit
  latest-run wording/run ID in workflow status presentation without persisting live observations.
- [ ] Run targeted tests, full tests and clippy; format, self-review, and commit Task 2 files.

## Task 3: Decouple startup and reconcile linkage safely

**Files:** Modify `src/main.rs`, `src/linkage.rs`, `src/poller/github.rs`, `README.md`; tests local.
Use Task 1 topology APIs and Task 2 streaming poller.

**Interfaces:** Change `discover_links` to
`Result<(LinkMap, Vec<WorkflowFile>)>`; propagate partial definition/file failures. Make
`run_discovery` return `Result<LinkMap>` and only replace caller's mapping on `Ok`.
Extract a testable startup coordinator accepting an AWS initialization future and source clients;
its concrete naming is local to `main.rs` and consumed nowhere else.

- [ ] Add failing tests: stalled AWS initialization cannot block GitHub rows/jobs; warm hydration
  precedes client initialization; stalled linkage cannot block initial status; interruption drops
  pending work; successful empty discovery clears memory/cache and survives restart; failed/partial
  discovery preserves prior mapping and clears the discovering flag. Cover both source-only modes.
- [ ] Observe failures. Start GitHub polling independently while AWS initializes. Do not gate the
  first live poll on linkage discovery. Keep initialization/discovery futures owned by orchestrator,
  cancellable on shutdown, without detached tasks able to overwrite newer app state. Bound AWS
  initialization/discovery waits to 15 seconds per operation and report failure rather than hanging.
  Preserve signal/boost/relink behavior and avoid consuming the initial Grace budget in setup.
- [ ] Persist topology after live updates/cycles, including useful partial state before an interrupt;
  loaded names stay visible if a source fails. Reconcile inventory changes through the poller's
  authoritative rules. Relink retries discovery; successful empty results replace old assignments
  and disk cache, failures retain prior data. GitHub workflow file fetching must report partial
  failures instead of silently returning an incomplete successful discovery.
- [ ] Update README for neutral startup cache, progressive refresh, cache scope and latest-run
  meaning. Document assumptions inline. Run `rumdl check` on changed Markdown.
- [ ] Run `mise run check`, `mise run coverage`, review the final diff, and commit Task 3 files.

## Validation and delivery

Per-task reviews verify spec compliance and quality; final review checks combined cancellation,
cache invalidation, deletion, scheduler and source-isolation behavior. Use existing mock clients
and local HTTP fixtures; no real builds or remote changes required. Commit on master; publishing
is not part of this implementation request. Report checks, assumptions and remaining limitations.
