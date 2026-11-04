use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{
    ActionsClient, ActionsUpdate, JobInfo, RunsPage, S3Upload, WorkflowFile,
    WorkflowInventoryEntry, WorkflowRunSummary,
};
use crate::model::{BuildStatus, WatchedPrState};
use futures::{stream, StreamExt};

pub struct GitHubActionsClient {
    octocrab: octocrab::Octocrab,
    owner: String,
    repo: String,
    branch: Option<String>,
    /// Latest run per workflow from earlier polls, so steady-state polls
    /// only need page 1 of the (slow, ~40 KB/run) runs listing.
    runs_cache: std::sync::Mutex<RunsCache>,
}

type LatestRuns = std::collections::HashMap<String, (u64, BuildStatus)>;

#[derive(Default)]
struct RunsCache {
    latest: LatestRuns,
    calls: u32,
    inventory: Option<Vec<WorkflowInventoryEntry>>,
    /// Names explicitly removed by successful inventory, distinct from unseen workflows.
    removed: std::collections::HashSet<String>,
}

impl RunsCache {
    fn reconcile_inventory(&mut self, entries: &[WorkflowInventoryEntry]) {
        let names: std::collections::HashSet<_> = entries.iter().map(|e| e.name.as_str()).collect();
        self.removed.extend(
            self.latest
                .keys()
                .filter(|name| !names.contains(name.as_str()))
                .cloned(),
        );
        if let Some(previous) = &self.inventory {
            self.removed.extend(
                previous
                    .iter()
                    .filter(|e| !names.contains(e.name.as_str()))
                    .map(|e| e.name.clone()),
            );
        }
        self.removed.retain(|name| !names.contains(name.as_str()));
        self.latest.retain(|name, _| names.contains(name.as_str()));
        self.inventory = Some(entries.to_vec());
    }
}

/// Runs per page. Listing time scales with payload: 100/page ≈ 3.2 s,
/// 30/page ≈ 1.4 s (measured on a real repo).
const RUNS_PER_PAGE: u32 = 30;
/// Safety cap on pages per full scan (≈ same 2000-run lookback as before).
const MAX_PAGES: u32 = 60;
/// Every Nth listing is a full scan, so removed workflows drop out.
const FULL_SCAN_EVERY: u32 = 20;

/// Full workflow discovery vs page 1 merged into the cache.
fn needs_full_scan<T>(known: &Option<T>, call: u32) -> bool {
    known.is_none() || call.is_multiple_of(FULL_SCAN_EVERY)
}

/// A cached Running workflow absent from the fresh page would freeze as
/// Running; the poll then escalates to a full scan to find its latest run.
fn has_stale_running(known: &LatestRuns, fresh: &LatestRuns) -> bool {
    known
        .iter()
        .any(|(name, (_, st))| *st == BuildStatus::Running && !fresh.contains_key(name))
}

/// Whether to fetch the next runs page.
fn more_pages(page: u32, fetched: u64, total_count: u64, full_scan: bool) -> bool {
    full_scan
        && fetched >= RUNS_PER_PAGE as u64
        && (page as u64) * (RUNS_PER_PAGE as u64) < total_count
        && page < MAX_PAGES
}

/// Incremental poll: fresh runs win, workflows not on page 1 keep their last run.
fn merge_latest(known: &mut LatestRuns, fresh: LatestRuns) {
    // Run IDs increase, so delayed responses cannot replace a newer execution.
    for (name, run) in fresh {
        if known.get(&name).is_none_or(|(id, _)| *id <= run.0) {
            known.insert(name, run);
        }
    }
}

impl GitHubActionsClient {
    pub fn new(token: &str, owner: String, repo: String, branch: Option<String>) -> Result<Self> {
        let octocrab = octocrab::Octocrab::builder()
            .personal_token(token.to_string())
            .build()
            .context("failed to build GitHub client")?;
        Ok(Self {
            octocrab,
            owner,
            repo,
            branch,
            runs_cache: std::sync::Mutex::new(RunsCache::default()),
        })
    }

    async fn get_json(&self, route: &str, branch: bool) -> Result<serde_json::Value> {
        let parameters = self
            .branch
            .as_ref()
            .filter(|_| branch)
            .map(|b| [("branch", b)]);
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            self.octocrab.get(route, parameters.as_ref()),
        )
        .await
        .with_context(|| format!("GitHub request timed out after 15s: {route}"))?
        .with_context(|| format!("GitHub request failed: {route}"))
    }

    async fn fetch_workflow_inventory(&self) -> Result<Vec<WorkflowInventoryEntry>> {
        let mut inventory = Vec::new();
        for page in 1..=MAX_PAGES {
            let route = format!(
                "/repos/{}/{}/actions/workflows?per_page=100&page={page}",
                self.owner, self.repo,
            );
            let resp = self
                .get_json(&route, false)
                .await
                .context("failed to list workflows")?;
            let workflows = resp["workflows"]
                .as_array()
                .context("workflow listing has no workflows array")?;
            for workflow in workflows {
                inventory.push(WorkflowInventoryEntry {
                    id: workflow["id"].as_u64().context("workflow has no id")?,
                    name: workflow["name"]
                        .as_str()
                        .context("workflow has no name")?
                        .to_string(),
                });
            }
            if workflows.len() < 100
                || u64::from(page) * 100 >= resp["total_count"].as_u64().unwrap_or(0)
            {
                return Ok(inventory);
            }
        }
        anyhow::bail!("workflow inventory exceeded {MAX_PAGES} pages")
    }

    fn publish_runs(
        &self,
        updates: &tokio::sync::mpsc::UnboundedSender<ActionsUpdate>,
        mut fresh: LatestRuns,
        pr_numbers: std::collections::HashSet<u64>,
    ) -> Result<()> {
        {
            let mut cache = self
                .runs_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("runs cache mutex poisoned"))?;
            fresh.retain(|name, _| !cache.removed.contains(name));
            merge_latest(&mut cache.latest, fresh.clone());
        }
        let summaries = fresh
            .iter()
            .map(|(name, (run_id, status))| WorkflowRunSummary {
                workflow_name: name.clone(),
                run_id: *run_id,
                status: *status,
            })
            .collect();
        updates.send(ActionsUpdate::Runs(RunsPage {
            summaries,
            pr_numbers,
        }))?;
        Ok(())
    }

    async fn scan_workflow(
        &self,
        workflow: &WorkflowInventoryEntry,
    ) -> Result<(LatestRuns, std::collections::HashSet<u64>)> {
        let mut latest = LatestRuns::new();
        let mut prs = std::collections::HashSet::new();
        for page in 1..=MAX_PAGES {
            let route = format!(
                "/repos/{}/{}/actions/workflows/{}/runs?per_page={RUNS_PER_PAGE}&page={page}",
                self.owner, self.repo, workflow.id
            );
            let resp = self.get_json(&route, true).await?;
            parse_workflow_runs(&resp, &mut latest, &mut prs, self.branch.as_deref());
            let runs = resp["workflow_runs"]
                .as_array()
                .context("workflow run listing has no workflow_runs array")?;
            if latest.is_empty()
                && page == MAX_PAGES
                && runs.len() >= RUNS_PER_PAGE as usize
                && u64::from(page * RUNS_PER_PAGE) < resp["total_count"].as_u64().unwrap_or(0)
            {
                anyhow::bail!(
                    "workflow {} run lookup exceeded {MAX_PAGES} pages",
                    workflow.name
                );
            }
            if !latest.is_empty()
                || !more_pages(
                    page,
                    runs.len() as u64,
                    resp["total_count"].as_u64().unwrap_or(0),
                    true,
                )
            {
                return Ok((latest, prs));
            }
        }
        anyhow::bail!(
            "workflow {} run lookup exceeded {MAX_PAGES} pages",
            workflow.name
        )
    }

    async fn discover_runs(
        &self,
        updates: tokio::sync::mpsc::UnboundedSender<ActionsUpdate>,
    ) -> Result<()> {
        let (refresh, known, mut inventory) = {
            let mut cache = self
                .runs_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("runs cache mutex poisoned"))?;
            cache.calls = cache.calls.wrapping_add(1);
            (
                needs_full_scan(&cache.inventory, cache.calls),
                cache.latest.clone(),
                cache.inventory.clone(),
            )
        };
        // Recent runs and inventory start together. Neither waits for the other.
        let recent = async {
            let route = build_runs_route(&self.owner, &self.repo, 1);
            let resp = self.get_json(&route, true).await?;
            resp["workflow_runs"]
                .as_array()
                .context("workflow run listing has no workflow_runs array")?;
            let mut fresh = LatestRuns::new();
            let mut prs = std::collections::HashSet::new();
            parse_workflow_runs(&resp, &mut fresh, &mut prs, self.branch.as_deref());
            self.publish_runs(&updates, fresh.clone(), prs)?;
            Ok::<_, anyhow::Error>((resp, fresh))
        };
        let refresh_inventory = async {
            if refresh {
                match self.fetch_workflow_inventory().await {
                    Ok(entries) => {
                        {
                            let mut cache = self
                                .runs_cache
                                .lock()
                                .map_err(|_| anyhow::anyhow!("runs cache mutex poisoned"))?;
                            cache.reconcile_inventory(&entries);
                        }
                        updates.send(ActionsUpdate::Inventory(entries.clone()))?;
                        Ok(Some(entries))
                    }
                    Err(error) => {
                        Err(error.context("workflow inventory unavailable; using bounded scan"))
                    }
                }
            } else {
                Ok(None)
            }
        };
        let (recent, refreshed) = tokio::join!(recent, refresh_inventory);
        let fresh_inventory = matches!(&refreshed, Ok(Some(_)));
        let inventory_error = match refreshed {
            Ok(Some(entries)) => {
                inventory = Some(entries);
                None
            }
            Ok(None) => None,
            Err(error) => Some(error),
        };
        let (mut resp, fresh) = recent?;
        let mut errors = Vec::new();
        if let Some(error) = inventory_error {
            errors.push(error);
        }
        if let Some(entries) = inventory {
            // Inventory can complete before recent runs. Reconcile again after both
            // responses so delayed runs cannot resurrect a deleted workflow in cache.
            if fresh_inventory {
                self.runs_cache
                    .lock()
                    .map_err(|_| anyhow::anyhow!("runs cache mutex poisoned"))?
                    .reconcile_inventory(&entries);
            }
            let targets: Vec<_> = entries
                .into_iter()
                .filter(|entry| {
                    !fresh.contains_key(&entry.name)
                        && (refresh
                            || !known.contains_key(&entry.name)
                            || known
                                .get(&entry.name)
                                .is_some_and(|(_, status)| *status == BuildStatus::Running))
                })
                .collect();
            let mut scans = stream::iter(
                targets
                    .into_iter()
                    .map(|entry| async move { self.scan_workflow(&entry).await }),
            )
            .buffer_unordered(4);
            while let Some(result) = scans.next().await {
                match result {
                    Ok((fresh, prs)) => self.publish_runs(&updates, fresh, prs)?,
                    Err(error) => errors.push(error),
                }
            }
        } else if refresh || has_stale_running(&known, &fresh) {
            let mut discovered = fresh;
            let mut page = 1;
            while more_pages(
                page,
                resp["workflow_runs"]
                    .as_array()
                    .context("workflow run listing has no workflow_runs array")?
                    .len() as u64,
                resp["total_count"].as_u64().unwrap_or(0),
                true,
            ) {
                page += 1;
                resp = self
                    .get_json(&build_runs_route(&self.owner, &self.repo, page), true)
                    .await?;
                let mut prs = std::collections::HashSet::new();
                parse_workflow_runs(&resp, &mut discovered, &mut prs, self.branch.as_deref());
                self.publish_runs(&updates, discovered.clone(), prs)?;
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(super::ActionsErrors(errors).into())
        }
    }
}

