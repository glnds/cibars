pub mod bar;
pub mod header;
pub mod statusbar;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use ratatui::DefaultTerminal;

use crate::app::App;
use crate::config::HookStatus;
use crate::model::{BuildStatus, PipelineGroup, WorkflowGroup};
use crate::poll_scheduler::PollState;

use bar::{ActionsTitle, BarWidget, PipelinesTitle};
use header::Header;
use statusbar::StatusBar;

/// Compute name width across all non-gone jobs in all groups (+ 2 for indent)
fn all_jobs_name_width(groups: &[WorkflowGroup]) -> usize {
    let max_job = groups
        .iter()
        .flat_map(|g| g.jobs.iter())
        .filter(|b| !b.gone)
        .map(|b| b.name.len())
        .max()
        .unwrap_or(10);
    // +2 for indent
    (max_job + 2).min(bar::MAX_NAME_WIDTH)
}

/// Compute name width across all non-gone stages in all pipeline groups
/// +4 for indent (2 for group + 2 for stage within group)
fn all_pipeline_stages_name_width(groups: &[PipelineGroup]) -> usize {
    let max_stage = groups
        .iter()
        .flat_map(|g| g.stages.iter())
        .filter(|s| !s.gone)
        .map(|s| s.name.len())
        .max()
        .unwrap_or(10);
    (max_stage + 4).min(bar::MAX_NAME_WIDTH)
}

/// Sort pipeline groups: those with running stages first, then alphabetical
fn sorted_pipeline_groups(groups: &[PipelineGroup]) -> Vec<&PipelineGroup> {
    let mut sorted: Vec<&PipelineGroup> = groups.iter().collect();
    sorted.sort_by(|a, b| {
        let a_running = a.stages.iter().any(|s| s.status == BuildStatus::Running);
        let b_running = b.stages.iter().any(|s| s.status == BuildStatus::Running);
        b_running.cmp(&a_running).then(a.name.cmp(&b.name))
    });
    sorted
}

/// Sort workflow groups: those with running jobs first, then alphabetical
fn sorted_workflow_groups(groups: &[WorkflowGroup]) -> Vec<&WorkflowGroup> {
    let mut sorted: Vec<&WorkflowGroup> = groups.iter().collect();
    sorted.sort_by(|a, b| {
        let a_running = a.jobs.iter().any(|j| j.status == BuildStatus::Running);
        let b_running = b.jobs.iter().any(|j| j.status == BuildStatus::Running);
        b_running.cmp(&a_running).then(a.name.cmp(&b.name))
    });
    sorted
}

const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 10;
const TICK_RATE_MS: u64 = 250;
const ANIMATION_INTERVAL: Duration = Duration::from_secs(1);

/// Handle the 'h' key press: install pre-push hook if needed.
/// Returns true if installation was attempted.
fn handle_hook_install(app: &Arc<Mutex<App>>) -> bool {
    let should_install = app
        .lock()
        .map(|a| matches!(a.hook_status, HookStatus::Missing | HookStatus::Incomplete))
        .unwrap_or(false);

    if !should_install {
        return false;
    }

    if let Ok(cwd) = std::env::current_dir() {
        let result = crate::config::install_pre_push_hook(&cwd);
        if let Ok(mut a) = app.lock() {
            match result {
                Ok(()) => {
                    a.hook_status = HookStatus::Installed;
                    tracing::info!("pre-push hook installed");
                }
                Err(e) => {
                    a.push_warning(format!("hook install failed: {e}"));
                    tracing::warn!("hook install failed: {e:#}");
                }
            }
        }
    }
    true
}

