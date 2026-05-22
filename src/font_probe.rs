//! Detect whether the current terminal's font renders Powerline / Nerd
//! glyphs as a single cell. Lifted from ebman — keep the two in sync.
//!
//! We can't inspect the font from a TUI, but we can write a known Powerline
//! triangle (`U+E0B0`) and ask the terminal where the cursor ended up. A
//! patched font draws the glyph in one cell — cursor advances by 1. Anything
//! else is treated as unsupported.
//!
//! The probe is best-effort: any I/O error falls back to `false`. It must run
//! *before* entering the alternate screen / raw mode.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor;
use crossterm::style::Print;
use crossterm::terminal;
use crossterm::ExecutableCommand;

/// Probe a Powerline right-triangle (`U+E0B0`); true if it advanced the cursor
/// by exactly one column (the patched-font signature).
pub fn detect_powerline_support() -> bool {
    if !std::io::stdout().is_terminal_like() {
        return false;
    }
    probe_glyph_width_one("\u{E0B0}").unwrap_or(false)
}

/// Probe a Nerd Font MDI codepoint (`U+F048B`) to verify width 1. The E0Bx
/// Powerline range and the Fxxxx Nerd Font range come from different glyph
/// blocks; some fonts ship one without the other. Advisory only.
pub fn detect_tab_icon_support() -> bool {
    if !std::io::stdout().is_terminal_like() {
        return false;
    }
    probe_glyph_width_one("\u{F048B}").unwrap_or(false)
}

fn probe_glyph_width_one(glyph: &str) -> io::Result<bool> {
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    let restore = ProbeGuard;
    stdout.execute(cursor::SavePosition)?;
    let (col_before, _row) = cursor::position().unwrap_or((0, 0));
    stdout.execute(Print(glyph))?;
    stdout.flush()?;
    std::thread::sleep(Duration::from_millis(20));
    let (col_after, _row) = cursor::position().unwrap_or((col_before, 0));
    stdout.execute(cursor::RestorePosition)?;
    stdout.execute(Print("  "))?;
    stdout.execute(cursor::RestorePosition)?;
    drop(restore);
    let advance = col_after.saturating_sub(col_before);
    Ok(classify_advance(advance))
}

/// Pure: a one-cell advance signals a patched font.
fn classify_advance(advance: u16) -> bool {
    advance == 1
}

/// Resolved-from-`auto` outcome. Pure structure so the decision is testable.
#[derive(Debug, PartialEq, Eq)]
pub struct AutoResolved {
    pub icons: &'static str,
    pub warn_tab_icons_missing: bool,
}

fn classify_auto(powerline: bool, tab_icons: bool) -> AutoResolved {
    if powerline {
        AutoResolved {
            icons: "powerline",
            warn_tab_icons_missing: !tab_icons,
        }
    } else {
        AutoResolved {
            icons: "unicode",
            warn_tab_icons_missing: false,
        }
    }
}

/// Resolve a configured icon style. `"auto"` triggers the probes and resolves
/// to `"powerline"` / `"unicode"`; any other value is passed through.
///
/// Pure-with-side-effects: only does I/O when the input is `"auto"`. Run once
/// at startup, before TUI init.
pub fn resolve_icons_setting(raw: &str) -> String {
    if raw.eq_ignore_ascii_case("auto") {
        let resolved = classify_auto(detect_powerline_support(), detect_tab_icon_support());
        if resolved.warn_tab_icons_missing {
            tracing::warn!(
                target: "pgman::font_probe",
                "Powerline glyph (U+E0B0) renders, but Nerd Font MDI codepoint \
                 (U+F048B) does not — some icons may misalign. Install a Nerd \
                 Font or set `icons = \"unicode\"`."
            );
        }
        resolved.icons.to_string()
    } else {
        raw.to_string()
    }
}

struct ProbeGuard;
impl Drop for ProbeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

trait IsTerminalLike {
    fn is_terminal_like(&self) -> bool;
}

impl IsTerminalLike for std::io::Stdout {
    fn is_terminal_like(&self) -> bool {
        use std::io::IsTerminal;
        self.is_terminal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_one_cell_advance_is_supported() {
        assert!(classify_advance(1));
    }

    #[test]
    fn classify_other_advances_are_unsupported() {
        assert!(!classify_advance(0));
        assert!(!classify_advance(2));
        assert!(!classify_advance(7));
    }

    #[test]
    fn classify_auto_powerline_with_tab_icons_works_cleanly() {
        let r = classify_auto(true, true);
        assert_eq!(r.icons, "powerline");
        assert!(!r.warn_tab_icons_missing);
    }

    #[test]
    fn classify_auto_powerline_without_tab_icons_warns() {
        let r = classify_auto(true, false);
        assert_eq!(r.icons, "powerline");
        assert!(r.warn_tab_icons_missing);
    }

    #[test]
    fn classify_auto_no_powerline_picks_unicode_and_does_not_warn() {
        let r = classify_auto(false, false);
        assert_eq!(r.icons, "unicode");
        assert!(!r.warn_tab_icons_missing);
        let r = classify_auto(false, true);
        assert_eq!(r.icons, "unicode");
        assert!(!r.warn_tab_icons_missing);
    }

    #[test]
    fn resolve_passes_through_non_auto_values() {
        assert_eq!(resolve_icons_setting("unicode"), "unicode");
        assert_eq!(resolve_icons_setting("ascii"), "ascii");
        assert_eq!(resolve_icons_setting("powerline"), "powerline");
        assert_eq!(resolve_icons_setting("bogus"), "bogus");
    }
}
