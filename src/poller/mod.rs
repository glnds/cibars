pub mod aws;
pub mod github;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use crate::app::App;
use crate::model::{Bar, BuildStatus, PipelineGroup, WorkflowGroup};

/// How long to back off when GitHub rate limit is hit.
const RATE_LIMIT_BACKOFF_SECS: u64 = 60;

/// Pipeline state from AWS API, including per-stage action breakdown.
pub struct PipelineState {
    pub name: String,
    pub status: BuildStatus,
    pub stages: Vec<StageState>,
}

pub struct StageState {
    pub name: String,
    pub actions: Vec<ActionState>,
}

pub struct ActionState {
    pub status: BuildStatus,
}

/// Pipeline definition (source config), from GetPipeline API.
pub struct PipelineDefinition {
    pub name: String,
    pub source_s3: Option<S3Source>,
}

pub struct S3Source {
    pub bucket: String,
    pub object_key: String,
}

/// A single job within a workflow run
pub struct JobInfo {
    pub name: String,
    pub status: BuildStatus,
}

/// Parsed GH workflow file with S3 upload targets.
pub struct WorkflowFile {
    pub name: String,
    pub s3_uploads: Vec<S3Upload>,
}

pub struct S3Upload {
    pub bucket: String,
    pub key: String,
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
    /// Fetch pipeline definition (source S3 config) for linkage discovery.
    async fn get_pipeline_definition(&self, name: &str) -> Result<PipelineDefinition>;
}

#[async_trait]
pub trait ActionsClient: Send + Sync {
    /// Fast: single API call, returns workflow-level status only.
    async fn list_latest_runs(&self) -> Result<Vec<WorkflowRunSummary>>;
    /// Fetch jobs for a specific run.
    async fn fetch_run_jobs(&self, run_id: u64) -> Result<Vec<JobInfo>>;
    /// Fetch workflow YAML files and extract S3 upload targets.
    async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>>;
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
            update_pipeline_groups(&mut a, states);
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let mut a = app.lock().expect("app mutex poisoned");
            if msg.contains("ExpiredToken") || msg.contains("UnauthorizedException") {
                a.push_warning(format!(
                    "AWS: SSO session expired \u{2014} run `aws sso login --profile {profile}` then press b"
                ));
            } else {
                a.push_warning(format!("AWS: {msg}"));
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
        a.push_warning("GitHub: rate-limited, backing off".to_string());
        a.loading_actions = false;
        return;
    }

    // Phase 1: fetch workflow summaries (single API call)
    let summaries = match client.list_latest_runs().await {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("{e:#}");
            let mut a = app.lock().expect("app mutex poisoned");
            let msg_lower = msg.to_lowercase();
            if msg_lower.contains("rate limit")
                || msg_lower.contains("403")
                || msg_lower.contains("429")
                || msg_lower.contains("abuse detection")
            {
                a.rate_limited_until =
                    Some(Instant::now() + Duration::from_secs(RATE_LIMIT_BACKOFF_SECS));
                a.push_warning(format!(
                    "GitHub: rate limited, backing off {RATE_LIMIT_BACKOFF_SECS}s"
                ));
            } else {
                a.push_warning(format!("GitHub: {msg}"));
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

fn reconcile_bars(bars: &mut Vec<Bar>, updates: Vec<(String, BuildStatus)>) {
    let seen: HashSet<&str> = updates.iter().map(|(n, _)| n.as_str()).collect();
    for bar in bars.iter_mut() {
        if !seen.contains(bar.name.as_str()) {
            bar.gone = true;
        }
    }
    for (name, status) in &updates {
        if let Some(bar) = bars.iter_mut().find(|b| b.name == *name) {
            bar.gone = false;
            bar.set_status(*status);
        } else {
            let mut bar = Bar::new(name.clone());
            bar.set_status(*status);
            bars.push(bar);
        }
    }
    // Reorder bars to match API response order; gone bars go to end.
    let order: std::collections::HashMap<&str, usize> = updates
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (n.as_str(), i))
        .collect();
    bars.sort_by_key(|b| order.get(b.name.as_str()).copied().unwrap_or(usize::MAX));
}

/// Compute stage-level status from the last action in a stage.
fn stage_status_from_actions(actions: &[ActionState]) -> BuildStatus {
    actions
        .last()
        .map(|a| a.status)
        .unwrap_or(BuildStatus::Idle)
}

fn update_pipeline_groups(app: &mut App, states: Vec<PipelineState>) {
    let seen: HashSet<&str> = states.iter().map(|s| s.name.as_str()).collect();

    for group in &mut app.pipeline_groups {
        if !seen.contains(group.name.as_str()) {
            group.gone = true;
        }
    }

    for state in states {
        let stage_updates: Vec<(String, BuildStatus)> = state
            .stages
            .iter()
            .map(|s| (s.name.clone(), stage_status_from_actions(&s.actions)))
            .collect();

        if let Some(g) = app
            .pipeline_groups
            .iter_mut()
            .find(|g| g.name == state.name)
        {
            g.gone = false;
            g.summary_status = state.status;
            reconcile_bars(&mut g.stages, stage_updates);
        } else {
            let mut group = PipelineGroup {
                name: state.name,
                stages: Vec::new(),
                gone: false,
                summary_status: state.status,
            };
            reconcile_bars(&mut group.stages, stage_updates);
            app.pipeline_groups.push(group);
        }
    }
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
            g.run_id = Some(summary.run_id);
        } else {
            app.workflow_groups.push(WorkflowGroup {
                name: summary.workflow_name.clone(),
                jobs: Vec::new(),
                gone: false,
                summary_status: summary.status,
                run_id: Some(summary.run_id),
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
    reconcile_bars(&mut group.jobs, updates);
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
                    stages: p
                        .stages
                        .iter()
                        .map(|s| StageState {
                            name: s.name.clone(),
                            actions: s
                                .actions
                                .iter()
                                .map(|a| ActionState { status: a.status })
                                .collect(),
                        })
                        .collect(),
                })
                .context("not found")
        }
        async fn get_pipeline_definition(&self, name: &str) -> Result<PipelineDefinition> {
            Ok(PipelineDefinition {
                name: name.to_string(),
                source_s3: None,
            })
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

        async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
            Ok(Vec::new())
        }
    }

