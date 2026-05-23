//! Application state and the event loop.

pub mod msg;

use crate::app::msg::AppMsg;
use crate::conn::{self, Dsn};
use crate::grid::Grid;
use crate::query::complete::{self as complete_q, Candidate};
use crate::query::schema::SchemaCache;
use crate::query::{self, reconstruct::ReconstructedQuery};
use crate::safety::{self, Decision, Guard, SafetyConfig};
use crate::theme::Theme;
use crate::tui::Tui;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::widgets::TableState;
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    /// Picking a connection to open from a list of detected data sources
    /// (e.g. multiple IntelliJ `.idea/dataSources.xml` entries) at startup.
    ConnPick,
    /// Expanded view of the currently-selected grid row — one line per
    /// column, long values wrapped. Read-only modal.
    RowDetail,
    /// "About pgman" overlay — version + tagline + credits. Same info as
    /// the splash, but accessible at any time.
    About,
    /// Focused single-cell view, opened from `RowDetail` with Enter on a
    /// field. Shows just that one (column, value) pair with the value
    /// wrapped to popup width and scrollable — useful for JSON / large
    /// text fields that overflow the row-detail card.
    CellDetail,
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

/// Active editor Tab-completion cycle. Built fresh when the user starts
/// cycling; cleared by any non-Tab editor key so a typo-and-retry reverts
/// the editor to a clean draft state.
#[derive(Debug, Clone)]
pub struct CompletionCycle {
    /// Byte offset where the partial identifier started — the renderer
    /// replaces `[start, end)` each step with the active candidate.
    pub start: usize,
    /// Byte offset of the end of the current insertion. Equals
    /// `start + candidates[index].insert.len()` after each step;
    /// before the first step equals the cursor at cycle-start.
    pub end: usize,
    /// The original buffer text spanning `[start, original_end)` at
    /// cycle-start — i.e. the whole identifier the user was on top of,
    /// including any chars trailing the cursor. Esc restores this so a
    /// mid-word Tab can be cleanly undone.
    pub origin: String,
    /// The prefix the user had typed BEFORE the cursor when the cycle
    /// began. Footer shows `no matches for {prefix}` etc; the prefix is
    /// a strict prefix of `origin`.
    pub origin_prefix: String,
    /// Byte offset of the cursor at cycle-start (so Esc-restore can
    /// put the cursor back exactly where Tab found it).
    pub origin_cursor: usize,
    /// Candidates in display order.
    pub candidates: Vec<Candidate>,
    /// Which candidate is currently inserted. `0` for the first step.
    pub index: usize,
}

/// A data source the user can pick at startup. Built from external sources
/// (IntelliJ `.idea/dataSources.xml` today; Spring `application*.yml` later)
/// before the TUI starts; surfaced via `Mode::ConnPick` when there's more
/// than one candidate.
#[derive(Debug, Clone)]
pub struct DataSourcePick {
    /// Human label shown in the picker (e.g. the `<data-source name="…">`).
    pub name: String,
    /// Where this pick came from, for the operator's benefit
    /// (e.g. "IntelliJ" / "Spring").
    pub origin: &'static str,
    /// Resolved DSN, ready to hand to `connect_and_bootstrap`.
    pub dsn: Dsn,
}

/// A run waiting on user confirmation (the safety guard returned `Confirm`).
#[derive(Debug, Clone)]
pub struct PendingRun {
    pub sql: String,
    pub kind: RunKind,
    pub decision: Decision,
    /// True when `sql` is a multi-statement script — run via
    /// `client.batch_execute` rather than the single-statement path.
    pub is_batch: bool,
    /// For multi-statement runs, a human summary shown in the confirm modal
    /// in place of the (less useful for batches) classification.
    pub summary: Option<String>,
}

