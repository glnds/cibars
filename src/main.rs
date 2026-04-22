mod app;
mod config;
mod linkage;
mod model;
mod poll_scheduler;
mod poller;
mod sso_health;
mod ui;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::signal::unix::{signal, SignalKind};

use anyhow::{Context, Result};

use app::App;
use config::Config;
use linkage::LinkMap;
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

async fn init_aws_pipeline_client(config: &Config) -> poller::aws::AwsPipelineClient {
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
    let mut scheduler = PollScheduler::new();
    let mut link_map = LinkMap::new();
    let cache_path = cwd.join(".cibars-links.toml");
    let mut stopped_runs = std::collections::HashMap::new();

    bootstrap_initial_poll(&app, &mut scheduler);

    loop {
        if scheduler.is_polling() {
            // Lazy-init AWS + link cache on first Polling tick.
            if aws_client.is_none() {
                tracing::info!("initializing AWS client (first Polling tick)");
                let client = init_aws_pipeline_client(&config).await;
                match linkage::load_link_cache(&cache_path) {
                    Ok(Some(cache)) => {
                        let cached_assignment = cache.job_assignment.clone();
                        link_map.load_from_cache(cache);
                        linkage::sync_linked_pipelines(&app, &link_map);
                        if let Some(assignment) = cached_assignment {
                            app.lock().expect("app mutex poisoned").job_assignment =
                                Some(assignment);
                        }
                        tracing::info!(
                            links = link_map.links().len(),
                            "loaded link cache from disk"
                        );
                    }
                    Ok(None) => {
                        link_map =
                            linkage::run_discovery(&client, &gh_client, &app, &cache_path).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "corrupt link cache, re-discovering");
                        let _ = std::fs::remove_file(&cache_path);
                        link_map =
                            linkage::run_discovery(&client, &gh_client, &app, &cache_path).await;
                    }
                }
                aws_client = Some(client);
            }

            let aws = aws_client
                .as_ref()
                .expect("aws_client just initialized above");
            let cycle_start = Instant::now();

            let interrupt = tokio::select! {
                biased;
                _ = boost_notify.notified() => Some(InterruptSource::Boost),
                _ = sigusr1.recv() => Some(InterruptSource::Sigusr1),
                _ = async {
                    tokio::join!(
                        poller::poll_actions_tick(&app, &gh_client),
                        poller::poll_pipelines_tick(&app, aws, &config.aws_profile, false),
                    );
                } => None,
            };

            if let Some(source) = interrupt {
                handle_poll_interrupt(&app, &mut scheduler, source);
                tracing::info!(state = ?scheduler.state(), ?source, "interrupt during poll");
                continue;
            }

            // Process poll results
            {
                let mut a = app.lock().expect("app mutex poisoned");
                poller::classify_workflows(&mut a, &config);
            }
            linkage::apply_links(&app, &mut link_map, &mut stopped_runs);
            linkage::sync_linked_pipelines(&app, &link_map);

            let any_running = {
                let mut a = app.lock().expect("app mutex poisoned");
                a.check_linkage_health(&link_map);
                let now = Instant::now();
                let any_running = effective_any_running(&a, now);
                scheduler.transition(any_running);
                a.poll_state = scheduler.state();
                any_running
            };

            let remaining = scheduler.interval().saturating_sub(cycle_start.elapsed());
            tracing::info!(
                state = ?scheduler.state(),
                any_running,
                ?remaining,
                "poll cycle complete"
            );

            tokio::select! {
                biased;
                _ = boost_notify.notified() => {
                    handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
                }
                _ = sigusr1.recv() => {
                    handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
                }
                _ = link_notify.notified() => {
                    handle_link_notify(&app, &mut link_map, &gh_client, aws_client.as_ref(), &cache_path).await;
                }
                _ = tokio::time::sleep(remaining) => {}
            }
        } else {
            // Sleep — park on signals, never wake on a timer.
            tokio::select! {
                biased;
                _ = boost_notify.notified() => {
                    handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
                }
                _ = sigusr1.recv() => {
                    handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
                }
                _ = link_notify.notified() => {
                    handle_link_notify(&app, &mut link_map, &gh_client, aws_client.as_ref(), &cache_path).await;
                }
            }
        }
    }
}

