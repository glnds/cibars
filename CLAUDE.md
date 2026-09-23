# cibars Development Guide

## !!! TRUNK-BASED DEV — NO BRANCHES, NO PRs !!!

**Commit and push directly to `master`. NEVER create feature branches or pull requests.**

## Stack

- **Language:** Rust (stable toolchain)
- **Key deps:** ratatui, tokio, clap (derive), aws-sdk-codepipeline, octocrab, anyhow, tracing
- **Architecture:** async polling (tokio) + TUI event loop (ratatui) on main thread

## Tooling

**mise pins every tool version** (rust, cmake, hk, rumdl, typos, trufflehog,
zizmor, cargo-deny, svu) in `mise.toml`/`mise.lock` — never install these ad
hoc. `mise run setup` installs them and registers the `hk` git hooks
(pre-commit: whitespace/EOF, typos, rumdl, trufflehog; pre-push: `cargo fmt
--check`, clippy `-D warnings`, `cargo test`, `cargo-deny`). CI
(`.github/workflows/ci.yml`) runs the identical toolchain via
`jdx/mise-action` and `mise run check`.

## Commands

```bash
mise run setup        # one-time: install pinned tools + git hooks
mise run build         # release build (macOS: ad-hoc codesign)
mise run test           # cargo test --locked
mise run lint            # cargo clippy --locked --all-targets -- -D warnings
mise run fmt              # cargo fmt
mise run check             # fmt --check + clippy + test (CI parity)
mise run audit               # cargo-deny: advisories, licenses, bans, sources
mise run install               # build + `cargo install --path .`
mise run run                     # cargo run against the dev pipeline
mise run version                   # preview next release tag (svu)
```

Equivalent raw cargo commands (`cargo build`, `cargo test`, `cargo clippy
--all-targets -- -D warnings`, `cargo fmt`) still work once `mise run setup`
has installed the pinned rust toolchain.

## !!! TDD IS MANDATORY — NO EXCEPTIONS !!!

**NEVER write implementation code without a failing test first.**
Cycle: write failing test → implement minimum fix → refactor → full suite green.
Do NOT skip this. Do NOT "just quickly fix" something. Test FIRST. Always.

## Workflow

- **Trunk-based dev:** commit directly to `master`, no long-lived branches
- **TDD mandatory:** failing test first, minimal implementation, refactor, full suite green
- Run `mise run check` (fmt --check + clippy + test) before considering any work complete

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
it and calls `scheduler.boost()`. Initial poll: orchestrator calls
`scheduler.boost()` once on startup so the first tick populates UI
with current GH+AWS state.

## Testing

| Layer | Approach |
|---|---|
| Model/mapping | Unit tests with fixture JSON |
| CLI parsing | Unit tests via clap test utilities |
| Poller | Integration tests against mocked APIs |
| UI | Snapshot tests via ratatui test helpers |
