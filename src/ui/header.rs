use chrono::Local;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

pub struct Header<'a> {
    pub profile: &'a str,
    pub region: &'a str,
    pub repo: &'a str,
}

impl Widget for Header<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let time = Local::now().format("%H:%M:%S");
        let line = Line::from(vec![
            Span::styled(
                format!(
                    "cibars (v{}-{})",
                    env!("CARGO_PKG_VERSION"),
                    env!("VERGEN_GIT_COMMIT_COUNT"),
                ),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::raw(self.profile),
            Span::raw(" | "),
            Span::raw(self.region),
            Span::raw(" | "),
            Span::raw(self.repo),
            Span::raw(" | "),
            Span::styled(format!("{time}"), Style::default().fg(Color::DarkGray)),
        ]);
        line.render(area, buf);
    }
}
