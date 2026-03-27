pub mod bar;
pub mod header;
pub mod statusbar;
pub mod theme;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget};
use ratatui::DefaultTerminal;

use crate::app::App;
use crate::config::HookStatus;
use crate::model::{BuildStatus, PipelineGroup, WorkflowCategory, WorkflowGroup};
use crate::poll_scheduler::PollState;

use bar::BarWidget;
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

/// Sort pipeline groups: those with running stages first, then alphabetical.
/// Excludes pipelines that are linked to a workflow (they render in Actions block).
fn sorted_pipeline_groups<'a>(
    groups: &'a [PipelineGroup],
    workflow_groups: &[WorkflowGroup],
) -> Vec<&'a PipelineGroup> {
    let linked: std::collections::HashSet<&str> = workflow_groups
        .iter()
        .filter_map(|wg| wg.linked_pipeline.as_deref())
        .collect();
    let mut sorted: Vec<&PipelineGroup> = groups
        .iter()
        .filter(|g| !linked.contains(g.name.as_str()))
        .collect();
    sorted.sort_by(|a, b| {
        let a_running = a.stages.iter().any(|s| s.status == BuildStatus::Running);
        let b_running = b.stages.iter().any(|s| s.status == BuildStatus::Running);
        b_running.cmp(&a_running).then(a.name.cmp(&b.name))
    });
    sorted
}

/// Sort workflow groups: CI before Review, running first within each category, then alphabetical
fn sorted_workflow_groups(groups: &[WorkflowGroup]) -> Vec<&WorkflowGroup> {
    let mut sorted: Vec<&WorkflowGroup> = groups.iter().collect();
    sorted.sort_by(|a, b| {
        let cat_ord = (a.category as u8).cmp(&(b.category as u8));
        let a_running = a.jobs.iter().any(|j| j.status == BuildStatus::Running);
        let b_running = b.jobs.iter().any(|j| j.status == BuildStatus::Running);
        cat_ord
            .then(b_running.cmp(&a_running))
            .then(a.name.cmp(&b.name))
    });
    sorted
}

const TICK_RATE_MS: u64 = 250;
const ANIMATION_INTERVAL: Duration = Duration::from_secs(1);

/// Toggle expand/collapse for both GitHub Actions and CodePipelines.
fn toggle_expand(app: &Arc<Mutex<App>>) {
    if let Ok(mut a) = app.lock() {
        let expanded = !a.actions_expanded;
        a.actions_expanded = expanded;
        a.pipelines_expanded = expanded;
    }
}

/// Handle boost key: optimistically update poll state to Watching.
/// Returns true if state was changed (Idle/LongIdle → Watching).
fn handle_boost(app: &Arc<Mutex<App>>) -> bool {
    if let Ok(mut a) = app.lock() {
        a.boost_pressed_at = Some(Instant::now());
        if matches!(a.poll_state, PollState::Idle | PollState::LongIdle) {
            a.poll_state = PollState::Watching;
            return true;
        }
    }
    false
}

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

/// Compute section height for actions/pipelines blocks.
/// Expanded: 2 (borders) + content rows. Collapsed/empty: 3 (borders + 1 content line).
fn section_height(has_content: bool, expanded: bool, content_rows: usize) -> u16 {
    if !has_content || !expanded {
        3
    } else {
        (2 + content_rows) as u16
    }
}

/// Compute layout constraints for the main UI.
fn compute_layout_constraints(actions_height: u16, pipelines_height: u16) -> Vec<Constraint> {
    vec![
        Constraint::Length(3),                // header block
        Constraint::Length(actions_height),   // actions block
        Constraint::Length(pipelines_height), // pipelines block
        Constraint::Fill(1),                  // remaining space
        Constraint::Length(3),                // status bar block
    ]
}

