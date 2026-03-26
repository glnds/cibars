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
pub const STATUS_RUNNING_TIP: Color = Color::Rgb(240, 80, 80);
pub const STATUS_FAILED: Color = Color::Rgb(255, 64, 64);
pub const STATUS_IDLE: Color = Color::Rgb(85, 85, 85);

// Separator color
pub const SEPARATOR: Color = Color::Rgb(48, 48, 48);

// Progress bar characters (braille, 4-dot height)
pub const BAR_FILLED: char = '\u{28FF}'; // ⣿
pub const BAR_UNFILLED: char = '\u{28FF}'; // ⣿

// Tick bar characters
pub const TICK_FILLED: char = '\u{25AE}'; // ▮
pub const TICK_EMPTY: char = '\u{25AF}'; // ▯

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
        assert_eq!(STATUS_RUNNING_TIP, Color::Rgb(240, 80, 80));
        assert_eq!(STATUS_FAILED, Color::Rgb(255, 64, 64));
        assert_eq!(STATUS_IDLE, Color::Rgb(85, 85, 85));
    }

    #[test]
    fn bar_chars_are_correct_unicode() {
        assert_eq!(BAR_FILLED, '⣿');
        assert_eq!(BAR_UNFILLED, '⣿');
    }

    #[test]
    fn tick_chars_are_correct_unicode() {
        assert_eq!(TICK_FILLED, '▮');
        assert_eq!(TICK_EMPTY, '▯');
    }

    #[test]
    fn bar_empty_and_separator_colors() {
        assert_eq!(BAR_EMPTY, Color::Rgb(48, 48, 48));
        assert_eq!(SEPARATOR, Color::Rgb(48, 48, 48));
    }
}
