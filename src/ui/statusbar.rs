use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};

use super::theme;
use crate::config::HookStatus;
use crate::poll_scheduler::{PollState, PollingPhase, COOLDOWN_DURATION};

const BOOST_FLASH_DURATION: Duration = Duration::from_millis(750);
const PUSH_SIGNAL_DURATION: Duration = Duration::from_millis(1500);
const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPINNER_FRAME_MS: u128 = 250;

pub struct StatusBar<'a> {
    pub poll_state: &'a PollState,
    pub warnings: &'a [String],
    pub hook_status: &'a HookStatus,
    pub has_global_hooks_path: bool,
    pub boost_pressed_at: Option<Instant>,
    pub push_signal_at: Option<Instant>,
    pub linkage_broken: bool,
    pub linkage_discovering: bool,
}

fn spinner_frame() -> char {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    SPINNER_FRAMES[((ms / SPINNER_FRAME_MS) % SPINNER_FRAMES.len() as u128) as usize]
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::BORDER_STATUS));
        let inner = block.inner(area);
        block.render(area, buf);

        let label_span = match self.poll_state {
            PollState::Sleep => Span::styled("☾ Sleep", Style::default().fg(theme::POLL_SLEEP)),
            PollState::Polling {
                phase: PollingPhase::Cooldown,
                since,
            } => {
                let remaining = COOLDOWN_DURATION.saturating_sub(since.elapsed()).as_secs();
                Span::styled(
                    format!("Cooldown {remaining}s"),
                    Style::default().fg(theme::POLL_COOL),
                )
            }
            PollState::Polling { .. } => Span::styled(
                format!("{} Polling", spinner_frame()),
                Style::default().fg(theme::POLL_FAST),
            ),
        };

        let dim_sep = Span::styled(" \u{2502} ", Style::default().fg(theme::SEPARATOR));

        let mut spans = vec![label_span];

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
    use crate::config::HookLocation;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn polling(phase: PollingPhase, age_secs: u64) -> PollState {
        let since = Instant::now()
            .checked_sub(Duration::from_secs(age_secs))
            .unwrap_or_else(Instant::now);
        PollState::Polling { phase, since }
    }

    fn render_bar(state: &PollState) -> String {
        render_bar_with_hook(state, &HookStatus::Installed(HookLocation::Local), false)
    }

    fn render_bar_with_hook(
        state: &PollState,
        hook_status: &HookStatus,
        has_global_hooks_path: bool,
    ) -> String {
        let bar = StatusBar {
            poll_state: state,
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

    // --- new state-label tests ---

    #[test]
    fn sleep_renders_moon_label() {
        let content = render_bar(&PollState::Sleep);
        assert!(content.contains("☾ Sleep"), "got: {content}");
    }

    #[test]
    fn polling_grace_renders_spinner() {
        let content = render_bar(&polling(PollingPhase::Grace, 0));
        assert!(content.contains("Polling"), "got: {content}");
        let has_spinner = SPINNER_FRAMES.iter().any(|c| content.contains(*c));
        assert!(has_spinner, "expected a spinner frame in: {content}");
    }

    #[test]
    fn polling_active_renders_spinner() {
        let content = render_bar(&polling(PollingPhase::Active, 5));
        assert!(content.contains("Polling"), "got: {content}");
        let has_spinner = SPINNER_FRAMES.iter().any(|c| content.contains(*c));
        assert!(has_spinner, "expected a spinner frame in: {content}");
    }

    #[test]
    fn cooldown_renders_countdown_seconds() {
        let content = render_bar(&polling(PollingPhase::Cooldown, 18));
        assert!(content.contains("Cooldown"), "got: {content}");
        // 60s - 18s elapsed ≈ 42s remaining (allow ±1s for test timing)
        assert!(
            content.contains("42s") || content.contains("41s"),
            "got: {content}"
        );
    }

    #[test]
    fn sleep_label_uses_dark_goldenrod() {
        let bar = StatusBar {
            poll_state: &PollState::Sleep,
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
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == "☾")
            .expect("☾ moon not found");
        assert_eq!(buf.cell((col, 1)).unwrap().fg, theme::POLL_SLEEP);
    }

    // --- existing behavior preserved ---

    #[test]
    fn shows_boost_not_refresh() {
        let content = render_bar(&PollState::Sleep);
        assert!(content.contains("b=boost"), "got: {content}");
        assert!(!content.contains("r=boost"), "got: {content}");
    }

    #[test]
    fn separator_uses_box_drawing_char() {
        let content = render_bar(&PollState::Sleep);
        assert!(
            content.contains('\u{2502}'),
            "expected │ (U+2502) in: {content}"
        );
    }

    // --- hook tests ---

    #[test]
    fn shows_local_hint_when_missing_no_global() {
        let content = render_bar_with_hook(&PollState::Sleep, &HookStatus::Missing, false);
        assert!(content.contains("p=install hook"), "got: {content}");
        assert!(!content.contains("g="), "got: {content}");
    }

    #[test]
    fn shows_both_hints_when_missing_with_global() {
        let content = render_bar_with_hook(&PollState::Sleep, &HookStatus::Missing, true);
        assert!(content.contains("p=local"), "got: {content}");
        assert!(content.contains("g=global"), "got: {content}");
    }

    #[test]
    fn shows_local_hint_when_incomplete_no_global() {
        let content = render_bar_with_hook(&PollState::Sleep, &HookStatus::Incomplete, false);
        assert!(content.contains("p=install hook"), "got: {content}");
    }

    #[test]
    fn no_hook_hint_when_installed() {
        let content = render_bar_with_hook(
            &PollState::Sleep,
            &HookStatus::Installed(HookLocation::Local),
            false,
        );
        assert!(!content.contains("p=install"), "got: {content}");
        assert!(!content.contains("g="), "got: {content}");
    }

    #[test]
    fn installed_hook_shows_checkmark() {
        let content = render_bar(&PollState::Sleep);
        assert!(content.contains("✓hook"), "got: {content}");
    }

    // --- boost flash tests ---

    fn render_with_boost(boost_pressed_at: Option<Instant>) -> Buffer {
        let bar = StatusBar {
            poll_state: &PollState::Sleep,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        buf
    }

    fn find_b_boost_col(buf: &Buffer) -> u16 {
        (0u16..120)
            .find(|&x| {
                buf.cell((x, 1)).unwrap().symbol() == "b"
                    && buf.cell((x + 1, 1)).unwrap().symbol() == "="
            })
            .expect("b=boost not found")
    }

    #[test]
    fn boost_flash_active() {
        let buf = render_with_boost(Some(Instant::now()));
        let b_col = find_b_boost_col(&buf);
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
        let buf = render_with_boost(Some(expired));
        let b_col = find_b_boost_col(&buf);
        let cell = buf.cell((b_col, 1)).unwrap();
        assert_ne!(cell.fg, theme::BOOST_FLASH);
    }

    #[test]
    fn boost_flash_none() {
        let buf = render_with_boost(None);
        let b_col = find_b_boost_col(&buf);
        let cell = buf.cell((b_col, 1)).unwrap();
        assert_ne!(cell.fg, theme::BOOST_FLASH);
    }

    // --- linkage tests ---

    fn render_bar_with_linkage(linkage_broken: bool, linkage_discovering: bool) -> String {
        let bar = StatusBar {
            poll_state: &PollState::Sleep,
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

    // --- border tests ---

    #[test]
    fn statusbar_renders_in_rounded_block() {
        let bar = StatusBar {
            poll_state: &PollState::Sleep,
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
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "╭");
        assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "╰");
    }

    #[test]
    fn statusbar_block_border_color() {
        let bar = StatusBar {
            poll_state: &PollState::Sleep,
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
        assert_eq!(buf.cell((0, 0)).unwrap().fg, theme::BORDER_STATUS);
    }

    // --- warnings tests ---

    fn render_bar_with_warnings(
        state: &PollState,
        hook_status: &HookStatus,
        warnings: &[String],
    ) -> String {
        let bar = StatusBar {
            poll_state: state,
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
            &PollState::Sleep,
            &HookStatus::Installed(HookLocation::Local),
            &warnings,
        );
        assert!(content.contains("AWS: timeout"), "got: {content}");
    }

    #[test]
    fn renders_hook_hint_and_warning_together() {
        let warnings = vec!["AWS: timeout".to_string()];
        let content = render_bar_with_warnings(&PollState::Sleep, &HookStatus::Missing, &warnings);
        assert!(content.contains("p=install hook"), "got: {content}");
        assert!(content.contains("AWS: timeout"), "got: {content}");
    }

    // --- push signal tests ---

    fn render_with_push(push_signal_at: Option<Instant>) -> String {
        let bar = StatusBar {
            poll_state: &PollState::Sleep,
            warnings: &[],
            hook_status: &HookStatus::Installed(HookLocation::Local),
            has_global_hooks_path: false,
            boost_pressed_at: None,
            push_signal_at,
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
    fn push_signal_shows_pushed_label() {
        let content = render_with_push(Some(Instant::now()));
        assert!(content.contains("pushed!"), "got: {content}");
        assert!(!content.contains("✓hook"), "got: {content}");
    }

    #[test]
    fn expired_push_signal_shows_checkmark() {
        let expired = Instant::now() - Duration::from_secs(5);
        let content = render_with_push(Some(expired));
        assert!(content.contains("✓hook"), "got: {content}");
        assert!(!content.contains("pushed!"), "got: {content}");
    }

    #[test]
    fn push_signal_uses_scan_color() {
        let bar = StatusBar {
            poll_state: &PollState::Sleep,
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
            poll_state: &PollState::Sleep,
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
            poll_state: &PollState::Sleep,
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

    // --- spinner cadence ---

    #[test]
    fn spinner_frame_is_in_set() {
        let f = spinner_frame();
        assert!(SPINNER_FRAMES.contains(&f));
    }
}
