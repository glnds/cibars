pub mod aws;
pub mod github;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use crate::app::App;
use crate::model::{Bar, BarSource, BuildStatus};

/// How long to back off when GitHub rate limit is hit.
const RATE_LIMIT_BACKOFF_SECS: u64 = 60;

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
    async fn list_pipeline_names(&self) -> Result<Vec<String>>;
    async fn get_pipeline_state(&self, name: &str) -> Result<PipelineState>;
}

#[async_trait]
pub trait ActionsClient: Send + Sync {
    async fn list_workflow_runs(&self) -> Result<Vec<WorkflowRunInfo>>;
}

pub async fn poll_once(
    app: &Arc<Mutex<App>>,
    pipeline_client: &dyn PipelineClient,
    actions_client: &dyn ActionsClient,
    tick_area_width: usize,
) {
    // Check if GitHub is rate-limited
    let skip_github = {
        let a = app.lock().expect("app mutex poisoned");
        a.rate_limited_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    };

    let pipe_result = poll_pipelines(pipeline_client).await;
    let actions_result = if skip_github {
        Err(anyhow::anyhow!("rate-limited, backing off"))
    } else {
        poll_actions(actions_client).await
    };

    let mut app = app.lock().expect("app mutex poisoned");
    app.warnings.clear();

    match pipe_result {
        Ok(states) => {
            update_pipeline_bars(&mut app, states, tick_area_width);
        }
        Err(e) => {
            app.warnings.push(format!("AWS: {e:#}"));
        }
    }

    match actions_result {
        Ok(runs) => {
            update_action_bars(&mut app, runs, tick_area_width);
            // Clear rate limit on success
            app.rate_limited_until = None;
        }
        Err(e) => {
            let msg = format!("{e:#}");
            // Set backoff if error looks like a rate limit
            if msg.to_lowercase().contains("rate limit") {
                app.rate_limited_until =
                    Some(Instant::now() + Duration::from_secs(RATE_LIMIT_BACKOFF_SECS));
                app.warnings.push(format!(
                    "GitHub: rate limited, backing off {RATE_LIMIT_BACKOFF_SECS}s"
                ));
            } else if !skip_github {
                app.warnings.push(format!("GitHub: {msg}"));
            } else {
                app.warnings
                    .push("GitHub: rate-limited, backing off".to_string());
            }
        }
    }

    app.last_poll = Some(Utc::now());
}

async fn poll_pipelines(client: &dyn PipelineClient) -> Result<Vec<PipelineState>> {
    let names = client.list_pipeline_names().await?;
    let mut states = Vec::new();
    for name in &names {
        states.push(client.get_pipeline_state(name).await?);
    }
    Ok(states)
}

async fn poll_actions(client: &dyn ActionsClient) -> Result<Vec<WorkflowRunInfo>> {
    client.list_workflow_runs().await
}

fn update_pipeline_bars(app: &mut App, states: Vec<PipelineState>, tick_area_width: usize) {
    let seen: HashSet<String> = states.iter().map(|s| s.name.clone()).collect();

    for bar in &mut app.bars_pipelines {
        if !seen.contains(&bar.name) {
            bar.gone = true;
        }
    }

    for state in states {
        if let Some(bar) = app.bars_pipelines.iter_mut().find(|b| b.name == state.name) {
            bar.gone = false;
            bar.update(state.status, tick_area_width);
        } else {
            let mut bar = Bar::new(state.name, BarSource::CodePipeline);
            bar.update(state.status, tick_area_width);
            app.bars_pipelines.push(bar);
        }
    }
}

fn update_action_bars(app: &mut App, runs: Vec<WorkflowRunInfo>, tick_area_width: usize) {
    let seen: HashSet<String> = runs.iter().map(|r| r.workflow_name.clone()).collect();

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
            let mut bar = Bar::new(run.workflow_name, BarSource::GitHubAction);
            bar.update(run.status, tick_area_width);
            app.bars_actions.push(bar);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;

    struct MockPipelineClient {
        pipelines: Vec<PipelineState>,
    }

    #[async_trait]
    impl PipelineClient for MockPipelineClient {
        async fn list_pipeline_names(&self) -> Result<Vec<String>> {
            Ok(self.pipelines.iter().map(|p| p.name.clone()).collect())
        }
        async fn get_pipeline_state(&self, name: &str) -> Result<PipelineState> {
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
        async fn list_workflow_runs(&self) -> Result<Vec<WorkflowRunInfo>> {
            Ok(self
                .runs
                .iter()
                .map(|r| WorkflowRunInfo {
                    workflow_name: r.workflow_name.clone(),
                    status: r.status.clone(),
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn poll_creates_new_bars() {
        let app = Arc::new(Mutex::new(App::new()));
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
        assert_eq!(app.bars_pipelines[0].name, "deploy");
        assert_eq!(app.bars_actions.len(), 1);
        assert_eq!(app.bars_actions[0].name, "ci");
        assert!(app.last_poll.is_some());
    }

    #[tokio::test]
    async fn poll_marks_gone_pipelines() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient {
            pipelines: vec![PipelineState {
                name: "deploy".to_string(),
                status: BuildStatus::Running,
            }],
        };
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions, 20).await;

        let pipes = MockPipelineClient { pipelines: vec![] };
        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert_eq!(app.bars_pipelines.len(), 1);
        assert!(app.bars_pipelines[0].gone);
    }

    #[tokio::test]
    async fn poll_updates_existing_bars() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient {
            pipelines: vec![PipelineState {
                name: "deploy".to_string(),
                status: BuildStatus::Running,
            }],
        };
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions, 20).await;
        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert_eq!(app.bars_pipelines.len(), 1);
        assert_eq!(app.bars_pipelines[0].fill, 2);
    }

    struct FailingPipelineClient;

    #[async_trait]
    impl PipelineClient for FailingPipelineClient {
        async fn list_pipeline_names(&self) -> Result<Vec<String>> {
            anyhow::bail!("connection refused")
        }
        async fn get_pipeline_state(&self, _name: &str) -> Result<PipelineState> {
            anyhow::bail!("connection refused")
        }
    }

    #[tokio::test]
    async fn poll_error_adds_warning() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = FailingPipelineClient;
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert_eq!(app.warnings.len(), 1);
        assert!(app.warnings[0].contains("AWS"));
        assert!(app.last_poll.is_some());
    }
}
