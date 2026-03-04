mod app;
mod config;
mod model;
mod poller;
mod ui;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use app::App;
use config::Config;

fn setup_tracing() -> Result<()> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let log_dir = dirs::home_dir().context("no home dir")?.join(".cibars");
    std::fs::create_dir_all(&log_dir)?;
    let log_file = std::fs::File::create(log_dir.join("cibars.log"))?;

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_writer(log_file).with_ansi(false))
        .init();
    Ok(())
}

async fn run_aws_poller(
    app: Arc<Mutex<App>>,
    config: Config,
    mut refresh_rx: tokio::sync::watch::Receiver<()>,
) -> Result<()> {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .profile_name(&config.aws_profile)
        .region(aws_config::Region::new(config.region.clone()))
        .identity_cache(
            aws_config::identity::IdentityCache::lazy()
                .load_timeout(std::time::Duration::from_secs(15))
                .build(),
        )
        .load()
        .await;

    let client =
        poller::aws::AwsPipelineClient::new(aws_sdk_codepipeline::Client::new(&aws_config));

    loop {
        let tick_area_width = {
            let a = app.lock().expect("app mutex poisoned");
            a.terminal_width as usize
        };
        poller::poll_pipelines_tick(&app, &client, tick_area_width, &config.aws_profile).await;
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
            _ = refresh_rx.changed() => {}
        }
    }
}

async fn run_github_poller(
    app: Arc<Mutex<App>>,
    config: Config,
    token: String,
    mut refresh_rx: tokio::sync::watch::Receiver<()>,
) -> Result<()> {
    let (owner, repo) = config
        .github_repo
        .split_once('/')
        .context("github-repo missing '/'; should have been validated in config")?;
    let client =
        poller::github::GitHubActionsClient::new(&token, owner.to_string(), repo.to_string())?;

    loop {
        let tick_area_width = {
            let a = app.lock().expect("app mutex poisoned");
            a.terminal_width as usize
        };
        poller::poll_actions_tick(&app, &client, tick_area_width).await;
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
            _ = refresh_rx.changed() => {}
        }
    }
}

fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    setup_tracing()?;

    let (config, token) = Config::load()?;
    tracing::info!("starting cibars");

    let app = Arc::new(Mutex::new(App::new()));

    // Build tokio runtime for async polling
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    // Watch channel for manual refresh
    let (refresh_tx, refresh_rx) = tokio::sync::watch::channel(());

    // SIGTERM handling: set flag checked by UI loop
    let term_flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term_flag))
        .context("failed to register SIGTERM handler")?;

    // Spawn AWS poller (loads AWS config, then polls pipelines)
    let aws_app = app.clone();
    let aws_config = config.clone();
    let aws_refresh_rx = refresh_rx;
    rt.spawn(async move {
        if let Err(e) = run_aws_poller(aws_app, aws_config, aws_refresh_rx).await {
            tracing::error!("AWS poller failed: {e:#}");
        }
    });

    // Spawn GitHub poller (starts immediately, no slow config loading)
    let gh_app = app.clone();
    let gh_config = config.clone();
    let gh_refresh_rx = refresh_tx.subscribe();
    rt.spawn(async move {
        if let Err(e) = run_github_poller(gh_app, gh_config, token, gh_refresh_rx).await {
            tracing::error!("GitHub poller failed: {e:#}");
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
        refresh_tx,
        &term_flag,
    );
    ratatui::restore();

    tracing::info!("shutting down");
    result
}
