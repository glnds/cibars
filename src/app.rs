use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::model::{Bar, WorkflowGroup};

pub struct App {
    pub bars_pipelines: Vec<Bar>,
    pub workflow_groups: Vec<WorkflowGroup>,
    pub actions_expanded: bool,
    pub last_poll: Option<DateTime<Utc>>,
    pub warnings: Vec<String>,
    /// Terminal width (updated each UI render), used by poller for tick_area_width.
    pub terminal_width: u16,
    /// If set, skip GitHub polling until this instant (rate limit back-off).
    pub rate_limited_until: Option<Instant>,
}

impl App {
    pub fn new() -> Self {
        Self {
            bars_pipelines: Vec::new(),
            workflow_groups: Vec::new(),
            actions_expanded: true,
            last_poll: None,
            warnings: Vec::new(),
            terminal_width: 80,
            rate_limited_until: None,
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
