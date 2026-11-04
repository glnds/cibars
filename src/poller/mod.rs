pub mod aws;
pub mod github;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use crate::app::{App, SourceHealth};
use crate::config::Config;
use crate::model::{Bar, BuildStatus, PipelineGroup, WorkflowCategory, WorkflowGroup};

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
    pub last_status_change: Option<chrono::DateTime<chrono::Utc>>,
}

/// Pipeline definition (source config), from GetPipeline API.
#[derive(Clone)]
pub struct PipelineDefinition {
    pub name: String,
    pub source_s3: Option<S3Source>,
}

#[derive(Clone)]
pub struct S3Source {
    pub bucket: String,
    pub object_key: String,
}

/// A single job within a workflow run
pub struct JobInfo {
    pub name: String,
    pub status: BuildStatus,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Parsed GH workflow file with S3 upload targets.
#[derive(Clone)]
pub struct WorkflowFile {
    pub name: String,
    pub s3_uploads: Vec<S3Upload>,
    /// Per-job S3 info extracted from YAML AST (empty for non-YAML sources).
    pub jobs: Vec<JobS3Info>,
}

#[derive(Clone, Debug)]
pub struct S3Upload {
    pub bucket: String,
    pub key: String,
}

/// Per-job S3 upload info extracted from workflow YAML AST.
#[derive(Clone, Debug)]
pub struct JobS3Info {
    /// YAML job key (e.g., "build-backend")
    pub job_id: String,
    /// Display name from `name:` field (e.g., "Build Backend")
    pub job_name: String,
    /// S3 uploads found in this job's `run:` steps
    pub s3_uploads: Vec<S3Upload>,
    /// Job IDs this job depends on (from `needs:`)
    pub needs: Vec<String>,
}

/// Workflow summary from the runs API (no jobs yet).
pub struct WorkflowRunSummary {
    pub workflow_name: String,
    pub run_id: u64,
    pub status: BuildStatus,
}

/// Result of a single `list_latest_runs` call. `summaries` are the visible
/// workflow runs (after bot-actor + head_branch filters); `pr_numbers` are
/// the PR numbers harvested from pull_request[_target] runs *before* those
/// filters, so the linger-after-PR scheduler hook can watch them even when
/// the runs themselves are hidden from the UI.
#[derive(Default)]
pub struct RunsPage {
    pub summaries: Vec<WorkflowRunSummary>,
    pub pr_numbers: std::collections::HashSet<u64>,
}

#[derive(Clone, Debug)]
pub struct WorkflowInventoryEntry {
    pub id: u64,
    pub name: String,
}

pub enum ActionsUpdate {
    Inventory(Vec<WorkflowInventoryEntry>),
    Runs(RunsPage),
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
    /// Fast: single API call, returns workflow-level summaries + harvested
    /// PR numbers (from pull_request[_target] events, pre-filter).
    async fn list_latest_runs(&self) -> Result<RunsPage>;
    async fn stream_latest_runs(
        &self,
        updates: tokio::sync::mpsc::UnboundedSender<ActionsUpdate>,
    ) -> Result<()> {
        updates
            .send(ActionsUpdate::Runs(self.list_latest_runs().await?))
            .map_err(|_| anyhow::anyhow!("workflow update receiver closed"))
    }
    /// Fetch jobs for a specific run.
    async fn fetch_run_jobs(&self, run_id: u64) -> Result<Vec<JobInfo>>;
    /// Fetch workflow YAML files and extract S3 upload targets.
    async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>>;
    /// Fetch the state of a single pull request. Used by the watched-PR
    /// poller to detect Open → Merged transitions and auto-boost.
    async fn fetch_pr_state(&self, pr_number: u64) -> Result<crate::model::WatchedPrState>;
}

#[async_trait]
pub trait AuthHealthClient: Send + Sync {
    async fn get_caller_identity(&self) -> Result<()>;
}

fn aws_sso_expired_msg(profile: &str) -> String {
    format!(
        "AWS: SSO session expired \u{2014} run `aws sso login --profile {profile}` then press b"
    )
}

/// Detect AWS auth/credential errors by string matching.
fn is_auth_error(msg: &str) -> bool {
    msg.contains("ExpiredToken")
        || msg.contains("UnauthorizedException")
        || msg.contains("InvalidIdentityToken")
        || msg.contains("ExpiredTokenException")
        || (msg.contains("AccessDenied") && (msg.contains("SSO") || msg.contains("STS")))
        || msg.contains("error occurred while loading credentials")
}

/// Poll AWS pipelines and update app state. Clears only AWS-specific warnings.
pub async fn poll_pipelines_tick(
    app: &Arc<Mutex<App>>,
    client: &dyn PipelineClient,
    profile: &str,
    force: bool,
) {
    {
        let a = app.lock().expect("app mutex poisoned");
        if matches!(a.aws_health, SourceHealth::AuthFailed { .. }) && !force {
            return;
        }
    }

    {
        let mut a = app.lock().expect("app mutex poisoned");
        a.clear_warnings_by_prefix("AWS:");
    }

    match poll_pipelines(client).await {
        Ok(poll) => {
            tracing::debug!(count = poll.states.len(), "polled pipelines");
            let mut a = app.lock().expect("app mutex poisoned");
            if poll.errors.is_empty() || !poll.states.is_empty() {
                a.aws_health = SourceHealth::Healthy;
            }
            update_pipeline_groups(&mut a, poll.states, &poll.names);
            for error in poll.errors {
                let msg = format!("{error:#}");
                if is_auth_error(&msg) {
                    a.aws_health = SourceHealth::AuthFailed { since: Utc::now() };
                    a.push_warning(aws_sso_expired_msg(profile));
                }
                a.push_warning(format!("AWS: {msg}"));
            }
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let mut a = app.lock().expect("app mutex poisoned");
            if is_auth_error(&msg) {
                a.aws_health = SourceHealth::AuthFailed { since: Utc::now() };
                a.push_warning(aws_sso_expired_msg(profile));
            } else {
                a.push_warning(format!("AWS: {msg}"));
            }
        }
    }

    let mut a = app.lock().expect("app mutex poisoned");
    a.loading_pipelines = false;
    a.last_poll = Some(Utc::now());
}

/// Lightweight AWS credential health check via STS GetCallerIdentity.
/// Used during LongIdle to detect expired SSO sessions without a full pipeline poll.
pub async fn check_aws_auth(app: &Arc<Mutex<App>>, client: &dyn AuthHealthClient, profile: &str) {
    match client.get_caller_identity().await {
        Ok(()) => {
            let mut a = app.lock().expect("app mutex poisoned");
            if matches!(a.aws_health, SourceHealth::AuthFailed { .. }) {
                a.aws_health = SourceHealth::Healthy;
                a.clear_warnings_by_prefix("AWS:");
            }
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if is_auth_error(&msg) {
                let mut a = app.lock().expect("app mutex poisoned");
                a.aws_health = SourceHealth::AuthFailed { since: Utc::now() };
                a.push_warning(aws_sso_expired_msg(profile));
            }
        }
    }
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
        a.clear_warnings_by_prefix("GitHub:");
    }

    if skip_github {
        let mut a = app.lock().expect("app mutex poisoned");
        a.push_warning("GitHub: rate-limited, backing off".to_string());
        a.loading_actions = false;
        return;
    }

    use futures::{stream::FuturesUnordered, StreamExt};
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let discovery = client.stream_latest_runs(sender);
    tokio::pin!(discovery);
    let mut discovery_done = false;
    let mut updates_done = false;
    let mut requested = HashSet::new();
    let mut jobs = FuturesUnordered::new();
    while !discovery_done || !updates_done || !jobs.is_empty() {
        tokio::select! {
            result = &mut discovery, if !discovery_done => {
                discovery_done = true;
                if let Err(error) = result {
                    record_actions_error(app, error);
                }
            }
            update = receiver.recv(), if !updates_done => {
                let Some(update) = update else { updates_done = true; continue; };
                let mut a = app.lock().expect("app mutex poisoned");
                match update {
                    ActionsUpdate::Inventory(entries) => {
                        a.github_inventory_names = Some(entries.iter().map(|e| e.name.clone()).collect());
                        update_workflow_inventory(&mut a, &entries);
                    }
                    ActionsUpdate::Runs(mut page) => {
                        if let Some(names) = &a.github_inventory_names {
                            page.summaries.retain(|s| names.contains(&s.workflow_name));
                        }
                        update_workflow_summaries(&mut a, &page.summaries);
                        let prs: Vec<_> = page.pr_numbers.into_iter().collect();
                        a.add_or_update_watched_prs(&prs, Instant::now());
                        for summary in page.summaries {
                            if requested.insert((summary.workflow_name.clone(), summary.run_id)) {
                                jobs.push(async move {
                                    let result = client.fetch_run_jobs(summary.run_id).await;
                                    (summary, result)
                                });
                            }
                        }
                    }
                }
                a.loading_actions = false;
                a.last_poll = Some(Utc::now());
            }
            Some((summary, result)) = jobs.next(), if !jobs.is_empty() => {
                match result {
                    Ok(jobs) => {
                        let mut a = app.lock().expect("app mutex poisoned");
                        if a.workflow_groups.iter().any(|g| !g.gone && g.name == summary.workflow_name && g.run_id == Some(summary.run_id)) {
                            update_workflow_jobs(&mut a, &summary.workflow_name, jobs);
                        }
                    }
                    Err(error) => record_actions_error(app, error.context(format!("jobs for {} run {}", summary.workflow_name, summary.run_id))),
                }
            }
        }
    }
    let mut a = app.lock().expect("app mutex poisoned");
    a.loading_actions = false;
    a.last_poll = Some(Utc::now());
}

#[derive(Debug)]
pub(crate) struct ActionsErrors(pub Vec<anyhow::Error>);

