use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_codepipeline::Client;

use super::{PipelineClient, PipelineState};
use crate::model::BuildStatus;

pub struct AwsPipelineClient {
    client: Client,
}

impl AwsPipelineClient {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

/// Map AWS pipeline execution status string to BuildStatus.
pub fn map_pipeline_status(status: Option<&str>) -> BuildStatus {
    match status {
        Some("InProgress") => BuildStatus::Running,
        Some("Succeeded") => BuildStatus::Succeeded,
        Some("Failed") | Some("Stopped") | Some("Superseded") => BuildStatus::Failed,
        _ => BuildStatus::Idle,
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
        let status = resp
            .stage_states()
            .last()
            .and_then(|s| s.latest_execution().map(|e| e.status().as_str()));
        Ok(PipelineState {
            name: name.to_string(),
            status: map_pipeline_status(status),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
