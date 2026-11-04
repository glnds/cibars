mod app;
mod config;
mod linkage;
mod model;
mod poll_scheduler;
mod poller;
mod sso_health;
mod topology;
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

fn setup_tracing(log_file: std::fs::File) -> Result<()> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_writer(log_file).with_ansi(false))
        .init();
    Ok(())
}

async fn init_aws_pipeline_client(aws: &config::AwsSource) -> poller::aws::AwsPipelineClient {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .profile_name(&aws.profile)
        .region(aws_config::Region::new(aws.region.clone()))
        .identity_cache(
            aws_config::identity::IdentityCache::lazy()
                .load_timeout(std::time::Duration::from_secs(15))
                .build(),
        )
        .load()
        .await;
    poller::aws::AwsPipelineClient::new(aws_sdk_codepipeline::Client::new(&aws_config))
}

/// Both branches own their work; dropping startup cancels pending requests.
async fn startup_sources<A, G, T>(aws: A, github: G) -> T
where
    A: std::future::Future<Output = T>,
    G: std::future::Future<Output = ()>,
{
    tokio::join!(aws, github).0
}

async fn retry_missing_source<A, G, T>(
    client: &mut Option<T>,
    requested: bool,
    initialize: A,
    github: G,
) -> bool
where
    A: std::future::Future<Output = Option<T>>,
    G: std::future::Future<Output = ()>,
{
    if !requested || client.is_some() {
        return false;
    }
    *client = startup_sources(initialize, github).await;
    true
}

const SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

async fn initialize_aws_source(
    app: &Arc<Mutex<App>>,
    source: Option<&config::AwsSource>,
) -> Option<poller::aws::AwsPipelineClient> {
    let aws = source?;
    match tokio::time::timeout(SETUP_TIMEOUT, init_aws_pipeline_client(aws)).await {
        Ok(client) => {
            if let Err(error) = tokio::time::timeout(
                SETUP_TIMEOUT,
                poller::poll_sources_tick(
                    app,
                    None,
                    Some((&client, aws.profile.as_str())),
                    PR_POLL_MIN_INTERVAL,
                ),
            )
            .await
            {
                let mut a = app.lock().expect("app mutex poisoned");
                a.loading_pipelines = false;
                a.push_warning(format!("AWS: initial poll: {error}"));
            }
            app.lock()
                .expect("app mutex poisoned")
                .clear_warnings_by_prefix("AWS initialization:");
            Some(client)
        }
        Err(error) => {
            let mut a = app.lock().expect("app mutex poisoned");
            a.loading_pipelines = false;
            a.push_warning(format!("AWS initialization: {error}"));
            None
        }
    }
}

fn persist_topology(app: &Arc<Mutex<App>>, config: &Config, cwd: &std::path::Path) {
    let cache = topology::TopologyCache::capture(&app.lock().expect("app mutex poisoned"), config);
    if let Err(error) = topology::save_topology(&cwd.join(".cibars-topology.toml"), &cache) {
        tracing::warn!(%error, "cannot save topology cache");
        app.lock()
            .expect("app mutex poisoned")
            .push_warning(format!("topology cache: {error}"));
    }
}

fn load_scoped_links(config: &Config, cwd: &std::path::Path) -> Result<Option<linkage::LinkCache>> {
    let Some(cache) = linkage::load_link_cache(&cwd.join(".cibars-links.toml"))? else {
        return Ok(None);
    };
    let valid = match &cache.scope {
        Some(scope) => Some(scope) == linkage::LinkScope::from_config(config).as_ref(),
        // Assumption: matching topology establishes the source of an older unscoped cache.
        None => topology::load_topology(&cwd.join(".cibars-topology.toml"))?
            .is_some_and(|topology| topology.matches_link_scope(config)),
    };
    Ok(valid.then_some(cache))
}

