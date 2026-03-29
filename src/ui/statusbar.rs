use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};

use super::theme;
use crate::config::HookStatus;
use crate::poll_scheduler::PollState;

const BOOST_FLASH_DURATION: Duration = Duration::from_millis(750);
const PUSH_SIGNAL_DURATION: Duration = Duration::from_millis(1500);

const NUM_TICKS: u64 = 5;

pub struct StatusBar<'a> {
    pub poll_state: &'a PollState,
    pub elapsed_since_poll: Duration,
    pub cooldown_remaining: Option<Duration>,
    pub warnings: &'a [String],
    pub hook_status: &'a HookStatus,
    pub boost_pressed_at: Option<Instant>,
    pub push_signal_at: Option<Instant>,
    pub linkage_broken: bool,
    pub linkage_discovering: bool,
}

/// Compute how many ticks are filled based on elapsed time and state interval.
/// Uses modular arithmetic so ticks cycle every interval — even during slow
/// API polls that exceed the interval, the bar keeps cycling visually.
fn filled_ticks(elapsed: Duration, state: &PollState) -> usize {
    let interval_ms: u64 = match state {
        PollState::Idle => 30_000,
        PollState::LongIdle => 300_000,
        PollState::Watching | PollState::Active | PollState::Cooldown => 5_000,
    };
    let elapsed_ms = elapsed.as_millis() as u64;
    let cycle_elapsed = elapsed_ms % interval_ms;
    let tick_duration_ms = interval_ms / (NUM_TICKS + 1);
    (cycle_elapsed / tick_duration_ms).min(NUM_TICKS) as usize
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

        let filled = filled_ticks(self.elapsed_since_poll, self.poll_state);
        let remaining = (NUM_TICKS as usize).saturating_sub(filled);
        let filled_str: String = std::iter::repeat_n(theme::TICK_FILLED, filled).collect();
        let empty_str: String = std::iter::repeat_n(theme::TICK_EMPTY, remaining).collect();

        let dim_sep = Span::styled(" \u{2502} ", Style::default().fg(theme::SEPARATOR));

        let mut spans = vec![
            Span::styled(format!("{label} "), Style::default().fg(color)),
            Span::styled(filled_str, Style::default().fg(color)),
            Span::styled(empty_str, Style::default().fg(theme::FG_DIM)),
        ];

        if let Some(cd) = self.cooldown_remaining {
            spans.push(dim_sep.clone());
            spans.push(Span::raw(format!("Cooldown: {}s", cd.as_secs())));
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
                HookStatus::Installed => {
                    spans.push(dim_sep.clone());
                    spans.push(Span::styled(
                        "\u{2713}hook",
                        Style::default().fg(theme::STATUS_SUCCESS),
                    ));
                }
                HookStatus::Missing | HookStatus::Incomplete => {
                    spans.push(dim_sep.clone());
                    spans.push(Span::styled(
                        "h=install pre-push hook",
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    /// Extract content from row 1 (inner area of bordered block).
    fn render_bar(state: &PollState, elapsed: Duration, cooldown: Option<Duration>) -> String {
        render_bar_with_hook(state, elapsed, cooldown, &HookStatus::Installed)
    }

    fn render_bar_with_hook(
        state: &PollState,
        elapsed: Duration,
        cooldown: Option<Duration>,
        hook_status: &HookStatus,
    ) -> String {
        let bar = StatusBar {
            poll_state: state,
            elapsed_since_poll: elapsed,
            cooldown_remaining: cooldown,
            warnings: &[],
            hook_status,
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
    fn idle_shows_slow_polling() {
        let content = render_bar(&PollState::Idle, Duration::ZERO, None);
        // No more "Polling:" label — just "Slow" followed by tick chars
        assert!(content.contains("Slow"), "got: {content}");
        assert!(!content.contains("Polling:"), "got: {content}");
    }

    #[test]
    fn active_shows_fast_polling() {
        let content = render_bar(&PollState::Active, Duration::ZERO, None);
        assert!(content.contains("Fast"), "got: {content}");
        assert!(!content.contains("Polling:"), "got: {content}");
    }

    #[test]
    fn cooldown_shows_cool_with_timer() {
        let content = render_bar(
            &PollState::Cooldown,
            Duration::ZERO,
            Some(Duration::from_secs(42)),
        );
        assert!(content.contains("Cool"), "got: {content}");
        assert!(content.contains("Cooldown: 42s"), "got: {content}");
    }

    #[test]
    fn idle_zero_elapsed_shows_all_empty() {
        let content = render_bar(&PollState::Idle, Duration::ZERO, None);
        let empty5: String = std::iter::repeat(theme::TICK_EMPTY).take(5).collect();
        assert!(content.contains(&empty5), "got: {content}");
    }

    #[test]
    fn idle_near_interval_shows_most_filled() {
        // At 20s, 20000/5000 = 4 ticks filled (tick_duration = 30000/6 = 5000ms)
        let content = render_bar(&PollState::Idle, Duration::from_secs(20), None);
        let filled4: String = std::iter::repeat(theme::TICK_FILLED).take(4).collect();
        let empty1: String = std::iter::repeat(theme::TICK_EMPTY).take(1).collect();
        let expected = format!("{filled4}{empty1}");
        assert!(content.contains(&expected), "got: {content}");
    }

    #[test]
    fn idle_at_interval_wraps_to_empty() {
        // At exactly the interval boundary, modular wraps → all empty (new cycle)
        let content = render_bar(&PollState::Idle, Duration::from_secs(30), None);
        let empty5: String = std::iter::repeat(theme::TICK_EMPTY).take(5).collect();
        assert!(content.contains(&empty5), "got: {content}");
    }

    #[test]
    fn active_partial_elapsed() {
        // 1s per tick, 2s elapsed → 2 filled
        let content = render_bar(&PollState::Active, Duration::from_secs(2), None);
        let filled2: String = std::iter::repeat(theme::TICK_FILLED).take(2).collect();
        let empty3: String = std::iter::repeat(theme::TICK_EMPTY).take(3).collect();
        let expected = format!("{filled2}{empty3}");
        assert!(content.contains(&expected), "got: {content}");
    }

    #[test]
    fn idle_partial_elapsed() {
        // 6s per tick, 12s elapsed → 2 filled
        let content = render_bar(&PollState::Idle, Duration::from_secs(12), None);
        let filled2: String = std::iter::repeat(theme::TICK_FILLED).take(2).collect();
        let empty3: String = std::iter::repeat(theme::TICK_EMPTY).take(3).collect();
        let expected = format!("{filled2}{empty3}");
        assert!(content.contains(&expected), "got: {content}");
    }

    #[test]
    fn shows_boost_not_refresh() {
        let content = render_bar(&PollState::Idle, Duration::ZERO, None);
        assert!(content.contains("b=boost"), "got: {content}");
        assert!(!content.contains("r=boost"), "got: {content}");
    }

    #[test]
    fn watching_shows_scan_gh() {
        let content = render_bar(&PollState::Watching, Duration::ZERO, None);
        assert!(content.contains("Scan:GH"), "got: {content}");
    }

    #[test]
    fn long_idle_shows_sleep() {
        let content = render_bar(&PollState::LongIdle, Duration::ZERO, None);
        assert!(content.contains("Sleep"), "got: {content}");
    }

    #[test]
    fn long_idle_partial_elapsed() {
        // 60s per tick, 120s elapsed → 2 filled
        let content = render_bar(&PollState::LongIdle, Duration::from_secs(120), None);
        let filled2: String = std::iter::repeat(theme::TICK_FILLED).take(2).collect();
        let empty3: String = std::iter::repeat(theme::TICK_EMPTY).take(3).collect();
        let expected = format!("{filled2}{empty3}");
        assert!(content.contains(&expected), "got: {content}");
    }

    #[test]
    fn filled_ticks_wraps_after_interval() {
        // 5500ms Active: 5500 % 5000 = 500ms → 500/833 = 0 ticks (new cycle)
        assert_eq!(
            filled_ticks(Duration::from_millis(5500), &PollState::Active),
            0
        );
        // 35s Idle: 35000 % 30000 = 5000ms → 5000/5000 = 1 tick
        assert_eq!(filled_ticks(Duration::from_secs(35), &PollState::Idle), 1);
        // 999s Active: 999000 % 5000 = 4000ms → 4000/833 = 4 ticks
        assert_eq!(
            filled_ticks(Duration::from_secs(999), &PollState::Active),
            4
        );
    }

    #[test]
    fn shows_hook_hint_when_missing() {
        let content =
            render_bar_with_hook(&PollState::Idle, Duration::ZERO, None, &HookStatus::Missing);
        assert!(content.contains("h=install"), "got: {content}");
    }

    #[test]
    fn shows_hook_hint_when_incomplete() {
        let content = render_bar_with_hook(
            &PollState::Idle,
            Duration::ZERO,
            None,
            &HookStatus::Incomplete,
        );
        assert!(content.contains("h=install"), "got: {content}");
    }

    #[test]
    fn no_hook_hint_when_installed() {
        let content = render_bar_with_hook(
            &PollState::Idle,
            Duration::ZERO,
            None,
            &HookStatus::Installed,
        );
        assert!(!content.contains("h=install"), "got: {content}");
    }

    #[test]
    fn installed_hook_shows_checkmark() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
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

    #[test]
    fn filled_ticks_cycle_boundary() {
        // Zero elapsed → 0 ticks in all states
        assert_eq!(filled_ticks(Duration::ZERO, &PollState::Active), 0);
        assert_eq!(filled_ticks(Duration::ZERO, &PollState::Idle), 0);
        assert_eq!(filled_ticks(Duration::ZERO, &PollState::LongIdle), 0);

        // At exact interval boundary, modular wraps to 0 (new cycle)
        assert_eq!(filled_ticks(Duration::from_secs(5), &PollState::Active), 0);
        assert_eq!(filled_ticks(Duration::from_secs(30), &PollState::Idle), 0);
        assert_eq!(
            filled_ticks(Duration::from_secs(300), &PollState::LongIdle),
            0
        );
    }

    #[test]
    fn filled_ticks_full_before_interval() {
        assert_eq!(
            filled_ticks(Duration::from_millis(4500), &PollState::Active),
            NUM_TICKS as usize
        );
        assert_eq!(
            filled_ticks(Duration::from_secs(26), &PollState::Idle),
            NUM_TICKS as usize
        );
        assert_eq!(
            filled_ticks(Duration::from_secs(260), &PollState::LongIdle),
            NUM_TICKS as usize
        );
    }

    #[test]
    fn separator_uses_box_drawing_char() {
        let content = render_bar(&PollState::Idle, Duration::ZERO, None);
        assert!(
            content.contains('\u{2502}'),
            "expected │ (U+2502) in: {content}"
        );
    }

    fn render_bar_with_warnings(
        state: &PollState,
        elapsed: Duration,
        cooldown: Option<Duration>,
        hook_status: &HookStatus,
        warnings: &[String],
    ) -> String {
        let bar = StatusBar {
            poll_state: state,
            elapsed_since_poll: elapsed,
            cooldown_remaining: cooldown,
            warnings,
            hook_status,
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
            Duration::ZERO,
            None,
            &HookStatus::Installed,
            &warnings,
        );
        assert!(content.contains("AWS: timeout"), "got: {content}");
    }

    #[test]
    fn renders_hook_hint_and_warning_together() {
        let warnings = vec!["AWS: timeout".to_string()];
        let content = render_bar_with_warnings(
            &PollState::Idle,
            Duration::ZERO,
            None,
            &HookStatus::Missing,
            &warnings,
        );
        assert!(content.contains("h=install"), "got: {content}");
        assert!(content.contains("AWS: timeout"), "got: {content}");
    }

    #[test]
    fn statusbar_renders_in_rounded_block() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);

        // Row 0: top border with rounded corner ╭
        let top_left = buf.cell((0, 0)).unwrap().symbol().to_string();
        assert_eq!(top_left, "╭", "expected rounded top-left corner");

        // Row 2: bottom border with rounded corner ╰
        let bot_left = buf.cell((0, 2)).unwrap().symbol().to_string();
        assert_eq!(bot_left, "╰", "expected rounded bottom-left corner");
    }

    #[test]
    fn boost_flash_active() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
            boost_pressed_at: Some(Instant::now()),
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);

        // Find "b" of "b=boost" by scanning cells in row 1
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
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
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
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
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

    fn render_bar_with_linkage(linkage_broken: bool, linkage_discovering: bool) -> String {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
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

    fn render_buf(state: &PollState, elapsed: Duration, cooldown: Option<Duration>) -> Buffer {
        let bar = StatusBar {
            poll_state: state,
            elapsed_since_poll: elapsed,
            cooldown_remaining: cooldown,
            warnings: &[],
            hook_status: &HookStatus::Installed,
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

    /// Find first cell in row 1 matching a char and return its fg color.
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
            let buf = render_buf(&state, Duration::ZERO, None);
            let color = label_color(&buf, first_char);
            assert_eq!(
                color, expected_color,
                "state {state:?}: expected {expected_color:?}, got {color:?}"
            );
        }
    }

    #[test]
    fn filled_ticks_use_state_color() {
        // Active with 2s elapsed → 2 filled ticks, should be POLL_FAST colored
        let buf = render_buf(&PollState::Active, Duration::from_secs(2), None);
        // Find first tick char
        let tick_col = (1u16..119)
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == "\u{25AE}")
            .expect("filled tick not found");
        let tick_color = buf.cell((tick_col, 1)).unwrap().fg;
        assert_eq!(tick_color, theme::POLL_FAST);
    }

    #[test]
    fn empty_ticks_use_dim_color() {
        // Active with 0 elapsed → all empty ticks
        let buf = render_buf(&PollState::Active, Duration::ZERO, None);
        let tick_col = (1u16..119)
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == "\u{25AF}")
            .expect("empty tick not found");
        let tick_color = buf.cell((tick_col, 1)).unwrap().fg;
        assert_eq!(tick_color, theme::FG_DIM);
    }

    #[test]
    fn statusbar_block_border_color() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);

        // Border cells should use theme::BORDER_STATUS color
        let border_cell = buf.cell((0, 0)).unwrap();
        assert_eq!(border_cell.fg, theme::BORDER_STATUS);
    }

    #[test]
    fn push_signal_shows_pushed_label() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
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
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
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
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
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
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
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
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::NoGitDir,
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
        assert!(!content.contains("h=install"), "got: {content}");
        assert!(!content.contains("pushed!"), "got: {content}");
    }
}
