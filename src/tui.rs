//! Terminal lifecycle — enter/leave the alternate screen and raw mode.
//!
//! `Tui` restores the terminal on `Drop`, so the orderly shutdown path
//! always leaves the user with a usable shell. The panic hook installed
//! by `enter` covers the other case: if any thread panics mid-run we
//! restore the terminal *before* the default hook prints the backtrace
//! to stderr, so the trace is readable and the cursor / echo are back.

use std::io::{self, Stdout};
use std::sync::Once;

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

/// Best-effort terminal restore. Idempotent and safe to call from a
/// panic hook (errors are ignored). Used by both `Drop` and the panic
/// hook so the two paths can't drift.
fn restore_terminal() {
    let _ = io::stdout().execute(DisableBracketedPaste);
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
}

/// Wrap the existing panic hook so the terminal is restored *before* the
/// default hook prints the backtrace — otherwise the trace lands inside
/// the alternate screen and disappears when we drop out of it.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
    });
}

impl Tui {
    /// Enter the alternate screen + raw mode and build the ratatui terminal.
    pub fn enter() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        // Bracketed paste: terminal wraps pasted text in escape codes so
        // crossterm delivers it as a single `Event::Paste(String)` rather
        // than streaming each character through `Event::Key`. Best-effort
        // — older terminals ignore the enable sequence; we just keep
        // pasting char-by-char in that case.
        let _ = stdout.execute(EnableBracketedPaste);
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }

    /// Render one frame.
    pub fn draw(&mut self, app: &mut crate::app::App) -> io::Result<()> {
        self.terminal.draw(|f| crate::ui::draw(f, app))?;
        Ok(())
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        restore_terminal();
    }
}
