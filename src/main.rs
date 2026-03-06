mod app;
mod config;
mod model;
mod poll_scheduler;
mod poller;
mod ui;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::signal::unix::{signal, SignalKind};

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
    boost_notify: Arc<tokio::sync::Notify>,
    mut sigusr1: tokio::signal::unix::Signal,
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
        let prev_state = scheduler.state();
        scheduler.transition(any_running);
        {
            let mut a = app.lock().expect("app mutex poisoned");
            if scheduler.state() == poll_scheduler::PollState::Active
                && prev_state != poll_scheduler::PollState::Active
            {
                a.reset_all_bars();
            }
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

        // Sleep, interruptible by boost key or SIGUSR1
        let interval = scheduler.interval();
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = boost_notify.notified() => {
                scheduler.boost();
            }
            _ = sigusr1.recv() => {
                scheduler.boost();
                tracing::info!("boost triggered by SIGUSR1");
            }
        }
    }
}

fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    setup_tracing()?;

    let cwd = std::env::current_dir().context("cannot read cwd")?;
    let (config, token) = Config::load(&cwd)?;
    tracing::info!("starting cibars");
    let mut app_state = App::new();
    app_state.hook_status = config::check_pre_push_hook(&cwd);
    let app = Arc::new(Mutex::new(app_state));

    // Build tokio runtime for async polling
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    // Notify for boost (manual poll trigger, zero overhead)
    let boost_notify = Arc::new(tokio::sync::Notify::new());

    // SIGTERM handling: set flag checked by UI loop
    let term_flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term_flag))
        .context("failed to register SIGTERM handler")?;

    // SIGUSR1 handling: external boost trigger (e.g. git pre-push hook)
    let sigusr1 =
        signal(SignalKind::user_defined1()).context("failed to register SIGUSR1 handler")?;

    // Spawn single poll orchestrator
    let poll_app = app.clone();
    let poll_config = config.clone();
    let poll_boost = boost_notify.clone();
    rt.spawn(async move {
        if let Err(e) =
            run_poll_orchestrator(poll_app, poll_config, token, poll_boost, sigusr1).await
        {
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
        boost_notify,
        &term_flag,
    );
    ratatui::restore();

    tracing::info!("shutting down");
    result
}
