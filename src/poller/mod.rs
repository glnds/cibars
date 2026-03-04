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

/// Workflow summary from the runs API (no jobs yet).
pub struct WorkflowRunSummary {
    pub workflow_name: String,
    pub run_id: u64,
    pub status: BuildStatus,
}

/// Full workflow run including jobs. Used in tests.
#[cfg(test)]
pub struct WorkflowRunInfo {
    pub workflow_name: String,
    pub run_id: u64,
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
    /// Fast: single API call, returns workflow-level status only.
    async fn list_latest_runs(&self) -> Result<Vec<WorkflowRunSummary>>;
    /// Fetch jobs for a specific run.
    async fn fetch_run_jobs(&self, run_id: u64) -> Result<Vec<JobInfo>>;
}

/// Poll AWS pipelines and update app state. Clears only AWS-specific warnings.
pub async fn poll_pipelines_tick(
    app: &Arc<Mutex<App>>,
    client: &dyn PipelineClient,
    profile: &str,
) {
    {
        let mut a = app.lock().expect("app mutex poisoned");
        a.warnings.retain(|w| !w.starts_with("AWS:"));
    }

    match poll_pipelines(client).await {
        Ok(states) => {
            tracing::debug!(count = states.len(), "polled pipelines");
            let mut a = app.lock().expect("app mutex poisoned");
            update_pipeline_bars(&mut a, states);
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let mut a = app.lock().expect("app mutex poisoned");
            if msg.contains("ExpiredToken") || msg.contains("UnauthorizedException") {
                a.warnings.push(format!(
                    "AWS: SSO session expired \u{2014} run `aws sso login --profile {profile}` then press b"
                ));
            } else {
                a.warnings.push(format!("AWS: {msg}"));
            }
        }
    }

    let mut a = app.lock().expect("app mutex poisoned");
    a.loading_pipelines = false;
    a.last_poll = Some(Utc::now());
}

/// Poll GitHub Actions in two phases for fast perceived startup.
/// Phase 1: fetch workflow summaries (1 API call) → update UI immediately.
/// Phase 2: fetch jobs per workflow in parallel → update UI with details.
pub async fn poll_actions_tick(app: &Arc<Mutex<App>>, client: &dyn ActionsClient) {
    let skip_github = {
        let a = app.lock().expect("app mutex poisoned");
        a.rate_limited_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    };

    {
        let mut a = app.lock().expect("app mutex poisoned");
        a.warnings.retain(|w| !w.starts_with("GitHub:"));
    }

    if skip_github {
        let mut a = app.lock().expect("app mutex poisoned");
        a.warnings
            .push("GitHub: rate-limited, backing off".to_string());
        a.loading_actions = false;
        return;
    }

    // Phase 1: fetch workflow summaries (single API call)
    let summaries = match client.list_latest_runs().await {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("{e:#}");
            let mut a = app.lock().expect("app mutex poisoned");
            if msg.to_lowercase().contains("rate limit") {
                a.rate_limited_until =
                    Some(Instant::now() + Duration::from_secs(RATE_LIMIT_BACKOFF_SECS));
                a.warnings.push(format!(
                    "GitHub: rate limited, backing off {RATE_LIMIT_BACKOFF_SECS}s"
                ));
            } else {
                a.warnings.push(format!("GitHub: {msg}"));
            }
            a.loading_actions = false;
            a.last_poll = Some(Utc::now());
            return;
        }
    };

    // Update UI with workflow-level status immediately
    {
        let mut a = app.lock().expect("app mutex poisoned");
        update_workflow_summaries(&mut a, &summaries);
        a.rate_limited_until = None;
        a.loading_actions = false;
        a.last_poll = Some(Utc::now());
    }

    // Phase 2: fetch jobs for each workflow in parallel
    let job_futs: Vec<_> = summaries
        .iter()
        .map(|s| client.fetch_run_jobs(s.run_id))
        .collect();
    let job_results = futures::future::join_all(job_futs).await;

    {
        let mut a = app.lock().expect("app mutex poisoned");
        for (summary, jobs_result) in summaries.iter().zip(job_results) {
            match jobs_result {
                Ok(jobs) => {
                    update_workflow_jobs(&mut a, &summary.workflow_name, jobs);
                }
                Err(e) => {
                    tracing::error!(
                        workflow = %summary.workflow_name,
                        error = %e,
                        "failed to fetch jobs"
                    );
                }
            }
        }
        a.last_poll = Some(Utc::now());
    }
}

/// Poll both sources in parallel. Used by tests.
#[cfg(test)]
pub async fn poll_once(
    app: &Arc<Mutex<App>>,
    pipeline_client: &dyn PipelineClient,
    actions_client: &dyn ActionsClient,
) {
    tokio::join!(
        poll_pipelines_tick(app, pipeline_client, "test-profile"),
        poll_actions_tick(app, actions_client),
    );
}

async fn poll_pipelines(client: &dyn PipelineClient) -> Result<Vec<PipelineState>> {
    let names = client.list_pipeline_names().await?;
    let futs: Vec<_> = names.iter().map(|n| client.get_pipeline_state(n)).collect();
    futures::future::join_all(futs).await.into_iter().collect()
}

