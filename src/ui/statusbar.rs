use chrono::{DateTime, Utc};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

pub struct StatusBar<'a> {
    pub last_poll: &'a Option<DateTime<Utc>>,
    pub warnings: &'a [String],
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let poll_text = match self.last_poll {
            Some(t) => {
                let local = t.with_timezone(&chrono::Local);
                format!("Last poll: {}", local.format("%H:%M:%S"))
            }
            None => "Last poll: --:--:--".to_string(),
        };

        let mut spans = vec![
            Span::raw(poll_text),
            Span::raw(" | e=expand r=refresh q=quit"),
        ];

        if !self.warnings.is_empty() {
            let warn_text = format!(" | {}", self.warnings.join("; "));
            spans.push(Span::styled(warn_text, Style::default().fg(Color::Yellow)));
        }

        Line::from(spans).render(area, buf);
    }
}