/// Render collapsed actions: status dots + count inside the block inner area.
fn render_collapsed_actions(
    groups: &[&WorkflowGroup],
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
) {
    let mut spans = Vec::new();
    let mut count = 0usize;
    for group in groups {
        let visible_jobs: Vec<_> = group.jobs.iter().filter(|j| !j.gone).collect();
        if visible_jobs.is_empty() {
            let color = if group.gone || group.category == WorkflowCategory::Review {
                theme::FG_DIM
            } else {
                group.summary_status.color()
            };
            spans.push(Span::styled("\u{25CF}", Style::default().fg(color)));
            count += 1;
        } else {
            for job in &visible_jobs {
                let color = if group.gone || group.category == WorkflowCategory::Review {
                    theme::FG_DIM
                } else {
                    job.status.color()
                };
                spans.push(Span::styled("\u{25CF}", Style::default().fg(color)));
                count += 1;
            }
        }
    }
    spans.push(Span::styled(
        format!(" ({count} jobs)"),
        Style::default().fg(theme::FG_DIM),
    ));
    Line::from(spans).render(area, buf);
}

/// Render collapsed pipelines: status dots + count inside the block inner area.
fn render_collapsed_pipelines(
    groups: &[&PipelineGroup],
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
) {
    let mut spans = Vec::new();
    let mut count = 0usize;
    for group in groups {
        let color = if group.gone || group.pending_link {
            theme::FG_DIM
        } else {
            group.summary_status.color()
        };
        spans.push(Span::styled("\u{25CF}", Style::default().fg(color)));
        count += 1;
    }
    let label = if count == 1 { "pipeline" } else { "pipelines" };
    spans.push(Span::styled(
        format!(" ({count} {label})"),
        Style::default().fg(theme::FG_DIM),
    ));
    Line::from(spans).render(area, buf);
}

