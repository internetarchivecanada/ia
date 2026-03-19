//! Color theme with archive.org-inspired palette.
//!
//! Provides true-color and 256-color fallback based on `$COLORTERM` detection.

use ratatui::style::Color;

/// Color theme for the dashboard TUI.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // Background & text
    #[allow(dead_code)] // Available for terminal background override
    pub bg: Color,
    pub text: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub text_very_muted: Color,
    pub border: Color,

    // Accent colors (archive.org-inspired)
    pub maroon: Color,
    pub maroon_bright: Color,
    pub gold: Color,
    pub blue: Color,
    pub green: Color,
    pub red: Color,
}

impl Theme {
    /// Detect terminal color capabilities and return the appropriate theme.
    pub fn detect() -> Self {
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        Self::for_env(&colorterm)
    }

    /// Build a theme for a given `COLORTERM` value.
    pub fn for_env(colorterm: &str) -> Self {
        let true_color = matches!(colorterm, "truecolor" | "24bit");
        if true_color {
            Self::true_color()
        } else {
            Self::indexed()
        }
    }

    fn true_color() -> Self {
        Self {
            bg: Color::Rgb(28, 28, 28),
            text: Color::Rgb(212, 212, 212),
            text_secondary: Color::Rgb(153, 153, 153),
            text_muted: Color::Rgb(102, 102, 102),
            text_very_muted: Color::Rgb(68, 68, 68),
            border: Color::Rgb(51, 51, 51),
            maroon: Color::Rgb(139, 26, 26),
            maroon_bright: Color::Rgb(205, 92, 92),
            gold: Color::Rgb(255, 215, 0),
            blue: Color::Rgb(91, 141, 217),
            green: Color::Rgb(63, 185, 80),
            red: Color::Rgb(248, 81, 73),
        }
    }

    fn indexed() -> Self {
        Self {
            bg: Color::Indexed(234),
            text: Color::Indexed(252),
            text_secondary: Color::Indexed(246),
            text_muted: Color::Indexed(242),
            text_very_muted: Color::Indexed(238),
            border: Color::Indexed(236),
            maroon: Color::Indexed(88),
            maroon_bright: Color::Indexed(167),
            gold: Color::Indexed(220),
            blue: Color::Indexed(68),
            green: Color::Indexed(71),
            red: Color::Indexed(196),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_true_color_detection() {
        let theme = Theme::for_env("truecolor");
        assert_eq!(theme.bg, Color::Rgb(28, 28, 28));
    }

    #[test]
    fn test_24bit_detection() {
        let theme = Theme::for_env("24bit");
        assert_eq!(theme.bg, Color::Rgb(28, 28, 28));
    }

    #[test]
    fn test_256_color_fallback() {
        let theme = Theme::for_env("");
        assert_eq!(theme.bg, Color::Indexed(234));
    }

    #[test]
    fn test_all_accent_colors_present() {
        let theme = Theme::detect();
        let _ = theme.maroon;
        let _ = theme.maroon_bright;
        let _ = theme.gold;
        let _ = theme.blue;
        let _ = theme.green;
        let _ = theme.red;
    }
}
