use ratatui::style::Color;

// Border colors (btop-inspired palette)
pub const BORDER_HEADER: Color = Color::Rgb(95, 135, 135);
pub const BORDER_ACTIONS: Color = Color::Rgb(95, 95, 135);
pub const BORDER_PIPELINES: Color = Color::Rgb(135, 135, 95);
pub const BORDER_STATUS: Color = Color::Rgb(48, 48, 48);

// Text colors
#[allow(dead_code)]
pub const FG_PRIMARY: Color = Color::Rgb(188, 188, 188);
pub const FG_DIM: Color = Color::Rgb(85, 85, 85);

// Bar colors
pub const BAR_EMPTY: Color = Color::Rgb(48, 48, 48);

// Status colors
pub const STATUS_SUCCESS: Color = Color::Rgb(0, 255, 127);
pub const STATUS_RUNNING: Color = Color::Rgb(240, 192, 80);
pub const STATUS_RUNNING_TIP: Color = Color::Rgb(255, 158, 100);
pub const STATUS_FAILED: Color = Color::Rgb(255, 64, 64);
pub const STATUS_IDLE: Color = Color::Rgb(85, 85, 85);

// Poll state colors
pub const POLL_SLEEP: Color = Color::Rgb(184, 134, 11); // DarkGoldenrod — Sleep moon
pub const POLL_SCAN: Color = Color::Rgb(72, 151, 212); // accent — push/relink flashes
pub const POLL_FAST: Color = Color::Rgb(240, 160, 60); // Polling label — warm amber
pub const POLL_COOL: Color = Color::Rgb(95, 135, 135); // Cooldown label — cpu_box

/// Linearly interpolate between two RGB colors. `t` is clamped to [0.0, 1.0].
pub fn lerp_color(from: Color, to: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    if let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (from, to) {
        Color::Rgb(
            (r1 as f32 + (r2 as f32 - r1 as f32) * t).round() as u8,
            (g1 as f32 + (g2 as f32 - g1 as f32) * t).round() as u8,
            (b1 as f32 + (b2 as f32 - b1 as f32) * t).round() as u8,
        )
    } else {
        from
    }
}

// Separator color
pub const SEPARATOR: Color = Color::Rgb(48, 48, 48);

// Progress bar characters (braille, 4-dot height)
pub const BAR_FILLED: char = '\u{28FF}'; // ⣿
pub const BAR_UNFILLED: char = '\u{28FF}'; // ⣿

// Boost flash color (btop hi_fg — keyboard shortcut highlight)
pub const BOOST_FLASH: Color = Color::Rgb(255, 85, 85);

// SSO status character
pub const CHECK_MARK: char = '\u{2713}'; // ✓

// Heartbeat LED character
pub const SYMBOL_HEARTBEAT: char = '\u{25C9}'; // ◉

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_colors_are_correct_rgb() {
        assert_eq!(BORDER_HEADER, Color::Rgb(95, 135, 135));
        assert_eq!(BORDER_ACTIONS, Color::Rgb(95, 95, 135));
        assert_eq!(BORDER_PIPELINES, Color::Rgb(135, 135, 95));
        assert_eq!(BORDER_STATUS, Color::Rgb(48, 48, 48));
    }

    #[test]
    fn text_colors_are_correct_rgb() {
        assert_eq!(FG_PRIMARY, Color::Rgb(188, 188, 188));
        assert_eq!(FG_DIM, Color::Rgb(85, 85, 85));
    }

    #[test]
    fn status_colors_are_correct_rgb() {
        assert_eq!(STATUS_SUCCESS, Color::Rgb(0, 255, 127));
        assert_eq!(STATUS_RUNNING, Color::Rgb(240, 192, 80));
        assert_eq!(STATUS_RUNNING_TIP, Color::Rgb(255, 158, 100));
        assert_eq!(STATUS_FAILED, Color::Rgb(255, 64, 64));
        assert_eq!(STATUS_IDLE, Color::Rgb(85, 85, 85));
    }

    #[test]
    fn bar_chars_are_correct_unicode() {
        assert_eq!(BAR_FILLED, '⣿');
        assert_eq!(BAR_UNFILLED, '⣿');
    }

    #[test]
    fn check_mark_is_correct_unicode() {
        assert_eq!(CHECK_MARK, '✓');
    }

    #[test]
    fn heartbeat_symbol_is_correct_unicode() {
        assert_eq!(SYMBOL_HEARTBEAT, '◉');
    }

    #[test]
    fn boost_flash_is_correct_rgb() {
        assert_eq!(BOOST_FLASH, Color::Rgb(255, 85, 85));
    }

    #[test]
    fn bar_empty_and_separator_colors() {
        assert_eq!(BAR_EMPTY, Color::Rgb(48, 48, 48));
        assert_eq!(SEPARATOR, Color::Rgb(48, 48, 48));
    }

    #[test]
    fn poll_state_colors_are_correct_rgb() {
        assert_eq!(POLL_SLEEP, Color::Rgb(184, 134, 11));
        assert_eq!(POLL_SCAN, Color::Rgb(72, 151, 212));
        assert_eq!(POLL_FAST, Color::Rgb(240, 160, 60));
        assert_eq!(POLL_COOL, Color::Rgb(95, 135, 135));
    }

    #[test]
    fn lerp_color_endpoints() {
        let a = Color::Rgb(100, 200, 50);
        let b = Color::Rgb(200, 100, 250);
        assert_eq!(lerp_color(a, b, 0.0), a);
        assert_eq!(lerp_color(a, b, 1.0), b);
    }

    #[test]
    fn lerp_color_midpoint() {
        let a = Color::Rgb(100, 200, 50);
        let b = Color::Rgb(200, 100, 250);
        assert_eq!(lerp_color(a, b, 0.5), Color::Rgb(150, 150, 150));
    }
}