/// Branch is supplied as Octocrab query parameters, which handles URL escaping.
fn build_runs_route(owner: &str, repo: &str, page: u32) -> String {
    format!("/repos/{owner}/{repo}/actions/runs?per_page={RUNS_PER_PAGE}&page={page}")
}

/// Map GitHub pull request `state` + `merged` boolean to WatchedPrState.
/// Recognized shapes: ("open", false) → Open; ("closed", true) → Merged;
/// ("closed", false) → ClosedUnmerged. Anything else → Unknown.
pub fn map_pr_state(state: &str, merged: bool) -> WatchedPrState {
    match (state, merged) {
        ("open", false) => WatchedPrState::Open,
        ("closed", true) => WatchedPrState::Merged,
        ("closed", false) => WatchedPrState::ClosedUnmerged,
        _ => WatchedPrState::Unknown,
    }
}

/// Map GitHub run status + conclusion to BuildStatus.
pub fn map_run_status(status: &str, conclusion: Option<&str>) -> BuildStatus {
    match status {
        "in_progress" | "queued" | "waiting" | "pending" => BuildStatus::Running,
        "completed" => match conclusion {
            Some("success") => BuildStatus::Succeeded,
            Some("skipped") => BuildStatus::Idle,
            Some("failure") | Some("cancelled") | Some("timed_out") => BuildStatus::Failed,
            _ => BuildStatus::Failed,
        },
        _ => BuildStatus::Idle,
    }
}

#[async_trait]
impl ActionsClient for GitHubActionsClient {
    async fn list_latest_runs(&self) -> Result<RunsPage> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let discovery = self.discover_runs(sender);
        let collect = async {
            let mut prs = std::collections::HashSet::new();
            while let Some(update) = receiver.recv().await {
                if let ActionsUpdate::Runs(page) = update {
                    prs.extend(page.pr_numbers);
                }
            }
            prs
        };
        let (result, pr_numbers) = tokio::join!(discovery, collect);
        result?;
        let cache = self
            .runs_cache
            .lock()
            .map_err(|_| anyhow::anyhow!("runs cache mutex poisoned"))?;
        let summaries = cache
            .latest
            .iter()
            .map(|(name, (run_id, status))| WorkflowRunSummary {
                workflow_name: name.clone(),
                run_id: *run_id,
                status: *status,
            })
            .collect();
        Ok(RunsPage {
            summaries,
            pr_numbers,
        })
    }

    async fn stream_latest_runs(
        &self,
        updates: tokio::sync::mpsc::UnboundedSender<ActionsUpdate>,
    ) -> Result<()> {
        self.discover_runs(updates).await
    }

    async fn fetch_run_jobs(&self, run_id: u64) -> Result<Vec<JobInfo>> {
        let mut jobs = Vec::new();
        let mut page = 1_u64;
        loop {
            let route = format!(
                "/repos/{}/{}/actions/runs/{run_id}/jobs?per_page=100&page={page}",
                self.owner, self.repo,
            );
            let resp = self
                .get_json(&route, false)
                .await
                .context("failed to list jobs")?;

            let job_array = resp["jobs"].as_array();
            if let Some(job_array) = job_array {
                for job in job_array {
                    let name = job["name"].as_str().unwrap_or("unknown").to_string();
                    let status = job["status"].as_str().unwrap_or("unknown");
                    let conclusion = job["conclusion"].as_str();
                    jobs.push(JobInfo {
                        name,
                        status: map_run_status(status, conclusion),
                        completed_at: parse_job_completed_at(job),
                    });
                }
            }
            if job_array.map_or(0, Vec::len) < 100
                || page * 100 >= resp["total_count"].as_u64().unwrap_or(0)
            {
                break;
            }
            page += 1;
        }
        Ok(jobs)
    }

    async fn fetch_pr_state(&self, pr_number: u64) -> Result<WatchedPrState> {
        let route = format!("/repos/{}/{}/pulls/{pr_number}", self.owner, self.repo,);
        let resp = self
            .get_json(&route, false)
            .await
            .context("failed to fetch pull request state")?;
        let state = resp["state"].as_str().unwrap_or("");
        let merged = resp["merged"].as_bool().unwrap_or(false);
        Ok(map_pr_state(state, merged))
    }

    async fn fetch_workflow_files(&self) -> Result<Vec<WorkflowFile>> {
        let route = format!(
            "/repos/{}/{}/contents/.github/workflows",
            self.owner, self.repo,
        );
        let listing = self
            .get_json(&route, false)
            .await
            .context("failed to list workflow files")?;

        let files = listing
            .as_array()
            .context("invalid workflow file listing")?;

        let yaml_paths: Vec<String> = files
            .iter()
            .filter_map(|f| {
                let name = f["name"].as_str()?;
                if name.ends_with(".yml") || name.ends_with(".yaml") {
                    f["path"].as_str().map(String::from)
                } else {
                    None
                }
            })
            .collect();

        // Fetch all workflow files concurrently
        let futs = yaml_paths.iter().map(|path| {
            let file_route = format!("/repos/{}/{}/contents/{path}", self.owner, self.repo);
            async move {
                let result = self.get_json(&file_route, false).await;
                (path.as_str(), result)
            }
        });

        let results = futures::future::join_all(futs).await;

        let mut workflow_files = Vec::new();
        for (path, result) in results {
            let file_resp =
                result.with_context(|| format!("failed to fetch workflow file {path}"))?;
            use base64::Engine;
            let encoded = file_resp["content"]
                .as_str()
                .context("missing workflow content")?
                .replace('\n', "");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .context("invalid workflow encoding")?;
            let content = String::from_utf8(bytes).context("invalid workflow UTF-8")?;
            let filename = path.rsplit('/').next().unwrap_or(path);
            let wf = parse_workflow_yaml(filename, &content)
                .with_context(|| format!("invalid workflow YAML {path}"))?;
            if !wf.s3_uploads.is_empty() {
                tracing::info!(
                    workflow = %wf.name,
                    uploads = wf.s3_uploads.len(),
                    "found S3 uploads in workflow"
                );
                workflow_files.push(wf);
            }
        }

        Ok(workflow_files)
    }
}

/// Compiled once, reused across all `extract_s3_paths` calls.
static S3_PATH_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"s3://([^/\s]+)/(\S+)").expect("valid regex"));

/// Regex to collapse `${{ ... }}` GitHub Actions expressions into `GHEXPR`
/// so that spaces inside expressions don't break the S3 URI regex.
static GH_EXPR_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"\$\{\{[^}]*\}\}").expect("valid regex"));

/// Extract S3 paths from shell commands in workflow YAML `run` steps.
/// Looks for `aws s3 cp` and `aws s3 sync` commands with `s3://bucket/key` patterns.
/// Handles `${{ vars.X }}` GitHub Actions expressions by collapsing them first.
pub fn extract_s3_paths(yaml_content: &str) -> Vec<S3Upload> {
    let mut uploads = Vec::new();
    let cleaned = GH_EXPR_RE.replace_all(yaml_content, "GHEXPR");

    for cap in S3_PATH_RE.captures_iter(&cleaned) {
        let bucket = cap[1].to_string();
        let key = cap[2].to_string();
        // Deduplicate
        if !uploads
            .iter()
            .any(|u: &S3Upload| u.bucket == bucket && u.key == key)
        {
            uploads.push(S3Upload { bucket, key });
        }
    }
    uploads
}

