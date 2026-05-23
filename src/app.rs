//! Application state and the event loop.

pub mod msg;

use crate::app::msg::AppMsg;
use crate::conn::{self, Dsn};
use crate::grid::Grid;
use crate::safety::{self, Decision, Guard, SafetyConfig};
use crate::theme::Theme;
use crate::tui::Tui;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::widgets::TableState;
use std::sync::Arc;
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
    /// Browsing the results grid.
    Normal,
    /// Help overlay.
    Help,
    /// Typing SQL in the editor.
    Editor,
    /// Confirmation modal for a guarded statement.
    Confirm,
}

/// Connection lifecycle state.
#[derive(Debug, Clone)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected { server_version: String },
    Failed(String),
}

/// What flavour of run the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Run,
    Explain,
    ExplainAnalyze,
}

impl RunKind {
    pub fn label(self) -> &'static str {
        match self {
            RunKind::Run => "run",
            RunKind::Explain => "EXPLAIN",
            RunKind::ExplainAnalyze => "EXPLAIN ANALYZE",
        }
    }
}

/// A run waiting on user confirmation (the safety guard returned `Confirm`).
#[derive(Debug, Clone)]
pub struct PendingRun {
    pub sql: String,
    pub kind: RunKind,
    pub decision: Decision,
}

pub struct App {
    pub theme: Theme,
    pub mode: Mode,
    pub conn_state: ConnState,
    pub dsn: Option<Dsn>,
    pub grid: Grid,
    pub grid_state: TableState,
    pub splash_visible: bool,
    pub anim_tick: usize,
    pub generation: u64,
    pub should_quit: bool,

    /// SQL editor buffer (single line for v1; multi-line is a follow-up).
    pub editor_buffer: String,
    /// Byte offset of the cursor within `editor_buffer`.
    pub editor_cursor: usize,
    /// A guarded run waiting on confirmation.
    pub pending_run: Option<PendingRun>,
    /// A short status line shown in the footer after a run (e.g. "EXPLAIN ok").
    pub last_status: Option<String>,
    /// A query / safety error to surface to the user.
    pub last_error: Option<String>,
    /// True while a query is in flight (drives the spinner).
    pub query_running: bool,

    client: Option<Arc<tokio_postgres::Client>>,
    safety_config: SafetyConfig,
    read_only: bool,
    statement_timeout_ms: u64,
    msg_tx: UnboundedSender<AppMsg>,
    msg_rx: Option<UnboundedReceiver<AppMsg>>,
}

