//! pgman's side of the shared panel vocabulary (`tui_common::chrome`):
//! the palette built from pgman's `Theme`, and the panel block and
//! title every overlay and pane draws with, so they all look alike —
//! and like ebman.

use crate::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use tui_common::chrome::{decorate_title, rounded_block, Chrome};

/// The shared helpers' palette, from pgman's theme.
pub(crate) fn chrome(theme: &Theme) -> Chrome {
    Chrome {
        icons: theme.icons,
        border_idle: theme.border_idle,
        border_active: theme.border_active,
        muted: theme.muted,
        text: theme.text,
    }
}

/// A rounded panel with an idle border; callers set their own border
/// colour after, as before.
pub(crate) fn panel_block(theme: &Theme) -> Block<'static> {
    rounded_block(&chrome(theme), false)
}

/// A panel title split at its first ` — `: the name, and the key hints
/// or detail after it. Pure.
pub(crate) fn split_title(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once(" — ") {
        Some((name, rest)) if !rest.trim().is_empty() => (name.trim(), Some(rest.trim())),
        _ => (raw.trim(), None),
    }
}

/// A panel title as it sits in the border: the name decorated and bold
/// (`[ ◆ environments ◆ ]`), any hints after it muted.
pub(crate) fn panel_title(theme: &Theme, raw: &str) -> Line<'static> {
    panel_title_in(theme, raw, theme.title)
}

/// [`panel_title`] with the name in `colour` — a warning's yellow.
pub(crate) fn panel_title_in(
    theme: &Theme,
    raw: &str,
    colour: ratatui::style::Color,
) -> Line<'static> {
    let (name, rest) = split_title(raw);
    let mut spans = vec![Span::styled(
        decorate_title(theme.icons, name),
        Style::default().fg(colour).add_modifier(Modifier::BOLD),
    )];
    if let Some(rest) = rest {
        spans.push(Span::styled(
            format!(" {rest} "),
            Style::default().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_split_name_from_hints() {
        assert_eq!(
            split_title(" environments — enter connect · q close "),
            ("environments", Some("enter connect · q close"))
        );
        assert_eq!(split_title(" about "), ("about", None));
        assert_eq!(
            split_title(" a — "),
            ("a —", None),
            "nothing after the dash"
        );
    }

    #[test]
    fn the_name_is_decorated_and_the_hints_are_not() {
        let line = panel_title(&Theme::default(), " sessions — r refresh ");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "[ ◆ sessions ◆ ] r refresh ");
    }
}