/// Parse a GH workflow YAML and extract its name + S3 upload targets.
pub fn parse_workflow_yaml(filename: &str, content: &str) -> Option<WorkflowFile> {
    // serde-saphyr: maintained safe-Rust YAML parser (replaces deprecated
    // serde_yaml). Parsed into serde_json::Value (preserve_order keeps jobs
    // in file order).
    let yaml: serde_json::Value = serde_saphyr::from_str(content).ok()?;
    let name = yaml
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or(filename)
        .to_string();

    let s3_uploads = extract_s3_paths(content);
    let jobs = extract_jobs_s3_info(&yaml);
    Some(WorkflowFile {
        name,
        s3_uploads,
        jobs,
    })
}

/// Extract per-job S3 upload info from a parsed YAML workflow.
/// Traverses `yaml["jobs"]` to extract each job's S3 uploads and `needs:` deps.
pub fn extract_jobs_s3_info(yaml: &serde_json::Value) -> Vec<super::JobS3Info> {
    let jobs_map = match yaml.get("jobs").and_then(|j| j.as_object()) {
        Some(m) => m,
        None => return Vec::new(),
    };

    let mut result = Vec::new();
    for (key, value) in jobs_map {
        let job_id = key.clone();

        let job_name = value
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or(&job_id)
            .to_string();

        // Extract `needs:` — can be a string or a sequence
        let needs = match value.get("needs") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(seq)) => seq
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        };

        // Extract S3 paths from all `run:` steps in this job
        let mut s3_uploads = Vec::new();
        if let Some(steps) = value.get("steps").and_then(|s| s.as_array()) {
            for step in steps {
                if let Some(run_cmd) = step.get("run").and_then(|r| r.as_str()) {
                    for upload in extract_s3_paths(run_cmd) {
                        if !s3_uploads.iter().any(|u: &super::S3Upload| {
                            u.bucket == upload.bucket && u.key == upload.key
                        }) {
                            s3_uploads.push(upload);
                        }
                    }
                }
            }
        }

        result.push(super::JobS3Info {
            job_id,
            job_name,
            s3_uploads,
            needs,
        });
    }
    result
}

