//! Colour themes. Lifted from ebman — keep the two in sync if either changes.
//! UI code must read colours from a `Theme`; no hardcoded `Color::*`.

use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: &'static str,

    // Severity colours
    pub health_green: Color,
    pub health_yellow: Color,
    pub health_red: Color,
    pub health_grey: Color,

    // Status colours
    pub status_ready: Color,
    pub status_updating: Color,
    pub status_terminating: Color,

    // Chrome
    pub border_idle: Color,
    pub border_active: Color,
    pub title: Color,
    pub title_alt: Color,
    pub muted: Color,
    pub text: Color,
    pub accent: Color,

    // SQL syntax highlighting in the editor.
    /// String literal — `'foo'`, dollar-quoted, etc.
    pub syn_string: Color,
    /// Identifier that doesn't resolve in the schema cache /
    /// in-scope tables — typo flag.
    pub syn_unknown: Color,

    // Splash elephant palette (matches the Claude Design export)
    pub elephant_outline: Color,
    pub elephant_body: Color,
    pub elephant_shade: Color,
    pub elephant_eye: Color,
    pub elephant_pupil: Color,
    pub elephant_cheek: Color,
    pub elephant_tusk: Color,

    // Backgrounds
    pub row_alt_bg: Color,
    pub row_red_bg: Color,
    pub row_yellow_bg: Color,
    pub row_selected_bg: Color,
    pub row_hover_bg: Color,

    // App palette (16 distinct colours; sequential assignment in UI code)
    pub app_palette: Vec<Color>,

    // Icons preference
    pub icons: IconStyle,
}

