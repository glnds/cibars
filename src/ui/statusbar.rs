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
                let secs = Utc::now().signed_duration_since(*t).num_seconds().max(0);
                format!("Last poll: {secs}s ago")
            }
            None => "Last poll: --".to_string(),
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

    fn render_status_bar(last_poll: &Option<DateTime<Utc>>) -> String {
        let bar = StatusBar {
            last_poll,
            warnings: &[],
        };
        let area = Rect::new(0, 0, 120, 1);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        (0..120)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect()
    }

    #[test]
    fn status_bar_contains_version_info() {
        let content = render_status_bar(&None);
        assert!(
            content.contains("v0.1.0"),
            "expected version, got: {content}"
        );
    }

    #[test]
    fn status_bar_no_poll_shows_dash() {
        let content = render_status_bar(&None);
        assert!(
            content.contains("Last poll: --"),
            "expected --, got: {content}"
        );
    }

    #[test]
    fn status_bar_recent_poll_shows_seconds_ago() {
        let content = render_status_bar(&Some(Utc::now()));
        assert!(
            content.contains("0s ago"),
            "expected 0s ago, got: {content}"
        );
    }
}
