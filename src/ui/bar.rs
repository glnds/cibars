use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::model::{Bar, BuildStatus};

/// Max chars for the name column
pub const MAX_NAME_WIDTH: usize = 30;

pub struct BarWidget<'a> {
    bar: &'a Bar,
    name_width: usize,
}

impl<'a> BarWidget<'a> {
    pub fn new(bar: &'a Bar, name_width: usize) -> Self {
        Self {
            bar,
            name_width: name_width.min(MAX_NAME_WIDTH),
        }
    }

    fn status_color(&self) -> Color {
        match self.bar.status {
            BuildStatus::Running => Color::Yellow,
            BuildStatus::Succeeded => Color::Green,
            BuildStatus::Failed => Color::Red,
            BuildStatus::Idle => Color::DarkGray,
        }
    }
}

impl Widget for BarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 1 {
            return;
        }

        let name_col = self.name_width + 2;
        let overhead = name_col + 2;
        if (area.width as usize) <= overhead {
            return;
        }
        let fill_width = area.width as usize - overhead;
        let color = self.status_color();

        let name_display = if self.bar.gone {
            format!("{:<w$}*", self.bar.name, w = self.name_width - 1)
        } else {
            format!("{:<w$}", self.bar.name, w = self.name_width)
        };

        let filled = self.bar.fill.min(fill_width);
        let empty = fill_width - filled;

        let line = Line::from(vec![
            Span::raw(format!("{name_display}  ")),
            Span::raw("["),
            Span::styled("|".repeat(filled), Style::default().fg(color)),
            Span::raw(" ".repeat(empty)),
            Span::raw("]"),
        ]);

        line.render(area, buf);
    }
}

/// Compute the name column width for a set of bars.
pub fn compute_name_width(bars: &[Bar]) -> usize {
    bars.iter()
        .map(|b| b.name.len())
        .max()
        .unwrap_or(10)
        .min(MAX_NAME_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BarSource;

    fn make_bar(name: &str, status: BuildStatus, fill: usize) -> Bar {
        Bar {
            name: name.to_string(),
            source: BarSource::CodePipeline,
            status,
            fill,
            write_pos: fill,
            gone: false,
        }
    }

    #[test]
    fn renders_idle_bar() {
        let bar = make_bar("deploy", BuildStatus::Idle, 0);
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.starts_with("deploy"));
        assert!(content.contains('['));
        assert!(content.ends_with(']'));
    }

    #[test]
    fn renders_running_bar_in_yellow() {
        let bar = make_bar("build", BuildStatus::Running, 3);
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let bracket_pos = buf
            .content()
            .iter()
            .position(|c| c.symbol() == "[")
            .unwrap();
        let first_fill = &buf.content()[bracket_pos + 1];
        assert_eq!(first_fill.symbol(), "|");
        assert_eq!(first_fill.fg, Color::Yellow);
    }

    #[test]
    fn renders_succeeded_bar_in_green() {
        let bar = make_bar("test", BuildStatus::Succeeded, 5);
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let bracket_pos = buf
            .content()
            .iter()
            .position(|c| c.symbol() == "[")
            .unwrap();
        let first_fill = &buf.content()[bracket_pos + 1];
        assert_eq!(first_fill.fg, Color::Green);
    }

    #[test]
    fn renders_failed_bar_in_red() {
        let bar = make_bar("lint", BuildStatus::Failed, 2);
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let bracket_pos = buf
            .content()
            .iter()
            .position(|c| c.symbol() == "[")
            .unwrap();
        let first_fill = &buf.content()[bracket_pos + 1];
        assert_eq!(first_fill.fg, Color::Red);
    }

    #[test]
    fn gone_bar_shows_asterisk() {
        let mut bar = make_bar("old", BuildStatus::Idle, 0);
        bar.gone = true;
        let widget = BarWidget::new(&bar, 10);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains('*'));
    }

    #[test]
    fn name_width_computed_correctly() {
        let bars = vec![
            make_bar("a", BuildStatus::Idle, 0),
            make_bar("long-name", BuildStatus::Idle, 0),
        ];
        assert_eq!(compute_name_width(&bars), 9);
    }
}
