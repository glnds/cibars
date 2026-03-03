pub mod aws;
pub mod github;

use anyhow::Result;
use async_trait::async_trait;

use crate::model::BuildStatus;

/// Simplified pipeline state from AWS API
pub struct PipelineState {
    pub name: String,
    pub status: BuildStatus,
}

/// Simplified workflow run from GitHub API
pub struct WorkflowRunInfo {
    pub workflow_name: String,
    pub status: BuildStatus,
}

#[async_trait]
pub trait PipelineClient: Send + Sync {
    async fn list_pipeline_names(&self) -> Result<Vec<String>>;
    async fn get_pipeline_state(&self, name: &str) -> Result<PipelineState>;
}

#[async_trait]
pub trait ActionsClient: Send + Sync {
    async fn list_workflow_runs(&self) -> Result<Vec<WorkflowRunInfo>>;
}