pub fn run_ui(
    app: Arc<Mutex<App>>,
    mut terminal: DefaultTerminal,
    profile: &str,
    region: &str,
    repo: &str,
    boost_notify: Arc<Notify>,
    term_flag: &AtomicBool,
) -> Result<()> {
    let mut last_animation = Instant::now();
    loop {
        // Check SIGTERM flag
        if term_flag.load(Ordering::Relaxed) {
            return Ok(());
        }

        terminal.draw(|frame| {
            let size = frame.area();

            // Update terminal width for poller to read
            if let Ok(mut a) = app.lock() {
                a.terminal_width = size.width;
            }

            if size.width < MIN_WIDTH || size.height < MIN_HEIGHT {
                let msg =
                    Paragraph::new("Terminal too small").style(Style::default().fg(Color::Red));
                frame.render_widget(msg, size);
                return;
            }

            let app = app.lock().expect("app mutex poisoned");
            let dim = app.poll_state != PollState::Active;

            let sorted_wf_groups: Vec<&WorkflowGroup> =
                sorted_workflow_groups(&app.workflow_groups);
            let sorted_pipe_groups: Vec<&PipelineGroup> =
                sorted_pipeline_groups(&app.pipeline_groups);

            let has_actions = !sorted_wf_groups.is_empty();
            let has_pipelines = !sorted_pipe_groups.is_empty();

            // Count action rows: just non-gone jobs (no per-workflow headers)
            let action_rows: usize = if app.actions_expanded {
                sorted_wf_groups
                    .iter()
                    .flat_map(|g| g.jobs.iter())
                    .filter(|j| !j.gone)
                    .count()
            } else {
                0
            };

            // Count pipeline rows: pipeline name headers + non-gone stages
            let pipe_rows: usize = if app.pipelines_expanded {
                sorted_pipe_groups
                    .iter()
                    .map(|g| {
                        let stage_count = g.stages.iter().filter(|s| !s.gone).count();
                        if stage_count > 0 {
                            1 + stage_count
                        } else {
                            0
                        }
                    })
                    .sum()
            } else {
                0
            };

            // Build dynamic layout constraints
            let mut constraints = vec![Constraint::Length(1)]; // header
            constraints.push(Constraint::Length(1)); // actions title
            for _ in 0..action_rows {
                constraints.push(Constraint::Length(1));
            }
            constraints.push(Constraint::Length(1)); // pipelines title
            for _ in 0..pipe_rows {
                constraints.push(Constraint::Length(1));
            }
            constraints.push(Constraint::Fill(1)); // remaining space
            constraints.push(Constraint::Length(1)); // status bar

            let areas = Layout::vertical(constraints).split(size);
            let mut idx = 0;

            // Header
            frame.render_widget(
                Header {
                    profile,
                    region,
                    repo,
                },
                areas[idx],
            );
            idx += 1;

            // Actions title (with inline dots)
            if !has_actions {
                let msg = if app.loading_actions {
                    "Loading GitHub Actions..."
                } else {
                    "No recent workflow runs found"
                };
                frame.render_widget(
                    Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
                    areas[idx],
                );
            } else {
                frame.render_widget(ActionsTitle::new(&sorted_wf_groups, dim), areas[idx]);
            }
            idx += 1;

            // Action job bars (when expanded)
            if app.actions_expanded {
                let job_name_width = all_jobs_name_width(&app.workflow_groups);
                for group in &sorted_wf_groups {
                    for bar in group.jobs.iter().filter(|j| !j.gone) {
                        let bar_dim = dim || group.gone;
                        frame.render_widget(
                            BarWidget::new(bar, job_name_width, bar_dim),
                            areas[idx],
                        );
                        idx += 1;
                    }
                }
            }

            // Pipelines title (with inline dots)
            if !has_pipelines {
                let msg = if app.loading_pipelines {
                    "Loading CodePipelines..."
                } else {
                    "No pipelines found in this account/region"
                };
                frame.render_widget(
                    Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
                    areas[idx],
                );
            } else {
                frame.render_widget(PipelinesTitle::new(&sorted_pipe_groups, dim), areas[idx]);
            }
            idx += 1;

            // Pipeline stage bars (when expanded)
            if app.pipelines_expanded {
                let stage_name_width = all_pipeline_stages_name_width(&app.pipeline_groups);
                for group in &sorted_pipe_groups {
                    let visible_stages: Vec<_> = group.stages.iter().filter(|s| !s.gone).collect();
                    if visible_stages.is_empty() {
                        continue;
                    }
                    // Pipeline name header
                    let name_color = if group.gone {
                        Color::DarkGray
                    } else {
                        group.summary_status.color()
                    };
                    let header_line = ratatui::text::Line::from(vec![
                        ratatui::text::Span::raw("  "),
                        ratatui::text::Span::styled(&group.name, Style::default().fg(name_color)),
                    ]);
                    frame.render_widget(header_line, areas[idx]);
                    idx += 1;
                    // Stage bars
                    for bar in &visible_stages {
                        let bar_dim = dim || group.gone;
                        frame.render_widget(
                            BarWidget::new(bar, stage_name_width, bar_dim),
                            areas[idx],
                        );
                        idx += 1;
                    }
                }
            }

            // Skip fill area
            idx += 1;

            // Status bar
            let elapsed = app
                .last_poll_started
                .map(|t| t.elapsed())
                .unwrap_or_default();
            frame.render_widget(
                StatusBar {
                    poll_state: &app.poll_state,
                    elapsed_since_poll: elapsed,
                    cooldown_remaining: app.cooldown_remaining,
                    warnings: &app.warnings,
                    hook_status: &app.hook_status,
                },
                areas[idx],
            );

            drop(app);
        })?;

        // Advance animation for Running bars every ~1s
        if last_animation.elapsed() >= ANIMATION_INTERVAL {
            last_animation = Instant::now();
            if let Ok(mut a) = app.lock() {
                let width = a.terminal_width as usize;

                let pipe_stage_name_width = all_pipeline_stages_name_width(&a.pipeline_groups);
                let pipe_fill_width = width.saturating_sub(pipe_stage_name_width + 4);
                for group in &mut a.pipeline_groups {
                    for stage in &mut group.stages {
                        stage.tick(pipe_fill_width);
                    }
                }

                let job_name_width = all_jobs_name_width(&a.workflow_groups);
                let job_fill_width = width.saturating_sub(job_name_width + 4);
                for group in &mut a.workflow_groups {
                    for job in &mut group.jobs {
                        job.tick(job_fill_width);
                    }
                }
            }
        }

        if event::poll(Duration::from_millis(TICK_RATE_MS))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Char('e') => {
                        if let Ok(mut a) = app.lock() {
                            a.actions_expanded = !a.actions_expanded;
                        }
                    }
                    KeyCode::Char('p') => {
                        if let Ok(mut a) = app.lock() {
                            a.pipelines_expanded = !a.pipelines_expanded;
                        }
                    }
                    KeyCode::Char('b') => {
                        boost_notify.notify_one();
                    }
                    KeyCode::Char('h') => {
                        handle_hook_install(&app);
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bar, BuildStatus};

    fn sorted_bars(bars: &[Bar]) -> Vec<&Bar> {
        let mut sorted: Vec<&Bar> = bars.iter().collect();
        sorted.sort_by(|a, b| {
            let a_running = a.status == BuildStatus::Running;
            let b_running = b.status == BuildStatus::Running;
            b_running.cmp(&a_running).then(a.name.cmp(&b.name))
        });
        sorted
    }

    fn make_test_bar(name: &str, status: BuildStatus) -> Bar {
        Bar {
            name: name.to_string(),
            status,
            fill: 0,
            write_pos: 0,
            gone: false,
        }
    }

    #[test]
    fn sorted_bars_running_first_then_alphabetical() {
        let bars = vec![
            make_test_bar("zebra", BuildStatus::Idle),
            make_test_bar("alpha", BuildStatus::Running),
            make_test_bar("beta", BuildStatus::Succeeded),
            make_test_bar("gamma", BuildStatus::Running),
        ];
        let sorted = sorted_bars(&bars);
        assert_eq!(sorted[0].name, "alpha");
        assert_eq!(sorted[1].name, "gamma");
        assert_eq!(sorted[2].name, "beta");
        assert_eq!(sorted[3].name, "zebra");
    }

    #[test]
    fn toggle_expand_actions() {
        let app = Arc::new(Mutex::new(App::new()));
        assert!(app.lock().unwrap().actions_expanded);
        {
            let mut a = app.lock().unwrap();
            a.actions_expanded = !a.actions_expanded;
        }
        assert!(!app.lock().unwrap().actions_expanded);
        {
            let mut a = app.lock().unwrap();
            a.actions_expanded = !a.actions_expanded;
        }
        assert!(app.lock().unwrap().actions_expanded);
    }

    #[test]
    fn handle_hook_install_skips_when_already_installed() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().hook_status = HookStatus::Installed;
        assert!(!handle_hook_install(&app));
    }

    #[test]
    fn handle_hook_install_skips_when_no_git_dir() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().hook_status = HookStatus::NoGitDir;
        assert!(!handle_hook_install(&app));
    }

    #[test]
    fn handle_hook_install_attempts_when_missing() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().hook_status = HookStatus::Missing;
        let attempted = handle_hook_install(&app);
        assert!(attempted);
    }

    #[test]
    fn handle_hook_install_attempts_when_incomplete() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().hook_status = HookStatus::Incomplete;
        let attempted = handle_hook_install(&app);
        assert!(attempted);
    }

    #[test]
    fn handle_hook_install_sets_installed_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git/hooks")).unwrap();

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().hook_status = HookStatus::Missing;

        let attempted = handle_hook_install(&app);
        assert!(attempted);
        assert_eq!(app.lock().unwrap().hook_status, HookStatus::Installed);

        std::env::set_current_dir(original_dir).unwrap();
    }

    #[test]
    fn toggle_expand_pipelines() {
        let app = Arc::new(Mutex::new(App::new()));
        assert!(app.lock().unwrap().pipelines_expanded);
        {
            let mut a = app.lock().unwrap();
            a.pipelines_expanded = !a.pipelines_expanded;
        }
        assert!(!app.lock().unwrap().pipelines_expanded);
        {
            let mut a = app.lock().unwrap();
            a.pipelines_expanded = !a.pipelines_expanded;
        }
        assert!(app.lock().unwrap().pipelines_expanded);
    }

    #[test]
    fn sorted_pipeline_groups_running_first() {
        let groups = vec![
            PipelineGroup {
                name: "zzz-idle".to_string(),
                stages: vec![],
                gone: false,
                summary_status: BuildStatus::Idle,
            },
            PipelineGroup {
                name: "aaa-running".to_string(),
                stages: vec![make_test_bar("Build", BuildStatus::Running)],
                gone: false,
                summary_status: BuildStatus::Running,
            },
        ];
        let sorted = sorted_pipeline_groups(&groups);
        assert_eq!(sorted[0].name, "aaa-running");
        assert_eq!(sorted[1].name, "zzz-idle");
    }

    #[test]
    fn gone_groups_included_in_display() {
        let groups = vec![
            WorkflowGroup {
                name: "CI".to_string(),
                jobs: vec![Bar::new("build".to_string()), Bar::new("test".to_string())],
                gone: false,
                summary_status: BuildStatus::Running,
                run_id: None,
            },
            WorkflowGroup {
                name: "Deploy".to_string(),
                jobs: vec![Bar::new("deploy-backend".to_string())],
                gone: true,
                summary_status: BuildStatus::Succeeded,
                run_id: None,
            },
        ];

        let visible: Vec<&WorkflowGroup> = sorted_workflow_groups(&groups);

        // Gone groups are now included (shown dimmed), not filtered out
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].name, "CI");
        assert!(visible.iter().any(|g| g.name == "Deploy" && g.gone));
    }

    #[test]
    fn all_jobs_name_width_empty() {
        assert_eq!(all_jobs_name_width(&[]), 12); // default 10 + 2 indent
    }

    #[test]
    fn all_jobs_name_width_computes_max_plus_indent() {
        let groups = vec![
            WorkflowGroup {
                name: "wf1".to_string(),
                jobs: vec![make_test_bar("build", BuildStatus::Idle)],
                gone: false,
                summary_status: BuildStatus::Idle,
                run_id: None,
            },
            WorkflowGroup {
                name: "wf2".to_string(),
                jobs: vec![make_test_bar("integration-test", BuildStatus::Idle)],
                gone: false,
                summary_status: BuildStatus::Idle,
                run_id: None,
            },
        ];
        assert_eq!(all_jobs_name_width(&groups), 18); // 16 + 2
    }

    #[test]
    fn all_pipeline_stages_name_width_empty() {
        assert_eq!(all_pipeline_stages_name_width(&[]), 14); // default 10 + 4 indent
    }

    #[test]
    fn all_pipeline_stages_name_width_computes_max_plus_indent() {
        let groups = vec![PipelineGroup {
            name: "my-pipeline".to_string(),
            stages: vec![
                make_test_bar("Source", BuildStatus::Succeeded),
                make_test_bar("Deploy", BuildStatus::Idle),
            ],
            gone: false,
            summary_status: BuildStatus::Idle,
        }];
        assert_eq!(all_pipeline_stages_name_width(&groups), 10); // 6 + 4
    }

    #[test]
    fn sorted_workflow_groups_running_first() {
        let groups = vec![
            WorkflowGroup {
                name: "zzz-idle".to_string(),
                jobs: vec![],
                gone: false,
                summary_status: BuildStatus::Idle,
                run_id: None,
            },
            WorkflowGroup {
                name: "aaa-running".to_string(),
                jobs: vec![make_test_bar("build", BuildStatus::Running)],
                gone: false,
                summary_status: BuildStatus::Running,
                run_id: None,
            },
        ];
        let sorted = sorted_workflow_groups(&groups);
        assert_eq!(sorted[0].name, "aaa-running");
        assert_eq!(sorted[1].name, "zzz-idle");
    }
}
