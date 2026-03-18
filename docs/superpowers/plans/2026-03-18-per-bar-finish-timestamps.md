# Per-Bar Finish Timestamps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show HH:MM finish timestamps per bar in the TUI,
sourced from API responses.

**Architecture:** Add `last_finished: Option<DateTime<Utc>>` to
`Bar`, flow timestamps through intermediate structs (`JobInfo`,
`ActionState`) and `reconcile_bars`, render in `BarWidget` with
dimmed style right of the progress bar.

**Tech Stack:** Rust, chrono, aws-sdk-codepipeline,
octocrab/serde_json, ratatui

**Spec:**
`docs/superpowers/specs/2026-03-18-per-bar-finish-timestamps-design.md`

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `src/model.rs` | Modify | Add `last_finished` to `Bar`, format helper |
| `src/poller/mod.rs` | Modify | Add timestamp to `JobInfo`, `ActionState`, `reconcile_bars` |
| `src/poller/github.rs` | Modify | Extract `completed_at` from job JSON |
| `src/poller/aws.rs` | Modify | Extract `last_status_change` from action |
| `src/ui/bar.rs` | Modify | Render timestamp in `BarWidget` |

---

### Task 1: Add `last_finished` field to `Bar` (model.rs)

**Files:**

- Modify: `src/model.rs:22-41` (Bar struct + Bar::new)

- [ ] **Step 1: Write failing test for Bar with last_finished**

In `src/model.rs` tests, add:

```rust
use chrono::{DateTime, Utc};

#[test]
fn new_bar_has_no_last_finished() {
    let bar = make_bar();
    assert!(bar.last_finished.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib model::tests::new_bar_has_no_last_finished`
Expected: FAIL — `last_finished` field doesn't exist

- [ ] **Step 3: Add `last_finished` field to `Bar`**

In `src/model.rs`, add to `Bar` struct:

```rust
pub last_finished: Option<chrono::DateTime<chrono::Utc>>,
```

Add `use chrono::{DateTime, Utc};` at the top of the file.

In `Bar::new()`, add `last_finished: None` to the Self
initializer.

- [ ] **Step 4: Fix all compilation errors**

Every place that constructs a `Bar` directly (not via
`Bar::new()`) must add `last_finished: None`. Known sites:

- `src/model.rs` tests: `make_bar` function at line 129
- `src/ui/bar.rs` tests: `make_bar` function at line 187

Run: `cargo build 2>&1 | head -40` to find any others, fix them.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib model::tests::new_bar_has_no_last_finished`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/model.rs src/ui/bar.rs
git commit -m "add last_finished field to Bar"
```

---

### Task 2: Add timestamp format helper (model.rs)

**Files:**

- Modify: `src/model.rs`

- [ ] **Step 1: Write failing test for format helper**

In `src/model.rs` tests:

```rust
use chrono::{FixedOffset, TimeZone};

#[test]
fn format_finished_time_renders_hh_mm() {
    let utc_time = Utc.with_ymd_and_hms(2026, 3, 18, 13, 28, 0).unwrap();
    let offset = FixedOffset::east_opt(3600).unwrap(); // UTC+1
    let result = format_finished_time(&utc_time, &offset);
    assert_eq!(result, "14:28");
}

#[test]
fn format_finished_time_zero_padded() {
    let utc_time = Utc.with_ymd_and_hms(2026, 1, 1, 1, 5, 0).unwrap();
    let offset = FixedOffset::east_opt(0).unwrap(); // UTC
    let result = format_finished_time(&utc_time, &offset);
    assert_eq!(result, "01:05");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib model::tests::format_finished`
Expected: FAIL — function doesn't exist

- [ ] **Step 3: Implement format helper**

In `src/model.rs`, add a public function:

```rust
use chrono::{DateTime, TimeZone, Utc};

pub fn format_finished_time<Tz: TimeZone>(
    time: &DateTime<Utc>,
    tz: &Tz,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    time.with_timezone(tz).format("%H:%M").to_string()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib model::tests::format_finished`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/model.rs
git commit -m "add timezone-safe format_finished_time helper"
```

---

### Task 3: Add timestamps to intermediate structs (poller/mod.rs)

**Files:**

- Modify: `src/poller/mod.rs:31-50` (ActionState, JobInfo)

- [ ] **Step 1: Write failing tests for new struct fields**

In `src/poller/mod.rs` tests, add:

```rust
use chrono::{TimeZone, Utc};

#[test]
fn action_state_carries_timestamp() {
    let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 25, 0).unwrap();
    let action = ActionState {
        status: BuildStatus::Succeeded,
        last_status_change: Some(ts),
    };
    assert_eq!(action.last_status_change, Some(ts));
}

