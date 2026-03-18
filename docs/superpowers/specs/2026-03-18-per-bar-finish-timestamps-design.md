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

## Poller Changes

### GitHub Actions (`src/poller/github.rs`)

Extract `completed_at` from job response when mapping to `Bar`.
The GitHub API returns this as ISO 8601 for completed jobs,
`null` for in-progress jobs. Maps directly to
`Option<DateTime<Utc>>`.

### AWS CodePipeline (`src/poller/aws.rs`)

Extract `last_status_change` from `ActionState::latest_execution`.
Populate `last_finished` only for terminal states
(Succeeded/Failed/Abandoned). Set `None` for InProgress.

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
  build [||||----] 14:28
  test  [||------] 14:32
CodePipelines
  * deploy-pipe
    Source [|||||]  14:25
    Build  [|||--]
```

`Build` has no timestamp because it is still running.

## Testing

### Unit tests (model)

- `Bar` with `last_finished` set and `None`
- UTC-to-local formatting logic

### Unit tests (poller mapping)

- GH job-to-Bar extracts `completed_at`
- AWS stage-to-Bar extracts `last_status_change` for terminal
  states
- AWS stage-to-Bar sets `None` for InProgress

### UI snapshot tests

- Bar with timestamp renders `[bar] HH:MM`
- Bar without timestamp renders full-width bar
- Collapsed view has no timestamps
