//! Terminal lifecycle — enter/leave the alternate screen and raw mode.
//!
//! `Tui` restores the terminal on `Drop`, so a panic mid-run still leaves the
//! user with a usable shell. (A panic hook that restores *before* printing the
//! panic message is a follow-up — see BACKLOG.md.)

use std::io::{self, Stdout};

use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Tui {
    /// Enter the alternate screen + raw mode and build the ratatui terminal.
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
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
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}