#[test]
fn job_info_carries_completed_at() {
    let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap();
    let job = JobInfo {
        name: "build".to_string(),
        status: BuildStatus::Succeeded,
        completed_at: Some(ts),
    };
    assert_eq!(job.completed_at, Some(ts));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib poller::tests::action_state_carries`
Expected: FAIL — field doesn't exist

- [ ] **Step 3: Add `last_status_change` to `ActionState`**

In `src/poller/mod.rs`:

```rust
pub struct ActionState {
    pub status: BuildStatus,
    pub last_status_change: Option<chrono::DateTime<chrono::Utc>>,
}
```

- [ ] **Step 4: Add `completed_at` to `JobInfo`**

```rust
pub struct JobInfo {
    pub name: String,
    pub status: BuildStatus,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}
```

- [ ] **Step 5: Fix all compilation errors from new fields**

Every `ActionState { status: ... }` and
`JobInfo { name: ..., status: ... }` construction must add the
new field. Known sites:

- `src/poller/aws.rs:98` — `ActionState { status }`
  → add `last_status_change: None` (will be filled in Task 5)
- `src/poller/github.rs:109-112` — `JobInfo { name, status }`
  → add `completed_at: None` (will be filled in Task 6)
- `src/poller/mod.rs` tests: `mock_stage` at line 454,
  `MockActionsClient::fetch_run_jobs` at line 432,
  `WorkflowRunInfo::jobs` constructions at lines 482-485,
  617-623, 843-850, etc.

Run: `cargo build 2>&1 | head -60` and fix all errors.

- [ ] **Step 6: Run full test suite**

Run: `cargo test`
Expected: all PASS

- [ ] **Step 7: Commit**

```bash
git add src/poller/
git commit -m "add timestamp fields to ActionState and JobInfo"
```

---

### Task 4: Flow timestamps through reconcile_bars (poller/mod.rs)

**Files:**

- Modify: `src/poller/mod.rs:236-260` (reconcile_bars)
- Modify: `src/poller/mod.rs:280-284` (update_pipeline_groups)
- Modify: `src/poller/mod.rs:348-359` (update_workflow_jobs)

- [ ] **Step 1: Write failing test for reconcile_bars with timestamps**

In `src/poller/mod.rs` tests:

```rust
use chrono::{TimeZone, Utc};

#[test]
fn reconcile_bars_sets_last_finished_from_tuple() {
    let mut bars: Vec<Bar> = vec![];
    let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap();
    let updates = vec![
        ("Source".to_string(), BuildStatus::Succeeded, Some(ts)),
        ("Build".to_string(), BuildStatus::Running, None),
    ];
    reconcile_bars(&mut bars, updates);

    let source = bars.iter().find(|b| b.name == "Source").unwrap();
    assert_eq!(source.last_finished, Some(ts));
    let build = bars.iter().find(|b| b.name == "Build").unwrap();
    assert!(build.last_finished.is_none());
}

#[test]
fn reconcile_bars_clears_last_finished_on_new_run() {
    let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap();
    let mut bars: Vec<Bar> = vec![];
    reconcile_bars(
        &mut bars,
        vec![("Build".to_string(), BuildStatus::Succeeded, Some(ts))],
    );
    assert_eq!(bars[0].last_finished, Some(ts));

    // New execution starts — timestamp should clear
    reconcile_bars(
        &mut bars,
        vec![("Build".to_string(), BuildStatus::Running, None)],
    );
    assert!(bars[0].last_finished.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib poller::tests::reconcile_bars_sets_last_finished`
Expected: FAIL — signature mismatch

- [ ] **Step 3: Update `reconcile_bars` signature and body**

Change `src/poller/mod.rs` `reconcile_bars`:

```rust
fn reconcile_bars(
    bars: &mut Vec<Bar>,
    updates: Vec<(String, BuildStatus, Option<chrono::DateTime<chrono::Utc>>)>,
) {
    let seen: HashSet<&str> = updates
        .iter()
        .map(|(n, _, _)| n.as_str())
        .collect();
    for bar in bars.iter_mut() {
        if !seen.contains(bar.name.as_str()) {
            bar.gone = true;
        }
    }
    for (name, status, finished) in &updates {
        if let Some(bar) = bars.iter_mut().find(|b| b.name == *name) {
            bar.gone = false;
            bar.set_status(*status);
            bar.last_finished = *finished;
        } else {
            let mut bar = Bar::new(name.clone());
            bar.status = *status;
            bar.last_finished = *finished;
            bars.push(bar);
        }
    }
    let order: std::collections::HashMap<&str, usize> = updates
        .iter()
        .enumerate()
        .map(|(i, (n, _, _))| (n.as_str(), i))
        .collect();
    bars.sort_by_key(|b| {
        order.get(b.name.as_str()).copied().unwrap_or(usize::MAX)
    });
}
```

- [ ] **Step 4: Update `stage_status_from_actions` to also return timestamp**

Create a new function that returns both status and timestamp:

```rust
fn stage_status_and_timestamp(
    actions: &[ActionState],
) -> (BuildStatus, Option<chrono::DateTime<chrono::Utc>>) {
    let last = actions.last();
    let status = last.map(|a| a.status).unwrap_or(BuildStatus::Idle);
    let timestamp = match status {
        BuildStatus::Succeeded | BuildStatus::Failed => {
            last.and_then(|a| a.last_status_change)
        }
        _ => None,
    };
    (status, timestamp)
}
```

Keep the original `stage_status_from_actions` working (it's used
in the existing test).

- [ ] **Step 5: Update `update_pipeline_groups` call site**

In `update_pipeline_groups`, change `stage_updates` construction:

```rust
let stage_updates: Vec<_> = state
    .stages
    .iter()
    .map(|s| {
        let (status, ts) = stage_status_and_timestamp(&s.actions);
        (s.name.clone(), status, ts)
    })
    .collect();
```

- [ ] **Step 6: Update `update_workflow_jobs` call site**

```rust
let updates: Vec<_> = jobs
    .into_iter()
    .map(|j| (j.name, j.status, j.completed_at))
    .collect();
```

- [ ] **Step 7: Fix all other `reconcile_bars` call sites in tests**

All existing test code that constructs update tuples as
`(String, BuildStatus)` must add a third element `None`.
Search for `reconcile_bars` calls in test code and update.

Run: `cargo build 2>&1 | head -60`

- [ ] **Step 8: Run tests to verify all pass**

Run: `cargo test`
Expected: all PASS

- [ ] **Step 9: Commit**

```bash
git add src/poller/mod.rs
git commit -m "flow timestamps through reconcile_bars and call sites"
```

---

### Task 5: Extract timestamps from AWS API (poller/aws.rs)

**Files:**

- Modify: `src/poller/aws.rs:89-99`

- [ ] **Step 1: Write failing test**

In `src/poller/mod.rs` tests, add a test that uses
`ActionState` with a timestamp:

```rust
#[test]
fn stage_status_and_timestamp_for_succeeded() {
    let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 25, 0).unwrap();
    let actions = vec![ActionState {
        status: BuildStatus::Succeeded,
        last_status_change: Some(ts),
    }];
    let (status, timestamp) = stage_status_and_timestamp(&actions);
    assert_eq!(status, BuildStatus::Succeeded);
    assert_eq!(timestamp, Some(ts));
}

#[test]
fn stage_status_and_timestamp_for_running() {
    let actions = vec![ActionState {
        status: BuildStatus::Running,
        last_status_change: None,
    }];
    let (status, timestamp) = stage_status_and_timestamp(&actions);
    assert_eq!(status, BuildStatus::Running);
    assert!(timestamp.is_none());
}
```

- [ ] **Step 2: Run tests to verify they pass**

These should pass since we added `stage_status_and_timestamp`
in Task 4. If not, fix.

Run: `cargo test --lib poller::tests::stage_status_and_timestamp`
Expected: PASS

- [ ] **Step 3: Extract `last_status_change` in AWS poller**

In `src/poller/aws.rs`, in the `get_pipeline_state` method,
update the action mapping (around line 92-99):

```rust
let actions = s
    .action_states()
    .iter()
    .map(|a| {
        let status = a
            .latest_execution()
            .and_then(|e| e.status())
            .map(|s| map_action_status(s.as_str()))
            .unwrap_or(BuildStatus::Idle);
        let last_status_change = a
            .latest_execution()
            .and_then(|e| e.last_status_change())
            .and_then(|dt| {
                let millis = dt.to_millis().ok()?;
                chrono::DateTime::from_timestamp_millis(millis)
            });
        // Only keep timestamp for terminal states
        let last_status_change = match status {
            BuildStatus::Succeeded | BuildStatus::Failed => {
                last_status_change
            }
            _ => None,
        };
        ActionState {
            status,
            last_status_change,
        }
    })
    .collect();
```

- [ ] **Step 4: Run full test suite**

Run: `cargo test`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add src/poller/aws.rs
git commit -m "extract last_status_change from AWS action executions"
```

---

### Task 6: Extract timestamps from GitHub API (poller/github.rs)

**Files:**

- Modify: `src/poller/github.rs:104-113`

- [ ] **Step 1: Write failing test**

Extract a `parse_job_completed_at` helper and test it. In
`src/poller/github.rs` tests:

```rust
use chrono::{TimeZone, Utc};

#[test]
fn parse_job_completed_at_extracts_timestamp() {
    let job = serde_json::json!({
        "completed_at": "2026-03-18T14:28:00Z"
    });
    let result = parse_job_completed_at(&job);
    assert_eq!(
        result,
        Some(Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap())
    );
}

#[test]
fn parse_job_completed_at_null_is_none() {
    let job = serde_json::json!({ "completed_at": null });
    assert!(parse_job_completed_at(&job).is_none());
}

#[test]
fn parse_job_completed_at_missing_is_none() {
    let job = serde_json::json!({});
    assert!(parse_job_completed_at(&job).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib poller::github::tests::parse_job_completed`
Expected: FAIL — `parse_job_completed_at` doesn't exist

- [ ] **Step 3: Implement helper and update `fetch_run_jobs`**

Add the helper function in `src/poller/github.rs`:

```rust
fn parse_job_completed_at(
    job: &serde_json::Value,
) -> Option<chrono::DateTime<chrono::Utc>> {
    job["completed_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
}
```

Then update `fetch_run_jobs` to use it:

In `src/poller/github.rs`, `fetch_run_jobs` method, update the
job loop (around line 104-113) to use the helper:

```rust
for job in job_array {
    let name = job["name"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let status = job["status"]
        .as_str()
        .unwrap_or("unknown");
    let conclusion = job["conclusion"].as_str();
    let completed_at = parse_job_completed_at(job);
    jobs.push(JobInfo {
        name,
        status: map_run_status(status, conclusion),
        completed_at,
    });
}
```

- [ ] **Step 4: Run full test suite**

Run: `cargo test`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add src/poller/github.rs
git commit -m "extract completed_at from GitHub job responses"
```

---

### Task 7: Render timestamps in BarWidget (ui/bar.rs)

**Files:**

- Modify: `src/ui/bar.rs:12-78`

- [ ] **Step 1: Write failing test — bar with timestamp**

In `src/ui/bar.rs` tests, update `make_bar` first to include
`last_finished`:

```rust
use chrono::{TimeZone, Utc};

#[test]
fn bar_renders_timestamp_when_finished() {
    let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap();
    let mut bar = make_bar("build", BuildStatus::Succeeded, 5);
    bar.last_finished = Some(ts);
    let widget = BarWidget::new(&bar, 10, false);
    let area = Rect::new(0, 0, 30, 1);
    let mut buf = Buffer::empty(area);
    widget.render(area, &mut buf);

    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();
    // Should contain HH:MM somewhere after the bar
    // Exact time depends on local timezone — just verify
    // the pattern exists (digits:digits after ']')
    let after_bracket = content.split(']').last().unwrap_or("");
    assert!(
        after_bracket.contains(':'),
        "expected HH:MM after bar, got: {content}"
    );
}

#[test]
fn bar_no_timestamp_when_none() {
    let bar = make_bar("build", BuildStatus::Running, 3);
    // last_finished is None (default)
    let widget = BarWidget::new(&bar, 10, false);
    let area = Rect::new(0, 0, 30, 1);
    let mut buf = Buffer::empty(area);
    widget.render(area, &mut buf);

    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();
    let after_bracket = content.split(']').last().unwrap_or("");
    assert!(
        !after_bracket.contains(':'),
        "no timestamp expected, got: {content}"
    );
}
```

- [ ] **Step 2: Run tests to verify first test fails**

Run: `cargo test --lib ui::bar::tests::bar_renders_timestamp`
Expected: FAIL — no timestamp rendered yet

- [ ] **Step 3: Implement timestamp rendering in BarWidget**

In `src/ui/bar.rs`, modify the `render` method of
`impl Widget for BarWidget<'_>`:

```rust
impl Widget for BarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 1 {
            return;
        }

        // Format timestamp if present
        let ts_str = self.bar.last_finished.map(|t| {
            use crate::model::format_finished_time;
            format_finished_time(&t, &chrono::Local)
        });
        // Reserve space: " HH:MM" = 6 chars
        let ts_reserve = if ts_str.is_some() { 6 } else { 0 };

        let dot_prefix_len = if self.status_dot.is_some() {
            2
        } else {
            0
        };
        let name_col = self.name_width + 2;
        let overhead = dot_prefix_len + name_col + 2 + ts_reserve;
        if (area.width as usize) <= overhead {
            return;
        }
        let fill_width = area.width as usize - overhead;
        let color = self.status_color();

        let name_display = if self.bar.gone {
            format!(
                "{:<w$}*",
                self.bar.name,
                w = self.name_width - 1
            )
        } else {
            format!("{:<w$}", self.bar.name, w = self.name_width)
        };

        let filled = self.bar.fill.min(fill_width);
        let empty = fill_width - filled;

        let mut spans = Vec::new();
        if let Some(dot_color) = self.status_dot {
            spans.push(Span::styled(
                "\u{25CF} ",
                Style::default().fg(dot_color),
            ));
        }
        spans.push(Span::raw(format!("{name_display}  ")));
        spans.push(Span::raw("["));
        spans.push(Span::styled(
            "|".repeat(filled),
            Style::default().fg(color),
        ));
        spans.push(Span::raw(" ".repeat(empty)));
        spans.push(Span::raw("]"));

        if let Some(ts) = ts_str {
            spans.push(Span::styled(
                format!(" {ts}"),
                Style::default().fg(Color::DarkGray),
            ));
        }

        Line::from(spans).render(area, buf);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib ui::bar::tests::bar_renders_timestamp && cargo test --lib ui::bar::tests::bar_no_timestamp`
Expected: PASS

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add src/ui/bar.rs
git commit -m "render HH:MM timestamp after progress bar"
```

---

### Task 8: End-to-end integration tests (poller/mod.rs)

**Files:**

- Modify: `src/poller/mod.rs` (test module)

Note: mock helpers (`MockActionsClient`, `mock_stage`) were
already updated to include timestamp fields in Tasks 3-4 when
fixing compilation errors. This task adds integration tests
that verify timestamps flow from mock API data through to
`Bar.last_finished`.

- [ ] **Step 1: Write integration tests for timestamp flow**

```rust
#[tokio::test]
async fn poll_preserves_job_timestamp() {
    let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap();
    let app = Arc::new(Mutex::new(App::new()));
    let pipes = MockPipelineClient { pipelines: vec![] };
    let actions = MockActionsClient {
        runs: vec![WorkflowRunInfo {
            workflow_name: "CI".to_string(),
            run_id: 100,
            status: BuildStatus::Succeeded,
            jobs: vec![JobInfo {
                name: "build".to_string(),
                status: BuildStatus::Succeeded,
                completed_at: Some(ts),
            }],
        }],
    };
    poll_once(&app, &pipes, &actions).await;

    let a = app.lock().unwrap();
    let job = &a.workflow_groups[0].jobs[0];
    assert_eq!(job.last_finished, Some(ts));
}

#[tokio::test]
async fn poll_clears_timestamp_on_new_run() {
    let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap();
    let app = Arc::new(Mutex::new(App::new()));
    let pipes = MockPipelineClient { pipelines: vec![] };

    let actions = MockActionsClient {
        runs: vec![WorkflowRunInfo {
            workflow_name: "CI".to_string(),
            run_id: 100,
            status: BuildStatus::Succeeded,
            jobs: vec![JobInfo {
                name: "build".to_string(),
                status: BuildStatus::Succeeded,
                completed_at: Some(ts),
            }],
        }],
    };
    poll_once(&app, &pipes, &actions).await;

    // New run starts
    let actions = MockActionsClient {
        runs: vec![WorkflowRunInfo {
            workflow_name: "CI".to_string(),
            run_id: 101,
            status: BuildStatus::Running,
            jobs: vec![JobInfo {
                name: "build".to_string(),
                status: BuildStatus::Running,
                completed_at: None,
            }],
        }],
    };
    poll_once(&app, &pipes, &actions).await;

    let a = app.lock().unwrap();
    let job = &a.workflow_groups[0].jobs[0];
    assert!(job.last_finished.is_none());
}
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: all PASS

- [ ] **Step 3: Commit**

```bash
git add src/poller/mod.rs
git commit -m "add timestamp integration tests for end-to-end flow"
```

---

### Task 9: Final validation

- [ ] **Step 1: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: no warnings

- [ ] **Step 2: Run fmt**

Run: `cargo fmt -- --check`
Expected: clean

- [ ] **Step 3: Run full test suite one final time**

Run: `cargo test`
Expected: all PASS

- [ ] **Step 4: Fix any issues found, commit**

If clippy or fmt report issues, fix and commit:

```bash
git add -A
git commit -m "fix clippy/fmt issues"
```