async fn handle_link_notify(
    app: &Arc<Mutex<App>>,
    link_map: &mut LinkMap,
    gh_client: &poller::github::GitHubActionsClient,
    aws_client: Option<&poller::aws::AwsPipelineClient>,
    cache_path: &std::path::Path,
) {
    tracing::info!("link re-discovery triggered by 'l' key");
    link_map.clear();
    if let Some(aws) = aws_client {
        *link_map = linkage::run_discovery(aws, gh_client, app, cache_path).await;
    }
    let mut a = app.lock().expect("app mutex poisoned");
    a.linkage_broken = false;
    a.check_linkage_health(link_map);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterruptSource {
    Boost,
    Sigusr1,
}

fn write_pid_file(pid_path: &std::path::Path) -> Result<()> {
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
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

fn bootstrap_initial_poll(app: &Arc<Mutex<App>>, scheduler: &mut PollScheduler) {
    scheduler.boost();
    let mut a = app.lock().expect("app mutex poisoned");
    a.poll_state = scheduler.state();
}

fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    setup_tracing()?;

    let cwd = std::env::current_dir().context("cannot read cwd")?;
    let (config, token) = Config::load(&cwd)?;
    let pid_path = config::pid_file_for(&cwd)?;
    write_pid_file(&pid_path)?;
    tracing::info!(pid = std::process::id(), "starting cibars");
    let mut app_state = App::new();
    config::ensure_delegation(&cwd);
    app_state.hook_status = config::check_pre_push_hook(&cwd);
    app_state.has_global_hooks_path = config::has_global_hooks_path(&cwd);
    let app = Arc::new(Mutex::new(app_state));

    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    let boost_notify = Arc::new(tokio::sync::Notify::new());
    let link_notify = Arc::new(tokio::sync::Notify::new());

    let term_flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term_flag))
        .context("failed to register SIGTERM handler")?;

    let sigusr1 =
        signal(SignalKind::user_defined1()).context("failed to register SIGUSR1 handler")?;

    // SSO health task: independent 5-min loop, runs always.
    rt.spawn(sso_health::run_sso_health_loop(
        app.clone(),
        config.aws_profile.clone(),
        config.region.clone(),
    ));

    // Poll orchestrator
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

