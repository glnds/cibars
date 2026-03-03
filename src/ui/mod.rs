pub mod bar;
pub mod header;
pub mod statusbar;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use ratatui::DefaultTerminal;

use crate::app::App;
use crate::model::{Bar, BuildStatus};

use bar::{compute_name_width, BarWidget};
use header::Header;
use statusbar::StatusBar;

const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 10;
const TICK_RATE_MS: u64 = 250;

fn sorted_bars(bars: &[Bar]) -> Vec<&Bar> {
    let mut sorted: Vec<&Bar> = bars.iter().collect();
    sorted.sort_by(|a, b| {
        let a_running = a.status == BuildStatus::Running;
        let b_running = b.status == BuildStatus::Running;
        b_running.cmp(&a_running).then(a.name.cmp(&b.name))
    });
    sorted
}

pub fn run_ui(
    app: Arc<Mutex<App>>,
    mut terminal: DefaultTerminal,
    profile: &str,
    region: &str,
    repo: &str,
) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let size = frame.area();

            if size.width < MIN_WIDTH || size.height < MIN_HEIGHT {
                let msg =
                    Paragraph::new("Terminal too small").style(Style::default().fg(Color::Red));
                frame.render_widget(msg, size);
                return;
            }

            let app = app.lock().expect("app mutex poisoned");

            let pipes_sorted = sorted_bars(&app.bars_pipelines);
            let actions_sorted = sorted_bars(&app.bars_actions);

            let pipe_count = pipes_sorted.len();
            let action_count = actions_sorted.len();

            // Build dynamic layout constraints
            let mut constraints = vec![Constraint::Length(1)]; // header
            constraints.push(Constraint::Length(1)); // pipelines title
            for _ in 0..pipe_count {
                constraints.push(Constraint::Length(1));
            }
            constraints.push(Constraint::Length(1)); // actions title
            for _ in 0..action_count {
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

            // Pipelines title
            if pipe_count == 0 {
                frame.render_widget(
                    Paragraph::new("No pipelines found in this account/region")
                        .style(Style::default().fg(Color::DarkGray)),
                    areas[idx],
                );
            } else {
                frame.render_widget(
                    Paragraph::new("CodePipelines").style(Style::default().fg(Color::Cyan)),
                    areas[idx],
                );
            }
            idx += 1;

            // Pipeline bars
            let pipe_name_width = compute_name_width(&app.bars_pipelines);
            for bar in &pipes_sorted {
                frame.render_widget(BarWidget::new(bar, pipe_name_width), areas[idx]);
                idx += 1;
            }

            // Actions title
            if action_count == 0 {
                frame.render_widget(
                    Paragraph::new("No recent workflow runs found")
                        .style(Style::default().fg(Color::DarkGray)),
                    areas[idx],
                );
            } else {
                frame.render_widget(
                    Paragraph::new("GitHub Actions").style(Style::default().fg(Color::Cyan)),
                    areas[idx],
                );
            }
            idx += 1;

            // Action bars
            let action_name_width = compute_name_width(&app.bars_actions);
            for bar in &actions_sorted {
                frame.render_widget(BarWidget::new(bar, action_name_width), areas[idx]);
                idx += 1;
            }

            // Skip fill area
            idx += 1;

            // Status bar
            frame.render_widget(
                StatusBar {
                    last_poll: &app.last_poll,
                    warnings: &app.warnings,
                },
                areas[idx],
            );

            drop(app);
        })?;

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
                    KeyCode::Char('r') => {
                        // TODO: trigger immediate poll
                    }
                    _ => {}
                }
            }
        }
    }
}
