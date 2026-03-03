#[derive(Debug, Clone, PartialEq)]
pub enum BuildStatus {
    Running,
    Succeeded,
    Failed,
    Idle,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BarSource {
    CodePipeline,
    GitHubAction,
}

#[derive(Debug, Clone)]
pub struct Bar {
    pub name: String,
    pub source: BarSource,
    pub status: BuildStatus,
    /// Number of '|' chars filled in current lap
    pub fill: usize,
    /// Current insertion index (resets on wrap)
    pub write_pos: usize,
    /// Pipeline/workflow disappeared from API
    pub gone: bool,
}

impl Bar {
    pub fn new(name: String, source: BarSource) -> Self {
        Self {
            name,
            source,
            status: BuildStatus::Idle,
            fill: 0,
            write_pos: 0,
            gone: false,
        }
    }

    /// Update bar state based on polled build status.
    /// `tick_area_width` is the available fill area.
    pub fn update(&mut self, status: BuildStatus, tick_area_width: usize) {
        match status {
            BuildStatus::Running => {
                self.status = BuildStatus::Running;
                self.write_pos += 1;
                if self.write_pos >= tick_area_width {
                    self.write_pos = 0;
                    self.fill = 0;
                }
                self.fill += 1;
            }
            BuildStatus::Succeeded | BuildStatus::Failed => {
                self.status = status;
            }
            BuildStatus::Idle => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bar(source: BarSource) -> Bar {
        Bar::new("test-pipe".to_string(), source)
    }

    #[test]
    fn new_bar_is_idle() {
        let bar = make_bar(BarSource::CodePipeline);
        assert_eq!(bar.status, BuildStatus::Idle);
        assert_eq!(bar.fill, 0);
        assert_eq!(bar.write_pos, 0);
        assert!(!bar.gone);
    }

    #[test]
    fn running_increments_fill_and_pos() {
        let mut bar = make_bar(BarSource::CodePipeline);
        bar.update(BuildStatus::Running, 10);
        assert_eq!(bar.status, BuildStatus::Running);
        assert_eq!(bar.fill, 1);
        assert_eq!(bar.write_pos, 1);
        bar.update(BuildStatus::Running, 10);
        assert_eq!(bar.fill, 2);
        assert_eq!(bar.write_pos, 2);
    }

    #[test]
    fn running_wraps_at_width() {
        let mut bar = make_bar(BarSource::CodePipeline);
        let width = 3;
        for _ in 0..3 {
            bar.update(BuildStatus::Running, width);
        }
        assert_eq!(bar.write_pos, 0);
        assert_eq!(bar.fill, 1);
        assert_eq!(bar.status, BuildStatus::Running);
    }

    #[test]
    fn wrap_resets_then_continues() {
        let mut bar = make_bar(BarSource::GitHubAction);
        let width = 2;
        bar.update(BuildStatus::Running, width);
        bar.update(BuildStatus::Running, width);
        assert_eq!(bar.write_pos, 0);
        assert_eq!(bar.fill, 1);
        bar.update(BuildStatus::Running, width);
        assert_eq!(bar.write_pos, 1);
        assert_eq!(bar.fill, 2);
    }

    #[test]
    fn succeeded_freezes_bar() {
        let mut bar = make_bar(BarSource::CodePipeline);
        bar.update(BuildStatus::Running, 10);
        bar.update(BuildStatus::Running, 10);
        bar.update(BuildStatus::Succeeded, 10);
        assert_eq!(bar.status, BuildStatus::Succeeded);
        assert_eq!(bar.fill, 2);
        assert_eq!(bar.write_pos, 2);
    }

    #[test]
    fn failed_freezes_bar() {
        let mut bar = make_bar(BarSource::CodePipeline);
        bar.update(BuildStatus::Running, 10);
        bar.update(BuildStatus::Failed, 10);
        assert_eq!(bar.status, BuildStatus::Failed);
        assert_eq!(bar.fill, 1);
        assert_eq!(bar.write_pos, 1);
    }

    #[test]
    fn idle_leaves_bar_unchanged() {
        let mut bar = make_bar(BarSource::CodePipeline);
        bar.update(BuildStatus::Running, 10);
        let fill_before = bar.fill;
        let pos_before = bar.write_pos;
        bar.update(BuildStatus::Idle, 10);
        assert_eq!(bar.fill, fill_before);
        assert_eq!(bar.write_pos, pos_before);
        assert_eq!(bar.status, BuildStatus::Running);
    }
}
