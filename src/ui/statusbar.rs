use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};

use super::theme;
use crate::app::App;
use crate::config::HookStatus;
use crate::poll_scheduler::PollState;

const BOOST_FLASH_DURATION: Duration = Duration::from_millis(750);
const PUSH_SIGNAL_DURATION: Duration = Duration::from_millis(1500);

pub struct StatusBar<'a> {
    pub poll_state: &'a PollState,
    pub tick: usize,
    pub cooldown_remaining: Option<Duration>,
    pub idle_remaining: Option<Duration>,
    pub watching_remaining: Option<Duration>,
    pub active_elapsed: Option<Duration>,
    pub warnings: &'a [String],
    pub hook_status: &'a HookStatus,
    pub has_global_hooks_path: bool,
    pub boost_pressed_at: Option<Instant>,
    pub push_signal_at: Option<Instant>,
    pub linkage_broken: bool,
    pub linkage_discovering: bool,
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::BORDER_STATUS));
        let inner = block.inner(area);
        block.render(area, buf);

        let (label, color) = match self.poll_state {
            PollState::Idle => ("Slow", theme::POLL_SLOW),
            PollState::LongIdle => ("Sleep", theme::POLL_SLEEP),
            PollState::Watching => ("Scan:GH", theme::POLL_SCAN),
            PollState::Active => ("Fast", theme::POLL_FAST),
            PollState::Cooldown => ("Cool", theme::POLL_COOL),
        };

        let filled = self.tick.min(App::NUM_TICKS);
        let remaining = App::NUM_TICKS.saturating_sub(filled);
        let filled_str: String = std::iter::repeat_n(theme::TICK_FILLED, filled).collect();
        let empty_str: String = std::iter::repeat_n(theme::TICK_EMPTY, remaining).collect();

        let dim_sep = Span::styled(" \u{2502} ", Style::default().fg(theme::SEPARATOR));

        let mut spans = vec![
            Span::styled(format!("{label} "), Style::default().fg(color)),
            Span::styled(filled_str, Style::default().fg(color)),
            Span::styled(empty_str, Style::default().fg(theme::FG_DIM)),
        ];

        let state_duration = self
            .idle_remaining
            .or(self.watching_remaining)
            .or(self.cooldown_remaining)
            .or(self.active_elapsed);
        if let Some(d) = state_duration {
            spans.push(dim_sep.clone());
            spans.push(Span::raw(format_duration(d)));
        }

        let boost_active = self
            .boost_pressed_at
            .is_some_and(|t| t.elapsed() < BOOST_FLASH_DURATION);

        spans.push(dim_sep.clone());
        spans.push(Span::raw("e=expand "));
        if boost_active {
            spans.push(Span::styled(
                "b=boost",
                Style::default().fg(theme::BOOST_FLASH),
            ));
        } else {
            spans.push(Span::raw("b=boost"));
        }
        spans.push(Span::raw(" q=quit"));

        let push_active = self
            .push_signal_at
            .is_some_and(|t| t.elapsed() < PUSH_SIGNAL_DURATION);

        if push_active {
            spans.push(dim_sep.clone());
            spans.push(Span::styled(
                "\u{2B06} pushed!",
                Style::default().fg(theme::POLL_SCAN),
            ));
        } else {
            match self.hook_status {
                HookStatus::Installed(loc) => {
                    spans.push(dim_sep.clone());
                    let label = match loc {
                        crate::config::HookLocation::Local => "\u{2713}hook",
                        crate::config::HookLocation::Global => "\u{2713}g-hook",
                    };
                    spans.push(Span::styled(
                        label,
                        Style::default().fg(theme::STATUS_SUCCESS),
                    ));
                }
                HookStatus::Shadowed => {
                    spans.push(dim_sep.clone());
                    spans.push(Span::styled(
                        "\u{26A0}hook:override g=fix",
                        Style::default().fg(theme::STATUS_RUNNING),
                    ));
                }
                HookStatus::Missing | HookStatus::Incomplete => {
                    spans.push(dim_sep.clone());
                    let hint = if self.has_global_hooks_path {
                        "p=local g=global hook"
                    } else {
                        "p=install hook"
                    };
                    spans.push(Span::styled(
                        hint,
                        Style::default().fg(theme::STATUS_RUNNING),
                    ));
                }
                HookStatus::NoGitDir => {}
            }
        }

        // Linkage status
        if self.linkage_discovering {
            spans.push(dim_sep.clone());
            spans.push(Span::styled(
                "\u{27F3} relinking...",
                Style::default().fg(theme::POLL_SCAN),
            ));
        } else if self.linkage_broken {
            spans.push(dim_sep.clone());
            spans.push(Span::styled(
                "l=relink \u{26A0}",
                Style::default().fg(theme::STATUS_RUNNING),
            ));
        } else {
            spans.push(dim_sep.clone());
            spans.push(Span::styled("l=relink", Style::default().fg(theme::FG_DIM)));
        }

        if !self.warnings.is_empty() {
            spans.push(dim_sep);
            spans.push(Span::styled(
                self.warnings.join("; "),
                Style::default().fg(theme::STATUS_RUNNING),
            ));
        }

        Line::from(spans).render(inner, buf);
    }
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HookLocation;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    /// Extract content from row 1 (inner area of bordered block).
    fn render_bar(state: &PollState, tick: usize, cooldown: Option<Duration>) -> String {
        render_bar_with_hook(
            state,
            tick,
            cooldown,
            &HookStatus::Installed(HookLocation::Local),
            false,
        )
    }

    fn render_bar_with_hook(
        state: &PollState,
        tick: usize,
        cooldown: Option<Duration>,
        hook_status: &HookStatus,
        has_global_hooks_path: bool,
    ) -> String {
        let bar = StatusBar {
            poll_state: state,
            tick,
            cooldown_remaining: cooldown,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status,
            has_global_hooks_path,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect()
    }

    // --- tick rendering tests ---

    #[test]
    fn tick_zero_shows_all_empty() {
        let content = render_bar(&PollState::Idle, 0, None);
        let empty5: String = std::iter::repeat(theme::TICK_EMPTY).take(5).collect();
        assert!(content.contains(&empty5), "got: {content}");
    }

    #[test]
    fn tick_two_shows_two_filled() {
        let content = render_bar(&PollState::Active, 2, None);
        let filled2: String = std::iter::repeat(theme::TICK_FILLED).take(2).collect();
        let empty3: String = std::iter::repeat(theme::TICK_EMPTY).take(3).collect();
        let expected = format!("{filled2}{empty3}");
        assert!(content.contains(&expected), "got: {content}");
    }

    #[test]
    fn tick_four_shows_four_filled() {
        let content = render_bar(&PollState::Idle, 4, None);
        let filled4: String = std::iter::repeat(theme::TICK_FILLED).take(4).collect();
        let empty1: String = std::iter::repeat(theme::TICK_EMPTY).take(1).collect();
        let expected = format!("{filled4}{empty1}");
        assert!(content.contains(&expected), "got: {content}");
    }

    #[test]
    fn tick_five_shows_all_filled() {
        let content = render_bar(&PollState::Active, 5, None);
        let filled5: String = std::iter::repeat(theme::TICK_FILLED).take(5).collect();
        assert!(content.contains(&filled5), "got: {content}");
    }

    #[test]
    fn tick_two_shows_two_filled_idle() {
        let content = render_bar(&PollState::Idle, 2, None);
        let filled2: String = std::iter::repeat(theme::TICK_FILLED).take(2).collect();
        let empty3: String = std::iter::repeat(theme::TICK_EMPTY).take(3).collect();
        let expected = format!("{filled2}{empty3}");
        assert!(content.contains(&expected), "got: {content}");
    }

    #[test]
    fn tick_two_shows_two_filled_long_idle() {
        let content = render_bar(&PollState::LongIdle, 2, None);
        let filled2: String = std::iter::repeat(theme::TICK_FILLED).take(2).collect();
        let empty3: String = std::iter::repeat(theme::TICK_EMPTY).take(3).collect();
        let expected = format!("{filled2}{empty3}");
        assert!(content.contains(&expected), "got: {content}");
    }

    // --- state label tests ---

    #[test]
    fn idle_shows_slow_polling() {
        let content = render_bar(&PollState::Idle, 0, None);
        assert!(content.contains("Slow"), "got: {content}");
        assert!(!content.contains("Polling:"), "got: {content}");
    }

    #[test]
    fn active_shows_fast_polling() {
        let content = render_bar(&PollState::Active, 0, None);
        assert!(content.contains("Fast"), "got: {content}");
        assert!(!content.contains("Polling:"), "got: {content}");
    }

    #[test]
    fn cooldown_shows_cool_with_timer() {
        let content = render_bar(&PollState::Cooldown, 0, Some(Duration::from_secs(42)));
        assert!(content.contains("Cool"), "got: {content}");
        assert!(content.contains("42s"), "got: {content}");
    }

    #[test]
    fn watching_shows_scan_gh() {
        let content = render_bar(&PollState::Watching, 0, None);
        assert!(content.contains("Scan:GH"), "got: {content}");
    }

    #[test]
    fn long_idle_shows_sleep() {
        let content = render_bar(&PollState::LongIdle, 0, None);
        assert!(content.contains("Sleep"), "got: {content}");
    }

    // --- state timer display tests ---

    fn render_bar_with_timers(
        state: &PollState,
        idle_remaining: Option<Duration>,
        watching_remaining: Option<Duration>,
        active_elapsed: Option<Duration>,
        cooldown_remaining: Option<Duration>,
    ) -> String {
        let bar = StatusBar {
            poll_state: state,
            tick: 0,
            cooldown_remaining,
            idle_remaining,
            watching_remaining,
            active_elapsed,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect()
    }

    #[test]
    fn idle_shows_remaining_time() {
        let content = render_bar_with_timers(
            &PollState::Idle,
            Some(Duration::from_secs(192)),
            None,
            None,
            None,
        );
        assert!(content.contains("Slow"), "got: {content}");
        assert!(content.contains("3m12s"), "got: {content}");
    }

    #[test]
    fn watching_shows_remaining_time() {
        let content = render_bar_with_timers(
            &PollState::Watching,
            None,
            Some(Duration::from_secs(42)),
            None,
            None,
        );
        assert!(content.contains("Scan:GH"), "got: {content}");
        assert!(content.contains("42s"), "got: {content}");
    }

    #[test]
    fn active_shows_elapsed_time() {
        let content = render_bar_with_timers(
            &PollState::Active,
            None,
            None,
            Some(Duration::from_secs(65)),
            None,
        );
        assert!(content.contains("Fast"), "got: {content}");
        assert!(content.contains("1m05s"), "got: {content}");
    }

    #[test]
    fn long_idle_shows_no_timer() {
        let content = render_bar_with_timers(&PollState::LongIdle, None, None, None, None);
        assert!(content.contains("Sleep"), "got: {content}");
        // No duration string should appear between ticks and the separator
        assert!(!content.contains("0s"), "got: {content}");
    }

    #[test]
    fn shows_boost_not_refresh() {
        let content = render_bar(&PollState::Idle, 0, None);
        assert!(content.contains("b=boost"), "got: {content}");
        assert!(!content.contains("r=boost"), "got: {content}");
    }

    #[test]
    fn separator_uses_box_drawing_char() {
        let content = render_bar(&PollState::Idle, 0, None);
        assert!(
            content.contains('\u{2502}'),
            "expected │ (U+2502) in: {content}"
        );
    }

    // --- hook tests ---

    #[test]
    fn shows_local_hint_when_missing_no_global() {
        let content = render_bar_with_hook(&PollState::Idle, 0, None, &HookStatus::Missing, false);
        assert!(content.contains("p=install hook"), "got: {content}");
        assert!(!content.contains("g="), "got: {content}");
    }

    #[test]
    fn shows_both_hints_when_missing_with_global() {
        let content = render_bar_with_hook(&PollState::Idle, 0, None, &HookStatus::Missing, true);
        assert!(content.contains("p=local"), "got: {content}");
        assert!(content.contains("g=global"), "got: {content}");
    }

    #[test]
    fn shows_local_hint_when_incomplete_no_global() {
        let content =
            render_bar_with_hook(&PollState::Idle, 0, None, &HookStatus::Incomplete, false);
        assert!(content.contains("p=install hook"), "got: {content}");
    }

    #[test]
    fn no_hook_hint_when_installed() {
        let content = render_bar_with_hook(
            &PollState::Idle,
            0,
            None,
            &HookStatus::Installed(HookLocation::Local),
            false,
        );
        assert!(!content.contains("p=install"), "got: {content}");
        assert!(!content.contains("g="), "got: {content}");
    }

    #[test]
    fn installed_hook_shows_checkmark() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let content: String = (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect();
        assert!(content.contains("✓hook"), "got: {content}");
    }

    // --- boost flash tests ---

    #[test]
    fn boost_flash_active() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: Some(Instant::now()),
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);

        let b_col = (0u16..120)
            .find(|&x| {
                buf.cell((x, 1)).unwrap().symbol() == "b"
                    && buf.cell((x + 1, 1)).unwrap().symbol() == "="
            })
            .expect("b=boost not found");
        for offset in 0..7 {
            let cell = buf.cell((b_col + offset, 1)).unwrap();
            assert_eq!(
                cell.fg,
                theme::BOOST_FLASH,
                "cell at offset {offset} ('{}') should be BOOST_FLASH",
                cell.symbol()
            );
        }
    }

    #[test]
    fn boost_flash_expired() {
        let expired = Instant::now() - Duration::from_secs(2);
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: Some(expired),
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);

        let b_col = (0u16..120)
            .find(|&x| {
                buf.cell((x, 1)).unwrap().symbol() == "b"
                    && buf.cell((x + 1, 1)).unwrap().symbol() == "="
            })
            .expect("b=boost not found");
        let cell = buf.cell((b_col, 1)).unwrap();
        assert_ne!(
            cell.fg,
            theme::BOOST_FLASH,
            "expired flash should not use BOOST_FLASH"
        );
    }

    #[test]
    fn boost_flash_none() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);

        let b_col = (0u16..120)
            .find(|&x| {
                buf.cell((x, 1)).unwrap().symbol() == "b"
                    && buf.cell((x + 1, 1)).unwrap().symbol() == "="
            })
            .expect("b=boost not found");
        let cell = buf.cell((b_col, 1)).unwrap();
        assert_ne!(
            cell.fg,
            theme::BOOST_FLASH,
            "no flash should not use BOOST_FLASH"
        );
    }

    // --- linkage tests ---

    fn render_bar_with_linkage(linkage_broken: bool, linkage_discovering: bool) -> String {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken,
            linkage_discovering,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect()
    }

    #[test]
    fn shows_relink_hint_normally() {
        let content = render_bar_with_linkage(false, false);
        assert!(content.contains("l=relink"), "got: {content}");
        assert!(!content.contains("\u{26A0}"), "got: {content}");
    }

    #[test]
    fn shows_relink_warning_when_broken() {
        let content = render_bar_with_linkage(true, false);
        assert!(content.contains("l=relink"), "got: {content}");
        assert!(content.contains("\u{26A0}"), "got: {content}");
    }

    #[test]
    fn shows_relinking_spinner_when_discovering() {
        let content = render_bar_with_linkage(false, true);
        assert!(content.contains("relinking"), "got: {content}");
        assert!(!content.contains("l=relink"), "got: {content}");
    }

    // --- color assertion helpers ---

    fn render_buf(state: &PollState, tick: usize, cooldown: Option<Duration>) -> Buffer {
        let bar = StatusBar {
            poll_state: state,
            tick,
            cooldown_remaining: cooldown,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        buf
    }

    fn label_color(buf: &Buffer, ch: &str) -> ratatui::style::Color {
        let col = (1u16..119)
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == ch)
            .unwrap_or_else(|| panic!("'{ch}' not found in row 1"));
        buf.cell((col, 1)).unwrap().fg
    }

    #[test]
    fn poll_state_label_colors() {
        let cases: Vec<(PollState, &str, ratatui::style::Color)> = vec![
            (PollState::Idle, "S", theme::POLL_SLOW),
            (PollState::LongIdle, "S", theme::POLL_SLEEP),
            (PollState::Watching, "S", theme::POLL_SCAN),
            (PollState::Active, "F", theme::POLL_FAST),
            (PollState::Cooldown, "C", theme::POLL_COOL),
        ];
        for (state, first_char, expected_color) in cases {
            let buf = render_buf(&state, 0, None);
            let color = label_color(&buf, first_char);
            assert_eq!(
                color, expected_color,
                "state {state:?}: expected {expected_color:?}, got {color:?}"
            );
        }
    }

    #[test]
    fn tick_uses_state_color() {
        let buf = render_buf(&PollState::Active, 2, None);
        let tick_col = (1u16..119)
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == "\u{25AE}")
            .expect("filled tick not found");
        let tick_color = buf.cell((tick_col, 1)).unwrap().fg;
        assert_eq!(tick_color, theme::POLL_FAST);
    }

    #[test]
    fn empty_ticks_use_dim_color() {
        let buf = render_buf(&PollState::Active, 0, None);
        let tick_col = (1u16..119)
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == "\u{25AF}")
            .expect("empty tick not found");
        let tick_color = buf.cell((tick_col, 1)).unwrap().fg;
        assert_eq!(tick_color, theme::FG_DIM);
    }

    // --- border / block tests ---

    #[test]
    fn statusbar_renders_in_rounded_block() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);

        let top_left = buf.cell((0, 0)).unwrap().symbol().to_string();
        assert_eq!(top_left, "╭", "expected rounded top-left corner");

        let bot_left = buf.cell((0, 2)).unwrap().symbol().to_string();
        assert_eq!(bot_left, "╰", "expected rounded bottom-left corner");
    }

    #[test]
    fn statusbar_block_border_color() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);

        let border_cell = buf.cell((0, 0)).unwrap();
        assert_eq!(border_cell.fg, theme::BORDER_STATUS);
    }

    // --- warnings tests ---

    fn render_bar_with_warnings(
        state: &PollState,
        tick: usize,
        cooldown: Option<Duration>,
        hook_status: &HookStatus,
        warnings: &[String],
    ) -> String {
        let bar = StatusBar {
            poll_state: state,
            tick,
            cooldown_remaining: cooldown,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings,
            hook_status,
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect()
    }

    #[test]
    fn renders_warnings() {
        let warnings = vec!["AWS: timeout".to_string()];
        let content = render_bar_with_warnings(
            &PollState::Idle,
            0,
            None,
            &HookStatus::Installed(HookLocation::Local),
            &warnings,
        );
        assert!(content.contains("AWS: timeout"), "got: {content}");
    }

    #[test]
    fn renders_hook_hint_and_warning_together() {
        let warnings = vec!["AWS: timeout".to_string()];
        let content =
            render_bar_with_warnings(&PollState::Idle, 0, None, &HookStatus::Missing, &warnings);
        assert!(content.contains("p=install hook"), "got: {content}");
        assert!(content.contains("AWS: timeout"), "got: {content}");
    }

    // --- push signal tests ---

    #[test]
    fn push_signal_shows_pushed_label() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: Some(Instant::now()),
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let content: String = (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect();
        assert!(content.contains("pushed!"), "got: {content}");
        assert!(!content.contains("✓hook"), "got: {content}");
    }

    #[test]
    fn expired_push_signal_shows_checkmark() {
        let expired = Instant::now() - Duration::from_secs(5);
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: Some(expired),
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let content: String = (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect();
        assert!(content.contains("✓hook"), "got: {content}");
        assert!(!content.contains("pushed!"), "got: {content}");
    }

    #[test]
    fn push_signal_uses_scan_color() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: Some(Instant::now()),
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let col = (0u16..120)
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == "\u{2B06}")
            .expect("⬆ not found");
        assert_eq!(buf.cell((col, 1)).unwrap().fg, theme::POLL_SCAN);
    }

    #[test]
    fn installed_hook_uses_success_color() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let col = (0u16..120)
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == "\u{2713}")
            .expect("✓ not found");
        assert_eq!(buf.cell((col, 1)).unwrap().fg, theme::STATUS_SUCCESS);
    }

    #[test]
    fn no_git_dir_omits_hook_indicator() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            tick: 0,
            cooldown_remaining: None,
            idle_remaining: None,
            watching_remaining: None,
            active_elapsed: None,
            warnings: &[],
            hook_status: &HookStatus::NoGitDir,
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let content: String = (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect();
        assert!(!content.contains("✓hook"), "got: {content}");
        assert!(!content.contains("p=install"), "got: {content}");
        assert!(!content.contains("pushed!"), "got: {content}");
    }

    // --- format_duration tests ---

    #[test]
    fn format_duration_seconds_only() {
        assert_eq!(format_duration(Duration::from_secs(42)), "42s");
    }

    #[test]
    fn format_duration_minutes_and_seconds() {
        assert_eq!(format_duration(Duration::from_secs(192)), "3m12s");
    }

    #[test]
    fn format_duration_zero() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
    }

    #[test]
    fn format_duration_exact_minute() {
        assert_eq!(format_duration(Duration::from_secs(60)), "1m00s");
    }

    #[test]
    fn format_duration_five_minutes() {
        assert_eq!(format_duration(Duration::from_secs(300)), "5m00s");
    }
}
