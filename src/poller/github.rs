use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{ActionsClient, JobInfo, WorkflowRunSummary};
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
            let name = run["name"].as_str().unwrap_or("unknown").to_string();
            let status = run["status"].as_str().unwrap_or("unknown");
            let conclusion = run["conclusion"].as_str();

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
}
