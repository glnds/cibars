use ratatui::style::Color;

// Border colors (Tokyo Night palette)
pub const BORDER_HEADER: Color = Color::Rgb(125, 207, 255);
pub const BORDER_ACTIONS: Color = Color::Rgb(122, 162, 247);
pub const BORDER_PIPELINES: Color = Color::Rgb(187, 154, 247);
pub const BORDER_STATUS: Color = Color::Rgb(86, 95, 137);

// Text colors
#[allow(dead_code)]
pub const FG_PRIMARY: Color = Color::Rgb(192, 202, 245);
pub const FG_DIM: Color = Color::Rgb(86, 95, 137);

// Bar colors
pub const BAR_EMPTY: Color = Color::Rgb(59, 66, 97);

// Status colors
pub const STATUS_SUCCESS: Color = Color::Rgb(158, 206, 106);
pub const STATUS_RUNNING: Color = Color::Rgb(224, 175, 104);
pub const STATUS_RUNNING_TIP: Color = Color::Rgb(255, 158, 100);
pub const STATUS_FAILED: Color = Color::Rgb(247, 118, 142);
pub const STATUS_IDLE: Color = Color::Rgb(86, 95, 137);

// Separator color
pub const SEPARATOR: Color = Color::Rgb(59, 66, 97);

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
        assert_eq!(BORDER_HEADER, Color::Rgb(125, 207, 255));
        assert_eq!(BORDER_ACTIONS, Color::Rgb(122, 162, 247));
        assert_eq!(BORDER_PIPELINES, Color::Rgb(187, 154, 247));
        assert_eq!(BORDER_STATUS, Color::Rgb(86, 95, 137));
    }

    #[test]
    fn text_colors_are_correct_rgb() {
        assert_eq!(FG_PRIMARY, Color::Rgb(192, 202, 245));
        assert_eq!(FG_DIM, Color::Rgb(86, 95, 137));
    }

    #[test]
    fn status_colors_are_correct_rgb() {
        assert_eq!(STATUS_SUCCESS, Color::Rgb(158, 206, 106));
        assert_eq!(STATUS_RUNNING, Color::Rgb(224, 175, 104));
        assert_eq!(STATUS_RUNNING_TIP, Color::Rgb(255, 158, 100));
        assert_eq!(STATUS_FAILED, Color::Rgb(247, 118, 142));
        assert_eq!(STATUS_IDLE, Color::Rgb(86, 95, 137));
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
        assert_eq!(BAR_EMPTY, Color::Rgb(59, 66, 97));
        assert_eq!(SEPARATOR, Color::Rgb(59, 66, 97));
    }
}