pub struct App {
    pub theme: Theme,
    pub mode: Mode,
    pub conn_state: ConnState,
    pub dsn: Option<Dsn>,
    /// Where the current DSN came from — surfaced in the failure view to
    /// help the operator answer "wait, which connection did pgman just try?"
    /// Examples: "--dsn flag", "auto-picked IntelliJ data source 'prod'".
    /// Set wherever `dsn` is set; `None` when the operator hasn't picked yet.
    pub dsn_origin: Option<String>,
    pub grid: Grid,
    pub grid_state: TableState,
    pub splash_visible: bool,
    /// Earliest moment at which the splash may auto-dismiss. The splash
    /// always shows for at least this long at startup, regardless of how
    /// quickly the connection completes — so the elephant gets its moment
    /// even on a fast local DB. A keypress still dismisses immediately.
    pub splash_until: Option<Instant>,
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
    /// Candidate data sources surfaced at startup. Populated when the operator
    /// didn't pass `--dsn` and we found multiple sources via discovery (e.g.
    /// IntelliJ). Drives `Mode::ConnPick`.
    pub data_source_picks: Vec<DataSourcePick>,
    /// Selected entry in `data_source_picks`.
    pub data_source_pick_index: usize,

    /// Vertical scroll offset for the help overlay (number of leading lines
    /// hidden above the viewport).
    pub help_scroll: u16,
    /// Last-rendered max scroll for the help overlay. Written by `draw_help`
    /// each frame and read by the j/k handler so an incremental scroll past
    /// the bottom doesn't accumulate phantom offsets.
    pub help_max_scroll: u16,
    /// Scroll / clamp state for the row-detail modal — same shape as
    /// `help_scroll` / `help_max_scroll`. `row_detail_scroll` is normally
    /// driven by the renderer's auto-scroll (so the focused field stays in
    /// view); the key handler only nudges it as a side-effect of moving
    /// `row_detail_field`.
    pub row_detail_scroll: u16,
    pub row_detail_max_scroll: u16,
    /// Scroll / clamp state for the per-cell zoom view (`Mode::CellDetail`).
    pub cell_detail_scroll: u16,
    pub cell_detail_max_scroll: u16,
    /// Currently-focused field (column index) inside the row-detail modal.
    /// Bounded by `row_detail_field_count` which the renderer writes each
    /// frame (it's just `grid.columns.len()` today, but kept as a separate
    /// field so the clamp matches what's actually rendered).
    pub row_detail_field: usize,
    pub row_detail_field_count: usize,

