use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::config::HookStatus;
use crate::linkage::JobAssignment;
use crate::model::{BuildStatus, PipelineGroup, WatchedPr, WatchedPrState, WorkflowGroup};
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
    /// Git pre-push hook status for boost integration.
    pub hook_status: HookStatus,
    /// True when core.hooksPath overrides .git/hooks (global hooks dir active).
    pub has_global_hooks_path: bool,
    /// AWS authentication health state.
    pub aws_health: SourceHealth,
    /// When boost key was last pressed, for visual flash feedback.
    pub boost_pressed_at: Option<Instant>,
    /// When a SIGUSR1 push signal was last received, for transient UI feedback.
    pub push_signal_at: Option<Instant>,
    /// True when cached link map has stale references.
    pub linkage_broken: bool,
    /// True while discover_links() is running.
    pub linkage_discovering: bool,
    /// Per-job assignment for pipeline-centric UI (set after link discovery).
    pub job_assignment: Option<JobAssignment>,
    /// PRs harvested from pull_request[_target] workflow runs; each entry's
    /// state is polled separately so the scheduler can linger past Cooldown
    /// while merges are pending and auto-boost when one lands.
    pub watched_prs: HashMap<u64, WatchedPr>,
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
            poll_state: PollState::Sleep,
            hook_status: HookStatus::NoGitDir,
            has_global_hooks_path: false,
            aws_health: SourceHealth::Unknown,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
            job_assignment: None,
            watched_prs: HashMap::new(),
        }
    }

    /// Insert fresh PRs as `Open` with `first_seen = now`. PRs already in
    /// the map are left alone — this preserves both `first_seen` (so the
    /// 1 h cap works from first sighting) and any polled state updates.
    #[allow(dead_code)] // wired into poll_actions_tick in T5
    pub fn add_or_update_watched_prs(&mut self, numbers: &[u64], now: Instant) {
        for &n in numbers {
            self.watched_prs
                .entry(n)
                .or_insert_with(|| WatchedPr::new(n, now));
        }
    }

    /// True iff any watched PR is still `Open` and was first seen within
    /// `cap`. Used by the orchestrator to keep Cooldown from draining to
    /// Sleep while a merge is still pending.
    #[allow(dead_code)] // wired into effective_any_running in T6
    pub fn has_watched_open_prs(&self, cap: Duration, now: Instant) -> bool {
        self.watched_prs.values().any(|pr| {
            pr.state == WatchedPrState::Open && now.saturating_duration_since(pr.first_seen) < cap
        })
    }

    /// Drop entries whose `first_seen` is older than `cap`. Prevents the
    /// map growing unbounded across a long-running daemon.
    #[allow(dead_code)] // wired into orchestrator in T9
    pub fn prune_expired_prs(&mut self, cap: Duration, now: Instant) {
        self.watched_prs
            .retain(|_, pr| now.saturating_duration_since(pr.first_seen) < cap);
    }

    pub fn push_warning(&mut self, msg: String) {
        if self.warnings.len() >= Self::MAX_WARNINGS {
            self.warnings.remove(0);
        }
        self.warnings.push(msg);
    }

    pub fn clear_warnings_by_prefix(&mut self, prefix: &str) {
        self.warnings.retain(|w| !w.starts_with(prefix));
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
        let pipeline_names: HashSet<&str> = self
            .pipeline_groups
            .iter()
            .map(|pg| pg.name.as_str())
            .collect();
        let workflow_names: HashSet<&str> = self
            .workflow_groups
            .iter()
            .map(|wg| wg.name.as_str())
            .collect();
        self.linkage_broken = link_map.links().iter().any(|l| {
            !pipeline_names.contains(l.pipeline_name.as_str())
                || !workflow_names.contains(l.workflow_name.as_str())
        });
    }

    pub fn has_any_running(&self) -> bool {
        self.pipeline_groups.iter().any(|g| {
            g.summary_status == BuildStatus::Running
                || g.stages.iter().any(|s| s.status == BuildStatus::Running)
                || g.pending_link
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
    fn has_any_running_true_when_pending_link() {
        let mut app = App::new();
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: true,
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

    #[test]
    fn new_app_has_no_push_signal() {
        let app = App::new();
        assert!(app.push_signal_at.is_none());
    }

    #[test]
    fn clear_warnings_by_prefix_removes_matching() {
        let mut app = App::new();
        app.push_warning("AWS: SSO expired".to_string());
        app.push_warning("AWS: timeout".to_string());
        app.clear_warnings_by_prefix("AWS:");
        assert!(app.warnings.is_empty());
    }

    #[test]
    fn clear_warnings_by_prefix_keeps_others() {
        let mut app = App::new();
        app.push_warning("AWS: SSO expired".to_string());
        app.push_warning("GitHub: rate limited".to_string());
        app.clear_warnings_by_prefix("AWS:");
        assert_eq!(app.warnings.len(), 1);
        assert_eq!(app.warnings[0], "GitHub: rate limited");
    }

    #[test]
    fn clear_warnings_by_prefix_no_match_noop() {
        let mut app = App::new();
        app.push_warning("GitHub: rate limited".to_string());
        app.clear_warnings_by_prefix("AWS:");
        assert_eq!(app.warnings.len(), 1);
    }

    #[test]
    fn clear_warnings_by_prefix_empty_warnings() {
        let mut app = App::new();
        app.clear_warnings_by_prefix("AWS:");
        assert!(app.warnings.is_empty());
    }

    // --- T4: watched_prs tests ---

    use crate::model::{WatchedPr, WatchedPrState};
    use std::time::{Duration, Instant};

    #[test]
    fn app_starts_with_empty_watched_prs() {
        let app = App::new();
        assert!(app.watched_prs.is_empty());
    }

    #[test]
    fn add_or_update_watched_prs_inserts_new_as_open_now() {
        let mut app = App::new();
        let now = Instant::now();
        app.add_or_update_watched_prs(&[7, 9], now);
        assert_eq!(app.watched_prs.len(), 2);
        assert_eq!(app.watched_prs[&7].state, WatchedPrState::Open);
        assert_eq!(app.watched_prs[&9].state, WatchedPrState::Open);
        assert_eq!(app.watched_prs[&7].first_seen, now);
    }

    #[test]
    fn add_or_update_watched_prs_preserves_first_seen_for_existing() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.add_or_update_watched_prs(&[7], t0);
        let t1 = t0 + Duration::from_secs(60);
        app.add_or_update_watched_prs(&[7], t1);
        assert_eq!(
            app.watched_prs[&7].first_seen, t0,
            "re-adding must not reset first_seen"
        );
    }

    #[test]
    fn add_or_update_watched_prs_preserves_state_for_existing() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.add_or_update_watched_prs(&[7], t0);
        // Simulate a state transition that happened between polls.
        app.watched_prs.get_mut(&7).unwrap().state = WatchedPrState::Merged;
        app.add_or_update_watched_prs(&[7], t0 + Duration::from_secs(30));
        assert_eq!(
            app.watched_prs[&7].state,
            WatchedPrState::Merged,
            "re-adding must not overwrite existing state"
        );
    }

    #[test]
    fn has_watched_open_prs_false_when_empty() {
        let app = App::new();
        assert!(!app.has_watched_open_prs(Duration::from_secs(3600), Instant::now()));
    }

    #[test]
    fn has_watched_open_prs_true_for_fresh_open_pr() {
        let mut app = App::new();
        let now = Instant::now();
        app.add_or_update_watched_prs(&[7], now);
        assert!(app.has_watched_open_prs(Duration::from_secs(3600), now));
    }

    #[test]
    fn has_watched_open_prs_false_for_merged_pr() {
        let mut app = App::new();
        let now = Instant::now();
        app.add_or_update_watched_prs(&[7], now);
        app.watched_prs.get_mut(&7).unwrap().state = WatchedPrState::Merged;
        assert!(!app.has_watched_open_prs(Duration::from_secs(3600), now));
    }

    #[test]
    fn has_watched_open_prs_false_for_closed_unmerged_pr() {
        let mut app = App::new();
        let now = Instant::now();
        app.add_or_update_watched_prs(&[7], now);
        app.watched_prs.get_mut(&7).unwrap().state = WatchedPrState::ClosedUnmerged;
        assert!(!app.has_watched_open_prs(Duration::from_secs(3600), now));
    }

    #[test]
    fn has_watched_open_prs_false_when_all_expired() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.add_or_update_watched_prs(&[7], t0);
        let cap = Duration::from_secs(3600);
        let later = t0 + cap + Duration::from_secs(1);
        assert!(!app.has_watched_open_prs(cap, later));
    }

    #[test]
    fn prune_expired_prs_removes_only_expired() {
        let mut app = App::new();
        let t0 = Instant::now();
        app.add_or_update_watched_prs(&[7], t0);
        let recent = t0 + Duration::from_secs(100);
        app.add_or_update_watched_prs(&[9], recent);
        let cap = Duration::from_secs(3600);
        let later = t0 + cap + Duration::from_secs(1);
        app.prune_expired_prs(cap, later);
        assert!(!app.watched_prs.contains_key(&7));
        assert!(app.watched_prs.contains_key(&9));
    }

    #[test]
    fn prune_expired_prs_empty_noop() {
        let mut app = App::new();
        app.prune_expired_prs(Duration::from_secs(3600), Instant::now());
        assert!(app.watched_prs.is_empty());
    }

    // Silence unused-variant warning in test builds.
    #[allow(dead_code)]
    fn _watched_pr_use(_: WatchedPr) {}
}
