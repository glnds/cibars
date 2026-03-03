# cibars: Functional Specification

## 1. Overview

`cibars` is a standalone terminal application written in Rust. It monitors AWS CodePipelines and GitHub Actions for a given project and renders a live, auto-refreshing TUI inside a terminal emulator or tmux pane.

---

## 2. Startup Parameters

The application accepts three required arguments at startup:

| Parameter | Flag | Format | Example |
|---|---|---|---|
| AWS Profile | `--aws-profile` | Named profile string | `staging` |
| AWS Region | `--region` | AWS region identifier | `eu-west-1` |
| GitHub Repository | `--github-repo` | `owner/repo` | `acme/backend` |

On startup, the application validates all three parameters before rendering the UI. If any is missing or invalid, the application exits with a clear error message and a non-zero exit code.

The GitHub personal access token is read from the `GITHUB_TOKEN` environment variable. If absent, the application exits with an error on startup.

---

## 3. Data Sources

### 3.1 AWS CodePipelines

- **Scope**: all CodePipelines visible to the given AWS profile and region
- **API calls used**:
  - `codepipeline:ListPipelines` to enumerate all pipelines
  - `codepipeline:GetPipelineState` to retrieve the current execution state per pipeline
- **Polling interval**: every 30 seconds
- **Auto-discovery**: new pipelines that appear between polls are automatically included in subsequent renders

### 3.2 GitHub Actions

- **Scope**: all workflow runs for the given repository
- **API calls used**:
  - `GET /repos/{owner}/{repo}/actions/runs` filtered to `status=in_progress` and recent completed runs
- **Polling interval**: every 30 seconds
- **Auto-discovery**: new workflow runs that start between polls appear automatically in the next render cycle

---

## 4. UI Requirements

### 4.1 Layout

The UI is a full-screen TUI divided into two sections:

1. **CodePipelines**: one bar per discovered AWS CodePipeline
2. **GitHub Actions**: one bar per discovered workflow in the repository

A header bar at the top shows the active region, repository, and current time. A status bar at the bottom shows the last successful poll timestamp, available key bindings, and any active warnings.

Each bar occupies exactly one row:

```
  <name padded to N chars>  [<fill>]
```

The usable fill area is `terminal_width - name_column - 4` characters (accounting for `[`, `]`, and surrounding spacing).

### 4.2 Bar Behaviour

A bar represents the current or most recent build for a given pipeline or workflow.

**While running:** each poll cycle appends one `|` to the fill area. The entire filled portion is rendered in yellow.

**On completion:** the entire filled portion (however many `|` characters have been written) switches color instantly: green for success, red for failure. The bar stays in this state until the next build starts.

**Wrap:** if the fill area is exhausted before the build completes, the write position resets to 0 and filling continues from the left, overwriting the previous content. Color remains yellow throughout. The wrap does not affect the final outcome color.

**Idle:** when no build has run since startup, the bar is empty and rendered in grey.

### 4.3 Bar Color Summary

| State | Color |
|---|---|
| Running | Yellow |
| Succeeded | Green |
| Failed | Red |
| Idle (no data) | Grey |

### 4.4 Bar Discovery and Ordering

- Bars are added automatically when a new pipeline or workflow is discovered during a poll
- Bars are never removed during a session; if a pipeline disappears it is marked `[gone]`
- Within each section, bars are ordered: running bars first (sorted by name), then idle/completed (sorted by name)

### 4.5 Refresh Behaviour

- The UI re-renders after each completed poll cycle
- A manual refresh can be triggered with the `r` key
- The last poll timestamp is shown in the status bar
- If a poll fails, a warning is shown in the status bar; the current bar state is left unchanged

### 4.6 Terminal Compatibility

- Minimum terminal size: 80 columns × 10 rows
- If the terminal is too small, display: `Terminal too small`
- The UI must render correctly inside a tmux pane

---

## 5. Key Bindings

| Key | Action |
|---|---|
| `r` | Trigger immediate refresh |
| `q` | Quit the application |

---

## 6. Error Handling

| Condition | Behaviour |
|---|---|
| Missing or invalid startup argument | Exit before rendering, print error to stderr |
| Missing `GITHUB_TOKEN` | Exit before rendering, print error to stderr |
| AWS credentials expired mid-session | Display warning in status bar, continue polling |
| GitHub API rate limit hit | Display warning in status bar, back off for the remainder of the rate limit window |
| Network timeout | Display warning in status bar, retry on next scheduled poll |
| No pipelines found | Render section with message: `No pipelines found in this account/region` |
| No Actions runs found | Render section with message: `No recent workflow runs found` |

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

## 8. Out of Scope (v1)

- Webhook-based push updates
- Filtering or searching pipelines by name
- Drill-down into individual pipeline stage details
- Notifications (desktop, Slack, etc.)
- Configuration file (everything via CLI args and env vars)
- Support for multiple repositories or AWS accounts simultaneously
