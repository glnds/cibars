# cibars: Build Plan

This document describes the intended architecture, module breakdown, and phased delivery plan. It is the primary reference for AI-assisted development (e.g. Claude Code).

---

## 1. Technology Choices

| Concern | Choice | Rationale |
|---|---|---|
| TUI framework | `ratatui` | Actively maintained successor to `tui-rs`, good tmux support |
| Async runtime | `tokio` | Standard for async Rust; needed for concurrent polling |
| AWS SDK | `aws-sdk-codepipeline` | Official AWS SDK for Rust |
| GitHub API | `octocrab` | Well-maintained GitHub API client for Rust |
| CLI argument parsing | `clap` (derive API) | Ergonomic, widely used |
| Error handling | `anyhow` | Simple, idiomatic error propagation |
| Logging (debug) | `tracing` + `tracing-subscriber` (to file) | Avoids polluting the TUI; log to `~/.cibars/cibars.log` |

---

## 2. Module Structure

```
cibars/
├── Cargo.toml
├── README.md
├── SPEC.md
├── PLAN.md
└── src/
    ├── main.rs           # Entry point: parse args, validate env, start runtime
    ├── config.rs         # Config struct populated from CLI args + env vars
    ├── app.rs            # Central App state; owns all Bar data
    ├── poller/
    │   ├── mod.rs        # Poller orchestrator: spawns both polling tasks
    │   ├── aws.rs        # AWS CodePipeline polling; maps results to BuildStatus
    │   └── github.rs     # GitHub Actions polling; maps results to BuildStatus
    ├── ui/
    │   ├── mod.rs        # UI entry point: renders full frame via ratatui
    │   ├── header.rs     # Header bar: region, repo, current time
    │   ├── bar.rs        # Tick bar widget: renders a single Bar row with lap-aware colors
    │   └── statusbar.rs  # Bottom bar: last poll time, key hints, warnings
    └── model.rs          # Shared types: Bar, Tick, BuildStatus, BarSource, App
```

---

## 3. Data Flow

```
main()
  └── parse Config from CLI + env
  └── validate Config (fail fast if invalid)
  └── init App state (Arc<Mutex<App>>)
  └── spawn poller task (tokio::spawn)
  │     └── every 30s: poll AWS + GitHub concurrently (tokio::join!)
  │     └── update App state via Mutex
  │     └── send render signal via tokio::sync::watch channel
  └── run UI event loop (main thread)
        └── on render signal: re-render frame via ratatui
        └── on key event: handle r / q
        └── on terminal resize: re-render or show resize warning
```

---

## 4. Shared State

`App` is wrapped in `Arc<Mutex<App>>` and shared between the poller task and the UI loop.

```rust
pub struct App {
    pub pipelines: Vec<PipelineEntry>,
    pub actions: Vec<ActionRun>,
    pub last_poll: Option<DateTime<Utc>>,
    pub warnings: Vec<String>,
}
```

The UI loop holds the lock only for the duration of a single frame render. The poller holds the lock only during state replacement after a completed poll cycle.

---

## 5. Model Types

```rust
pub enum BuildStatus {
    Running,
    Succeeded,
    Failed,
    Idle,
}

pub struct Bar {
    pub name: String,
    pub source: BarSource,
    pub status: BuildStatus,
    pub fill: usize,        // number of '|' characters currently written
    pub write_pos: usize,   // current insertion index within the fill area
    pub gone: bool,
}

pub enum BarSource {
    CodePipeline,
    GitHubAction,
}

pub struct App {
    pub bars_pipelines: Vec<Bar>,
    pub bars_actions: Vec<Bar>,
    pub last_poll: Option<DateTime<Utc>>,
    pub warnings: Vec<String>,
}
```

On each poll cycle the poller updates each `Bar` as follows:

- If the build is **still running**: increment `write_pos`; if `write_pos >= tick_area_width` reset to 0 and reset `fill` to 0; increment `fill`; set `status = Running`
- If the build **just completed**: set `status = Succeeded | Failed`; do not change `fill` or `write_pos`
- If **no build is active**: leave the bar unchanged

---

## 6. Phased Delivery

### Phase 1: Scaffold and CLI

- Set up `Cargo.toml` with all dependencies
- Implement `config.rs`: parse and validate CLI args + `GITHUB_TOKEN`
- Implement `main.rs`: validate config, print parsed config to stdout, exit
- Goal: `cargo run -- --aws-profile foo --region eu-west-1 --github-repo acme/backend` works without panicking

### Phase 2: Data Model and Polling

- Implement `model.rs`: `Bar`, `BuildStatus`, `BarSource`, `App`
- Implement `Bar::update(status, tick_area_width)` with wrap logic
- Implement `poller/aws.rs`: call `ListPipelines` + `GetPipelineState`, map to `BuildStatus` per pipeline
- Implement `poller/github.rs`: call GitHub Actions runs endpoint, map latest run per workflow to `BuildStatus`
- Implement `poller/mod.rs`: orchestrate concurrent polling, call `bar.update()` on all bars, create new bars for newly discovered pipelines/workflows
- Goal: polling works end-to-end and prints bar state as text to stdout for verification

### Phase 3: TUI Skeleton

- Implement `ui/mod.rs`: initialise ratatui, enter alternate screen, restore on exit
- Implement `app.rs` with `Arc<Mutex<App>>` and `tokio::sync::watch` channel
- Render a static placeholder layout (header, two empty bar sections, status bar)
- Goal: a blank but correct TUI renders in tmux without corrupting the terminal

### Phase 4: Populated UI

- Implement `ui/bar.rs`: render a single `Bar` row; draw each `Tick` with lap-aware brightness (current lap = bright, previous lap = dim)
- Implement `ui/header.rs` and `ui/statusbar.rs`
- Apply bar ordering: running bars first, then idle, both sorted by name within each group
- Goal: full live tick-bar UI with real data

### Phase 5: Polish and Edge Cases

- Handle terminal too small
- Inline warnings in status bar for poll errors and rate limits
- Graceful `SIGINT`/`SIGTERM` handling (restore terminal state)
- `tracing` logging to `~/.cibars/cibars.log`
- Manual refresh via `r` key
- Goal: production-ready binary

---

## 7. Testing Strategy

| Layer | Approach |
|---|---|
| Model mapping | Unit tests with fixture JSON from AWS/GitHub API responses |
| Poller | Integration tests against LocalStack (CodePipeline) and GitHub API mock (deferred -- not yet implemented) |
| UI | Manual verification in tmux; snapshot tests via `ratatui` test helpers if needed; key handler logic extracted into testable functions with unit tests |
| CLI parsing | Unit tests via `clap` test utilities |

---

## 8. Development Notes for Claude Code

- Always run `cargo clippy` and `cargo fmt` before considering a phase complete
- Never use `unwrap()` in production paths; use `?` with `anyhow::Context`
- The UI event loop runs on the main thread; all async work happens in spawned tasks
- The `ratatui` terminal must be restored (alternate screen exited) on any panic or exit path — use a drop guard pattern
- Keep each module focused: `poller/` modules must not contain any UI code; `ui/` modules must not call any external APIs