    /// Snapshot of the database catalog used by Tab-completion in the
    /// editor. Refilled on every successful `Booted`. Empty before
    /// connect (or after a failed catalog fetch).
    pub schema_cache: SchemaCache,
    /// Active completion cycle, when the user has pressed Tab one or
    /// more times in a row. Reset on any non-Tab editor keypress so a
    /// subsequent Tab starts a fresh cycle from the current cursor.
    pub completion: Option<CompletionCycle>,

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
    pub fn new(
        theme: Theme,
        dsn: Option<Dsn>,
        data_source_picks: Vec<DataSourcePick>,
        safety_config: SafetyConfig,
    ) -> Self {
        let db = dsn
            .as_ref()
            .map(|d| d.dbname.as_str())
            .unwrap_or("default");
        let profile = safety_config.profile_for(db);
        let read_only = profile.read_only;
        let statement_timeout_ms = profile.statement_timeout_ms;
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        // When the operator hasn't pre-selected a DSN and we discovered
        // multiple candidates, the post-splash mode is the picker — that's
        // the "lovely discovery" surface. Splash always shows first
        // (`splash_until` keeps it visible for ~3s minimum); when it
        // dismisses the user lands on either the picker or Normal mode.
        let show_picker = dsn.is_none() && data_source_picks.len() >= 2;
        let mode = if show_picker { Mode::ConnPick } else { Mode::Normal };
        Self {
            theme,
            mode,
            conn_state: ConnState::Disconnected,
            dsn,
            dsn_origin: None,
            grid: Grid::default(),
            grid_state: TableState::default(),
            splash_visible: true,
            splash_until: Some(Instant::now() + Duration::from_secs(3)),
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
            help_scroll: 0,
            help_max_scroll: 0,
            row_detail_scroll: 0,
            row_detail_max_scroll: 0,
            row_detail_field: 0,
            row_detail_field_count: 0,
            cell_detail_scroll: 0,
            cell_detail_max_scroll: 0,
            schema_cache: SchemaCache::default(),
            completion: None,
            data_source_picks,
            data_source_pick_index: 0,
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
            self.tick_splash();
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

    /// Auto-dismiss the splash once its minimum-display deadline has passed.
    /// Cheap to call every loop iteration — it's a single `Instant::now`.
    fn tick_splash(&mut self) {
        if !self.splash_visible {
            return;
        }
        if let Some(until) = self.splash_until {
            if Instant::now() >= until {
                self.splash_visible = false;
                self.splash_until = None;
            }
        }
    }

    /// Whether the frame clock should keep ticking — for the splash trunk /
    /// blink animation, the connecting spinner, and the in-flight-query
    /// spinner.
    fn wants_animation(&self) -> bool {
        self.splash_visible
            || self.query_running
            || matches!(self.mode, Mode::About)
            || matches!(self.conn_state, ConnState::Connecting)
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
                    schema_cache: b.schema_cache,
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
                schema_cache,
                ..
            } => {
                self.conn_state = ConnState::Connected { server_version };
                self.client = Some(client);
                self.grid = grid;
                self.grid_state
                    .select(if self.grid.is_empty() { None } else { Some(0) });
                self.schema_cache = schema_cache;
                // Splash stays up — `tick_splash` honours the 3s minimum.
            }
            AppMsg::BootFailed { error, .. } => {
                self.conn_state = ConnState::Failed(error);
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
        // Any key dismisses the splash and is otherwise consumed — gives
        // snappy users an instant skip past the 3s minimum.
        if self.splash_visible {
            self.splash_visible = false;
            self.splash_until = None;
            return;
        }

        match self.mode {
            Mode::Help => self.on_help_key(key),
            Mode::Confirm => self.on_confirm_key(key),
            Mode::TxDecision => self.on_tx_decision_key(key),
            Mode::LogPick => self.on_log_pick_key(key),
            Mode::ConnPick => self.on_conn_pick_key(key),
            Mode::RowDetail => self.on_row_detail_key(key),
            Mode::CellDetail => self.on_cell_detail_key(key),
            Mode::About => self.on_about_key(key),
            Mode::Editor => self.on_editor_key(key),
            Mode::Normal => self.on_normal_key(key),
        }
    }

    fn on_about_key(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'A')
        ) {
            self.mode = Mode::Normal;
        }
    }

    /// Open the expanded view of the currently-selected grid row. No-op
    /// when the grid is empty or nothing is selected.
    fn open_row_detail(&mut self) {
        let Some(idx) = self.grid_state.selected() else {
            return;
        };
        if self.grid.rows.get(idx).is_none() {
            return;
        }
        self.row_detail_scroll = 0;
        self.row_detail_field = 0;
        self.mode = Mode::RowDetail;
    }

    /// Row-detail modal: j/k navigate fields (renderer auto-scrolls so the
    /// focused field stays visible); g/G first/last field; PageUp/Down
    /// jump by 10 fields; `y` yanks the focused value; Enter zooms into
    /// the focused field (`Mode::CellDetail`); Esc/q close.
    fn on_row_detail_key(&mut self, key: KeyEvent) {
        let last = self.row_detail_field_count.saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.row_detail_scroll = 0;
            }
            KeyCode::Enter => self.open_cell_detail(),
            KeyCode::Char('j') | KeyCode::Down => {
                self.row_detail_field = (self.row_detail_field + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.row_detail_field = self.row_detail_field.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.row_detail_field = 0,
            KeyCode::Char('G') | KeyCode::End => self.row_detail_field = last,
            KeyCode::PageDown => {
                self.row_detail_field = (self.row_detail_field + 10).min(last);
            }
            KeyCode::PageUp => {
                self.row_detail_field = self.row_detail_field.saturating_sub(10);
            }
            KeyCode::Char('y') => self.yank_focused_field(),
            _ => {}
        }
    }

    /// Zoom into the currently-focused field. No-op when the row or
    /// field cursor is out of bounds.
    fn open_cell_detail(&mut self) {
        let Some(idx) = self.grid_state.selected() else {
            return;
        };
        let Some(row) = self.grid.rows.get(idx) else {
            return;
        };
        if row.get(self.row_detail_field).is_none() {
            return;
        }
        self.cell_detail_scroll = 0;
        self.mode = Mode::CellDetail;
    }

