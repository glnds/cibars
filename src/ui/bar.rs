use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::model::{format_finished_time, Bar, PipelineGroup, WorkflowCategory, WorkflowGroup};

/// Max chars for the name column
pub const MAX_NAME_WIDTH: usize = 30;

pub struct BarWidget<'a> {
    bar: &'a Bar,
    name_width: usize,
    dim: bool,
    status_dot: Option<Color>,
}

impl<'a> BarWidget<'a> {
    pub fn new(bar: &'a Bar, name_width: usize, dim: bool) -> Self {
        Self {
            bar,
            name_width: name_width.min(MAX_NAME_WIDTH),
            dim,
            status_dot: None,
        }
    }

    pub fn with_dot(mut self, color: Color) -> Self {
        self.status_dot = Some(color);
        self
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

        let ts_str = self
            .bar
            .last_finished
            .map(|t| format_finished_time(&t, &chrono::Local));
        let ts_reserve = 7; // always reserve space for timestamp stability

        let dot_prefix_len = if self.status_dot.is_some() { 2 } else { 0 };
        let name_col = self.name_width + 2;
        let overhead = dot_prefix_len + name_col + 2 + ts_reserve;
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

        let mut spans = Vec::new();
        if let Some(dot_color) = self.status_dot {
            spans.push(Span::styled("\u{25CF} ", Style::default().fg(dot_color)));
        }
        spans.push(Span::raw(format!("{name_display}  ")));
        spans.push(Span::raw("["));
        spans.push(Span::styled("|".repeat(filled), Style::default().fg(color)));
        spans.push(Span::raw(" ".repeat(empty)));
        spans.push(Span::raw("]"));
        match ts_str {
            Some(ts) => spans.push(Span::styled(
                format!(" {ts} "),
                Style::default().fg(Color::DarkGray),
            )),
            None => spans.push(Span::raw("       ")), // 7 spaces
        }

        Line::from(spans).render(area, buf);
    }
}

/// Compute the name column width for a set of bars.
#[cfg(test)]
pub fn compute_name_width(bars: &[Bar]) -> usize {
    bars.iter()
        .map(|b| b.name.len())
        .max()
        .unwrap_or(10)
        .min(MAX_NAME_WIDTH)
}

/// "GitHub Actions" title with inline status dots for all jobs across groups.
/// Dots always retain their status color — they are never dimmed by poll state.
pub struct ActionsTitle<'a> {
    groups: &'a [&'a WorkflowGroup],
    expanded: bool,
}

impl<'a> ActionsTitle<'a> {
    pub fn new(groups: &'a [&'a WorkflowGroup], expanded: bool) -> Self {
        Self { groups, expanded }
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

        if !self.expanded {
            for group in self.groups {
                let is_review = group.category == WorkflowCategory::Review;
                let visible_jobs: Vec<_> = group.jobs.iter().filter(|j| !j.gone).collect();
                if visible_jobs.is_empty() {
                    let color = if group.gone || is_review {
                        Color::DarkGray
                    } else {
                        group.summary_status.color()
                    };
                    spans.push(Span::styled("\u{25CF} ", Style::default().fg(color)));
                } else {
                    for job in visible_jobs {
                        let color = if group.gone || is_review {
                            Color::DarkGray
                        } else {
                            job.status.color()
                        };
                        spans.push(Span::styled("\u{25CF} ", Style::default().fg(color)));
                    }
                }
            }
        }

        Line::from(spans).render(area, buf);
    }
}

/// "CodePipelines" title with inline status dots for each pipeline group.
/// Dots always retain their status color — they are never dimmed by poll state.
pub struct PipelinesTitle<'a> {
    groups: &'a [&'a PipelineGroup],
    expanded: bool,
}

