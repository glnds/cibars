pub mod bar;
pub mod header;
pub mod statusbar;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::DefaultTerminal;

use crate::app::App;

const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 10;
const TICK_RATE_MS: u64 = 250;

pub fn run_ui(app: Arc<Mutex<App>>, mut terminal: DefaultTerminal) -> Result<()> {
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

            let [header_area, pipes_area, actions_area, status_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(size);

            frame.render_widget(Paragraph::new("cibars"), header_area);

            frame.render_widget(
                Block::default()
                    .title("CodePipelines")
                    .borders(Borders::ALL),
                pipes_area,
            );

            frame.render_widget(
                Block::default()
                    .title("GitHub Actions")
                    .borders(Borders::ALL),
                actions_area,
            );

            frame.render_widget(Paragraph::new("q=quit  r=refresh"), status_area);

            drop(app);
        })?;

        if event::poll(Duration::from_millis(TICK_RATE_MS))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('r') => {
                        // TODO: trigger immediate poll
                    }
                    _ => {}
                }
            }
        }
    }
}
