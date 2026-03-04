mod app;
mod config;
mod model;
mod poll_scheduler;
mod poller;
mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use app::App;
use config::Config;
use poll_scheduler::PollScheduler;

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

async fn init_aws_client(config: &Config) -> poller::aws::AwsPipelineClient {
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

    poller::aws::AwsPipelineClient::new(aws_sdk_codepipeline::Client::new(&aws_config))
}

async fn run_poll_orchestrator(
    app: Arc<Mutex<App>>,
    config: Config,
    token: String,
    boost_flag: Arc<AtomicBool>,
) -> Result<()> {
    let (owner, repo) = config
        .github_repo
        .split_once('/')
        .context("github-repo missing '/'; should have been validated in config")?;
    let gh_client =
        poller::github::GitHubActionsClient::new(&token, owner.to_string(), repo.to_string())?;

    let mut aws_client: Option<poller::aws::AwsPipelineClient> = None;
    let mut scheduler = PollScheduler::new();

    loop {
        // Consume boost flag (atomic swap, no race)
        if boost_flag.swap(false, Ordering::Relaxed) {
            scheduler.boost();
        }
        let need_aws = scheduler.should_poll_aws();

        // Lazy-init AWS on first need
        if need_aws && aws_client.is_none() {
            tracing::info!("initializing AWS client (first active poll)");
            aws_client = Some(init_aws_client(&config).await);
        }

        // Poll: parallel when both, GH-only otherwise
        if let Some(aws) = aws_client.as_ref().filter(|_| need_aws) {
            tokio::join!(
                poller::poll_actions_tick(&app, &gh_client),
                poller::poll_pipelines_tick(&app, aws, &config.aws_profile),
            );
        } else {
            poller::poll_actions_tick(&app, &gh_client).await;
        }

        // Transition + update App display state
        let any_running = app.lock().expect("app mutex poisoned").has_any_running();
        scheduler.transition(any_running);
        {
            let mut a = app.lock().expect("app mutex poisoned");
            a.poll_state = scheduler.state();
            a.cooldown_remaining = scheduler.cooldown_remaining();
            // Reset tick bar after poll completes (not before),
            // so elapsed only measures sleep time, not poll duration.
            a.last_poll_started = Some(Instant::now());
        }

        tracing::debug!(
            state = ?scheduler.state(),
            any_running,
            interval = ?scheduler.interval(),
            "poll cycle complete"
        );

        // Sleep, interruptible by boost key
        let interval = scheduler.interval();
        let flag = boost_flag.clone();
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                }
            } => {}
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

    // AtomicBool for boost (manual poll trigger, no race)
    let boost_flag = Arc::new(AtomicBool::new(false));

    // SIGTERM handling: set flag checked by UI loop
    let term_flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term_flag))
        .context("failed to register SIGTERM handler")?;

    // Spawn single poll orchestrator
    let poll_app = app.clone();
    let poll_config = config.clone();
    let poll_boost = boost_flag.clone();
    rt.spawn(async move {
        if let Err(e) = run_poll_orchestrator(poll_app, poll_config, token, poll_boost).await {
            tracing::error!("poll orchestrator failed: {e:#}");
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
        boost_flag,
        &term_flag,
    );
    ratatui::restore();

    tracing::info!("shutting down");
    result
}
