#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStatus {
    Running,
    Succeeded,
    Failed,
    Idle,
}

impl BuildStatus {
    pub fn color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            Self::Running => Color::Yellow,
            Self::Succeeded => Color::Green,
            Self::Failed => Color::Red,
            Self::Idle => Color::DarkGray,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Bar {
    pub name: String,
    pub status: BuildStatus,
    /// Number of '|' chars filled in current lap
    pub fill: usize,
    /// Current insertion index (resets on wrap)
    pub write_pos: usize,
    /// Pipeline/workflow disappeared from API
    pub gone: bool,
}

impl Bar {
    pub fn new(name: String) -> Self {
        Self {
            name,
            status: BuildStatus::Idle,
            fill: 0,
            write_pos: 0,
            gone: false,
        }
    }

    /// Update bar status from a poll result.
    /// Resets fill/write_pos on transition into Running (new execution).
    /// Idle is a no-op (preserves current state).
    pub fn set_status(&mut self, status: BuildStatus) {
        match status {
            BuildStatus::Running => {
                if self.status != BuildStatus::Running {
                    tracing::debug!(bar = %self.name, from = ?self.status, "→ Running");
                    self.fill = 0;
                    self.write_pos = 0;
                }
                self.status = BuildStatus::Running;
            }
            BuildStatus::Succeeded | BuildStatus::Failed => {
                if self.status != status {
                    tracing::debug!(bar = %self.name, from = ?self.status, to = ?status, "status change");
                }
                self.status = status;
            }
            BuildStatus::Idle => {
                if self.status == BuildStatus::Running {
                    tracing::warn!(bar = %self.name, "Idle received while Running");
                }
            }
        }
    }

    /// Advance animation for Running bars. Called by UI every ~1s.
    pub fn tick(&mut self, tick_area_width: usize) {
        if self.status != BuildStatus::Running || tick_area_width == 0 {
            return;
        }
        self.advance_fill(tick_area_width);
    }

    fn advance_fill(&mut self, tick_area_width: usize) {
        self.write_pos += 1;
        if self.write_pos >= tick_area_width {
            self.write_pos = 0;
            self.fill = 0;
        }
        self.fill += 1;
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowGroup {
    pub name: String,
    pub jobs: Vec<Bar>,
    pub gone: bool,
    /// Workflow-level status from runs API; shown as dot before jobs load.
    pub summary_status: BuildStatus,
}

#[derive(Debug, Clone)]
pub struct PipelineGroup {
    pub name: String,
    pub stages: Vec<StageInfo>,
    pub gone: bool,
    pub summary_status: BuildStatus,
}

#[derive(Debug, Clone)]
pub struct StageInfo {
    pub name: String,
    pub actions: Vec<Bar>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bar() -> Bar {
        Bar::new("test-pipe".to_string())
    }

    #[test]
    fn new_bar_is_idle() {
        let bar = make_bar();
        assert_eq!(bar.status, BuildStatus::Idle);
        assert_eq!(bar.fill, 0);
        assert_eq!(bar.write_pos, 0);
        assert!(!bar.gone);
    }

    // --- set_status tests ---

    #[test]
    fn set_status_idle_to_running_resets() {
        let mut bar = make_bar();
        bar.fill = 5;
        bar.write_pos = 5;
        bar.set_status(BuildStatus::Running);
        assert_eq!(bar.status, BuildStatus::Running);
        assert_eq!(bar.fill, 0);
        assert_eq!(bar.write_pos, 0);
    }

    #[test]
    fn set_status_running_while_running_no_reset() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        bar.tick(10);
        bar.tick(10);
        assert_eq!(bar.fill, 2);
        bar.set_status(BuildStatus::Running);
        assert_eq!(bar.fill, 2);
        assert_eq!(bar.write_pos, 2);
    }

    #[test]
    fn set_status_succeeded_from_running() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        bar.tick(10);
        bar.set_status(BuildStatus::Succeeded);
        assert_eq!(bar.status, BuildStatus::Succeeded);
        assert_eq!(bar.fill, 1);
    }

    #[test]
    fn set_status_failed_from_running() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        bar.tick(10);
        bar.set_status(BuildStatus::Failed);
        assert_eq!(bar.status, BuildStatus::Failed);
        assert_eq!(bar.fill, 1);
    }

