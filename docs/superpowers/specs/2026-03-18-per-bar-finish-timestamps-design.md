# Per-Bar Finish Timestamps

Show when each job/stage last completed, displayed as absolute
time (`HH:MM`) right-aligned after the progress bar.

## Requirements

- Show last finished time per bar (job or stage), not per group
- Format: absolute local time `HH:MM`
- Placement: right of progress bar, dimmed/dark gray
- No timestamp when bar is running or never completed
- No timestamp in collapsed view — only when expanded
- Data sourced from API responses (no local transition tracking)

## Data Model

Add to `Bar` struct:

```rust
pub last_finished: Option<DateTime<Utc>>
```

No changes to `WorkflowGroup` or `PipelineGroup`.

### Intermediate structs

- `JobInfo` (in `poller/mod.rs`) gets
  `completed_at: Option<DateTime<Utc>>`
- `ActionState` (in `poller/mod.rs`) gets
  `last_status_change: Option<DateTime<Utc>>`

### Data flow through `reconcile_bars`

`reconcile_bars` currently takes `Vec<(String, BuildStatus)>`.
Change the tuple to `(String, BuildStatus, Option<DateTime<Utc>>)`
so the timestamp flows from poller data into `Bar`.

All call sites must be updated:

- `update_pipeline_groups`: `stage_updates` construction passes
  timestamp from `StageState`
- `update_workflow_jobs`: `updates` construction passes timestamp
  from `JobInfo`

Inside `reconcile_bars`, set `bar.last_finished` from the tuple
value — both for existing bars and new bars. This is the single
location for setting `last_finished`; no separate clearing logic
needed. When the poller returns `None` for a running bar, the
tuple naturally carries `None`, which overwrites any previous
value. Follow the existing pattern where `Bar::new()` takes only
a name and fields are set via direct assignment afterward.

## Poller Changes

### GitHub Actions (`src/poller/github.rs`)

Extract `completed_at` from job response when mapping to
`JobInfo`. The GitHub API returns this as ISO 8601 for completed
jobs, `null` for in-progress jobs. Maps directly to
`Option<DateTime<Utc>>`.

### AWS CodePipeline (`src/poller/aws.rs`)

Extract `last_status_change` from the last action in the stage
(the same action that determines stage status via
`stage_status_from_actions`). Populate `last_finished` only for
terminal states. Check the mapped `BuildStatus` — both
`Succeeded` and `Failed` (which includes AWS `Abandoned`)
are terminal. Set `None` for `Running`/`Idle`.

Convert `aws_smithy_types::DateTime` to `chrono::DateTime<Utc>`
(e.g., via epoch millis).

### Rationale

No extra API calls — both timestamps come from responses already
being fetched. Reading the API's own timestamp is more accurate
than detecting transitions locally (avoids poll-interval drift,
survives app restarts).

## UI Rendering (`src/ui/bar.rs`)

Current layout:

```text
[indent][dot] name [progress-bar]
```

New layout when `last_finished` is `Some`:

```text
[indent][dot] name [progress-bar] HH:MM
```

- Reserve 6 chars (space + `HH:MM`) from progress bar width
- Render timestamp in dimmed/dark gray style
- Convert UTC to local time via `chrono::Local`
- When `None`: no timestamp, progress bar gets full width

### Example

```text
GitHub Actions
  build [||||||||] 14:28
  test  [||||||||] 14:32
CodePipelines
  * deploy-pipe
    Source [|||||||||] 14:25
    Build  [|||------]
```

`Build` has no timestamp because it is still running.
`build` and `test` are completed so they show finish times.

Note: example is schematic, not pixel-accurate.

## Testing

### Unit tests (model)

- `Bar` with `last_finished` set and `None`
- UTC-to-local formatting: extract formatting into a function
  that accepts a generic `TimeZone` parameter so tests can use
  `chrono::FixedOffset` for deterministic output

### Unit tests (poller mapping)

- GH job-to-Bar extracts `completed_at`
- AWS stage-to-Bar extracts `last_status_change` for terminal
  states
- AWS stage-to-Bar sets `None` for InProgress

### UI snapshot tests

- Bar with timestamp renders `[bar] HH:MM`
- Bar without timestamp renders full-width bar
- Collapsed view has no timestamps
