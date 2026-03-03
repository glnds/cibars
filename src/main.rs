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

    // Spawn poller task
    let poll_app = app.clone();
    let config_clone = config.clone();
    let token_clone = token.clone();
    rt.spawn(async move {
        let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .profile_name(&config_clone.aws_profile)
            .region(aws_config::Region::new(config_clone.region.clone()))
            .load()
            .await;

        let pipe_client =
            poller::aws::AwsPipelineClient::new(aws_sdk_codepipeline::Client::new(&aws_config));

        let (owner, repo) = config_clone
            .github_repo
            .split_once('/')
            .expect("validated in config");
        let actions_client = poller::github::GitHubActionsClient::new(
            &token_clone,
            owner.to_string(),
            repo.to_string(),
        )
        .expect("failed to create GitHub client");

        loop {
            poller::poll_once(&poll_app, &pipe_client, &actions_client, 60).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });

    // Init TUI and run event loop on main thread
    let terminal = ratatui::init();
    let result = ui::run_ui(
        app.clone(),
        terminal,
        &config.aws_profile,
        &config.region,
        &config.github_repo,
    );
    ratatui::restore();

    result
}
