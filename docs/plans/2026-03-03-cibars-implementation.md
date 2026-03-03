<!-- markdownlint-disable MD036 -->
# cibars Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use
> superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a TUI that monitors AWS CodePipelines and GitHub
Actions, rendering live progress bars with auto-refresh.

**Architecture:** Async polling (tokio) on spawned task writes to
`Arc<Mutex<App>>` and signals via `watch` channel. Synchronous
TUI event loop (ratatui) on main thread reads state each tick.
Drop guard restores terminal on all exit paths.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, tokio 1,
clap 4 (derive), aws-sdk-codepipeline 1, octocrab, anyhow,
tracing

---

## Phase 1: Scaffold and CLI

### Task 1: Project scaffolding

**Files:**

- Create: `Cargo.toml`
- Create: `src/main.rs`

**Step 1: Create Cargo.toml**

```toml
[package]
name = "cibars"
version = "0.1.0"
edition = "2021"

[dependencies]
anyhow = "1"
async-trait = "0.1"
aws-config = { version = "1", features = [
    "behavior-version-latest"
] }
aws-sdk-codepipeline = "1"
chrono = "0.4"
clap = { version = "4", features = ["derive"] }
crossterm = "0.28"
octocrab = "0.44"
ratatui = "0.29"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = [
    "env-filter"
] }
```

**Step 2: Create minimal main.rs**

```rust
fn main() {
    println!("cibars");
}
```

**Step 3: Verify build**

Run: `cargo build`
Expected: compiles without errors

**Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs
git commit -m "scaffold: init project with dependencies"
```

---

### Task 2: Config module (TDD)

**Files:**

- Create: `src/config.rs`
- Modify: `src/main.rs`

**Step 1: Write failing tests in config.rs**

```rust
// src/config.rs
use anyhow::{Context, Result, ensure};
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "cibars",
    about = "CI build status bars"
)]
pub struct Config {
    /// AWS profile name
    #[arg(long)]
    pub aws_profile: String,

    /// AWS region
    #[arg(long)]
    pub region: String,

    /// GitHub repository (owner/repo)
    #[arg(long)]
    pub github_repo: String,
}

impl Config {
    pub fn load() -> Result<(Self, String)> {
        let config = Self::parse();
        let token = std::env::var("GITHUB_TOKEN")
            .context(
                "GITHUB_TOKEN environment variable is \
                 required"
            )?;
        ensure!(
            config.github_repo.contains('/'),
            "github-repo must be in owner/repo format"
        );
        Ok((config, token))
    }

