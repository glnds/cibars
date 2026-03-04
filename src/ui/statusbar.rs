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
        let version = format!(
            "v{}-{} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("VERGEN_GIT_COMMIT_COUNT"),
            env!("VERGEN_GIT_SHA"),
        );

        let poll_text = match self.last_poll {
            Some(t) => {
                let local = t.with_timezone(&chrono::Local);
                format!("Last poll: {}", local.format("%H:%M:%S"))
            }
            None => "Last poll: --:--:--".to_string(),
        };

        let mut spans = vec![
            Span::styled(version, Style::default().fg(Color::DarkGray)),
            Span::raw(" | "),
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn status_bar_contains_version_info() {
        let bar = StatusBar {
            last_poll: &None,
            warnings: &[],
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);

        let content: String = (0..80)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect();

        assert!(
            content.contains("v0.1.0"),
            "expected version, got: {content}"
        );
        assert!(
            content.contains("("),
            "expected build info parens, got: {content}"
        );
    }
}
