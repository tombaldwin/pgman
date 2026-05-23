//! Application state and the event loop.

pub mod msg;

use crate::app::msg::AppMsg;
use crate::conn::{self, Dsn};
use crate::grid::Grid;
use crate::query::{self, reconstruct::ReconstructedQuery};
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
    /// An auto-tx write just succeeded; awaiting commit/rollback.
    TxDecision,
    /// Picking a reconstructed query from a parsed log.
    LogPick,
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

    /// SQL editor buffer; `\n` separates lines.
    pub editor_buffer: String,
    /// Byte offset of the cursor within `editor_buffer`.
    pub editor_cursor: usize,
    /// Remembered char-column for vertical motion (Up/Down). `None` outside a
    /// vertical-motion run; cleared by any other edit or horizontal move.
    pub editor_preferred_col: Option<usize>,
    /// Past run statements, newest at the end.
    pub history: Vec<String>,
    /// Position in `history` while navigating with Ctrl-P/Ctrl-N. `None` =
    /// editing the live draft.
    pub history_pos: Option<usize>,
    /// A guarded run waiting on confirmation.
    pub pending_run: Option<PendingRun>,
    /// True while an explicit transaction is open (auto_tx write succeeded —
    /// waiting on the user to commit or rollback).
    pub tx_open: bool,
    /// Reconstructed queries from the most recent log-import; `Mode::LogPick`
    /// browses these.
    pub log_picks: Vec<ReconstructedQuery>,
    /// Selected entry in `log_picks`.
    pub log_pick_index: usize,
    /// A short status line shown in the footer after a run (e.g. "EXPLAIN ok").
    pub last_status: Option<String>,
    /// A query / safety error to surface to the user.
    pub last_error: Option<String>,
    /// True while a query is in flight (drives the spinner).
    pub query_running: bool,

    /// Saved working buffer while navigating history (restored on Ctrl-N past
    /// the newest entry).
    history_draft: String,
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
            editor_preferred_col: None,
            history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            pending_run: None,
            tx_open: false,
            log_picks: Vec::new(),
            log_pick_index: 0,
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
                grid,
                kind_label,
                tx_open_after,
                ..
            } => {
                self.grid = grid;
                self.grid_state
                    .select(if self.grid.is_empty() { None } else { Some(0) });
                self.query_running = false;
                self.last_error = None;
                self.last_status = Some(format!(
                    "{kind_label} ok · {} row(s)",
                    self.grid.row_count()
                ));
                if tx_open_after {
                    self.tx_open = true;
                    self.mode = Mode::TxDecision;
                }
            }
            AppMsg::QueryFailed { error, .. } => {
                self.query_running = false;
                self.last_status = None;
                self.last_error = Some(error);
            }
            AppMsg::TxClosed {
                committed, error, ..
            } => {
                self.tx_open = false;
                self.query_running = false;
                self.mode = Mode::Editor;
                match error {
                    Some(e) => self.last_error = Some(format!("tx close failed: {e}")),
                    None => {
                        self.last_status = Some(
                            if committed {
                                "committed"
                            } else {
                                "rolled back"
                            }
                            .to_string(),
                        );
                    }
                }
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
            Mode::TxDecision => self.on_tx_decision_key(key),
            Mode::LogPick => self.on_log_pick_key(key),
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            // Run keys (Ctrl-* primary; F-keys are aliases for full-keyboard
            // users — F-keys on a MacBook need fn+). Enter inserts a newline.
            KeyCode::Char('r') if ctrl => self.request_run(RunKind::Run),
            KeyCode::Char('e') if ctrl => self.request_run(RunKind::Explain),
            KeyCode::Char('a') if ctrl => self.request_run(RunKind::ExplainAnalyze),
            KeyCode::Char('l') if ctrl => self.start_log_import(),
            KeyCode::F(5) => self.request_run(RunKind::Run),
            KeyCode::F(6) => self.request_run(RunKind::Explain),
            KeyCode::F(7) => self.request_run(RunKind::ExplainAnalyze),
            KeyCode::F(8) => self.start_log_import(),

            // History navigation.
            KeyCode::Char('p') if ctrl => self.history_prev(),
            KeyCode::Char('n') if ctrl => self.history_next(),
            KeyCode::Char('u') if ctrl => {
                self.editor_buffer.clear();
                self.editor_cursor = 0;
                self.editor_dirty();
            }

            // Plain typing — only when no Ctrl/Alt.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.editor_dirty();
                editor_insert(&mut self.editor_buffer, &mut self.editor_cursor, c);
            }
            KeyCode::Enter => {
                self.editor_dirty();
                editor_insert(&mut self.editor_buffer, &mut self.editor_cursor, '\n');
            }
            KeyCode::Backspace => {
                self.editor_dirty();
                editor_backspace(&mut self.editor_buffer, &mut self.editor_cursor);
            }
            KeyCode::Delete => {
                self.editor_dirty();
                editor_delete(&mut self.editor_buffer, &mut self.editor_cursor);
            }
            KeyCode::Left => {
                self.editor_preferred_col = None;
                editor_move_left(&self.editor_buffer, &mut self.editor_cursor);
            }
            KeyCode::Right => {
                self.editor_preferred_col = None;
                editor_move_right(&self.editor_buffer, &mut self.editor_cursor);
            }
            KeyCode::Up => {
                editor_move_up(
                    &self.editor_buffer,
                    &mut self.editor_cursor,
                    &mut self.editor_preferred_col,
                );
            }
            KeyCode::Down => {
                editor_move_down(
                    &self.editor_buffer,
                    &mut self.editor_cursor,
                    &mut self.editor_preferred_col,
                );
            }
            KeyCode::Home => {
                self.editor_preferred_col = None;
                self.editor_cursor = line_start_byte(&self.editor_buffer, self.editor_cursor);
            }
            KeyCode::End => {
                self.editor_preferred_col = None;
                self.editor_cursor = line_end_byte(&self.editor_buffer, self.editor_cursor);
            }
            _ => {}
        }
    }

    /// Any edit / non-vertical motion exits history navigation and resets
    /// preferred-column tracking.
    fn editor_dirty(&mut self) {
        self.history_pos = None;
        self.editor_preferred_col = None;
    }

    /// Step back into older history (Ctrl-P). The first step saves the live
    /// draft so Ctrl-N past the newest entry can restore it.
    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let new_pos = match self.history_pos {
            None => {
                self.history_draft = self.editor_buffer.clone();
                self.history.len() - 1
            }
            Some(i) if i > 0 => i - 1,
            Some(_) => return,
        };
        self.history_pos = Some(new_pos);
        self.editor_buffer = self.history[new_pos].clone();
        self.editor_cursor = self.editor_buffer.len();
        self.editor_preferred_col = None;
    }

    /// Step forward into newer history (Ctrl-N). Past the newest entry,
    /// restores the saved draft.
    fn history_next(&mut self) {
        let Some(pos) = self.history_pos else {
            return;
        };
        if pos + 1 < self.history.len() {
            self.history_pos = Some(pos + 1);
            self.editor_buffer = self.history[pos + 1].clone();
        } else {
            self.editor_buffer = std::mem::take(&mut self.history_draft);
            self.history_pos = None;
        }
        self.editor_cursor = self.editor_buffer.len();
        self.editor_preferred_col = None;
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

    /// Tx-open prompt: `y` commits, `n` / `esc` rolls back.
    fn on_tx_decision_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.close_tx(true),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.close_tx(false),
            _ => {}
        }
    }

    /// Spawn a `COMMIT` or `ROLLBACK` of the open transaction.
    fn close_tx(&mut self, commit: bool) {
        let Some(client) = self.client.clone() else {
            self.tx_open = false;
            self.mode = Mode::Editor;
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        self.query_running = true;
        self.last_status = Some(if commit {
            "committing…".to_string()
        } else {
            "rolling back…".to_string()
        });
        tokio::spawn(async move {
            let result = if commit {
                conn::tx_commit(&client).await
            } else {
                conn::tx_rollback(&client).await
            };
            let _ = tx.send(AppMsg::TxClosed {
                generation,
                committed: commit,
                error: result.err(),
            });
        });
    }

    /// F8 in editor mode — parse the editor buffer through `hibernate::parse`
    /// and `pglog::parse`, then enter `Mode::LogPick` if anything was found.
    fn start_log_import(&mut self) {
        let log = &self.editor_buffer;
        let mut picks: Vec<ReconstructedQuery> = Vec::new();
        picks.extend(query::hibernate::parse(log));
        picks.extend(query::pglog::parse(log));
        if picks.is_empty() {
            self.last_error = Some(
                "no queries found (paste a Hibernate or Postgres log into the editor first)"
                    .to_string(),
            );
            return;
        }
        self.last_error = None;
        self.last_status = Some(format!("{} pick(s) found", picks.len()));
        self.log_picks = picks;
        self.log_pick_index = 0;
        self.mode = Mode::LogPick;
    }

    /// Log-pick browser: j/k navigate, Enter loads the selection into the
    /// editor, Esc cancels.
    fn on_log_pick_key(&mut self, key: KeyEvent) {
        let last = self.log_picks.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.log_picks.clear();
                self.mode = Mode::Editor;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.log_pick_index = (self.log_pick_index + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.log_pick_index = self.log_pick_index.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.log_pick_index = 0,
            KeyCode::Char('G') | KeyCode::End => self.log_pick_index = last,
            KeyCode::Enter => {
                if let Some(pick) = self.log_picks.get(self.log_pick_index) {
                    self.editor_buffer = pick.runnable_sql.clone();
                    self.editor_cursor = self.editor_buffer.len();
                    self.editor_preferred_col = None;
                    self.history_pos = None;
                    self.last_status = Some(format!(
                        "loaded query · {} char(s)",
                        self.editor_buffer.len()
                    ));
                }
                self.log_picks.clear();
                self.mode = Mode::Editor;
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
        // Push to history (skip consecutive duplicates, cap at 50 entries).
        if self.history.last() != Some(&sql) {
            self.history.push(sql.clone());
            if self.history.len() > 50 {
                self.history.remove(0);
            }
        }
        self.history_pos = None;
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let wrap_in_tx = decision.wrap_in_tx;
        let is_run = matches!(kind, RunKind::Run);
        self.query_running = true;
        self.last_error = None;
        self.last_status = Some(format!("running {}…", kind.label()));
        tokio::spawn(async move {
            let result = execute(&client, &sql, kind, &decision).await;
            // Run + wrap_in_tx leaves the transaction open on success — the
            // caller will need to commit or rollback.
            let tx_open_after = is_run && wrap_in_tx && result.is_ok();
            let msg = match result {
                Ok(grid) => AppMsg::QueryOk {
                    generation,
                    grid,
                    kind_label: kind.label().to_string(),
                    tx_open_after,
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

/// Move the cursor up one line, preserving the preferred char-column.
fn editor_move_up(buffer: &str, cursor: &mut usize, preferred_col: &mut Option<usize>) {
    let (line, col) = cursor_position(buffer, *cursor);
    if line == 0 {
        return;
    }
    let target = preferred_col.unwrap_or(col);
    *preferred_col = Some(target);
    *cursor = byte_offset_at_line_col(buffer, line - 1, target);
}

/// Move the cursor down one line, preserving the preferred char-column.
fn editor_move_down(buffer: &str, cursor: &mut usize, preferred_col: &mut Option<usize>) {
    let (line, col) = cursor_position(buffer, *cursor);
    let total_lines = buffer.matches('\n').count() + 1;
    if line + 1 >= total_lines {
        return;
    }
    let target = preferred_col.unwrap_or(col);
    *preferred_col = Some(target);
    *cursor = byte_offset_at_line_col(buffer, line + 1, target);
}

/// `(line_index, char_column)` of `cursor` within `buffer`.
pub(crate) fn cursor_position(buffer: &str, cursor: usize) -> (usize, usize) {
    let prefix = &buffer[..cursor.min(buffer.len())];
    let mut line = 0;
    let mut col = 0;
    for c in prefix.chars() {
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Byte offset for `(line, char_col)`. Clamps to line end / buffer end if the
/// position is past the line / past the buffer.
fn byte_offset_at_line_col(buffer: &str, line: usize, col: usize) -> usize {
    let mut current_line = 0;
    let mut line_start = 0;
    if line > 0 {
        for (i, c) in buffer.char_indices() {
            if c == '\n' {
                current_line += 1;
                if current_line == line {
                    line_start = i + 1;
                    break;
                }
            }
        }
        if current_line < line {
            return buffer.len();
        }
    }
    for (count, (i, c)) in buffer[line_start..].char_indices().enumerate() {
        if c == '\n' || count == col {
            return line_start + i;
        }
    }
    buffer.len()
}

/// Byte offset of the start of the line containing `cursor`.
fn line_start_byte(buffer: &str, cursor: usize) -> usize {
    let mut i = cursor.min(buffer.len());
    while i > 0 && buffer.as_bytes()[i - 1] != b'\n' {
        i -= 1;
    }
    i
}

/// Byte offset of the end of the line containing `cursor` (before the `\n`).
fn line_end_byte(buffer: &str, cursor: usize) -> usize {
    let mut i = cursor.min(buffer.len());
    while i < buffer.len() && buffer.as_bytes()[i] != b'\n' {
        i += 1;
    }
    i
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
                // BEGIN + run; transaction is LEFT OPEN on success so the
                // app's commit/rollback prompt can decide what happens next.
                conn::run_in_tx_open(client, sql).await
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

    #[test]
    fn cursor_position_walks_newlines() {
        assert_eq!(cursor_position("hello", 3), (0, 3));
        let buf = "abc\nde\nf";
        assert_eq!(cursor_position(buf, 0), (0, 0));
        assert_eq!(cursor_position(buf, 3), (0, 3));
        assert_eq!(cursor_position(buf, 4), (1, 0));
        assert_eq!(cursor_position(buf, 6), (1, 2));
        assert_eq!(cursor_position(buf, 7), (2, 0));
        assert_eq!(cursor_position(buf, 8), (2, 1));
    }

    #[test]
    fn byte_offset_at_line_col_clamps_past_line_end() {
        let buf = "abc\nde\nf";
        assert_eq!(byte_offset_at_line_col(buf, 0, 0), 0);
        assert_eq!(byte_offset_at_line_col(buf, 0, 3), 3);
        assert_eq!(byte_offset_at_line_col(buf, 1, 0), 4);
        assert_eq!(byte_offset_at_line_col(buf, 1, 99), 6); // clamps to line end
        assert_eq!(byte_offset_at_line_col(buf, 2, 0), 7);
        assert_eq!(byte_offset_at_line_col(buf, 5, 0), 8); // line out of range
    }

    #[test]
    fn editor_move_up_down_track_preferred_column() {
        let buf = String::from("abc\nde\nfgh");
        // Start at end of "fgh" (line 2, col 3).
        let mut cur = buf.len();
        let mut pref = None;
        editor_move_up(&buf, &mut cur, &mut pref);
        // Line 1 is "de" — only 2 cols, so cursor clamps to its end.
        assert_eq!(cur, 6);
        assert_eq!(pref, Some(3));
        editor_move_up(&buf, &mut cur, &mut pref);
        // Line 0 is "abc" — 3 cols, preferred 3 lands at the end.
        assert_eq!(cur, 3);
        editor_move_down(&buf, &mut cur, &mut pref);
        assert_eq!(cur, 6); // back to "de" end, preferred still 3
        editor_move_down(&buf, &mut cur, &mut pref);
        assert_eq!(cur, buf.len()); // "fgh" end (col 3)
        editor_move_down(&buf, &mut cur, &mut pref);
        assert_eq!(cur, buf.len()); // no further down — no change
    }

    #[test]
    fn line_start_and_end_bytes_find_line_edges() {
        let buf = "abc\nde\nf";
        // cursor in the middle of "de" (byte 5)
        assert_eq!(line_start_byte(buf, 5), 4);
        assert_eq!(line_end_byte(buf, 5), 6);
        // cursor on line 0
        assert_eq!(line_start_byte(buf, 2), 0);
        assert_eq!(line_end_byte(buf, 2), 3);
        // cursor at last char
        assert_eq!(line_start_byte(buf, 8), 7);
        assert_eq!(line_end_byte(buf, 8), 8);
    }
}