    fn mock_pipeline(name: &str, status: BuildStatus, stages: Vec<StageState>) -> PipelineState {
        PipelineState {
            name: name.to_string(),
            status,
            stages,
        }
    }

    fn mock_stage(name: &str, actions: Vec<(&str, BuildStatus)>) -> StageState {
        StageState {
            name: name.to_string(),
            actions: actions
                .into_iter()
                .map(|(_n, s)| ActionState { status: s })
                .collect(),
        }
    }

    #[tokio::test]
    async fn poll_creates_pipeline_groups() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "deploy",
                BuildStatus::Running,
                vec![mock_stage(
                    "Build",
                    vec![("compile", BuildStatus::Succeeded)],
                )],
            )],
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
        assert_eq!(app.pipeline_groups.len(), 1);
        assert_eq!(app.pipeline_groups[0].name, "deploy");
        assert_eq!(app.pipeline_groups[0].summary_status, BuildStatus::Running);
        assert_eq!(app.pipeline_groups[0].stages.len(), 1);
        assert_eq!(app.pipeline_groups[0].stages[0].name, "Build");
        assert_eq!(
            app.pipeline_groups[0].stages[0].status,
            BuildStatus::Succeeded
        );
        assert_eq!(app.workflow_groups.len(), 1);
        assert_eq!(app.workflow_groups[0].name, "ci");
        assert!(app.last_poll.is_some());
        assert!(!app.loading_pipelines);
        assert!(!app.loading_actions);
    }

    #[tokio::test]
    async fn poll_marks_gone_pipelines() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Running, vec![])],
        };
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions).await;

        let pipes = MockPipelineClient { pipelines: vec![] };
        poll_once(&app, &pipes, &actions).await;

        let app = app.lock().unwrap();
        assert_eq!(app.pipeline_groups.len(), 1);
        assert!(app.pipeline_groups[0].gone);
    }

    #[tokio::test]
    async fn poll_updates_existing_pipeline_groups() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "deploy",
                BuildStatus::Running,
                vec![mock_stage("Build", vec![("compile", BuildStatus::Running)])],
            )],
        };
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions).await;
        poll_once(&app, &pipes, &actions).await;

        let app = app.lock().unwrap();
        assert_eq!(app.pipeline_groups.len(), 1);
        assert_eq!(
            app.pipeline_groups[0].stages[0].status,
            BuildStatus::Running
        );
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
        async fn get_pipeline_definition(&self, _name: &str) -> Result<PipelineDefinition> {
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
    async fn poll_creates_pipeline_with_multi_stage_status_from_last_action() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "my-pipe",
                BuildStatus::Running,
                vec![
                    mock_stage("Source", vec![("checkout", BuildStatus::Succeeded)]),
                    mock_stage(
                        "Build",
                        vec![
                            ("compile", BuildStatus::Succeeded),
                            ("test", BuildStatus::Running),
                        ],
                    ),
                ],
            )],
        };
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions).await;

        let app = app.lock().unwrap();
        let group = &app.pipeline_groups[0];
        assert_eq!(group.stages.len(), 2);
        assert_eq!(group.stages[0].name, "Source");
        assert_eq!(group.stages[0].status, BuildStatus::Succeeded);
        assert_eq!(group.stages[1].name, "Build");
        // Last action ("test") determines stage status
        assert_eq!(group.stages[1].status, BuildStatus::Running);
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
        async fn get_pipeline_definition(&self, _name: &str) -> Result<PipelineDefinition> {
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

    struct RateLimitActionsClient {
        error_msg: String,
    }

    #[async_trait]
    impl ActionsClient for RateLimitActionsClient {
        async fn list_latest_runs(&self) -> Result<Vec<WorkflowRunSummary>> {
            anyhow::bail!("{}", self.error_msg)
        }
        async fn fetch_run_jobs(&self, _run_id: u64) -> Result<Vec<JobInfo>> {
            Ok(vec![])
        }
        async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn rate_limit_detected_from_string_match() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = RateLimitActionsClient {
            error_msg: "API rate limit exceeded".to_string(),
        };
        poll_actions_tick(&app, &client).await;
        let a = app.lock().unwrap();
        assert!(a.rate_limited_until.is_some());
    }

    #[tokio::test]
    async fn rate_limit_detected_from_403() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = RateLimitActionsClient {
            error_msg: "HTTP 403 Forbidden".to_string(),
        };
        poll_actions_tick(&app, &client).await;
        let a = app.lock().unwrap();
        assert!(a.rate_limited_until.is_some());
    }

    #[tokio::test]
    async fn rate_limit_detected_from_429() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = RateLimitActionsClient {
            error_msg: "HTTP 429 Too Many Requests".to_string(),
        };
        poll_actions_tick(&app, &client).await;
        let a = app.lock().unwrap();
        assert!(a.rate_limited_until.is_some());
    }

    #[tokio::test]
    async fn rate_limit_detected_from_abuse_detection() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = RateLimitActionsClient {
            error_msg: "abuse detection mechanism triggered".to_string(),
        };
        poll_actions_tick(&app, &client).await;
        let a = app.lock().unwrap();
        assert!(a.rate_limited_until.is_some());
    }

    #[tokio::test]
    async fn non_rate_limit_error_not_flagged() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = RateLimitActionsClient {
            error_msg: "network timeout".to_string(),
        };
        poll_actions_tick(&app, &client).await;
        let a = app.lock().unwrap();
        assert!(a.rate_limited_until.is_none());
    }

    #[tokio::test]
    async fn reconcile_bars_reorders_to_match_api_order() {
        let app = Arc::new(Mutex::new(App::new()));
        // First poll: stages arrive as Source, Build, Deploy
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "my-pipe",
                BuildStatus::Running,
                vec![
                    mock_stage("Source", vec![("checkout", BuildStatus::Succeeded)]),
                    mock_stage("Build", vec![("compile", BuildStatus::Running)]),
                    mock_stage("Deploy", vec![("deploy", BuildStatus::Idle)]),
                ],
            )],
        };
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions).await;

        {
            let a = app.lock().unwrap();
            let stages: Vec<&str> = a.pipeline_groups[0]
                .stages
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            assert_eq!(stages, vec!["Source", "Build", "Deploy"]);
        }

        // Second poll: API returns stages in different order (e.g. Deploy, Source, Build)
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "my-pipe",
                BuildStatus::Running,
                vec![
                    mock_stage("Deploy", vec![("deploy", BuildStatus::Running)]),
                    mock_stage("Source", vec![("checkout", BuildStatus::Succeeded)]),
                    mock_stage("Build", vec![("compile", BuildStatus::Succeeded)]),
                ],
            )],
        };
        poll_once(&app, &pipes, &actions).await;

        let a = app.lock().unwrap();
        let stages: Vec<&str> = a.pipeline_groups[0]
            .stages
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        // Stages should follow the API response order
        assert_eq!(stages, vec!["Deploy", "Source", "Build"]);
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
