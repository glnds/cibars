# cibars Development Guide

## Stack

- **Language:** Rust (stable toolchain)
- **Key deps:** ratatui, tokio, clap (derive), aws-sdk-codepipeline, octocrab, anyhow, tracing
- **Architecture:** async polling (tokio) + TUI event loop (ratatui) on main thread

## Commands

```bash
cargo build          # compile
cargo test           # run all tests
cargo clippy         # lint (must pass with no warnings)
cargo fmt            # format (must be clean before commit)
cargo run -- --aws-profile <p> --region <r> --github-repo owner/repo
```

## Workflow

- **Trunk-based dev:** commit directly to `master`, no long-lived branches
- **TDD mandatory:** failing test first, minimal implementation, refactor, full suite green
- Run `cargo clippy` and `cargo fmt` before considering any work complete

## Code Rules

- **No `unwrap()` in production code.** Use `?` with `anyhow::Context`
- `unwrap()` is acceptable in tests only
- Keep modules focused: `poller/` has no UI code; `ui/` makes no API calls
- Restore terminal state on all exit paths (drop guard pattern)
- UI event loop on main thread; all async work in spawned tokio tasks
- Shared state via `Arc<Mutex<App>>` with minimal lock duration

## Project Structure

```text
src/
  main.rs            # entry point: parse args, validate, start runtime
  config.rs          # CLI args + env var validation
  app.rs             # central App state
  model.rs           # Bar, BuildStatus, BarSource types
  poller/            # async polling (aws.rs, github.rs)
  ui/                # TUI rendering (header.rs, bar.rs, statusbar.rs)
```

## Polling State Machine

See `README.md` for the full state machine diagram and transitions.

Dev details: `PollScheduler` lives in `src/poll_scheduler.rs`.
Boost signal uses `Arc<AtomicBool>` shared between UI thread and
poll orchestrator — UI sets flag on `b` press, orchestrator swaps
it and calls `scheduler.boost()`. Initial poll uses
`needs_initial_poll` flag to poll both GH+AWS on startup.

## Testing

| Layer | Approach |
|---|---|
| Model/mapping | Unit tests with fixture JSON |
| CLI parsing | Unit tests via clap test utilities |
| Poller | Integration tests against mocked APIs |
| UI | Snapshot tests via ratatui test helpers |
