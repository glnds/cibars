use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::config::HookStatus;
use crate::linkage::JobAssignment;
use crate::model::{BuildStatus, PipelineGroup, WorkflowGroup};
use crate::poll_scheduler::PollState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceHealth {
    Unknown,
    Healthy,
    AuthFailed { since: DateTime<Utc> },
}

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
    /// AWS authentication health state.
    pub aws_health: SourceHealth,
    /// When boost key was last pressed, for visual flash feedback.
    pub boost_pressed_at: Option<Instant>,
    /// True when cached link map has stale references.
    pub linkage_broken: bool,
    /// True while discover_links() is running.
    pub linkage_discovering: bool,
    /// Per-job assignment for pipeline-centric UI (set after link discovery).
    pub job_assignment: Option<JobAssignment>,
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
            aws_health: SourceHealth::Unknown,
            boost_pressed_at: None,
            linkage_broken: false,
            linkage_discovering: false,
            job_assignment: None,
        }
    }

    pub fn push_warning(&mut self, msg: String) {
        if self.warnings.len() >= Self::MAX_WARNINGS {
            self.warnings.remove(0);
        }
        self.warnings.push(msg);
    }

    /// Compare link map against live data. Only meaningful after initial
    /// loading is complete (both pipelines and actions fetched at least once).
    pub fn check_linkage_health(&mut self, link_map: &crate::linkage::LinkMap) {
        if self.loading_pipelines || self.loading_actions {
            return;
        }
        if link_map.links().is_empty() {
            self.linkage_broken = false;
            return;
        }
        let ghost_pipeline = link_map.links().iter().any(|l| {
            !self
                .pipeline_groups
                .iter()
                .any(|pg| pg.name == l.pipeline_name)
        });
        let ghost_workflow = link_map.links().iter().any(|l| {
            !self
                .workflow_groups
                .iter()
                .any(|wg| wg.name == l.workflow_name)
        });
        self.linkage_broken = ghost_pipeline || ghost_workflow;
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
    use crate::model::{Bar, WorkflowCategory, WorkflowGroup};

    #[test]
    fn app_starts_with_unknown_aws_health() {
        let app = App::new();
        assert_eq!(app.aws_health, SourceHealth::Unknown);
    }

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
            pending_link: false,
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
            pending_link: false,
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
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
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
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
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
            pending_link: false,
        });
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        assert!(!app.has_any_running());
    }

    #[test]
    fn has_any_running_mixed_states() {
        let mut app = App::new();
        app.pipeline_groups.push(PipelineGroup {
            name: "p1".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded,
            pending_link: false,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "p2".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Failed,
            pending_link: false,
        });
        assert!(!app.has_any_running());
        // Now add one with a running stage
        let mut stage = Bar::new("Build".into());
        stage.set_status(BuildStatus::Running);
        app.pipeline_groups.push(PipelineGroup {
            name: "p3".into(),
            stages: vec![stage],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: false,
        });
        assert!(app.has_any_running());
    }

    #[test]
    fn app_starts_with_no_boost() {
        let app = App::new();
        assert!(app.boost_pressed_at.is_none());
    }

    #[test]
    fn default_matches_new() {
        let new_app = App::new();
        let default_app = App::default();
        assert_eq!(new_app.loading_pipelines, default_app.loading_pipelines);
        assert_eq!(new_app.loading_actions, default_app.loading_actions);
        assert_eq!(new_app.actions_expanded, default_app.actions_expanded);
    }

    use crate::linkage::LinkMap;

    #[test]
    fn check_linkage_health_skips_while_loading() {
        let mut app = App::new();
        let mut link_map = LinkMap::new();
        link_map.add_discovered("pipe".into(), "wf".into(), "b".into(), "k".into());
        // loading_pipelines is true by default
        app.check_linkage_health(&link_map);
        assert!(!app.linkage_broken, "should not flag broken while loading");
    }

    #[test]
    fn check_linkage_health_empty_links_not_broken() {
        let mut app = App::new();
        app.loading_pipelines = false;
        app.loading_actions = false;
        let link_map = LinkMap::new();
        app.check_linkage_health(&link_map);
        assert!(!app.linkage_broken);
    }

    #[test]
    fn check_linkage_health_ghost_pipeline() {
        let mut app = App::new();
        app.loading_pipelines = false;
        app.loading_actions = false;
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        let mut link_map = LinkMap::new();
        link_map.add_discovered("missing-pipe".into(), "CI".into(), "b".into(), "k".into());
        app.check_linkage_health(&link_map);
        assert!(app.linkage_broken, "ghost pipeline should flag broken");
    }

    #[test]
    fn check_linkage_health_ghost_workflow() {
        let mut app = App::new();
        app.loading_pipelines = false;
        app.loading_actions = false;
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: false,
        });
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy".into(), "missing-wf".into(), "b".into(), "k".into());
        app.check_linkage_health(&link_map);
        assert!(app.linkage_broken, "ghost workflow should flag broken");
    }

    #[test]
    fn check_linkage_health_all_present() {
        let mut app = App::new();
        app.loading_pipelines = false;
        app.loading_actions = false;
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: false,
        });
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy".into(), "CI".into(), "b".into(), "k".into());
        app.check_linkage_health(&link_map);
        assert!(!app.linkage_broken, "all present should not be broken");
    }

    #[test]
    fn app_starts_with_linkage_defaults() {
        let app = App::new();
        assert!(!app.linkage_broken);
        assert!(!app.linkage_discovering);
    }
}
