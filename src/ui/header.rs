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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};

    fn render_header(profile: &str, region: &str, repo: &str) -> Buffer {
        let header = Header {
            profile,
            region,
            repo,
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        header.render(area, &mut buf);
        buf
    }

    fn buffer_text(buf: &Buffer) -> String {
        buf.content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }

    #[test]
    fn renders_profile_region_repo() {
        let buf = render_header("my-profile", "eu-west-1", "owner/repo");
        let content = buffer_text(&buf);
        assert!(content.contains("my-profile"), "got: {content}");
        assert!(content.contains("eu-west-1"), "got: {content}");
        assert!(content.contains("owner/repo"), "got: {content}");
    }

    #[test]
    fn renders_version_in_cyan_bold() {
        let buf = render_header("p", "r", "o/r");
        let content = buffer_text(&buf);
        let version_start = content.find("cibars").expect("version string not found");
        let cell = &buf.content()[version_start];
        assert_eq!(cell.fg, Color::Cyan);
        assert!(
            cell.modifier.contains(Modifier::BOLD),
            "expected BOLD modifier"
        );
    }

    #[test]
    fn renders_timestamp_in_dark_gray() {
        let buf = render_header("p", "r", "o/r");
        let content = buffer_text(&buf);
        // Timestamp is the last span; find its position after the last " | "
        let last_sep = content.rfind(" | ").expect("no separator found");
        let ts_start = last_sep + 3;
        let cell = &buf.content()[ts_start];
        assert_eq!(cell.fg, Color::DarkGray);
    }

    #[test]
    fn renders_all_separator_pipes() {
        let buf = render_header("p", "r", "o/r");
        let content = buffer_text(&buf);
        let count = content.matches(" | ").count();
        assert_eq!(count, 4, "expected 4 separators, got {count} in: {content}");
    }
}
