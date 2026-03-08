use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{ActionsClient, JobInfo, S3Upload, WorkflowFile, WorkflowRunSummary};
use crate::model::BuildStatus;

pub struct GitHubActionsClient {
    octocrab: octocrab::Octocrab,
    owner: String,
    repo: String,
}

impl GitHubActionsClient {
    pub fn new(token: &str, owner: String, repo: String) -> Result<Self> {
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
    async fn list_latest_runs(&self) -> Result<Vec<WorkflowRunSummary>> {
        let mut latest_per_workflow: std::collections::HashMap<String, (u64, BuildStatus)> =
            std::collections::HashMap::new();

        let mut page: u32 = 1;
        // Safety cap to avoid infinite loops on API misbehaviour.
        const MAX_PAGES: u32 = 20;

        loop {
            let route = format!(
                "/repos/{}/{}/actions/runs?per_page=100&page={page}",
                self.owner, self.repo,
            );
            let resp: serde_json::Value = self
                .octocrab
                .get(&route, None::<&()>)
                .await
                .context("failed to list workflow runs")?;

            parse_workflow_runs(&resp, &mut latest_per_workflow);

            let total_count = resp["total_count"].as_u64().unwrap_or(0);
            let fetched = resp["workflow_runs"]
                .as_array()
                .map_or(0, |a| a.len() as u64);

            // Stop when this page is incomplete or we've fetched everything.
            if fetched < 100 || (page as u64) * 100 >= total_count || page >= MAX_PAGES {
                break;
            }
            page += 1;
        }

        tracing::debug!(
            workflows = latest_per_workflow.len(),
            pages = page,
            "deduped workflow runs"
        );

        Ok(latest_per_workflow
            .into_iter()
            .map(|(workflow_name, (run_id, status))| WorkflowRunSummary {
                workflow_name,
                run_id,
                status,
            })
            .collect())
    }

    async fn fetch_run_jobs(&self, run_id: u64) -> Result<Vec<JobInfo>> {
        let route = format!(
            "/repos/{}/{}/actions/runs/{run_id}/jobs",
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
                });
            }
        }
        Ok(jobs)
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

        let mut workflow_files = Vec::new();
        for path in &yaml_paths {
            let file_route = format!("/repos/{}/{}/contents/{path}", self.owner, self.repo,);
            match self
                .octocrab
                .get::<serde_json::Value, _, _>(&file_route, None::<&()>)
                .await
            {
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

/// Extract S3 paths from shell commands in workflow YAML `run` steps.
/// Looks for `aws s3 cp` and `aws s3 sync` commands with `s3://bucket/key` patterns.
pub fn extract_s3_paths(yaml_content: &str) -> Vec<S3Upload> {
    let re = regex::Regex::new(r"s3://([^/\s]+)/(\S+)").expect("valid regex");
    let mut uploads = Vec::new();

    for cap in re.captures_iter(yaml_content) {
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
    Some(WorkflowFile { name, s3_uploads })
}

/// Parse workflow runs from a JSON response page into the latest-per-workflow map.
/// Skips runs with missing IDs (logs a warning).
fn parse_workflow_runs(
    resp: &serde_json::Value,
    latest: &mut std::collections::HashMap<String, (u64, BuildStatus)>,
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
        parse_workflow_runs(&resp, &mut latest);
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
        parse_workflow_runs(&resp, &mut latest);
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
        parse_workflow_runs(&resp, &mut latest);
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
        parse_workflow_runs(&resp, &mut latest);
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
        parse_workflow_runs(&resp, &mut latest);
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
        parse_workflow_runs(&resp, &mut latest);
        assert!(latest.is_empty());
    }

    #[test]
    fn parse_run_missing_name_defaults_to_unknown() {
        let resp = serde_json::json!({
            "workflow_runs": [{"id": 1, "status": "completed", "conclusion": "success"}]
        });
        let mut latest = std::collections::HashMap::new();
        parse_workflow_runs(&resp, &mut latest);
        assert!(latest.contains_key("unknown"));
    }
}
