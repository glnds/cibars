use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{ActionsClient, JobInfo, RunsPage, S3Upload, WorkflowFile, WorkflowRunSummary};
use crate::model::{BuildStatus, WatchedPrState};

pub struct GitHubActionsClient {
    octocrab: octocrab::Octocrab,
    owner: String,
    repo: String,
    branch: Option<String>,
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
        })
    }
}

/// Build the API route for listing workflow runs, optionally filtered by branch.
fn build_runs_route(owner: &str, repo: &str, page: u32, branch: Option<&str>) -> String {
    let mut route = format!("/repos/{owner}/{repo}/actions/runs?per_page=100&page={page}");
    if let Some(b) = branch {
        route.push_str("&branch=");
        route.push_str(b);
    }
    route
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
        let mut latest_per_workflow: std::collections::HashMap<String, (u64, BuildStatus)> =
            std::collections::HashMap::new();
        let mut pr_numbers: std::collections::HashSet<u64> = std::collections::HashSet::new();

        let mut page: u32 = 1;
        // Safety cap to avoid infinite loops on API misbehaviour.
        const MAX_PAGES: u32 = 20;

        loop {
            let route = build_runs_route(&self.owner, &self.repo, page, self.branch.as_deref());
            let resp: serde_json::Value = self
                .octocrab
                .get(&route, None::<&()>)
                .await
                .context("failed to list workflow runs")?;

            let prev_count = latest_per_workflow.len();
            parse_workflow_runs(
                &resp,
                &mut latest_per_workflow,
                &mut pr_numbers,
                self.branch.as_deref(),
            );
            let new_count = latest_per_workflow.len();

            let total_count = resp["total_count"].as_u64().unwrap_or(0);
            let fetched = resp["workflow_runs"]
                .as_array()
                .map_or(0, |a| a.len() as u64);

            // No new workflows on this page — all subsequent pages are older, stop.
            let no_new_workflows = new_count == prev_count;

            if fetched < 100
                || (page as u64) * 100 >= total_count
                || page >= MAX_PAGES
                || no_new_workflows
            {
                break;
            }
            page += 1;
        }

        tracing::debug!(
            workflows = latest_per_workflow.len(),
            pages = page,
            "deduped workflow runs"
        );

        let summaries = latest_per_workflow
            .into_iter()
            .map(|(workflow_name, (run_id, status))| WorkflowRunSummary {
                workflow_name,
                run_id,
                status,
            })
            .collect();
        Ok(RunsPage {
            summaries,
            pr_numbers,
        })
    }

    async fn fetch_run_jobs(&self, run_id: u64) -> Result<Vec<JobInfo>> {
        let route = format!(
            "/repos/{}/{}/actions/runs/{run_id}/jobs?per_page=100",
            self.owner, self.repo,
        );
        let resp: serde_json::Value = self
            .octocrab
            .get(&route, None::<&()>)
            .await
            .context("failed to list jobs")?;

        let mut jobs = Vec::new();
        if let Some(job_array) = resp["jobs"].as_array() {
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
        Ok(jobs)
    }

    async fn fetch_pr_state(&self, pr_number: u64) -> Result<WatchedPrState> {
        let route = format!("/repos/{}/{}/pulls/{pr_number}", self.owner, self.repo,);
        let resp: serde_json::Value = self
            .octocrab
            .get(&route, None::<&()>)
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
        let listing: serde_json::Value = self
            .octocrab
            .get(&route, None::<&()>)
            .await
            .context("failed to list workflow files")?;

        let files = match listing.as_array() {
            Some(a) => a,
            None => return Ok(Vec::new()),
        };

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
                let result = self
                    .octocrab
                    .get::<serde_json::Value, _, _>(&file_route, None::<&()>)
                    .await;
                (path.as_str(), result)
            }
        });

        let results = futures::future::join_all(futs).await;

        let mut workflow_files = Vec::new();
        for (path, result) in results {
            match result {
                Ok(file_resp) => {
                    let content = file_resp["content"]
                        .as_str()
                        .map(|c| c.replace('\n', ""))
                        .and_then(|c| {
                            use base64::Engine;
                            base64::engine::general_purpose::STANDARD.decode(c).ok()
                        })
                        .and_then(|bytes| String::from_utf8(bytes).ok());

                    if let Some(content) = content {
                        let filename = path.rsplit('/').next().unwrap_or(path);
                        if let Some(wf) = parse_workflow_yaml(filename, &content) {
                            if !wf.s3_uploads.is_empty() {
                                tracing::info!(
                                    workflow = %wf.name,
                                    uploads = wf.s3_uploads.len(),
                                    "found S3 uploads in workflow"
                                );
                                workflow_files.push(wf);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %path, error = %e, "failed to fetch workflow file");
                }
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
    let yaml: serde_yaml::Value = serde_yaml::from_str(content).ok()?;
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
pub fn extract_jobs_s3_info(yaml: &serde_yaml::Value) -> Vec<super::JobS3Info> {
    let jobs_map = match yaml.get("jobs").and_then(|j| j.as_mapping()) {
        Some(m) => m,
        None => return Vec::new(),
    };

    let mut result = Vec::new();
    for (key, value) in jobs_map {
        let job_id = match key.as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        let job_name = value
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or(&job_id)
            .to_string();

        // Extract `needs:` — can be a string or a sequence
        let needs = match value.get("needs") {
            Some(serde_yaml::Value::String(s)) => vec![s.clone()],
            Some(serde_yaml::Value::Sequence(seq)) => seq
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        };

        // Extract S3 paths from all `run:` steps in this job
        let mut s3_uploads = Vec::new();
        if let Some(steps) = value.get("steps").and_then(|s| s.as_sequence()) {
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
) {
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
            latest
                .entry(name)
                .or_insert((run_id, map_run_status(status, conclusion)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- build_runs_route tests ---

    #[test]
    fn build_runs_route_no_branch() {
        let route = build_runs_route("owner", "repo", 1, None);
        assert_eq!(route, "/repos/owner/repo/actions/runs?per_page=100&page=1");
    }

    #[test]
    fn build_runs_route_with_branch() {
        let route = build_runs_route("owner", "repo", 1, Some("master"));
        assert_eq!(
            route,
            "/repos/owner/repo/actions/runs?per_page=100&page=1&branch=master"
        );
    }

    #[test]
    fn build_runs_route_page_2() {
        let route = build_runs_route("o", "r", 2, Some("main"));
        assert_eq!(
            route,
            "/repos/o/r/actions/runs?per_page=100&page=2&branch=main"
        );
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
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
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
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        let jobs = extract_jobs_s3_info(&parsed);

        let deploy = jobs.iter().find(|j| j.job_id == "deploy").unwrap();
        assert_eq!(deploy.needs, vec!["build"]);
    }

    #[test]
    fn extract_jobs_s3_info_no_jobs() {
        let yaml = r#"
name: Empty
on: push
"#;
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
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
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
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
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
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