/// Combine real build activity with the watched-PR signal. Keeps the
/// scheduler in Active/Cooldown while any harvested PR is still Open within
/// the cap, so a slow GitHub-UI merge doesn't cause Sleep before the
/// downstream master-push run and CodePipeline execution are visible.
fn effective_any_running(app: &App, now: Instant) -> bool {
    app.has_any_running() || app.has_watched_open_prs(poll_scheduler::PR_WATCH_CAP, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poll_scheduler::{PollState, PollingPhase};
    use std::sync::{Arc, Mutex};

    fn polling_phase(state: PollState) -> PollingPhase {
        match state {
            PollState::Polling { phase, .. } => phase,
            other => panic!("expected Polling, got {other:?}"),
        }
    }

    // --- T6: effective_any_running + PR_WATCH_CAP tests ---

    #[test]
    fn effective_any_running_is_false_for_fresh_app() {
        let app = App::new();
        assert!(!effective_any_running(&app, Instant::now()));
    }

    #[test]
    fn effective_any_running_true_when_watched_pr_open() {
        let mut app = App::new();
        app.add_or_update_watched_prs(&[7], Instant::now());
        assert!(effective_any_running(&app, Instant::now()));
    }

    #[test]
    fn effective_any_running_false_when_watched_pr_merged() {
        let mut app = App::new();
        app.add_or_update_watched_prs(&[7], Instant::now());
        app.watched_prs.get_mut(&7).unwrap().state = crate::model::WatchedPrState::Merged;
        assert!(!effective_any_running(&app, Instant::now()));
    }

    #[test]
    fn watched_open_pr_prevents_cooldown_to_sleep() {
        use crate::poll_scheduler::{PollState, PollingPhase, COOLDOWN_DURATION};
        let mut app = App::new();
        let now = Instant::now();
        app.add_or_update_watched_prs(&[7], now);

        let mut scheduler = PollScheduler::new();
        scheduler.boost();
        scheduler.transition(true); // Grace -> Active
        scheduler.transition(false); // Active -> Cooldown
        scheduler.force_phase_since(
            PollingPhase::Cooldown,
            COOLDOWN_DURATION + std::time::Duration::from_secs(1),
        );

        scheduler.transition(effective_any_running(&app, now));
        assert_ne!(
            scheduler.state(),
            PollState::Sleep,
            "scheduler must not enter Sleep while a watched PR is still Open"
        );
    }

    /// E2E: simulates the pre-push hook shell command sending SIGUSR1 via PID file,
    /// then verifies the full chain: signal received → scheduler boosts → Polling{Grace}
    /// state + push_signal_at set.
    #[tokio::test]
    async fn e2e_hook_signal_triggers_polling() {
        use tokio::signal::unix::{signal, SignalKind};

        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        write_pid_file(&pid_path).unwrap();

        let mut sigusr1 = signal(SignalKind::user_defined1()).unwrap();

        let hook_cmd = format!(
            "kill -USR1 $(cat {} 2>/dev/null) 2>/dev/null || true",
            pid_path.display()
        );
        std::process::Command::new("sh")
            .arg("-c")
            .arg(&hook_cmd)
            .status()
            .unwrap();

        let received =
            tokio::time::timeout(std::time::Duration::from_millis(500), sigusr1.recv()).await;
        assert!(
            received.is_ok(),
            "SIGUSR1 not received — hook failed to signal"
        );

        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);

        let a = app.lock().unwrap();
        assert!(matches!(
            a.poll_state,
            PollState::Polling {
                phase: PollingPhase::Grace,
                ..
            }
        ));
        assert!(a.push_signal_at.is_some());
    }

    #[tokio::test]
    async fn e2e_hook_noop_without_pid_file() {
        let cmd = "kill -USR1 $(cat /nonexistent/cibars.pid 2>/dev/null) 2>/dev/null || true";
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .status()
            .unwrap();
        assert!(status.success(), "hook should exit 0 even without PID file");
    }

    #[tokio::test]
    async fn e2e_hook_wrong_pid_does_not_signal() {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigusr1 = signal(SignalKind::user_defined1()).unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(50), sigusr1.recv()).await;

        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("other_project.pid");
        std::fs::write(&pid_path, "999999999").unwrap();

        let hook_cmd = format!(
            "kill -USR1 $(cat {} 2>/dev/null) 2>/dev/null || true",
            pid_path.display()
        );
        std::process::Command::new("sh")
            .arg("-c")
            .arg(&hook_cmd)
            .status()
            .unwrap();

        let received =
            tokio::time::timeout(std::time::Duration::from_millis(200), sigusr1.recv()).await;
        assert!(
            received.is_err(),
            "should NOT receive signal from wrong PID"
        );
    }

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
        cleanup_pid_file(&pid_path);
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
    fn handle_interrupt_transitions_to_grace_from_sleep() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        assert_eq!(scheduler.state(), PollState::Sleep);
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        assert_eq!(polling_phase(scheduler.state()), PollingPhase::Grace);
    }

    #[test]
    fn handle_interrupt_updates_app_poll_state() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
        let a = app.lock().unwrap();
        assert_eq!(a.poll_state, scheduler.state());
    }

    #[test]
    fn bootstrap_initial_poll_transitions_sleep_to_grace() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        assert_eq!(scheduler.state(), PollState::Sleep);

        bootstrap_initial_poll(&app, &mut scheduler);

        assert_eq!(polling_phase(scheduler.state()), PollingPhase::Grace);
        assert_eq!(app.lock().unwrap().poll_state, scheduler.state());
    }

    #[test]
    fn handle_interrupt_from_cooldown_re_enters_grace() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        // Drive to Cooldown via boost → Active → Cooldown
        scheduler.boost();
        scheduler.transition(true); // Grace → Active
        scheduler.transition(false); // Active → Cooldown
        assert_eq!(polling_phase(scheduler.state()), PollingPhase::Cooldown);

        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        assert_eq!(polling_phase(scheduler.state()), PollingPhase::Grace);
    }

    #[test]
    fn e2e_pid_file_overwritten_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        std::fs::write(&pid_path, "99999").unwrap();
        write_pid_file(&pid_path).unwrap();
        let content = std::fs::read_to_string(&pid_path).unwrap();
        assert_eq!(content, std::process::id().to_string());
    }

    #[test]
    fn e2e_pid_file_creates_nested_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("deep").join("nested").join("cibars.pid");
        assert!(!pid_path.parent().unwrap().exists());
        write_pid_file(&pid_path).unwrap();
        assert!(pid_path.exists());
        let content = std::fs::read_to_string(&pid_path).unwrap();
        assert_eq!(content, std::process::id().to_string());
    }
}