fn reconcile_bars(bars: &mut Vec<Bar>, updates: Vec<(String, BuildStatus)>, source: BarSource) {
    let seen: HashSet<&str> = updates.iter().map(|(n, _)| n.as_str()).collect();
    for bar in bars.iter_mut() {
        if !seen.contains(bar.name.as_str()) {
            bar.gone = true;
        }
    }
    for (name, status) in updates {
        if let Some(bar) = bars.iter_mut().find(|b| b.name == name) {
            bar.gone = false;
            bar.set_status(status);
        } else {
            let mut bar = Bar::new(name, source.clone());
            bar.set_status(status);
            bars.push(bar);
        }
    }
}

fn update_pipeline_bars(app: &mut App, states: Vec<PipelineState>) {
    let updates: Vec<_> = states.into_iter().map(|s| (s.name, s.status)).collect();
    reconcile_bars(&mut app.bars_pipelines, updates, BarSource::CodePipeline);
}

/// Phase 1: create/update workflow groups from summaries (no jobs yet).
fn update_workflow_summaries(app: &mut App, summaries: &[WorkflowRunSummary]) {
    let seen: HashSet<&str> = summaries.iter().map(|s| s.workflow_name.as_str()).collect();

    for group in &mut app.workflow_groups {
        if !seen.contains(group.name.as_str()) {
            group.gone = true;
        }
    }

    for summary in summaries {
        if let Some(g) = app
            .workflow_groups
            .iter_mut()
            .find(|g| g.name == summary.workflow_name)
        {
            g.gone = false;
            g.summary_status = summary.status;
        } else {
            app.workflow_groups.push(WorkflowGroup {
                name: summary.workflow_name.clone(),
                jobs: Vec::new(),
                gone: false,
                summary_status: summary.status,
            });
        }
    }
}

/// Phase 2: fill in jobs for a specific workflow group.
fn update_workflow_jobs(app: &mut App, workflow_name: &str, jobs: Vec<JobInfo>) {
    let group = match app
        .workflow_groups
        .iter_mut()
        .find(|g| g.name == workflow_name)
    {
        Some(g) => g,
        None => return,
    };

    let updates: Vec<_> = jobs.into_iter().map(|j| (j.name, j.status)).collect();
    reconcile_bars(&mut group.jobs, updates, BarSource::GitHubAction);
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
                    status: p.status,
                })
                .context("not found")
        }
    }

    struct MockActionsClient {
        runs: Vec<WorkflowRunInfo>,
    }

    #[async_trait]
    impl ActionsClient for MockActionsClient {
        async fn list_latest_runs(&self) -> Result<Vec<WorkflowRunSummary>> {
            Ok(self
                .runs
                .iter()
                .map(|r| WorkflowRunSummary {
                    workflow_name: r.workflow_name.clone(),
                    run_id: r.run_id,
                    status: r.status,
                })
                .collect())
        }

        async fn fetch_run_jobs(&self, run_id: u64) -> Result<Vec<JobInfo>> {
            Ok(self
                .runs
                .iter()
                .find(|r| r.run_id == run_id)
                .map(|r| {
                    r.jobs
                        .iter()
                        .map(|j| JobInfo {
                            name: j.name.clone(),
                            status: j.status,
                        })
                        .collect()
                })
                .unwrap_or_default())
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

        poll_once(&app, &pipes, &actions).await;

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
        poll_once(&app, &pipes, &actions).await;

        let pipes = MockPipelineClient { pipelines: vec![] };
        poll_once(&app, &pipes, &actions).await;

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
        poll_once(&app, &pipes, &actions).await;
        poll_once(&app, &pipes, &actions).await;

        let app = app.lock().unwrap();
        assert_eq!(app.bars_pipelines.len(), 1);
        // fill stays 0: set_status doesn't advance fill, tick() does (UI responsibility)
        assert_eq!(app.bars_pipelines[0].fill, 0);
        assert_eq!(app.bars_pipelines[0].status, BuildStatus::Running);
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
        poll_once(&app, &pipes, &actions).await;

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
        poll_once(&app, &pipes, &actions).await;

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
        poll_once(&app, &pipes, &actions).await;

        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions).await;

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
        poll_once(&app, &pipes, &actions).await;

        let app = app.lock().unwrap();
        assert!(!app.loading_pipelines);
        assert!(!app.loading_actions);
    }

    struct ExpiredTokenClient;

    #[async_trait]
    impl PipelineClient for ExpiredTokenClient {
        async fn list_pipeline_names(&self) -> Result<Vec<String>> {
            anyhow::bail!("ExpiredToken: the security token is expired")
        }
        async fn get_pipeline_state(&self, _name: &str) -> Result<PipelineState> {
            anyhow::bail!("ExpiredToken")
        }
    }

    #[tokio::test]
    async fn poll_expired_token_shows_sso_login_hint() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = ExpiredTokenClient;
        poll_pipelines_tick(&app, &pipes, "my-profile").await;

        let app = app.lock().unwrap();
        assert_eq!(app.warnings.len(), 1);
        assert!(app.warnings[0].contains("aws sso login"));
        assert!(app.warnings[0].contains("my-profile"));
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
        poll_once(&app, &pipes, &actions).await;

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
        poll_once(&app, &pipes, &actions).await;

        let app = app.lock().unwrap();
        let group = &app.workflow_groups[0];
        assert_eq!(group.jobs.len(), 2);
        let build = group.jobs.iter().find(|j| j.name == "build").unwrap();
        assert!(!build.gone);
        let test = group.jobs.iter().find(|j| j.name == "test").unwrap();
        assert!(test.gone);
    }
}
