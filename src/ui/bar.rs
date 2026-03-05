use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::model::{Bar, WorkflowGroup};

/// Max chars for the name column
pub const MAX_NAME_WIDTH: usize = 30;

pub struct BarWidget<'a> {
    bar: &'a Bar,
    name_width: usize,
    dim: bool,
}

impl<'a> BarWidget<'a> {
    pub fn new(bar: &'a Bar, name_width: usize, dim: bool) -> Self {
        Self {
            bar,
            name_width: name_width.min(MAX_NAME_WIDTH),
            dim,
        }
    }

    fn status_color(&self) -> Color {
        if self.dim {
            Color::DarkGray
        } else {
            self.bar.status.color()
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

/// "GitHub Actions" title with inline status dots for all jobs across groups.
pub struct ActionsTitle<'a> {
    groups: &'a [&'a WorkflowGroup],
}

impl<'a> ActionsTitle<'a> {
    pub fn new(groups: &'a [&'a WorkflowGroup]) -> Self {
        Self { groups }
    }
}

impl Widget for ActionsTitle<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 1 {
            return;
        }

        let mut spans = vec![Span::styled(
            "GitHub Actions ",
            Style::default().fg(Color::Cyan),
        )];

        for group in self.groups {
            let visible_jobs: Vec<_> = group.jobs.iter().filter(|j| !j.gone).collect();
            if visible_jobs.is_empty() {
                // Jobs not loaded yet; show one dot per workflow using summary status
                spans.push(Span::styled(
                    "\u{25CF} ",
                    Style::default().fg(group.summary_status.color()),
                ));
            } else {
                for job in visible_jobs {
                    spans.push(Span::styled(
                        "\u{25CF} ",
                        Style::default().fg(job.status.color()),
                    ));
                }
            }
        }

        Line::from(spans).render(area, buf);
    }
}

/// "CodePipelines" title with inline status dots for each pipeline.
pub struct PipelinesTitle<'a> {
    bars: &'a [Bar],
}

impl<'a> PipelinesTitle<'a> {
    pub fn new(bars: &'a [Bar]) -> Self {
        Self { bars }
    }
}

impl Widget for PipelinesTitle<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 1 {
            return;
        }

        let mut spans = vec![Span::styled(
            "CodePipelines ",
            Style::default().fg(Color::Cyan),
        )];

        for bar in self.bars {
            spans.push(Span::styled(
                "\u{25CF} ",
                Style::default().fg(bar.status.color()),
            ));
        }

        Line::from(spans).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BuildStatus;

    fn make_bar(name: &str, status: BuildStatus, fill: usize) -> Bar {
        Bar {
            name: name.to_string(),
            status,
            fill,
            write_pos: fill,
            gone: false,
        }
    }

    #[test]
    fn renders_idle_bar() {
        let bar = make_bar("deploy", BuildStatus::Idle, 0);
        let widget = BarWidget::new(&bar, 10, false);
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
        let widget = BarWidget::new(&bar, 10, false);
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
        let widget = BarWidget::new(&bar, 10, false);
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
        let widget = BarWidget::new(&bar, 10, false);
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
        let widget = BarWidget::new(&bar, 10, false);
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

    fn make_group(name: &str, job_statuses: &[BuildStatus]) -> WorkflowGroup {
        WorkflowGroup {
            name: name.to_string(),
            jobs: job_statuses
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let mut bar = Bar::new(format!("job-{i}"));
                    bar.status = *s;
                    bar
                })
                .collect(),
            gone: false,
            summary_status: BuildStatus::Running,
        }
    }

    #[test]
    fn actions_title_renders_label_and_dots() {
        let group = make_group("CI", &[BuildStatus::Succeeded, BuildStatus::Running]);
        let groups = vec![&group];
        let widget = ActionsTitle::new(&groups);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("GitHub Actions"));

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 2);
        assert_eq!(dots[0].fg, Color::Green);
        assert_eq!(dots[1].fg, Color::Yellow);
    }

    #[test]
    fn actions_title_dot_colors() {
        let group = make_group(
            "Deploy",
            &[
                BuildStatus::Failed,
                BuildStatus::Idle,
                BuildStatus::Succeeded,
                BuildStatus::Running,
            ],
        );
        let groups = vec![&group];
        let widget = ActionsTitle::new(&groups);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 4);
        assert_eq!(dots[0].fg, Color::Red);
        assert_eq!(dots[1].fg, Color::DarkGray);
        assert_eq!(dots[2].fg, Color::Green);
        assert_eq!(dots[3].fg, Color::Yellow);
    }

    #[test]
    fn pipelines_title_renders_label_and_dots() {
        let bars = vec![
            make_bar("pipe-a", BuildStatus::Succeeded, 5),
            make_bar("pipe-b", BuildStatus::Running, 3),
        ];
        let widget = PipelinesTitle::new(&bars);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("CodePipelines"));

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 2);
        assert_eq!(dots[0].fg, Color::Green);
        assert_eq!(dots[1].fg, Color::Yellow);
    }

    #[test]
    fn pipelines_title_dot_colors() {
        let bars = vec![
            make_bar("p1", BuildStatus::Failed, 1),
            make_bar("p2", BuildStatus::Idle, 0),
            make_bar("p3", BuildStatus::Succeeded, 5),
            make_bar("p4", BuildStatus::Running, 2),
        ];
        let widget = PipelinesTitle::new(&bars);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 4);
        assert_eq!(dots[0].fg, Color::Red);
        assert_eq!(dots[1].fg, Color::DarkGray);
        assert_eq!(dots[2].fg, Color::Green);
        assert_eq!(dots[3].fg, Color::Yellow);
    }

    #[test]
    fn dim_bar_renders_dark_gray_regardless_of_status() {
        let bar = make_bar("build", BuildStatus::Running, 3);
        let widget = BarWidget::new(&bar, 10, true);
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
        assert_eq!(first_fill.fg, Color::DarkGray);
    }

    #[test]
    fn actions_title_multiple_groups() {
        let g1 = make_group("CI", &[BuildStatus::Succeeded]);
        let g2 = make_group("Deploy", &[BuildStatus::Failed, BuildStatus::Running]);
        let groups = vec![&g1, &g2];
        let widget = ActionsTitle::new(&groups);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 3);
    }
}
