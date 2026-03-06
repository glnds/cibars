use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::config::HookStatus;
use crate::model::{BuildStatus, PipelineGroup, WorkflowGroup};
use crate::poll_scheduler::PollState;

pub struct App {
    pub pipeline_groups: Vec<PipelineGroup>,
    pub workflow_groups: Vec<WorkflowGroup>,
    pub actions_expanded: bool,
    pub pipelines_expanded: bool,
    pub last_poll: Option<DateTime<Utc>>,
    pub warnings: Vec<String>,
    /// Terminal width (updated each UI render), used by animation tick.
    pub terminal_width: u16,
    /// If set, skip GitHub polling until this instant (rate limit back-off).
    pub rate_limited_until: Option<Instant>,
    /// True until first pipeline poll completes.
    pub loading_pipelines: bool,
    /// True until first actions poll completes.
    pub loading_actions: bool,
    /// Current poll state machine state, for UI display.
    pub poll_state: PollState,
    /// Set by orchestrator at start of each poll cycle.
    pub last_poll_started: Option<Instant>,
    /// Cooldown remaining, set by orchestrator for UI display.
    pub cooldown_remaining: Option<std::time::Duration>,
    /// Git pre-push hook status for boost integration.
    pub hook_status: HookStatus,
}

impl App {
    pub const MAX_WARNINGS: usize = 10;

    pub fn new() -> Self {
        Self {
            pipeline_groups: Vec::new(),
            workflow_groups: Vec::new(),
            actions_expanded: true,
            pipelines_expanded: true,
            last_poll: None,
            warnings: Vec::new(),
            terminal_width: 80,
            rate_limited_until: None,
            loading_pipelines: true,
            loading_actions: true,
            poll_state: PollState::Idle,
            last_poll_started: None,
            cooldown_remaining: None,
            hook_status: HookStatus::NoGitDir,
        }
    }

    pub fn push_warning(&mut self, msg: String) {
        if self.warnings.len() >= Self::MAX_WARNINGS {
            self.warnings.remove(0);
        }
        self.warnings.push(msg);
    }

    pub fn has_any_running(&self) -> bool {
        self.pipeline_groups.iter().any(|g| {
            g.summary_status == BuildStatus::Running
                || g.stages.iter().any(|s| s.status == BuildStatus::Running)
        }) || self.workflow_groups.iter().any(|g| {
            g.summary_status == BuildStatus::Running
                || g.jobs.iter().any(|j| j.status == BuildStatus::Running)
        })
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bar, WorkflowGroup};

    #[test]
    fn app_starts_with_loading_flags() {
        let app = App::new();
        assert!(app.loading_pipelines);
        assert!(app.loading_actions);
    }

    #[test]
    fn has_any_running_empty() {
        let app = App::new();
        assert!(!app.has_any_running());
    }

    #[test]
    fn has_any_running_pipeline_summary_running() {
        let mut app = App::new();
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
        });
        assert!(app.has_any_running());
    }

    #[test]
    fn has_any_running_pipeline_stage_running() {
        let mut app = App::new();
        let mut stage = Bar::new("Build".into());
        stage.set_status(BuildStatus::Running);
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![stage],
            gone: false,
            summary_status: BuildStatus::Succeeded,
        });
        assert!(app.has_any_running());
    }

    #[test]
    fn has_any_running_gh_summary_running() {
        let mut app = App::new();
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
        });
        assert!(app.has_any_running());
    }

    #[test]
    fn has_any_running_gh_job_running() {
        let mut app = App::new();
        let mut job = Bar::new("build".into());
        job.set_status(BuildStatus::Running);
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![job],
            gone: false,
            summary_status: BuildStatus::Succeeded,
        });
        assert!(app.has_any_running());
    }

    #[test]
    fn push_warning_caps_at_limit() {
        let mut app = App::new();
        for i in 0..15 {
            app.push_warning(format!("warning {i}"));
        }
        assert_eq!(app.warnings.len(), App::MAX_WARNINGS);
        assert_eq!(app.warnings[0], "warning 5");
        assert_eq!(app.warnings[App::MAX_WARNINGS - 1], "warning 14");
    }

    #[test]
    fn has_any_running_all_succeeded() {
        let mut app = App::new();
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded,
        });
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded,
        });
        assert!(!app.has_any_running());
    }
}