/// Extract `completed_at` timestamp from a GitHub Actions job JSON object.
fn parse_job_completed_at(job: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    job["completed_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Parse workflow runs from a JSON response page into the latest-per-workflow map.
/// Skips runs with missing IDs (logs a warning).
/// Returns workflow IDs with eligible runs, for inventory-based scan completion.
/// When `branch_filter` is Some, only runs whose `head_branch` matches are
/// kept in `latest`. PR numbers are harvested into `prs` for every run whose
/// `event` is `pull_request` or `pull_request_target`, regardless of the
/// bot-actor and head_branch filters — downstream code uses these to watch
/// the PR for merge even when the run itself is hidden from the UI.
fn parse_workflow_runs(
    resp: &serde_json::Value,
    latest: &mut std::collections::HashMap<String, (u64, BuildStatus)>,
    prs: &mut std::collections::HashSet<u64>,
    branch_filter: Option<&str>,
) -> std::collections::HashSet<u64> {
    let mut workflow_ids = std::collections::HashSet::new();
    if let Some(runs) = resp["workflow_runs"].as_array() {
        for run in runs {
            let run_id = match run["id"].as_u64() {
                Some(id) => id,
                None => {
                    tracing::warn!(?run, "skipping workflow run with missing or invalid id");
                    continue;
                }
            };

            // Harvest PR numbers BEFORE any filter: Claude-review runs (bot
            // actors) and off-branch PR runs must still contribute here so
            // the linger-after-PR scheduler hook can see them.
            let event = run["event"].as_str().unwrap_or("");
            if event == "pull_request" || event == "pull_request_target" {
                if let Some(arr) = run["pull_requests"].as_array() {
                    for pr in arr {
                        if let Some(n) = pr["number"].as_u64() {
                            prs.insert(n);
                        }
                    }
                }
            }

            // Skip all bot-triggered runs (dependabot, etc.)
            let actor = run["actor"]["login"].as_str().unwrap_or("");
            if actor.ends_with("[bot]") {
                continue;
            }

            // Filter by head_branch when a branch filter is configured.
            // The API &branch= param also returns PR runs targeting the branch,
            // so we need client-side filtering on head_branch.
            if let Some(filter) = branch_filter {
                let head_branch = run["head_branch"].as_str().unwrap_or("");
                if head_branch != filter {
                    continue;
                }
            }

            let status = run["status"].as_str().unwrap_or("unknown");
            let conclusion = run["conclusion"].as_str();

            // Skip completed+skipped runs (e.g. conditional workflows like "Claude Code")
            if status == "completed" && conclusion == Some("skipped") {
                continue;
            }

            let name = run["name"].as_str().unwrap_or("unknown").to_string();
            if let Some(id) = run["workflow_id"].as_u64() {
                workflow_ids.insert(id);
            }
            latest
                .entry(name)
                .or_insert((run_id, map_run_status(status, conclusion)));
        }
    }
    workflow_ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn workflow_files_partial_http_failure_is_not_authoritative() {
        use base64::Engine;
        let good = base64::engine::general_purpose::STANDARD.encode("name: Deploy\njobs:\n  upload:\n    steps:\n      - run: aws s3 cp x s3://bucket/key\n");
        let client = client_with_pages(vec![
            (
                "/repos/owner/repo/contents/.github/workflows&page=1".into(),
                serde_json::json!([
                    {"name":"good.yml", "path":".github/workflows/good.yml"},
                    {"name":"bad.yml", "path":".github/workflows/bad.yml"}
                ]),
            ),
            (
                "/repos/owner/repo/contents/.github/workflows/good.yml&page=1".into(),
                serde_json::json!({"content": good}),
            ),
            (
                "/repos/owner/repo/contents/.github/workflows/bad.yml&page=1".into(),
                serde_json::json!({"_fixture_status":500,"message":"unavailable"}),
            ),
        ])
        .await;
        assert!(client.fetch_workflow_files().await.is_err());
    }

    async fn client_with_pages(pages: Vec<(String, serde_json::Value)>) -> GitHubActionsClient {
        client_with_gated_pages(pages, None).await
    }

    async fn client_with_gated_pages(
        pages: Vec<(String, serde_json::Value)>,
        gate: Option<(String, std::sync::Arc<tokio::sync::Notify>)>,
    ) -> GitHubActionsClient {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pages: std::sync::Arc<HashMap<_, _>> = std::sync::Arc::new(pages.into_iter().collect());
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let pages = pages.clone();
                let gate = gate.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(socket.read_u8().await.unwrap());
                    }
                    let request = String::from_utf8(request).unwrap();
                    let mut route = request.split_whitespace().nth(1).unwrap().to_string();
                    // GitHub defaults to page 1 when the query omits it.
                    if !route.contains("&page=") {
                        route.push_str("&page=1");
                    }
                    if let Some((gated_route, release)) = gate {
                        if route == gated_route {
                            release.notified().await;
                        }
                    }
                    let fixture = pages.get(&route).expect("unexpected API request");
                    let status = fixture["_fixture_status"].as_u64().unwrap_or(200);
                    let body = fixture.to_string();
                    let response = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });

        GitHubActionsClient {
            octocrab: octocrab::Octocrab::builder()
                .base_uri(format!("http://{address}"))
                .unwrap()
                .build()
                .unwrap(),
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            branch: None,
            runs_cache: std::sync::Mutex::new(RunsCache::default()),
        }
    }

    #[tokio::test]
    async fn progressive_empty_inventory_cannot_be_undone_by_late_recent_runs() {
        let route = "/repos/owner/repo/actions/runs?per_page=30&page=1";
        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let client = client_with_gated_pages(
            vec![
                (
                    route.into(),
                    serde_json::json!({"total_count":1,"workflow_runs":[run_fixture(1,"CI")]}),
                ),
                (
                    "/repos/owner/repo/actions/workflows?per_page=100&page=1".into(),
                    serde_json::json!({"total_count":0,"workflows":[]}),
                ),
            ],
            Some((route.into(), gate.clone())),
        )
        .await;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let discovery = client.stream_latest_runs(sender);
        tokio::pin!(discovery);
        tokio::select! {
            _ = &mut discovery => panic!("recent response held"),
            update = receiver.recv() => assert!(matches!(update, Some(ActionsUpdate::Inventory(entries)) if entries.is_empty())),
        }
        gate.notify_one();
        discovery.await.unwrap();
        assert!(
            client.runs_cache.lock().unwrap().latest.is_empty(),
            "authoritative empty inventory must remain empty"
        );
        gate.notify_one();
        assert!(
            client
                .list_latest_runs()
                .await
                .unwrap()
                .summaries
                .is_empty(),
            "ordinary polls must retain known deletion"
        );
    }

    #[tokio::test]
    async fn progressive_recent_new_workflow_survives_cached_inventory() {
        let client = client_with_pages(vec![(
            "/repos/owner/repo/actions/runs?per_page=30&page=1".into(),
            serde_json::json!({"total_count":2,"workflow_runs":[run_fixture(10,"CI"),run_fixture(9,"Nightly")]}),
        )]).await;
        client.runs_cache.lock().unwrap().inventory = Some(vec![WorkflowInventoryEntry {
            id: 1,
            name: "CI".into(),
        }]);
        let page = client.list_latest_runs().await.unwrap();
        assert!(
            page.summaries.iter().any(|s| s.workflow_name == "Nightly"),
            "cached inventory must not discard a new recent workflow"
        );
        assert_eq!(client.runs_cache.lock().unwrap().latest["Nightly"].0, 9);
    }

    #[tokio::test]
    async fn progressive_rate_limit_classification_uses_http_status() {
        let client = client_with_pages(vec![
            (
                "/missing429000&page=1".into(),
                serde_json::json!({"_fixture_status":404,"message":"resource missing"}),
            ),
            (
                "/blocked&page=1".into(),
                serde_json::json!({"_fixture_status":403,"message":"blocked"}),
            ),
        ])
        .await;
        let missing = client
            .get_json("/missing429000", false)
            .await
            .err()
            .unwrap();
        assert!(!super::super::actions_rate_limited(&missing));
        let blocked = client.get_json("/blocked", false).await.err().unwrap();
        assert!(super::super::actions_rate_limited(&blocked));
        let aggregate: anyhow::Error = super::super::ActionsErrors(vec![missing, blocked]).into();
        assert!(super::super::actions_rate_limited(&aggregate));
    }

    #[tokio::test]
    async fn progressive_recent_jobs_survive_late_targeted_error() {
        let target = "/repos/owner/repo/actions/workflows/2/runs?per_page=30&page=1";
        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let client = client_with_gated_pages(vec![
            ("/repos/owner/repo/actions/runs?per_page=30&page=1".into(), serde_json::json!({"total_count":1,"workflow_runs":[run_fixture(1,"CI")]})),
            ("/repos/owner/repo/actions/workflows?per_page=100&page=1".into(), serde_json::json!({"total_count":2,"workflows":[{"id":1,"name":"CI"},{"id":2,"name":"Nightly"}]})),
            ("/repos/owner/repo/actions/runs/1/jobs?per_page=100&page=1".into(), serde_json::json!({"total_count":1,"jobs":[{"name":"build","status":"completed","conclusion":"success"}]})),
            (target.into(), serde_json::json!({"message":"late error"})),
        ], Some((target.into(), gate.clone()))).await;
        let app = std::sync::Arc::new(std::sync::Mutex::new(crate::app::App::new()));
        let tick = super::super::poll_actions_tick(&app, &client);
        tokio::pin!(tick);
        let jobs_ready = async {
            loop {
                if app
                    .lock()
                    .unwrap()
                    .workflow_groups
                    .iter()
                    .any(|g| g.name == "CI" && !g.jobs.is_empty())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        tokio::select! {
            _ = &mut tick => panic!("history held"),
            ready = tokio::time::timeout(std::time::Duration::from_secs(3), jobs_ready) => ready.unwrap(),
        }
        assert_eq!(
            app.lock()
                .unwrap()
                .workflow_groups
                .iter()
                .find(|g| g.name == "CI")
                .unwrap()
                .jobs
                .len(),
            1
        );
        gate.notify_one();
        tick.await;
        assert_eq!(client.runs_cache.lock().unwrap().latest["CI"].0, 1);
        assert!(app
            .lock()
            .unwrap()
            .warnings
            .iter()
            .any(|w| w.contains("no workflow_runs array")));
    }

    #[tokio::test]
    async fn progressive_http_request_timeout_is_descriptive() {
        let route = "/repos/owner/repo/actions/runs?per_page=30&page=1";
        let client = client_with_gated_pages(
            vec![(route.into(), serde_json::json!({}))],
            Some((
                route.into(),
                std::sync::Arc::new(tokio::sync::Notify::new()),
            )),
        )
        .await;
        let error = client.get_json(route, false).await.err().unwrap();
        assert!(error.to_string().contains("timed out after 15s"));
        assert!(error.to_string().contains(route));
    }

    #[tokio::test]
    async fn progressive_targeted_filtered_page_keeps_prs_and_finds_eligible_run() {
        let mut bot = run_fixture(60, "Nightly");
        bot["actor"]["login"] = serde_json::json!("renovate[bot]");
        bot["event"] = serde_json::json!("pull_request_target");
        bot["pull_requests"] = serde_json::json!([{"number":42}]);
        let client = client_with_pages(vec![
            ("/repos/owner/repo/actions/runs?per_page=30&page=1".into(), serde_json::json!({"total_count":1,"workflow_runs":[run_fixture(90,"CI")]})),
            ("/repos/owner/repo/actions/workflows?per_page=100&page=1".into(), serde_json::json!({"total_count":3,"workflows":[{"id":1,"name":"CI"},{"id":2,"name":"Nightly"},{"id":3,"name":"Never"}]})),
            ("/repos/owner/repo/actions/workflows/2/runs?per_page=30&page=1".into(), serde_json::json!({"total_count":31,"workflow_runs":vec![bot;30]})),
            ("/repos/owner/repo/actions/workflows/2/runs?per_page=30&page=2".into(), serde_json::json!({"total_count":31,"workflow_runs":[run_fixture(30,"Nightly")]})),
            ("/repos/owner/repo/actions/workflows/3/runs?per_page=30&page=1".into(), serde_json::json!({"total_count":0,"workflow_runs":[]})),
        ]).await;
        let page = client.list_latest_runs().await.unwrap();
        assert!(page.pr_numbers.contains(&42));
        assert!(page
            .summaries
            .iter()
            .any(|s| s.workflow_name == "Nightly" && s.run_id == 30));
        assert_eq!(page.summaries.len(), 2);
    }

    fn run_fixture(id: u64, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "name": name, "status": "completed", "conclusion": "success",
            "actor": {"login": "alice"}, "head_branch": "master", "event": "push",
            "pull_requests": [], "workflow_id": if name == "CI" { 1 } else { 2 },
        })
    }

    fn sparse_run_pages() -> Vec<(String, serde_json::Value)> {
        // Newest first: 60 CI runs hide the latest Nightly run on page 3.
        let mut pages: Vec<_> = (1..=3)
            .map(|page| {
                let runs = if page < 3 {
                    (0..30)
                        .map(|offset| run_fixture(90 - (page - 1) * 30 - offset, "CI"))
                        .collect::<Vec<_>>()
                } else {
                    vec![run_fixture(30, "Nightly")]
                };
                (
                    format!("/repos/owner/repo/actions/runs?per_page=30&page={page}"),
                    serde_json::json!({"total_count": 61, "workflow_runs": runs}),
                )
            })
            .collect();
        pages.push((
            "/repos/owner/repo/actions/workflows?per_page=100&page=1".to_string(),
            serde_json::json!({"total_count": 2, "workflows": [
                {"id": 1, "name": "CI", "state": "active"},
                {"id": 2, "name": "Nightly", "state": "active"},
            ]}),
        ));
        pages.push((
            "/repos/owner/repo/actions/workflows/1/runs?per_page=30&page=1".into(),
            serde_json::json!({"total_count": 1, "workflow_runs": [run_fixture(60, "CI")]}),
        ));
        pages.push((
            "/repos/owner/repo/actions/workflows/2/runs?per_page=30&page=1".into(),
            serde_json::json!({"total_count": 1, "workflow_runs": [run_fixture(30, "Nightly")]}),
        ));
        pages
    }

    async fn assert_sparse_workflow_found(client: &GitHubActionsClient) {
        let result = client.list_latest_runs().await.unwrap();
        let latest: LatestRuns = result
            .summaries
            .into_iter()
            .map(|run| (run.workflow_name, (run.run_id, run.status)))
            .collect();
        assert_eq!(latest.get("Nightly"), Some(&(30, BuildStatus::Succeeded)));
        assert_eq!(latest.get("CI"), Some(&(90, BuildStatus::Succeeded)));
        assert_eq!(latest.len(), 2);
    }

    #[tokio::test]
    async fn pagination_initial_scan_reaches_sparse_workflow() {
        let client = client_with_pages(sparse_run_pages()).await;
        assert_sparse_workflow_found(&client).await;
    }

    #[tokio::test]
    async fn pagination_periodic_scan_retains_sparse_workflow() {
        let client = client_with_pages(sparse_run_pages()).await;
        *client.runs_cache.lock().unwrap() = RunsCache {
            latest: known(&[
                ("CI", 89, BuildStatus::Succeeded),
                ("Nightly", 30, BuildStatus::Succeeded),
                ("Removed", 1, BuildStatus::Succeeded),
            ]),
            calls: FULL_SCAN_EVERY - 1,
            inventory: None,
            removed: Default::default(),
        };
        assert_sparse_workflow_found(&client).await;
    }

    #[tokio::test]
    async fn pagination_stale_running_scan_reaches_sparse_workflow() {
        let client = client_with_pages(sparse_run_pages()).await;
        client.runs_cache.lock().unwrap().latest = known(&[
            ("CI", 89, BuildStatus::Succeeded),
            ("Nightly", 30, BuildStatus::Running),
        ]);
        assert_sparse_workflow_found(&client).await;
    }

    #[tokio::test]
    async fn pagination_filtered_runs_do_not_cover_workflow_inventory() {
        let mut pages = sparse_run_pages();
        for run in pages[0].1["workflow_runs"].as_array_mut().unwrap() {
            run["actor"]["login"] = serde_json::json!("renovate[bot]");
        }
        pages[3].1 = serde_json::json!({"total_count": 1, "workflows": [
            {"id": 1, "name": "CI", "state": "active"},
        ]});
        let client = client_with_pages(pages).await;
        let result = client.list_latest_runs().await.unwrap();
        let ci = result
            .summaries
            .iter()
            .find(|run| run.workflow_name == "CI")
            .unwrap();
        assert_eq!(ci.run_id, 60);
        assert_eq!(result.summaries.len(), 1);
    }

    #[tokio::test]
    async fn pagination_incremental_poll_needs_only_first_runs_page() {
        let client = client_with_pages(sparse_run_pages().into_iter().take(1).collect()).await;
        client.runs_cache.lock().unwrap().latest = known(&[
            ("CI", 89, BuildStatus::Succeeded),
            ("Nightly", 30, BuildStatus::Succeeded),
        ]);
        client.runs_cache.lock().unwrap().inventory = Some(vec![
            WorkflowInventoryEntry {
                id: 1,
                name: "CI".into(),
            },
            WorkflowInventoryEntry {
                id: 2,
                name: "Nightly".into(),
            },
        ]);
        assert_sparse_workflow_found(&client).await;
    }

    #[tokio::test]
    async fn pagination_unavailable_inventory_falls_back_to_bounded_scan() {
        let mut pages = sparse_run_pages();
        pages[3].1 = serde_json::json!({"message": "unavailable"});
        let client = client_with_pages(pages).await;
        assert!(client
            .list_latest_runs()
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("inventory unavailable"));
        let cache = client.runs_cache.lock().unwrap();
        assert_eq!(
            cache.latest.get("Nightly"),
            Some(&(30, BuildStatus::Succeeded))
        );
        assert_eq!(cache.latest.get("CI"), Some(&(90, BuildStatus::Succeeded)));
    }

    #[tokio::test]
    async fn pagination_unseen_workflows_cannot_exceed_run_page_cap() {
        let mut pages = sparse_run_pages();
        let inventory = pages.remove(3);
        let mut pages: Vec<_> = (1..=MAX_PAGES)
            .map(|page| {
                let runs: Vec<_> = (0..30)
                    .map(|offset| run_fixture(u64::from(9000 - page * 30 - offset), "CI"))
                    .collect();
                (
                    format!("/repos/owner/repo/actions/runs?per_page=30&page={page}"),
                    serde_json::json!({"total_count": 9000, "workflow_runs": runs}),
                )
            })
            .collect();
        pages.push(inventory);
        for page in 1..=MAX_PAGES {
            let mut bot_run = run_fixture(9000 - u64::from(page), "Nightly");
            bot_run["actor"]["login"] = serde_json::json!("renovate[bot]");
            pages.push((
                format!("/repos/owner/repo/actions/workflows/2/runs?per_page=30&page={page}"),
                serde_json::json!({"total_count":9000,"workflow_runs":vec![bot_run;30]}),
            ));
        }
        let client = client_with_pages(pages).await;

        // Nightly has no eligible run in the retained 1800-run lookback.
        let error = client.list_latest_runs().await.err().unwrap();
        assert!(error.to_string().contains("exceeded 60 pages"));
        let cache = client.runs_cache.lock().unwrap();
        assert_eq!(cache.latest.len(), 1);
        assert_eq!(cache.latest["CI"].0, 8970);
    }

    #[tokio::test]
    async fn pagination_full_scan_stops_when_inventory_is_covered() {
        let mut runs: Vec<_> = (0..30).map(|i| run_fixture(90 - i, "CI")).collect();
        runs[1] = run_fixture(89, "Nightly");
        let client = client_with_pages(vec![
            (
                "/repos/owner/repo/actions/runs?per_page=30&page=1".to_string(),
                serde_json::json!({"total_count": 9000, "workflow_runs": runs}),
            ),
            (
                "/repos/owner/repo/actions/runs?per_page=30&page=2".to_string(),
                serde_json::json!({"total_count": 9000, "workflow_runs": [
                    run_fixture(1, "Legacy"),
                ]}),
            ),
            (
                "/repos/owner/repo/actions/workflows?per_page=100&page=1".to_string(),
                serde_json::json!({"total_count": 2, "workflows": [
                    {"id": 1, "name": "CI", "state": "active"},
                    {"id": 2, "name": "Nightly", "state": "active"},
                ]}),
            ),
        ])
        .await;

        let result = client.list_latest_runs().await.unwrap();
        assert_eq!(result.summaries.len(), 2);
    }

    #[tokio::test]
    async fn pagination_inventory_includes_workflows_after_first_hundred() {
        let mut pages = Vec::new();
        for page in 1..=5 {
            let runs: Vec<_> = (0..30)
                .map(|offset| {
                    let workflow_id = if page == 5 {
                        101
                    } else {
                        ((page - 1) * 30 + offset + 1).min(100)
                    };
                    let mut run = run_fixture(1000 - (page - 1) * 30 - offset, "CI");
                    run["name"] = serde_json::json!(format!("workflow-{workflow_id}"));
                    run["workflow_id"] = serde_json::json!(workflow_id);
                    run
                })
                .collect();
            pages.push((
                format!("/repos/owner/repo/actions/runs?per_page=30&page={page}"),
                serde_json::json!({"total_count": 9000, "workflow_runs": runs}),
            ));
        }
        for page in 1..=2 {
            let workflows: Vec<_> = if page == 1 { 1..=100 } else { 101..=101 }
                .map(|id| {
                    serde_json::json!({
                        "id": id, "name": format!("workflow-{id}"), "state": "active",
                    })
                })
                .collect();
            pages.push((
                format!("/repos/owner/repo/actions/workflows?per_page=100&page={page}"),
                serde_json::json!({"total_count": 101, "workflows": workflows}),
            ));
        }
        for id in 31..=101 {
            let mut run = run_fixture(1000 - id, "CI");
            run["name"] = serde_json::json!(format!("workflow-{id}"));
            run["workflow_id"] = serde_json::json!(id);
            pages.push((
                format!("/repos/owner/repo/actions/workflows/{id}/runs?per_page=30&page=1"),
                serde_json::json!({"total_count":1,"workflow_runs":[run]}),
            ));
        }
        pages.push((
            "/repos/owner/repo/actions/runs?per_page=30&page=6".to_string(),
            serde_json::json!({"total_count": 9000, "workflow_runs": [
                run_fixture(1, "Legacy"),
            ]}),
        ));
        let client = client_with_pages(pages).await;
        let result = client.list_latest_runs().await.unwrap();
        assert_eq!(result.summaries.len(), 101);
        assert!(result
            .summaries
            .iter()
            .any(|run| run.workflow_name == "workflow-101"));
    }

    #[tokio::test]
    async fn pagination_jobs_includes_failed_job_after_first_hundred() {
        let first_jobs: Vec<_> = (1..=100)
            .map(|id| {
                serde_json::json!({
                    "id": id, "run_id": 42, "name": format!("matrix-{id}"),
                    "status": "completed", "conclusion": "success",
                    "started_at": "2026-11-03T10:00:00Z",
                    "completed_at": "2026-11-03T10:05:00Z", "steps": [],
                })
            })
            .collect();
        let client = client_with_pages(vec![
            (
                "/repos/owner/repo/actions/runs/42/jobs?per_page=100&page=1".to_string(),
                serde_json::json!({"total_count": 101, "jobs": first_jobs}),
            ),
            (
                "/repos/owner/repo/actions/runs/42/jobs?per_page=100&page=2".to_string(),
                serde_json::json!({"total_count": 101, "jobs": [{
                    "id": 101, "run_id": 42, "name": "deploy", "status": "completed",
                    "conclusion": "failure", "started_at": "2026-11-03T10:05:00Z",
                    "completed_at": "2026-11-03T10:06:00Z", "steps": [],
                }]}),
            ),
        ])
        .await;

        let jobs = client.fetch_run_jobs(42).await.unwrap();
        assert_eq!(jobs.len(), 101);
        assert_eq!(jobs[0].name, "matrix-1");
        assert_eq!(jobs[0].status, BuildStatus::Succeeded);
        assert_eq!(jobs[100].name, "deploy");
        assert_eq!(jobs[100].status, BuildStatus::Failed);
        assert_eq!(
            jobs[100].completed_at,
            Some("2026-11-03T10:06:00Z".parse().unwrap())
        );
    }

    // --- build_runs_route tests ---

    // --- incremental run listing ---

    fn known(entries: &[(&str, u64, BuildStatus)]) -> HashMap<String, (u64, BuildStatus)> {
        entries
            .iter()
            .map(|(n, id, st)| (n.to_string(), (*id, *st)))
            .collect()
    }

    #[test]
    fn full_scan_on_first_call() {
        assert!(needs_full_scan::<Vec<WorkflowInventoryEntry>>(&None, 1));
    }

    #[test]
    fn page_one_only_when_cache_warm() {
        let k = known(&[("CI", 1, BuildStatus::Succeeded)]);
        assert!(!needs_full_scan(&Some(k), 1));
    }

    #[test]
    fn known_empty_inventory_keeps_twentieth_call_refresh_cadence() {
        let inventory: Option<Vec<WorkflowInventoryEntry>> = Some(vec![]);
        for call in 1..FULL_SCAN_EVERY {
            assert!(!needs_full_scan(&inventory, call));
        }
        assert!(needs_full_scan(&inventory, FULL_SCAN_EVERY));
    }

    #[test]
    fn periodic_full_rescan() {
        let k = known(&[("CI", 1, BuildStatus::Succeeded)]);
        assert!(needs_full_scan(&Some(&k), FULL_SCAN_EVERY));
        assert!(needs_full_scan(&Some(&k), 2 * FULL_SCAN_EVERY));
    }

    #[test]
    fn running_cached_run_does_not_force_full_scan() {
        let k = known(&[("CI", 1, BuildStatus::Running)]);
        assert!(!needs_full_scan(&Some(k), 1));
    }

    #[test]
    fn stale_running_when_cached_running_missing_from_page() {
        // a running run must not freeze in the cache: keep paging until seen
        let k = known(&[
            ("CI", 1, BuildStatus::Succeeded),
            ("Deploy", 2, BuildStatus::Running),
        ]);
        assert!(has_stale_running(
            &k,
            &known(&[("CI", 3, BuildStatus::Running)])
        ));
        assert!(!has_stale_running(
            &k,
            &known(&[("Deploy", 2, BuildStatus::Succeeded)])
        ));
        let idle = known(&[("CI", 1, BuildStatus::Succeeded)]);
        assert!(!has_stale_running(&idle, &HashMap::new()));
    }

    #[test]
    fn parse_collects_only_eligible_workflow_ids() {
        let resp = serde_json::json!({
            "total_count": 3,
            "workflow_runs": [
                {"id": 1, "workflow_id": 1, "name": "CI", "event": "push", "status": "completed",
                 "conclusion": "success", "actor": {"login": "renovate[bot]"}},
                {"id": 2, "workflow_id": 2, "name": "CI", "event": "push", "status": "completed",
                 "conclusion": "success", "actor": {"login": "glnds"}},
                {"id": 3, "workflow_id": 3, "name": "Claude", "event": "push", "status": "completed",
                 "conclusion": "skipped", "actor": {"login": "glnds"}},
            ]
        });
        let mut latest = HashMap::new();
        let mut prs = std::collections::HashSet::new();
        assert_eq!(
            parse_workflow_runs(&resp, &mut latest, &mut prs, None),
            std::collections::HashSet::from([2])
        );
    }

    #[test]
    fn more_pages_rules() {
        let full = |page, fetched, total| more_pages(page, fetched, total, true);
        assert!(full(1, RUNS_PER_PAGE as u64, 500));
        assert!(!full(1, 10, 500), "short page = last page");
        assert!(
            !full(1, RUNS_PER_PAGE as u64, RUNS_PER_PAGE as u64),
            "total reached"
        );
        assert!(!full(MAX_PAGES, RUNS_PER_PAGE as u64, 5000), "page cap");
        assert!(
            !more_pages(1, RUNS_PER_PAGE as u64, 500, false),
            "incremental = page 1"
        );
    }

    #[test]
    fn merge_keeps_unseen_and_overwrites_seen() {
        let mut k = known(&[
            ("CI", 1, BuildStatus::Succeeded),
            ("Nightly", 5, BuildStatus::Failed),
        ]);
        let fresh = known(&[("CI", 9, BuildStatus::Running)]);
        merge_latest(&mut k, fresh);
        assert_eq!(k.get("CI"), Some(&(9, BuildStatus::Running)));
        assert_eq!(k.get("Nightly"), Some(&(5, BuildStatus::Failed)));
    }

    #[test]
    fn progressive_merge_rejects_delayed_older_run() {
        let mut cache = known(&[("CI", 20, BuildStatus::Succeeded)]);
        merge_latest(&mut cache, known(&[("CI", 10, BuildStatus::Running)]));
        assert_eq!(cache["CI"], (20, BuildStatus::Succeeded));
    }

    #[test]
    fn partial_scan_keeps_cache() {
        let mut k = known(&[("Old", 1, BuildStatus::Succeeded)]);
        merge_latest(&mut k, known(&[("CI", 2, BuildStatus::Succeeded)]));
        assert!(k.contains_key("Old"));
        assert!(k.contains_key("CI"));
    }

    #[test]
    fn build_runs_route_no_branch() {
        let route = build_runs_route("owner", "repo", 1);
        assert_eq!(route, "/repos/owner/repo/actions/runs?per_page=30&page=1");
    }

    #[tokio::test]
    async fn progressive_branch_query_escapes_reserved_characters() {
        let mut client = client_with_pages(vec![("/repos/owner/repo/actions/runs?per_page=30&page=1&branch=feature%2Fa%26status%3Dcompleted+%23%3F".into(), serde_json::json!({"total_count":0, "workflow_runs":[]}))]).await;
        client.branch = Some("feature/a&status=completed #?".into());
        client
            .get_json(&build_runs_route("owner", "repo", 1), true)
            .await
            .unwrap();
    }

    #[test]
    fn build_runs_route_page_2() {
        let route = build_runs_route("o", "r", 2);
        assert_eq!(route, "/repos/o/r/actions/runs?per_page=30&page=2");
    }

    #[test]
    fn parse_skips_run_with_missing_id() {
        let resp = serde_json::json!({
            "total_count": 2,
            "workflow_runs": [
                {"name": "CI", "status": "completed", "conclusion": "success"},
                {"name": "Deploy", "id": 42, "status": "completed", "conclusion": "success"}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert_eq!(latest.len(), 1);
        assert!(latest.contains_key("Deploy"));
        assert!(!latest.contains_key("CI"));
    }

    #[test]
    fn parse_skips_run_with_null_id() {
        let resp = serde_json::json!({
            "total_count": 1,
            "workflow_runs": [
                {"name": "CI", "id": null, "status": "in_progress"}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert!(latest.is_empty());
    }

    #[test]
    fn parse_keeps_first_run_per_workflow() {
        let resp = serde_json::json!({
            "total_count": 2,
            "workflow_runs": [
                {"name": "CI", "id": 10, "status": "completed", "conclusion": "success"},
                {"name": "CI", "id": 9, "status": "completed", "conclusion": "failure"}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert_eq!(latest.len(), 1);
        assert_eq!(latest["CI"].0, 10);
        assert_eq!(latest["CI"].1, BuildStatus::Succeeded);
    }

    #[test]
    fn maps_in_progress() {
        assert_eq!(map_run_status("in_progress", None), BuildStatus::Running);
    }

    #[test]
    fn maps_queued() {
        assert_eq!(map_run_status("queued", None), BuildStatus::Running);
    }

    #[test]
    fn maps_completed_success() {
        assert_eq!(
            map_run_status("completed", Some("success")),
            BuildStatus::Succeeded
        );
    }

    #[test]
    fn maps_completed_failure() {
        assert_eq!(
            map_run_status("completed", Some("failure")),
            BuildStatus::Failed
        );
    }

    #[test]
    fn maps_completed_cancelled() {
        assert_eq!(
            map_run_status("completed", Some("cancelled")),
            BuildStatus::Failed
        );
    }

    #[test]
    fn maps_completed_timed_out() {
        assert_eq!(
            map_run_status("completed", Some("timed_out")),
            BuildStatus::Failed
        );
    }

    #[test]
    fn maps_completed_no_conclusion() {
        assert_eq!(map_run_status("completed", None), BuildStatus::Failed);
    }

    #[test]
    fn maps_unknown_to_idle() {
        assert_eq!(map_run_status("unknown", None), BuildStatus::Idle);
    }

    // --- extract_s3_paths tests ---

    #[test]
    fn extract_s3_cp_command() {
        let yaml = "run: aws s3 cp dist.zip s3://my-bucket/my-app/dist.zip";
        let uploads = extract_s3_paths(yaml);
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].bucket, "my-bucket");
        assert_eq!(uploads[0].key, "my-app/dist.zip");
    }

    #[test]
    fn extract_s3_sync_command() {
        let yaml = "run: aws s3 sync ./build s3://deploy-bucket/frontend/";
        let uploads = extract_s3_paths(yaml);
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].bucket, "deploy-bucket");
        assert_eq!(uploads[0].key, "frontend/");
    }

    #[test]
    fn extract_multiple_s3_paths() {
        let yaml = r#"
        run: |
          aws s3 cp a.zip s3://bucket-a/key-a.zip
          aws s3 cp b.zip s3://bucket-b/key-b.zip
        "#;
        let uploads = extract_s3_paths(yaml);
        assert_eq!(uploads.len(), 2);
    }

    #[test]
    fn extract_deduplicates() {
        let yaml = r#"
          aws s3 cp a.zip s3://bucket/key.zip
          aws s3 cp a.zip s3://bucket/key.zip
        "#;
        let uploads = extract_s3_paths(yaml);
        assert_eq!(uploads.len(), 1);
    }

    #[test]
    fn extract_no_s3_paths() {
        let yaml = "run: echo hello";
        let uploads = extract_s3_paths(yaml);
        assert!(uploads.is_empty());
    }

    #[test]
    fn extract_s3_with_env_var_in_bucket() {
        let yaml = "run: aws s3 cp dist.zip s3://${AWS_ACCOUNT_ID}-deploy/app.zip";
        let uploads = extract_s3_paths(yaml);
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].bucket, "${AWS_ACCOUNT_ID}-deploy");
    }

    #[test]
    fn extract_s3_with_github_actions_expression_in_bucket() {
        let yaml =
            "run: aws s3 cp dist.zip s3://attracr-ci-artifacts-${{ vars.AWS_ACCOUNT_ID }}/frontend/frontend-artifacts.zip";
        let uploads = extract_s3_paths(yaml);
        assert_eq!(uploads.len(), 1, "should match despite ${{}} spaces");
        assert_eq!(uploads[0].key, "frontend/frontend-artifacts.zip");
    }

    #[test]
    fn extract_s3_with_multiple_github_actions_expressions() {
        let yaml = "run: aws s3 cp app.zip s3://${{ vars.BUCKET }}-${{ vars.ENV }}/deploy/app.zip";
        let uploads = extract_s3_paths(yaml);
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].key, "deploy/app.zip");
    }

    // --- parse_workflow_yaml tests ---

    #[test]
    fn parse_yaml_with_name_and_s3() {
        let yaml = r#"
name: Deploy Frontend
on: push
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - run: aws s3 cp dist.zip s3://my-bucket/frontend/dist.zip
"#;
        let wf = parse_workflow_yaml("deploy.yml", yaml).unwrap();
        assert_eq!(wf.name, "Deploy Frontend");
        assert_eq!(wf.s3_uploads.len(), 1);
        assert_eq!(wf.s3_uploads[0].bucket, "my-bucket");
        assert_eq!(wf.s3_uploads[0].key, "frontend/dist.zip");
    }

    #[test]
    fn parse_yaml_no_name_uses_filename() {
        let yaml = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: aws s3 cp out.zip s3://bucket/key.zip
"#;
        let wf = parse_workflow_yaml("ci.yml", yaml).unwrap();
        assert_eq!(wf.name, "ci.yml");
    }

    #[test]
    fn parse_yaml_no_s3() {
        let yaml = r#"
name: CI
on: push
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test
"#;
        let wf = parse_workflow_yaml("ci.yml", yaml).unwrap();
        assert!(wf.s3_uploads.is_empty());
    }

    #[test]
    fn parse_yaml_tolerates_non_string_keys() {
        let yaml = r#"
name: Matrix
on: push
jobs:
  test:
    runs-on: ubuntu-latest
    env:
      1: one
      true: yes
      3.11: py
    steps:
      - run: aws s3 cp a.zip s3://bucket/a.zip
"#;
        let wf = parse_workflow_yaml("m.yml", yaml).unwrap();
        assert_eq!(wf.name, "Matrix");
        assert_eq!(wf.jobs.len(), 1);
    }

    #[test]
    fn parse_invalid_yaml_returns_none() {
        let result = parse_workflow_yaml("bad.yml", "\t\t---\n\t bad:\n\t\t\t[[[unterminated");
        assert!(result.is_none());
    }

    #[test]
    fn maps_waiting_to_running() {
        assert_eq!(map_run_status("waiting", None), BuildStatus::Running);
    }

    #[test]
    fn maps_pending_to_running() {
        assert_eq!(map_run_status("pending", None), BuildStatus::Running);
    }

    #[test]
    fn maps_completed_skipped_to_idle() {
        assert_eq!(
            map_run_status("completed", Some("skipped")),
            BuildStatus::Idle
        );
    }

    #[test]
    fn parse_excludes_skipped_run() {
        let resp = serde_json::json!({
            "workflow_runs": [
                {"name": "Claude Code", "id": 100, "status": "completed", "conclusion": "skipped"},
                {"name": "CI", "id": 101, "status": "completed", "conclusion": "success"}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert_eq!(latest.len(), 1);
        assert!(latest.contains_key("CI"));
        assert!(!latest.contains_key("Claude Code"));
    }

    #[test]
    fn parse_skips_skipped_keeps_next_non_skipped() {
        let resp = serde_json::json!({
            "workflow_runs": [
                {"name": "CI", "id": 50, "status": "completed", "conclusion": "skipped"},
                {"name": "CI", "id": 49, "status": "completed", "conclusion": "success"}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert_eq!(latest.len(), 1);
        assert_eq!(latest["CI"].0, 49);
        assert_eq!(latest["CI"].1, BuildStatus::Succeeded);
    }

    #[test]
    fn parse_all_skipped_produces_empty_map() {
        let resp = serde_json::json!({
            "workflow_runs": [
                {"name": "Claude Code", "id": 10, "status": "completed", "conclusion": "skipped"},
                {"name": "Claude Code", "id": 9, "status": "completed", "conclusion": "skipped"}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert!(latest.is_empty());
    }

    #[test]
    fn parse_filters_by_head_branch() {
        let resp = serde_json::json!({
            "workflow_runs": [
                {"name": "CI", "id": 10, "status": "completed", "conclusion": "success", "head_branch": "master", "actor": {"login": "glnds"}},
                {"name": "CI", "id": 9, "status": "completed", "conclusion": "failure", "head_branch": "feature/foo", "actor": {"login": "glnds"}}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            Some("master"),
        );
        assert_eq!(latest.len(), 1);
        assert_eq!(latest["CI"].0, 10);
    }

    #[test]
    fn parse_skips_bot_actors() {
        let resp = serde_json::json!({
            "workflow_runs": [
                {"name": "CI", "id": 10, "status": "completed", "conclusion": "success", "head_branch": "master", "actor": {"login": "glnds"}},
                {"name": "pip in /backend - Update #123", "id": 9, "status": "completed", "conclusion": "success", "head_branch": "master", "actor": {"login": "dependabot[bot]"}},
                {"name": "CI", "id": 8, "status": "completed", "conclusion": "failure", "head_branch": "dependabot/npm/lodash", "actor": {"login": "dependabot[bot]"}}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert_eq!(latest.len(), 1);
        assert!(latest.contains_key("CI"));
    }

    #[test]
    fn parse_keeps_human_actors() {
        let resp = serde_json::json!({
            "workflow_runs": [
                {"name": "CI", "id": 10, "status": "completed", "conclusion": "success", "head_branch": "master", "actor": {"login": "glnds"}},
                {"name": "Deploy", "id": 9, "status": "completed", "conclusion": "success", "head_branch": "feature/foo", "actor": {"login": "coworker"}}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert_eq!(latest.len(), 2);
    }

    // --- Issue 2: early-exit stale page detection ---

    #[test]
    fn parse_workflow_runs_returns_no_new_on_stale_page() {
        // First page: discover "CI" workflow
        let page1 = serde_json::json!({
            "workflow_runs": [
                {"name": "CI", "id": 10, "status": "completed", "conclusion": "success"}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &page1,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert_eq!(latest.len(), 1);

        // Second page: only has older "CI" run — no new workflows added
        let page2 = serde_json::json!({
            "workflow_runs": [
                {"name": "CI", "id": 5, "status": "completed", "conclusion": "failure"}
            ]
        });
        let prev_count = latest.len();
        parse_workflow_runs(
            &page2,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        let new_count = latest.len();
        // Stale page: no new workflow names discovered
        assert_eq!(prev_count, new_count);
    }

    #[test]
    fn parse_workflow_runs_detects_new_on_fresh_page() {
        let page1 = serde_json::json!({
            "workflow_runs": [
                {"name": "CI", "id": 10, "status": "completed", "conclusion": "success"}
            ]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &page1,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );

        // Second page has a new workflow "Deploy"
        let page2 = serde_json::json!({
            "workflow_runs": [
                {"name": "Deploy", "id": 8, "status": "completed", "conclusion": "success"}
            ]
        });
        let prev_count = latest.len();
        parse_workflow_runs(
            &page2,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        let new_count = latest.len();
        // Fresh page: new workflow discovered
        assert!(new_count > prev_count);
    }

    #[test]
    fn parse_run_missing_name_defaults_to_unknown() {
        let resp = serde_json::json!({
            "workflow_runs": [{"id": 1, "status": "completed", "conclusion": "success"}]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(
            &resp,
            &mut latest,
            &mut std::collections::HashSet::new(),
            None,
        );
        assert!(latest.contains_key("unknown"));
    }

    // --- extract_jobs_s3_info tests ---

    #[test]
    fn extract_jobs_s3_info_multi_job_workflow() {
        let yaml = r#"
name: CI
on: push
jobs:
  validate-cfn:
    name: Validate CloudFormation
    runs-on: ubuntu-latest
    steps:
      - run: cfn-lint templates/*.yaml
  test-backend:
    name: Backend Tests
    runs-on: ubuntu-latest
    steps:
      - run: cargo test
  build-backend:
    name: Build Backend
    needs: [validate-cfn, test-backend]
    runs-on: ubuntu-latest
    steps:
      - run: aws s3 cp backend.zip s3://bucket/backend/source.zip
  build-frontend:
    name: Build Frontend
    needs: [validate-cfn, test-backend]
    runs-on: ubuntu-latest
    steps:
      - run: aws s3 cp frontend.zip s3://bucket/frontend/artifacts.zip
"#;
        let parsed: serde_json::Value = serde_saphyr::from_str(yaml).unwrap();
        let jobs = extract_jobs_s3_info(&parsed);

        assert_eq!(jobs.len(), 4);

        let cfn = jobs.iter().find(|j| j.job_id == "validate-cfn").unwrap();
        assert_eq!(cfn.job_name, "Validate CloudFormation");
        assert!(cfn.s3_uploads.is_empty());
        assert!(cfn.needs.is_empty());

        let test = jobs.iter().find(|j| j.job_id == "test-backend").unwrap();
        assert_eq!(test.job_name, "Backend Tests");
        assert!(test.s3_uploads.is_empty());
        assert!(test.needs.is_empty());

        let build_be = jobs.iter().find(|j| j.job_id == "build-backend").unwrap();
        assert_eq!(build_be.job_name, "Build Backend");
        assert_eq!(build_be.s3_uploads.len(), 1);
        assert_eq!(build_be.s3_uploads[0].key, "backend/source.zip");
        assert_eq!(build_be.needs, vec!["validate-cfn", "test-backend"]);

        let build_fe = jobs.iter().find(|j| j.job_id == "build-frontend").unwrap();
        assert_eq!(build_fe.job_name, "Build Frontend");
        assert_eq!(build_fe.s3_uploads.len(), 1);
        assert_eq!(build_fe.s3_uploads[0].key, "frontend/artifacts.zip");
        assert_eq!(build_fe.needs, vec!["validate-cfn", "test-backend"]);
    }

    #[test]
    fn extract_jobs_s3_info_single_string_needs() {
        let yaml = r#"
name: Deploy
on: push
jobs:
  build:
    name: Build
    runs-on: ubuntu-latest
    steps:
      - run: aws s3 cp out.zip s3://bucket/key.zip
  deploy:
    name: Deploy
    needs: build
    runs-on: ubuntu-latest
    steps:
      - run: echo deploying
"#;
        let parsed: serde_json::Value = serde_saphyr::from_str(yaml).unwrap();
        let jobs = extract_jobs_s3_info(&parsed);

        let deploy = jobs.iter().find(|j| j.job_id == "deploy").unwrap();
        assert_eq!(deploy.needs, vec!["build"]);
    }

    #[test]
    fn extract_jobs_s3_info_preserves_file_order() {
        let yaml = r#"
name: CI
on: push
jobs:
  zeta:
    runs-on: ubuntu-latest
  alpha:
    runs-on: ubuntu-latest
  mid:
    runs-on: ubuntu-latest
"#;
        let parsed: serde_json::Value = serde_saphyr::from_str(yaml).unwrap();
        let ids: Vec<_> = extract_jobs_s3_info(&parsed)
            .into_iter()
            .map(|j| j.job_id)
            .collect();
        assert_eq!(ids, vec!["zeta", "alpha", "mid"]);
    }

    #[test]
    fn parse_yaml_resolves_anchors_and_on_mapping() {
        let yaml = r#"
name: Deploy
on:
  push:
    branches: [master]
  workflow_dispatch:
x-upload: &upload
  run: aws s3 cp app.zip s3://bucket/app/app.zip
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - *upload
"#;
        let wf = parse_workflow_yaml("deploy.yml", yaml).unwrap();
        assert_eq!(wf.name, "Deploy");
        let build = wf.jobs.iter().find(|j| j.job_id == "build").unwrap();
        assert_eq!(build.s3_uploads.len(), 1);
        assert_eq!(build.s3_uploads[0].key, "app/app.zip");
    }

    #[test]
    fn extract_jobs_s3_info_no_jobs() {
        let yaml = r#"
name: Empty
on: push
"#;
        let parsed: serde_json::Value = serde_saphyr::from_str(yaml).unwrap();
        let jobs = extract_jobs_s3_info(&parsed);
        assert!(jobs.is_empty());
    }

    #[test]
    fn extract_jobs_s3_info_job_without_name_uses_id() {
        let yaml = r#"
name: CI
on: push
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: cargo clippy
"#;
        let parsed: serde_json::Value = serde_saphyr::from_str(yaml).unwrap();
        let jobs = extract_jobs_s3_info(&parsed);

        let lint = jobs.iter().find(|j| j.job_id == "lint").unwrap();
        assert_eq!(lint.job_name, "lint");
    }

    #[test]
    fn extract_jobs_s3_info_github_expression_in_s3() {
        let yaml = r#"
name: CI
on: push
jobs:
  build:
    name: Build
    runs-on: ubuntu-latest
    steps:
      - run: aws s3 cp out.zip s3://bucket-${{ vars.ACCOUNT }}/app/out.zip
"#;
        let parsed: serde_json::Value = serde_saphyr::from_str(yaml).unwrap();
        let jobs = extract_jobs_s3_info(&parsed);

        let build = jobs.iter().find(|j| j.job_id == "build").unwrap();
        assert_eq!(build.s3_uploads.len(), 1);
        assert_eq!(build.s3_uploads[0].key, "app/out.zip");
    }

    // --- parse_job_completed_at tests ---

    #[test]
    fn parse_job_completed_at_extracts_timestamp() {
        use chrono::TimeZone;
        let job = serde_json::json!({
            "completed_at": "2026-03-18T14:28:00Z"
        });
        let result = parse_job_completed_at(&job);
        assert_eq!(
            result,
            Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 3, 18, 14, 28, 0)
                    .unwrap()
            )
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

    // --- T7: map_pr_state tests ---

    #[test]
    fn map_pr_state_open_false_is_open() {
        assert_eq!(map_pr_state("open", false), WatchedPrState::Open);
    }

    #[test]
    fn map_pr_state_closed_true_is_merged() {
        assert_eq!(map_pr_state("closed", true), WatchedPrState::Merged);
    }

    #[test]
    fn map_pr_state_closed_false_is_closed_unmerged() {
        assert_eq!(
            map_pr_state("closed", false),
            WatchedPrState::ClosedUnmerged
        );
    }

    #[test]
    fn map_pr_state_unknown_bucket_when_open_and_merged_true() {
        // Defensive: API shouldn't emit this shape, but we must not panic.
        assert_eq!(map_pr_state("open", true), WatchedPrState::Unknown);
    }

    #[test]
    fn map_pr_state_unknown_bucket_for_unrecognized_state() {
        assert_eq!(map_pr_state("weird", false), WatchedPrState::Unknown);
    }

    // --- T2: PR number harvesting tests ---

    fn call_parse(
        resp: &serde_json::Value,
        branch_filter: Option<&str>,
    ) -> (
        std::collections::HashMap<String, (u64, BuildStatus)>,
        std::collections::HashSet<u64>,
    ) {
        let mut latest = std::collections::HashMap::new();
        let mut prs = std::collections::HashSet::new();
        parse_workflow_runs(resp, &mut latest, &mut prs, branch_filter);
        (latest, prs)
    }

    #[test]
    fn parse_extracts_pr_number_from_pull_request_event() {
        let resp = serde_json::json!({
            "workflow_runs": [{
                "name": "CI", "id": 100, "status": "in_progress",
                "actor": {"login": "alice"},
                "head_branch": "feat/x",
                "event": "pull_request",
                "pull_requests": [{"number": 7}],
            }]
        });
        let (_, prs) = call_parse(&resp, None);
        assert!(prs.contains(&7));
    }

    #[test]
    fn parse_ignores_pr_numbers_on_push_event() {
        let resp = serde_json::json!({
            "workflow_runs": [{
                "name": "CI", "id": 100, "status": "in_progress",
                "actor": {"login": "alice"},
                "head_branch": "master",
                "event": "push",
                "pull_requests": [{"number": 7}],
            }]
        });
        let (_, prs) = call_parse(&resp, None);
        assert!(prs.is_empty());
    }

    #[test]
    fn parse_extracts_pr_numbers_even_for_bot_actors() {
        // The bot filter drops the run from `latest`, but the PR number
        // must still be harvested so we can watch the PR for merge.
        let resp = serde_json::json!({
            "workflow_runs": [{
                "name": "Claude Code Review", "id": 100, "status": "in_progress",
                "actor": {"login": "claude[bot]"},
                "head_branch": "feat/x",
                "event": "pull_request",
                "pull_requests": [{"number": 9}],
            }]
        });
        let (latest, prs) = call_parse(&resp, None);
        assert!(
            latest.is_empty(),
            "bot-actor run should still be filtered out"
        );
        assert!(prs.contains(&9), "but PR number must be harvested");
    }

    #[test]
    fn parse_extracts_pr_numbers_even_when_head_branch_mismatches() {
        let resp = serde_json::json!({
            "workflow_runs": [{
                "name": "CI", "id": 100, "status": "in_progress",
                "actor": {"login": "alice"},
                "head_branch": "feat/x",
                "event": "pull_request",
                "pull_requests": [{"number": 11}],
            }]
        });
        let (latest, prs) = call_parse(&resp, Some("master"));
        assert!(
            latest.is_empty(),
            "branch filter should drop from visible list"
        );
        assert!(prs.contains(&11), "PR number must still be harvested");
    }

    #[test]
    fn parse_dedupes_pr_numbers_across_runs() {
        let resp = serde_json::json!({
            "workflow_runs": [
                {
                    "name": "CI", "id": 100, "status": "in_progress",
                    "actor": {"login": "alice"},
                    "head_branch": "feat/x",
                    "event": "pull_request",
                    "pull_requests": [{"number": 3}],
                },
                {
                    "name": "Review", "id": 101, "status": "in_progress",
                    "actor": {"login": "bob"},
                    "head_branch": "feat/x",
                    "event": "pull_request",
                    "pull_requests": [{"number": 3}],
                }
            ]
        });
        let (_, prs) = call_parse(&resp, None);
        assert_eq!(prs.len(), 1);
        assert!(prs.contains(&3));
    }

    #[test]
    fn parse_accepts_pull_request_target_event() {
        let resp = serde_json::json!({
            "workflow_runs": [{
                "name": "CI", "id": 100, "status": "in_progress",
                "actor": {"login": "alice"},
                "head_branch": "feat/x",
                "event": "pull_request_target",
                "pull_requests": [{"number": 13}],
            }]
        });
        let (_, prs) = call_parse(&resp, None);
        assert!(prs.contains(&13));
    }

    #[test]
    fn parse_empty_when_pull_requests_missing_or_empty() {
        let resp = serde_json::json!({
            "workflow_runs": [
                {
                    "name": "CI", "id": 100, "status": "in_progress",
                    "actor": {"login": "alice"},
                    "head_branch": "master",
                    "event": "pull_request",
                },
                {
                    "name": "CI2", "id": 101, "status": "in_progress",
                    "actor": {"login": "alice"},
                    "head_branch": "master",
                    "event": "pull_request",
                    "pull_requests": [],
                }
            ]
        });
        let (_, prs) = call_parse(&resp, None);
        assert!(prs.is_empty());
    }
}
