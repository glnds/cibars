use chrono::Local;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::theme;
use crate::app::SourceHealth;

pub struct Header<'a> {
    pub profile: &'a str,
    pub region: &'a str,
    pub repo: &'a str,
    pub aws_health: &'a SourceHealth,
}

impl Widget for Header<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let time = Local::now().format("%H:%M:%S");

        // Profile span: red with suffix when auth failed
        let profile_spans: Vec<Span> = match self.aws_health {
            SourceHealth::AuthFailed { .. } => vec![
                Span::styled(self.profile, Style::default().fg(theme::STATUS_FAILED)),
                Span::styled(
                    " \u{26A0} SSO expired",
                    Style::default().fg(theme::STATUS_FAILED),
                ),
            ],
            SourceHealth::Healthy => vec![Span::raw(self.profile)],
        };

        let mut spans = vec![
            Span::styled(
                format!(
                    "cibars (v{}-{})",
                    env!("CARGO_PKG_VERSION"),
                    env!("VERGEN_GIT_COMMIT_COUNT"),
                ),
                Style::default()
                    .fg(theme::BORDER_HEADER)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
        ];
        spans.extend(profile_spans);
        spans.extend([
            Span::raw(" | "),
            Span::raw(self.region),
            Span::raw(" | "),
            Span::raw(self.repo),
            Span::raw(" | "),
            Span::styled(format!("{time}"), Style::default().fg(theme::FG_DIM)),
        ]);

        Line::from(spans).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SourceHealth;
    use crate::ui::theme;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};

    fn render_header_with_health(
        profile: &str,
        region: &str,
        repo: &str,
        aws_health: &SourceHealth,
    ) -> Buffer {
        let header = Header {
            profile,
            region,
            repo,
            aws_health,
        };
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        header.render(area, &mut buf);
        buf
    }

    fn render_header(profile: &str, region: &str, repo: &str) -> Buffer {
        render_header_with_health(profile, region, repo, &SourceHealth::Healthy)
    }

    #[test]
    fn renders_sso_expired_when_auth_failed() {
        let health = SourceHealth::AuthFailed {
            since: chrono::Utc::now(),
        };
        let buf = render_header_with_health("my-profile", "eu-west-1", "owner/repo", &health);
        let content = buffer_text(&buf);
        assert!(content.contains("SSO expired"), "got: {content}");
    }

    #[test]
    fn renders_profile_red_when_auth_failed() {
        let health = SourceHealth::AuthFailed {
            since: chrono::Utc::now(),
        };
        let buf = render_header_with_health("my-profile", "eu-west-1", "owner/repo", &health);
        let content = buffer_text(&buf);
        let profile_pos = content.find("my-profile").expect("profile not found");
        let cell = &buf.content()[profile_pos];
        assert_eq!(cell.fg, theme::STATUS_FAILED);
    }

    #[test]
    fn renders_profile_default_when_healthy() {
        let health = SourceHealth::Healthy;
        let buf = render_header_with_health("my-profile", "eu-west-1", "owner/repo", &health);
        let content = buffer_text(&buf);
        assert!(!content.contains("SSO expired"), "got: {content}");
        let profile_pos = content.find("my-profile").expect("profile not found");
        let cell = &buf.content()[profile_pos];
        assert_eq!(cell.fg, Color::Reset);
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
        assert_eq!(cell.fg, theme::BORDER_HEADER);
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
        assert_eq!(cell.fg, theme::FG_DIM);
    }

    #[test]
    fn renders_all_separator_pipes() {
        let buf = render_header("p", "r", "o/r");
        let content = buffer_text(&buf);
        let count = content.matches(" | ").count();
        assert_eq!(count, 4, "expected 4 separators, got {count} in: {content}");
    }
}
