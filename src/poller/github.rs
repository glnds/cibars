use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{ActionsClient, JobInfo, WorkflowRunInfo};
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
    async fn list_workflow_runs(&self) -> Result<Vec<WorkflowRunInfo>> {
        let route = format!(
            "/repos/{}/{}/actions/runs?per_page=50",
            self.owner, self.repo,
        );
        let resp: serde_json::Value = self
            .octocrab
            .get(&route, None::<&()>)
            .await
            .context("failed to list workflow runs")?;

        // Deduplicate to latest run per workflow
        let mut latest_per_workflow: std::collections::HashMap<String, (u64, BuildStatus)> =
            std::collections::HashMap::new();

        if let Some(runs) = resp["workflow_runs"].as_array() {
            for run in runs {
                let name = run["name"].as_str().unwrap_or("unknown").to_string();
                let run_id = run["id"].as_u64().unwrap_or(0);
                let status = run["status"].as_str().unwrap_or("unknown");
                let conclusion = run["conclusion"].as_str();

                latest_per_workflow
                    .entry(name)
                    .or_insert((run_id, map_run_status(status, conclusion)));
            }
        }

        tracing::debug!(
            workflows = latest_per_workflow.len(),
            "deduped workflow runs"
        );

        // Fetch jobs for each workflow's latest run
        let mut results = Vec::new();
        for (workflow_name, (run_id, status)) in latest_per_workflow {
            let jobs = match self.fetch_jobs(run_id).await {
                Ok(j) => {
                    tracing::debug!(workflow = %workflow_name, run_id, count = j.len(), "fetched jobs");
                    j
                }
                Err(e) => {
                    tracing::error!(workflow = %workflow_name, run_id, error = %e, "failed to fetch jobs");
                    Vec::new()
                }
            };
            results.push(WorkflowRunInfo {
                workflow_name,
                run_id,
                status,
                jobs,
            });
        }

        Ok(results)
    }
}

impl GitHubActionsClient {
    async fn fetch_jobs(&self, run_id: u64) -> Result<Vec<JobInfo>> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