impl App {
    pub fn new(theme: Theme, dsn: Option<Dsn>, safety_config: SafetyConfig) -> Self {
        let db = dsn
            .as_ref()
            .map(|d| d.dbname.as_str())
            .unwrap_or("default");
        let profile = safety_config.profile_for(db);
        let read_only = profile.read_only;
        let statement_timeout_ms = profile.statement_timeout_ms;
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        Self {
            theme,
            mode: Mode::Normal,
            conn_state: ConnState::Disconnected,
            dsn,
            grid: Grid::default(),
            grid_state: TableState::default(),
            splash_visible: true,
            anim_tick: 0,
            generation: 0,
            should_quit: false,
            editor_buffer: String::new(),
            editor_cursor: 0,
            pending_run: None,
            last_status: None,
            last_error: None,
            query_running: false,
            client: None,
            safety_config,
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
        // One frame clock for all animation sources. Gated by `wants_animation`
        // so an idle, connected app does no work.
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
                    self.anim_tick = self.anim_tick.wrapping_add(1);
                }
                Some(msg) = msg_rx.recv() => {
                    self.on_msg(msg);
                }
            }
        }
        Ok(())
    }

    /// Whether the frame clock should keep ticking — for the connecting and
    /// running spinners.
    fn wants_animation(&self) -> bool {
        self.query_running || matches!(self.conn_state, ConnState::Connecting)
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
                    client: b.client,
                },
                Err(error) => AppMsg::BootFailed { generation, error },
            };
            let _ = tx.send(msg);
        });
    }

    /// Apply a finished message from a spawned task.
    fn on_msg(&mut self, msg: AppMsg) {
        if msg.generation() != self.generation {
            tracing::debug!("dropping stale message from generation {}", msg.generation());
            return;
        }
        match msg {
            AppMsg::Booted {
                server_version,
                grid,
                client,
                ..
            } => {
                self.conn_state = ConnState::Connected { server_version };
                self.client = Some(client);
                self.grid = grid;
                self.grid_state
                    .select(if self.grid.is_empty() { None } else { Some(0) });
                self.splash_visible = false; // connection-ready dismisses splash
            }
            AppMsg::BootFailed { error, .. } => {
                self.conn_state = ConnState::Failed(error);
                self.splash_visible = false;
            }
            AppMsg::QueryOk {
                grid, kind_label, ..
            } => {
                self.grid = grid;
                self.grid_state
                    .select(if self.grid.is_empty() { None } else { Some(0) });
                self.query_running = false;
                self.last_error = None;
                self.last_status = Some(format!("{kind_label} ok · {} row(s)", self.grid.row_count()));
            }
            AppMsg::QueryFailed { error, .. } => {
                self.query_running = false;
                self.last_status = None;
                self.last_error = Some(error);
            }
        }
    }

    fn on_event(&mut self, ev: Event) {
        if let Event::Key(key) = ev {
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
            Mode::Confirm => self.on_confirm_key(key),
            Mode::Editor => self.on_editor_key(key),
            Mode::Normal => self.on_normal_key(key),
        }
    }

    fn on_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('e') | KeyCode::Char('i') | KeyCode::Tab => {
                self.mode = Mode::Editor;
            }
            KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select_row(0),
            KeyCode::Char('G') | KeyCode::End => {
                self.select_row(self.grid.row_count().saturating_sub(1));
            }
            _ => {}
        }
    }

    fn on_editor_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::F(5) | KeyCode::Enter => self.request_run(RunKind::Run),
            KeyCode::F(6) => self.request_run(RunKind::Explain),
            KeyCode::F(7) => self.request_run(RunKind::ExplainAnalyze),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.editor_buffer.clear();
                self.editor_cursor = 0;
            }
            // Only insert plain typing — ignore Ctrl-* / Alt-* combos so they
            // don't drop a character into the buffer.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                editor_insert(&mut self.editor_buffer, &mut self.editor_cursor, c);
            }
            KeyCode::Backspace => {
                editor_backspace(&mut self.editor_buffer, &mut self.editor_cursor);
            }
            KeyCode::Delete => {
                editor_delete(&mut self.editor_buffer, &mut self.editor_cursor);
            }
            KeyCode::Left => editor_move_left(&self.editor_buffer, &mut self.editor_cursor),
            KeyCode::Right => editor_move_right(&self.editor_buffer, &mut self.editor_cursor),
            KeyCode::Home => self.editor_cursor = 0,
            KeyCode::End => self.editor_cursor = self.editor_buffer.len(),
            _ => {}
        }
    }

    fn on_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(pending) = self.pending_run.take() {
                    self.spawn_run(pending.sql, pending.kind, pending.decision);
                }
                self.mode = Mode::Editor;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.pending_run = None;
                self.mode = Mode::Editor;
                self.last_status = Some("cancelled".to_string());
            }
            _ => {}
        }
    }

    // -- run dispatch --

    /// User requested a run. Classify, evaluate safety, and either run, prompt,
    /// or reject.
    fn request_run(&mut self, kind: RunKind) {
        let sql = self.editor_buffer.trim().to_string();
        if sql.is_empty() {
            self.last_error = Some("editor is empty".to_string());
            return;
        }
        if self.client.is_none() {
            self.last_error = Some("not connected".to_string());
            return;
        }

        let db = self
            .dsn
            .as_ref()
            .map(|d| d.dbname.as_str())
            .unwrap_or("default");
        let decision = safety::evaluate(&self.safety_config, db, &sql);

        // EXPLAIN (without ANALYZE) never executes the inner statement — bypass
        // guards entirely.
        if kind == RunKind::Explain {
            self.spawn_run(sql, kind, decision);
            return;
        }

        // Run / EXPLAIN ANALYZE both execute (ANALYZE on DML is wrapped in a
        // rollback transaction inside `spawn_run`).
        match decision.guard {
            Guard::Block => {
                self.last_error = Some(format!(
                    "blocked by safety: {:?} on '{db}'",
                    decision.kind
                ));
            }
            Guard::Confirm => {
                self.pending_run = Some(PendingRun {
                    sql,
                    kind,
                    decision,
                });
                self.mode = Mode::Confirm;
            }
            Guard::Allow => self.spawn_run(sql, kind, decision),
        }
    }

    fn spawn_run(&mut self, sql: String, kind: RunKind, decision: Decision) {
        let Some(client) = self.client.clone() else {
            self.last_error = Some("not connected".to_string());
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        self.query_running = true;
        self.last_error = None;
        self.last_status = Some(format!("running {}…", kind.label()));
        tokio::spawn(async move {
            let result = execute(&client, &sql, kind, &decision).await;
            let msg = match result {
                Ok(grid) => AppMsg::QueryOk {
                    generation,
                    grid,
                    kind_label: kind.label().to_string(),
                },
                Err(error) => AppMsg::QueryFailed { generation, error },
            };
            let _ = tx.send(msg);
        });
    }

    // -- grid nav --

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

// -- editor buffer ops (pure; tested) --

/// Insert a character at `*cursor`, advancing the cursor by the character's
/// UTF-8 length.
fn editor_insert(buffer: &mut String, cursor: &mut usize, c: char) {
    buffer.insert(*cursor, c);
    *cursor += c.len_utf8();
}

/// Delete the character before the cursor (Backspace).
fn editor_backspace(buffer: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let mut prev = *cursor - 1;
    while !buffer.is_char_boundary(prev) {
        prev -= 1;
    }
    buffer.replace_range(prev..*cursor, "");
    *cursor = prev;
}

/// Delete the character at the cursor (Delete / Del).
fn editor_delete(buffer: &mut String, cursor: &mut usize) {
    if *cursor >= buffer.len() {
        return;
    }
    let mut next = *cursor + 1;
    while next < buffer.len() && !buffer.is_char_boundary(next) {
        next += 1;
    }
    buffer.replace_range(*cursor..next, "");
}

/// Move the cursor one character left, respecting UTF-8 boundaries.
fn editor_move_left(buffer: &str, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let mut prev = *cursor - 1;
    while !buffer.is_char_boundary(prev) {
        prev -= 1;
    }
    *cursor = prev;
}

/// Move the cursor one character right, respecting UTF-8 boundaries.
fn editor_move_right(buffer: &str, cursor: &mut usize) {
    if *cursor >= buffer.len() {
        return;
    }
    let mut next = *cursor + 1;
    while next < buffer.len() && !buffer.is_char_boundary(next) {
        next += 1;
    }
    *cursor = next;
}

/// Build and run the effective SQL for `kind`, honouring the safety decision.
async fn execute(
    client: &tokio_postgres::Client,
    sql: &str,
    kind: RunKind,
    decision: &Decision,
) -> Result<Grid, String> {
    match kind {
        RunKind::Run => {
            if decision.wrap_in_tx {
                conn::run_in_tx_commit(client, sql).await
            } else {
                conn::run_statement(client, sql).await
            }
        }
        RunKind::Explain => {
            let wrapped = format!("EXPLAIN {sql}");
            conn::run_statement(client, &wrapped).await
        }
        RunKind::ExplainAnalyze => {
            let wrapped = format!("EXPLAIN ANALYZE {sql}");
            if decision.kind.is_write() {
                // The DML inside EXPLAIN ANALYZE actually runs — wrap and
                // rollback so it never lands.
                conn::run_in_tx_rollback(client, &wrapped).await
            } else {
                conn::run_statement(client, &wrapped).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_insert_advances_cursor_by_utf8_length() {
        let mut buf = String::from("ab");
        let mut cur = 1;
        editor_insert(&mut buf, &mut cur, 'X');
        assert_eq!(buf, "aXb");
        assert_eq!(cur, 2);

        // Multi-byte char: 'é' is 2 bytes.
        editor_insert(&mut buf, &mut cur, 'é');
        assert_eq!(buf, "aXéb");
        assert_eq!(cur, 4);
    }

    #[test]
    fn editor_backspace_steps_to_a_char_boundary() {
        let mut buf = String::from("aé"); // a=1 byte, é=2 bytes
        let mut cur = buf.len(); // 3
        editor_backspace(&mut buf, &mut cur);
        assert_eq!(buf, "a");
        assert_eq!(cur, 1);
        editor_backspace(&mut buf, &mut cur);
        assert_eq!(buf, "");
        assert_eq!(cur, 0);
        // Backspace at start is a no-op.
        editor_backspace(&mut buf, &mut cur);
        assert_eq!(cur, 0);
    }

    #[test]
    fn editor_delete_steps_to_a_char_boundary() {
        let mut buf = String::from("éb"); // é=2 bytes, b=1
        let mut cur = 0;
        editor_delete(&mut buf, &mut cur);
        assert_eq!(buf, "b");
        assert_eq!(cur, 0);
        // Delete at end is a no-op.
        let mut buf = String::from("ab");
        let mut cur = 2;
        editor_delete(&mut buf, &mut cur);
        assert_eq!(buf, "ab");
        assert_eq!(cur, 2);
    }

    #[test]
    fn editor_move_left_and_right_respect_utf8_boundaries() {
        let buf = String::from("aéb"); // bytes: a(1), é(2), b(1) = 4 bytes
        let mut cur = buf.len();
        editor_move_left(&buf, &mut cur);
        assert_eq!(cur, 3); // before 'b'
        editor_move_left(&buf, &mut cur);
        assert_eq!(cur, 1); // before 'é'
        editor_move_left(&buf, &mut cur);
        assert_eq!(cur, 0);
        editor_move_left(&buf, &mut cur);
        assert_eq!(cur, 0); // saturates
        editor_move_right(&buf, &mut cur);
        assert_eq!(cur, 1);
        editor_move_right(&buf, &mut cur);
        assert_eq!(cur, 3); // past 'é'
    }
}
