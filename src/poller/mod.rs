pub mod aws;
pub mod github;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use crate::app::App;
use crate::model::{Bar, BarSource, BuildStatus, WorkflowGroup};

/// How long to back off when GitHub rate limit is hit.
const RATE_LIMIT_BACKOFF_SECS: u64 = 60;

/// Simplified pipeline state from AWS API
pub struct PipelineState {
    pub name: String,
    pub status: BuildStatus,
}

/// A single job within a workflow run
pub struct JobInfo {
    pub name: String,
    pub status: BuildStatus,
}

/// Simplified workflow run from GitHub API
pub struct WorkflowRunInfo {
    pub workflow_name: String,
    /// Used by GitHubActionsClient to fetch jobs; not read elsewhere.
    #[allow(dead_code)]
    pub run_id: u64,
    /// Overall run status; kept for data-model completeness.
    #[allow(dead_code)]
    pub status: BuildStatus,
    pub jobs: Vec<JobInfo>,
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
    let skip_github = {
        let a = app.lock().expect("app mutex poisoned");
        a.rate_limited_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    };

    {
        let mut a = app.lock().expect("app mutex poisoned");
        a.warnings.clear();
    }

    let pipe_fut = poll_pipelines(pipeline_client);
    let actions_fut = async {
        if skip_github {
            Err(anyhow::anyhow!("rate-limited, backing off"))
        } else {
            poll_actions(actions_client).await
        }
    };

    tokio::pin!(pipe_fut);
    tokio::pin!(actions_fut);
    let mut pipe_done = false;
    let mut actions_done = false;

    while !pipe_done || !actions_done {
        tokio::select! {
            result = &mut pipe_fut, if !pipe_done => {
                pipe_done = true;
                let mut a = app.lock().expect("app mutex poisoned");
                match result {
                    Ok(states) => {
                        update_pipeline_bars(&mut a, states, tick_area_width);
                    }
                    Err(e) => {
                        a.warnings.push(format!("AWS: {e:#}"));
                    }
                }
                a.loading_pipelines = false;
            }
            result = &mut actions_fut, if !actions_done => {
                actions_done = true;
                let mut a = app.lock().expect("app mutex poisoned");
                match result {
                    Ok(runs) => {
                        update_workflows(&mut a, runs, tick_area_width);
                        a.rate_limited_until = None;
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        if msg.to_lowercase().contains("rate limit") {
                            a.rate_limited_until =
                                Some(Instant::now() + Duration::from_secs(RATE_LIMIT_BACKOFF_SECS));
                            a.warnings.push(format!(
                                "GitHub: rate limited, backing off {RATE_LIMIT_BACKOFF_SECS}s"
                            ));
                        } else if !skip_github {
                            a.warnings.push(format!("GitHub: {msg}"));
                        } else {
                            a.warnings
                                .push("GitHub: rate-limited, backing off".to_string());
                        }
                    }
                }
                a.loading_actions = false;
            }
        }
    }

    let mut a = app.lock().expect("app mutex poisoned");
    a.last_poll = Some(Utc::now());
}

