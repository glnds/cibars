use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{ActionsClient, WorkflowRunInfo};
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

        let mut latest_per_workflow = std::collections::HashMap::new();

        if let Some(runs) = resp["workflow_runs"].as_array() {
            for run in runs {
                let name = run["name"].as_str().unwrap_or("unknown").to_string();
                let status = run["status"].as_str().unwrap_or("unknown");
                let conclusion = run["conclusion"].as_str();

                latest_per_workflow
                    .entry(name.clone())
                    .or_insert_with(|| WorkflowRunInfo {
                        workflow_name: name,
                        status: map_run_status(status, conclusion),
                    });
            }
        }

        Ok(latest_per_workflow.into_values().collect())
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