async fn run_poll_orchestrator(
    app: Arc<Mutex<App>>,
    config: Config,
    token: Option<String>,
    boost_notify: Arc<tokio::sync::Notify>,
    link_notify: Arc<tokio::sync::Notify>,
    mut sigusr1: tokio::signal::unix::Signal,
    cwd: std::path::PathBuf,
) -> Result<()> {
    let gh_client = match (&config.github, token) {
        (Some(gh), Some(token)) => {
            let (owner, repo) = gh
                .repo
                .split_once('/')
                .context("github-repo missing '/'; should have been validated in config")?;
            Some(poller::github::GitHubActionsClient::new(
                &token,
                owner.to_string(),
                repo.to_string(),
                gh.branch.clone(),
            )?)
        }
        (Some(_), None) => anyhow::bail!("github source enabled but no token resolved"),
        (None, _) => None,
    };
    let linkage_enabled = gh_client.is_some() && config.aws.is_some();

    let mut scheduler = PollScheduler::new();
    let mut link_map = LinkMap::new();
    let cache_path = cwd.join(".cibars-links.toml");
    let mut stopped_runs = std::collections::HashMap::new();

    bootstrap_initial_poll(&app, &mut scheduler);

    if let Ok(Some(cache)) = load_scoped_links(&config, &cwd) {
        link_map.load_from_cache(cache);
        linkage::sync_linked_pipelines(&app, &link_map);
    }
    let mut aws_client = loop {
        let aws_setup = async {
            let client = initialize_aws_source(&app, config.aws.as_ref()).await;
            persist_topology(&app, &config, &cwd);
            client
        };
        let github_setup = async {
            if let Some(gh) = gh_client.as_ref() {
                poller::poll_sources_tick(&app, Some(gh), None, PR_POLL_MIN_INTERVAL).await;
                persist_topology(&app, &config, &cwd);
            }
        };
        let outcome = tokio::select! {
            biased;
            _ = boost_notify.notified() => Err(InterruptSource::Boost),
            _ = sigusr1.recv() => Err(InterruptSource::Sigusr1),
            client = startup_sources(aws_setup, github_setup) => Ok(client),
        };
        persist_topology(&app, &config, &cwd);
        match outcome {
            Ok(client) => break client,
            Err(source) => handle_poll_interrupt(&app, &mut scheduler, source),
        }
    };
    // Assumption: setup is not observation time; start the initial Grace budget afterward.
    bootstrap_initial_poll(&app, &mut scheduler);
    let mut discover_requested = linkage_enabled && aws_client.is_some();
    let mut retry_aws_initialization = false;

    loop {
        if retry_aws_initialization {
            let github = async {
                if let Some(gh) = gh_client.as_ref() {
                    poller::poll_sources_tick(&app, Some(gh), None, PR_POLL_MIN_INTERVAL).await;
                }
            };
            let outcome = tokio::select! {
                biased;
                _ = boost_notify.notified() => Err(InterruptSource::Boost),
                _ = sigusr1.recv() => Err(InterruptSource::Sigusr1),
                _ = retry_missing_source(&mut aws_client, true, initialize_aws_source(&app, config.aws.as_ref()), github) => Ok(()),
            };
            persist_topology(&app, &config, &cwd);
            match outcome {
                Ok(()) => {
                    retry_aws_initialization = false;
                    discover_requested = linkage_enabled && aws_client.is_some();
                    bootstrap_initial_poll(&app, &mut scheduler);
                }
                Err(source) => {
                    handle_poll_interrupt(&app, &mut scheduler, source);
                    continue;
                }
            }
        }
        if scheduler.is_polling() {
            let aws = aws_client
                .as_ref()
                .zip(config.aws.as_ref())
                .map(|(c, cfg)| (c as &dyn poller::PipelineClient, cfg.profile.as_str()));
            let gh = gh_client.as_ref().map(|c| c as &dyn poller::ActionsClient);
            let cycle_start = Instant::now();

            let outcome = tokio::select! {
                biased;
                _ = boost_notify.notified() => CycleOutcome::Interrupted(InterruptSource::Boost),
                _ = sigusr1.recv() => CycleOutcome::Interrupted(InterruptSource::Sigusr1),
                merge_detected = async {
                    let poll = poller::poll_sources_tick(&app, gh, aws, PR_POLL_MIN_INTERVAL);
                    let discovery = async {
                        if discover_requested {
                            if let (Some(gh), Some(aws)) = (gh_client.as_ref(), aws_client.as_ref()) {
                                handle_link_notify(&app, &mut link_map, gh, Some(aws), &cache_path, &config).await;
                            }
                        }
                    };
                    tokio::join!(poll, discovery).0
                } => CycleOutcome::Completed { merge_detected },
            };

            // Streaming updates survive an interrupted cycle and are useful on the next launch.
            persist_topology(&app, &config, &cwd);

            let merge_detected = match outcome {
                CycleOutcome::Interrupted(source) => {
                    handle_poll_interrupt(&app, &mut scheduler, source);
                    retry_aws_initialization = source == InterruptSource::Boost
                        && aws_client.is_none()
                        && config.aws.is_some();
                    tracing::info!(state = ?scheduler.state(), ?source, "interrupt during poll");
                    continue;
                }
                CycleOutcome::Completed { merge_detected } => merge_detected,
            };
            discover_requested = false;

            // Process poll results
            {
                let mut a = app.lock().expect("app mutex poisoned");
                poller::classify_workflows(&mut a, &config);
            }
            if linkage_enabled {
                linkage::apply_links(&app, &mut link_map, &mut stopped_runs);
                linkage::sync_linked_pipelines(&app, &link_map);
            }

            let any_running = {
                let mut a = app.lock().expect("app mutex poisoned");
                a.check_linkage_health(&link_map);
                let now = Instant::now();
                let any_running = effective_any_running(&a, now);
                scheduler.transition(any_running);
                a.poll_state = scheduler.state();
                a.prune_expired_prs(poll_scheduler::PR_WATCH_CAP, now);
                any_running
            };

            if merge_detected {
                handle_pr_merge_detected(&app, &mut scheduler);
                tracing::info!(state = ?scheduler.state(), "PR merge detected — boosting");
            }

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
                    retry_aws_initialization = aws_client.is_none() && config.aws.is_some();
                }
                _ = sigusr1.recv() => {
                    handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
                }
                _ = link_notify.notified() => {
                    discover_requested = linkage_enabled && aws_client.is_some();
                    bootstrap_initial_poll(&app, &mut scheduler);
                }
                _ = tokio::time::sleep(remaining) => {}
            }
        } else {
            // Sleep — park on signals, never wake on a timer.
            tokio::select! {
                biased;
                _ = boost_notify.notified() => {
                    handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
                    retry_aws_initialization = aws_client.is_none() && config.aws.is_some();
                }
                _ = sigusr1.recv() => {
                    handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
                }
                _ = link_notify.notified() => {
                    discover_requested = linkage_enabled && aws_client.is_some();
                    bootstrap_initial_poll(&app, &mut scheduler);
                }
            }
        }
    }
}