async fn poll_pipelines(client: &dyn PipelineClient) -> Result<Vec<PipelineState>> {
    let names = client.list_pipeline_names().await?;
    let futs: Vec<_> = names.iter().map(|n| client.get_pipeline_state(n)).collect();
    futures::future::join_all(futs).await.into_iter().collect()
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

fn update_workflows(app: &mut App, runs: Vec<WorkflowRunInfo>, tick_area_width: usize) {
    let seen: HashSet<String> = runs.iter().map(|r| r.workflow_name.clone()).collect();

    // Mark gone groups
    for group in &mut app.workflow_groups {
        if !seen.contains(&group.name) {
            group.gone = true;
        }
    }

    for run in runs {
        let group = if let Some(g) = app
            .workflow_groups
            .iter_mut()
            .find(|g| g.name == run.workflow_name)
        {
            g.gone = false;
            g
        } else {
            app.workflow_groups.push(WorkflowGroup {
                name: run.workflow_name,
                jobs: Vec::new(),
                gone: false,
            });
            app.workflow_groups.last_mut().expect("just pushed")
        };

        let seen_jobs: HashSet<String> = run.jobs.iter().map(|j| j.name.clone()).collect();

        // Mark gone jobs
        for job in &mut group.jobs {
            if !seen_jobs.contains(&job.name) {
                job.gone = true;
            }
        }

        // Create/update jobs
        for job_info in run.jobs {
            if let Some(bar) = group.jobs.iter_mut().find(|b| b.name == job_info.name) {
                bar.gone = false;
                bar.update(job_info.status, tick_area_width);
            } else {
                let mut bar = Bar::new(job_info.name, BarSource::GitHubAction);
                bar.update(job_info.status, tick_area_width);
                group.jobs.push(bar);
            }
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
                    run_id: r.run_id,
                    status: r.status.clone(),
                    jobs: r
                        .jobs
                        .iter()
                        .map(|j| JobInfo {
                            name: j.name.clone(),
                            status: j.status.clone(),
                        })
                        .collect(),
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
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![JobInfo {
                    name: "build".to_string(),
                    status: BuildStatus::Succeeded,
                }],
            }],
        };

        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert_eq!(app.bars_pipelines.len(), 1);
        assert_eq!(app.bars_pipelines[0].name, "deploy");
        assert_eq!(app.workflow_groups.len(), 1);
        assert_eq!(app.workflow_groups[0].name, "ci");
        assert_eq!(app.workflow_groups[0].jobs.len(), 1);
        assert_eq!(app.workflow_groups[0].jobs[0].name, "build");
        assert!(app.last_poll.is_some());
        assert!(!app.loading_pipelines);
        assert!(!app.loading_actions);
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

    #[tokio::test]
    async fn update_workflows_creates_groups_with_jobs() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient { pipelines: vec![] };
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".to_string(),
                run_id: 100,
                status: BuildStatus::Running,
                jobs: vec![
                    JobInfo {
                        name: "build".to_string(),
                        status: BuildStatus::Succeeded,
                    },
                    JobInfo {
                        name: "test".to_string(),
                        status: BuildStatus::Running,
                    },
                ],
            }],
        };
        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert_eq!(app.workflow_groups.len(), 1);
        assert_eq!(app.workflow_groups[0].name, "CI");
        assert_eq!(app.workflow_groups[0].jobs.len(), 2);
        assert_eq!(app.workflow_groups[0].jobs[0].name, "build");
        assert_eq!(
            app.workflow_groups[0].jobs[0].status,
            BuildStatus::Succeeded
        );
        assert_eq!(app.workflow_groups[0].jobs[1].name, "test");
        assert_eq!(app.workflow_groups[0].jobs[1].status, BuildStatus::Running);
    }

    #[tokio::test]
    async fn update_workflows_marks_gone_groups() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient { pipelines: vec![] };

        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".to_string(),
                run_id: 100,
                status: BuildStatus::Succeeded,
                jobs: vec![],
            }],
        };
        poll_once(&app, &pipes, &actions, 20).await;

        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert_eq!(app.workflow_groups.len(), 1);
        assert!(app.workflow_groups[0].gone);
    }

    #[tokio::test]
    async fn poll_clears_loading_flags_on_error() {
        let app = Arc::new(Mutex::new(App::new()));
        assert!(app.lock().unwrap().loading_pipelines);
        assert!(app.lock().unwrap().loading_actions);

        let pipes = FailingPipelineClient;
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        assert!(!app.loading_pipelines);
        assert!(!app.loading_actions);
    }

    #[tokio::test]
    async fn update_workflows_marks_gone_jobs() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient { pipelines: vec![] };

        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".to_string(),
                run_id: 100,
                status: BuildStatus::Running,
                jobs: vec![
                    JobInfo {
                        name: "build".to_string(),
                        status: BuildStatus::Running,
                    },
                    JobInfo {
                        name: "test".to_string(),
                        status: BuildStatus::Running,
                    },
                ],
            }],
        };
        poll_once(&app, &pipes, &actions, 20).await;

        // Second poll: "test" job disappears
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".to_string(),
                run_id: 101,
                status: BuildStatus::Running,
                jobs: vec![JobInfo {
                    name: "build".to_string(),
                    status: BuildStatus::Succeeded,
                }],
            }],
        };
        poll_once(&app, &pipes, &actions, 20).await;

        let app = app.lock().unwrap();
        let group = &app.workflow_groups[0];
        assert_eq!(group.jobs.len(), 2);
        let build = group.jobs.iter().find(|j| j.name == "build").unwrap();
        assert!(!build.gone);
        let test = group.jobs.iter().find(|j| j.name == "test").unwrap();
        assert!(test.gone);
    }
}