    /// For testing: parse from args without env var
    pub fn try_from_args(
        args: &[&str],
    ) -> Result<Self> {
        let config = Self::try_parse_from(args)?;
        ensure!(
            config.github_repo.contains('/'),
            "github-repo must be in owner/repo format"
        );
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_args_parse() {
        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile", "staging",
            "--region", "eu-west-1",
            "--github-repo", "acme/backend",
        ])
        .unwrap();
        assert_eq!(config.aws_profile, "staging");
        assert_eq!(config.region, "eu-west-1");
        assert_eq!(config.github_repo, "acme/backend");
    }

    #[test]
    fn missing_profile_fails() {
        let result = Config::try_from_args(&[
            "cibars",
            "--region", "eu-west-1",
            "--github-repo", "acme/backend",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn missing_region_fails() {
        let result = Config::try_from_args(&[
            "cibars",
            "--aws-profile", "staging",
            "--github-repo", "acme/backend",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn missing_repo_fails() {
        let result = Config::try_from_args(&[
            "cibars",
            "--aws-profile", "staging",
            "--region", "eu-west-1",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_repo_format_fails() {
        let result = Config::try_from_args(&[
            "cibars",
            "--aws-profile", "staging",
            "--region", "eu-west-1",
            "--github-repo", "no-slash-here",
        ]);
        assert!(result.is_err());
    }
}
```

**Step 2: Run tests to verify they pass**

Run: `cargo test --lib config`
Expected: all 5 tests pass (implementation included above)

**Step 3: Wire config into main.rs**

```rust
// src/main.rs
mod config;

use config::Config;

fn main() {
    match Config::load() {
        Ok((config, _token)) => {
            println!("Profile: {}", config.aws_profile);
            println!("Region:  {}", config.region);
            println!("Repo:    {}", config.github_repo);
        }
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    }
}
```

**Step 4: Verify with cargo clippy + fmt**

Run: `cargo clippy && cargo fmt --check`
Expected: no warnings, no formatting issues

**Step 5: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: add CLI config parsing with validation"
```

---

## Phase 2: Data Model and Polling

### Task 3: Model types and Bar update logic (TDD)

**Files:**

- Create: `src/model.rs`
- Modify: `src/main.rs` (add `mod model;`)

**Step 1: Write model types + tests**

```rust
// src/model.rs

#[derive(Debug, Clone, PartialEq)]
pub enum BuildStatus {
    Running,
    Succeeded,
    Failed,
    Idle,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BarSource {
    CodePipeline,
    GitHubAction,
}

#[derive(Debug, Clone)]
pub struct Bar {
    pub name: String,
    pub source: BarSource,
    pub status: BuildStatus,
    /// Number of '|' chars filled in current lap
    pub fill: usize,
    /// Current insertion index (resets on wrap)
    pub write_pos: usize,
    /// Pipeline/workflow disappeared from API
    pub gone: bool,
}

impl Bar {
    pub fn new(
        name: String,
        source: BarSource,
    ) -> Self {
        Self {
            name,
            source,
            status: BuildStatus::Idle,
            fill: 0,
            write_pos: 0,
            gone: false,
        }
    }

    /// Update bar state based on polled build status.
    /// `tick_area_width` is the available fill area.
    pub fn update(
        &mut self,
        status: BuildStatus,
        tick_area_width: usize,
    ) {
        match status {
            BuildStatus::Running => {
                self.status = BuildStatus::Running;
                self.write_pos += 1;
                if self.write_pos >= tick_area_width {
                    self.write_pos = 0;
                    self.fill = 0;
                }
                self.fill += 1;
            }
            BuildStatus::Succeeded
            | BuildStatus::Failed => {
                self.status = status;
                // freeze fill and write_pos
            }
            BuildStatus::Idle => {
                // leave unchanged
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bar(source: BarSource) -> Bar {
        Bar::new("test-pipe".to_string(), source)
    }

    #[test]
    fn new_bar_is_idle() {
        let bar = make_bar(BarSource::CodePipeline);
        assert_eq!(bar.status, BuildStatus::Idle);
        assert_eq!(bar.fill, 0);
        assert_eq!(bar.write_pos, 0);
        assert!(!bar.gone);
    }

    #[test]
    fn running_increments_fill_and_pos() {
        let mut bar = make_bar(BarSource::CodePipeline);
        bar.update(BuildStatus::Running, 10);
        assert_eq!(bar.status, BuildStatus::Running);
        assert_eq!(bar.fill, 1);
        assert_eq!(bar.write_pos, 1);

        bar.update(BuildStatus::Running, 10);
        assert_eq!(bar.fill, 2);
        assert_eq!(bar.write_pos, 2);
    }

    #[test]
    fn running_wraps_at_width() {
        let mut bar = make_bar(BarSource::CodePipeline);
        let width = 3;
        // Fill to capacity
        for _ in 0..3 {
            bar.update(BuildStatus::Running, width);
        }
        assert_eq!(bar.write_pos, 0);
        assert_eq!(bar.fill, 1);
        assert_eq!(bar.status, BuildStatus::Running);
    }

    #[test]
    fn wrap_resets_then_continues() {
        let mut bar = make_bar(BarSource::GitHubAction);
        let width = 2;
        // Two updates fill the bar
        bar.update(BuildStatus::Running, width);
        bar.update(BuildStatus::Running, width);
        // Third update wraps
        assert_eq!(bar.write_pos, 0);
        assert_eq!(bar.fill, 1);
        // Fourth update continues from wrap
        bar.update(BuildStatus::Running, width);
        assert_eq!(bar.write_pos, 1);
        assert_eq!(bar.fill, 2);
    }

    #[test]
    fn succeeded_freezes_bar() {
        let mut bar = make_bar(BarSource::CodePipeline);
        bar.update(BuildStatus::Running, 10);
        bar.update(BuildStatus::Running, 10);
        bar.update(BuildStatus::Succeeded, 10);
        assert_eq!(bar.status, BuildStatus::Succeeded);
        assert_eq!(bar.fill, 2);
        assert_eq!(bar.write_pos, 2);
    }

    #[test]
    fn failed_freezes_bar() {
        let mut bar = make_bar(BarSource::CodePipeline);
        bar.update(BuildStatus::Running, 10);
        bar.update(BuildStatus::Failed, 10);
        assert_eq!(bar.status, BuildStatus::Failed);
        assert_eq!(bar.fill, 1);
        assert_eq!(bar.write_pos, 1);
    }

    #[test]
    fn idle_leaves_bar_unchanged() {
        let mut bar = make_bar(BarSource::CodePipeline);
        bar.update(BuildStatus::Running, 10);
        let fill_before = bar.fill;
        let pos_before = bar.write_pos;
        bar.update(BuildStatus::Idle, 10);
        assert_eq!(bar.fill, fill_before);
        assert_eq!(bar.write_pos, pos_before);
        // status stays Running (Idle doesn't change it)
        assert_eq!(bar.status, BuildStatus::Running);
    }
}
```

**Step 2: Add `mod model;` to main.rs and run tests**

Run: `cargo test --lib model`
Expected: all 7 tests pass

**Step 3: Verify with clippy + fmt**

Run: `cargo clippy && cargo fmt --check`

**Step 4: Commit**

```bash
git add src/model.rs src/main.rs
git commit -m "feat: add model types with Bar update logic"
```

---

### Task 4: App state module

**Files:**

- Create: `src/app.rs`
- Modify: `src/main.rs` (add `mod app;`)

**Step 1: Write app.rs**

```rust
// src/app.rs
use chrono::{DateTime, Utc};

use crate::model::Bar;

pub struct App {
    pub bars_pipelines: Vec<Bar>,
    pub bars_actions: Vec<Bar>,
    pub last_poll: Option<DateTime<Utc>>,
    pub warnings: Vec<String>,
}

impl App {
    pub fn new() -> Self {
        Self {
            bars_pipelines: Vec::new(),
            bars_actions: Vec::new(),
            last_poll: None,
            warnings: Vec::new(),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
```

**Step 2: Add `mod app;` to main.rs, run build**

Run: `cargo build`
Expected: compiles

**Step 3: Commit**

```bash
git add src/app.rs src/main.rs
git commit -m "feat: add App state struct"
```

---

### Task 5: Poller trait abstractions (TDD)

**Files:**

- Create: `src/poller/mod.rs`
- Create: `src/poller/aws.rs`
- Create: `src/poller/github.rs`
- Modify: `src/main.rs` (add `mod poller;`)

**Step 1: Define traits and AWS response mapping types**

```rust
// src/poller/mod.rs
pub mod aws;
pub mod github;

use anyhow::Result;
use async_trait::async_trait;

use crate::model::BuildStatus;

/// Simplified pipeline state from AWS API
pub struct PipelineState {
    pub name: String,
    pub status: BuildStatus,
}

/// Simplified workflow run from GitHub API
pub struct WorkflowRunInfo {
    pub workflow_name: String,
    pub status: BuildStatus,
}

#[async_trait]
pub trait PipelineClient: Send + Sync {
    async fn list_pipeline_names(
        &self,
    ) -> Result<Vec<String>>;
    async fn get_pipeline_state(
        &self,
        name: &str,
    ) -> Result<PipelineState>;
}

#[async_trait]
pub trait ActionsClient: Send + Sync {
    async fn list_workflow_runs(
        &self,
    ) -> Result<Vec<WorkflowRunInfo>>;
}
```

**Step 2: Write AWS poller with mapping tests**

```rust
// src/poller/aws.rs
use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_codepipeline::Client;

use super::{PipelineClient, PipelineState};
use crate::model::BuildStatus;

pub struct AwsPipelineClient {
    client: Client,
}

impl AwsPipelineClient {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

/// Map AWS pipeline execution status string to
/// BuildStatus.
pub fn map_pipeline_status(
    status: Option<&str>,
) -> BuildStatus {
    match status {
        Some("InProgress") => BuildStatus::Running,
        Some("Succeeded") => BuildStatus::Succeeded,
        Some("Failed")
        | Some("Stopped")
        | Some("Superseded") => BuildStatus::Failed,
        _ => BuildStatus::Idle,
    }
}

#[async_trait]
impl PipelineClient for AwsPipelineClient {
    async fn list_pipeline_names(
        &self,
    ) -> Result<Vec<String>> {
        let resp = self
            .client
            .list_pipelines()
            .send()
            .await
            .context("failed to list pipelines")?;
        let names = resp
            .pipelines()
            .iter()
            .filter_map(|p| p.name().map(String::from))
            .collect();
        Ok(names)
    }

    async fn get_pipeline_state(
        &self,
        name: &str,
    ) -> Result<PipelineState> {
        let resp = self
            .client
            .get_pipeline_state()
            .name(name)
            .send()
            .await
            .with_context(|| {
                format!(
                    "failed to get state for {name}"
                )
            })?;
        let status = resp
            .stage_states()
            .last()
            .and_then(|s| {
                s.latest_execution()
                    .and_then(|e| {
                        e.status()
                            .map(|s| s.as_str())
                    })
            });
        Ok(PipelineState {
            name: name.to_string(),
            status: map_pipeline_status(status),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_in_progress() {
        assert_eq!(
            map_pipeline_status(Some("InProgress")),
            BuildStatus::Running,
        );
    }

    #[test]
    fn maps_succeeded() {
        assert_eq!(
            map_pipeline_status(Some("Succeeded")),
            BuildStatus::Succeeded,
        );
    }

    #[test]
    fn maps_failed() {
        assert_eq!(
            map_pipeline_status(Some("Failed")),
            BuildStatus::Failed,
        );
    }

    #[test]
    fn maps_stopped() {
        assert_eq!(
            map_pipeline_status(Some("Stopped")),
            BuildStatus::Failed,
        );
    }

    #[test]
    fn maps_superseded() {
        assert_eq!(
            map_pipeline_status(Some("Superseded")),
            BuildStatus::Failed,
        );
    }

    #[test]
    fn maps_none_to_idle() {
        assert_eq!(
            map_pipeline_status(None),
            BuildStatus::Idle,
        );
    }

    #[test]
    fn maps_unknown_to_idle() {
        assert_eq!(
            map_pipeline_status(Some("Whatever")),
            BuildStatus::Idle,
        );
    }
}
```

**Step 3: Write GitHub poller with mapping tests**

```rust
// src/poller/github.rs
use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{ActionsClient, WorkflowRunInfo};
use crate::model::BuildStatus;

pub struct GitHubActionsClient {
    octocrab: octocrab::Octocrab,
    owner: String,
    repo: String,
}

impl GitHubActionsClient {
    pub fn new(
        token: &str,
        owner: String,
        repo: String,
    ) -> Result<Self> {
        let octocrab = octocrab::Octocrab::builder()
            .personal_token(token.to_string())
            .build()
            .context("failed to build GitHub client")?;
        Ok(Self {
            octocrab,
            owner,
            repo,
        })
    }
}

/// Map GitHub run status + conclusion to BuildStatus.
pub fn map_run_status(
    status: &str,
    conclusion: Option<&str>,
) -> BuildStatus {
    match status {
        "in_progress" | "queued" | "waiting"
        | "pending" => BuildStatus::Running,
        "completed" => match conclusion {
            Some("success") => BuildStatus::Succeeded,
            Some("failure") | Some("cancelled")
            | Some("timed_out") => BuildStatus::Failed,
            _ => BuildStatus::Failed,
        },
        _ => BuildStatus::Idle,
    }
}

#[async_trait]
impl ActionsClient for GitHubActionsClient {
    async fn list_workflow_runs(
        &self,
    ) -> Result<Vec<WorkflowRunInfo>> {
        let route = format!(
            "/repos/{}/{}/actions/runs?per_page=50",
            self.owner, self.repo,
        );
        let resp: serde_json::Value = self
            .octocrab
            .get(&route, None::<&()>)
            .await
            .context(
                "failed to list workflow runs"
            )?;

        let mut latest_per_workflow =
            std::collections::HashMap::new();

        if let Some(runs) =
            resp["workflow_runs"].as_array()
        {
            for run in runs {
                let name = run["name"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let status = run["status"]
                    .as_str()
                    .unwrap_or("unknown");
                let conclusion =
                    run["conclusion"].as_str();

                // Keep first (most recent) per workflow
                latest_per_workflow
                    .entry(name.clone())
                    .or_insert_with(|| {
                        WorkflowRunInfo {
                            workflow_name: name,
                            status: map_run_status(
                                status, conclusion,
                            ),
                        }
                    });
            }
        }

        Ok(
            latest_per_workflow
                .into_values()
                .collect()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_in_progress() {
        assert_eq!(
            map_run_status("in_progress", None),
            BuildStatus::Running,
        );
    }

    #[test]
    fn maps_queued() {
        assert_eq!(
            map_run_status("queued", None),
            BuildStatus::Running,
        );
    }

    #[test]
    fn maps_completed_success() {
        assert_eq!(
            map_run_status(
                "completed",
                Some("success"),
            ),
            BuildStatus::Succeeded,
        );
    }

    #[test]
    fn maps_completed_failure() {
        assert_eq!(
            map_run_status(
                "completed",
                Some("failure"),
            ),
            BuildStatus::Failed,
        );
    }

    #[test]
    fn maps_completed_cancelled() {
        assert_eq!(
            map_run_status(
                "completed",
                Some("cancelled"),
            ),
            BuildStatus::Failed,
        );
    }

    #[test]
    fn maps_completed_timed_out() {
        assert_eq!(
            map_run_status(
                "completed",
                Some("timed_out"),
            ),
            BuildStatus::Failed,
        );
    }

    #[test]
    fn maps_completed_no_conclusion() {
        assert_eq!(
            map_run_status("completed", None),
            BuildStatus::Failed,
        );
    }

    #[test]
    fn maps_unknown_to_idle() {
        assert_eq!(
            map_run_status("unknown", None),
            BuildStatus::Idle,
        );
    }
}
```

**Step 4: Run all tests**

Run: `cargo test`
Expected: all tests pass (config + model + aws + github)

**Step 5: Verify clippy + fmt**

Run: `cargo clippy && cargo fmt --check`

**Step 6: Commit**

```bash
git add src/poller/ src/main.rs
git commit -m "feat: add poller traits with AWS and GitHub mapping"
```

---

### Task 6: Poller orchestrator

**Files:**

- Modify: `src/poller/mod.rs`
- Modify: `src/app.rs`

**Step 1: Add poll orchestration to poller/mod.rs**

Add a `poll_once` function that:

1. Calls `pipeline_client.list_pipeline_names()`
2. For each pipeline, calls `get_pipeline_state()`
3. Calls `actions_client.list_workflow_runs()`
4. Updates `App` bars: creates new `Bar` for newly
   discovered pipelines/workflows, marks disappeared
   ones as `gone`, calls `bar.update()` on each
5. Sets `last_poll` to `Utc::now()`
6. On error: adds warning string, leaves bars unchanged

```rust
// Add to src/poller/mod.rs

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use chrono::Utc;

use crate::app::App;
use crate::model::{Bar, BarSource};

pub async fn poll_once(
    app: &Arc<Mutex<App>>,
    pipeline_client: &dyn PipelineClient,
    actions_client: &dyn ActionsClient,
    tick_area_width: usize,
) {
    let (pipe_result, actions_result) = tokio::join!(
        poll_pipelines(pipeline_client),
        poll_actions(actions_client),
    );

    let mut app = app.lock().expect("app mutex poisoned");
    app.warnings.clear();

    match pipe_result {
        Ok(states) => {
            update_pipeline_bars(
                &mut app,
                states,
                tick_area_width,
            );
        }
        Err(e) => {
            app.warnings.push(
                format!("AWS: {e:#}")
            );
        }
    }

    match actions_result {
        Ok(runs) => {
            update_action_bars(
                &mut app,
                runs,
                tick_area_width,
            );
        }
        Err(e) => {
            app.warnings.push(
                format!("GitHub: {e:#}")
            );
        }
    }

    app.last_poll = Some(Utc::now());
}

async fn poll_pipelines(
    client: &dyn PipelineClient,
) -> Result<Vec<PipelineState>> {
    let names = client.list_pipeline_names().await?;
    let mut states = Vec::new();
    for name in &names {
        states.push(
            client.get_pipeline_state(name).await?
        );
    }
    Ok(states)
}

async fn poll_actions(
    client: &dyn ActionsClient,
) -> Result<Vec<WorkflowRunInfo>> {
    client.list_workflow_runs().await
}

fn update_pipeline_bars(
    app: &mut App,
    states: Vec<PipelineState>,
    tick_area_width: usize,
) {
    let seen: HashSet<String> =
        states.iter().map(|s| s.name.clone()).collect();

    // Mark disappeared pipelines
    for bar in &mut app.bars_pipelines {
        if !seen.contains(&bar.name) {
            bar.gone = true;
        }
    }

    for state in states {
        if let Some(bar) = app
            .bars_pipelines
            .iter_mut()
            .find(|b| b.name == state.name)
        {
            bar.gone = false;
            bar.update(state.status, tick_area_width);
        } else {
            let mut bar = Bar::new(
                state.name,
                BarSource::CodePipeline,
            );
            bar.update(state.status, tick_area_width);
            app.bars_pipelines.push(bar);
        }
    }
}

fn update_action_bars(
    app: &mut App,
    runs: Vec<WorkflowRunInfo>,
    tick_area_width: usize,
) {
    let seen: HashSet<String> = runs
        .iter()
        .map(|r| r.workflow_name.clone())
        .collect();

    for bar in &mut app.bars_actions {
        if !seen.contains(&bar.name) {
            bar.gone = true;
        }
    }

    for run in runs {
        if let Some(bar) = app
            .bars_actions
            .iter_mut()
            .find(|b| b.name == run.workflow_name)
        {
            bar.gone = false;
            bar.update(run.status, tick_area_width);
        } else {
            let mut bar = Bar::new(
                run.workflow_name,
                BarSource::GitHubAction,
            );
            bar.update(run.status, tick_area_width);
            app.bars_actions.push(bar);
        }
    }
}
```

**Step 2: Write tests for orchestrator using mock clients**

Add tests at the bottom of `src/poller/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct MockPipelineClient {
        pipelines: Vec<PipelineState>,
    }

    #[async_trait]
    impl PipelineClient for MockPipelineClient {
        async fn list_pipeline_names(
            &self,
        ) -> Result<Vec<String>> {
            Ok(self
                .pipelines
                .iter()
                .map(|p| p.name.clone())
                .collect())
        }

        async fn get_pipeline_state(
            &self,
            name: &str,
        ) -> Result<PipelineState> {
            self.pipelines
                .iter()
                .find(|p| p.name == name)
                .map(|p| PipelineState {
                    name: p.name.clone(),
                    status: p.status.clone(),
                })
                .context("not found")
        }
    }

    struct MockActionsClient {
        runs: Vec<WorkflowRunInfo>,
    }

    #[async_trait]
    impl ActionsClient for MockActionsClient {
        async fn list_workflow_runs(
            &self,
        ) -> Result<Vec<WorkflowRunInfo>> {
            Ok(self
                .runs
                .iter()
                .map(|r| WorkflowRunInfo {
                    workflow_name:
                        r.workflow_name.clone(),
                    status: r.status.clone(),
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn poll_creates_new_bars() {
        let app =
            Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient {
            pipelines: vec![PipelineState {
                name: "deploy".to_string(),
                status: BuildStatus::Running,
            }],
        };
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "ci".to_string(),
                status: BuildStatus::Succeeded,
            }],
        };

        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert_eq!(app.bars_pipelines.len(), 1);
        assert_eq!(
            app.bars_pipelines[0].name,
            "deploy"
        );
        assert_eq!(app.bars_actions.len(), 1);
        assert_eq!(
            app.bars_actions[0].name,
            "ci"
        );
        assert!(app.last_poll.is_some());
    }

    #[tokio::test]
    async fn poll_marks_gone_pipelines() {
        let app =
            Arc::new(Mutex::new(App::new()));
        // First poll: pipeline exists
        let pipes = MockPipelineClient {
            pipelines: vec![PipelineState {
                name: "deploy".to_string(),
                status: BuildStatus::Running,
            }],
        };
        let actions = MockActionsClient {
            runs: vec![],
        };
        poll_once(&app, &pipes, &actions, 20).await;

        // Second poll: pipeline gone
        let pipes = MockPipelineClient {
            pipelines: vec![],
        };
        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert_eq!(app.bars_pipelines.len(), 1);
        assert!(app.bars_pipelines[0].gone);
    }

    #[tokio::test]
    async fn poll_updates_existing_bars() {
        let app =
            Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient {
            pipelines: vec![PipelineState {
                name: "deploy".to_string(),
                status: BuildStatus::Running,
            }],
        };
        let actions = MockActionsClient {
            runs: vec![],
        };
        poll_once(&app, &pipes, &actions, 20).await;
        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert_eq!(app.bars_pipelines.len(), 1);
        assert_eq!(app.bars_pipelines[0].fill, 2);
    }
}
```

**Step 3: Run tests**

Run: `cargo test`
Expected: all tests pass

**Step 4: Verify clippy + fmt**

Run: `cargo clippy && cargo fmt --check`

**Step 5: Commit**

```bash
git add src/poller/mod.rs src/poller/aws.rs \
  src/poller/github.rs src/app.rs
git commit -m "feat: add poller orchestrator with mock tests"
```

---

## Phase 3: TUI Skeleton

### Task 7: UI module with terminal init and event loop

**Files:**

- Create: `src/ui/mod.rs`
- Create: `src/ui/header.rs`
- Create: `src/ui/bar.rs`
- Create: `src/ui/statusbar.rs`
- Modify: `src/main.rs`

**Step 1: Create UI module stubs**

```rust
// src/ui/mod.rs
pub mod bar;
pub mod header;
pub mod statusbar;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind,
};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::DefaultTerminal;

use crate::app::App;

const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 10;
const TICK_RATE_MS: u64 = 250;

pub fn run_ui(
    app: Arc<Mutex<App>>,
    mut terminal: DefaultTerminal,
) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let size = frame.area();

            if size.width < MIN_WIDTH
                || size.height < MIN_HEIGHT
            {
                let msg = Paragraph::new(
                    "Terminal too small",
                )
                .style(
                    Style::default()
                        .fg(Color::Red),
                );
                frame.render_widget(msg, size);
                return;
            }

            let app = app.lock()
                .expect("app mutex poisoned");

            let [header_area, pipes_area,
                 actions_area, status_area] =
                Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Fill(1),
                    Constraint::Fill(1),
                    Constraint::Length(1),
                ])
                .areas(size);

            // Placeholder rendering
            frame.render_widget(
                Paragraph::new("cibars"),
                header_area,
            );

            frame.render_widget(
                Block::default()
                    .title("CodePipelines")
                    .borders(Borders::ALL),
                pipes_area,
            );

            frame.render_widget(
                Block::default()
                    .title("GitHub Actions")
                    .borders(Borders::ALL),
                actions_area,
            );

            frame.render_widget(
                Paragraph::new(
                    "q=quit  r=refresh"
                ),
                status_area,
            );

            drop(app);
        })?;

        if event::poll(
            Duration::from_millis(TICK_RATE_MS),
        )? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => {
                        return Ok(());
                    }
                    KeyCode::Char('r') => {
                        // TODO: trigger immediate poll
                    }
                    _ => {}
                }
            }
        }
    }
}
```

```rust
// src/ui/header.rs
// placeholder — populated in Task 9
```

```rust
// src/ui/bar.rs
// placeholder — populated in Task 8
```

```rust
// src/ui/statusbar.rs
// placeholder — populated in Task 9
```

**Step 2: Wire up main.rs with tokio + TUI**

```rust
// src/main.rs
mod app;
mod config;
mod model;
mod poller;
mod ui;

use std::sync::{Arc, Mutex};

use anyhow::Result;

use app::App;
use config::Config;

fn main() -> Result<()> {
    let (config, token) = Config::load()?;

    let app = Arc::new(Mutex::new(App::new()));

    // Build tokio runtime for async polling
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    // TODO: spawn poller task (Task 10)

    // Init TUI and run event loop on main thread
    let terminal = ratatui::init();
    let result = ui::run_ui(app.clone(), terminal);
    ratatui::restore();

    // Suppress unused warnings until pollers wired
    let _ = (config, token);

    result
}
```

**Step 3: Verify build + clippy**

Run: `cargo build && cargo clippy && cargo fmt --check`
Expected: compiles, no warnings

**Step 4: Commit**

```bash
git add src/ui/ src/main.rs
git commit -m "feat: add TUI skeleton with placeholder layout"
```

---

## Phase 4: Populated UI

### Task 8: Bar widget (TDD)

**Files:**

- Modify: `src/ui/bar.rs`

**Step 1: Write bar rendering with tests**

```rust
// src/ui/bar.rs
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::model::{Bar, BuildStatus};

/// Max chars for the name column
pub const MAX_NAME_WIDTH: usize = 30;

pub struct BarWidget<'a> {
    bar: &'a Bar,
    name_width: usize,
}

impl<'a> BarWidget<'a> {
    pub fn new(
        bar: &'a Bar,
        name_width: usize,
    ) -> Self {
        Self {
            bar,
            name_width: name_width
                .min(MAX_NAME_WIDTH),
        }
    }

    fn status_color(&self) -> Color {
        match self.bar.status {
            BuildStatus::Running => Color::Yellow,
            BuildStatus::Succeeded => Color::Green,
            BuildStatus::Failed => Color::Red,
            BuildStatus::Idle => Color::DarkGray,
        }
    }
}

impl Widget for BarWidget<'_> {
    fn render(
        self,
        area: Rect,
        buf: &mut Buffer,
    ) {
        if area.width < 10 || area.height < 1 {
            return;
        }

        let name_col = self.name_width + 2;
        // [, ], and 2 spaces between name and [
        let overhead = name_col + 2;
        if (area.width as usize) <= overhead {
            return;
        }
        let fill_width =
            area.width as usize - overhead;
        let color = self.status_color();

        let name_display = if self.bar.gone {
            format!(
                "{:<w$}*",
                self.bar.name,
                w = self.name_width - 1,
            )
        } else {
            format!(
                "{:<w$}",
                self.bar.name,
                w = self.name_width,
            )
        };

        let filled = self.bar.fill.min(fill_width);
        let empty = fill_width - filled;

        let line = Line::from(vec![
            Span::raw(format!("{name_display}  ")),
            Span::raw("["),
            Span::styled(
                "|".repeat(filled),
                Style::default().fg(color),
            ),
            Span::raw(" ".repeat(empty)),
            Span::raw("]"),
        ]);

        line.render(area, buf);
    }
}

/// Compute the name column width for a set of bars.
pub fn compute_name_width(
    bars: &[Bar],
) -> usize {
    bars.iter()
        .map(|b| b.name.len())
        .max()
        .unwrap_or(10)
        .min(MAX_NAME_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BarSource;

    fn make_bar(
        name: &str,
        status: BuildStatus,
        fill: usize,
    ) -> Bar {
        Bar {
            name: name.to_string(),
            source: BarSource::CodePipeline,
            status,
            fill,
            write_pos: fill,
            gone: false,
        }
    }

    #[test]
    fn renders_idle_bar() {
        let bar =
            make_bar("deploy", BuildStatus::Idle, 0);
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next()
                .unwrap_or(' '))
            .collect();
        // Name padded to 10 + 2 spaces + [ + 11 spaces + ]
        assert!(content.starts_with("deploy"));
        assert!(content.contains('['));
        assert!(content.ends_with(']'));
    }

    #[test]
    fn renders_running_bar_in_yellow() {
        let bar =
            make_bar("build", BuildStatus::Running, 3);
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        // Check that filled cells have yellow fg
        let bracket_pos = buf
            .content()
            .iter()
            .position(|c| c.symbol() == "[")
            .unwrap();
        let first_fill = &buf.content()[
            bracket_pos + 1
        ];
        assert_eq!(first_fill.symbol(), "|");
        assert_eq!(
            first_fill.fg,
            Color::Yellow,
        );
    }

    #[test]
    fn renders_succeeded_bar_in_green() {
        let bar = make_bar(
            "test",
            BuildStatus::Succeeded,
            5,
        );
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let bracket_pos = buf
            .content()
            .iter()
            .position(|c| c.symbol() == "[")
            .unwrap();
        let first_fill = &buf.content()[
            bracket_pos + 1
        ];
        assert_eq!(first_fill.fg, Color::Green);
    }

    #[test]
    fn renders_failed_bar_in_red() {
        let bar = make_bar(
            "lint",
            BuildStatus::Failed,
            2,
        );
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let bracket_pos = buf
            .content()
            .iter()
            .position(|c| c.symbol() == "[")
            .unwrap();
        let first_fill = &buf.content()[
            bracket_pos + 1
        ];
        assert_eq!(first_fill.fg, Color::Red);
    }

    #[test]
    fn gone_bar_shows_asterisk() {
        let mut bar =
            make_bar("old", BuildStatus::Idle, 0);
        bar.gone = true;
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next()
                .unwrap_or(' '))
            .collect();
        assert!(content.contains('*'));
    }

    #[test]
    fn name_width_computed_correctly() {
        let bars = vec![
            make_bar("a", BuildStatus::Idle, 0),
            make_bar(
                "long-name",
                BuildStatus::Idle,
                0,
            ),
        ];
        assert_eq!(compute_name_width(&bars), 9);
    }
}
```

**Step 2: Run tests**

Run: `cargo test --lib ui::bar`
Expected: all 6 tests pass

**Step 3: Verify clippy + fmt**

Run: `cargo clippy && cargo fmt --check`

**Step 4: Commit**

```bash
git add src/ui/bar.rs
git commit -m "feat: add bar widget with color rendering"
```

---

### Task 9: Header and status bar widgets

**Files:**

- Modify: `src/ui/header.rs`
- Modify: `src/ui/statusbar.rs`

**Step 1: Implement header.rs**

```rust
// src/ui/header.rs
use chrono::Local;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

pub struct Header<'a> {
    pub profile: &'a str,
    pub region: &'a str,
    pub repo: &'a str,
}

impl Widget for Header<'_> {
    fn render(
        self,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let time = Local::now().format("%H:%M:%S");
        let line = Line::from(vec![
            Span::styled(
                "cibars",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(
                        Modifier::BOLD
                    ),
            ),
            Span::raw(" | "),
            Span::raw(self.profile),
            Span::raw(" | "),
            Span::raw(self.region),
            Span::raw(" | "),
            Span::raw(self.repo),
            Span::raw(" | "),
            Span::styled(
                format!("{time}"),
                Style::default()
                    .fg(Color::DarkGray),
            ),
        ]);
        line.render(area, buf);
    }
}
```

**Step 2: Implement statusbar.rs**

```rust
// src/ui/statusbar.rs
use chrono::{DateTime, Utc};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

pub struct StatusBar<'a> {
    pub last_poll: &'a Option<DateTime<Utc>>,
    pub warnings: &'a [String],
}

impl Widget for StatusBar<'_> {
    fn render(
        self,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let poll_text =
            match self.last_poll {
                Some(t) => {
                    let local = t
                        .with_timezone(
                            &chrono::Local,
                        );
                    format!(
                        "Last poll: {}",
                        local.format("%H:%M:%S"),
                    )
                }
                None => {
                    "Last poll: --:--:--"
                        .to_string()
                }
            };

        let mut spans = vec![
            Span::raw(poll_text),
            Span::raw(" | r=refresh q=quit"),
        ];

        if !self.warnings.is_empty() {
            let warn_text = format!(
                " | {}",
                self.warnings.join("; "),
            );
            spans.push(Span::styled(
                warn_text,
                Style::default()
                    .fg(Color::Yellow),
            ));
        }

        Line::from(spans).render(area, buf);
    }
}
```

**Step 3: Run build + clippy**

Run: `cargo build && cargo clippy && cargo fmt --check`

**Step 4: Commit**

```bash
git add src/ui/header.rs src/ui/statusbar.rs
git commit -m "feat: add header and status bar widgets"
```

---

### Task 10: Wire live data into UI + ordering

**Files:**

- Modify: `src/ui/mod.rs`
- Modify: `src/main.rs`
- Modify: `src/config.rs` (store split owner/repo)

**Step 1: Update ui/mod.rs to render real bars**

Replace the placeholder rendering with actual widget
calls. Use `Layout::vertical` with dynamic constraints
based on bar count. Render bars sorted: running first
(by name), then non-running (by name).

Key sorting logic:

```rust
fn sorted_bars(bars: &[Bar]) -> Vec<&Bar> {
    let mut sorted: Vec<&Bar> =
        bars.iter().collect();
    sorted.sort_by(|a, b| {
        let a_running =
            a.status == BuildStatus::Running;
        let b_running =
            b.status == BuildStatus::Running;
        b_running
            .cmp(&a_running)
            .then(a.name.cmp(&b.name))
    });
    sorted
}
```

Render each section with a title line and bar rows.
Show "No pipelines found" / "No recent workflow runs"
when respective vec is empty.

**Step 2: Wire poller task in main.rs**

```rust
// In main.rs, after creating App and runtime:
let poll_app = app.clone();
rt.spawn(async move {
    let aws_config =
        aws_config::defaults(
            aws_config::BehaviorVersion::latest(),
        )
        .profile_name(&config.aws_profile)
        .region(
            aws_config::Region::new(
                config.region.clone(),
            ),
        )
        .load()
        .await;

    let pipe_client =
        poller::aws::AwsPipelineClient::new(
            aws_sdk_codepipeline::Client::new(
                &aws_config,
            ),
        );

    let (owner, repo) =
        config.github_repo.split_once('/')
            .expect("validated in config");
    let actions_client =
        poller::github::GitHubActionsClient::new(
            &token,
            owner.to_string(),
            repo.to_string(),
        )
        .expect("failed to create GitHub client");

    loop {
        poller::poll_once(
            &poll_app,
            &pipe_client,
            &actions_client,
            60, // default tick area width
        )
        .await;
        tokio::time::sleep(
            std::time::Duration::from_secs(30),
        )
        .await;
    }
});
```

**Step 3: Verify build + clippy + all tests**

Run: `cargo test && cargo clippy && cargo fmt --check`
Expected: all pass

**Step 4: Commit**

```bash
git add src/ui/mod.rs src/main.rs
git commit -m "feat: wire live polling data into TUI"
```

---

## Phase 5: Polish and Edge Cases

### Task 11: Terminal too small + inline warnings

Already handled in the `run_ui` function (Task 7).
Verify it works by resizing terminal during runtime.
Warnings are rendered via StatusBar widget (Task 9).
No additional code needed — this is a verification task.

**Step 1: Manual test**

Run cibars, resize terminal below 80x10.
Expected: shows "Terminal too small".
Resize back: normal UI returns.

**Step 2: Commit (if any fixes needed)**

---

### Task 12: Graceful shutdown + tracing

**Files:**

- Modify: `src/main.rs`

**Step 1: Add tracing setup**

```rust
use tracing_subscriber::{
    fmt, EnvFilter, layer::SubscriberExt,
    util::SubscriberInitExt,
};

fn setup_tracing() -> Result<()> {
    let log_dir =
        dirs::home_dir()
            .context("no home dir")?
            .join(".cibars");
    std::fs::create_dir_all(&log_dir)?;
    let log_file = std::fs::File::create(
        log_dir.join("cibars.log"),
    )?;
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    EnvFilter::new("info")
                }),
        )
        .with(
            fmt::layer()
                .with_writer(log_file)
                .with_ansi(false),
        )
        .init();
    Ok(())
}
```

Note: add `dirs = "6"` to Cargo.toml dependencies.

**Step 2: SIGINT/SIGTERM handling**

The `ratatui::init()` already installs a panic hook
that calls `ratatui::restore()`. For SIGINT, the
`crossterm` event loop catches `Ctrl+C` as a key event
when raw mode is enabled. The `q` key and any error
from the draw loop will exit `run_ui()`, after which
`ratatui::restore()` runs in main. This covers all
exit paths.

Add `tracing::info!` calls at key points:
startup, each poll cycle, shutdown.

**Step 3: Verify clippy + fmt + tests**

Run: `cargo test && cargo clippy && cargo fmt --check`

**Step 4: Commit**

```bash
git add src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: add tracing to file + graceful shutdown"
```

---

### Task 13: Manual refresh via 'r' key

**Files:**

- Modify: `src/main.rs`
- Modify: `src/ui/mod.rs`

**Step 1: Add a watch channel for refresh signal**

In `main.rs`, create a `tokio::sync::watch` channel.
Pass the sender to `run_ui` and the receiver to the
poller task. When 'r' is pressed, UI sends signal.
Poller checks for signal before sleeping 30s.

```rust
// In main.rs:
let (refresh_tx, refresh_rx) =
    tokio::sync::watch::channel(());

// In poller loop:
loop {
    poller::poll_once(...).await;
    tokio::select! {
        _ = tokio::time::sleep(
            Duration::from_secs(30)
        ) => {}
        _ = refresh_rx.changed() => {}
    }
}
```

```rust
// In ui/run_ui, on 'r' key:
KeyCode::Char('r') => {
    let _ = refresh_tx.send(());
}
```

Update `run_ui` signature to accept `refresh_tx`.

**Step 2: Verify build + tests**

Run: `cargo test && cargo clippy && cargo fmt --check`

**Step 3: Commit**

```bash
git add src/main.rs src/ui/mod.rs
git commit -m "feat: add manual refresh via r key"
```

---

## Summary

| Phase | Tasks | Key deliverable |
|---|---|---|
| 1 | 1-2 | Compiling project + CLI config |
| 2 | 3-6 | Model + pollers + orchestrator |
| 3 | 7 | TUI skeleton with placeholders |
| 4 | 8-10 | Live bars with real data |
| 5 | 11-13 | Polish, logging, refresh |

**Total: 13 tasks, each with TDD steps.**

After all tasks: run `cargo test && cargo clippy &&
cargo fmt --check` to verify everything is green.