pub fn run_ui(
    app: Arc<Mutex<App>>,
    mut terminal: DefaultTerminal,
    profile: &str,
    region: &str,
    repo: &str,
    boost_notify: Arc<Notify>,
    link_notify: Arc<Notify>,
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

            let app = app.lock().expect("app mutex poisoned");
            let dim = app.poll_state != PollState::Active;

            let sorted_wf_groups: Vec<&WorkflowGroup> =
                sorted_workflow_groups(&app.workflow_groups);
            let sorted_pipe_groups: Vec<&PipelineGroup> =
                sorted_pipeline_groups(&app.pipeline_groups, &app.workflow_groups);

            let has_actions = !sorted_wf_groups.is_empty();
            let has_pipelines = !sorted_pipe_groups.is_empty();

            // Count action rows: CI jobs + review separator + review jobs
            let action_rows: usize = if app.actions_expanded {
                let ci_jobs: usize = sorted_wf_groups
                    .iter()
                    .filter(|g| g.category == WorkflowCategory::CI)
                    .map(|g| {
                        let job_count = g.jobs.iter().filter(|j| !j.gone).count();
                        let pipeline_rows = if let Some(ref pipe_name) = g.linked_pipeline {
                            if let Some(pg) =
                                app.pipeline_groups.iter().find(|p| p.name == *pipe_name)
                            {
                                let visible_stages = pg.stages.iter().filter(|s| !s.gone).count();
                                if visible_stages > 0 {
                                    1 + visible_stages // header + stages
                                } else {
                                    1 // just header
                                }
                            } else {
                                0
                            }
                        } else {
                            0
                        };
                        job_count + pipeline_rows
                    })
                    .sum();
                let review_jobs: usize = sorted_wf_groups
                    .iter()
                    .filter(|g| g.category == WorkflowCategory::Review)
                    .flat_map(|g| g.jobs.iter())
                    .filter(|j| !j.gone)
                    .count();
                let separator = if review_jobs > 0 { 1 } else { 0 };
                ci_jobs + separator + review_jobs
            } else {
                0
            };

            // Count pipeline rows: headers + stages + between-group spacers
            let pipe_rows: usize = if app.pipelines_expanded {
                let groups_with_stages: usize = sorted_pipe_groups
                    .iter()
                    .filter(|g| g.stages.iter().any(|s| !s.gone))
                    .count();
                let base: usize = sorted_pipe_groups
                    .iter()
                    .map(|g| {
                        let stage_count = g.stages.iter().filter(|s| !s.gone).count();
                        if stage_count > 0 {
                            1 + stage_count
                        } else {
                            0
                        }
                    })
                    .sum();
                let between_group_spacers = groups_with_stages.saturating_sub(1);
                base + between_group_spacers
            } else {
                0
            };

            // Compute block heights
            let actions_height = section_height(has_actions, app.actions_expanded, action_rows);
            let pipelines_height = section_height(has_pipelines, app.pipelines_expanded, pipe_rows);

            let constraints = compute_layout_constraints(actions_height, pipelines_height);
            let areas = Layout::vertical(constraints).split(size);

            // --- Header block (area 0) ---
            frame.render_widget(
                Header {
                    profile,
                    region,
                    repo,
                    aws_health: &app.aws_health,
                },
                areas[0],
            );

            // --- Actions block (area 1) ---
            let actions_block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::BORDER_ACTIONS))
                .title(Span::styled(
                    " GitHub Actions ",
                    Style::default().fg(theme::BORDER_ACTIONS),
                ));
            let actions_inner = actions_block.inner(areas[1]);
            actions_block.render(areas[1], frame.buffer_mut());

            if !has_actions {
                let msg = if app.loading_actions {
                    "Loading GitHub Actions..."
                } else {
                    "No recent workflow runs found"
                };
                frame.render_widget(
                    Paragraph::new(msg).style(Style::default().fg(theme::FG_DIM)),
                    actions_inner,
                );
            } else if app.actions_expanded {
                // Build inner row constraints
                let mut inner_constraints = Vec::new();
                for _ in 0..action_rows {
                    inner_constraints.push(Constraint::Length(1));
                }
                let inner_areas = Layout::vertical(inner_constraints).split(actions_inner);
                let mut row = 0;

                let job_name_width = all_jobs_name_width(&app.workflow_groups);

                // CI workflows first
                for group in sorted_wf_groups
                    .iter()
                    .filter(|g| g.category == WorkflowCategory::CI)
                {
                    for bar in group.jobs.iter().filter(|j| !j.gone) {
                        if row >= inner_areas.len() {
                            break;
                        }
                        let bar_dim = dim || group.gone;
                        let dot_color = if group.gone {
                            theme::FG_DIM
                        } else {
                            bar.status.color()
                        };
                        frame.render_widget(
                            BarWidget::new(bar, job_name_width, bar_dim).with_dot(dot_color),
                            inner_areas[row],
                        );
                        row += 1;
                    }

                    // Render linked pipeline indented under this workflow
                    if let Some(ref pipe_name) = group.linked_pipeline {
                        if let Some(pg) = app.pipeline_groups.iter().find(|p| p.name == *pipe_name)
                        {
                            if row < inner_areas.len() {
                                // Pipeline header: "  └─ ● pipeline-name"
                                let dot_color = if pg.pending_link || pg.gone {
                                    theme::FG_DIM
                                } else {
                                    pg.summary_status.color()
                                };
                                let header_line = Line::from(vec![
                                    Span::styled(
                                        "  \u{2514}\u{2500} ",
                                        Style::default().fg(theme::FG_DIM),
                                    ),
                                    Span::styled("\u{25CF} ", Style::default().fg(dot_color)),
                                    Span::styled(&pg.name, Style::default().fg(dot_color)),
                                ]);
                                frame.render_widget(header_line, inner_areas[row]);
                                row += 1;
                            }

                            let stage_name_width =
                                all_pipeline_stages_name_width(&app.pipeline_groups);
                            for stage in pg.stages.iter().filter(|s| !s.gone) {
                                if row >= inner_areas.len() {
                                    break;
                                }
                                let bar_dim = dim || pg.gone || pg.pending_link;
                                let stage_area = Rect::new(
                                    inner_areas[row].x + 4,
                                    inner_areas[row].y,
                                    inner_areas[row].width.saturating_sub(4),
                                    1,
                                );
                                frame.render_widget(
                                    BarWidget::new(stage, stage_name_width, bar_dim),
                                    stage_area,
                                );
                                row += 1;
                            }
                        }
                    }
                }

                // Separator + Review workflows
                let has_review_jobs = sorted_wf_groups
                    .iter()
                    .filter(|g| g.category == WorkflowCategory::Review)
                    .any(|g| g.jobs.iter().any(|j| !j.gone));

                if has_review_jobs && row < inner_areas.len() {
                    let sep_width = inner_areas[row].width as usize;
                    let label = " reviews ";
                    let pad_len = sep_width.saturating_sub(label.len() + 2);
                    let pad = "\u{2500}".repeat(pad_len);
                    let sep_text = format!("\u{2500}\u{2500}{label}{pad}");
                    let sep_line = Line::from(Span::styled(
                        sep_text,
                        Style::default().fg(theme::SEPARATOR),
                    ));
                    frame.render_widget(sep_line, inner_areas[row]);
                    row += 1;

                    for group in sorted_wf_groups
                        .iter()
                        .filter(|g| g.category == WorkflowCategory::Review)
                    {
                        for bar in group.jobs.iter().filter(|j| !j.gone) {
                            if row >= inner_areas.len() {
                                break;
                            }
                            frame.render_widget(
                                BarWidget::new(bar, job_name_width, true).with_dot(theme::FG_DIM),
                                inner_areas[row],
                            );
                            row += 1;
                        }
                    }
                }
            } else {
                // Collapsed: dots + count
                render_collapsed_actions(&sorted_wf_groups, actions_inner, frame.buffer_mut());
            }

            // --- Pipelines block (area 2) ---
            let pipelines_block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::BORDER_PIPELINES))
                .title(Span::styled(
                    " CodePipelines ",
                    Style::default().fg(theme::BORDER_PIPELINES),
                ));
            let pipelines_inner = pipelines_block.inner(areas[2]);
            pipelines_block.render(areas[2], frame.buffer_mut());

            if !has_pipelines {
                let msg = if app.loading_pipelines {
                    "Loading CodePipelines..."
                } else {
                    "No pipelines found in this account/region"
                };
                frame.render_widget(
                    Paragraph::new(msg).style(Style::default().fg(theme::FG_DIM)),
                    pipelines_inner,
                );
            } else if app.pipelines_expanded {
                let mut inner_constraints = Vec::new();
                for _ in 0..pipe_rows {
                    inner_constraints.push(Constraint::Length(1));
                }
                let inner_areas = Layout::vertical(inner_constraints).split(pipelines_inner);
                let mut row = 0;

                let stage_name_width = all_pipeline_stages_name_width(&app.pipeline_groups);
                let pipe_groups_visible: Vec<_> = sorted_pipe_groups
                    .iter()
                    .filter(|g| g.stages.iter().any(|s| !s.gone))
                    .collect();
                for (gi, group) in pipe_groups_visible.iter().enumerate() {
                    if gi > 0 && row < inner_areas.len() {
                        let sep = "\u{2500}".repeat(inner_areas[row].width as usize);
                        frame.render_widget(
                            Line::from(Span::styled(sep, Style::default().fg(theme::SEPARATOR))),
                            inner_areas[row],
                        );
                        row += 1;
                    }
                    if row >= inner_areas.len() {
                        break;
                    }
                    // Pipeline name header with status dot
                    let dot_color = if group.gone || group.pending_link {
                        theme::FG_DIM
                    } else {
                        group.summary_status.color()
                    };
                    let header_line = Line::from(vec![
                        Span::raw("  "),
                        Span::styled("\u{25CF} ", Style::default().fg(dot_color)),
                        Span::styled(&group.name, Style::default().fg(dot_color)),
                    ]);
                    frame.render_widget(header_line, inner_areas[row]);
                    row += 1;
                    // Stage bars
                    for bar in group.stages.iter().filter(|s| !s.gone) {
                        if row >= inner_areas.len() {
                            break;
                        }
                        let bar_dim = dim || group.gone || group.pending_link;
                        frame.render_widget(
                            BarWidget::new(bar, stage_name_width, bar_dim),
                            inner_areas[row],
                        );
                        row += 1;
                    }
                }
            } else {
                // Collapsed: dots + count
                render_collapsed_pipelines(
                    &sorted_pipe_groups,
                    pipelines_inner,
                    frame.buffer_mut(),
                );
            }

            // --- Status bar block (area 4, after fill at area 3) ---
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
                    boost_pressed_at: app.boost_pressed_at,
                    linkage_broken: app.linkage_broken,
                    linkage_discovering: app.linkage_discovering,
                },
                areas[4],
            );

            drop(app);
        })?;

        // Advance animation for Running bars every ~1s
        if last_animation.elapsed() >= ANIMATION_INTERVAL {
            last_animation = Instant::now();
            if let Ok(mut a) = app.lock() {
                let width = a.terminal_width as usize;

                let pipe_stage_name_width = all_pipeline_stages_name_width(&a.pipeline_groups);
                // +2 for block borders (left + right │)
                let pipe_fill_width = width.saturating_sub(pipe_stage_name_width + 4 + 7 + 2);
                for group in &mut a.pipeline_groups {
                    for stage in &mut group.stages {
                        stage.tick(pipe_fill_width);
                    }
                }

                let job_name_width = all_jobs_name_width(&a.workflow_groups);
                // +2 for block borders (left + right │)
                let job_fill_width = width.saturating_sub(job_name_width + 4 + 7 + 2);
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
                        toggle_expand(&app);
                    }
                    KeyCode::Char('b') => {
                        tracing::info!("UI: boost key pressed");
                        boost_notify.notify_one();
                        handle_boost(&app);
                    }
                    KeyCode::Char('h') => {
                        handle_hook_install(&app);
                    }
                    KeyCode::Char('l') => {
                        let discovering = app.lock().map(|a| a.linkage_discovering).unwrap_or(true);
                        if !discovering {
                            link_notify.notify_one();
                        }
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
            last_finished: None,
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
    fn toggle_expand_toggles_both_flags() {
        let app = Arc::new(Mutex::new(App::new()));
        // Both start expanded
        assert!(app.lock().unwrap().actions_expanded);
        assert!(app.lock().unwrap().pipelines_expanded);

        // Simulate 'e' toggle
        toggle_expand(&app);
        assert!(!app.lock().unwrap().actions_expanded);
        assert!(!app.lock().unwrap().pipelines_expanded);

        // Toggle back
        toggle_expand(&app);
        assert!(app.lock().unwrap().actions_expanded);
        assert!(app.lock().unwrap().pipelines_expanded);
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
    fn sorted_pipeline_groups_running_first() {
        let groups = vec![
            PipelineGroup {
                name: "zzz-idle".to_string(),
                stages: vec![],
                gone: false,
                summary_status: BuildStatus::Idle,
                pending_link: false,
            },
            PipelineGroup {
                name: "aaa-running".to_string(),
                stages: vec![make_test_bar("Build", BuildStatus::Running)],
                gone: false,
                summary_status: BuildStatus::Running,
                pending_link: false,
            },
        ];
        let sorted = sorted_pipeline_groups(&groups, &[]);
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
                category: WorkflowCategory::default(),
                linked_pipeline: None,
            },
            WorkflowGroup {
                name: "Deploy".to_string(),
                jobs: vec![Bar::new("deploy-backend".to_string())],
                gone: true,
                summary_status: BuildStatus::Succeeded,
                run_id: None,
                category: WorkflowCategory::default(),
                linked_pipeline: None,
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
                category: WorkflowCategory::default(),
                linked_pipeline: None,
            },
            WorkflowGroup {
                name: "wf2".to_string(),
                jobs: vec![make_test_bar("integration-test", BuildStatus::Idle)],
                gone: false,
                summary_status: BuildStatus::Idle,
                run_id: None,
                category: WorkflowCategory::default(),
                linked_pipeline: None,
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
            pending_link: false,
        }];
        assert_eq!(all_pipeline_stages_name_width(&groups), 10); // 6 + 4
    }

    /// Count action rows: CI jobs + review separator + review jobs
    fn count_action_rows(groups: &[&WorkflowGroup]) -> usize {
        let ci_jobs: usize = groups
            .iter()
            .filter(|g| g.category == WorkflowCategory::CI)
            .flat_map(|g| g.jobs.iter())
            .filter(|j| !j.gone)
            .count();
        let review_jobs: usize = groups
            .iter()
            .filter(|g| g.category == WorkflowCategory::Review)
            .flat_map(|g| g.jobs.iter())
            .filter(|j| !j.gone)
            .count();
        let separator = if review_jobs > 0 { 1 } else { 0 };
        ci_jobs + separator + review_jobs
    }

    #[test]
    fn action_rows_includes_separator_when_review_present() {
        let ci = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![make_test_bar("build", BuildStatus::Idle)],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let review = WorkflowGroup {
            name: "Claude Code Review".to_string(),
            jobs: vec![make_test_bar("review", BuildStatus::Idle)],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::Review,
            linked_pipeline: None,
        };
        let groups = vec![&ci, &review];
        // 1 CI job + 1 separator + 1 review job = 3
        assert_eq!(count_action_rows(&groups), 3);
    }

    #[test]
    fn action_rows_no_separator_without_review() {
        let ci = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![make_test_bar("build", BuildStatus::Idle)],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let groups = vec![&ci];
        assert_eq!(count_action_rows(&groups), 1);
    }

    #[test]
    fn action_rows_multiple_ci_groups() {
        let ci1 = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![
                make_test_bar("build", BuildStatus::Idle),
                make_test_bar("test", BuildStatus::Idle),
            ],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let ci2 = WorkflowGroup {
            name: "Deploy".to_string(),
            jobs: vec![make_test_bar("deploy", BuildStatus::Idle)],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let groups = vec![&ci1, &ci2];
        // 3 CI bars, no spacers
        assert_eq!(count_action_rows(&groups), 3);
    }

    #[test]
    fn action_rows_single_group_multiple_jobs() {
        let ci = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![
                make_test_bar("build", BuildStatus::Idle),
                make_test_bar("test", BuildStatus::Idle),
                make_test_bar("lint", BuildStatus::Idle),
            ],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let groups = vec![&ci];
        // 3 jobs
        assert_eq!(count_action_rows(&groups), 3);
    }

    #[test]
    fn action_rows_single_job() {
        let ci = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![make_test_bar("build", BuildStatus::Idle)],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let groups = vec![&ci];
        assert_eq!(count_action_rows(&groups), 1);
    }

    #[test]
    fn action_rows_multiple_groups_with_separator() {
        let ci1 = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![make_test_bar("build", BuildStatus::Idle)],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let ci2 = WorkflowGroup {
            name: "Deploy".to_string(),
            jobs: vec![make_test_bar("deploy", BuildStatus::Idle)],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let review = WorkflowGroup {
            name: "Review".to_string(),
            jobs: vec![make_test_bar("review", BuildStatus::Idle)],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::Review,
            linked_pipeline: None,
        };
        let groups = vec![&ci1, &ci2, &review];
        // 2 CI + 1 separator + 1 review = 4
        assert_eq!(count_action_rows(&groups), 4);
    }

    /// Count pipeline rows: headers + stages + between-group spacers
    fn count_pipe_rows(groups: &[&PipelineGroup]) -> usize {
        let groups_with_stages: Vec<_> = groups
            .iter()
            .filter(|g| g.stages.iter().any(|s| !s.gone))
            .collect();
        let base: usize = groups_with_stages
            .iter()
            .map(|g| 1 + g.stages.iter().filter(|s| !s.gone).count())
            .sum();
        let between_group_spacers = groups_with_stages.len().saturating_sub(1);
        base + between_group_spacers
    }

    #[test]
    fn pipe_rows_multiple_groups_with_spacer() {
        let g1 = PipelineGroup {
            name: "pipe-a".to_string(),
            stages: vec![
                make_test_bar("Source", BuildStatus::Succeeded),
                make_test_bar("Build", BuildStatus::Running),
            ],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        };
        let g2 = PipelineGroup {
            name: "pipe-b".to_string(),
            stages: vec![make_test_bar("Deploy", BuildStatus::Idle)],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: false,
        };
        let groups = vec![&g1, &g2];
        // g1: 1 header + 2 stages = 3
        // g2: 1 header + 1 stage = 2
        // 1 between-group spacer
        // total = 6
        assert_eq!(count_pipe_rows(&groups), 6);
    }

    #[test]
    fn pipe_rows_single_group_multiple_stages() {
        let g = PipelineGroup {
            name: "pipe".to_string(),
            stages: vec![
                make_test_bar("Source", BuildStatus::Succeeded),
                make_test_bar("Build", BuildStatus::Running),
                make_test_bar("Deploy", BuildStatus::Idle),
            ],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        };
        let groups = vec![&g];
        // 1 header + 3 stages = 4
        assert_eq!(count_pipe_rows(&groups), 4);
    }

    #[test]
    fn pipe_rows_single_stage() {
        let g = PipelineGroup {
            name: "pipe".to_string(),
            stages: vec![make_test_bar("Source", BuildStatus::Succeeded)],
            gone: false,
            summary_status: BuildStatus::Succeeded,
            pending_link: false,
        };
        let groups = vec![&g];
        // 1 header + 1 stage = 2
        assert_eq!(count_pipe_rows(&groups), 2);
    }

    #[test]
    fn sorted_workflow_groups_ci_before_review() {
        let groups = vec![
            WorkflowGroup {
                name: "Claude Code Review".to_string(),
                jobs: vec![],
                gone: false,
                summary_status: BuildStatus::Succeeded,
                run_id: None,
                category: WorkflowCategory::Review,
                linked_pipeline: None,
            },
            WorkflowGroup {
                name: "CI".to_string(),
                jobs: vec![make_test_bar("build", BuildStatus::Idle)],
                gone: false,
                summary_status: BuildStatus::Idle,
                run_id: None,
                category: WorkflowCategory::CI,
                linked_pipeline: None,
            },
        ];
        let sorted = sorted_workflow_groups(&groups);
        assert_eq!(sorted[0].name, "CI");
        assert_eq!(sorted[1].name, "Claude Code Review");
    }

    #[test]
    fn sorted_workflow_groups_running_first_within_category() {
        let groups = vec![
            WorkflowGroup {
                name: "zzz-idle".to_string(),
                jobs: vec![],
                gone: false,
                summary_status: BuildStatus::Idle,
                run_id: None,
                category: WorkflowCategory::CI,
                linked_pipeline: None,
            },
            WorkflowGroup {
                name: "aaa-running".to_string(),
                jobs: vec![make_test_bar("build", BuildStatus::Running)],
                gone: false,
                summary_status: BuildStatus::Running,
                run_id: None,
                category: WorkflowCategory::CI,
                linked_pipeline: None,
            },
            WorkflowGroup {
                name: "review-running".to_string(),
                jobs: vec![make_test_bar("job", BuildStatus::Running)],
                gone: false,
                summary_status: BuildStatus::Running,
                run_id: None,
                category: WorkflowCategory::Review,
                linked_pipeline: None,
            },
            WorkflowGroup {
                name: "review-idle".to_string(),
                jobs: vec![],
                gone: false,
                summary_status: BuildStatus::Idle,
                run_id: None,
                category: WorkflowCategory::Review,
                linked_pipeline: None,
            },
        ];
        let sorted = sorted_workflow_groups(&groups);
        // CI first (running before idle), then Review (running before idle)
        assert_eq!(sorted[0].name, "aaa-running");
        assert_eq!(sorted[1].name, "zzz-idle");
        assert_eq!(sorted[2].name, "review-running");
        assert_eq!(sorted[3].name, "review-idle");
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
                category: WorkflowCategory::default(),
                linked_pipeline: None,
            },
            WorkflowGroup {
                name: "aaa-running".to_string(),
                jobs: vec![make_test_bar("build", BuildStatus::Running)],
                gone: false,
                summary_status: BuildStatus::Running,
                run_id: None,
                category: WorkflowCategory::default(),
                linked_pipeline: None,
            },
        ];
        let sorted = sorted_workflow_groups(&groups);
        assert_eq!(sorted[0].name, "aaa-running");
        assert_eq!(sorted[1].name, "zzz-idle");
    }

    #[test]
    fn section_height_expanded_with_content() {
        assert_eq!(section_height(true, true, 5), 7); // 2 borders + 5 rows
    }

    #[test]
    fn section_height_collapsed() {
        assert_eq!(section_height(true, false, 5), 3);
    }

    #[test]
    fn section_height_no_content() {
        assert_eq!(section_height(false, true, 0), 3);
    }

    #[test]
    fn compute_layout_constraints_produces_five_sections() {
        let c = compute_layout_constraints(7, 5);
        assert_eq!(c.len(), 5);
        assert_eq!(c[0], Constraint::Length(3)); // header
        assert_eq!(c[1], Constraint::Length(7)); // actions
        assert_eq!(c[2], Constraint::Length(5)); // pipelines
        assert_eq!(c[3], Constraint::Fill(1)); // fill
        assert_eq!(c[4], Constraint::Length(3)); // status
    }

    #[test]
    fn compute_layout_constraints_collapsed() {
        let c = compute_layout_constraints(3, 3);
        assert_eq!(c[1], Constraint::Length(3));
        assert_eq!(c[2], Constraint::Length(3));
    }

    #[test]
    fn collapsed_actions_renders_dots_and_count() {
        use ratatui::buffer::Buffer;
        let group = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![
                make_test_bar("build", BuildStatus::Succeeded),
                make_test_bar("test", BuildStatus::Running),
            ],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let groups = vec![&group];
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        render_collapsed_actions(&groups, area, &mut buf);
        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(content.contains("(2 jobs)"), "got: {content}");
        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 2);
    }

    #[test]
    fn collapsed_pipelines_renders_dots_and_count() {
        use ratatui::buffer::Buffer;
        let g1 = PipelineGroup {
            name: "pipe-a".to_string(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded,
            pending_link: false,
        };
        let g2 = PipelineGroup {
            name: "pipe-b".to_string(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        };
        let groups = vec![&g1, &g2];
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        render_collapsed_pipelines(&groups, area, &mut buf);
        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(content.contains("(2 pipelines)"), "got: {content}");
    }

    #[test]
    fn collapsed_pipelines_singular_label() {
        use ratatui::buffer::Buffer;
        let g = PipelineGroup {
            name: "pipe".to_string(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded,
            pending_link: false,
        };
        let groups = vec![&g];
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        render_collapsed_pipelines(&groups, area, &mut buf);
        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(content.contains("(1 pipeline)"), "got: {content}");
    }

    #[test]
    fn collapsed_dots_retain_status_colors() {
        use ratatui::buffer::Buffer;
        let group = WorkflowGroup {
            name: "CI".to_string(),
            jobs: vec![
                make_test_bar("build", BuildStatus::Succeeded),
                make_test_bar("test", BuildStatus::Failed),
            ],
            gone: false,
            summary_status: BuildStatus::Failed,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: None,
        };
        let groups = vec![&group];
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        render_collapsed_actions(&groups, area, &mut buf);
        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots[0].fg, theme::STATUS_SUCCESS);
        assert_eq!(dots[1].fg, theme::STATUS_FAILED);
    }

    #[test]
    fn handle_boost_idle_to_watching() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().poll_state = PollState::Idle;
        assert!(handle_boost(&app));
        assert_eq!(app.lock().unwrap().poll_state, PollState::Watching);
        assert!(app.lock().unwrap().boost_pressed_at.is_some());
    }

    #[test]
    fn handle_boost_long_idle_to_watching() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().poll_state = PollState::LongIdle;
        assert!(handle_boost(&app));
        assert_eq!(app.lock().unwrap().poll_state, PollState::Watching);
    }

    #[test]
    fn handle_boost_noop_in_active() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().poll_state = PollState::Active;
        assert!(!handle_boost(&app));
        assert_eq!(app.lock().unwrap().poll_state, PollState::Active);
    }

    #[test]
    fn handle_boost_noop_in_cooldown() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().poll_state = PollState::Cooldown;
        assert!(!handle_boost(&app));
        assert_eq!(app.lock().unwrap().poll_state, PollState::Cooldown);
    }

    #[test]
    fn handle_boost_noop_in_watching() {
        let app = Arc::new(Mutex::new(App::new()));
        app.lock().unwrap().poll_state = PollState::Watching;
        assert!(!handle_boost(&app));
        assert_eq!(app.lock().unwrap().poll_state, PollState::Watching);
    }

    #[test]
    fn sorted_pipeline_groups_filters_linked() {
        let pipelines = vec![
            PipelineGroup {
                name: "linked-pipe".into(),
                stages: vec![],
                gone: false,
                summary_status: BuildStatus::Idle,
                pending_link: false,
            },
            PipelineGroup {
                name: "unlinked-pipe".into(),
                stages: vec![],
                gone: false,
                summary_status: BuildStatus::Idle,
                pending_link: false,
            },
        ];
        let workflows = vec![WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: Some("linked-pipe".into()),
        }];
        let sorted = sorted_pipeline_groups(&pipelines, &workflows);
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].name, "unlinked-pipe");
    }

    #[test]
    fn no_minimum_terminal_size_enforced() {
        // The UI should render at any terminal size, letting ratatui clip naturally.
        // This test prevents reintroduction of a terminal size gate by checking
        // that the non-test source has no "const MIN_" declarations.
        let source = include_str!("mod.rs");
        let prod_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("should have code before #[cfg(test)]");
        assert!(
            !prod_source.contains("const MIN_WIDTH"),
            "const MIN_WIDTH should not exist — no terminal size gate"
        );
        assert!(
            !prod_source.contains("const MIN_HEIGHT"),
            "const MIN_HEIGHT should not exist — no terminal size gate"
        );
    }
}