impl<'a> PipelinesTitle<'a> {
    pub fn new(groups: &'a [&'a PipelineGroup], expanded: bool) -> Self {
        Self { groups, expanded }
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

        if !self.expanded {
            for group in self.groups {
                let color = if group.gone || group.pending_link {
                    Color::DarkGray
                } else {
                    group.summary_status.color()
                };
                spans.push(Span::styled("\u{25CF} ", Style::default().fg(color)));
            }
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
            last_finished: None,
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
        assert!(content.contains(']'));
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
            run_id: None,
            category: WorkflowCategory::CI,
        }
    }

    #[test]
    fn actions_title_renders_label_and_dots() {
        let group = make_group("CI", &[BuildStatus::Succeeded, BuildStatus::Running]);
        let groups = vec![&group];
        let widget = ActionsTitle::new(&groups, false);
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
        let widget = ActionsTitle::new(&groups, false);
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

    fn make_pipe_group(name: &str, summary_status: BuildStatus) -> PipelineGroup {
        PipelineGroup {
            name: name.to_string(),
            stages: vec![],
            gone: false,
            summary_status,
            pending_link: false,
        }
    }

    #[test]
    fn pipelines_title_renders_label_and_dots() {
        let g1 = make_pipe_group("pipe-a", BuildStatus::Succeeded);
        let g2 = make_pipe_group("pipe-b", BuildStatus::Running);
        let groups = vec![&g1, &g2];
        let widget = PipelinesTitle::new(&groups, false);
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
        // One dot per pipeline group
        assert_eq!(dots.len(), 2);
        assert_eq!(dots[0].fg, Color::Green);
        assert_eq!(dots[1].fg, Color::Yellow);
    }

    #[test]
    fn pipelines_title_dot_colors() {
        let g1 = make_pipe_group("p1", BuildStatus::Failed);
        let g2 = make_pipe_group("p2", BuildStatus::Idle);
        let g3 = make_pipe_group("p3", BuildStatus::Succeeded);
        let g4 = make_pipe_group("p4", BuildStatus::Running);
        let groups = vec![&g1, &g2, &g3, &g4];
        let widget = PipelinesTitle::new(&groups, false);
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
    fn pipelines_title_gone_group_dimmed() {
        let mut g = make_pipe_group("pipe", BuildStatus::Running);
        g.gone = true;
        let groups = vec![&g];
        let widget = PipelinesTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 1);
        assert_eq!(dots[0].fg, Color::DarkGray);
    }

    #[test]
    fn actions_title_dots_stay_colored_when_dim() {
        let group = make_group("CI", &[BuildStatus::Succeeded, BuildStatus::Running]);
        let groups = vec![&group];
        let widget = ActionsTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 2);
        // Dots should retain status colors even when dim=true
        assert_eq!(dots[0].fg, Color::Green);
        assert_eq!(dots[1].fg, Color::Yellow);
    }

    #[test]
    fn pipelines_title_dots_stay_colored_when_dim() {
        let g1 = make_pipe_group("pipe-a", BuildStatus::Succeeded);
        let g2 = make_pipe_group("pipe-b", BuildStatus::Running);
        let groups = vec![&g1, &g2];
        let widget = PipelinesTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 2);
        // Dots should retain status colors even when dim=true
        assert_eq!(dots[0].fg, Color::Green);
        assert_eq!(dots[1].fg, Color::Yellow);
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
        let widget = ActionsTitle::new(&groups, false);
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

    #[test]
    fn dim_bar_succeeded_renders_dark_gray() {
        let bar = make_bar("deploy", BuildStatus::Succeeded, 5);
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
    fn dim_bar_failed_renders_dark_gray() {
        let bar = make_bar("lint", BuildStatus::Failed, 3);
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
    fn dim_bar_idle_renders_dark_gray() {
        let bar = make_bar("idle", BuildStatus::Idle, 0);
        let widget = BarWidget::new(&bar, 10, true);
        let area = Rect::new(0, 0, 25, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        // Idle bar has fill=0, so no '|' chars — just verify it doesn't crash
        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains('['));
        assert!(content.contains(']'));
    }

    #[test]
    fn bar_renders_nothing_for_small_area() {
        let bar = make_bar("build", BuildStatus::Running, 3);
        let widget = BarWidget::new(&bar, 10, false);
        let area = Rect::new(0, 0, 9, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert_eq!(content.trim(), "");
    }

    #[test]
    fn bar_renders_nothing_for_narrow_area() {
        let name_width: usize = 10;
        let overhead = name_width + 2 + 2; // name_col + 2
        let bar = make_bar("build", BuildStatus::Running, 3);
        let widget = BarWidget::new(&bar, name_width, false);
        let area = Rect::new(0, 0, overhead as u16, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert_eq!(content.trim(), "");
    }

    #[test]
    fn actions_title_summary_dot_when_no_jobs() {
        let mut group = make_group("CI", &[]);
        group.summary_status = BuildStatus::Succeeded;
        let groups = vec![&group];
        let widget = ActionsTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 1);
        assert_eq!(dots[0].fg, Color::Green);
    }

    #[test]
    fn actions_title_review_dots_dimmed() {
        let ci_group = make_group("CI", &[BuildStatus::Succeeded]);
        let mut review_group = make_group("Review", &[BuildStatus::Running]);
        review_group.category = WorkflowCategory::Review;
        let groups = vec![&ci_group, &review_group];
        let widget = ActionsTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 2);
        // CI dot: full color (Green for Succeeded)
        assert_eq!(dots[0].fg, Color::Green);
        // Review dot: dimmed (DarkGray regardless of status)
        assert_eq!(dots[1].fg, Color::DarkGray);
    }

    #[test]
    fn actions_title_gone_group_dimmed() {
        let mut group = make_group("Old", &[]);
        group.gone = true;
        group.summary_status = BuildStatus::Succeeded;
        let groups = vec![&group];
        let widget = ActionsTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 1);
        assert_eq!(dots[0].fg, Color::DarkGray);
    }

    #[test]
    fn bar_renders_with_status_dot_prefix() {
        let bar = make_bar("deploy", BuildStatus::Succeeded, 3);
        let widget = BarWidget::new(&bar, 10, false).with_dot(Color::Green);
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        // Dot should appear before the name
        let dot_pos = content.find('\u{25CF}').expect("dot should be present");
        let name_pos = content.find("deploy").expect("name should be present");
        assert!(dot_pos < name_pos, "dot should precede name");

        // Dot color should be green (never dimmed)
        let dot_cell = buf
            .content()
            .iter()
            .find(|c| c.symbol() == "\u{25CF}")
            .unwrap();
        assert_eq!(dot_cell.fg, Color::Green);
    }

    #[test]
    fn bar_renders_without_dot_when_none() {
        let bar = make_bar("deploy", BuildStatus::Succeeded, 3);
        let widget = BarWidget::new(&bar, 10, false);
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let has_dot = buf.content().iter().any(|c| c.symbol() == "\u{25CF}");
        assert!(
            !has_dot,
            "no dot should be rendered when status_dot is None"
        );
    }

    #[test]
    fn actions_title_no_dots_when_expanded() {
        let group = make_group("CI", &[BuildStatus::Succeeded, BuildStatus::Running]);
        let groups = vec![&group];
        let widget = ActionsTitle::new(&groups, true);
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
        assert_eq!(dots.len(), 0, "expanded title should have no dots");
    }

    #[test]
    fn actions_title_dots_when_collapsed() {
        let group = make_group("CI", &[BuildStatus::Succeeded, BuildStatus::Running]);
        let groups = vec![&group];
        let widget = ActionsTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 2, "collapsed title should show dots");
    }

    #[test]
    fn pipelines_title_no_dots_when_expanded() {
        let g1 = make_pipe_group("pipe-a", BuildStatus::Succeeded);
        let g2 = make_pipe_group("pipe-b", BuildStatus::Running);
        let groups = vec![&g1, &g2];
        let widget = PipelinesTitle::new(&groups, true);
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
        assert_eq!(dots.len(), 0, "expanded title should have no dots");
    }

    #[test]
    fn pipelines_title_dots_when_collapsed() {
        let g1 = make_pipe_group("pipe-a", BuildStatus::Succeeded);
        let g2 = make_pipe_group("pipe-b", BuildStatus::Running);
        let groups = vec![&g1, &g2];
        let widget = PipelinesTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 2, "collapsed title should show dots");
    }

    #[test]
    fn pipelines_title_pending_link_dims_dot() {
        let mut g = make_pipe_group("pipe", BuildStatus::Succeeded);
        g.pending_link = true;
        let groups = vec![&g];
        let widget = PipelinesTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 1);
        assert_eq!(
            dots[0].fg,
            Color::DarkGray,
            "pending_link should dim the dot to DarkGray"
        );
    }

    #[test]
    fn pipelines_title_pending_link_cleared_shows_status() {
        let mut g = make_pipe_group("pipe", BuildStatus::Succeeded);
        g.pending_link = false;
        let groups = vec![&g];
        let widget = PipelinesTitle::new(&groups, false);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dots: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.symbol() == "\u{25CF}")
            .collect();
        assert_eq!(dots.len(), 1);
        assert_eq!(
            dots[0].fg,
            Color::Green,
            "without pending_link, dot should show status color"
        );
    }

    #[test]
    fn bar_renders_timestamp_when_finished() {
        use chrono::{TimeZone, Utc};
        let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap();
        let mut bar = make_bar("build", BuildStatus::Succeeded, 5);
        bar.last_finished = Some(ts);
        let widget = BarWidget::new(&bar, 10, false);
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        let after_bracket = content.split(']').last().unwrap_or("");
        assert!(
            after_bracket.contains(':'),
            "expected HH:MM after bar, got: {content}"
        );
    }

    #[test]
    fn bar_timestamp_has_trailing_space() {
        use chrono::{TimeZone, Utc};
        let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap();
        let mut bar = make_bar("build", BuildStatus::Succeeded, 5);
        bar.last_finished = Some(ts);
        let widget = BarWidget::new(&bar, 10, false);
        let area = Rect::new(0, 0, 35, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        // Timestamp should end with a trailing space (right margin)
        assert!(
            content.ends_with(' '),
            "expected trailing space after timestamp, got: '{content}'"
        );
        // Verify there's still a timestamp present
        let after_bracket = content.split(']').last().unwrap_or("");
        assert!(
            after_bracket.contains(':'),
            "expected HH:MM after bar, got: {content}"
        );
    }

    #[test]
    fn bar_no_timestamp_when_none() {
        let bar = make_bar("build", BuildStatus::Running, 3);
        let widget = BarWidget::new(&bar, 10, false);
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let content: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        let after_bracket = content.split(']').last().unwrap_or("");
        assert!(
            !after_bracket.contains(':'),
            "no timestamp expected, got: {content}"
        );
    }

    #[test]
    fn bars_with_and_without_timestamp_have_same_bracket_positions() {
        use chrono::{TimeZone, Utc};

        let ts = Utc.with_ymd_and_hms(2026, 3, 18, 14, 28, 0).unwrap();
        let mut bar_with_ts = make_bar("build", BuildStatus::Succeeded, 5);
        bar_with_ts.last_finished = Some(ts);
        let bar_without_ts = make_bar("build", BuildStatus::Succeeded, 5);

        let area = Rect::new(0, 0, 40, 1);

        let mut buf_with = Buffer::empty(area);
        BarWidget::new(&bar_with_ts, 10, false).render(area, &mut buf_with);
        let bracket_open_with = buf_with
            .content()
            .iter()
            .position(|c| c.symbol() == "[")
            .unwrap();
        let bracket_close_with = buf_with
            .content()
            .iter()
            .position(|c| c.symbol() == "]")
            .unwrap();

        let mut buf_without = Buffer::empty(area);
        BarWidget::new(&bar_without_ts, 10, false).render(area, &mut buf_without);
        let bracket_open_without = buf_without
            .content()
            .iter()
            .position(|c| c.symbol() == "[")
            .unwrap();
        let bracket_close_without = buf_without
            .content()
            .iter()
            .position(|c| c.symbol() == "]")
            .unwrap();

        assert_eq!(
            bracket_open_with, bracket_open_without,
            "opening bracket should be at same position"
        );
        assert_eq!(
            bracket_close_with, bracket_close_without,
            "closing bracket should be at same position (stable bar length)"
        );
    }

    #[test]
    fn bar_dot_never_dimmed() {
        let bar = make_bar("build", BuildStatus::Running, 3);
        // dim=true but dot should retain its color
        let widget = BarWidget::new(&bar, 10, true).with_dot(Color::Yellow);
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let dot_cell = buf
            .content()
            .iter()
            .find(|c| c.symbol() == "\u{25CF}")
            .expect("dot should be present");
        assert_eq!(dot_cell.fg, Color::Yellow, "dot should not be dimmed");
    }
}
