# cibars: Functional Specification

## 1. Overview

`cibars` is a standalone terminal application written in Rust. It
monitors AWS CodePipelines and GitHub Actions for a given project
and renders a live, auto-refreshing TUI inside a terminal emulator
or tmux pane.

---

## 2. Startup Parameters

The application accepts three required arguments at startup:

| Parameter | Flag | Format | Example |
|---|---|---|---|
| AWS Profile | `--aws-profile` | Named profile string | `staging` |
| AWS Region | `--region` | AWS region identifier | `eu-west-1` |
| GitHub Repository | `--github-repo` | `owner/repo` | `acme/backend` |

On startup, the application validates all three parameters before
rendering the UI. If any is missing or invalid, the application
exits with a clear error message and a non-zero exit code.

The GitHub personal access token is read from the `GITHUB_TOKEN`
environment variable. If absent, the application exits with an
error on startup.

---

## 3. Data Sources

### 3.1 AWS CodePipelines

- **Scope**: all CodePipelines visible to the given AWS profile and region
- **API calls used**:
  - `codepipeline:ListPipelines` to enumerate all pipelines
  - `codepipeline:GetPipelineState` to retrieve the current execution
    state per pipeline
- **Polling**: controlled by the polling state machine (see section 8);
  AWS is only polled in Active and Cooldown states
- **Auto-discovery**: new pipelines that appear between polls are
  automatically included in subsequent renders

### 3.2 GitHub Actions

- **Scope**: all workflow runs for the given repository
- **API calls used (phase-2 polling)**:
  - `GET /repos/{owner}/{repo}/actions/runs` to fetch workflow
    summaries (1 API call)
  - `GET /repos/{owner}/{repo}/actions/runs/{id}/jobs` per running
    workflow, fetched in parallel
- **Polling**: controlled by the polling state machine (see section 8);
  GitHub is polled in every state
- **Auto-discovery**: new workflow runs that start between polls appear
  automatically in the next render cycle

---

## 4. UI Requirements

### 4.1 Layout

The UI is a full-screen TUI divided into two sections:

1. **CodePipelines**: one bar per discovered AWS CodePipeline
2. **GitHub Actions**: workflow groups with expandable jobs

A header bar at the top shows the active region, repository, and
current time. A status bar at the bottom shows the last successful
poll timestamp, available key bindings, and any active warnings.

Each bar occupies exactly one row:

```text
  <name padded to N chars>  [<fill>]
```

The usable fill area is `terminal_width - name_column - 4`
characters (accounting for `[`, `]`, and surrounding spacing).

### 4.2 Bar Behaviour

A bar represents the current or most recent build for a given
pipeline or workflow.

**While running:** each poll cycle appends one `|` to the fill
area. The entire filled portion is rendered in yellow.

**On completion:** the entire filled portion (however many `|`
characters have been written) switches color instantly: green for
success, red for failure. The bar stays in this state until the
next build starts.

**Wrap:** if the fill area is exhausted before the build completes,
the write position resets to 0 and filling continues from the left,
overwriting the previous content. Color remains yellow throughout.
The wrap does not affect the final outcome color.

**Idle:** when no build has run since startup, the bar is empty
and rendered in grey.

### 4.3 Bar Color Summary

| State | Color |
|---|---|
| Running | Yellow |
| Succeeded | Green |
| Failed | Red |
| Idle (no data) | Grey |

### 4.4 GitHub Actions: Workflow Groups

GitHub Actions are displayed as workflow groups with jobs underneath.
Each workflow group has a summary status line, and when expanded
(`e` key), individual jobs are shown as indented bars beneath the
workflow name.

### 4.5 Bar Discovery and Ordering

- Bars are added automatically when a new pipeline or workflow is
  discovered during a poll
- Bars are never removed during a session; if a pipeline or workflow
  disappears it is marked `[gone]`
- Within each section, bars are ordered: running bars first (sorted
  by name), then idle/completed (sorted by name)

