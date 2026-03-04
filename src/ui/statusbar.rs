use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::poll_scheduler::PollState;

const NUM_TICKS: u64 = 5;

pub struct StatusBar<'a> {
    pub poll_state: &'a PollState,
    pub elapsed_since_poll: Duration,
    pub cooldown_remaining: Option<Duration>,
    pub warnings: &'a [String],
}

/// Compute how many ticks are filled based on elapsed time and state interval.
fn filled_ticks(elapsed: Duration, state: &PollState) -> usize {
    let tick_duration_ms = match state {
        PollState::Idle => 30_000 / NUM_TICKS, // 6s per tick
        PollState::Watching | PollState::Active | PollState::Cooldown => 5_000 / NUM_TICKS, // 1s per tick
    };
    let filled = elapsed.as_millis() as u64 / tick_duration_ms;
    filled.min(NUM_TICKS) as usize
}

/// Build the tick bar string: filled '=' + remaining '-'.
fn tick_bar(filled: usize) -> String {
    let remaining = (NUM_TICKS as usize).saturating_sub(filled);
    format!("{}{}", "=".repeat(filled), "-".repeat(remaining))
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let label = match self.poll_state {
            PollState::Idle => "Slow",
            PollState::Watching | PollState::Active | PollState::Cooldown => "Fast",
        };

        let filled = filled_ticks(self.elapsed_since_poll, self.poll_state);
        let bar = tick_bar(filled);

        let mut spans = vec![Span::raw(format!("{label} Polling: {bar}"))];

        if let Some(cd) = self.cooldown_remaining {
            spans.push(Span::raw(format!(" | Cooldown: {}s", cd.as_secs())));
        }

        spans.push(Span::raw(" | e=expand r=boost q=quit"));

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

    fn render_bar(state: &PollState, elapsed: Duration, cooldown: Option<Duration>) -> String {
        let bar = StatusBar {
            poll_state: state,
            elapsed_since_poll: elapsed,
            cooldown_remaining: cooldown,
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
    fn idle_shows_slow_polling() {
        let content = render_bar(&PollState::Idle, Duration::ZERO, None);
        assert!(content.contains("Slow Polling:"), "got: {content}");
    }

    #[test]
    fn active_shows_fast_polling() {
        let content = render_bar(&PollState::Active, Duration::ZERO, None);
        assert!(content.contains("Fast Polling:"), "got: {content}");
    }

    #[test]
    fn cooldown_shows_fast_polling_with_timer() {
        let content = render_bar(
            &PollState::Cooldown,
            Duration::ZERO,
            Some(Duration::from_secs(42)),
        );
        assert!(content.contains("Fast Polling:"), "got: {content}");
        assert!(content.contains("Cooldown: 42s"), "got: {content}");
    }

    #[test]
    fn idle_zero_elapsed_shows_all_empty() {
        let content = render_bar(&PollState::Idle, Duration::ZERO, None);
        assert!(content.contains("-----"), "got: {content}");
    }

    #[test]
    fn idle_full_elapsed_shows_all_filled() {
        let content = render_bar(&PollState::Idle, Duration::from_secs(30), None);
        assert!(content.contains("====="), "got: {content}");
    }

    #[test]
    fn active_partial_elapsed() {
        // 1s per tick, 2s elapsed → 2 filled
        let content = render_bar(&PollState::Active, Duration::from_secs(2), None);
        assert!(content.contains("==---"), "got: {content}");
    }

    #[test]
    fn idle_partial_elapsed() {
        // 6s per tick, 12s elapsed → 2 filled
        let content = render_bar(&PollState::Idle, Duration::from_secs(12), None);
        assert!(content.contains("==---"), "got: {content}");
    }

    #[test]
    fn shows_boost_not_refresh() {
        let content = render_bar(&PollState::Idle, Duration::ZERO, None);
        assert!(content.contains("r=boost"), "got: {content}");
        assert!(!content.contains("r=refresh"), "got: {content}");
    }

    #[test]
    fn filled_ticks_clamped_to_max() {
        // Even with huge elapsed, never exceeds NUM_TICKS
        assert_eq!(
            filled_ticks(Duration::from_secs(999), &PollState::Active),
            5
        );
    }

    #[test]
    fn tick_bar_formatting() {
        assert_eq!(tick_bar(0), "-----");
        assert_eq!(tick_bar(3), "===--");
        assert_eq!(tick_bar(5), "=====");
    }
}
