use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_codepipeline::Client;

use super::{ActionState, PipelineClient, PipelineDefinition, PipelineState, S3Source, StageState};
use crate::model::BuildStatus;

pub struct AwsPipelineClient {
    client: Client,
}

impl AwsPipelineClient {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

/// Map AWS action execution status string to BuildStatus.
pub fn map_action_status(status: &str) -> BuildStatus {
    match status {
        "InProgress" => BuildStatus::Running,
        "Succeeded" => BuildStatus::Succeeded,
        "Failed" | "Abandoned" => BuildStatus::Failed,
        _ => BuildStatus::Idle,
    }
}

/// Aggregate statuses across all pipeline stages.
/// Priority: Running > Failed > Succeeded > Idle.
pub fn aggregate_stage_statuses(stage_statuses: &[Option<&str>]) -> BuildStatus {
    let mut has_in_progress = false;
    let mut has_failed = false;
    let mut has_succeeded = false;

    for status in stage_statuses {
        match *status {
            Some("InProgress") => has_in_progress = true,
            Some("Failed") | Some("Stopped") | Some("Superseded") => has_failed = true,
            Some("Succeeded") => has_succeeded = true,
            _ => {}
        }
    }

    if has_in_progress {
        BuildStatus::Running
    } else if has_failed {
        BuildStatus::Failed
    } else if has_succeeded {
        BuildStatus::Succeeded
    } else {
        BuildStatus::Idle
    }
}

#[async_trait]
impl PipelineClient for AwsPipelineClient {
    async fn list_pipeline_names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let mut paginator = self.client.list_pipelines().into_paginator().send();
        while let Some(resp) = paginator.next().await {
            let resp = resp.context("failed to list pipelines")?;
            names.extend(
                resp.pipelines()
                    .iter()
                    .filter_map(|p| p.name().map(String::from)),
            );
        }
        Ok(names)
    }

    async fn get_pipeline_state(&self, name: &str) -> Result<PipelineState> {
        let resp = self
            .client
            .get_pipeline_state()
            .name(name)
            .send()
            .await
            .with_context(|| format!("failed to get state for {name}"))?;
        let stage_statuses: Vec<Option<&str>> = resp
            .stage_states()
            .iter()
            .map(|s| s.latest_execution().map(|e| e.status().as_str()))
            .collect();
        let stages: Vec<StageState> = resp
            .stage_states()
            .iter()
            .map(|s| {
                let stage_name = s.stage_name().unwrap_or("unknown").to_string();
                let actions = s
                    .action_states()
                    .iter()
                    .map(|a| {
                        let status = a
                            .latest_execution()
                            .and_then(|e| e.status())
                            .map(|s| map_action_status(s.as_str()))
                            .unwrap_or(BuildStatus::Idle);
                        ActionState { status }
                    })
                    .collect();
                StageState {
                    name: stage_name,
                    actions,
                }
            })
            .collect();
        Ok(PipelineState {
            name: name.to_string(),
            status: aggregate_stage_statuses(&stage_statuses),
            stages,
        })
    }

