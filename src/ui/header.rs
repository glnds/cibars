use chrono::Local;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};

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

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::BORDER_HEADER))
            .title(Span::styled(
                format!(
                    " cibars v{}-{} ",
                    env!("CARGO_PKG_VERSION"),
                    env!("VERGEN_GIT_COMMIT_COUNT"),
                ),
                Style::default()
                    .fg(theme::BORDER_HEADER)
                    .add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(area);
        block.render(area, buf);

        // Profile span: color + icon reflects AWS SSO session state
        let profile_spans: Vec<Span> = match self.aws_health {
            SourceHealth::AuthFailed { .. } => vec![
                Span::styled(self.profile, Style::default().fg(theme::STATUS_FAILED)),
                Span::styled(
                    " \u{26A0} SSO expired",
                    Style::default().fg(theme::STATUS_FAILED),
                ),
            ],
            SourceHealth::Healthy => vec![
                Span::raw(self.profile),
                Span::styled(
                    format!(" {}", theme::CHECK_MARK),
                    Style::default().fg(theme::STATUS_SUCCESS),
                ),
            ],
            SourceHealth::Unknown => vec![
                Span::raw(self.profile),
                Span::styled(
                    format!(" {}", theme::CHECK_MARK),
                    Style::default().fg(theme::FG_DIM),
                ),
            ],
        };

        let sep = Span::styled(" \u{2502} ", Style::default().fg(theme::SEPARATOR));

        let mut spans: Vec<Span> = Vec::new();
        spans.extend(profile_spans);
        spans.push(sep.clone());
        spans.push(Span::raw(self.region));
        spans.push(sep.clone());
        spans.push(Span::raw(self.repo));
        spans.push(sep);
        spans.push(Span::styled(
            format!("{time}"),
            Style::default().fg(theme::FG_DIM),
        ));

        Line::from(spans).render(inner, buf);
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
        let area = Rect::new(0, 0, 100, 3);
        let mut buf = Buffer::empty(area);
        header.render(area, &mut buf);
        buf
    }

    fn render_header(profile: &str, region: &str, repo: &str) -> Buffer {
        render_header_with_health(profile, region, repo, &SourceHealth::Healthy)
    }

    /// Extract text from a single row of the buffer.
    fn row_text(buf: &Buffer, row: u16) -> String {
        let width = buf.area().width as usize;
        let start = row as usize * width;
        buf.content()[start..start + width]
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }

    #[test]
    fn renders_sso_expired_when_auth_failed() {
        let health = SourceHealth::AuthFailed {
            since: chrono::Utc::now(),
        };
        let buf = render_header_with_health("my-profile", "eu-west-1", "owner/repo", &health);
        let content = row_text(&buf, 1);
        assert!(content.contains("SSO expired"), "got: {content}");
    }

    #[test]
    fn renders_profile_red_when_auth_failed() {
        let health = SourceHealth::AuthFailed {
            since: chrono::Utc::now(),
        };
        let buf = render_header_with_health("my-profile", "eu-west-1", "owner/repo", &health);
        let content = row_text(&buf, 1);
        let profile_pos = content.find("my-profile").expect("profile not found");
        // Offset into buffer: row 1 starts at width
        let cell = &buf.content()[buf.area().width as usize + profile_pos];
        assert_eq!(cell.fg, theme::STATUS_FAILED);
    }

    #[test]
    fn renders_profile_default_when_healthy() {
        let health = SourceHealth::Healthy;
        let buf = render_header_with_health("my-profile", "eu-west-1", "owner/repo", &health);
        let content = row_text(&buf, 1);
        assert!(!content.contains("SSO expired"), "got: {content}");
        let profile_pos = content.find("my-profile").expect("profile not found");
        let cell = &buf.content()[buf.area().width as usize + profile_pos];
        assert_eq!(cell.fg, Color::Reset);
    }

    #[test]
    fn renders_profile_region_repo() {
        let buf = render_header("my-profile", "eu-west-1", "owner/repo");
        let content = row_text(&buf, 1);
        assert!(content.contains("my-profile"), "got: {content}");
        assert!(content.contains("eu-west-1"), "got: {content}");
        assert!(content.contains("owner/repo"), "got: {content}");
    }

    #[test]
    fn renders_version_in_cyan_bold() {
        let buf = render_header("p", "r", "o/r");
        // Version is now in the block title on row 0
        let row0 = row_text(&buf, 0);
        let version_pos = row0
            .find("cibars")
            .expect("version string not found in title");
        let cell = &buf.content()[version_pos];
        assert_eq!(cell.fg, theme::BORDER_HEADER);
        assert!(
            cell.modifier.contains(Modifier::BOLD),
            "expected BOLD modifier"
        );
    }

    #[test]
    fn renders_timestamp_in_dark_gray() {
        let buf = render_header("p", "r", "o/r");
        // Find 3rd separator (after profile, region, repo) — skip border │
        let width = buf.area().width as usize;
        let row_start = width; // row 1
        let cells = &buf.content()[row_start..row_start + width];
        let sep_positions: Vec<usize> = cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.symbol() == "\u{2502}" && c.fg == theme::SEPARATOR)
            .map(|(i, _)| i)
            .collect();
        assert!(sep_positions.len() >= 3, "expected ≥3 separators");
        let ts_start = sep_positions[2] + 2; // skip "│ "
        let cell = &buf.content()[row_start + ts_start];
        assert_eq!(cell.fg, theme::FG_DIM);
    }

    #[test]
    fn renders_all_separator_pipes() {
        let buf = render_header("p", "r", "o/r");
        let content = row_text(&buf, 1);
        // Separators are now dim │ chars, count them in the content row only
        let count = content.matches(" \u{2502} ").count();
        assert_eq!(count, 3, "expected 3 separators, got {count} in: {content}");
    }

    // --- New tests for Block wrapping ---

    #[test]
    fn header_renders_in_rounded_block() {
        let buf = render_header("p", "r", "o/r");
        let row0 = row_text(&buf, 0);
        let row2 = row_text(&buf, 2);
        // Rounded corners: ╭ top-left, ╰ bottom-left
        assert!(
            row0.starts_with("╭"),
            "expected rounded top-left corner, got: {row0}"
        );
        assert!(
            row2.starts_with("╰"),
            "expected rounded bottom-left corner, got: {row2}"
        );
    }

    #[test]
    fn header_block_has_title() {
        let buf = render_header("p", "r", "o/r");
        let row0 = row_text(&buf, 0);
        assert!(
            row0.contains("cibars"),
            "expected 'cibars' in block title, got: {row0}"
        );
    }

    #[test]
    fn header_block_border_color() {
        let buf = render_header("p", "r", "o/r");
        // Top-left corner cell should have BORDER_HEADER color
        let cell = &buf.content()[0];
        assert_eq!(cell.fg, theme::BORDER_HEADER);
    }

    /// Find cell index of a character in a buffer row (char offset, not byte offset).
    fn find_cell_pos(buf: &Buffer, row: u16, needle: char) -> Option<usize> {
        let width = buf.area().width as usize;
        let start = row as usize * width;
        buf.content()[start..start + width]
            .iter()
            .position(|c| c.symbol().starts_with(needle))
    }

    #[test]
    fn renders_dim_checkmark_when_unknown() {
        let health = SourceHealth::Unknown;
        let buf = render_header_with_health("my-profile", "eu-west-1", "owner/repo", &health);
        let check_pos = find_cell_pos(&buf, 1, '\u{2713}').expect("✓ not found in row");
        let cell = &buf.content()[buf.area().width as usize + check_pos];
        assert_eq!(cell.fg, theme::FG_DIM);
    }

    #[test]
    fn renders_green_checkmark_when_healthy() {
        let health = SourceHealth::Healthy;
        let buf = render_header_with_health("my-profile", "eu-west-1", "owner/repo", &health);
        let check_pos = find_cell_pos(&buf, 1, '\u{2713}').expect("✓ not found in row");
        let cell = &buf.content()[buf.area().width as usize + check_pos];
        assert_eq!(cell.fg, theme::STATUS_SUCCESS);
    }
}