async fn handle_link_notify(
    app: &Arc<Mutex<App>>,
    link_map: &mut LinkMap,
    gh_client: &dyn poller::ActionsClient,
    aws_client: Option<&dyn poller::PipelineClient>,
    cache_path: &std::path::Path,
    config: &Config,
) {
    tracing::info!("link re-discovery triggered by 'l' key");
    if let Some(aws) = aws_client {
        match tokio::time::timeout(
            SETUP_TIMEOUT,
            linkage::run_discovery(aws, gh_client, app, cache_path),
        )
        .await
        {
            Ok(Ok(fresh)) => {
                *link_map = fresh;
                app.lock()
                    .expect("app mutex poisoned")
                    .clear_warnings_by_prefix("link discovery:");
                let assignment = app
                    .lock()
                    .expect("app mutex poisoned")
                    .job_assignment
                    .clone();
                let mut cache = link_map.to_cache(&chrono::Utc::now().to_rfc3339(), assignment);
                cache.scope = linkage::LinkScope::from_config(config);
                if let Err(error) = linkage::save_link_cache(cache_path, &cache) {
                    app.lock()
                        .expect("app mutex poisoned")
                        .push_warning(format!("link cache: {error}"));
                }
            }
            result => {
                let error = match result {
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => error.to_string(),
                    _ => unreachable!(),
                };
                tracing::warn!(%error, "link discovery failed; retaining previous links");
                app.lock()
                    .expect("app mutex poisoned")
                    .push_warning(format!("link discovery: {error}"));
            }
        }
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

/// PR state polling cadence. 30 s per PR keeps API load modest while still
/// catching a manual merge within one Cooldown window.
const PR_POLL_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

enum CycleOutcome {
    Interrupted(InterruptSource),
    Completed { merge_detected: bool },
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

/// Liveness probe via `kill -0` (no libc dep). True if the process exists.
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Write our PID file unless another live cibars already owns it.
/// The pre-push hook signals the single PID in this file, so a second
/// instance would silently steal (and on exit delete) the first one's hook.
fn acquire_pid_file(pid_path: &std::path::Path) -> Result<()> {
    let own = std::process::id();
    if let Some(pid) = std::fs::read_to_string(pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        if pid != own && pid_alive(pid) {
            anyhow::bail!(
                "cibars already running for this repo (pid {pid}); PID file {}",
                pid_path.display()
            );
        }
    }
    write_pid_file(pid_path)
}

/// Claim the PID file, then open (truncate) the log. Order matters: a refused
/// second instance must not wipe the running instance's log.
fn acquire_pid_then_open_log(
    pid_path: &std::path::Path,
    log_dir: &std::path::Path,
) -> Result<std::fs::File> {
    acquire_pid_file(pid_path)?;
    std::fs::create_dir_all(log_dir)
        .with_context(|| format!("cannot create {}", log_dir.display()))?;
    let log_path = log_dir.join("cibars.log");
    std::fs::File::create(&log_path)
        .with_context(|| format!("cannot open log {}", log_path.display()))
}

/// Remove the PID file only if it still holds our PID.
fn cleanup_pid_file(pid_path: &std::path::Path) {
    let own = std::process::id().to_string();
    if std::fs::read_to_string(pid_path).is_ok_and(|s| s.trim() == own) {
        let _ = std::fs::remove_file(pid_path);
    }
}

fn handle_poll_interrupt(
    app: &Arc<Mutex<App>>,
    scheduler: &mut PollScheduler,
    source: InterruptSource,
) {
    scheduler.boost();
    let mut a = app.lock().expect("app mutex poisoned");
    a.poll_state = scheduler.state();
    // An explicit retry after SSO login must bypass the cached auth failure.
    // Unknown preserves honest health reporting until the next API result.
    if source == InterruptSource::Boost
        && matches!(a.aws_health, app::SourceHealth::AuthFailed { .. })
    {
        a.aws_health = app::SourceHealth::Unknown;
    }
    if source == InterruptSource::Sigusr1 {
        a.push_signal_at = Some(Instant::now());
    }
}

/// Boost the scheduler back to Grace when `poll_pr_states_tick` observes an
/// Open → Merged transition on a watched PR. Mirrors `handle_poll_interrupt`
/// for the local-signal case: the merge itself happens server-side in the
/// GitHub UI and has no hook point on this machine, so the PR poller's
/// transition detection is the only wake path.
fn handle_pr_merge_detected(app: &Arc<Mutex<App>>, scheduler: &mut PollScheduler) {
    scheduler.boost();
    let mut a = app.lock().expect("app mutex poisoned");
    a.poll_state = scheduler.state();
}

fn bootstrap_initial_poll(app: &Arc<Mutex<App>>, scheduler: &mut PollScheduler) {
    scheduler.boost();
    let mut a = app.lock().expect("app mutex poisoned");
    a.poll_state = scheduler.state();
}

fn restore_startup_caches(app: &mut App, config: &Config, cwd: &std::path::Path) {
    match topology::load_topology(&cwd.join(".cibars-topology.toml")) {
        Ok(Some(cache)) => cache.hydrate(app, config),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "cannot load topology cache; discovering live structure");
            app.push_warning(format!("topology cache: {error}"));
        }
    }
    if app.linkage_enabled() {
        match load_scoped_links(config, cwd) {
            Ok(Some(cache)) => app.job_assignment = cache.job_assignment,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "cannot load link assignments; discovering live links");
                app.push_warning(format!("link cache: {error}"));
            }
        }
    }
}

fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let cwd = std::env::current_dir().context("cannot read cwd")?;
    let pid_path = config::pid_file_for(&cwd)?;
    let log_dir = dirs::home_dir().context("no home dir")?.join(".cibars");
    let log_file = acquire_pid_then_open_log(&pid_path, &log_dir)?;
    let (config, token) = match setup_tracing(log_file).and_then(|_| Config::load(&cwd)) {
        Ok(v) => v,
        Err(e) => {
            cleanup_pid_file(&pid_path);
            return Err(e);
        }
    };
    tracing::info!(pid = std::process::id(), "starting cibars");
    let mut app_state = App::with_sources(config.github.is_some(), config.aws.is_some());
    restore_startup_caches(&mut app_state, &config, &cwd);
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

    // SSO health task: independent 5-min loop, AWS source only.
    if let Some(aws) = &config.aws {
        rt.spawn(sso_health::run_sso_health_loop(
            app.clone(),
            aws.profile.clone(),
            aws.region.clone(),
        ));
    }

    // Poll orchestrator
    let poll_app = app.clone();
    let poll_config = config.clone();
    let poll_boost = boost_notify.clone();
    let poll_link = link_notify.clone();
    let poll_cwd = cwd.clone();
    let poll_task = rt.spawn(async move {
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
        config.aws.as_ref().map(|a| a.profile.as_str()),
        config.aws.as_ref().map(|a| a.region.as_str()),
        config.github.as_ref().map(|g| g.repo.as_str()),
        boost_notify,
        link_notify,
        &term_flag,
    );
    ratatui::restore();

    // Stop owned requests before capturing their last useful partial structure.
    poll_task.abort();
    let _ = rt.block_on(poll_task);
    persist_topology(&app, &config, &cwd);

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

    struct StartupActions;
    #[async_trait::async_trait]
    impl poller::ActionsClient for StartupActions {
        async fn list_latest_runs(&self) -> Result<poller::RunsPage> {
            Ok(poller::RunsPage {
                summaries: vec![poller::WorkflowRunSummary {
                    workflow_name: "CI".into(),
                    run_id: 7,
                    status: model::BuildStatus::Succeeded,
                }],
                ..Default::default()
            })
        }
        async fn fetch_run_jobs(&self, _: u64) -> Result<Vec<poller::JobInfo>> {
            Ok(vec![poller::JobInfo {
                name: "Tests".into(),
                status: model::BuildStatus::Succeeded,
                completed_at: None,
            }])
        }
        async fn fetch_workflow_files(&self) -> Result<Vec<poller::WorkflowFile>> {
            Ok(vec![])
        }
        async fn fetch_pr_state(&self, _: u64) -> Result<model::WatchedPrState> {
            Ok(model::WatchedPrState::Unknown)
        }
    }

    #[tokio::test]
    async fn boost_retries_missing_aws_after_timeout_without_losing_github_rows() {
        use futures::FutureExt;
        let app = Arc::new(Mutex::new(App::new()));
        let mut client: Option<StartupPipelines> = None;
        // Automatic ticks leave a timed-out source alone until an explicit boost.
        assert!(
            !retry_missing_source(
                &mut client,
                false,
                async { panic!("automatic retry") },
                async {}
            )
            .await
        );
        let ready = tokio::sync::Notify::new();
        let mut retry = Box::pin(retry_missing_source(
            &mut client,
            true,
            async {
                ready.notified().await;
                Some(StartupPipelines)
            },
            async {
                poller::poll_sources_tick(&app, Some(&StartupActions), None, PR_POLL_MIN_INTERVAL)
                    .await;
            },
        ));
        assert!(retry.as_mut().now_or_never().is_none());
        assert_eq!(app.lock().unwrap().workflow_groups[0].jobs[0].name, "Tests");
        ready.notify_one();
        assert!(retry.await);
        assert!(client.is_some());
        assert_eq!(app.lock().unwrap().workflow_groups[0].run_id, Some(7));
    }

    struct StartupPipelines;
    #[async_trait::async_trait]
    impl poller::PipelineClient for StartupPipelines {
        async fn list_pipeline_names(&self) -> Result<Vec<String>> {
            Ok(vec!["Deploy".into()])
        }
        async fn get_pipeline_state(&self, _: &str) -> Result<poller::PipelineState> {
            Ok(poller::PipelineState {
                name: "Deploy".into(),
                status: model::BuildStatus::Running,
                stages: vec![],
            })
        }
        async fn get_pipeline_definition(&self, _: &str) -> Result<poller::PipelineDefinition> {
            std::future::pending().await
        }
    }

    struct RelinkPipelines(bool);
    #[async_trait::async_trait]
    impl poller::PipelineClient for RelinkPipelines {
        async fn list_pipeline_names(&self) -> Result<Vec<String>> {
            if self.0 {
                anyhow::bail!("unavailable");
            }
            Ok(vec![])
        }
        async fn get_pipeline_state(&self, _: &str) -> Result<poller::PipelineState> {
            unreachable!()
        }
        async fn get_pipeline_definition(&self, _: &str) -> Result<poller::PipelineDefinition> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn relink_failure_retains_previous_and_success_clears_warning_and_disk_links() {
        let config = Config {
            github: Some(config::GithubSource {
                repo: "o/r".into(),
                branch: None,
            }),
            aws: Some(config::AwsSource {
                profile: "p".into(),
                region: "r".into(),
            }),
            review_workflows: vec![],
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cibars-links.toml");
        let app = Arc::new(Mutex::new(App::new()));
        let mut links = LinkMap::new();
        links.add_discovered("Old".into(), "CI".into(), "b".into(), "k".into());
        linkage::save_link_cache(&path, &links.to_cache("old", None)).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        handle_link_notify(
            &app,
            &mut links,
            &StartupActions,
            Some(&RelinkPipelines(true)),
            &path,
            &config,
        )
        .await;
        assert_eq!(links.links().len(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert!(app
            .lock()
            .unwrap()
            .warnings
            .iter()
            .any(|w| w.starts_with("link discovery:")));
        handle_link_notify(
            &app,
            &mut links,
            &StartupActions,
            Some(&RelinkPipelines(false)),
            &path,
            &config,
        )
        .await;
        assert!(links.links().is_empty());
        assert!(app
            .lock()
            .unwrap()
            .warnings
            .iter()
            .all(|w| !w.starts_with("link discovery:")));
        let saved = linkage::load_link_cache(&path).unwrap().unwrap();
        assert!(saved.links.is_empty());
        assert_eq!(saved.scope, linkage::LinkScope::from_config(&config));
        let mut restored = App::new();
        restore_startup_caches(&mut restored, &config, dir.path());
        assert!(restored.job_assignment.is_none());
    }

    #[tokio::test]
    async fn startup_real_github_poll_publishes_jobs_while_aws_initialization_pending() {
        use futures::FutureExt;
        let app = Arc::new(Mutex::new(App::new()));
        let mut startup = Box::pin(startup_sources(std::future::pending::<()>(), async {
            poller::poll_sources_tick(&app, Some(&StartupActions), None, PR_POLL_MIN_INTERVAL)
                .await;
        }));
        assert!(startup.as_mut().now_or_never().is_none());
        {
            let a = app.lock().unwrap();
            assert_eq!(a.workflow_groups[0].run_id, Some(7));
            assert_eq!(a.workflow_groups[0].jobs[0].name, "Tests");
            assert!(!a.loading_actions);
        }
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            github: Some(config::GithubSource {
                repo: "o/r".into(),
                branch: None,
            }),
            aws: Some(config::AwsSource {
                profile: "p".into(),
                region: "r".into(),
            }),
            review_workflows: vec![],
        };
        // An interrupt preserves the published partial topology, without execution state.
        drop(startup);
        persist_topology(&app, &config, dir.path());
        let mut warm = App::new();
        restore_startup_caches(&mut warm, &config, dir.path());
        startup_sources(
            async {
                // Client initialization observes hydration before it starts any external work.
                assert_eq!(warm.workflow_groups[0].jobs[0].name, "Tests");
                assert_eq!(
                    warm.workflow_groups[0].jobs[0].status,
                    model::BuildStatus::Idle
                );
                assert_eq!(warm.workflow_groups[0].run_id, None);
            },
            async {},
        )
        .await;
    }

    #[tokio::test]
    async fn startup_real_aws_poll_publishes_while_github_pending() {
        use futures::FutureExt;
        let app = Arc::new(Mutex::new(App::new()));
        let mut startup = Box::pin(startup_sources(
            async {
                poller::poll_sources_tick(
                    &app,
                    None,
                    Some((&StartupPipelines, "p")),
                    PR_POLL_MIN_INTERVAL,
                )
                .await;
            },
            std::future::pending::<()>(),
        ));
        assert!(startup.as_mut().now_or_never().is_none());
        let a = app.lock().unwrap();
        assert_eq!(a.pipeline_groups[0].name, "Deploy");
        assert!(!a.loading_pipelines);
    }

    #[tokio::test]
    async fn startup_single_source_modes_complete_without_other_client() {
        for github in [true, false] {
            let app = Arc::new(Mutex::new(App::with_sources(github, !github)));
            startup_sources(
                async {
                    if !github {
                        poller::poll_sources_tick(
                            &app,
                            None,
                            Some((&StartupPipelines, "p")),
                            PR_POLL_MIN_INTERVAL,
                        )
                        .await;
                    }
                },
                async {
                    if github {
                        poller::poll_sources_tick(
                            &app,
                            Some(&StartupActions),
                            None,
                            PR_POLL_MIN_INTERVAL,
                        )
                        .await;
                    }
                },
            )
            .await;
            let a = app.lock().unwrap();
            assert_eq!(a.workflow_groups.len(), usize::from(github));
            assert_eq!(a.pipeline_groups.len(), usize::from(!github));
        }
    }

    #[tokio::test]
    async fn pending_linkage_cannot_block_live_jobs_and_cancellation_clears_flag() {
        use futures::FutureExt;
        let app = Arc::new(Mutex::new(App::new()));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("links.toml");
        let mut work = Box::pin(async {
            tokio::join!(
                poller::poll_sources_tick(
                    &app,
                    Some(&StartupActions),
                    Some((&StartupPipelines, "p")),
                    PR_POLL_MIN_INTERVAL
                ),
                linkage::run_discovery(&StartupPipelines, &StartupActions, &app, &path),
            )
        });
        assert!(work.as_mut().now_or_never().is_none());
        assert_eq!(app.lock().unwrap().workflow_groups[0].jobs[0].name, "Tests");
        assert!(app.lock().unwrap().linkage_discovering);
        drop(work);
        assert!(!app.lock().unwrap().linkage_discovering);
    }

    #[tokio::test]
    async fn startup_sources_publish_independently_and_drop_pending_work() {
        use futures::FutureExt;
        let aws_ready = tokio::sync::Notify::new();
        let gh_ready = tokio::sync::Notify::new();
        let aws_published = AtomicBool::new(false);
        let gh_published = AtomicBool::new(false);
        let aws_dropped = AtomicBool::new(false);
        struct Dropped<'a>(&'a AtomicBool);
        impl Drop for Dropped<'_> {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let work = startup_sources(
            async {
                let _drop = Dropped(&aws_dropped);
                aws_ready.notified().await;
                aws_published.store(true, std::sync::atomic::Ordering::SeqCst);
                42
            },
            async {
                gh_published.store(true, std::sync::atomic::Ordering::SeqCst);
                gh_ready.notified().await;
            },
        );
        tokio::pin!(work);
        assert!(work.as_mut().now_or_never().is_none());
        assert!(gh_published.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!aws_published.load(std::sync::atomic::Ordering::SeqCst));
        aws_ready.notify_one();
        assert!(work.as_mut().now_or_never().is_none());
        assert!(aws_published.load(std::sync::atomic::Ordering::SeqCst));
        assert!(aws_dropped.load(std::sync::atomic::Ordering::SeqCst));
        gh_ready.notify_one();
        assert_eq!(work.await, 42);

        let work = startup_sources(
            async {
                let _drop = Dropped(&aws_dropped);
                std::future::pending::<()>().await
            },
            async {},
        );
        aws_dropped.store(false, std::sync::atomic::Ordering::SeqCst);
        let mut work = Box::pin(work);
        assert!(work.as_mut().now_or_never().is_none());
        drop(work);
        assert!(aws_dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn startup_unscoped_legacy_assignment_requires_matching_topology() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".cibars-links.toml"),
            r#"
schema_version = 2
discovered_at = "2026-11-04"
links = []
[job_assignment]
pipeline_jobs = {}
shared_jobs = [["CI", ["Tests"]]]
"#,
        )
        .unwrap();
        let config = Config {
            github: Some(config::GithubSource {
                repo: "o/r".into(),
                branch: None,
            }),
            aws: Some(config::AwsSource {
                profile: "p".into(),
                region: "r".into(),
            }),
            review_workflows: vec![],
        };
        let mut app = App::new();
        restore_startup_caches(&mut app, &config, dir.path());
        assert!(app.job_assignment.is_none());

        topology::save_topology(
            &dir.path().join(".cibars-topology.toml"),
            &topology::TopologyCache::capture(&app, &config),
        )
        .unwrap();
        restore_startup_caches(&mut app, &config, dir.path());
        assert_eq!(app.job_assignment.unwrap().shared_jobs[0].1, ["Tests"]);

        let mut changed = config.clone();
        changed.github.as_mut().unwrap().repo = "other/repo".into();
        let mut changed_app = App::new();
        restore_startup_caches(&mut changed_app, &changed, dir.path());
        assert!(changed_app.job_assignment.is_none());
    }

    #[test]
    fn startup_scoped_cache_requires_both_sources_to_match() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            github: Some(config::GithubSource {
                repo: "o/r".into(),
                branch: Some("main".into()),
            }),
            aws: Some(config::AwsSource {
                profile: "p".into(),
                region: "r".into(),
            }),
            review_workflows: vec![],
        };
        let assignment = linkage::JobAssignment {
            pipeline_jobs: Default::default(),
            shared_jobs: vec![("CI".into(), vec!["Tests".into()])],
        };
        let mut links = LinkMap::new().to_cache("now", Some(assignment));
        links.scope = linkage::LinkScope::from_config(&config);
        linkage::save_link_cache(&dir.path().join(".cibars-links.toml"), &links).unwrap();
        for variant in 0..7 {
            let mut current = config.clone();
            match variant {
                1 => current.github.as_mut().unwrap().repo = "other/repo".into(),
                2 => current.github.as_mut().unwrap().branch = None,
                3 => current.aws.as_mut().unwrap().profile = "other".into(),
                4 => current.aws.as_mut().unwrap().region = "other".into(),
                5 => current.github = None,
                6 => current.aws = None,
                _ => {}
            }
            let mut app = App::with_sources(current.github.is_some(), current.aws.is_some());
            restore_startup_caches(&mut app, &current, dir.path());
            assert_eq!(
                app.job_assignment.is_some(),
                variant == 0,
                "variant {variant}"
            );
        }
    }

    #[test]
    fn startup_corrupt_topology_warns_without_restoring_live_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".cibars-topology.toml"), "invalid [").unwrap();
        let config = Config {
            github: None,
            aws: None,
            review_workflows: vec![],
        };
        let mut app = App::with_sources(false, false);
        restore_startup_caches(&mut app, &config, dir.path());
        assert!(app
            .warnings
            .iter()
            .any(|warning| warning.contains("topology")));
        assert!(app.workflow_groups.is_empty() && app.pipeline_groups.is_empty());
        assert!(app.job_assignment.is_none());
    }

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

    // --- T9: handle_pr_merge_detected tests ---

    #[test]
    fn handle_pr_merge_detected_from_cooldown_enters_grace() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        scheduler.boost();
        scheduler.transition(true); // Grace -> Active
        scheduler.transition(false); // Active -> Cooldown
        assert_eq!(polling_phase(scheduler.state()), PollingPhase::Cooldown);

        handle_pr_merge_detected(&app, &mut scheduler);

        assert_eq!(polling_phase(scheduler.state()), PollingPhase::Grace);
        assert_eq!(app.lock().unwrap().poll_state, scheduler.state());
    }

    #[test]
    fn handle_pr_merge_detected_from_sleep_enters_grace() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        assert_eq!(scheduler.state(), PollState::Sleep);
        handle_pr_merge_detected(&app, &mut scheduler);
        assert_eq!(polling_phase(scheduler.state()), PollingPhase::Grace);
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

    #[test]
    fn acquire_pid_file_refuses_second_live_instance() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        std::fs::write(&pid_path, other.id().to_string()).unwrap();
        let err = acquire_pid_file(&pid_path).unwrap_err().to_string();
        other.kill().unwrap();
        other.wait().unwrap();
        assert!(err.contains("already running"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(&pid_path).unwrap(),
            other.id().to_string(),
            "must not clobber the live instance's PID file"
        );
    }

    #[test]
    fn refused_second_instance_keeps_running_instances_log() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        let log_dir = dir.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(log_dir.join("cibars.log"), "first instance log\n").unwrap();
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        std::fs::write(&pid_path, other.id().to_string()).unwrap();
        let result = acquire_pid_then_open_log(&pid_path, &log_dir);
        other.kill().unwrap();
        other.wait().unwrap();
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(log_dir.join("cibars.log")).unwrap(),
            "first instance log\n",
            "refused launch must not truncate the running instance's log"
        );
    }

    #[test]
    fn acquire_pid_then_open_log_truncates_when_free() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        let log_dir = dir.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(log_dir.join("cibars.log"), "old run\n").unwrap();
        acquire_pid_then_open_log(&pid_path, &log_dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(log_dir.join("cibars.log")).unwrap(),
            ""
        );
        assert_eq!(
            std::fs::read_to_string(&pid_path).unwrap(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn acquire_pid_file_replaces_stale_pid() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        let mut dead = std::process::Command::new("true").spawn().unwrap();
        dead.wait().unwrap();
        std::fs::write(&pid_path, dead.id().to_string()).unwrap();
        acquire_pid_file(&pid_path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&pid_path).unwrap(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn acquire_pid_file_replaces_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        std::fs::write(&pid_path, "not-a-pid").unwrap();
        acquire_pid_file(&pid_path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&pid_path).unwrap(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn cleanup_pid_file_leaves_foreign_pid() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        std::fs::write(&pid_path, "1").unwrap();
        cleanup_pid_file(&pid_path);
        assert!(
            pid_path.exists(),
            "must not delete another instance's PID file"
        );
    }

    #[test]
    fn cleanup_pid_file_removes_own_pid() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");
        acquire_pid_file(&pid_path).unwrap();
        cleanup_pid_file(&pid_path);
        assert!(!pid_path.exists());
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
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();
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

    #[tokio::test]
    async fn boost_retries_aws_after_auth_failure() {
        struct RecoveredPipelineClient;

        #[async_trait::async_trait]
        impl poller::PipelineClient for RecoveredPipelineClient {
            async fn list_pipeline_names(&self) -> Result<Vec<String>> {
                Ok(Vec::new())
            }

            async fn get_pipeline_state(&self, _: &str) -> Result<poller::PipelineState> {
                unreachable!("empty pipeline listing")
            }

            async fn get_pipeline_definition(&self, _: &str) -> Result<poller::PipelineDefinition> {
                unreachable!("polling does not fetch definitions")
            }
        }

        let app = Arc::new(Mutex::new(App::with_sources(false, true)));
        app.lock().unwrap().aws_health = app::SourceHealth::AuthFailed {
            since: chrono::Utc::now(),
        };
        let mut scheduler = PollScheduler::new();
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        poller::poll_sources_tick(
            &app,
            None,
            Some((&RecoveredPipelineClient, "test-profile")),
            PR_POLL_MIN_INTERVAL,
        )
        .await;

        let a = app.lock().unwrap();
        assert_eq!(a.aws_health, app::SourceHealth::Healthy);
        assert!(!a.loading_pipelines);
    }

    #[test]
    fn push_signal_preserves_auth_failure_backoff() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().aws_health = app::SourceHealth::AuthFailed {
            since: chrono::Utc::now(),
        };
        let mut scheduler = PollScheduler::new();
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
        assert!(matches!(
            app.lock().unwrap().aws_health,
            app::SourceHealth::AuthFailed { .. }
        ));
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