    #[test]
    fn set_status_idle_is_noop() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        bar.tick(10);
        let fill_before = bar.fill;
        let pos_before = bar.write_pos;
        bar.set_status(BuildStatus::Idle);
        assert_eq!(bar.fill, fill_before);
        assert_eq!(bar.write_pos, pos_before);
        assert_eq!(bar.status, BuildStatus::Running);
    }

    #[test]
    fn set_status_resets_on_new_execution() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        bar.tick(10);
        bar.tick(10);
        bar.set_status(BuildStatus::Succeeded);
        assert_eq!(bar.fill, 2);
        // New execution starts
        bar.set_status(BuildStatus::Running);
        assert_eq!(bar.fill, 0);
        assert_eq!(bar.write_pos, 0);
    }

    // --- tick tests ---

    #[test]
    fn tick_advances_running_bar() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        bar.tick(10);
        assert_eq!(bar.fill, 1);
        assert_eq!(bar.write_pos, 1);
        bar.tick(10);
        assert_eq!(bar.fill, 2);
        assert_eq!(bar.write_pos, 2);
    }

    #[test]
    fn tick_wraps_at_width() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        let width = 3;
        for _ in 0..3 {
            bar.tick(width);
        }
        assert_eq!(bar.write_pos, 0);
        assert_eq!(bar.fill, 1);
        assert_eq!(bar.status, BuildStatus::Running);
    }

    #[test]
    fn tick_wrap_resets_then_continues() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        let width = 2;
        bar.tick(width);
        bar.tick(width);
        assert_eq!(bar.write_pos, 0);
        assert_eq!(bar.fill, 1);
        bar.tick(width);
        assert_eq!(bar.write_pos, 1);
        assert_eq!(bar.fill, 2);
    }

    #[test]
    fn tick_noop_for_idle() {
        let mut bar = make_bar();
        bar.tick(10);
        assert_eq!(bar.fill, 0);
        assert_eq!(bar.write_pos, 0);
    }

    #[test]
    fn tick_noop_for_succeeded() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        bar.tick(10);
        bar.set_status(BuildStatus::Succeeded);
        bar.tick(10);
        assert_eq!(bar.fill, 1);
        assert_eq!(bar.write_pos, 1);
    }

    #[test]
    fn tick_noop_for_failed() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        bar.tick(10);
        bar.set_status(BuildStatus::Failed);
        bar.tick(10);
        assert_eq!(bar.fill, 1);
    }

    #[test]
    fn tick_noop_for_zero_width() {
        let mut bar = make_bar();
        bar.set_status(BuildStatus::Running);
        bar.tick(0);
        assert_eq!(bar.fill, 0);
        assert_eq!(bar.write_pos, 0);
    }

    // --- WorkflowGroup tests ---

    #[test]
    fn workflow_group_new() {
        let group = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
        };
        assert_eq!(group.name, "CI");
        assert!(group.jobs.is_empty());
        assert!(!group.gone);
    }

    #[test]
    fn workflow_group_with_jobs() {
        let group = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![Bar::new("build".to_string()), Bar::new("test".to_string())],
            gone: false,
            summary_status: BuildStatus::Running,
        };
        assert_eq!(group.jobs.len(), 2);
        assert_eq!(group.jobs[0].name, "build");
        assert_eq!(group.jobs[1].name, "test");
    }

    #[test]
    fn workflow_group_gone_marking() {
        let mut group = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
        };
        group.gone = true;
        assert!(group.gone);
    }

    // --- PipelineGroup tests ---

    #[test]
    fn pipeline_group_new() {
        let group = PipelineGroup {
            name: "deploy-pipe".to_string(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
        };
        assert_eq!(group.name, "deploy-pipe");
        assert!(group.stages.is_empty());
        assert!(!group.gone);
    }

    #[test]
    fn pipeline_group_with_stages_and_actions() {
        let group = PipelineGroup {
            name: "my-pipeline".to_string(),
            stages: vec![
                StageInfo {
                    name: "Source".to_string(),
                    actions: vec![Bar::new("checkout".to_string())],
                },
                StageInfo {
                    name: "Build".to_string(),
                    actions: vec![
                        Bar::new("compile".to_string()),
                        Bar::new("test".to_string()),
                    ],
                },
            ],
            gone: false,
            summary_status: BuildStatus::Running,
        };
        assert_eq!(group.stages.len(), 2);
        assert_eq!(group.stages[0].name, "Source");
        assert_eq!(group.stages[0].actions.len(), 1);
        assert_eq!(group.stages[1].name, "Build");
        assert_eq!(group.stages[1].actions.len(), 2);
        assert_eq!(group.stages[1].actions[1].name, "test");
    }

    #[test]
    fn pipeline_group_gone_marking() {
        let mut group = PipelineGroup {
            name: "old-pipe".to_string(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
        };
        group.gone = true;
        assert!(group.gone);
    }
}
