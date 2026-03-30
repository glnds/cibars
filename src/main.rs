mod app;
mod config;
mod linkage;
mod model;
mod poll_scheduler;
mod poller;
mod ui;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::signal::unix::{signal, SignalKind};

use anyhow::{Context, Result};

use app::{App, SourceHealth};
use chrono::Utc;
use config::Config;
use linkage::LinkMap;
use poll_scheduler::{PollScheduler, PollState};

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

async fn init_aws_client(
    config: &Config,
) -> (poller::aws::AwsPipelineClient, poller::aws::StsAuthClient) {
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

    let pipeline_client =
        poller::aws::AwsPipelineClient::new(aws_sdk_codepipeline::Client::new(&aws_config));
    let sts_client = poller::aws::StsAuthClient::new(aws_sdk_sts::Client::new(&aws_config));
    (pipeline_client, sts_client)
}

async fn run_poll_orchestrator(
    app: Arc<Mutex<App>>,
    config: Config,
    token: String,
    boost_notify: Arc<tokio::sync::Notify>,
    link_notify: Arc<tokio::sync::Notify>,
    mut sigusr1: tokio::signal::unix::Signal,
    cwd: std::path::PathBuf,
) -> Result<()> {
    let (owner, repo) = config
        .github_repo
        .split_once('/')
        .context("github-repo missing '/'; should have been validated in config")?;
    let gh_client = poller::github::GitHubActionsClient::new(
        &token,
        owner.to_string(),
        repo.to_string(),
        config.branch.clone(),
    )?;

    let mut aws_client: Option<poller::aws::AwsPipelineClient> = None;
    let mut sts_client: Option<poller::aws::StsAuthClient> = None;
    let mut scheduler = PollScheduler::new();
    let mut link_map = LinkMap::new();
    let cache_path = cwd.join(".cibars-links.toml");
    let mut stopped_runs = std::collections::HashMap::new();
    let mut force_next_aws = false;

    loop {
        let cycle_start = Instant::now();

        let force_aws = force_next_aws || {
            let a = app.lock().expect("app mutex poisoned");
            match &a.aws_health {
                SourceHealth::AuthFailed { since } => {
                    let elapsed = Utc::now()
                        .signed_duration_since(*since)
                        .num_seconds()
                        .unsigned_abs();
                    elapsed >= 300 // 5 minutes
                }
                SourceHealth::Unknown | SourceHealth::Healthy => false,
            }
        };
        force_next_aws = false; // consumed

        let need_aws = scheduler.should_poll_aws();
        let poll_aws = need_aws || force_aws;

        // Lazy-init AWS on first need + cache-first link discovery
        if need_aws && aws_client.is_none() {
            tracing::info!("initializing AWS client (first active poll)");
            let (client, sts) = init_aws_client(&config).await;
            sts_client = Some(sts);

            // Cache-first: try loading from disk
            match linkage::load_link_cache(&cache_path) {
                Ok(Some(cache)) => {
                    let cached_assignment = cache.job_assignment.clone();
                    link_map.load_from_cache(cache);
                    linkage::sync_linked_pipelines(&app, &link_map);
                    if let Some(assignment) = cached_assignment {
                        app.lock().expect("app mutex poisoned").job_assignment = Some(assignment);
                    }
                    tracing::info!(
                        links = link_map.links().len(),
                        "loaded link cache from disk"
                    );
                }
                Ok(None) => {
                    link_map = linkage::run_discovery(&client, &gh_client, &app, &cache_path).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "corrupt link cache, re-discovering");
                    let _ = std::fs::remove_file(&cache_path);
                    link_map = linkage::run_discovery(&client, &gh_client, &app, &cache_path).await;
                }
            }

            aws_client = Some(client);
        }

        // Poll with boost/SIGUSR1 interruption — if a signal arrives
        // mid-API-call, cancel the poll and restart in Watching mode.
        let source = tokio::select! {
            biased;
            _ = boost_notify.notified() => Some(InterruptSource::Boost),
            _ = sigusr1.recv() => Some(InterruptSource::Sigusr1),
            _ = async {
                if let Some(aws) = aws_client.as_ref().filter(|_| poll_aws) {
                    tokio::join!(
                        poller::poll_actions_tick(&app, &gh_client),
                        poller::poll_pipelines_tick(&app, aws, &config.aws_profile, force_aws),
                    );
                } else if let Some(sts) = sts_client.as_ref()
                    .filter(|_| scheduler.state() == PollState::LongIdle)
                {
                    tokio::join!(
                        poller::poll_actions_tick(&app, &gh_client),
                        poller::check_aws_auth(&app, sts, &config.aws_profile),
                    );
                } else {
                    poller::poll_actions_tick(&app, &gh_client).await;
                }
            } => None,
        };

        if let Some(source) = source {
            handle_poll_interrupt(&app, &mut scheduler, source);
            force_next_aws = true;
            tracing::info!(state = ?scheduler.state(), ?source, "boost interrupted poll");
            continue;
        }

        // Classify workflows by category (CI vs Review)
        {
            let mut a = app.lock().expect("app mutex poisoned");
            poller::classify_workflows(&mut a, &config);
        }

        // Apply linkage: mark GH workflows as Succeeded when linked CP starts Running
        linkage::apply_links(&app, &mut link_map, &mut stopped_runs);

        // Sync linked_pipeline on workflow groups and check health
        linkage::sync_linked_pipelines(&app, &link_map);
        {
            let mut a = app.lock().expect("app mutex poisoned");
            a.check_linkage_health(&link_map);
        }

        // Transition + update App display state
        let any_running = app.lock().expect("app mutex poisoned").has_any_running();
        scheduler.transition(any_running);
        {
            let mut a = app.lock().expect("app mutex poisoned");
            a.poll_state = scheduler.state();
            a.cooldown_remaining = scheduler.cooldown_remaining();
        }

        // Sleep only the remaining interval after poll duration
        let remaining = scheduler.interval().saturating_sub(cycle_start.elapsed());

        tracing::info!(
            state = ?scheduler.state(),
            any_running,
            interval = ?scheduler.interval(),
            ?remaining,
            "poll cycle complete"
        );
        tokio::select! {
            biased;
            _ = boost_notify.notified() => {
                handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
                force_next_aws = true;
                tracing::info!(state = ?scheduler.state(), "boost triggered by key");
            }
            _ = sigusr1.recv() => {
                handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
                force_next_aws = true;
                tracing::info!(state = ?scheduler.state(), "boost triggered by SIGUSR1");
            }
            _ = link_notify.notified() => {
                tracing::info!("link re-discovery triggered by 'l' key");
                link_map.clear();
                if let Some(aws) = aws_client.as_ref() {
                    link_map = linkage::run_discovery(aws, &gh_client, &app, &cache_path).await;
                }
                {
                    let mut a = app.lock().expect("app mutex poisoned");
                    a.linkage_broken = false;
                    a.check_linkage_health(&link_map);
                }
            }
            _ = tokio::time::sleep(remaining) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterruptSource {
    Boost,
    Sigusr1,
}

fn write_pid_file(pid_path: &std::path::Path) -> Result<()> {
    std::fs::write(pid_path, std::process::id().to_string())
        .with_context(|| format!("cannot write PID file {}", pid_path.display()))?;
    Ok(())
}

fn cleanup_pid_file(pid_path: &std::path::Path) {
    let _ = std::fs::remove_file(pid_path);
}

fn handle_poll_interrupt(
    app: &Arc<Mutex<App>>,
    scheduler: &mut PollScheduler,
    source: InterruptSource,
) {
    scheduler.boost();
    let mut a = app.lock().expect("app mutex poisoned");
    a.poll_state = scheduler.state();
    if source == InterruptSource::Sigusr1 {
        a.push_signal_at = Some(Instant::now());
    }
}

fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    setup_tracing()?;

    let cwd = std::env::current_dir().context("cannot read cwd")?;
    let (config, token) = Config::load(&cwd)?;
    let pid_path = dirs::home_dir()
        .context("no home dir")?
        .join(".cibars/cibars.pid");
    write_pid_file(&pid_path)?;
    tracing::info!(pid = std::process::id(), "starting cibars");
    let mut app_state = App::new();
    app_state.hook_status = config::check_pre_push_hook(&cwd);
    let app = Arc::new(Mutex::new(app_state));

    // Build tokio runtime for async polling
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    // Notify for boost (manual poll trigger, zero overhead)
    let boost_notify = Arc::new(tokio::sync::Notify::new());
    let link_notify = Arc::new(tokio::sync::Notify::new());

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
    let poll_link = link_notify.clone();
    let poll_cwd = cwd.clone();
    rt.spawn(async move {
        if let Err(e) = run_poll_orchestrator(
            poll_app,
            poll_config,
            token,
            poll_boost,
            poll_link,
            sigusr1,
            poll_cwd,
        )
        .await
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
        link_notify,
        &term_flag,
    );
    ratatui::restore();

    cleanup_pid_file(&pid_path);
    tracing::info!("shutting down");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn write_pid_file_creates_file_with_pid() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        write_pid_file(&pid_path).unwrap();
        let content = std::fs::read_to_string(&pid_path).unwrap();
        assert_eq!(content, std::process::id().to_string());
    }

    #[test]
    fn cleanup_pid_file_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        std::fs::write(&pid_path, "12345").unwrap();
        assert!(pid_path.exists());
        cleanup_pid_file(&pid_path);
        assert!(!pid_path.exists());
    }

    #[test]
    fn cleanup_pid_file_noop_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        cleanup_pid_file(&pid_path); // should not panic
    }

    #[test]
    fn handle_interrupt_sigusr1_sets_push_signal() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
        assert!(app.lock().unwrap().push_signal_at.is_some());
    }

    #[test]
    fn handle_interrupt_boost_no_push_signal() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        assert!(app.lock().unwrap().push_signal_at.is_none());
    }

    #[test]
    fn handle_interrupt_transitions_to_watching() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        assert_eq!(scheduler.state(), PollState::Idle);
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        assert_eq!(scheduler.state(), PollState::Watching);
    }

    #[test]
    fn handle_interrupt_updates_app_poll_state() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
        let a = app.lock().unwrap();
        assert_eq!(a.poll_state, scheduler.state());
    }
}