impl std::fmt::Display for ActionsErrors {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, error) in self.0.iter().enumerate() {
            if index > 0 {
                write!(formatter, "; ")?;
            }
            write!(formatter, "{error:#}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ActionsErrors {}

fn actions_rate_limited(error: &anyhow::Error) -> bool {
    if let Some(errors) = error.downcast_ref::<ActionsErrors>() {
        return errors.0.iter().any(actions_rate_limited);
    }
    for cause in error.chain() {
        if let Some(octocrab::Error::GitHub { source, .. }) =
            cause.downcast_ref::<octocrab::Error>()
        {
            return matches!(source.status_code.as_u16(), 403 | 429);
        }
    }
    // Legacy adapters expose plain errors. Inspect their leaf message, never run/job context.
    let lower = error.root_cause().to_string().to_lowercase();
    lower.contains("rate limit")
        || lower.contains("abuse detection")
        || ["http 403 ", "http 429 ", "403 ", "429 "]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
}

fn record_actions_error(app: &Arc<Mutex<App>>, error: anyhow::Error) {
    let mut a = app.lock().expect("app mutex poisoned");
    if actions_rate_limited(&error) {
        a.rate_limited_until = Some(Instant::now() + Duration::from_secs(RATE_LIMIT_BACKOFF_SECS));
        a.push_warning(format!(
            "GitHub: rate limited, backing off {RATE_LIMIT_BACKOFF_SECS}s"
        ));
    } else {
        a.push_warning(format!("GitHub: {error:#}"));
    }
}

/// Poll the state of every watched PR whose last check is older than
/// `min_interval` and whose state is not terminal (`Merged` /
/// `ClosedUnmerged`). Updates `App.watched_prs` in place. Returns `true`
/// iff a watched PR transitioned `Open → Merged` during this tick — the
/// orchestrator uses that signal to auto-boost the scheduler so the
/// downstream master-push run and CodePipeline execution get picked up.
///
/// On fetch errors, state and `last_checked` are left untouched so the
/// next tick retries.
/// One poll cycle over the enabled sources, concurrently.
/// `aws` is (client, profile). Returns true when a watched PR merged.
pub async fn poll_sources_tick(
    app: &Arc<Mutex<App>>,
    gh: Option<&dyn ActionsClient>,
    aws: Option<(&dyn PipelineClient, &str)>,
    pr_min_interval: Duration,
) -> bool {
    let actions = async {
        if let Some(gh) = gh {
            poll_actions_tick(app, gh).await;
        }
    };
    let pipelines = async {
        if let Some((client, profile)) = aws {
            poll_pipelines_tick(app, client, profile, false).await;
        }
    };
    let prs = async {
        match gh {
            Some(gh) => poll_pr_states_tick(app, gh, pr_min_interval).await,
            None => false,
        }
    };
    let (_, _, merge_detected) = tokio::join!(actions, pipelines, prs);
    merge_detected
}

pub async fn poll_pr_states_tick(
    app: &Arc<Mutex<App>>,
    client: &dyn ActionsClient,
    min_interval: Duration,
) -> bool {
    use crate::model::WatchedPrState;
    let now = Instant::now();

    let pollable: Vec<u64> = {
        let a = app.lock().expect("app mutex poisoned");
        a.watched_prs
            .values()
            .filter(|pr| {
                matches!(pr.state, WatchedPrState::Open | WatchedPrState::Unknown)
                    && match pr.last_checked {
                        Some(t) => now.saturating_duration_since(t) >= min_interval,
                        None => true,
                    }
            })
            .map(|pr| pr.number)
            .collect()
    };
    if pollable.is_empty() {
        return false;
    }

    let futs: Vec<_> = pollable.iter().map(|n| client.fetch_pr_state(*n)).collect();
    let results = futures::future::join_all(futs).await;

    let mut merge_detected = false;
    let mut a = app.lock().expect("app mutex poisoned");
    for (pr_number, result) in pollable.iter().zip(results) {
        let Some(entry) = a.watched_prs.get_mut(pr_number) else {
            continue;
        };
        match result {
            Ok(new_state) => {
                let prior = entry.state;
                entry.state = new_state;
                entry.last_checked = Some(now);
                if prior == WatchedPrState::Open && new_state == WatchedPrState::Merged {
                    merge_detected = true;
                }
            }
            Err(e) => {
                tracing::warn!(pr = %pr_number, error = %e, "failed to fetch PR state");
            }
        }
    }
    merge_detected
}

/// Poll both sources in parallel. Used by tests.
#[cfg(test)]
pub async fn poll_once(
    app: &Arc<Mutex<App>>,
    pipeline_client: &dyn PipelineClient,
    actions_client: &dyn ActionsClient,
) {
    tokio::join!(
        poll_pipelines_tick(app, pipeline_client, "test-profile", false),
        poll_actions_tick(app, actions_client),
    );
}

struct PipelinePoll {
    names: Vec<String>,
    states: Vec<PipelineState>,
    errors: Vec<anyhow::Error>,
}

async fn poll_pipelines(client: &dyn PipelineClient) -> Result<PipelinePoll> {
    let names = client.list_pipeline_names().await?;
    let futs: Vec<_> = names.iter().map(|n| client.get_pipeline_state(n)).collect();
    let results = futures::future::join_all(futs).await;
    let mut states = Vec::new();
    let mut errors = Vec::new();
    for (name, result) in names.iter().zip(results) {
        match result {
            Ok(state) => states.push(state),
            Err(error) => errors.push(error.context(format!("pipeline {name}"))),
        }
    }
    Ok(PipelinePoll {
        names,
        states,
        errors,
    })
}

fn reconcile_bars(
    bars: &mut Vec<Bar>,
    updates: Vec<(String, BuildStatus, Option<chrono::DateTime<chrono::Utc>>)>,
) {
    let seen: HashSet<&str> = updates.iter().map(|(n, _, _)| n.as_str()).collect();
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
            bar.set_status(*status);
            bar.last_finished = *finished;
            bars.push(bar);
        }
    }
    // Reorder bars to match API response order; gone bars go to end.
    let order: std::collections::HashMap<&str, usize> = updates
        .iter()
        .enumerate()
        .map(|(i, (n, _, _))| (n.as_str(), i))
        .collect();
    bars.sort_by_key(|b| order.get(b.name.as_str()).copied().unwrap_or(usize::MAX));
}

/// Aggregate all action statuses and use the latest completion timestamp.
fn stage_status_and_timestamp(
    actions: &[ActionState],
) -> (BuildStatus, Option<chrono::DateTime<chrono::Utc>>) {
    // Assume active actions take priority, matching pipeline-level aggregation.
    let status = actions
        .iter()
        .map(|a| a.status)
        .max_by_key(|status| match status {
            BuildStatus::Running => 3,
            BuildStatus::Failed => 2,
            BuildStatus::Succeeded => 1,
            BuildStatus::Idle => 0,
        })
        .unwrap_or(BuildStatus::Idle);
    let timestamp = match status {
        BuildStatus::Succeeded | BuildStatus::Failed => {
            actions.iter().filter_map(|a| a.last_status_change).max()
        }
        _ => None,
    };
    (status, timestamp)
}

fn update_pipeline_groups(app: &mut App, states: Vec<PipelineState>, listed_names: &[String]) {
    // A listed pipeline still exists even if its state request failed.
    let seen: HashSet<&str> = listed_names.iter().map(String::as_str).collect();

    for group in &mut app.pipeline_groups {
        group.gone = !seen.contains(group.name.as_str());
    }

    for state in states {
        let stage_updates: Vec<_> = state
            .stages
            .iter()
            .map(|s| {
                let (status, ts) = stage_status_and_timestamp(&s.actions);
                (s.name.clone(), status, ts)
            })
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
                pending_link: false,
            };
            reconcile_bars(&mut group.stages, stage_updates);
            app.pipeline_groups.push(group);
        }
    }
}

/// Phase 1: create/update workflow groups from summaries (no jobs yet).
fn update_workflow_summaries(app: &mut App, summaries: &[WorkflowRunSummary]) {
    for summary in summaries {
        if let Some(g) = app
            .workflow_groups
            .iter_mut()
            .find(|g| g.name == summary.workflow_name)
        {
            // GitHub run IDs increase; delayed summaries cannot revive older executions.
            if g.run_id.is_some_and(|id| id > summary.run_id) {
                continue;
            }
            g.gone = false;
            if g.run_id != Some(summary.run_id) {
                for job in &mut g.jobs {
                    *job = Bar::new(job.name.clone());
                }
            }
            g.summary_status = summary.status;
            g.run_id = Some(summary.run_id);
        } else {
            app.workflow_groups.push(WorkflowGroup {
                name: summary.workflow_name.clone(),
                jobs: Vec::new(),
                gone: false,
                summary_status: summary.status,
                run_id: Some(summary.run_id),
                category: WorkflowCategory::default(),
                linked_pipeline: None,
            });
        }
    }
}

fn update_workflow_inventory(app: &mut App, entries: &[WorkflowInventoryEntry]) {
    for group in &mut app.workflow_groups {
        group.gone = !entries.iter().any(|entry| entry.name == group.name);
    }
    for entry in entries {
        if !app
            .workflow_groups
            .iter()
            .any(|group| group.name == entry.name)
        {
            app.workflow_groups.push(WorkflowGroup {
                name: entry.name.clone(),
                jobs: Vec::new(),
                gone: false,
                summary_status: BuildStatus::Idle,
                run_id: None,
                category: WorkflowCategory::default(),
                linked_pipeline: None,
            });
        }
    }
}

