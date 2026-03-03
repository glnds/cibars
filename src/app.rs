use chrono::{DateTime, Utc};

use crate::model::Bar;

pub struct App {
    pub bars_pipelines: Vec<Bar>,
    pub bars_actions: Vec<Bar>,
    pub last_poll: Option<DateTime<Utc>>,
    pub warnings: Vec<String>,
}

impl App {
    pub fn new() -> Self {
        Self {
            bars_pipelines: Vec::new(),
            bars_actions: Vec::new(),
            last_poll: None,
            warnings: Vec::new(),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
