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

`PollScheduler` in `src/poll_scheduler.rs` controls what gets polled
and how often. AWS CodePipeline depends on GitHub Actions — AWS is
only polled when GitHub detects running builds.

**Startup:** The very first poll cycle always polls both GH and AWS
(`needs_initial_poll` flag), regardless of state. This gives
immediate visibility into current pipeline status. After the first
`transition()` call the flag clears and normal state rules apply.

```text
              boost (b key)
    ┌──────────────────────────┐
    │                          ▼
  Idle ──────────────────► Watching
  30s GH, no AWS           5s GH, no AWS
    ▲                          │
    │                          │ GH finds running
    │                          ▼
  Cooldown ◄──────────── Active
  5s GH+AWS               5s GH+AWS
  60s timer                    │
    │                          │ nothing running
    └──────────────────────────┘
```

| State | GH interval | Poll AWS? | Entry |
|---|---|---|---|
| Idle | 30s | No | Startup, or cooldown expired |
| Watching | 5s | No | User pressed `b` from Idle |
| Active | 5s | Yes | GH detects running builds |
| Cooldown | 5s | Yes | Nothing running (from Active) |

**Transitions:**

- Idle + boost → Watching
- Idle + GH running → Active
- Watching + GH running → Active
- Watching + 60s timeout → Idle
- Active + nothing running → Cooldown
- Cooldown + running → Active
- Cooldown + 60s → Idle
- Boost is no-op in Active/Cooldown

**Boost signal:** `Arc<AtomicBool>` shared between UI thread and
poll orchestrator. UI sets flag on `b` press; orchestrator swaps it
and calls `scheduler.boost()`.

## Testing

| Layer | Approach |
|---|---|
| Model/mapping | Unit tests with fixture JSON |
| CLI parsing | Unit tests via clap test utilities |
| Poller | Integration tests against mocked APIs |
| UI | Snapshot tests via ratatui test helpers |
