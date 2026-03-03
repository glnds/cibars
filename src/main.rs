mod app;
mod config;
mod model;
mod poller;
mod ui;

use std::sync::{Arc, Mutex};

use anyhow::Result;

use app::App;
use config::Config;

fn main() -> Result<()> {
    let (config, token) = Config::load()?;

    let app = Arc::new(Mutex::new(App::new()));

    // Build tokio runtime for async polling
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    // TODO: spawn poller task (Task 10)

    // Init TUI and run event loop on main thread
    let terminal = ratatui::init();
    let result = ui::run_ui(app.clone(), terminal);
    ratatui::restore();

    // Suppress unused warnings until pollers wired
    let _ = (config, token);

    result
}
