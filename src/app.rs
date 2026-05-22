//! Application state and the event loop.

pub mod msg;

use crate::app::msg::AppMsg;
use crate::conn::{self, Dsn};
use crate::grid::Grid;
use crate::theme::Theme;
use crate::tui::Tui;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::widgets::TableState;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// The query M0 runs on connect — a read-only database overview. Every column
/// is text, so it renders without type-specific decoding.
const BOOTSTRAP_SQL: &str = "select datname as database, \
    pg_size_pretty(pg_database_size(datname)) as size \
    from pg_database where not datistemplate order by datname";

/// Top-level interaction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Help,
}

/// Connection lifecycle state.
#[derive(Debug, Clone)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected { server_version: String },
    Failed(String),
}

pub struct App {
    pub theme: Theme,
    pub mode: Mode,
    pub conn_state: ConnState,
    pub dsn: Option<Dsn>,
    pub grid: Grid,
    pub grid_state: TableState,
    pub splash_visible: bool,
    pub splash_tick: usize,
    pub generation: u64,
    pub should_quit: bool,

    read_only: bool,
    statement_timeout_ms: u64,
    msg_tx: UnboundedSender<AppMsg>,
    msg_rx: Option<UnboundedReceiver<AppMsg>>,
}

impl App {
    pub fn new(theme: Theme, dsn: Option<Dsn>, read_only: bool, statement_timeout_ms: u64) -> Self {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        Self {
            theme,
            mode: Mode::Normal,
            conn_state: ConnState::Disconnected,
            dsn,
            grid: Grid::default(),
            grid_state: TableState::default(),
            splash_visible: true,
            splash_tick: 0,
            generation: 0,
            should_quit: false,
            read_only,
            statement_timeout_ms,
            msg_tx,
            msg_rx: Some(msg_rx),
        }
    }

    /// Run the event loop until the user quits.
    pub async fn run(&mut self, tui: &mut Tui) -> anyhow::Result<()> {
        let mut msg_rx = self
            .msg_rx
            .take()
            .expect("App::run must be called exactly once");

        if self.dsn.is_some() {
            self.start_connect();
        }

        let mut events = EventStream::new();
        // One frame clock for all animation sources (splash, spinner). Gated
        // by `wants_animation` so an idle, connected app does no work.
        let mut frame = tokio::time::interval(Duration::from_millis(110));
        frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        while !self.should_quit {
            tui.draw(self)?;
            let animate = self.wants_animation();
            tokio::select! {
                ev = events.next() => {
                    if let Some(Ok(ev)) = ev {
                        self.on_event(ev);
                    }
                }
                _ = frame.tick(), if animate => {
                    self.splash_tick = self.splash_tick.wrapping_add(1);
                }
                Some(msg) = msg_rx.recv() => {
                    self.on_msg(msg);
                }
            }
        }
        Ok(())
    }

    /// Whether the frame clock should keep ticking.
    fn wants_animation(&self) -> bool {
        self.splash_visible || matches!(self.conn_state, ConnState::Connecting)
    }

    /// Spawn the connect + bootstrap-query task. The result returns as an
    /// `AppMsg` tagged with the current generation.
    fn start_connect(&mut self) {
        let Some(dsn) = self.dsn.clone() else {
            return;
        };
        self.conn_state = ConnState::Connecting;
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let read_only = self.read_only;
        let statement_timeout_ms = self.statement_timeout_ms;
        tokio::spawn(async move {
            let msg = match conn::connect_and_bootstrap(
                dsn,
                read_only,
                statement_timeout_ms,
                BOOTSTRAP_SQL.to_string(),
            )
            .await
            {
                Ok(b) => AppMsg::Booted {
                    generation,
                    server_version: b.server_version,
                    grid: b.grid,
                },
                Err(error) => AppMsg::BootFailed { generation, error },
            };
            let _ = tx.send(msg);
        });
    }

    fn on_msg(&mut self, msg: AppMsg) {
        if msg.generation() != self.generation {
            tracing::debug!("dropping stale message from generation {}", msg.generation());
            return;
        }
        match msg {
            AppMsg::Booted {
                server_version,
                grid,
                ..
            } => {
                self.conn_state = ConnState::Connected { server_version };
                self.grid = grid;
                self.grid_state
                    .select(if self.grid.is_empty() { None } else { Some(0) });
                self.splash_visible = false; // connection-ready dismisses splash
            }
            AppMsg::BootFailed { error, .. } => {
                self.conn_state = ConnState::Failed(error);
                self.splash_visible = false;
            }
        }
    }

    fn on_event(&mut self, ev: Event) {
        if let Event::Key(key) = ev {
            // Ignore key-release events (kitty protocol / Windows).
            if key.kind != KeyEventKind::Release {
                self.on_key(key);
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        // Any key dismisses the splash and is otherwise consumed.
        if self.splash_visible {
            self.splash_visible = false;
            return;
        }
        match self.mode {
            Mode::Help => {
                if matches!(key.code, KeyCode::Char('q' | '?') | KeyCode::Esc) {
                    self.mode = Mode::Normal;
                }
            }
            Mode::Normal => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
                KeyCode::Char('?') => self.mode = Mode::Help,
                KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
                KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
                KeyCode::Char('g') | KeyCode::Home => self.select_row(0),
                KeyCode::Char('G') | KeyCode::End => {
                    self.select_row(self.grid.row_count().saturating_sub(1));
                }
                _ => {}
            },
        }
    }

    fn scroll(&mut self, delta: isize) {
        let count = self.grid.row_count();
        if count == 0 {
            return;
        }
        let current = self.grid_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, count as isize - 1);
        self.grid_state.select(Some(next as usize));
    }

    fn select_row(&mut self, idx: usize) {
        let count = self.grid.row_count();
        if count == 0 {
            return;
        }
        self.grid_state.select(Some(idx.min(count - 1)));
    }
}
