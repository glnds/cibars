use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::model::Bar;

pub struct App {
    pub bars_pipelines: Vec<Bar>,
    pub bars_actions: Vec<Bar>,
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
            bars_actions: Vec::new(),
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
