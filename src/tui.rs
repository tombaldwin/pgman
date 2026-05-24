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

/// The terminal-side surface `App::run` interacts with: render one
/// frame, then (for the `\e` external-editor handoff) suspend +
/// resume the alt-screen. Trait-abstracted so a `HeadlessTui` can
/// drive the run loop in tests without a real terminal.
pub trait TuiHost {
    fn draw(&mut self, app: &mut crate::app::App) -> io::Result<()>;
    fn suspend(&mut self) -> io::Result<()>;
    fn resume(&mut self) -> io::Result<()>;
}

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

    /// Step out of the alternate screen + raw mode so a child process
    /// (like `$EDITOR`) can take over the terminal. The terminal
    /// stays in this state until `resume()` is called or `Tui` is
    /// dropped. Idempotent enough that a failed resume still leaves
    /// the operator with a usable shell.
    pub fn suspend(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        let _ = stdout.execute(DisableBracketedPaste);
        disable_raw_mode()?;
        stdout.execute(LeaveAlternateScreen)?;
        Ok(())
    }

    /// Re-enter alt screen + raw mode after a `suspend()`. Caller
    /// should call `terminal.clear()` (via the next `draw`) so the
    /// display catches up after the child clobbered the screen.
    pub fn resume(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let _ = stdout.execute(EnableBracketedPaste);
        self.terminal.clear()?;
        Ok(())
    }
}

impl TuiHost for Tui {
    fn draw(&mut self, app: &mut crate::app::App) -> io::Result<()> {
        Tui::draw(self, app)
    }
    fn suspend(&mut self) -> io::Result<()> {
        Tui::suspend(self)
    }
    fn resume(&mut self) -> io::Result<()> {
        Tui::resume(self)
    }
}

/// A no-op TuiHost for tests / batch / headless contexts. Records
/// the call sequence so tests can assert on it. Drops are no-ops —
/// nothing to restore.
#[derive(Debug, Default)]
pub struct HeadlessTui {
    pub draws: usize,
    pub suspends: usize,
    pub resumes: usize,
}

impl TuiHost for HeadlessTui {
    fn draw(&mut self, _app: &mut crate::app::App) -> io::Result<()> {
        self.draws += 1;
        Ok(())
    }
    fn suspend(&mut self) -> io::Result<()> {
        self.suspends += 1;
        Ok(())
    }
    fn resume(&mut self) -> io::Result<()> {
        self.resumes += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_terminal_is_idempotent() {
        // Cheap smoke: calling twice doesn't return an error path —
        // every step inside is best-effort `let _ = …`.
        restore_terminal();
        restore_terminal();
    }

    #[test]
    fn install_panic_hook_runs_only_once() {
        // Once::call_once invariant — call twice, both succeed.
        install_panic_hook();
        install_panic_hook();
    }

    #[test]
    fn headless_records_call_sequence() {
        let mut h = HeadlessTui::default();
        let mut app = crate::app::App::new(
            crate::theme::Theme::default(),
            None,
            Vec::new(),
            crate::safety::SafetyConfig::default(),
        );
        h.draw(&mut app).unwrap();
        h.suspend().unwrap();
        h.resume().unwrap();
        h.draw(&mut app).unwrap();
        assert_eq!(h.draws, 2);
        assert_eq!(h.suspends, 1);
        assert_eq!(h.resumes, 1);
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        restore_terminal();
    }
}