// `IconStyle` now lives in the shared `tb-tui-common` crate. The
// re-export keeps the canonical path `crate::theme::IconStyle`
// stable for every existing call site.
pub use tui_common::theme::IconStyle;

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "dark",
            health_green: Color::Rgb(140, 220, 160),
            health_yellow: Color::Rgb(240, 210, 130),
            health_red: Color::Rgb(240, 130, 130),
            health_grey: Color::Rgb(150, 150, 160),

            status_ready: Color::Rgb(140, 220, 160),
            status_updating: Color::Rgb(240, 210, 130),
            status_terminating: Color::Rgb(240, 130, 130),

            border_idle: Color::Rgb(80, 90, 110),
            border_active: Color::Rgb(120, 200, 240),
            title: Color::Rgb(140, 200, 240),
            title_alt: Color::Rgb(220, 160, 240),
            muted: Color::Rgb(150, 155, 170),
            text: Color::Rgb(220, 222, 230),
            accent: Color::Rgb(255, 200, 120),

            syn_string: Color::Rgb(180, 220, 140),
            syn_unknown: Color::Rgb(240, 140, 130),

            elephant_outline: Color::Rgb(14, 22, 56),
            elephant_body: Color::Rgb(125, 166, 230),
            elephant_shade: Color::Rgb(77, 123, 194),
            elephant_eye: Color::Rgb(255, 255, 255),
            elephant_pupil: Color::Rgb(10, 16, 36),
            elephant_cheek: Color::Rgb(255, 143, 168),
            elephant_tusk: Color::Rgb(255, 245, 214),

            row_alt_bg: Color::Rgb(18, 22, 28),
            row_red_bg: Color::Rgb(48, 18, 22),
            row_yellow_bg: Color::Rgb(40, 36, 16),
            row_selected_bg: Color::Rgb(40, 60, 90),
            row_hover_bg: Color::Rgb(30, 38, 50),

            app_palette: vec![
                Color::Rgb(120, 200, 240),
                Color::Rgb(220, 160, 240),
                Color::Rgb(140, 220, 200),
                Color::Rgb(220, 180, 140),
                Color::Rgb(180, 220, 240),
                Color::Rgb(240, 180, 220),
                Color::Rgb(180, 140, 230),
                Color::Rgb(140, 200, 220),
                Color::Rgb(220, 160, 200),
                Color::Rgb(150, 220, 200),
                Color::Rgb(120, 180, 255),
                Color::Rgb(200, 180, 255),
                Color::Rgb(255, 180, 230),
                Color::Rgb(180, 220, 255),
                Color::Rgb(170, 230, 230),
                Color::Rgb(220, 200, 250),
            ],

            icons: IconStyle::Unicode,
        }
    }

    /// Lighter palette suited to light-background terminals.
    pub fn light() -> Self {
        Self {
            name: "light",
            health_green: Color::Rgb(40, 130, 70),
            health_yellow: Color::Rgb(160, 110, 0),
            health_red: Color::Rgb(170, 30, 40),
            health_grey: Color::Rgb(100, 100, 110),

            status_ready: Color::Rgb(40, 130, 70),
            status_updating: Color::Rgb(160, 110, 0),
            status_terminating: Color::Rgb(170, 30, 40),

            border_idle: Color::Rgb(160, 165, 175),
            border_active: Color::Rgb(40, 100, 170),
            title: Color::Rgb(40, 100, 170),
            title_alt: Color::Rgb(120, 60, 170),
            muted: Color::Rgb(110, 115, 125),
            text: Color::Rgb(30, 32, 40),
            accent: Color::Rgb(180, 90, 0),

            syn_string: Color::Rgb(40, 120, 40),
            syn_unknown: Color::Rgb(180, 40, 40),

            elephant_outline: Color::Rgb(14, 22, 56),
            elephant_body: Color::Rgb(125, 166, 230),
            elephant_shade: Color::Rgb(77, 123, 194),
            elephant_eye: Color::Rgb(255, 255, 255),
            elephant_pupil: Color::Rgb(10, 16, 36),
            elephant_cheek: Color::Rgb(255, 143, 168),
            elephant_tusk: Color::Rgb(255, 245, 214),

            row_alt_bg: Color::Rgb(238, 240, 244),
            row_red_bg: Color::Rgb(250, 220, 220),
            row_yellow_bg: Color::Rgb(252, 240, 200),
            row_selected_bg: Color::Rgb(210, 224, 240),
            row_hover_bg: Color::Rgb(228, 234, 246),

            app_palette: vec![
                Color::Rgb(40, 100, 170),
                Color::Rgb(120, 60, 170),
                Color::Rgb(20, 130, 130),
                Color::Rgb(170, 100, 40),
                Color::Rgb(80, 80, 170),
                Color::Rgb(170, 80, 130),
                Color::Rgb(90, 60, 170),
                Color::Rgb(20, 110, 140),
                Color::Rgb(170, 90, 130),
                Color::Rgb(40, 130, 110),
                Color::Rgb(20, 90, 200),
                Color::Rgb(100, 80, 200),
                Color::Rgb(190, 80, 170),
                Color::Rgb(60, 110, 200),
                Color::Rgb(40, 150, 150),
                Color::Rgb(140, 80, 200),
            ],

            icons: IconStyle::Unicode,
        }
    }

    /// High-contrast palette for accessibility.
    pub fn high_contrast() -> Self {
        Self {
            name: "high-contrast",
            health_green: Color::Rgb(0, 255, 80),
            health_yellow: Color::Rgb(255, 230, 0),
            health_red: Color::Rgb(255, 60, 60),
            health_grey: Color::Rgb(220, 220, 220),

            status_ready: Color::Rgb(0, 255, 80),
            status_updating: Color::Rgb(255, 230, 0),
            status_terminating: Color::Rgb(255, 60, 60),

            border_idle: Color::Rgb(200, 200, 200),
            border_active: Color::Rgb(255, 255, 255),
            title: Color::Rgb(120, 220, 255),
            title_alt: Color::Rgb(255, 160, 255),
            muted: Color::Rgb(220, 220, 220),
            text: Color::Rgb(255, 255, 255),
            accent: Color::Rgb(255, 180, 0),

            syn_string: Color::Rgb(120, 240, 120),
            syn_unknown: Color::Rgb(255, 80, 80),

            elephant_outline: Color::Rgb(0, 0, 0),
            elephant_body: Color::Rgb(125, 166, 230),
            elephant_shade: Color::Rgb(77, 123, 194),
            elephant_eye: Color::Rgb(255, 255, 255),
            elephant_pupil: Color::Rgb(0, 0, 0),
            elephant_cheek: Color::Rgb(255, 143, 168),
            elephant_tusk: Color::Rgb(255, 245, 214),

            row_alt_bg: Color::Rgb(20, 20, 20),
            row_red_bg: Color::Rgb(80, 0, 0),
            row_yellow_bg: Color::Rgb(60, 50, 0),
            row_selected_bg: Color::Rgb(0, 80, 160),
            row_hover_bg: Color::Rgb(40, 40, 40),

            app_palette: vec![
                Color::Rgb(0, 220, 255),
                Color::Rgb(255, 100, 255),
                Color::Rgb(0, 255, 200),
                Color::Rgb(255, 180, 0),
                Color::Rgb(150, 200, 255),
                Color::Rgb(255, 200, 220),
                Color::Rgb(190, 130, 255),
                Color::Rgb(80, 220, 240),
                Color::Rgb(255, 150, 200),
                Color::Rgb(80, 255, 200),
                Color::Rgb(120, 180, 255),
                Color::Rgb(200, 180, 255),
                Color::Rgb(255, 180, 240),
                Color::Rgb(180, 220, 255),
                Color::Rgb(170, 255, 230),
                Color::Rgb(220, 200, 255),
            ],

            icons: IconStyle::Unicode,
        }
    }

    /// Returns black or white depending on which gives better contrast against
    /// `bg`. Uses the WCAG perceived-luminance formula, thresholded at 140/255.
    pub fn contrast_text(&self, bg: Color) -> Color {
        match bg {
            Color::Rgb(r, g, b) => {
                let luminance = (299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000;
                if luminance > 140 {
                    Color::Black
                } else {
                    Color::White
                }
            }
            _ => self.text,
        }
    }

    /// Parse a theme by name. Returns the matched theme plus an optional warning
    /// when the input didn't match a known preset.
    pub fn resolve(name: &str) -> (Self, Option<String>) {
        match name.trim().to_ascii_lowercase().as_str() {
            "" | "dark" => (Self::dark(), None),
            "light" => (Self::light(), None),
            "high-contrast" | "highcontrast" | "hc" => (Self::high_contrast(), None),
            other => (
                Self::dark(),
                Some(format!("unknown theme {other:?} — using 'dark'")),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_known_names() {
        let (t, w) = Theme::resolve("dark");
        assert_eq!(t.name, "dark");
        assert!(w.is_none());
        let (t, w) = Theme::resolve("LIGHT");
        assert_eq!(t.name, "light");
        assert!(w.is_none());
        let (t, w) = Theme::resolve("  Dark ");
        assert_eq!(t.name, "dark");
        assert!(w.is_none());
    }

    #[test]
    fn resolve_high_contrast_variants() {
        for n in ["high-contrast", "highcontrast", "HC", "Hc"] {
            let (t, w) = Theme::resolve(n);
            assert_eq!(t.name, "high-contrast", "for input {n:?}");
            assert!(w.is_none());
        }
    }

    #[test]
    fn resolve_unknown_falls_back_with_warning() {
        let (t, w) = Theme::resolve("dracula");
        assert_eq!(t.name, "dark");
        let msg = w.expect("expected a warning");
        assert!(msg.to_lowercase().contains("dracula"));
    }

    #[test]
    fn contrast_text_picks_black_on_bright_bg() {
        let theme = Theme::dark();
        assert_eq!(theme.contrast_text(theme.health_yellow), Color::Black);
        assert_eq!(theme.contrast_text(theme.accent), Color::Black);
    }

    #[test]
    fn contrast_text_falls_back_for_non_rgb() {
        let theme = Theme::dark();
        assert_eq!(theme.contrast_text(Color::Reset), theme.text);
    }
}