    /// Cell-detail modal: scroll the value, `y` yanks it (same shortcut
    /// as in RowDetail for muscle-memory), Esc/q/Enter pop back to the
    /// row view.
    fn on_cell_detail_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                self.mode = Mode::RowDetail;
                self.cell_detail_scroll = 0;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.cell_detail_scroll = self
                    .cell_detail_scroll
                    .saturating_add(1)
                    .min(self.cell_detail_max_scroll);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cell_detail_scroll = self.cell_detail_scroll.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.cell_detail_scroll = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.cell_detail_scroll = self.cell_detail_max_scroll;
            }
            KeyCode::PageDown => {
                self.cell_detail_scroll = self
                    .cell_detail_scroll
                    .saturating_add(10)
                    .min(self.cell_detail_max_scroll);
            }
            KeyCode::PageUp => {
                self.cell_detail_scroll = self.cell_detail_scroll.saturating_sub(10);
            }
            KeyCode::Char('y') => self.yank_focused_field(),
            _ => {}
        }
    }

    /// Copy the currently-focused field's value to the system clipboard.
    /// Surfaces success / failure via `last_status` / `last_error`.
    fn yank_focused_field(&mut self) {
        let Some(idx) = self.grid_state.selected() else {
            return;
        };
        let Some(row) = self.grid.rows.get(idx) else {
            return;
        };
        let Some(value) = row.get(self.row_detail_field) else {
            return;
        };
        let column = self
            .grid
            .columns
            .get(self.row_detail_field)
            .cloned()
            .unwrap_or_default();
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(value.to_string())) {
            Ok(()) => {
                let chars = value.chars().count();
                self.last_status = Some(format!("yanked '{column}' · {chars} char(s)"));
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("yank failed: {e}"));
            }
        }
    }

    /// Connection picker (startup): j/k navigate, Enter selects + connects,
    /// Esc/q quits since there's nothing else to do without a connection.
    fn on_conn_pick_key(&mut self, key: KeyEvent) {
        let last = self.data_source_picks.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => {
                self.data_source_pick_index = (self.data_source_pick_index + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.data_source_pick_index = self.data_source_pick_index.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.data_source_pick_index = 0,
            KeyCode::Char('G') | KeyCode::End => self.data_source_pick_index = last,
            KeyCode::Enter => {
                if let Some(pick) = self.data_source_picks.get(self.data_source_pick_index) {
                    let dsn = pick.dsn.clone();
                    // Re-resolve safety profile against the *picked* db name
                    // — the placeholder in App::new used the empty default.
                    let profile = self.safety_config.profile_for(&dsn.dbname);
                    self.read_only = profile.read_only;
                    self.statement_timeout_ms = profile.statement_timeout_ms;
                    self.dsn = Some(dsn);
                    self.dsn_origin =
                        Some(format!("picked {} data source '{}'", pick.origin, pick.name));
                    self.mode = Mode::Normal;
                    self.start_connect();
                }
            }
            _ => {}
        }
    }

    fn on_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q' | '?') | KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.help_scroll = 0;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.help_scroll = self
                    .help_scroll
                    .saturating_add(1)
                    .min(self.help_max_scroll);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.help_scroll = 0;
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.help_scroll = self.help_max_scroll;
            }
            KeyCode::PageDown => {
                self.help_scroll = self
                    .help_scroll
                    .saturating_add(10)
                    .min(self.help_max_scroll);
            }
            KeyCode::PageUp => {
                self.help_scroll = self.help_scroll.saturating_sub(10);
            }
            _ => {}
        }
    }

    fn on_normal_key(&mut self, key: KeyEvent) {
        // Failure-screen shortcuts — only active while we're showing the
        // "connection failed" body. `r` retries the same DSN; `p` re-opens
        // the picker when we have data sources to choose from.
        if matches!(self.conn_state, ConnState::Failed(_)) {
            match key.code {
                KeyCode::Char('r') => {
                    if self.dsn.is_some() {
                        self.start_connect();
                    }
                    return;
                }
                KeyCode::Char('p') if !self.data_source_picks.is_empty() => {
                    self.mode = Mode::ConnPick;
                    self.data_source_pick_index = 0;
                    return;
                }
                _ => {}
            }
        }
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
            KeyCode::Enter => self.open_row_detail(),
            KeyCode::Char('A') => self.mode = Mode::About,
            _ => {}
        }
    }

    fn on_editor_key(&mut self, key: KeyEvent) {
        // Tab drives identifier completion — it's the only key that
        // reads the active cycle, so handle it before the universal
        // "non-Tab key cancels the cycle" reset below.
        if matches!(key.code, KeyCode::Tab)
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.editor_complete();
            return;
        }
        // Esc with an active cycle abandons completion *without* leaving
        // editor mode — restores the originally-typed prefix so the user
        // can keep typing. Without an active cycle, Esc still exits to
        // Normal (the existing behaviour) via the match below.
        if matches!(key.code, KeyCode::Esc) && self.completion.is_some() {
            self.editor_abandon_completion();
            return;
        }
        // Any other editor key abandons an in-progress completion cycle
        // so a typo-then-keep-typing reverts the editor to a clean
        // draft state (next Tab starts fresh from the new cursor).
        self.completion = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            // Run keys (Ctrl-* primary; F-keys are aliases for full-keyboard
            // users — F-keys on a MacBook need fn+). Enter inserts a newline.
            KeyCode::Char('r') if ctrl => self.request_run(RunKind::Run),
            KeyCode::Char('e') if ctrl => self.request_run(RunKind::Explain),
            KeyCode::Char('a') if ctrl => self.request_run(RunKind::ExplainAnalyze),
            KeyCode::Char('l') if ctrl => self.start_log_import(),
            KeyCode::Char('d') if ctrl => self.load_dbunit_fixture(),
            KeyCode::F(5) => self.request_run(RunKind::Run),
            KeyCode::F(6) => self.request_run(RunKind::Explain),
            KeyCode::F(7) => self.request_run(RunKind::ExplainAnalyze),
            KeyCode::F(8) => self.start_log_import(),
            KeyCode::F(9) => self.load_dbunit_fixture(),

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

    /// Abandon an active completion cycle: restore the original buffer
    /// text the cycle replaced (including any chars that trailed the
    /// cursor when Tab fired) and put the cursor back where it was when
    /// the user pressed Tab. No-op when no cycle is active.
    fn editor_abandon_completion(&mut self) {
        let Some(cycle) = self.completion.take() else {
            return;
        };
        self.editor_buffer
            .replace_range(cycle.start..cycle.end, &cycle.origin);
        self.editor_cursor = cycle.origin_cursor;
        self.last_status = Some("completion cancelled".to_string());
    }

    /// Tab-completion in the editor. First press starts a cycle from the
    /// partial identifier under the cursor; subsequent presses cycle
    /// through matches. Any non-Tab key drops the cycle (so a typo-then-
    /// keep-typing reverts cleanly).
    fn editor_complete(&mut self) {
        // Editor housekeeping (mirrors editor_dirty) — without clearing
        // the cycle, which we own here.
        self.history_pos = None;
        self.editor_preferred_col = None;

        if let Some(cycle) = self.completion.clone() {
            // -- advance an active cycle --
            if cycle.candidates.is_empty() {
                return;
            }
            let next = (cycle.index + 1) % cycle.candidates.len();
            let cand = cycle.candidates[next].clone();
            self.editor_buffer
                .replace_range(cycle.start..cycle.end, &cand.insert);
            let new_end = cycle.start + cand.insert.len();
            self.editor_cursor = new_end;
            self.last_status = Some(format!(
                "completion {}/{} · {}",
                next + 1,
                cycle.candidates.len(),
                cand.kind.label()
            ));
            self.completion = Some(CompletionCycle {
                start: cycle.start,
                end: new_end,
                origin: cycle.origin,
                origin_prefix: cycle.origin_prefix,
                origin_cursor: cycle.origin_cursor,
                candidates: cycle.candidates,
                index: next,
            });
            return;
        }

        // -- start a fresh cycle --
        if self.schema_cache.is_empty() {
            self.last_status = Some("completion: no schema cache yet".to_string());
            return;
        }
        let Some(id) = complete_q::extract_identifier(&self.editor_buffer, self.editor_cursor)
        else {
            return;
        };
        // Refuse to start a cycle when the user hasn't typed anything to
        // match against — otherwise an empty prefix matches every name
        // in the cache and inserts a random identifier at the cursor.
        // Qualified-empty (`u.|`) is still allowed: the qualifier IS the
        // anchor.
        if id.qualifier.is_none() && id.prefix.is_empty() {
            self.last_status = Some("completion: type a prefix first".to_string());
            return;
        }
        let cands = complete_q::candidates_for(
            &self.editor_buffer,
            self.editor_cursor,
            &self.schema_cache,
        );
        if cands.is_empty() {
            self.last_status = Some(format!(
                "completion: no matches for {:?}",
                id.prefix
            ));
            return;
        }
        // Replace [prefix_start, id.end) — the qualifier and dot stay
        // put; the trailing identifier chars past the cursor are also
        // replaced so Tab inside an existing word (`SELECT user|_id`)
        // swaps the WHOLE word, not just the part before the cursor.
        // `prefix.len()` is byte length (prefix was sliced from the buffer).
        let prefix_start = self.editor_cursor.saturating_sub(id.prefix.len());
        let replace_end = id.end;
        // Snapshot the original text BEFORE we mutate so Esc-abandon can
        // put it back verbatim (including any post-cursor chars).
        let original_text = self.editor_buffer[prefix_start..replace_end].to_string();
        let original_cursor = self.editor_cursor;
        let cand = cands[0].clone();
        self.editor_buffer
            .replace_range(prefix_start..replace_end, &cand.insert);
        let new_end = prefix_start + cand.insert.len();
        self.editor_cursor = new_end;
        self.last_status = Some(format!(
            "completion 1/{} · {}",
            cands.len(),
            cand.kind.label()
        ));
        self.completion = Some(CompletionCycle {
            start: prefix_start,
            end: new_end,
            origin: original_text,
            origin_prefix: id.prefix,
            origin_cursor: original_cursor,
            candidates: cands,
            index: 0,
        });
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
                    self.spawn_run(
                        pending.sql,
                        pending.kind,
                        pending.decision,
                        pending.is_batch,
                    );
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
    /// or reject. Multi-statement buffers (e.g. DBUnit scripts) take the batch
    /// path.
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

        // Multi-statement script (DBUnit apply, hand-written batch, …) — take
        // the batch path which uses `batch_execute` and classifies each part.
        let statements = safety::split_statements(&sql);
        if statements.len() > 1 {
            return self.request_run_batch(sql, statements, kind, db.to_string());
        }

        let decision = safety::evaluate(&self.safety_config, db, &sql);

        // EXPLAIN (without ANALYZE) never executes the inner statement — bypass
        // guards entirely.
        if kind == RunKind::Explain {
            self.spawn_run(sql, kind, decision, false);
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
                    is_batch: false,
                    summary: None,
                });
                self.mode = Mode::Confirm;
            }
            Guard::Allow => self.spawn_run(sql, kind, decision, false),
        }
    }

    /// Multi-statement run: classify each piece, take the most-restrictive
    /// guard, and either reject, prompt with a summary, or batch-execute.
    fn request_run_batch(
        &mut self,
        sql: String,
        statements: Vec<String>,
        kind: RunKind,
        db: String,
    ) {
        if !matches!(kind, RunKind::Run) {
            self.last_error = Some(format!(
                "{} not supported for multi-statement scripts",
                kind.label()
            ));
            return;
        }
        let decisions: Vec<Decision> = statements
            .iter()
            .map(|s| safety::evaluate(&self.safety_config, &db, s))
            .collect();
        let max_guard = decisions
            .iter()
            .map(|d| d.guard)
            .fold(Guard::Allow, |acc, g| match (acc, g) {
                (Guard::Block, _) | (_, Guard::Block) => Guard::Block,
                (Guard::Confirm, _) | (_, Guard::Confirm) => Guard::Confirm,
                _ => Guard::Allow,
            });
        let kinds: Vec<safety::StatementKind> = decisions.iter().map(|d| d.kind).collect();
        let summary = batch_summary(&kinds);
        let synthesized = Decision {
            kind: safety::StatementKind::Other,
            guard: max_guard,
            wrap_in_tx: decisions.iter().any(|d| d.wrap_in_tx),
            blocked_by_read_only: decisions.iter().any(|d| d.blocked_by_read_only),
        };

        match max_guard {
            Guard::Block => {
                self.last_error =
                    Some(format!("batch blocked by safety: {summary}"));
            }
            Guard::Confirm => {
                self.pending_run = Some(PendingRun {
                    sql,
                    kind,
                    decision: synthesized,
                    is_batch: true,
                    summary: Some(summary),
                });
                self.mode = Mode::Confirm;
            }
            Guard::Allow => self.spawn_run(sql, kind, synthesized, true),
        }
    }

    /// Load the editor buffer as a path to a DBUnit FlatXmlDataSet, replace
    /// the buffer with the generated clean+insert script. The user reviews,
    /// then runs via Ctrl-R (which takes the multi-statement batch path).
    fn load_dbunit_fixture(&mut self) {
        use crate::dbunit;
        let path_str = self.editor_buffer.trim().to_string();
        if path_str.is_empty() {
            self.last_error = Some(
                "editor is empty — type a fixture file path then ctrl-d".to_string(),
            );
            return;
        }
        let path = std::path::PathBuf::from(&path_str);
        let xml = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                self.last_error = Some(format!("fixture read failed: {e}"));
                return;
            }
        };
        let fixture = match dbunit::parse_flat_xml(&xml) {
            Ok(f) => f,
            Err(e) => {
                self.last_error = Some(format!("fixture parse failed: {e}"));
                return;
            }
        };
        if fixture.rows.is_empty() {
            self.last_error = Some("fixture has no rows".to_string());
            return;
        }
        let row_count = fixture.rows.len();
        let table_count = fixture.tables().len();
        self.editor_buffer =
            dbunit::generate_apply_script(&fixture, dbunit::CleanMode::Truncate);
        self.editor_cursor = 0;
        self.editor_preferred_col = None;
        self.history_pos = None;
        self.last_error = None;
        self.last_status = Some(format!(
            "fixture loaded · {row_count} row(s), {table_count} table(s) · ctrl-r to apply"
        ));
    }

    fn spawn_run(&mut self, sql: String, kind: RunKind, decision: Decision, is_batch: bool) {
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
            let result = execute(&client, &sql, kind, &decision, is_batch).await;
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

/// One-line summary of a multi-statement batch, used in the confirm modal.
/// Example: `"7 statements · Truncate ×2, Insert ×5"`.
fn batch_summary(kinds: &[safety::StatementKind]) -> String {
    use safety::StatementKind as SK;
    let mut counts: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    for k in kinds {
        let label = match k {
            SK::Select => "Select",
            SK::Insert => "Insert",
            SK::Update { .. } => "Update",
            SK::Delete { .. } => "Delete",
            SK::Truncate => "Truncate",
            SK::Drop => "Drop",
            SK::AlterDdl => "DDL",
            SK::Other => "Other",
        };
        *counts.entry(label).or_insert(0) += 1;
    }
    let parts: Vec<String> = counts.iter().map(|(k, n)| format!("{k} ×{n}")).collect();
    format!("{} statements · {}", kinds.len(), parts.join(", "))
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
/// `is_batch` routes through `client.batch_execute` for multi-statement runs.
async fn execute(
    client: &tokio_postgres::Client,
    sql: &str,
    kind: RunKind,
    decision: &Decision,
    is_batch: bool,
) -> Result<Grid, String> {
    if is_batch {
        // Only plain Run makes sense for a multi-statement script.
        if !matches!(kind, RunKind::Run) {
            return Err(format!(
                "{} not supported for multi-statement scripts",
                kind.label()
            ));
        }
        return if decision.wrap_in_tx {
            conn::run_batch_in_tx_open(client, sql).await
        } else {
            conn::run_batch(client, sql).await
        };
    }
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