### 4.6 Refresh Behaviour

- The UI re-renders after each completed poll cycle
- A manual boost can be triggered with the `b` key (transitions to
  Watching state for fast GitHub-only polling)
- The last poll timestamp is shown in the status bar
- If a poll fails, a warning is shown in the status bar; the current
  bar state is left unchanged

### 4.7 Terminal Compatibility

- Minimum terminal size: 80 columns × 10 rows
- If the terminal is too small, display: `Terminal too small`
- The UI must render correctly inside a tmux pane

---

## 5. Key Bindings

| Key | Action |
|---|---|
| `q` | Quit the application |
| `Ctrl-C` | Quit the application |
| `e` | Toggle expand/collapse GitHub Actions jobs |
| `b` | Boost: trigger immediate fast polling |
| `h` | Install git pre-push hook for auto-boost |

---

## 6. Error Handling

| Condition | Behaviour |
|---|---|
| Missing/invalid startup arg | Exit before rendering, print error to stderr |
| Missing `GITHUB_TOKEN` | Exit before rendering, print error to stderr |
| AWS credentials expired | Display warning in status bar, continue polling |
| GitHub API rate limit hit | Display warning, back off until rate limit resets |
| Network timeout | Display warning, retry on next scheduled poll |
| No pipelines found | Show: `No pipelines found in this account/region` |
| No Actions runs found | Show: `No recent workflow runs found` |

---

## 7. Non-Functional Requirements

| Property | Requirement |
|---|---|
| Language | Rust (stable toolchain) |
| Binary | Single statically-linked binary, no runtime dependencies |
| Memory footprint | Under 50 MB RSS during normal operation |
| CPU usage | Negligible between poll cycles (no busy-wait loops) |
| Startup time | First render within 3 seconds on a normal network |
| Graceful shutdown | On `SIGINT`/`SIGTERM`, restore terminal state before exit |

---

## 8. Polling State Machine

cibars uses an intelligent polling strategy to minimize API calls
while staying responsive. AWS CodePipeline depends on GitHub
Actions, so AWS is only polled when GitHub detects running builds.

**Startup:** The first poll cycle always polls both GitHub and AWS
to give immediate visibility into current status.

```text
              boost (b key)
    ┌──────────────────────────┐
    │                          ▼
  Idle ──────────────────► Watching
  30s GH, no AWS           5s GH, no AWS
    ▲         │                │
    │         │ 5min idle      │ GH finds running
    │         ▼                ▼
  LongIdle  Cooldown ◄──── Active
  5min GH   5s GH+AWS      5s GH+AWS
  no AWS    60s timer           │
    │         │                 │ nothing running
    │         └─────────────────┘
    │  boost (b key)
    └──────────────────► Watching
```

| State | GH interval | Poll AWS? | Entry |
|---|---|---|---|
| Idle | 30s | No | Startup (after initial poll), or cooldown expired |
| LongIdle | 5min | No | 5min of Idle with no running builds |
| Watching | 5s | No | User pressed `b` from Idle or LongIdle |
| Active | 5s | Yes | GitHub detects running builds |
| Cooldown | 5s | Yes | Nothing running (from Active), 60s timer |

**Key transitions:**

- Press `b` or send SIGUSR1 in Idle/LongIdle enters Watching
  (fast GH-only polling)
- 5min of Idle with no running builds enters LongIdle (5min polling)
- GitHub finds running builds enters Active (adds AWS polling)
- All builds finish enters Cooldown (keeps fast polling for 60s)
- 60s of inactivity returns to Idle
- Pressing `b` in Active/Cooldown is a no-op (already fast)

---

## 9. Out of Scope (v1)

- Webhook-based push updates
- Filtering or searching pipelines by name
- Drill-down into individual pipeline stage details
- Notifications (desktop, Slack, etc.)
- Support for multiple repositories or AWS accounts simultaneously