    async fn get_pipeline_definition(&self, name: &str) -> Result<PipelineDefinition> {
        let resp = self
            .client
            .get_pipeline()
            .name(name)
            .send()
            .await
            .with_context(|| format!("failed to get definition for {name}"))?;

        let source_s3 = resp
            .pipeline()
            .and_then(|p| {
                p.stages().iter().find(|s| {
                    s.name() == "Source"
                        || s.actions().iter().any(|a| {
                            a.action_type_id()
                                .map(|t| t.provider() == "S3")
                                .unwrap_or(false)
                        })
                })
            })
            .and_then(|stage| {
                stage.actions().iter().find(|a| {
                    a.action_type_id()
                        .map(|t| t.provider() == "S3")
                        .unwrap_or(false)
                })
            })
            .and_then(|action| {
                let config = action.configuration()?;
                let bucket = config.get("S3Bucket")?.to_string();
                let key = config.get("S3ObjectKey")?.to_string();
                Some(S3Source {
                    bucket,
                    object_key: key,
                })
            });

        Ok(PipelineDefinition {
            name: name.to_string(),
            source_s3,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Map AWS pipeline execution status string to BuildStatus.
    fn map_pipeline_status(status: Option<&str>) -> BuildStatus {
        match status {
            Some("InProgress") => BuildStatus::Running,
            Some("Succeeded") => BuildStatus::Succeeded,
            Some("Failed") | Some("Stopped") | Some("Superseded") => BuildStatus::Failed,
            _ => BuildStatus::Idle,
        }
    }

    #[test]
    fn maps_in_progress() {
        assert_eq!(
            map_pipeline_status(Some("InProgress")),
            BuildStatus::Running
        );
    }

    #[test]
    fn maps_succeeded() {
        assert_eq!(
            map_pipeline_status(Some("Succeeded")),
            BuildStatus::Succeeded
        );
    }

    #[test]
    fn maps_failed() {
        assert_eq!(map_pipeline_status(Some("Failed")), BuildStatus::Failed);
    }

    #[test]
    fn maps_stopped() {
        assert_eq!(map_pipeline_status(Some("Stopped")), BuildStatus::Failed);
    }

    #[test]
    fn maps_superseded() {
        assert_eq!(map_pipeline_status(Some("Superseded")), BuildStatus::Failed);
    }

    #[test]
    fn maps_none_to_idle() {
        assert_eq!(map_pipeline_status(None), BuildStatus::Idle);
    }

    #[test]
    fn maps_unknown_to_idle() {
        assert_eq!(map_pipeline_status(Some("Whatever")), BuildStatus::Idle);
    }

    // --- aggregate_stage_statuses tests ---

    #[test]
    fn aggregate_all_succeeded() {
        let statuses = vec![Some("Succeeded"), Some("Succeeded"), Some("Succeeded")];
        assert_eq!(aggregate_stage_statuses(&statuses), BuildStatus::Succeeded);
    }

    #[test]
    fn aggregate_any_in_progress_overrides() {
        let statuses = vec![Some("Succeeded"), Some("InProgress"), Some("Succeeded")];
        assert_eq!(aggregate_stage_statuses(&statuses), BuildStatus::Running);
    }

    #[test]
    fn aggregate_failed_no_in_progress() {
        let statuses = vec![Some("Succeeded"), Some("Failed")];
        assert_eq!(aggregate_stage_statuses(&statuses), BuildStatus::Failed);
    }

    #[test]
    fn aggregate_stopped() {
        let statuses = vec![Some("Succeeded"), Some("Stopped")];
        assert_eq!(aggregate_stage_statuses(&statuses), BuildStatus::Failed);
    }

    #[test]
    fn aggregate_superseded() {
        let statuses = vec![Some("Superseded")];
        assert_eq!(aggregate_stage_statuses(&statuses), BuildStatus::Failed);
    }

    #[test]
    fn aggregate_in_progress_overrides_failed() {
        let statuses = vec![Some("Failed"), Some("InProgress")];
        assert_eq!(aggregate_stage_statuses(&statuses), BuildStatus::Running);
    }

    #[test]
    fn aggregate_empty() {
        let statuses: Vec<Option<&str>> = vec![];
        assert_eq!(aggregate_stage_statuses(&statuses), BuildStatus::Idle);
    }

    #[test]
    fn aggregate_all_none() {
        let statuses = vec![None, None];
        assert_eq!(aggregate_stage_statuses(&statuses), BuildStatus::Idle);
    }

    #[test]
    fn aggregate_mixed_with_none() {
        let statuses = vec![None, Some("Succeeded")];
        assert_eq!(aggregate_stage_statuses(&statuses), BuildStatus::Succeeded);
    }

    // --- map_action_status tests ---

    #[test]
    fn action_in_progress() {
        assert_eq!(map_action_status("InProgress"), BuildStatus::Running);
    }

    #[test]
    fn action_succeeded() {
        assert_eq!(map_action_status("Succeeded"), BuildStatus::Succeeded);
    }

    #[test]
    fn action_failed() {
        assert_eq!(map_action_status("Failed"), BuildStatus::Failed);
    }

    #[test]
    fn action_abandoned() {
        assert_eq!(map_action_status("Abandoned"), BuildStatus::Failed);
    }

    #[test]
    fn action_unknown_to_idle() {
        assert_eq!(map_action_status("Whatever"), BuildStatus::Idle);
    }
}
