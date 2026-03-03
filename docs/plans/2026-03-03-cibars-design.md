# cibars Design Document

## Architecture

Approach A from PLAN.md: watch channel + shared mutex.

- **Main thread:** synchronous TUI event loop via ratatui + crossterm.
  Polls terminal events with ~250ms timeout, checks watch channel for
  render signals.
- **Tokio runtime:** created via `Runtime::new()` (not `#[tokio::main]`
  — main thread stays free for TUI). Single spawned task runs poll
  loop every 30s.
- **Shared state:** `Arc<Mutex<App>>` — poller replaces state after
  each cycle, UI clones what it needs during render.
- **Signal:** `tokio::sync::watch<()>` — poller sends after updating
  state, UI checks each tick.
- **Terminal restore:** drop guard on `Terminal<CrosstermBackend>`
  ensures alternate screen exit on any path (panic, SIGINT, quit).

## Data Model

```rust
pub enum BuildStatus { Running, Succeeded, Failed, Idle }
pub enum BarSource { CodePipeline, GitHubAction }

pub struct Bar {
    pub name: String,
    pub source: BarSource,
    pub status: BuildStatus,
    pub fill: usize,
    pub write_pos: usize,
    pub gone: bool,
}

pub struct App {
    pub bars_pipelines: Vec<Bar>,
    pub bars_actions: Vec<Bar>,
    pub last_poll: Option<DateTime<Utc>>,
    pub warnings: Vec<String>,
}
```

**Bar update per poll cycle:**

- **Running:** increment `write_pos`; if >= `tick_area_width`, reset
  both to 0; increment `fill`
- **Just completed:** set status Succeeded/Failed, freeze fill/pos
- **Idle:** leave unchanged

**Ordering:** running first (by name), then non-running (by name).

## Polling

### AWS CodePipeline (poller/aws.rs)

- `ListPipelines` -> all pipeline names
- `GetPipelineState` per pipeline -> latest execution status
- Map: InProgress->Running, Succeeded->Succeeded,
  Failed/Stopped/Superseded->Failed
- New pipelines: add Bar. Disappeared: `gone = true`

### GitHub Actions (poller/github.rs)

- `GET /repos/{owner}/{repo}/actions/runs` via octocrab
- Group by workflow name, take latest run per workflow
- Map: in_progress/queued->Running, success->Succeeded,
  failure/cancelled->Failed
- Same discovery/gone logic as AWS

### Orchestrator (poller/mod.rs)

- `tokio::join!` both pollers concurrently
- Acquire mutex, update App, release, send watch signal
- On error: add warning to `App.warnings`, leave bars unchanged
- GitHub rate limit: skip polling until reset, display warning

## UI Rendering

**Layout (top to bottom):**

1. Header (1 row):
   `cibars | {profile} | {region} | {repo} | HH:MM:SS`
2. CodePipelines section: title + one row per pipeline bar
3. GitHub Actions section: title + one row per action bar
4. Status bar (1 row):
   `Last poll: HH:MM:SS | r=refresh q=quit | {warnings}`

**Bar rendering:**

- Format: `{name:<N}  [{fill_area}]`
- Name col: longest name in section, capped at ~30 chars
- Fill area: `terminal_width - name_col - 4`
- Colors: yellow=running, green=success, red=failure, grey=idle
- Fill chars: `|` up to `write_pos`, spaces for rest

**Min terminal:** 80x10 — smaller shows centered "Terminal too small".

**Keys:** `q`=quit, `r`=immediate poll trigger.

## Error Handling

| Condition | Behavior |
|---|---|
| Missing/invalid startup args | stderr + exit 1, no TUI |
| Missing GITHUB_TOKEN | stderr + exit 1, no TUI |
| AWS creds expired mid-session | warning in status bar, retry |
| GitHub rate limit | skip until reset, warning |
| Network timeout | warning, retry next cycle |
| No pipelines found | section message |
| No workflow runs found | section message |
| SIGINT/SIGTERM | drop guard restores terminal |
| Panic | panic hook + drop guard restores terminal |

## Testing

| Layer | What | How |
|---|---|---|
| config.rs | CLI parsing | clap try_parse tests |
| model.rs | Bar update logic | Unit: wrap, complete, idle |
| poller/aws.rs | API mapping | Unit with fixture JSON |
| poller/github.rs | API mapping | Unit with fixture JSON |
| poller/mod.rs | Orchestration | Integration with mock traits |
| ui/bar.rs | Bar widget | ratatui Buffer snapshots |

Pollers use trait abstractions (`PipelineClient`, `GitHubClient`)
for mock injection in tests.