/// Apply workflow category classification from config.
pub fn classify_workflows(app: &mut App, config: &Config) {
    for group in &mut app.workflow_groups {
        group.category = config.classify_workflow(&group.name);
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

    let updates: Vec<_> = jobs
        .into_iter()
        .map(|j| (j.name, j.status, j.completed_at))
        .collect();
    reconcile_bars(&mut group.jobs, updates);
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;

    #[test]
    fn progressive_summary_retains_sparse_workflows_and_resets_new_run_jobs() {
        let mut app = App::new();
        let summary = |name: &str, run_id| WorkflowRunSummary {
            workflow_name: name.into(),
            run_id,
            status: BuildStatus::Running,
        };
        update_workflow_summaries(&mut app, &[summary("CI", 1), summary("Nightly", 2)]);
        update_workflow_jobs(
            &mut app,
            "CI",
            vec![JobInfo {
                name: "build".into(),
                status: BuildStatus::Succeeded,
                completed_at: Some(Utc::now()),
            }],
        );
        update_workflow_summaries(&mut app, &[summary("CI", 3)]);
        assert!(
            !app.workflow_groups[1].gone,
            "run pages cannot delete sparse workflows"
        );
        let job = &app.workflow_groups[0].jobs[0];
        assert_eq!(
            job.status,
            BuildStatus::Idle,
            "new run must clear old success"
        );
        assert!(job.last_finished.is_none());
    }

    #[test]
    fn progressive_superseded_summary_cannot_replace_newer_run() {
        let mut app = App::new();
        for id in [20, 10] {
            update_workflow_summaries(
                &mut app,
                &[WorkflowRunSummary {
                    workflow_name: "CI".into(),
                    run_id: id,
                    status: BuildStatus::Running,
                }],
            );
        }
        assert_eq!(app.workflow_groups[0].run_id, Some(20));
    }

    struct GatedJobsClient;

    #[async_trait]
    impl ActionsClient for GatedJobsClient {
        async fn list_latest_runs(&self) -> Result<RunsPage> {
            Ok(RunsPage {
                summaries: [1, 2]
                    .into_iter()
                    .map(|id| WorkflowRunSummary {
                        workflow_name: format!("workflow-{id}"),
                        run_id: id,
                        status: BuildStatus::Running,
                    })
                    .collect(),
                ..Default::default()
            })
        }
        async fn fetch_run_jobs(&self, id: u64) -> Result<Vec<JobInfo>> {
            if id == 2 {
                std::future::pending::<()>().await;
            }
            Ok(vec![JobInfo {
                name: "fast".into(),
                status: BuildStatus::Succeeded,
                completed_at: None,
            }])
        }
        async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
            Ok(vec![])
        }
        async fn fetch_pr_state(&self, _: u64) -> Result<crate::model::WatchedPrState> {
            Ok(crate::model::WatchedPrState::Unknown)
        }
    }

    #[tokio::test]
    async fn progressive_fast_jobs_publish_while_other_jobs_pending() {
        let app = Arc::new(Mutex::new(App::new()));
        let tick = poll_actions_tick(&app, &GatedJobsClient);
        tokio::pin!(tick);
        tokio::select! {
            _ = &mut tick => panic!("stalled job should keep tick pending"),
            _ = tokio::time::sleep(Duration::from_millis(30)) => {}
        }
        assert_eq!(app.lock().unwrap().workflow_groups[0].jobs.len(), 1);
    }

    struct StreamingClient {
        fail: bool,
        hold: bool,
        inventory: Option<Vec<WorkflowInventoryEntry>>,
    }

    #[async_trait]
    impl ActionsClient for StreamingClient {
        async fn list_latest_runs(&self) -> Result<RunsPage> {
            unreachable!()
        }
        async fn stream_latest_runs(
            &self,
            sender: tokio::sync::mpsc::UnboundedSender<ActionsUpdate>,
        ) -> Result<()> {
            if let Some(entries) = &self.inventory {
                sender.send(ActionsUpdate::Inventory(entries.clone()))?;
            }
            sender.send(ActionsUpdate::Runs(RunsPage {
                summaries: vec![WorkflowRunSummary {
                    workflow_name: "CI".into(),
                    run_id: 1,
                    status: BuildStatus::Running,
                }],
                ..Default::default()
            }))?;
            if self.hold {
                std::future::pending::<()>().await;
            }
            if self.fail {
                anyhow::bail!("late discovery failure");
            }
            Ok(())
        }
        async fn fetch_run_jobs(&self, _: u64) -> Result<Vec<JobInfo>> {
            Ok(vec![JobInfo {
                name: "build".into(),
                status: BuildStatus::Succeeded,
                completed_at: None,
            }])
        }
        async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
            Ok(vec![])
        }
        async fn fetch_pr_state(&self, _: u64) -> Result<crate::model::WatchedPrState> {
            Ok(crate::model::WatchedPrState::Unknown)
        }
    }

    #[tokio::test]
    async fn progressive_jobs_publish_before_discovery_finishes() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = StreamingClient {
            fail: false,
            hold: true,
            inventory: Some(vec![
                WorkflowInventoryEntry {
                    id: 1,
                    name: "CI".into(),
                },
                WorkflowInventoryEntry {
                    id: 2,
                    name: "Never".into(),
                },
            ]),
        };
        let tick = poll_actions_tick(&app, &client);
        tokio::pin!(tick);
        tokio::select! { _ = &mut tick => panic!("discovery held"), _ = tokio::time::sleep(Duration::from_millis(30)) => {} }
        let a = app.lock().unwrap();
        assert_eq!(a.workflow_groups[0].jobs.len(), 1);
        assert_eq!(a.workflow_groups[1].summary_status, BuildStatus::Idle);
        assert_eq!(a.workflow_groups[1].run_id, None);
    }

    #[tokio::test]
    async fn progressive_late_error_preserves_buffered_runs_and_jobs() {
        let app = Arc::new(Mutex::new(App::new()));
        poll_actions_tick(
            &app,
            &StreamingClient {
                fail: true,
                hold: false,
                inventory: None,
            },
        )
        .await;
        let a = app.lock().unwrap();
        assert_eq!(a.workflow_groups[0].jobs.len(), 1);
        assert!(a
            .warnings
            .iter()
            .any(|w| w.contains("late discovery failure")));
    }

    #[tokio::test]
    async fn progressive_only_authoritative_empty_inventory_deletes() {
        let app = Arc::new(Mutex::new(App::new()));
        poll_actions_tick(
            &app,
            &StreamingClient {
                fail: false,
                hold: false,
                inventory: None,
            },
        )
        .await;
        poll_actions_tick(
            &app,
            &StreamingClient {
                fail: true,
                hold: false,
                inventory: None,
            },
        )
        .await;
        assert!(!app.lock().unwrap().workflow_groups[0].gone);
        poll_actions_tick(
            &app,
            &StreamingClient {
                fail: false,
                hold: false,
                inventory: Some(vec![]),
            },
        )
        .await;
        assert!(app.lock().unwrap().workflow_groups[0].gone);
        poll_actions_tick(
            &app,
            &StreamingClient {
                fail: false,
                hold: false,
                inventory: None,
            },
        )
        .await;
        assert!(
            app.lock().unwrap().workflow_groups[0].gone,
            "ordinary run polls cannot undo authoritative deletion"
        );
    }

    #[tokio::test]
    async fn progressive_numeric_run_identifiers_do_not_trigger_rate_limit() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = RateLimitActionsClient {
            error_msg: "GitHub request timed out after 15s: /repos/o/r/actions/runs/123429000/jobs"
                .into(),
        };
        poll_actions_tick(&app, &client).await;
        let a = app.lock().unwrap();
        assert!(
            a.rate_limited_until.is_none(),
            "run identifiers are not HTTP statuses"
        );
        assert!(a.warnings[0].contains("timed out"));
    }

    #[derive(Default)]
    struct SupersededJobsClient {
        old_started: tokio::sync::Notify,
        dropped: std::sync::atomic::AtomicBool,
        hold: bool,
    }

    #[async_trait]
    impl ActionsClient for SupersededJobsClient {
        async fn list_latest_runs(&self) -> Result<RunsPage> {
            unreachable!()
        }
        async fn stream_latest_runs(
            &self,
            sender: tokio::sync::mpsc::UnboundedSender<ActionsUpdate>,
        ) -> Result<()> {
            for id in [1, 2] {
                sender.send(ActionsUpdate::Runs(RunsPage {
                    summaries: vec![WorkflowRunSummary {
                        workflow_name: "CI".into(),
                        run_id: id,
                        status: BuildStatus::Running,
                    }],
                    ..Default::default()
                }))?;
                if id == 1 {
                    self.old_started.notified().await;
                }
            }
            Ok(())
        }
        async fn fetch_run_jobs(&self, id: u64) -> Result<Vec<JobInfo>> {
            struct Guard<'a>(&'a std::sync::atomic::AtomicBool);
            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            if id == 1 {
                let _guard = Guard(&self.dropped);
                self.old_started.notify_one();
                if self.hold {
                    std::future::pending::<()>().await;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(vec![JobInfo {
                name: format!("job-{id}"),
                status: BuildStatus::Succeeded,
                completed_at: None,
            }])
        }
        async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
            Ok(vec![])
        }
        async fn fetch_pr_state(&self, _: u64) -> Result<crate::model::WatchedPrState> {
            Ok(crate::model::WatchedPrState::Unknown)
        }
    }

    #[tokio::test]
    async fn progressive_old_job_result_cannot_overwrite_new_run() {
        let app = Arc::new(Mutex::new(App::new()));
        poll_actions_tick(&app, &SupersededJobsClient::default()).await;
        let a = app.lock().unwrap();
        assert_eq!(a.workflow_groups[0].run_id, Some(2));
        assert_eq!(a.workflow_groups[0].jobs[0].name, "job-2");
        assert_eq!(a.workflow_groups[0].jobs.len(), 1);
    }

    #[tokio::test]
    async fn progressive_cancelled_tick_drops_unfinished_job_future() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = SupersededJobsClient {
            hold: true,
            ..Default::default()
        };
        let mut tick = Box::pin(poll_actions_tick(&app, &client));
        tokio::select! { _ = &mut tick => panic!("old job held"), _ = tokio::time::sleep(Duration::from_millis(30)) => {} }
        drop(tick);
        assert!(client.dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

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
                                .map(|a| ActionState {
                                    status: a.status,
                                    last_status_change: None,
                                })
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

    #[derive(Default)]
    struct MockActionsClient {
        runs: Vec<WorkflowRunInfo>,
        pr_numbers: std::collections::HashSet<u64>,
        /// Scripted per-PR state responses. Missing PRs default to Unknown.
        /// Read through the `fetch_pr_state` impl only — silence dead-code
        /// until T8 tests drive it directly.
        #[allow(dead_code)]
        pr_states: std::collections::HashMap<u64, crate::model::WatchedPrState>,
    }

    #[async_trait]
    impl ActionsClient for MockActionsClient {
        async fn stream_latest_runs(
            &self,
            updates: tokio::sync::mpsc::UnboundedSender<ActionsUpdate>,
        ) -> Result<()> {
            updates.send(ActionsUpdate::Inventory(
                self.runs
                    .iter()
                    .map(|r| WorkflowInventoryEntry {
                        id: r.run_id,
                        name: r.workflow_name.clone(),
                    })
                    .collect(),
            ))?;
            updates.send(ActionsUpdate::Runs(self.list_latest_runs().await?))?;
            Ok(())
        }
        async fn list_latest_runs(&self) -> Result<RunsPage> {
            Ok(RunsPage {
                summaries: self
                    .runs
                    .iter()
                    .map(|r| WorkflowRunSummary {
                        workflow_name: r.workflow_name.clone(),
                        run_id: r.run_id,
                        status: r.status,
                    })
                    .collect(),
                pr_numbers: self.pr_numbers.clone(),
            })
        }

        async fn fetch_pr_state(&self, pr_number: u64) -> Result<crate::model::WatchedPrState> {
            Ok(self
                .pr_states
                .get(&pr_number)
                .copied()
                .unwrap_or(crate::model::WatchedPrState::Unknown))
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
                            completed_at: j.completed_at,
                        })
                        .collect()
                })
                .unwrap_or_default())
        }

        async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn poll_sources_tick_github_only_leaves_pipelines_alone() {
        let app = Arc::new(Mutex::new(App::with_sources(true, false)));
        let gh = MockActionsClient::default();
        let merged = poll_sources_tick(&app, Some(&gh), None, Duration::ZERO).await;
        assert!(!merged);
        let a = app.lock().unwrap();
        assert!(!a.loading_actions, "actions polled");
        assert!(a.pipeline_groups.is_empty());
    }

    #[tokio::test]
    async fn poll_sources_tick_aws_only_polls_pipelines() {
        let app = Arc::new(Mutex::new(App::with_sources(false, true)));
        let aws = MockPipelineClient {
            pipelines: vec![mock_pipeline("p1", BuildStatus::Succeeded, vec![])],
        };
        let merged = poll_sources_tick(&app, None, Some((&aws, "prof")), Duration::ZERO).await;
        assert!(!merged);
        let a = app.lock().unwrap();
        assert!(!a.loading_pipelines, "pipelines polled");
        assert_eq!(a.pipeline_groups.len(), 1);
        assert!(a.workflow_groups.is_empty());
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
                .map(|(_n, s)| ActionState {
                    status: s,
                    last_status_change: None,
                })
                .collect(),
        }
    }

    // --- T8: poll_pr_states_tick tests ---

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct PrStateMock {
        calls: AtomicUsize,
        script: std::sync::Mutex<
            std::collections::HashMap<u64, VecDeque<crate::model::WatchedPrState>>,
        >,
        error: bool,
    }

    impl PrStateMock {
        fn with_responses(responses: &[(u64, &[crate::model::WatchedPrState])]) -> Self {
            let mut script = std::collections::HashMap::new();
            for (pr, resps) in responses {
                script.insert(*pr, resps.to_vec().into());
            }
            Self {
                calls: AtomicUsize::new(0),
                script: std::sync::Mutex::new(script),
                error: false,
            }
        }

        fn erroring() -> Self {
            Self {
                error: true,
                ..Default::default()
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ActionsClient for PrStateMock {
        async fn list_latest_runs(&self) -> Result<RunsPage> {
            Ok(RunsPage::default())
        }
        async fn fetch_run_jobs(&self, _: u64) -> Result<Vec<JobInfo>> {
            Ok(vec![])
        }
        async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
            Ok(vec![])
        }
        async fn fetch_pr_state(&self, pr_number: u64) -> Result<crate::model::WatchedPrState> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.error {
                anyhow::bail!("scripted error");
            }
            let mut script = self.script.lock().unwrap();
            let next = script.get_mut(&pr_number).and_then(|q| q.pop_front());
            Ok(next.unwrap_or(crate::model::WatchedPrState::Unknown))
        }
    }

    fn set_pr(
        app: &Arc<Mutex<App>>,
        number: u64,
        state: crate::model::WatchedPrState,
        last_checked: Option<Instant>,
    ) {
        let mut a = app.lock().unwrap();
        a.add_or_update_watched_prs(&[number], Instant::now());
        let entry = a.watched_prs.get_mut(&number).unwrap();
        entry.state = state;
        entry.last_checked = last_checked;
    }

    #[tokio::test]
    async fn poll_pr_states_skips_merged_prs() {
        let app = Arc::new(Mutex::new(App::new()));
        set_pr(&app, 7, crate::model::WatchedPrState::Merged, None);
        let mock = PrStateMock::default();
        let changed = poll_pr_states_tick(&app, &mock, Duration::from_secs(30)).await;
        assert!(!changed);
        assert_eq!(mock.call_count(), 0, "terminal state must not be polled");
    }

    #[tokio::test]
    async fn poll_pr_states_skips_closed_unmerged_prs() {
        let app = Arc::new(Mutex::new(App::new()));
        set_pr(&app, 7, crate::model::WatchedPrState::ClosedUnmerged, None);
        let mock = PrStateMock::default();
        let changed = poll_pr_states_tick(&app, &mock, Duration::from_secs(30)).await;
        assert!(!changed);
        assert_eq!(mock.call_count(), 0);
    }

    #[tokio::test]
    async fn poll_pr_states_skips_prs_polled_within_min_interval() {
        let app = Arc::new(Mutex::new(App::new()));
        let recent = Instant::now() - Duration::from_secs(10);
        set_pr(&app, 7, crate::model::WatchedPrState::Open, Some(recent));
        let mock = PrStateMock::default();
        poll_pr_states_tick(&app, &mock, Duration::from_secs(30)).await;
        assert_eq!(mock.call_count(), 0, "throttle should suppress this poll");
    }

    #[tokio::test]
    async fn poll_pr_states_polls_stale_open_pr() {
        let app = Arc::new(Mutex::new(App::new()));
        let stale = Instant::now() - Duration::from_secs(60);
        set_pr(&app, 7, crate::model::WatchedPrState::Open, Some(stale));
        let mock = PrStateMock::with_responses(&[(7, &[crate::model::WatchedPrState::Open])]);
        let changed = poll_pr_states_tick(&app, &mock, Duration::from_secs(30)).await;
        assert!(!changed, "Open → Open is not a merge transition");
        assert_eq!(mock.call_count(), 1);
        let a = app.lock().unwrap();
        assert!(a.watched_prs[&7].last_checked.is_some());
    }

    #[tokio::test]
    async fn poll_pr_states_keeps_prior_state_on_error() {
        let app = Arc::new(Mutex::new(App::new()));
        set_pr(&app, 7, crate::model::WatchedPrState::Open, None);
        let mock = PrStateMock::erroring();
        poll_pr_states_tick(&app, &mock, Duration::from_secs(30)).await;
        let a = app.lock().unwrap();
        assert_eq!(a.watched_prs[&7].state, crate::model::WatchedPrState::Open);
        assert!(
            a.watched_prs[&7].last_checked.is_none(),
            "error must leave last_checked untouched so next tick retries"
        );
    }

    #[tokio::test]
    async fn poll_pr_states_returns_true_on_open_to_merged() {
        let app = Arc::new(Mutex::new(App::new()));
        set_pr(&app, 7, crate::model::WatchedPrState::Open, None);
        let mock = PrStateMock::with_responses(&[(7, &[crate::model::WatchedPrState::Merged])]);
        let changed = poll_pr_states_tick(&app, &mock, Duration::from_secs(30)).await;
        assert!(changed);
        let a = app.lock().unwrap();
        assert_eq!(
            a.watched_prs[&7].state,
            crate::model::WatchedPrState::Merged
        );
    }

    #[tokio::test]
    async fn poll_pr_states_returns_true_exactly_once_across_two_ticks() {
        let app = Arc::new(Mutex::new(App::new()));
        set_pr(&app, 7, crate::model::WatchedPrState::Open, None);
        // Script: first call returns Merged, second is never reached (state is
        // no longer pollable after first flip).
        let mock = PrStateMock::with_responses(&[(
            7,
            &[
                crate::model::WatchedPrState::Merged,
                crate::model::WatchedPrState::Merged,
            ],
        )]);

        let changed1 = poll_pr_states_tick(&app, &mock, Duration::from_secs(30)).await;
        assert!(changed1, "first tick detects merge");

        let changed2 = poll_pr_states_tick(&app, &mock, Duration::from_secs(30)).await;
        assert!(!changed2, "no re-boost on subsequent tick");
        assert_eq!(
            mock.call_count(),
            1,
            "terminal state must prevent further API calls"
        );
    }

    // --- T10: end-to-end linger-after-PR scenarios ---

    #[tokio::test]
    async fn e2e_pr_run_keeps_polling_active() {
        use crate::poll_scheduler::{
            PollScheduler, PollState, PollingPhase, COOLDOWN_DURATION, PR_WATCH_CAP,
        };

        let app = Arc::new(Mutex::new(App::new()));
        let mut pr_numbers = std::collections::HashSet::new();
        pr_numbers.insert(11);
        let client = MockActionsClient {
            runs: vec![],
            pr_numbers,
            ..Default::default()
        };
        poll_actions_tick(&app, &client).await;

        let mut scheduler = PollScheduler::new();
        scheduler.boost();
        scheduler.transition(true); // Grace -> Active
        scheduler.transition(false); // Active -> Cooldown
        scheduler.force_phase_since(
            PollingPhase::Cooldown,
            COOLDOWN_DURATION + Duration::from_secs(1),
        );

        let now = Instant::now();
        let a = app.lock().unwrap();
        let any = a.has_any_running() || a.has_watched_open_prs(PR_WATCH_CAP, now);
        drop(a);
        assert!(
            any,
            "harvested watched PR must make effective_any_running true"
        );

        scheduler.transition(any);
        assert_ne!(
            scheduler.state(),
            PollState::Sleep,
            "scheduler must not drain to Sleep while PR is Open"
        );
    }

    #[tokio::test]
    async fn e2e_pr_merge_flips_scheduler_back_to_grace() {
        use crate::poll_scheduler::{PollScheduler, PollState, PollingPhase};

        let app = Arc::new(Mutex::new(App::new()));
        set_pr(&app, 11, crate::model::WatchedPrState::Open, None);

        let mock = PrStateMock::with_responses(&[(11, &[crate::model::WatchedPrState::Merged])]);

        let mut scheduler = PollScheduler::new();
        scheduler.boost();
        scheduler.transition(true); // Grace -> Active
        scheduler.transition(false); // Active -> Cooldown
        match scheduler.state() {
            PollState::Polling {
                phase: PollingPhase::Cooldown,
                ..
            } => {}
            other => panic!("expected Cooldown, got {other:?}"),
        }

        let merge_detected = poll_pr_states_tick(&app, &mock, Duration::from_secs(30)).await;
        assert!(merge_detected, "Open → Merged must surface from the tick");

        // Orchestrator's handle_pr_merge_detected is scheduler.boost() + state sync.
        scheduler.boost();
        match scheduler.state() {
            PollState::Polling {
                phase: PollingPhase::Grace,
                ..
            } => {}
            other => panic!("expected Grace after merge-boost, got {other:?}"),
        }
    }

    #[test]
    fn e2e_pr_watch_expires_after_cap() {
        use crate::poll_scheduler::PR_WATCH_CAP;

        let mut app = App::new();
        let t0 = Instant::now();
        app.add_or_update_watched_prs(&[11], t0);

        let later = t0 + PR_WATCH_CAP + Duration::from_secs(1);
        assert!(
            !app.has_watched_open_prs(PR_WATCH_CAP, later),
            "past-cap PR must not keep polling active"
        );
        app.prune_expired_prs(PR_WATCH_CAP, later);
        assert!(
            !app.watched_prs.contains_key(&11),
            "prune_expired_prs must drop entries past cap"
        );
    }

    #[tokio::test]
    async fn poll_actions_tick_records_pr_numbers_in_app() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut pr_numbers = std::collections::HashSet::new();
        pr_numbers.insert(42);
        let client = MockActionsClient {
            runs: vec![],
            pr_numbers,
            ..Default::default()
        };
        poll_actions_tick(&app, &client).await;
        let a = app.lock().unwrap();
        assert!(
            a.watched_prs.contains_key(&42),
            "poll_actions_tick must feed harvested PR numbers into app.watched_prs"
        );
        assert_eq!(a.watched_prs[&42].state, crate::model::WatchedPrState::Open);
    }

    #[tokio::test]
    async fn actions_client_runs_page_exposes_pr_numbers() {
        let mut pr_numbers = std::collections::HashSet::new();
        pr_numbers.insert(42);
        let client = MockActionsClient {
            runs: vec![],
            pr_numbers,
            ..Default::default()
        };
        let page = client.list_latest_runs().await.unwrap();
        assert!(page.pr_numbers.contains(&42));
        assert!(page.summaries.is_empty());
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
                    completed_at: None,
                }],
            }],
            ..Default::default()
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
        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let pipes = MockPipelineClient { pipelines: vec![] };
        poll_once(&app, &pipes, &actions).await;

        let app = app.lock().unwrap();
        assert_eq!(app.pipeline_groups.len(), 1);
        assert!(app.pipeline_groups[0].gone);
    }

    struct PartialPipelineClient {
        successful: MockPipelineClient,
        errors: Vec<(&'static str, &'static str)>,
    }

    #[async_trait]
    impl PipelineClient for PartialPipelineClient {
        async fn list_pipeline_names(&self) -> Result<Vec<String>> {
            let mut names = self.successful.list_pipeline_names().await?;
            names.extend(self.errors.iter().map(|(name, _)| (*name).to_string()));
            Ok(names)
        }

        async fn get_pipeline_state(&self, name: &str) -> Result<PipelineState> {
            if let Some((_, error)) = self.errors.iter().find(|(failed, _)| *failed == name) {
                anyhow::bail!("{error}");
            }
            self.successful.get_pipeline_state(name).await
        }

        async fn get_pipeline_definition(&self, name: &str) -> Result<PipelineDefinition> {
            self.successful.get_pipeline_definition(name).await
        }
    }

    #[tokio::test]
    async fn poll_partial_failure_updates_successes_and_preserves_listed_pipeline() {
        let app = Arc::new(Mutex::new(App::new()));
        let initial = MockPipelineClient {
            pipelines: vec![
                mock_pipeline(
                    "healthy",
                    BuildStatus::Running,
                    vec![mock_stage("Build", vec![("build", BuildStatus::Running)])],
                ),
                mock_pipeline(
                    "denied",
                    BuildStatus::Failed,
                    vec![mock_stage("Build", vec![("build", BuildStatus::Failed)])],
                ),
                mock_pipeline("removed", BuildStatus::Succeeded, vec![]),
            ],
        };
        poll_pipelines_tick(&app, &initial, "test-profile", false).await;

        let partial = PartialPipelineClient {
            successful: MockPipelineClient {
                pipelines: vec![mock_pipeline(
                    "healthy",
                    BuildStatus::Succeeded,
                    vec![mock_stage("Build", vec![("build", BuildStatus::Succeeded)])],
                )],
            },
            errors: vec![
                ("denied", "AccessDenied: not authorized"),
                ("deleted-during-poll", "PipelineNotFoundException"),
            ],
        };
        poll_pipelines_tick(&app, &partial, "test-profile", false).await;

        let a = app.lock().unwrap();
        let healthy = a
            .pipeline_groups
            .iter()
            .find(|g| g.name == "healthy")
            .unwrap();
        assert_eq!(healthy.summary_status, BuildStatus::Succeeded);
        assert_eq!(healthy.stages[0].status, BuildStatus::Succeeded);
        let denied = a
            .pipeline_groups
            .iter()
            .find(|g| g.name == "denied")
            .unwrap();
        assert!(!denied.gone, "a failed fetch is not evidence of deletion");
        assert_eq!(denied.summary_status, BuildStatus::Failed);
        assert_eq!(denied.stages[0].status, BuildStatus::Failed);
        assert!(
            a.pipeline_groups
                .iter()
                .find(|g| g.name == "removed")
                .unwrap()
                .gone
        );
        assert_eq!(a.pipeline_groups.len(), 3);
        assert_eq!(a.aws_health, SourceHealth::Healthy);
        assert!(!a.loading_pipelines);
        assert!(a.last_poll.is_some());
        assert!(a
            .warnings
            .iter()
            .any(|w| w.contains("denied") && w.contains("AccessDenied")));
        assert!(a
            .warnings
            .iter()
            .any(|w| w.contains("deleted-during-poll") && w.contains("PipelineNotFoundException")));
    }

    #[tokio::test]
    async fn poll_partial_auth_failure_applies_successes_and_keeps_auth_failed() {
        let app = Arc::new(Mutex::new(App::new()));
        let partial = PartialPipelineClient {
            successful: MockPipelineClient {
                pipelines: vec![mock_pipeline("healthy", BuildStatus::Succeeded, vec![])],
            },
            errors: vec![("expired", "ExpiredToken: the security token is expired")],
        };
        poll_pipelines_tick(&app, &partial, "test-profile", false).await;

        let a = app.lock().unwrap();
        assert_eq!(a.pipeline_groups.len(), 1);
        assert_eq!(a.pipeline_groups[0].name, "healthy");
        assert!(matches!(a.aws_health, SourceHealth::AuthFailed { .. }));
        assert!(a
            .warnings
            .iter()
            .any(|w| w.contains("aws sso login") && w.contains("test-profile")));
        assert!(a
            .warnings
            .iter()
            .any(|w| w.contains("expired") && w.contains("ExpiredToken")));
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
        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
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
        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let app = app.lock().unwrap();
        assert_eq!(app.warnings.len(), 1);
        assert!(app.warnings[0].contains("AWS"));
        assert!(app.last_poll.is_some());
    }

    #[tokio::test]
    async fn poll_creates_pipeline_with_multi_action_stage_status() {
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
        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let app = app.lock().unwrap();
        let group = &app.pipeline_groups[0];
        assert_eq!(group.stages.len(), 2);
        assert_eq!(group.stages[0].name, "Source");
        assert_eq!(group.stages[0].status, BuildStatus::Succeeded);
        assert_eq!(group.stages[1].name, "Build");
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
                        completed_at: None,
                    },
                    JobInfo {
                        name: "test".to_string(),
                        status: BuildStatus::Running,
                        completed_at: None,
                    },
                ],
            }],
            ..Default::default()
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
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
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
        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
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
        poll_pipelines_tick(&app, &pipes, "my-profile", false).await;

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
        async fn list_latest_runs(&self) -> Result<RunsPage> {
            anyhow::bail!("{}", self.error_msg)
        }
        async fn fetch_run_jobs(&self, _run_id: u64) -> Result<Vec<JobInfo>> {
            Ok(vec![])
        }
        async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
            Ok(Vec::new())
        }
        async fn fetch_pr_state(&self, _pr_number: u64) -> Result<crate::model::WatchedPrState> {
            Ok(crate::model::WatchedPrState::Unknown)
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
        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
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
                        completed_at: None,
                    },
                    JobInfo {
                        name: "test".to_string(),
                        status: BuildStatus::Running,
                        completed_at: None,
                    },
                ],
            }],
            ..Default::default()
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
                    completed_at: None,
                }],
            }],
            ..Default::default()
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

    #[tokio::test]
    async fn rate_limit_skip_path_adds_warning() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().rate_limited_until = Some(Instant::now() + Duration::from_secs(300));
        let client = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
        poll_actions_tick(&app, &client).await;

        let a = app.lock().unwrap();
        assert!(
            a.warnings.iter().any(|w| w.contains("rate-limited")),
            "expected a warning containing 'rate-limited', got: {:?}",
            a.warnings
        );
        assert!(!a.loading_actions);
    }

    struct PartialFailActionsClient;

    #[async_trait]
    impl ActionsClient for PartialFailActionsClient {
        async fn list_latest_runs(&self) -> Result<RunsPage> {
            Ok(RunsPage {
                summaries: vec![
                    WorkflowRunSummary {
                        workflow_name: "CI".into(),
                        run_id: 1,
                        status: BuildStatus::Running,
                    },
                    WorkflowRunSummary {
                        workflow_name: "Deploy".into(),
                        run_id: 2,
                        status: BuildStatus::Running,
                    },
                ],
                pr_numbers: std::collections::HashSet::new(),
            })
        }
        async fn fetch_run_jobs(&self, run_id: u64) -> Result<Vec<JobInfo>> {
            if run_id == 1 {
                anyhow::bail!("network error")
            } else {
                Ok(vec![JobInfo {
                    name: "deploy-job".into(),
                    status: BuildStatus::Succeeded,
                    completed_at: None,
                }])
            }
        }
        async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
            Ok(vec![])
        }
        async fn fetch_pr_state(&self, _pr_number: u64) -> Result<crate::model::WatchedPrState> {
            Ok(crate::model::WatchedPrState::Unknown)
        }
    }

    #[tokio::test]
    async fn phase2_job_fetch_error_continues_other_workflows() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = PartialFailActionsClient;
        poll_actions_tick(&app, &client).await;

        let a = app.lock().unwrap();
        let deploy = a
            .workflow_groups
            .iter()
            .find(|g| g.name == "Deploy")
            .expect("Deploy workflow should exist");
        assert_eq!(deploy.jobs.len(), 1);
        assert_eq!(deploy.jobs[0].name, "deploy-job");

        let ci = a
            .workflow_groups
            .iter()
            .find(|g| g.name == "CI")
            .expect("CI workflow should exist");
        assert!(
            ci.jobs.is_empty(),
            "CI jobs should be empty after fetch error"
        );
    }

    struct UnauthorizedClient;

    #[async_trait]
    impl PipelineClient for UnauthorizedClient {
        async fn list_pipeline_names(&self) -> Result<Vec<String>> {
            anyhow::bail!("UnauthorizedException: user is not authorized")
        }
        async fn get_pipeline_state(&self, _name: &str) -> Result<PipelineState> {
            anyhow::bail!("UnauthorizedException")
        }
        async fn get_pipeline_definition(&self, _name: &str) -> Result<PipelineDefinition> {
            anyhow::bail!("UnauthorizedException")
        }
    }

    #[tokio::test]
    async fn poll_unauthorized_exception_shows_sso_hint() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = UnauthorizedClient;
        poll_pipelines_tick(&app, &pipes, "my-profile", false).await;

        let a = app.lock().unwrap();
        assert_eq!(a.warnings.len(), 1);
        assert!(
            a.warnings[0].contains("aws sso login"),
            "expected SSO hint, got: {}",
            a.warnings[0]
        );
        assert!(a.warnings[0].contains("my-profile"));
    }

    #[test]
    fn reconcile_bars_new_bar_with_terminal_status_gets_minimum_fill() {
        let mut bars: Vec<Bar> = vec![];
        let updates = vec![
            ("Source".to_string(), BuildStatus::Succeeded, None),
            ("Build".to_string(), BuildStatus::Failed, None),
        ];
        reconcile_bars(&mut bars, updates);

        let source = bars.iter().find(|b| b.name == "Source").unwrap();
        assert_eq!(source.status, BuildStatus::Succeeded);
        assert_eq!(
            source.fill, 1,
            "new bar with terminal status should get minimum fill=1"
        );

        let build = bars.iter().find(|b| b.name == "Build").unwrap();
        assert_eq!(build.status, BuildStatus::Failed);
        assert_eq!(
            build.fill, 1,
            "new bar with terminal status should get minimum fill=1"
        );
    }

    #[test]
    fn classify_workflows_sets_categories() {
        let mut app = App::new();
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.workflow_groups.push(WorkflowGroup {
            name: "Claude Code Review".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });

        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "p",
            "--region",
            "r",
            "--github-repo",
            "o/r",
        ])
        .unwrap();
        classify_workflows(&mut app, &config);

        assert_eq!(app.workflow_groups[0].category, WorkflowCategory::CI);
        assert_eq!(app.workflow_groups[1].category, WorkflowCategory::Review);
    }

    #[test]
    fn action_state_carries_timestamp() {
        use chrono::TimeZone;
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 3, 18, 14, 25, 0)
            .unwrap();
        let action = ActionState {
            status: BuildStatus::Succeeded,
            last_status_change: Some(ts),
        };
        assert_eq!(action.last_status_change, Some(ts));
    }

    #[test]
    fn job_info_carries_completed_at() {
        use chrono::TimeZone;
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 3, 18, 14, 28, 0)
            .unwrap();
        let job = JobInfo {
            name: "build".to_string(),
            status: BuildStatus::Succeeded,
            completed_at: Some(ts),
        };
        assert_eq!(job.completed_at, Some(ts));
    }

    #[test]
    fn reconcile_bars_marks_missing_as_gone() {
        let mut bars = vec![Bar::new("alpha".to_string()), Bar::new("beta".to_string())];
        bars[0].set_status(BuildStatus::Succeeded);
        bars[1].set_status(BuildStatus::Running);

        // Update only contains "alpha"; "beta" should be marked gone
        let updates = vec![("alpha".to_string(), BuildStatus::Succeeded, None)];
        reconcile_bars(&mut bars, updates);

        let alpha = bars.iter().find(|b| b.name == "alpha").unwrap();
        assert!(!alpha.gone);
        let beta = bars.iter().find(|b| b.name == "beta").unwrap();
        assert!(beta.gone);
    }

    #[test]
    fn reconcile_bars_sets_last_finished_from_tuple() {
        use chrono::TimeZone;
        let mut bars: Vec<Bar> = vec![];
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 3, 18, 14, 28, 0)
            .unwrap();
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
    fn stage_status_and_timestamp_for_succeeded() {
        use chrono::TimeZone;
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 3, 18, 14, 25, 0)
            .unwrap();
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

    #[test]
    fn is_auth_error_expired_token() {
        assert!(is_auth_error("ExpiredToken: the security token is expired"));
    }

    #[test]
    fn is_auth_error_unauthorized_exception() {
        assert!(is_auth_error(
            "UnauthorizedException: user is not authorized"
        ));
    }

    #[test]
    fn is_auth_error_invalid_identity_token() {
        assert!(is_auth_error("InvalidIdentityToken: bad token"));
    }

    #[test]
    fn is_auth_error_expired_token_exception() {
        assert!(is_auth_error("ExpiredTokenException: token expired"));
    }

    #[test]
    fn is_auth_error_access_denied_with_sso() {
        assert!(is_auth_error("AccessDenied: SSO session has expired"));
    }

    #[test]
    fn is_auth_error_access_denied_with_sts() {
        assert!(is_auth_error("AccessDenied: STS token is invalid"));
    }

    #[test]
    fn is_auth_error_plain_access_denied_is_not_auth() {
        assert!(!is_auth_error(
            "AccessDenied: not authorized to perform action"
        ));
    }

    #[test]
    fn is_auth_error_connection_refused_is_not_auth() {
        assert!(!is_auth_error("connection refused"));
    }

    #[test]
    fn is_auth_error_network_timeout_is_not_auth() {
        assert!(!is_auth_error("network timeout"));
    }

    #[test]
    fn is_auth_error_loading_credentials() {
        assert!(is_auth_error(
            "failed to list pipelines: dispatch failure: other: \
             an error occurred while loading credentials: \
             an error occurred while loading credentials"
        ));
    }

    #[tokio::test]
    async fn poll_expired_token_sets_auth_failed() {
        use crate::app::SourceHealth;
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = ExpiredTokenClient;
        poll_pipelines_tick(&app, &pipes, "my-profile", false).await;

        let a = app.lock().unwrap();
        assert!(
            matches!(a.aws_health, SourceHealth::AuthFailed { .. }),
            "expected AuthFailed, got: {:?}",
            a.aws_health
        );
    }

    #[tokio::test]
    async fn poll_success_transitions_unknown_to_healthy() {
        use crate::app::SourceHealth;
        let app = Arc::new(Mutex::new(App::new()));
        // App starts with Unknown after step 1
        assert_eq!(app.lock().unwrap().aws_health, SourceHealth::Unknown);

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Succeeded, vec![])],
        };
        poll_pipelines_tick(&app, &pipes, "my-profile", false).await;

        let a = app.lock().unwrap();
        assert_eq!(a.aws_health, SourceHealth::Healthy);
    }

    #[tokio::test]
    async fn poll_success_clears_auth_failed() {
        use crate::app::SourceHealth;
        let app = Arc::new(Mutex::new(App::new()));
        {
            let mut a = app.lock().unwrap();
            a.aws_health = SourceHealth::AuthFailed { since: Utc::now() };
        }
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Succeeded, vec![])],
        };
        poll_pipelines_tick(&app, &pipes, "my-profile", true).await;

        let a = app.lock().unwrap();
        assert_eq!(a.aws_health, SourceHealth::Healthy);
    }

    #[test]
    fn reconcile_bars_clears_last_finished_on_new_run() {
        use chrono::TimeZone;
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 3, 18, 14, 28, 0)
            .unwrap();
        let mut bars: Vec<Bar> = vec![];
        reconcile_bars(
            &mut bars,
            vec![("Build".to_string(), BuildStatus::Succeeded, Some(ts))],
        );
        assert_eq!(bars[0].last_finished, Some(ts));

        reconcile_bars(
            &mut bars,
            vec![("Build".to_string(), BuildStatus::Running, None)],
        );
        assert!(bars[0].last_finished.is_none());
    }

    #[tokio::test]
    async fn poll_preserves_job_timestamp() {
        use chrono::TimeZone;
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 3, 18, 14, 28, 0)
            .unwrap();
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
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let a = app.lock().unwrap();
        let job = &a.workflow_groups[0].jobs[0];
        assert_eq!(job.last_finished, Some(ts));
    }

    #[tokio::test]
    async fn poll_clears_timestamp_on_new_run() {
        use chrono::TimeZone;
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 3, 18, 14, 28, 0)
            .unwrap();
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
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

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
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let a = app.lock().unwrap();
        let job = &a.workflow_groups[0].jobs[0];
        assert!(job.last_finished.is_none());
    }

    #[tokio::test]
    async fn poll_skips_when_auth_failed_and_not_forced() {
        use crate::app::SourceHealth;
        let app = Arc::new(Mutex::new(App::new()));
        {
            let mut a = app.lock().unwrap();
            a.aws_health = SourceHealth::AuthFailed { since: Utc::now() };
            a.push_warning("AWS: SSO session expired".to_string());
            a.loading_pipelines = false;
        }
        let pipes = FailingPipelineClient;
        poll_pipelines_tick(&app, &pipes, "my-profile", false).await;

        let a = app.lock().unwrap();
        // Warning should be preserved (not cleared)
        assert!(
            a.warnings.iter().any(|w| w.contains("SSO")),
            "SSO warning should be preserved, got: {:?}",
            a.warnings
        );
        // loading_pipelines should remain unchanged (skip = no-op)
        assert!(!a.loading_pipelines);
    }

    #[tokio::test]
    async fn poll_force_bypasses_auth_failed_skip() {
        use crate::app::SourceHealth;
        let app = Arc::new(Mutex::new(App::new()));
        {
            let mut a = app.lock().unwrap();
            a.aws_health = SourceHealth::AuthFailed { since: Utc::now() };
        }
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Succeeded, vec![])],
        };
        poll_pipelines_tick(&app, &pipes, "my-profile", true).await;

        let a = app.lock().unwrap();
        // Should have recovered
        assert_eq!(a.aws_health, SourceHealth::Healthy);
        assert_eq!(a.pipeline_groups.len(), 1);
    }

    #[tokio::test]
    async fn poll_first_sees_terminal_stage_gets_minimum_fill() {
        // Pipeline with Source already Succeeded and Build already Failed
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "my-pipe",
                BuildStatus::Succeeded,
                vec![
                    mock_stage("Source", vec![("checkout", BuildStatus::Succeeded)]),
                    mock_stage("Build", vec![("compile", BuildStatus::Failed)]),
                ],
            )],
        };
        // GH workflow with a job already Succeeded
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".to_string(),
                run_id: 42,
                status: BuildStatus::Succeeded,
                jobs: vec![JobInfo {
                    name: "lint".to_string(),
                    status: BuildStatus::Succeeded,
                    completed_at: None,
                }],
            }],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let a = app.lock().unwrap();

        // Pipeline stages should have minimum fill=1
        let source = &a.pipeline_groups[0].stages[0];
        assert_eq!(source.name, "Source");
        assert_eq!(source.status, BuildStatus::Succeeded);
        assert!(
            source.fill >= 1,
            "terminal stage on first poll must have fill >= 1, got {}",
            source.fill
        );

        let build = &a.pipeline_groups[0].stages[1];
        assert_eq!(build.name, "Build");
        assert_eq!(build.status, BuildStatus::Failed);
        assert!(
            build.fill >= 1,
            "terminal stage on first poll must have fill >= 1, got {}",
            build.fill
        );

        // GH Actions job should also have minimum fill=1
        let job = &a.workflow_groups[0].jobs[0];
        assert_eq!(job.name, "lint");
        assert_eq!(job.status, BuildStatus::Succeeded);
        assert!(
            job.fill >= 1,
            "terminal job on first poll must have fill >= 1, got {}",
            job.fill
        );
    }

    // --- check_aws_auth tests ---

    struct OkAuthClient;

    #[async_trait]
    impl AuthHealthClient for OkAuthClient {
        async fn get_caller_identity(&self) -> Result<()> {
            Ok(())
        }
    }

    struct ExpiredAuthClient;

    #[async_trait]
    impl AuthHealthClient for ExpiredAuthClient {
        async fn get_caller_identity(&self) -> Result<()> {
            anyhow::bail!("ExpiredToken: the security token is expired")
        }
    }

    struct NetworkErrorAuthClient;

    #[async_trait]
    impl AuthHealthClient for NetworkErrorAuthClient {
        async fn get_caller_identity(&self) -> Result<()> {
            anyhow::bail!("connection refused")
        }
    }

    #[tokio::test]
    async fn check_aws_auth_expired_sets_auth_failed() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = ExpiredAuthClient;
        check_aws_auth(&app, &client, "my-profile").await;

        let a = app.lock().unwrap();
        assert!(matches!(a.aws_health, SourceHealth::AuthFailed { .. }));
        assert!(a.warnings.iter().any(|w| w.contains("aws sso login")));
        assert!(a.warnings.iter().any(|w| w.contains("my-profile")));
    }

    #[tokio::test]
    async fn check_aws_auth_success_clears_auth_failed() {
        let app = Arc::new(Mutex::new(App::new()));
        {
            let mut a = app.lock().unwrap();
            a.aws_health = SourceHealth::AuthFailed { since: Utc::now() };
            a.push_warning("AWS: SSO session expired".to_string());
        }

        let client = OkAuthClient;
        check_aws_auth(&app, &client, "my-profile").await;

        let a = app.lock().unwrap();
        assert!(matches!(a.aws_health, SourceHealth::Healthy));
        assert!(a.warnings.is_empty());
    }

    #[tokio::test]
    async fn check_aws_auth_non_auth_error_does_not_set_auth_failed() {
        let app = Arc::new(Mutex::new(App::new()));
        let client = NetworkErrorAuthClient;
        check_aws_auth(&app, &client, "my-profile").await;

        let a = app.lock().unwrap();
        assert!(!matches!(a.aws_health, SourceHealth::AuthFailed { .. }));
        assert!(a.warnings.is_empty());
    }

    #[tokio::test]
    async fn check_aws_auth_success_noop_when_already_healthy() {
        let app = Arc::new(Mutex::new(App::new()));
        {
            let mut a = app.lock().unwrap();
            a.aws_health = SourceHealth::Healthy;
        }

        let client = OkAuthClient;
        check_aws_auth(&app, &client, "my-profile").await;

        let a = app.lock().unwrap();
        assert!(matches!(a.aws_health, SourceHealth::Healthy));
        assert!(a.warnings.is_empty());
    }

    #[test]
    fn stage_status_and_timestamp_empty_actions() {
        let actions: Vec<ActionState> = vec![];
        let (status, timestamp) = stage_status_and_timestamp(&actions);
        assert_eq!(status, BuildStatus::Idle);
        assert!(timestamp.is_none());
    }

    #[test]
    fn stage_status_and_timestamp_failed_with_time() {
        use chrono::TimeZone;
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 3, 18, 14, 25, 0)
            .unwrap();
        let actions = vec![ActionState {
            status: BuildStatus::Failed,
            last_status_change: Some(ts),
        }];
        let (status, timestamp) = stage_status_and_timestamp(&actions);
        assert_eq!(status, BuildStatus::Failed);
        assert_eq!(timestamp, Some(ts));
    }

    #[test]
    fn stage_status_and_timestamp_running_ignores_timestamp() {
        use chrono::TimeZone;
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 3, 18, 14, 25, 0)
            .unwrap();
        let actions = vec![ActionState {
            status: BuildStatus::Running,
            last_status_change: Some(ts),
        }];
        let (status, timestamp) = stage_status_and_timestamp(&actions);
        assert_eq!(status, BuildStatus::Running);
        assert!(timestamp.is_none(), "Running should not expose timestamp");
    }

    #[test]
    fn stage_status_and_timestamp_failed_after_success() {
        use chrono::TimeZone;
        let ts = chrono::Utc.with_ymd_and_hms(2026, 3, 18, 15, 0, 0).unwrap();
        let actions = vec![
            ActionState {
                status: BuildStatus::Succeeded,
                last_status_change: Some(
                    chrono::Utc.with_ymd_and_hms(2026, 3, 18, 14, 0, 0).unwrap(),
                ),
            },
            ActionState {
                status: BuildStatus::Failed,
                last_status_change: Some(ts),
            },
        ];
        let (status, timestamp) = stage_status_and_timestamp(&actions);
        assert_eq!(status, BuildStatus::Failed);
        assert_eq!(timestamp, Some(ts));
    }

    #[test]
    fn stage_status_considers_earlier_running_or_failed_actions() {
        for (statuses, expected) in [
            (
                [BuildStatus::Running, BuildStatus::Succeeded],
                BuildStatus::Running,
            ),
            (
                [BuildStatus::Failed, BuildStatus::Succeeded],
                BuildStatus::Failed,
            ),
            (
                [BuildStatus::Running, BuildStatus::Failed],
                BuildStatus::Running,
            ),
            (
                [BuildStatus::Failed, BuildStatus::Running],
                BuildStatus::Running,
            ),
        ] {
            let actions: Vec<_> = statuses
                .into_iter()
                .map(|status| ActionState {
                    status,
                    last_status_change: None,
                })
                .collect();
            assert_eq!(stage_status_and_timestamp(&actions), (expected, None));
        }
    }

    #[test]
    fn stage_completion_timestamp_uses_latest_action_completion() {
        let earlier = chrono::DateTime::from_timestamp(100, 0).unwrap();
        let later = chrono::DateTime::from_timestamp(200, 0).unwrap();
        for status in [BuildStatus::Succeeded, BuildStatus::Failed] {
            let actions = vec![
                ActionState {
                    status,
                    last_status_change: Some(later),
                },
                ActionState {
                    status: BuildStatus::Succeeded,
                    last_status_change: Some(earlier),
                },
            ];
            assert_eq!(stage_status_and_timestamp(&actions), (status, Some(later)));
        }
    }

    #[test]
    fn stage_failed_action_is_not_hidden_by_later_success() {
        let actions = vec![
            ActionState {
                status: BuildStatus::Failed,
                last_status_change: None,
            },
            ActionState {
                status: BuildStatus::Succeeded,
                last_status_change: None,
            },
        ];
        assert_eq!(
            stage_status_and_timestamp(&actions),
            (BuildStatus::Failed, None)
        );
    }

    #[test]
    fn stage_running_action_suppresses_finished_timestamp() {
        let finished = chrono::DateTime::from_timestamp(100, 0).unwrap();
        let actions = vec![
            ActionState {
                status: BuildStatus::Running,
                last_status_change: None,
            },
            ActionState {
                status: BuildStatus::Succeeded,
                last_status_change: Some(finished),
            },
        ];
        assert_eq!(
            stage_status_and_timestamp(&actions),
            (BuildStatus::Running, None)
        );
    }

    #[tokio::test]
    async fn update_workflow_jobs_missing_group_is_noop() {
        let mut app = App::new();
        update_workflow_jobs(
            &mut app,
            "nonexistent",
            vec![JobInfo {
                name: "build".to_string(),
                status: BuildStatus::Running,
                completed_at: None,
            }],
        );
        assert!(app.workflow_groups.is_empty());
    }

    // === Multi-poll E2E scenarios ===

    #[tokio::test]
    async fn e2e_pipeline_lifecycle_idle_running_succeeded() {
        let app = Arc::new(Mutex::new(App::new()));
        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };

        // Poll 1: pipeline arrives Idle
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "deploy",
                BuildStatus::Idle,
                vec![mock_stage("Source", vec![("checkout", BuildStatus::Idle)])],
            )],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups[0].summary_status, BuildStatus::Idle);
            assert!(!a.has_any_running());
        }

        // Poll 2: pipeline now Running
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "deploy",
                BuildStatus::Running,
                vec![mock_stage(
                    "Source",
                    vec![("checkout", BuildStatus::Running)],
                )],
            )],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups[0].summary_status, BuildStatus::Running);
            assert!(a.has_any_running());
        }

        // Poll 3: pipeline Succeeded
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "deploy",
                BuildStatus::Succeeded,
                vec![mock_stage(
                    "Source",
                    vec![("checkout", BuildStatus::Succeeded)],
                )],
            )],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups[0].summary_status, BuildStatus::Succeeded);
            assert!(!a.has_any_running());
        }
    }

    #[tokio::test]
    async fn e2e_pipeline_lifecycle_running_to_failed() {
        let app = Arc::new(Mutex::new(App::new()));
        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "deploy",
                BuildStatus::Running,
                vec![mock_stage("Build", vec![("compile", BuildStatus::Running)])],
            )],
        };
        poll_once(&app, &pipes, &actions).await;
        assert!(app.lock().unwrap().has_any_running());

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "deploy",
                BuildStatus::Failed,
                vec![mock_stage("Build", vec![("compile", BuildStatus::Failed)])],
            )],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups[0].summary_status, BuildStatus::Failed);
            assert!(!a.has_any_running());
        }
    }

    #[tokio::test]
    async fn e2e_workflow_appears_then_goes_gone() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient { pipelines: vec![] };

        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![],
            }],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;
        assert_eq!(app.lock().unwrap().workflow_groups.len(), 1);
        assert!(!app.lock().unwrap().workflow_groups[0].gone);

        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.workflow_groups.len(), 1, "group should persist");
            assert!(a.workflow_groups[0].gone, "should be marked gone");
        }
    }

    #[tokio::test]
    async fn e2e_workflow_reappears_after_gone() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient { pipelines: vec![] };

        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![],
            }],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;
        assert!(app.lock().unwrap().workflow_groups[0].gone);

        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 2,
                status: BuildStatus::Running,
                jobs: vec![],
            }],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert!(!a.workflow_groups[0].gone, "should no longer be gone");
            assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Running);
        }
    }

    #[tokio::test]
    async fn e2e_auth_recovery_flow() {
        let app = Arc::new(Mutex::new(App::new()));

        let pipes = ExpiredTokenClient;
        poll_pipelines_tick(&app, &pipes, "my-profile", false).await;
        {
            let a = app.lock().unwrap();
            assert!(matches!(a.aws_health, SourceHealth::AuthFailed { .. }));
            assert!(!a.warnings.is_empty());
        }

        let auth_client = OkAuthClient;
        check_aws_auth(&app, &auth_client, "my-profile").await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.aws_health, SourceHealth::Healthy);
            assert!(a.warnings.is_empty());
        }

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Succeeded, vec![])],
        };
        poll_pipelines_tick(&app, &pipes, "my-profile", false).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.aws_health, SourceHealth::Healthy);
            assert_eq!(a.pipeline_groups.len(), 1);
        }
    }

    #[tokio::test]
    async fn e2e_rate_limit_backoff_and_recovery() {
        let app = Arc::new(Mutex::new(App::new()));

        let client = RateLimitActionsClient {
            error_msg: "403 Forbidden".to_string(),
        };
        poll_actions_tick(&app, &client).await;
        {
            let a = app.lock().unwrap();
            assert!(a.rate_limited_until.is_some());
        }

        let client = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![],
            }],
            ..Default::default()
        };
        poll_actions_tick(&app, &client).await;
        {
            let a = app.lock().unwrap();
            assert!(
                a.warnings.iter().any(|w| w.contains("rate-limited")),
                "should show rate-limited warning"
            );
            assert!(a.workflow_groups.is_empty());
        }

        {
            let mut a = app.lock().unwrap();
            a.rate_limited_until = None;
            a.warnings.clear();
        }

        poll_actions_tick(&app, &client).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.workflow_groups.len(), 1);
            assert_eq!(a.workflow_groups[0].name, "CI");
        }
    }

    #[tokio::test]
    async fn e2e_multi_pipeline_independent_lifecycle() {
        let app = Arc::new(Mutex::new(App::new()));
        let actions = MockActionsClient {
            runs: vec![],
            ..Default::default()
        };

        let pipes = MockPipelineClient {
            pipelines: vec![
                mock_pipeline(
                    "pipe-a",
                    BuildStatus::Running,
                    vec![mock_stage("Build", vec![("compile", BuildStatus::Running)])],
                ),
                mock_pipeline(
                    "pipe-b",
                    BuildStatus::Succeeded,
                    vec![mock_stage(
                        "Deploy",
                        vec![("deploy", BuildStatus::Succeeded)],
                    )],
                ),
            ],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups.len(), 2);
            let pa = a
                .pipeline_groups
                .iter()
                .find(|g| g.name == "pipe-a")
                .unwrap();
            assert_eq!(pa.summary_status, BuildStatus::Running);
            let pb = a
                .pipeline_groups
                .iter()
                .find(|g| g.name == "pipe-b")
                .unwrap();
            assert_eq!(pb.summary_status, BuildStatus::Succeeded);
            assert!(a.has_any_running());
        }

        let pipes = MockPipelineClient {
            pipelines: vec![
                mock_pipeline(
                    "pipe-a",
                    BuildStatus::Succeeded,
                    vec![mock_stage(
                        "Build",
                        vec![("compile", BuildStatus::Succeeded)],
                    )],
                ),
                mock_pipeline(
                    "pipe-b",
                    BuildStatus::Succeeded,
                    vec![mock_stage(
                        "Deploy",
                        vec![("deploy", BuildStatus::Succeeded)],
                    )],
                ),
            ],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert!(!a.has_any_running());
        }
    }

    #[tokio::test]
    async fn e2e_classify_workflows_across_polls() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient { pipelines: vec![] };
        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "p",
            "--region",
            "r",
            "--github-repo",
            "o/r",
        ])
        .unwrap();

        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Running,
                jobs: vec![],
            }],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let mut a = app.lock().unwrap();
            classify_workflows(&mut a, &config);
            assert_eq!(a.workflow_groups[0].category, WorkflowCategory::CI);
        }

        let actions = MockActionsClient {
            runs: vec![
                WorkflowRunInfo {
                    workflow_name: "CI".into(),
                    run_id: 2,
                    status: BuildStatus::Succeeded,
                    jobs: vec![],
                },
                WorkflowRunInfo {
                    workflow_name: "Claude Code Review".into(),
                    run_id: 3,
                    status: BuildStatus::Running,
                    jobs: vec![],
                },
            ],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let mut a = app.lock().unwrap();
            classify_workflows(&mut a, &config);
            let ci = a.workflow_groups.iter().find(|g| g.name == "CI").unwrap();
            assert_eq!(ci.category, WorkflowCategory::CI);
            let review = a
                .workflow_groups
                .iter()
                .find(|g| g.name == "Claude Code Review")
                .unwrap();
            assert_eq!(review.category, WorkflowCategory::Review);
        }
    }

    #[tokio::test]
    async fn e2e_concurrent_pipeline_and_workflow_updates() {
        let app = Arc::new(Mutex::new(App::new()));

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Running, vec![])],
        };
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![JobInfo {
                    name: "build".into(),
                    status: BuildStatus::Succeeded,
                    completed_at: None,
                }],
            }],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let a = app.lock().unwrap();
        assert_eq!(a.pipeline_groups.len(), 1);
        assert_eq!(a.pipeline_groups[0].name, "deploy");
        assert_eq!(a.workflow_groups.len(), 1);
        assert_eq!(a.workflow_groups[0].name, "CI");
        assert_eq!(a.workflow_groups[0].jobs.len(), 1);
        assert_eq!(a.pipeline_groups[0].summary_status, BuildStatus::Running);
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Succeeded);
    }

    #[tokio::test]
    async fn e2e_linkage_gh_completes_cp_starts() {
        use crate::linkage::{apply_links, LinkMap};

        let app = Arc::new(Mutex::new(App::new()));
        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "deploy".into(),
            "CI".into(),
            "bucket".into(),
            "key.zip".into(),
        );

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Idle, vec![])],
        };
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Running,
                jobs: vec![JobInfo {
                    name: "build".into(),
                    status: BuildStatus::Running,
                    completed_at: None,
                }],
            }],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline(
                "deploy",
                BuildStatus::Running,
                vec![mock_stage("Source", vec![("s3", BuildStatus::Succeeded)])],
            )],
        };
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Running,
                jobs: vec![JobInfo {
                    name: "build".into(),
                    status: BuildStatus::Running,
                    completed_at: None,
                }],
            }],
            ..Default::default()
        };
        poll_once(&app, &pipes, &actions).await;

        let mut stopped_runs = std::collections::HashMap::new();
        apply_links(&app, &mut link_map, &mut stopped_runs);

        let a = app.lock().unwrap();
        let ci = a.workflow_groups.iter().find(|g| g.name == "CI").unwrap();
        assert_eq!(
            ci.summary_status,
            BuildStatus::Succeeded,
            "linked GH workflow should be Succeeded when CP is Running"
        );
        let build_job = ci.jobs.iter().find(|j| j.name == "build").unwrap();
        assert_eq!(
            build_job.status,
            BuildStatus::Succeeded,
            "linked GH job should be Succeeded when CP is Running"
        );
    }
}
