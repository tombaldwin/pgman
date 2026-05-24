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
use crate::tui::{Tui, TuiHost};

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
    /// Reverse-incremental history search (Ctrl-R from editor). The
    /// operator types a substring; the editor buffer reflects the
    /// most-recent matching history entry. Enter accepts (stays in
    /// the editor with the match); Esc cancels (restores the
    /// pre-search buffer / cursor). Ctrl-R again jumps to the next
    /// older match.
    HistorySearch,
    /// Interactive row filter on the results grid (`/` from Normal).
    /// Each char updates `grid_filter` and re-filters live so the
    /// operator sees results as they type. Enter accepts; Esc
    /// clears the filter.
    GridFilter,
    /// Tree view of the most recent EXPLAIN / EXPLAIN ANALYZE plan.
    /// Opened automatically when Ctrl-E / Ctrl-A succeeds and the
    /// JSON parses; j/k navigate, Enter expand/collapse, Esc closes.
    ExplainTree,
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
    /// Which candidate is currently inserted. `None` means we expanded
    /// to a common prefix (or just showed the popup) without inserting
    /// any specific candidate — the next Tab will pick `candidates[0]`.
    /// `Some(i)` means `candidates[i]` is currently in the buffer.
    pub selected: Option<usize>,
}

/// Active `\watch` session — re-run the saved SQL every `interval`
/// seconds until the operator hits any other key. `last_fire` advances
/// when a tick fires; we hold off on firing if a query is currently in
/// flight so consecutive ticks can't pile up.
#[derive(Debug, Clone)]
pub struct WatchState {
    pub sql: String,
    pub interval: std::time::Duration,
    pub last_fire: std::time::Instant,
}

/// Inputs that block a `\watch` tick from firing — extracted out so
/// the decision can be unit-tested without a real Instant or a tokio
/// runtime.
#[derive(Debug, Clone, Copy)]
pub struct WatchTickInputs {
    pub query_running: bool,
    pub tx_open: bool,
    pub pending_run: bool,
    /// True when the App is in a mode that should pause the watch
    /// loop (Confirm, TxDecision, picker, RowDetail, etc.).
    pub mode_blocks: bool,
}

/// Pure decision: should the next `\watch` tick dispatch a re-run?
/// Returns `true` when the interval has elapsed AND nothing is
/// blocking (no query in flight, no modal up, etc.).
///
/// ```
/// use std::time::{Duration, Instant};
/// use pgman::app::{watch_should_fire, WatchState, WatchTickInputs};
/// let now = Instant::now();
/// let state = WatchState {
///     sql: "SELECT 1".into(),
///     interval: Duration::from_secs(2),
///     last_fire: now,
/// };
/// let clear = WatchTickInputs {
///     query_running: false, tx_open: false,
///     pending_run: false, mode_blocks: false,
/// };
/// assert!(!watch_should_fire(&state, now, clear));              // not yet
/// assert!(watch_should_fire(&state, now + Duration::from_secs(2), clear));
/// // Any blocker suppresses fire even past the interval.
/// let blocked = WatchTickInputs { query_running: true, ..clear };
/// assert!(!watch_should_fire(&state, now + Duration::from_secs(10), blocked));
/// ```
pub fn watch_should_fire(
    state: &WatchState,
    now: std::time::Instant,
    inputs: WatchTickInputs,
) -> bool {
    if inputs.query_running || inputs.tx_open || inputs.pending_run || inputs.mode_blocks {
        return false;
    }
    now.duration_since(state.last_fire) >= state.interval
}

/// Pure decision: next sort state when the operator presses `s` over
/// `target_col`. Cycle is `None → Some((col, true)) → Some((col,
/// false)) → None`. Pressing `s` over a *different* column jumps to
/// ASC on the new column rather than continuing the prior cycle —
/// matches how spreadsheet apps disambiguate "I want this column now".
///
/// ```
/// use pgman::app::next_sort_state;
/// assert_eq!(next_sort_state(None, 3), Some((3, true)));
/// assert_eq!(next_sort_state(Some((3, true)), 3), Some((3, false)));
/// assert_eq!(next_sort_state(Some((3, false)), 3), None);
/// // Different column → ASC, not "continue from here".
/// assert_eq!(next_sort_state(Some((3, false)), 5), Some((5, true)));
/// ```
pub fn next_sort_state(
    current: Option<(usize, bool)>,
    target_col: usize,
) -> Option<(usize, bool)> {
    match current {
        None => Some((target_col, true)),
        Some((c, true)) if c == target_col => Some((target_col, false)),
        Some((c, false)) if c == target_col => None,
        _ => Some((target_col, true)),
    }
}

/// Pure: filter `rows` by `pattern` (case-insensitive substring
/// across every column). Returns the row indices that match, in
/// original order. `None` pattern means "everything".
///
/// ```
/// use pgman::app::compute_visible_rows;
/// let rows = vec![
///     vec!["1".into(), "alice".into()],
///     vec!["2".into(), "BOB".into()],
/// ];
/// assert_eq!(compute_visible_rows(&rows, None), vec![0, 1]);
/// assert_eq!(compute_visible_rows(&rows, Some("bo")), vec![1]); // case-insens
/// ```
pub fn compute_visible_rows(rows: &[Vec<String>], pattern: Option<&str>) -> Vec<usize> {
    match pattern {
        None => (0..rows.len()).collect(),
        Some(pat) => {
            let needle = pat.to_ascii_lowercase();
            (0..rows.len())
                .filter(|i| {
                    rows[*i]
                        .iter()
                        .any(|c| c.to_ascii_lowercase().contains(&needle))
                })
                .collect()
        }
    }
}

/// One visible row in the flattened EXPLAIN tree. Carries enough
/// for the renderer to draw the line (indent + label) and for the
/// key handler to know which node it points at (the `path` + the
/// `has_children` flag).
#[derive(Debug, Clone)]
pub struct ExplainRow {
    /// Indices from the root of the tree to this node.
    pub path: Vec<usize>,
    pub depth: usize,
    pub node_type: String,
    pub relation: Option<String>,
    pub alias: Option<String>,
    pub hot_score: Option<f64>,
    /// Whether this node has children at all (gates the
    /// expand/collapse marker rendering).
    pub has_children: bool,
    /// Whether the node is currently collapsed (its children are
    /// hidden). Used for the marker glyph.
    pub collapsed: bool,
    /// Per-node extras (`Filter`, `Index Cond`, …) the renderer can
    /// surface alongside the node line.
    pub extras: Vec<(String, String)>,
    pub actual_rows: Option<f64>,
    pub plan_rows: Option<f64>,
    pub actual_total_time: Option<f64>,
    pub total_cost: Option<f64>,
}

fn flatten_plan(
    node: &crate::query::explain::PlanNode,
    path: &mut Vec<usize>,
    depth: usize,
    collapsed: &std::collections::HashSet<Vec<usize>>,
    out: &mut Vec<ExplainRow>,
) {
    let is_collapsed = collapsed.contains(path);
    out.push(ExplainRow {
        path: path.clone(),
        depth,
        node_type: node.node_type.clone(),
        relation: node.relation_name.clone(),
        alias: node.alias.clone(),
        hot_score: node.hot_score(),
        has_children: !node.children.is_empty(),
        collapsed: is_collapsed,
        extras: node.extras.clone(),
        actual_rows: node.actual_rows,
        plan_rows: node.plan_rows,
        actual_total_time: node.actual_total_time,
        total_cost: node.total_cost,
    });
    if is_collapsed {
        return;
    }
    for (i, child) in node.children.iter().enumerate() {
        path.push(i);
        flatten_plan(child, path, depth + 1, collapsed, out);
        path.pop();
    }
}

/// Cancel-side of the database connection. Trait-abstracted so
/// `cancel_running_query` is unit-testable without a real
/// `tokio_postgres::Client`: production wires
/// [`PgCancelDispatcher`] from the live `CancelToken`; tests
/// inject a recording fake.
///
/// The dispatcher is fire-and-forget: the actual `CancelRequest`
/// TCP runs on a spawned task because the cancel can sit behind
/// network latency.
pub trait CancelDispatcher: std::fmt::Debug + Send + Sync {
    fn dispatch(&self);
}

/// Production cancel dispatcher backed by a `tokio_postgres::CancelToken`.
/// Each `dispatch` clones the token and spawns the actual
/// `CancelRequest` send so the App's main loop doesn't block.
pub struct PgCancelDispatcher {
    token: tokio_postgres::CancelToken,
}

impl std::fmt::Debug for PgCancelDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgCancelDispatcher").finish_non_exhaustive()
    }
}

impl PgCancelDispatcher {
    pub fn new(token: tokio_postgres::CancelToken) -> Self {
        Self { token }
    }
}

impl CancelDispatcher for PgCancelDispatcher {
    fn dispatch(&self) {
        let token = self.token.clone();
        tokio::spawn(async move {
            // CancelRequest is an unauthenticated short packet;
            // servers always accept it plaintext. `NoTls` saves
            // the cost of a fresh TLS handshake just to interrupt
            // a query.
            if let Err(e) = token.cancel_query(tokio_postgres::NoTls).await {
                tracing::warn!("CancelRequest failed: {e}");
            }
        });
    }
}

/// Pure decision: should the splash dismiss at `now`? `until` is
/// the absolute deadline set at App start (3s after launch);
/// `conn_resolved` reflects whether the connection is no longer in
/// the Connecting state (any other state lets us drop the splash
/// early so a fast failure isn't hidden behind the elephant).
///
/// ```
/// use std::time::{Duration, Instant};
/// use pgman::app::splash_should_dismiss;
/// let t0 = Instant::now();
/// let until = Some(t0 + Duration::from_secs(3));
/// // Invisible → never dismisses (nothing to do).
/// assert!(!splash_should_dismiss(false, until, false, t0));
/// // Past deadline → dismiss.
/// assert!(splash_should_dismiss(true, until, false, t0 + Duration::from_secs(4)));
/// // Connection resolved before deadline → dismiss anyway.
/// assert!(splash_should_dismiss(true, until, true, t0));
/// ```
pub fn splash_should_dismiss(
    visible: bool,
    until: Option<std::time::Instant>,
    conn_resolved: bool,
    now: std::time::Instant,
) -> bool {
    if !visible {
        return false;
    }
    match until {
        Some(deadline) => now >= deadline || conn_resolved,
        None => false,
    }
}

/// Pure decision: is the draft auto-save throttle past its window?
/// Returns `true` when at least `min_gap` has elapsed since the last
/// save, OR when there's never been a save yet. The run-loop uses
/// 500 ms; tests can pass an arbitrary gap.
///
/// ```
/// use std::time::{Duration, Instant};
/// use pgman::app::draft_save_due;
/// let t0 = Instant::now();
/// let gap = Duration::from_millis(500);
/// assert!(draft_save_due(None, t0, gap));                // never saved → due
/// assert!(!draft_save_due(Some(t0), t0, gap));           // just saved
/// assert!(draft_save_due(
///     Some(t0),
///     t0 + Duration::from_millis(501),
///     gap,
/// ));
/// ```
pub fn draft_save_due(
    last_save: Option<std::time::Instant>,
    now: std::time::Instant,
    min_gap: std::time::Duration,
) -> bool {
    match last_save {
        Some(t) => now.duration_since(t) >= min_gap,
        None => true,
    }
}

/// Pure: reverse-incremental history match. Starting from
/// `from_exclusive` (or `history.len()` when `None`), walks backward
/// to find the newest entry that contains `needle` (case-
/// insensitively). Returns the index of the match.
///
/// ```
/// use pgman::app::history_search_next;
/// let h = vec!["SELECT 1".into(), "INSERT INTO t".into(), "SELECT 2".into()];
/// assert_eq!(history_search_next(&h, "sel", None), Some(2));   // newest
/// assert_eq!(history_search_next(&h, "sel", Some(2)), Some(0)); // older
/// assert_eq!(history_search_next(&h, "sel", Some(0)), None);    // past oldest
/// ```
pub fn history_search_next(
    history: &[String],
    needle: &str,
    from_exclusive: Option<usize>,
) -> Option<usize> {
    let n = needle.to_ascii_lowercase();
    let start = from_exclusive.unwrap_or(history.len());
    (0..start).rev().find(|&i| history[i].to_ascii_lowercase().contains(&n))
}

/// Reverse-incremental history search state (Ctrl-R). The match cursor
/// walks `App::history` from newest to oldest, looking for entries whose
/// text contains `query` (case-insensitive substring). On accept the
/// matched history entry stays in the editor; on cancel the
/// pre-search buffer and cursor are restored.
#[derive(Debug, Clone, Default)]
pub struct HistorySearchState {
    /// The substring the operator has typed so far.
    pub query: String,
    /// Index into `App::history` of the current match, if any.
    pub matched: Option<usize>,
    /// Buffer + cursor at the moment the search started — restored on Esc.
    pub saved_buffer: String,
    pub saved_cursor: usize,
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
    /// Vertical scroll offset (lines hidden above the viewport) for the
    /// editor pane. The renderer auto-adjusts this each frame to keep
    /// the cursor's line visible; the field is plain state (not derived)
    /// so the renderer doesn't have to recompute from scratch when the
    /// buffer changes between frames.
    pub editor_scroll: u16,
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
    /// Active reverse-incremental history search session (Ctrl-R).
    /// When `Some`, `mode == Mode::HistorySearch` and the editor
    /// buffer reflects the current match — Enter accepts, Esc
    /// restores the saved buffer / cursor.
    pub history_search: Option<HistorySearchState>,
    /// `\watch` session — when `Some`, the main loop re-runs the
    /// saved SQL at `interval` cadence. Any key event clears it.
    pub watch: Option<WatchState>,
    /// Recent server-emitted notices (`RAISE NOTICE`, `RAISE WARNING`,
    /// etc.). Newest at the end; bounded so a chatty trigger can't
    /// grow unbounded. The latest one is mirrored into `last_status`
    /// when it arrives so the operator sees it immediately.
    pub notices: Vec<crate::conn::NoticeMsg>,
    /// True when the operator hit the `\e` keybinding and the main
    /// `run()` loop should suspend the TUI, fork the external editor,
    /// and reload the buffer. The action needs `&mut Tui`, which the
    /// editor key handler doesn't have — so it's deferred here.
    pub external_edit_pending: bool,
    /// `Instant` of the last successful `persist_draft`. The main
    /// loop saves at most every 500ms when the buffer is dirty, so a
    /// panic mid-typing loses at most half a second of work.
    pub draft_last_save: Option<Instant>,
    /// Set whenever the buffer is mutated; cleared on save. Avoids
    /// `write_atomic`-ing a buffer that hasn't changed.
    pub draft_dirty: bool,
    /// Column under the cursor in the results grid. h/l move it; sort
    /// + future column-aware actions operate on this column.
    pub grid_col_cursor: usize,
    /// Sort state for the grid: `None` = display order from the
    /// query; `Some((col, asc))` = sorted by that column. Cycled by
    /// `s` in Normal mode: off → ASC → DESC → off.
    pub grid_sort: Option<(usize, bool)>,
    /// The grid as it landed from the query — preserved so a "clear
    /// sort" can restore the original row order without re-running.
    pub grid_raw_rows: Option<Vec<Vec<String>>>,
    /// Active row-filter pattern (case-insensitive substring across
    /// all columns). `None` = no filter; rendered rows are
    /// `visible_rows` indices into the (possibly sorted) `grid.rows`.
    pub grid_filter: Option<String>,
    /// Indices into `grid.rows` for the currently-visible rows under
    /// the active filter. Equal to `0..rows.len()` when no filter is
    /// set. Rebuilt whenever filter / sort / grid changes.
    pub grid_visible_rows: Vec<usize>,
    /// Most recent EXPLAIN / EXPLAIN ANALYZE plan, when `Mode::ExplainTree`
    /// is active. Built from `EXPLAIN (FORMAT JSON)` output on a
    /// successful run.
    pub explain_plan: Option<crate::query::explain::PlanNode>,
    /// Cursor into the flattened (visible-after-collapses) plan list.
    /// j/k move it; Enter toggles collapse on the focused node.
    pub explain_cursor: usize,
    /// Paths (chains of child-array indices from the root) of nodes
    /// the operator has collapsed. The renderer hides anything below
    /// these.
    pub explain_collapsed: std::collections::HashSet<Vec<usize>>,

    /// Saved working buffer while navigating history (restored on Ctrl-N past
    /// the newest entry).
    history_draft: String,
    client: Option<Arc<tokio_postgres::Client>>,
    /// Cancel-side of the live connection. Set on every `Booted`
    /// alongside `client`. Cleared on disconnect. Trait-abstracted so
    /// tests can inject a recording fake to verify Ctrl-C routes
    /// through it without going through tokio_postgres.
    cancel_dispatcher: Option<Box<dyn CancelDispatcher>>,
    /// SSH tunnel paired with `client` — non-None when the connection
    /// went via a bastion. Held here so its `Drop` (which terminates
    /// the ssh subprocess) only fires when the App loses the client.
    tunnel: Option<crate::tunnel::SshTunnel>,
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
            editor_scroll: 0,
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
            history_search: None,
            watch: None,
            notices: Vec::new(),
            external_edit_pending: false,
            draft_last_save: None,
            draft_dirty: false,
            grid_col_cursor: 0,
            grid_sort: None,
            grid_raw_rows: None,
            grid_filter: None,
            grid_visible_rows: Vec::new(),
            explain_plan: None,
            explain_cursor: 0,
            explain_collapsed: std::collections::HashSet::new(),
            data_source_picks,
            data_source_pick_index: 0,
            client: None,
            cancel_dispatcher: None,
            tunnel: None,
            safety_config,
            read_only,
            statement_timeout_ms,
            msg_tx,
            msg_rx: Some(msg_rx),
        }
    }

    /// Run the event loop until the user quits. Production entry —
    /// wires crossterm's `EventStream` into a channel + delegates to
    /// the generic [`run_with`] inner loop. Tests use `run_with`
    /// directly with a synthetic event channel + a [`HeadlessTui`].
    pub async fn run(&mut self, tui: &mut Tui) -> anyhow::Result<()> {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<Event>();
        // Forward crossterm events into the channel so the inner
        // loop only deals with one event-source shape. The spawned
        // task ends when the channel is dropped (after run_with
        // returns), giving us a clean shutdown.
        tokio::spawn(async move {
            let mut events = EventStream::new();
            while let Some(Ok(ev)) = events.next().await {
                if event_tx.send(ev).is_err() {
                    break;
                }
            }
        });
        self.run_with(tui, event_rx).await
    }

    /// Generic loop body — drives any [`TuiHost`] from any `Event`
    /// source. Production passes the real `Tui` + a channel fed by
    /// crossterm; tests pass `HeadlessTui` + a synthetic channel.
    pub async fn run_with<T: TuiHost>(
        &mut self,
        tui: &mut T,
        mut events: UnboundedReceiver<Event>,
    ) -> anyhow::Result<()> {
        let mut msg_rx = self
            .msg_rx
            .take()
            .expect("App::run must be called exactly once");

        if self.dsn.is_some() {
            self.start_connect();
        }

        // One frame clock for all animation sources. Gated by `wants_animation`
        // so an idle, connected app does no work.
        let mut frame = tokio::time::interval(Duration::from_millis(110));
        frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        while !self.should_quit {
            self.tick_splash();
            tui.draw(self)?;
            let animate = self.wants_animation();
            tokio::select! {
                ev = events.recv() => {
                    match ev {
                        Some(ev) => self.on_event(ev),
                        // Channel closed — the event producer is gone.
                        // Treat as quit so the loop terminates cleanly
                        // (tests rely on this to drive the loop with a
                        // bounded sequence and then drop the sender).
                        None => self.should_quit = true,
                    }
                }
                _ = frame.tick(), if animate => {
                    self.anim_tick = self.anim_tick.wrapping_add(1);
                    // Hot path for `\watch`: re-fires the saved
                    // query at the configured cadence. Tick checks
                    // are cheap (one Instant::now per frame).
                    self.tick_watch();
                }
                Some(msg) = msg_rx.recv() => {
                    self.on_msg(msg);
                }
            }
            // Deferred actions that need `&mut TuiHost`. `\e`
            // (external editor) sets the flag from the editor key
            // handler; we do the suspend / fork / resume here so the
            // editor key path stays sync and doesn't have to plumb
            // TuiHost through every dispatch.
            if self.external_edit_pending {
                self.external_edit_pending = false;
                self.run_external_editor(tui);
            }
            // Periodic auto-save: persist at most every 500 ms when
            // the buffer is dirty, so a panic in any spawned task
            // loses at most half a second of work. Cheap atomic
            // write via rename; not even a syscall when not dirty.
            if self.draft_dirty
                && draft_save_due(self.draft_last_save, Instant::now(), Duration::from_millis(500))
            {
                let _ = persist_draft(&self.editor_buffer);
                self.draft_last_save = Some(Instant::now());
                self.draft_dirty = false;
            }
        }
        // Persist the editor draft so the next launch can restore
        // whatever the operator had in flight. Best-effort: failure
        // logs and moves on — the loop is already finishing.
        if let Err(e) = persist_draft(&self.editor_buffer) {
            tracing::warn!("could not save editor draft: {e}");
        }
        Ok(())
    }

    /// Suspend the TUI, write the editor buffer to a temp file, run
    /// `$EDITOR` (or `$VISUAL`, falling back to `vi`), read the file
    /// back, then resume the TUI. Errors land in `last_error` and the
    /// terminal is always restored.
    fn run_external_editor<T: TuiHost>(&mut self, tui: &mut T) {
        let editor_cmd = std::env::var("EDITOR")
            .ok()
            .or_else(|| std::env::var("VISUAL").ok())
            .unwrap_or_else(|| "vi".to_string());

        if let Err(e) = tui.suspend() {
            self.last_error = Some(format!("could not suspend TUI: {e}"));
            return;
        }
        let result = external_edit_via(&self.editor_buffer, &editor_cmd);
        // Resume the TUI even if the editor errored — leaving the
        // operator stuck in a half-suspended terminal would be much
        // worse than a slightly delayed error message.
        let resume_err = tui.resume().err();
        match result {
            Ok(text) => {
                self.editor_buffer = text;
                self.editor_cursor = self.editor_buffer.len();
                self.editor_preferred_col = None;
                self.history_pos = None;
                self.draft_dirty = true;
                self.last_status =
                    Some(format!("loaded {} char(s) from $EDITOR", self.editor_buffer.len()));
            }
            Err(e) => {
                self.last_error = Some(e);
            }
        }
        if let Some(e) = resume_err {
            self.last_error = Some(format!("TUI resume after $EDITOR failed: {e}"));
        }
    }

    /// Auto-dismiss the splash either when its 3-second minimum has
    /// elapsed OR as soon as the connection resolves (Connected or
    /// Failed) — otherwise a fast failure / fast bootstrap would be
    /// hidden behind the elephant for up to 3s. The picker / disconnected
    /// idle state still gets its full 3s of splash because `conn_state`
    /// is `Disconnected` there, so the early-dismiss branch doesn't fire.
    /// Cheap to call every loop iteration — a single `Instant::now`.
    fn tick_splash(&mut self) {
        if !self.splash_visible {
            return;
        }
        let connection_resolved = matches!(
            self.conn_state,
            ConnState::Connected { .. } | ConnState::Failed(_)
        );
        if splash_should_dismiss(
            self.splash_visible,
            self.splash_until,
            connection_resolved,
            Instant::now(),
        ) {
            self.splash_visible = false;
            self.splash_until = None;
        }
    }

    /// Whether the frame clock should keep ticking — for the splash trunk /
    /// blink animation, the connecting spinner, and the in-flight-query
    /// spinner.
    fn wants_animation(&self) -> bool {
        self.splash_visible
            || self.query_running
            || self.watch.is_some()
            || matches!(self.mode, Mode::About)
            || matches!(self.conn_state, ConnState::Connecting)
    }

    /// Spawn the connect + bootstrap-query task. The result returns as an
    /// `AppMsg` tagged with the current generation.
    fn start_connect(&mut self) {
        let Some(dsn) = self.dsn.clone() else {
            return;
        };
        // Bump the generation so a late Booted/BootFailed from a prior
        // attempt can't clobber this one's state. The `on_msg` filter
        // already drops messages whose generation doesn't match; we just
        // need to make the field actually move.
        self.generation = self.generation.wrapping_add(1);
        self.conn_state = ConnState::Connecting;
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let read_only = self.read_only;
        let statement_timeout_ms = self.statement_timeout_ms;
        // Notice channel — server-emitted `RAISE NOTICE` / `WARNING` /
        // `INFO` flow through here. Forwarded into the App's main
        // message queue as `AppMsg::Notice` so a single select! loop
        // serves everything. Each connect gets a fresh pair so a
        // stale receiver from a prior session can't leak.
        let (notice_tx, mut notice_rx) =
            tokio::sync::mpsc::unbounded_channel::<conn::NoticeMsg>();
        let forward_tx = tx.clone();
        let notice_generation = generation;
        tokio::spawn(async move {
            while let Some(notice) = notice_rx.recv().await {
                let msg = AppMsg::Notice {
                    generation: notice_generation,
                    notice,
                };
                if forward_tx.send(msg).is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let msg = match conn::connect_and_bootstrap(
                dsn,
                read_only,
                statement_timeout_ms,
                BOOTSTRAP_SQL.to_string(),
                notice_tx,
            )
            .await
            {
                Ok(b) => AppMsg::Booted {
                    generation,
                    server_version: b.server_version,
                    grid: b.grid,
                    client: b.client,
                    schema_cache: b.schema_cache,
                    tunnel: b.tunnel,
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
                tunnel,
                ..
            } => {
                self.conn_state = ConnState::Connected { server_version };
                // Pre-build the cancel dispatcher so Ctrl-C can fire
                // without touching the Client. Replaced on every new
                // Booted so the dispatcher always matches the live
                // backend PID.
                self.cancel_dispatcher =
                    Some(Box::new(PgCancelDispatcher::new(client.cancel_token())));
                self.client = Some(client);
                // Hold the new tunnel (if any) so its Drop fires when
                // the App loses the client at quit / next reconnect.
                // The PREVIOUS tunnel — if there was one — must be
                // dropped off-thread: `SshTunnel::drop` does
                // `child.kill()` + blocking `child.wait()`, and a
                // wedged ssh subprocess (e.g. stuck ProxyCommand)
                // would otherwise freeze the UI loop here.
                if let Some(old) = self.tunnel.take() {
                    tokio::task::spawn_blocking(move || drop(old));
                }
                self.tunnel = tunnel;
                self.grid = grid;
                self.grid_state
                    .select(if self.grid.is_empty() { None } else { Some(0) });
                self.reset_grid_view();
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
                self.reset_grid_view();
                self.query_running = false;
                self.last_error = None;
                self.last_status = Some(format!(
                    "{kind_label} ok · {} row(s)",
                    self.grid.row_count()
                ));
                // EXPLAIN / EXPLAIN ANALYZE: parse the JSON we asked
                // for and pop the tree visualiser. On parse failure
                // we fall back to the raw grid (the JSON text is
                // still readable that way), surface the parse error
                // in last_status so the operator sees what happened.
                if kind_label == "EXPLAIN" || kind_label == "EXPLAIN ANALYZE" {
                    if let Some(text) = self
                        .grid
                        .rows
                        .first()
                        .and_then(|r| r.first())
                        .cloned()
                    {
                        match crate::query::explain::parse(&text) {
                            Ok(plan) => {
                                self.explain_plan = Some(plan);
                                self.explain_cursor = 0;
                                self.explain_collapsed.clear();
                                self.mode = Mode::ExplainTree;
                            }
                            Err(e) => {
                                self.last_status = Some(format!(
                                    "{kind_label} parse: {e} — falling back to raw text"
                                ));
                            }
                        }
                    }
                }
                if tx_open_after {
                    self.tx_open = true;
                    self.mode = Mode::TxDecision;
                }
            }
            AppMsg::QueryFailed {
                error, position, ..
            } => {
                self.query_running = false;
                self.last_status = None;
                self.last_error = Some(error);
                // Postgres flagged a syntax error at a specific
                // character — move the editor cursor there so the
                // operator sees the offending token. The position is
                // 1-indexed CHARS into the SQL we submitted; convert
                // to a 0-indexed BYTE offset into `editor_buffer`.
                // Out-of-range positions are ignored (could happen
                // for batches where we sent a transformed string).
                if let Some(p) = position {
                    // Postgres reports a 1-indexed CHAR position into
                    // the string WE submitted. `request_run` trims
                    // leading whitespace before submitting, so we
                    // skip past the same trimmed prefix in the
                    // editor buffer before counting chars. Without
                    // this, `\n\nSELECT FROM x` with an error at
                    // submitted position 8 lands the cursor 2 chars
                    // off because the leading `\n\n` is in the
                    // buffer but not in the submitted SQL.
                    let trimmed_prefix_bytes = self
                        .editor_buffer
                        .len()
                        - self.editor_buffer.trim_start().len();
                    let target_chars = (p.saturating_sub(1)) as usize;
                    let after_trim = &self.editor_buffer[trimmed_prefix_bytes..];
                    let inner_byte = after_trim
                        .char_indices()
                        .nth(target_chars)
                        .map(|(b, _)| b)
                        .unwrap_or(after_trim.len());
                    let byte_offset = trimmed_prefix_bytes + inner_byte;
                    self.editor_cursor = byte_offset.min(self.editor_buffer.len());
                    self.editor_preferred_col = None;
                    if self.mode == Mode::Normal {
                        self.mode = Mode::Editor;
                    }
                }
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
            AppMsg::Notice { notice, .. } => {
                // Surface server-emitted notices in the status footer,
                // and stash recent ones so a follow-up "show notices"
                // panel can render them. Severity goes first so a
                // `WARNING` reads visibly different from a `NOTICE`.
                self.last_status = Some(format!("[{}] {}", notice.severity, notice.message));
                tracing::info!(
                    "pg notice [{}]: {}{}{}",
                    notice.severity,
                    notice.message,
                    notice.detail.as_deref().map(|d| format!(" · detail: {d}")).unwrap_or_default(),
                    notice.hint.as_deref().map(|h| format!(" · hint: {h}")).unwrap_or_default(),
                );
                self.notices.push(notice);
                if self.notices.len() > 50 {
                    self.notices.remove(0);
                }
            }
        }
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            Event::Paste(text) => self.on_paste(text),
            _ => {}
        }
    }

    /// Bracketed paste: terminal delivered the entire pasted blob in one
    /// event. Only meaningful in the editor (the only typing surface);
    /// elsewhere we ignore so a stray paste on the grid doesn't trigger
    /// arbitrary keypress side effects.
    fn on_paste(&mut self, text: String) {
        if self.mode != Mode::Editor {
            return;
        }
        // Splash, if still visible, was waiting on a key — dismiss it
        // so the paste lands on the actual editor surface, not an empty
        // pre-app frame.
        self.splash_visible = false;
        self.splash_until = None;
        // Drop any active completion cycle — a paste mid-cycle is a hard
        // commit / reset boundary.
        self.completion = None;
        self.editor_dirty();
        // Normalise line endings to LF: most terminals deliver CRLF on
        // Windows or `\r` from old-Mac sources. Don't collapse blank
        // lines — the operator pasted them deliberately.
        let cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
        // Bulk insert (O(N)) — looping editor_insert char-by-char is
        // O(N²) because each `String::insert(idx, c)` shifts the tail
        // of the buffer. A 5MB schema-diff paste froze the UI for
        // multiple seconds; insert_str makes it instant.
        self.editor_buffer.insert_str(self.editor_cursor, &cleaned);
        self.editor_cursor += cleaned.len();
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // Any key cancels an active `\watch` session — psql-style.
        // Done BEFORE the Ctrl-C-quits arm so Ctrl-C-while-watching
        // doesn't also tear down the session.
        let was_watching = self.watch.is_some();
        if was_watching {
            self.cancel_watch();
        }
        // Ctrl-C quits — UNLESS we're in the editor, where Ctrl-C
        // cancels a running query (the editor's own handler picks it
        // up below). An idle editor still falls through to the
        // editor handler, which no-ops on Ctrl-C with no query — a
        // reflex Ctrl-C in mid-typing shouldn't lose the buffer.
        // If we were watching, the key already served its purpose
        // (stopping the loop) — don't quit on it.
        if !was_watching
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            if self.mode != Mode::Editor {
                self.should_quit = true;
                return;
            }
            // Fall through to the editor's on_editor_key for the
            // cancel-or-no-op logic.
        }
        // (No early return for Ctrl-C-while-watching here: if a
        // query is also in flight, the editor handler's
        // `cancel_running_query` arm below still needs to fire so
        // one Ctrl-C stops the watch AND cancels the live query.
        // With no in-flight query, the editor handler no-ops on
        // Ctrl-C and we end up in the right place anyway.)
        // Any key dismisses the splash — but the key then flows through
        // to the mode dispatcher rather than being consumed. Snappy
        // users press a key to skip the elephant AND have that key do
        // its normal job in one go.
        if self.splash_visible {
            self.splash_visible = false;
            self.splash_until = None;
            // fall through so the key reaches the active mode's handler
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
            Mode::HistorySearch => self.on_history_search_key(key),
            Mode::GridFilter => self.on_grid_filter_key(key),
            Mode::ExplainTree => self.on_explain_tree_key(key),
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

    /// Map the visible-row cursor (TableState index) to the actual
    /// `grid.rows` index, honouring any active filter. Returns
    /// `None` when nothing is selected or the visible set is empty.
    pub(crate) fn selected_grid_row_idx(&self) -> Option<usize> {
        let visible_idx = self.grid_state.selected()?;
        self.grid_visible_rows.get(visible_idx).copied()
    }

    /// Open the expanded view of the currently-selected grid row. No-op
    /// when the grid is empty or nothing is selected.
    fn open_row_detail(&mut self) {
        let Some(idx) = self.selected_grid_row_idx() else {
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
        let Some(idx) = self.selected_grid_row_idx() else {
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
        let Some(idx) = self.selected_grid_row_idx() else {
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
            // q (and Ctrl-C) quit; Esc is a no-op so a reflex press
            // can't abandon the picker by accident.
            KeyCode::Char('q') => self.should_quit = true,
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
                // Only offer "change connection" when there are at least
                // two candidates — otherwise the picker would just show
                // the same DSN that just failed, and Enter would retry it
                // (already on `r`).
                KeyCode::Char('p') if self.data_source_picks.len() >= 2 => {
                    self.mode = Mode::ConnPick;
                    self.data_source_pick_index = 0;
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            // q (and Ctrl-C) are the only quit keys. Esc used to also
            // quit, but a reflex Esc shouldn't ever lose the session —
            // overlays bind Esc to "close me", and in Normal mode Esc
            // is a no-op so an extra press from inside a closed overlay
            // is harmless.
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('e') | KeyCode::Char('i') | KeyCode::Tab => {
                self.mode = Mode::Editor;
            }
            // `c` opens the connection picker mid-session — psql's
            // `\c` equivalent. Requires at least one discovered data
            // source to be useful; with zero we surface a status hint
            // rather than dropping into an empty picker.
            KeyCode::Char('c') => self.start_connection_change(),
            KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
            KeyCode::Char('h') | KeyCode::Left => self.move_col_cursor(-1),
            KeyCode::Char('l') | KeyCode::Right => self.move_col_cursor(1),
            KeyCode::Char('s') => self.cycle_sort(),
            KeyCode::Char('Y') => self.export_grid_to_clipboard(),
            KeyCode::Char('/') => self.start_filter(),
            KeyCode::Char('n') => self.filter_step(true),
            KeyCode::Char('N') => self.filter_step(false),
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
        // Ctrl-Space is the industry-standard alias — IDEs and most
        // shells bind it to "open the completion popup". Same handler
        // as Tab; gives muscle-memory users a familiar shortcut without
        // pre-empting Tab's role as the indent / fast-cycle key.
        if matches!(key.code, KeyCode::Char(' '))
            && key.modifiers.contains(KeyModifiers::CONTROL)
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
        // While a completion popup is up in pre-selection state (LCP
        // expanded / popup-only, nothing committed via Tab yet),
        // narrowing keys — plain char insertion, Backspace, Delete —
        // should keep the popup live and re-narrow the candidate list
        // instead of clearing the cycle. Any other key (Enter, arrow
        // keys, Ctrl-*, etc.) drops the cycle as before.
        let was_pre_selected = self
            .completion
            .as_ref()
            .map(|c| c.selected.is_none())
            .unwrap_or(false);
        let is_narrowing_key = match key.code {
            KeyCode::Char(_) => !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT),
            KeyCode::Backspace | KeyCode::Delete => true,
            _ => false,
        };
        let preserve_cycle = was_pre_selected && is_narrowing_key;
        if !preserve_cycle {
            // Existing: clear cycle on any non-narrowing key. Also wipe
            // a stale `completion N/M …` status the footer was showing.
            if self.completion.is_some() {
                if let Some(s) = &self.last_status {
                    if s.starts_with("completion") {
                        self.last_status = None;
                    }
                }
            }
            self.completion = None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            // Run keys (Ctrl-* primary; F-keys are aliases for full-keyboard
            // users — F-keys on a MacBook need fn+). Enter inserts a newline.
            // Ctrl-R is reverse-incremental history search (matches
            // bash / readline / psql convention). Run moves to F5 and
            // Ctrl-Enter (terminal-dependent) — see below.
            KeyCode::Char('r') if ctrl => self.start_history_search(),
            KeyCode::Char('e') if ctrl => self.request_run(RunKind::Explain),
            KeyCode::Char('a') if ctrl => self.request_run(RunKind::ExplainAnalyze),
            KeyCode::Char('l') if ctrl => self.start_log_import(),
            KeyCode::Char('d') if ctrl => self.load_dbunit_fixture(),
            // Ctrl-W → start a \watch session against the editor's
            // current buffer (or, if it's empty, the most recent
            // history entry). Suppressed mid-query and during an
            // open auto_tx — watch would otherwise pile up runs on
            // a paused session.
            KeyCode::Char('w') if ctrl => self.start_watch(),
            // Ctrl-F → pretty-print the buffer via `pg_format`.
            // Errors when pg_format isn't installed and points the
            // operator at the install command for their OS.
            KeyCode::Char('f') if ctrl => self.reformat_buffer(),
            // Ctrl-X → `\e` external editor. Sets a flag so the main
            // `run()` loop can do the suspend / spawn / resume dance
            // (which needs `&mut Tui`).
            KeyCode::Char('x') if ctrl => self.external_edit_pending = true,
            // Some terminals report Ctrl-Enter; others fold it into
            // Ctrl-J. Both run.
            KeyCode::Enter if ctrl => self.request_run(RunKind::Run),
            KeyCode::Char('j') if ctrl => self.request_run(RunKind::Run),
            // Ctrl-C while a query is in flight sends a PostgreSQL
            // CancelRequest to the same backend. No-op otherwise (we
            // run in raw mode so Ctrl-C doesn't quit).
            KeyCode::Char('c') if ctrl && self.query_running => self.cancel_running_query(),
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
        // If we kept the cycle alive across a narrowing key, recompute
        // the candidate set against the new buffer state so the popup
        // reflects what's now matching.
        if preserve_cycle {
            self.refresh_completion();
        }

        // Auto-trigger completion when the operator just typed `.` after
        // an identifier (e.g. `users.|` or `u.|`). Modern editors do
        // this to save a Tab keystroke for the common qualified-access
        // case. Suppressed when:
        //   - a cycle is already alive (refresh_completion handled it),
        //   - the char before the `.` isn't alphabetic / `_` (so we
        //     don't fire on `3.14`-style numeric literals),
        //   - completion fails (no schema cache, no matches) — the
        //     status message is restored so we don't yell at the user
        //     for typing `.` in normal text.
        let just_typed_dot = matches!(key.code, KeyCode::Char('.'))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if just_typed_dot && self.completion.is_none() {
            // The `.` is at the byte position immediately before the
            // cursor. Walk back ONE char (not one byte) so identifiers
            // ending in non-ASCII letters — `café.`, `naïve.`,
            // quoted-name-style `"My Table".` once we support those —
            // still trigger. Reading `bytes[dot_byte - 1]` would catch
            // only ASCII suffixes.
            let dot_byte = self.editor_cursor.saturating_sub(1);
            let prev_char = self.editor_buffer[..dot_byte].chars().next_back();
            if matches!(prev_char, Some(c) if c.is_alphabetic() || c == '_') {
                let saved_status = self.last_status.clone();
                self.editor_complete();
                if self.completion.is_none() {
                    self.last_status = saved_status;
                }
            }
        }

        // Auto-trigger completion when the operator just typed a space
        // immediately after an identifier-introducing keyword. Keeps
        // the list of trigger keywords short and conservative so we
        // only fire where the popup is unambiguously useful — typing
        // `FROM <Tab>` saves one keystroke, but firing on every space
        // in `WHERE x = 5 ` would be noise. Skipped when a cycle is
        // already alive (which means `refresh_completion` is handling
        // the keystroke).
        const TRIGGER_KEYWORDS: &[&str] = &[
            "FROM", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS",
            "INTO", "WHERE", "AND", "OR", "ON",
        ];
        let just_typed_space = matches!(key.code, KeyCode::Char(' '))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if just_typed_space && self.completion.is_none() {
            // The just-typed space is at `editor_cursor - 1`. Strip it
            // and any further trailing whitespace, then read back the
            // last alphanumeric / `_` word. Walk char_indices in reverse
            // so a multi-byte boundary char (en-dash, smart quote, NBSP,
            // …) doesn't land us mid-codepoint — `rfind(predicate) + 1`
            // would have panicked on those.
            let before_space = &self.editor_buffer[..self.editor_cursor.saturating_sub(1)];
            let trimmed = before_space.trim_end();
            let word_start = trimmed
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            let last_word = &trimmed[word_start..];
            if !last_word.is_empty()
                && TRIGGER_KEYWORDS
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case(last_word))
            {
                let saved_status = self.last_status.clone();
                self.editor_complete();
                if self.completion.is_none() {
                    self.last_status = saved_status;
                }
            }
        }
    }

    /// Re-extract candidates from the current buffer/cursor. Called
    /// after a narrowing key while the completion popup is in
    /// pre-selection state so the popup stays live and narrows as the
    /// operator types. Preserves the cycle's `origin` fields so a
    /// later Esc still undoes back to the pre-Tab state.
    fn refresh_completion(&mut self) {
        let existing = match self.completion.take() {
            Some(c) => c,
            None => return,
        };
        let Some(id) =
            complete_q::extract_identifier(&self.editor_buffer, self.editor_cursor)
        else {
            return;
        };
        // Empty prefix is fine — the candidate set falls back to "all
        // identifier-shaped candidates for the surrounding clause"
        // (matches the Tab-on-whitespace UX). The cycle drops naturally
        // when those produce no matches.
        let cands = complete_q::candidates_for(
            &self.editor_buffer,
            self.editor_cursor,
            &self.schema_cache,
        );
        if cands.is_empty() {
            // Mirror the tailored messaging from editor_complete so the
            // status footer doesn't show `no matches for ""` when the
            // operator narrows the prefix down to empty in a clause
            // context that has nothing else to suggest.
            let msg = if id.prefix.is_empty() {
                match &id.qualifier {
                    Some(q) => format!("completion: no matches for {q}.…"),
                    None => "completion: nothing to suggest here".to_string(),
                }
            } else {
                format!("completion: no matches for {:?}", id.prefix)
            };
            self.last_status = Some(msg);
            return;
        }
        let prefix_start = self.editor_cursor.saturating_sub(id.prefix.len());
        let cand_count = cands.len();
        self.completion = Some(CompletionCycle {
            start: prefix_start,
            end: self.editor_cursor,
            // Original cycle's origin is preserved so Esc-restore
            // returns to the pre-Tab state, not the mid-narrow state.
            origin: existing.origin,
            origin_prefix: existing.origin_prefix,
            origin_cursor: existing.origin_cursor,
            candidates: cands,
            selected: None,
        });
        self.last_status = Some(format!(
            "completion: {} match{} · Tab to pick",
            cand_count,
            if cand_count == 1 { "" } else { "es" }
        ));
    }

    /// Any edit / non-vertical motion exits history navigation and resets
    /// preferred-column tracking.
    fn editor_dirty(&mut self) {
        self.history_pos = None;
        self.editor_preferred_col = None;
        // Mark the buffer dirty for the periodic auto-save in run().
        // We don't persist inline because editor_dirty is called
        // BEFORE the actual mutation at most call sites — the run-
        // loop's "save when stable" pass picks up the post-mutation
        // state.
        self.draft_dirty = true;
    }

    /// Abandon an active completion cycle: restore the original buffer
    /// text the cycle replaced (including any chars that trailed the
    /// cursor when Tab fired) and put the cursor back where it was when
    /// the user pressed Tab. No-op when no cycle is active.
    fn editor_abandon_completion(&mut self) {
        let Some(cycle) = self.completion.take() else {
            return;
        };
        // If the operator backspaced past the cycle's start, the
        // stored range no longer points at valid bytes — bail on
        // the restore but still drop the cycle. Same for cursor: a
        // refresh-narrow may have shrunk the buffer below the
        // pre-Tab cursor position; clamp to current buffer length
        // (which is always a valid char boundary).
        if cycle.start <= self.editor_buffer.len()
            && cycle.end <= self.editor_buffer.len()
            && cycle.start <= cycle.end
            && self.editor_buffer.is_char_boundary(cycle.start)
            && self.editor_buffer.is_char_boundary(cycle.end)
        {
            self.editor_buffer
                .replace_range(cycle.start..cycle.end, &cycle.origin);
        }
        self.editor_cursor = cycle.origin_cursor.min(self.editor_buffer.len());
        self.last_status = Some("completion cancelled".to_string());
    }

    /// Tab-completion in the editor. Bash-style two-phase:
    ///
    /// - First Tab on a fresh prefix:
    ///   - 1 match: insert it.
    ///   - 2+ matches sharing a longer common prefix: insert just the
    ///     common prefix (so `t_` → `t_us` when every match starts with
    ///     `t_us`). The popup shows all candidates; no row highlighted.
    ///   - 2+ matches sharing no extra prefix: don't insert anything;
    ///     show the popup so the operator can see the options and type
    ///     more characters to narrow.
    /// - Second Tab (cycle present, no candidate selected): pick the
    ///   first match.
    /// - Third+ Tab: cycle through.
    ///
    /// Any non-Tab editor key drops the cycle so typing more characters
    /// reverts cleanly.
    fn editor_complete(&mut self) {
        // Editor housekeeping (mirrors editor_dirty) — without clearing
        // the cycle, which we own here.
        self.history_pos = None;
        self.editor_preferred_col = None;

        if let Some(cycle) = self.completion.clone() {
            if cycle.candidates.is_empty() {
                return;
            }
            // Either advance to next candidate, or — if nothing's
            // selected yet (we expanded a common prefix or just showed
            // the popup) — pick the first match.
            let next = match cycle.selected {
                None => 0,
                Some(i) => (i + 1) % cycle.candidates.len(),
            };
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
                selected: Some(next),
            });
            return;
        }

        // -- start a fresh cycle --
        let Some(id) = complete_q::extract_identifier(&self.editor_buffer, self.editor_cursor)
        else {
            return;
        };
        let cands = complete_q::candidates_for(
            &self.editor_buffer,
            self.editor_cursor,
            &self.schema_cache,
        );
        if cands.is_empty() {
            // Tailor the message: empty-cache vs. nothing-to-suggest vs.
            // typed-prefix-but-no-match. SQL vocabulary (keywords,
            // operators) doesn't depend on the cache, so an empty cache
            // doesn't preclude *all* candidates — we only mention the
            // cache when there'd otherwise be no useful hint.
            let msg = if self.schema_cache.is_empty() && id.prefix.is_empty() {
                "completion: connect to a database for identifier suggestions".to_string()
            } else if id.prefix.is_empty() {
                match &id.qualifier {
                    Some(q) => format!("completion: no matches for {q}.…"),
                    None => "completion: nothing to suggest here".to_string(),
                }
            } else {
                format!("completion: no matches for {:?}", id.prefix)
            };
            self.last_status = Some(msg);
            return;
        }

        let prefix_start = self.editor_cursor.saturating_sub(id.prefix.len());
        let replace_end = id.end;
        let original_text = self.editor_buffer[prefix_start..replace_end].to_string();
        let original_cursor = self.editor_cursor;

        // 1) Exact-match fast path: the typed prefix already IS one of
        //    the candidates (case-insensitively). The operator typed the
        //    full name; commit and dismiss the popup. Runs BEFORE the
        //    single-match path so that a lone candidate matching the
        //    typed prefix exactly (e.g. cache has only `users`, operator
        //    typed `users`) also dismisses the popup rather than leaving
        //    a one-row cycle hanging. Empty prefix can't match (no
        //    candidate insert is empty), so this is a no-op there.
        if let Some(exact) = cands
            .iter()
            .find(|c| !c.insert.is_empty() && c.insert.eq_ignore_ascii_case(&id.prefix))
        {
            let cand = exact.clone();
            self.editor_buffer
                .replace_range(prefix_start..replace_end, &cand.insert);
            let new_end = prefix_start + cand.insert.len();
            self.editor_cursor = new_end;
            self.last_status =
                Some(format!("completion · exact match · {}", cand.kind.label()));
            self.completion = None;
            return;
        }

        // 2) Empty unqualified prefix → always show the popup with no
        //    auto-insertion. The operator pressed Tab on whitespace
        //    asking "what can I type here?"; silently inserting a
        //    single candidate would be a footgun (e.g. `INSERT INTO t
        //    (<Tab>` with a one-column table would commit the column
        //    without the operator seeing the choice). Qualified-empty
        //    (`u.|`) still falls through to single-match — the
        //    qualifier IS the operator's signal of intent.
        if id.prefix.is_empty() && id.qualifier.is_none() {
            let cand_count = cands.len();
            self.last_status = Some(format!(
                "completion: {} match{} · Tab to pick",
                cand_count,
                if cand_count == 1 { "" } else { "es" }
            ));
            self.completion = Some(CompletionCycle {
                start: prefix_start,
                end: replace_end,
                origin: original_text,
                origin_prefix: id.prefix,
                origin_cursor: original_cursor,
                candidates: cands,
                selected: None,
            });
            return;
        }

        // 3) Single-match fast path: insert it and keep the cycle
        //    around so Esc undoes the auto-insert.
        if cands.len() == 1 {
            let cand = cands[0].clone();
            self.editor_buffer
                .replace_range(prefix_start..replace_end, &cand.insert);
            let new_end = prefix_start + cand.insert.len();
            self.editor_cursor = new_end;
            self.last_status = Some(format!("completion 1/1 · {}", cand.kind.label()));
            self.completion = Some(CompletionCycle {
                start: prefix_start,
                end: new_end,
                origin: original_text,
                origin_prefix: id.prefix,
                origin_cursor: original_cursor,
                candidates: cands,
                selected: Some(0),
            });
            return;
        }

        // 4) Multi-match: compute the longest common prefix
        //    (case-insensitive) of all candidate inserts. If it extends
        //    past what the operator already typed, advance the buffer
        //    to that common prefix and show the popup — no specific
        //    row selected yet, so a second Tab picks the first match.
        let inserts: Vec<&str> = cands.iter().map(|c| c.insert.as_str()).collect();
        let lcp = complete_q::longest_common_prefix_ci(&inserts);
        let insert_text = if lcp.len() > id.prefix.len() {
            // Mirror the operator's case onto the LCP (so `t_` stays
            // lowercase; `T_` stays uppercase) — the LCP itself is
            // from the first candidate's case which may not match.
            complete_q::case_match(&lcp, &id.prefix)
        } else {
            // No common prefix to expand. Keep the operator's typed
            // text — don't insert anything yet.
            id.prefix.clone()
        };
        self.editor_buffer
            .replace_range(prefix_start..replace_end, &insert_text);
        let new_end = prefix_start + insert_text.len();
        self.editor_cursor = new_end;
        self.last_status = Some(format!(
            "completion: {} match{} · Tab to pick",
            cands.len(),
            if cands.len() == 1 { "" } else { "es" }
        ));
        self.completion = Some(CompletionCycle {
            start: prefix_start,
            end: new_end,
            origin: original_text,
            origin_prefix: id.prefix,
            origin_cursor: original_cursor,
            candidates: cands,
            selected: None,
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

    /// Ctrl-R from the editor — enter reverse-incremental history
    /// search. Snapshots the buffer/cursor so Esc can restore them;
    /// then begins searching from the newest history entry. The
    /// initial query is empty, so the most-recent entry shows by
    /// default (matches bash's "Ctrl-R then Enter recalls the last
    /// command" idiom). If history is empty, surface a status and
    /// stay in editor mode.
    fn start_history_search(&mut self) {
        if self.history.is_empty() {
            self.last_status = Some("history is empty".to_string());
            return;
        }
        let saved_buffer = self.editor_buffer.clone();
        let saved_cursor = self.editor_cursor;
        let initial = self.history.len() - 1;
        self.history_search = Some(HistorySearchState {
            query: String::new(),
            matched: Some(initial),
            saved_buffer,
            saved_cursor,
        });
        // Mirror the most-recent entry into the buffer so the
        // operator can see what they'd commit to with Enter.
        self.editor_buffer = self.history[initial].clone();
        self.editor_cursor = self.editor_buffer.len();
        self.mode = Mode::HistorySearch;
        self.refresh_history_search_status();
    }

    /// Sync the footer status to the active history-search session —
    /// `(reverse-i-search) 'query'` when there's a match, or
    /// `(failed reverse-i-search) 'query'` when not (mirroring bash).
    fn refresh_history_search_status(&mut self) {
        let Some(state) = self.history_search.as_ref() else {
            return;
        };
        self.last_status = Some(match state.matched {
            Some(_) => format!("(reverse-i-search) '{}'", state.query),
            None => format!("(failed reverse-i-search) '{}'", state.query),
        });
    }

    /// Reverse-incremental search step: starting from
    /// `state.matched.unwrap_or(history.len())`, walk backward (older)
    /// looking for an entry whose lowercased text contains the lower-
    /// cased query as a substring. Updates `state.matched` and the
    /// editor buffer in place.
    fn history_search_step(&mut self, from_index: Option<usize>) {
        let Some(state) = self.history_search.as_ref() else {
            return;
        };
        let found = history_search_next(&self.history, &state.query, from_index);
        // Borrow `history_search` mutably for the write of `matched`.
        if let Some(s) = self.history_search.as_mut() {
            s.matched = found;
        }
        if let Some(i) = found {
            self.editor_buffer = self.history[i].clone();
            self.editor_cursor = self.editor_buffer.len();
        }
        // If `found` is None we leave the buffer alone (showing the
        // last good match) — same UX as bash, where a failed search
        // displays `(failed reverse-i-search)` but keeps the prior
        // match on screen.
    }

    /// Handle a key while in Mode::HistorySearch. Char/Backspace edit
    /// the query and re-search from the latest match. Ctrl-R jumps to
    /// the next-older match. Enter accepts (stays in Editor with the
    /// matched buffer). Esc cancels (restores the snapshot).
    fn on_history_search_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if let Some(state) = self.history_search.take() {
                    self.editor_buffer = state.saved_buffer;
                    self.editor_cursor = state.saved_cursor;
                }
                self.last_status = None;
                self.mode = Mode::Editor;
            }
            KeyCode::Enter => {
                // Accept: keep whatever's in the buffer (the matched
                // history entry) and exit back to Editor.
                self.history_search = None;
                self.last_status = None;
                self.mode = Mode::Editor;
            }
            KeyCode::Char('r') if ctrl => {
                // Jump to the next-older match. Start from the
                // CURRENT match's index (exclusive) so we move
                // backward through history.
                let from = self.history_search.as_ref().and_then(|s| s.matched);
                self.history_search_step(from);
                self.refresh_history_search_status();
            }
            KeyCode::Backspace => {
                if let Some(state) = self.history_search.as_mut() {
                    state.query.pop();
                }
                self.history_search_step(None);
                self.refresh_history_search_status();
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(state) = self.history_search.as_mut() {
                    state.query.push(c);
                }
                self.history_search_step(None);
                self.refresh_history_search_status();
            }
            _ => {}
        }
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

    /// Ctrl-C while a query is in flight. Sends a PostgreSQL
    /// `CancelRequest` to the backend by opening a fresh TCP
    /// connection to the same Postgres process and emitting the
    /// magic 16-byte packet — that's what `tokio_postgres`'s
    /// `CancelToken::cancel_query` does. The original `execute`
    /// future then resolves with a cancellation error and lands as
    /// the normal `QueryFailed` message, which resets
    /// `query_running` for us.
    ///
    /// Tunneled connections inherit the original `Config` (with
    /// `hostaddr = 127.0.0.1` pointed at the local ssh-forward), so
    /// the cancel TCP rides through the same tunnel.
    /// Ctrl-W in the editor — start a `\watch`-equivalent session
    /// against the current buffer (or the most recent history entry
    /// when the buffer is empty). Re-runs every 2 s until the
    /// operator hits any other key. Refused when a query is in
    /// flight or an auto_tx is open: piling up runs against a
    /// half-committed session would be a footgun.
    /// Ctrl-F → run `pg_format` over the editor buffer and replace
    /// the contents with its prettyprinted output. `pg_format` is a
    /// widely-deployed Perl tool (`brew install pgformatter`, `apt
    /// install pgformatter`); a missing binary surfaces an
    /// actionable error rather than a silent no-op.
    ///
    /// Done inline — `pg_format` is sub-second on any realistic
    /// buffer; the alternative (`spawn_blocking` + message round-
    /// trip) adds more plumbing than the operation deserves.
    fn reformat_buffer(&mut self) {
        if self.editor_buffer.trim().is_empty() {
            self.last_status = Some("nothing to format".into());
            return;
        }
        match pg_format_via(&self.editor_buffer, "pg_format") {
            Ok(formatted) => {
                let chars = formatted.len();
                self.editor_buffer = formatted;
                self.editor_cursor = self.editor_buffer.len();
                self.editor_preferred_col = None;
                self.history_pos = None;
                self.last_status =
                    Some(format!("formatted via pg_format · {chars} char(s)"));
            }
            Err(e) => self.last_error = Some(e),
        }
    }

    fn start_watch(&mut self) {
        if self.query_running || self.tx_open {
            self.last_error =
                Some("can't \\watch while a query is running or a tx is open".to_string());
            return;
        }
        let sql = self.editor_buffer.trim().to_string();
        let sql = if sql.is_empty() {
            match self.history.last() {
                Some(s) => s.clone(),
                None => {
                    self.last_error = Some("nothing to watch (empty buffer, no history)".into());
                    return;
                }
            }
        } else {
            sql
        };
        // Fire the first run immediately; `last_fire` is set to the
        // past so the tick check passes on the next loop iteration.
        let interval = std::time::Duration::from_secs(2);
        self.watch = Some(WatchState {
            sql,
            interval,
            last_fire: std::time::Instant::now() - interval,
        });
        self.last_status = Some(format!(
            "\\watch every {}s · any key to stop",
            interval.as_secs()
        ));
    }

    /// Stop the active `\watch` session if any. Called from any key
    /// event so a single keypress always cancels (matches psql).
    fn cancel_watch(&mut self) {
        if self.watch.is_some() {
            self.watch = None;
            self.last_status = Some("\\watch stopped".into());
        }
    }

    /// Called once per frame tick — fires the next `\watch` run when
    /// the interval has elapsed and no query is currently in flight.
    /// Goes through the same safety pipeline as a manual Ctrl-R.
    fn tick_watch(&mut self) {
        let Some(state) = self.watch.as_ref() else {
            return;
        };
        let inputs = WatchTickInputs {
            query_running: self.query_running,
            tx_open: self.tx_open,
            pending_run: self.pending_run.is_some(),
            // Editor / Normal are the only modes that pair with a
            // running watch — every other mode is a single-shot
            // prompt or overlay that we shouldn't fire under.
            mode_blocks: !matches!(self.mode, Mode::Editor | Mode::Normal),
        };
        if !watch_should_fire(state, std::time::Instant::now(), inputs) {
            return;
        }
        let sql = state.sql.clone();
        // Stamp last_fire BEFORE dispatching so even a fast-completing
        // query doesn't fire twice within the interval.
        if let Some(s) = self.watch.as_mut() {
            s.last_fire = std::time::Instant::now();
        }
        // Stash the watch state — `request_run` reads the editor
        // buffer, so we briefly swap it in, run, and then restore.
        // (Routes through the safety pipeline like a normal run.)
        let saved_buffer = std::mem::replace(&mut self.editor_buffer, sql);
        let saved_cursor = self.editor_cursor;
        self.editor_cursor = self.editor_buffer.len();
        self.request_run(RunKind::Run);
        self.editor_buffer = saved_buffer;
        self.editor_cursor = saved_cursor.min(self.editor_buffer.len());
    }

    /// Open the data-source picker mid-session so the operator can
    /// switch connections without quitting. Requires at least one
    /// discovered data source — without that there's nothing
    /// meaningful to pick. Cancels any running query first so we
    /// don't waste a fire-and-forget run against a connection we're
    /// about to abandon. The picker's existing Enter handler does
    /// the actual reconnect.
    fn start_connection_change(&mut self) {
        if self.data_source_picks.is_empty() {
            self.last_status = Some(
                "no data sources to pick — pass --dsn or add `[[connections]]` to pgman.toml"
                    .into(),
            );
            return;
        }
        if self.query_running {
            self.cancel_running_query();
        }
        self.data_source_pick_index = 0;
        self.mode = Mode::ConnPick;
    }

    fn cancel_running_query(&mut self) {
        let Some(dispatcher) = self.cancel_dispatcher.as_ref() else {
            return;
        };
        self.last_status = Some("cancelling query…".to_string());
        dispatcher.dispatch();
    }

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
                Err(err) => AppMsg::QueryFailed {
                    generation,
                    error: err.msg,
                    position: err.position,
                },
            };
            let _ = tx.send(msg);
        });
    }

    // -- grid nav --

    /// Reset the per-grid view state — sort / filter / column cursor
    /// — so a fresh result set starts clean. Called whenever a new
    /// `Grid` lands on the App via `QueryOk` or `Booted`.
    fn reset_grid_view(&mut self) {
        self.grid_col_cursor = 0;
        self.grid_sort = None;
        self.grid_raw_rows = None;
        self.grid_filter = None;
        self.rebuild_visible_rows();
    }

    /// Recompute `grid_visible_rows` against the current `grid.rows`
    /// and `grid_filter`. Filter is a case-insensitive substring
    /// match across every column of each row. With no filter the
    /// visible set is just `0..rows.len()` (kept materialised so the
    /// render path has one code path).
    fn rebuild_visible_rows(&mut self) {
        self.grid_visible_rows =
            compute_visible_rows(&self.grid.rows, self.grid_filter.as_deref());
        // Keep the selection in range.
        let visible = self.grid_visible_rows.len();
        if visible == 0 {
            self.grid_state.select(None);
        } else {
            let cur = self.grid_state.selected().unwrap_or(0);
            self.grid_state.select(Some(cur.min(visible - 1)));
        }
    }

    /// `s` in Normal mode — cycle sort on the focused column:
    /// `off → ASC → DESC → off`. ASC takes a snapshot of the raw
    /// row order so `off` can restore it without re-running the
    /// query.
    fn cycle_sort(&mut self) {
        if self.grid.columns.is_empty() {
            return;
        }
        let col = self.grid_col_cursor.min(self.grid.columns.len() - 1);
        let next = next_sort_state(self.grid_sort, col);
        match next {
            Some((col, asc)) => {
                if self.grid_raw_rows.is_none() {
                    self.grid_raw_rows = Some(self.grid.rows.clone());
                }
                self.grid.rows.sort_by(|a, b| {
                    let av = a.get(col).map(String::as_str).unwrap_or("");
                    let bv = b.get(col).map(String::as_str).unwrap_or("");
                    let ord = crate::grid::cmp_cells(av, bv);
                    if asc {
                        ord
                    } else {
                        ord.reverse()
                    }
                });
                self.grid_sort = Some((col, asc));
                let dir = if asc { "ASC" } else { "DESC" };
                self.last_status = Some(format!(
                    "sorted by {} {dir}",
                    self.grid.columns[col]
                ));
            }
            None => {
                if let Some(raw) = self.grid_raw_rows.take() {
                    self.grid.rows = raw;
                }
                self.grid_sort = None;
                self.last_status = Some("sort cleared".into());
            }
        }
        self.rebuild_visible_rows();
    }

    /// Move the column cursor by `delta`, clamped to the grid's
    /// column range.
    fn move_col_cursor(&mut self, delta: isize) {
        let n = self.grid.columns.len();
        if n == 0 {
            return;
        }
        let cur = self.grid_col_cursor as isize;
        let next = (cur + delta).clamp(0, n as isize - 1);
        self.grid_col_cursor = next as usize;
    }

    /// `Y` in Normal mode — copy the (sorted, filtered) grid to the
    /// clipboard as CSV. `arboard` is already a dep for cell yank.
    fn export_grid_to_clipboard(&mut self) {
        if self.grid.columns.is_empty() {
            self.last_status = Some("nothing to export".into());
            return;
        }
        // Build a Grid that respects the active filter — sort is
        // already baked into `self.grid.rows`. CSV is the only
        // format here; format chooser is a follow-up.
        let mut export = crate::grid::Grid {
            columns: self.grid.columns.clone(),
            rows: Vec::with_capacity(self.grid_visible_rows.len()),
        };
        for &i in &self.grid_visible_rows {
            if let Some(row) = self.grid.rows.get(i) {
                export.rows.push(row.clone());
            }
        }
        let csv = crate::batch::format_csv(&export);
        match arboard::Clipboard::new() {
            Ok(mut cb) => match cb.set_text(csv) {
                Ok(()) => {
                    self.last_status = Some(format!(
                        "copied {} row(s) to clipboard as CSV",
                        export.rows.len()
                    ));
                }
                Err(e) => self.last_error = Some(format!("clipboard write: {e}")),
            },
            Err(e) => self.last_error = Some(format!("clipboard init: {e}")),
        }
    }

    /// `/` in Normal — enter the live grid-filter input.
    fn start_filter(&mut self) {
        if self.grid.is_empty() {
            self.last_status = Some("nothing to filter".into());
            return;
        }
        // Start from an empty pattern; whatever was previously set is
        // discarded so each `/` is a fresh search. The footer's
        // status reflects the live pattern.
        self.grid_filter = Some(String::new());
        self.rebuild_visible_rows();
        self.mode = Mode::GridFilter;
        self.refresh_filter_status();
    }

    /// `n` / `N` in Normal — step the row cursor to the next / prev
    /// matching row (only meaningful while a filter is active).
    fn filter_step(&mut self, forward: bool) {
        if self.grid_filter.is_none() {
            // Make the no-op visible — vim muscle memory expects
            // `n` to do something useful, and silent failure feels
            // like the terminal is stuck.
            self.last_status =
                Some("no active filter (press `/` to start one)".into());
            return;
        }
        // visible_rows is already the filtered set in display order;
        // step the existing cursor through it.
        let visible = self.grid_visible_rows.len();
        if visible == 0 {
            return;
        }
        let cur = self.grid_state.selected().unwrap_or(0);
        let next = if forward {
            (cur + 1).min(visible - 1)
        } else {
            cur.saturating_sub(1)
        };
        self.grid_state.select(Some(next));
    }

    /// Update the footer to reflect the live filter state, bash-style.
    fn refresh_filter_status(&mut self) {
        let pat = self.grid_filter.as_deref().unwrap_or("");
        let n = self.grid_visible_rows.len();
        let total = self.grid.rows.len();
        self.last_status = Some(format!(
            "filter: /{pat}  · {n}/{total} row(s) · enter accept · esc clear"
        ));
    }

    /// Mode::GridFilter handler. Char / Backspace edit the pattern
    /// and re-filter live; Enter accepts (stays in Normal with the
    /// filter still applied); Esc clears + returns to Normal.
    /// Walk the plan tree honouring the collapsed-set; return the
    /// flat sequence of visible rows the renderer will draw. Pure
    /// over `(plan, collapsed)` — same logic both the renderer and
    /// the key handler consult.
    pub fn flattened_explain_rows(&self) -> Vec<ExplainRow> {
        let Some(plan) = self.explain_plan.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut path = Vec::new();
        flatten_plan(plan, &mut path, 0, &self.explain_collapsed, &mut out);
        out
    }

    fn on_explain_tree_key(&mut self, key: KeyEvent) {
        let rows = self.flattened_explain_rows();
        let last = rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.explain_cursor = (self.explain_cursor + 1).min(last);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.explain_cursor = self.explain_cursor.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.explain_cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.explain_cursor = last,
            KeyCode::Enter | KeyCode::Char(' ') => {
                // Toggle collapse on the focused node, IF it has
                // children. Leaf nodes stay open (collapsing them
                // would just hide the line they're on).
                if let Some(row) = rows.get(self.explain_cursor) {
                    if row.has_children {
                        if !self.explain_collapsed.remove(&row.path) {
                            self.explain_collapsed.insert(row.path.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn on_grid_filter_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.grid_filter = None;
                self.rebuild_visible_rows();
                self.last_status = Some("filter cleared".into());
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.last_status = None;
            }
            KeyCode::Backspace => {
                if let Some(f) = self.grid_filter.as_mut() {
                    f.pop();
                }
                self.rebuild_visible_rows();
                self.refresh_filter_status();
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(f) = self.grid_filter.as_mut() {
                    f.push(c);
                }
                self.rebuild_visible_rows();
                self.refresh_filter_status();
            }
            _ => {}
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

/// Pipe `input` to `binary` over stdin and return its stdout. The
/// extracted core of `\f` reformatting — taking the binary path as
/// a parameter lets the integration test point at a PATH-stubbed
/// `fake_pg_format` without rebinding `$PATH` process-wide.
pub fn pg_format_via(input: &str, binary: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            format!(
                "{binary} not on PATH (brew install pgformatter \
                 or apt install pgformatter)"
            )
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| format!("{binary} stdin: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("{binary} failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{binary} error ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    let formatted = String::from_utf8(output.stdout)
        .map_err(|e| format!("{binary} produced non-UTF8: {e}"))?;
    Ok(formatted.trim_end_matches('\n').to_string())
}

/// Write `input` to a temp file, invoke `editor_cmd` (with optional
/// args, whitespace-split) against the file, read it back. The
/// extracted core of `\e` external-editor handling — same shape as
/// `pg_format_via`: takes the editor command as a parameter so the
/// integration test can drive a stubbed editor without `$EDITOR` env
/// gymnastics.
pub fn external_edit_via(input: &str, editor_cmd: &str) -> Result<String, String> {
    use std::process::Command;
    let (prog, args) = split_editor_command(editor_cmd);
    // Per-call temp file: pid + a monotonic counter so parallel
    // invocations (cargo's test runner spawns several test threads in
    // one process) don't collide on the same path. Cleaned up on
    // every exit branch.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "pgman-edit-{}-{}.sql",
        std::process::id(),
        seq
    ));
    std::fs::write(&path, input)
        .map_err(|e| format!("could not write temp file for {prog}: {e}"))?;
    let status = Command::new(&prog)
        .args(&args)
        .arg(&path)
        .status()
        .map_err(|e| {
            // Best-effort cleanup before surfacing the spawn failure.
            let _ = std::fs::remove_file(&path);
            format!("could not run {prog}: {e}")
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(format!(
            "{prog} exited with status {status} — buffer unchanged"
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {prog} output: {e}"))?;
    let _ = std::fs::remove_file(&path);
    Ok(text.trim_end_matches('\n').to_string())
}

/// Path to the auto-saved editor draft. Lives under
/// `util::data_dir()` (persistent across upgrades), separate from
/// the log cache.
fn draft_path() -> std::path::PathBuf {
    crate::util::data_dir().join("draft.sql")
}

/// Best-effort restore of the editor buffer from the last session.
/// `None` when the file is absent or unreadable — the operator gets
/// a fresh blank buffer in either case rather than a startup error.
pub fn load_draft() -> Option<String> {
    load_draft_from(&draft_path())
}

/// Path-parameterised core of [`load_draft`]. Lets tests use a
/// unique temp file so parallel tests don't race on the shared
/// `~/.local/share/pgman/draft.sql`.
pub fn load_draft_from(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Write the buffer atomically (via `util::write_atomic`) on quit.
/// Empty buffers still get written so a deliberate Ctrl-U + quit
/// clears the saved draft.
pub(crate) fn persist_draft(buf: &str) -> std::io::Result<()> {
    persist_draft_to(&draft_path(), buf)
}

/// Path-parameterised core of [`persist_draft`]. Same atomic-rename
/// guarantee — a crash mid-write leaves either the old file intact
/// or the new file complete, never a truncated half-write.
pub fn persist_draft_to(path: &std::path::Path, buf: &str) -> std::io::Result<()> {
    crate::util::write_atomic(path, buf)
}

/// Split `$EDITOR` into program + initial args by whitespace. Matches
/// the convention shells use when expanding `$EDITOR` — `code --wait`
/// becomes `code` with a single `--wait` arg. We don't go through a
/// shell ourselves (so no glob / quote handling) — operators with
/// quotes-or-spaces-in-paths set EDITOR_PROG / EDITOR_ARGS env vars
/// or alias to a wrapper script.
pub(crate) fn split_editor_command(s: &str) -> (String, Vec<String>) {
    let mut parts = s.split_whitespace();
    let prog = parts.next().unwrap_or("vi").to_string();
    let args: Vec<String> = parts.map(str::to_string).collect();
    (prog, args)
}

/// Build and run the effective SQL for `kind`, honouring the safety decision.
/// `is_batch` routes through `client.batch_execute` for multi-statement runs.
async fn execute(
    client: &tokio_postgres::Client,
    sql: &str,
    kind: RunKind,
    decision: &Decision,
    is_batch: bool,
) -> Result<Grid, conn::QueryErr> {
    if is_batch {
        // Only plain Run makes sense for a multi-statement script.
        if !matches!(kind, RunKind::Run) {
            return Err(conn::QueryErr::msg(format!(
                "{} not supported for multi-statement scripts",
                kind.label()
            )));
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
            // FORMAT JSON so the result is a single text cell we can
            // parse into a plan tree (rendered in Mode::ExplainTree).
            let wrapped = format!("EXPLAIN (FORMAT JSON) {sql}");
            conn::run_statement(client, &wrapped).await.map_err(|mut e| {
                // Position came back relative to the wrapped string;
                // shift it back into the user's buffer. The wrapper
                // is `EXPLAIN (FORMAT JSON) ` = 23 chars; positions
                // ≤ that point inside the wrapper itself, so drop
                // them.
                e.position = e.position.and_then(|p| p.checked_sub(23));
                e
            })
        }
        RunKind::ExplainAnalyze => {
            let wrapped = format!("EXPLAIN (ANALYZE, FORMAT JSON) {sql}");
            let result = if decision.kind.is_write() {
                // The DML inside EXPLAIN ANALYZE actually runs — wrap and
                // rollback so it never lands.
                conn::run_in_tx_rollback(client, &wrapped).await
            } else {
                conn::run_statement(client, &wrapped).await
            };
            result.map_err(|mut e| {
                e.position = e.position.and_then(|p| p.checked_sub(31)); // len("EXPLAIN (ANALYZE, FORMAT JSON) ")
                e
            })
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

    // ---- editor_complete (UI glue) -----------------------------------------

    use crate::query::schema::{SchemaCache, TableMeta};
    use crate::safety::SafetyConfig;
    use crate::theme::Theme;

    fn test_app_with_cache(tables: &[(&str, &[&str])]) -> App {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        let mut cache = SchemaCache::default();
        for (name, cols) in tables {
            cache.tables.push(TableMeta {
                schema: "public".into(),
                name: (*name).into(),
            });
            cache
                .columns_by_table
                .insert(("public".into(), (*name).into()), cols.iter().map(|s| s.to_string()).collect());
        }
        cache.schemas.push("public".into());
        a.schema_cache = cache;
        a
    }

    fn set_editor(a: &mut App, text: &str) {
        a.editor_buffer = text.into();
        a.editor_cursor = a.editor_buffer.len();
    }

    #[test]
    fn exact_match_commits_and_dismisses_popup() {
        // Cache has `user`, `users`, `user_logs`. Operator types
        // `FROM user` and Tab: the exact match commits, no popup.
        let mut a = test_app_with_cache(&[
            ("user", &["id"]),
            ("users", &["id"]),
            ("user_logs", &["id"]),
        ]);
        set_editor(&mut a, "SELECT * FROM user");
        a.editor_complete();
        assert_eq!(a.editor_buffer, "SELECT * FROM user");
        assert!(
            a.completion.is_none(),
            "exact match should dismiss the popup; got {:?}",
            a.completion.as_ref().map(|c| c.candidates.len())
        );
    }

    #[test]
    fn exact_match_is_case_insensitive_and_canonicalises_case() {
        // Operator typed `USERS`, cache has `users`, plus a sibling that
        // doesn't share the LCP `users` so the exact-match branch (not
        // single-match) is exercised.
        let mut a = test_app_with_cache(&[
            ("users", &["id"]),
            ("users_archived", &["id"]),
        ]);
        set_editor(&mut a, "SELECT * FROM USERS");
        a.editor_complete();
        assert_eq!(a.editor_buffer, "SELECT * FROM users");
        assert!(
            a.completion.is_none(),
            "exact match should dismiss the popup"
        );
    }

    fn type_key(a: &mut App, code: KeyCode) {
        a.on_editor_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn auto_trigger_after_dot_opens_popup() {
        // Two columns means LCP-popup, no auto-commit — perfect for
        // checking the auto-trigger actually opens the cycle.
        let mut a = test_app_with_cache(&[("users", &["id", "email"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT  FROM users u WHERE u");
        a.editor_cursor = 7; // between the two spaces, no cycle yet
        // Move the cursor to just after `u` of `u WHERE u` — actually,
        // type `.` at end (cursor positioned after the second `u`).
        a.editor_cursor = a.editor_buffer.len();
        type_key(&mut a, KeyCode::Char('.'));
        assert_eq!(a.editor_buffer, "SELECT  FROM users u WHERE u.");
        let cycle = a
            .completion
            .as_ref()
            .expect("auto-trigger should open a cycle after typing `.` post-identifier");
        // Columns of users via alias u.
        assert!(
            cycle.candidates.iter().any(|c| c.display == "email"),
            "expected `email` in candidates, got {:?}",
            cycle.candidates.iter().map(|c| &c.display).collect::<Vec<_>>()
        );
    }

    #[test]
    fn auto_trigger_skipped_for_numeric_literals() {
        // `3.` — the char before `.` is a digit, so auto-trigger is
        // suppressed. (No popup; status preserved.)
        let mut a = test_app_with_cache(&[("users", &["id"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT 3");
        a.last_status = Some("preserved status".into());
        type_key(&mut a, KeyCode::Char('.'));
        assert_eq!(a.editor_buffer, "SELECT 3.");
        assert!(
            a.completion.is_none(),
            "should not auto-trigger on numeric `3.`"
        );
        assert_eq!(a.last_status.as_deref(), Some("preserved status"));
    }

    #[test]
    fn dot_after_lcp_expansion_narrows_via_refresh_not_auto_trigger() {
        // Operator types `t_` Tab (expands LCP to `t_user_`), then
        // narrows by typing more chars. If they happen to type `.`
        // (unlikely but possible if a name has a `.`-shaped suffix
        // in some dialect), the live-narrowing path takes precedence
        // over auto-trigger — the existing cycle stays alive.
        let mut a = test_app_with_cache(&[
            ("t_user_logs", &["id"]),
            ("t_user_roles", &["id"]),
        ]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT * FROM t_");
        // First Tab: LCP-expands to `t_user_`, popup with 2 candidates.
        a.editor_complete();
        assert_eq!(a.editor_buffer, "SELECT * FROM t_user_");
        assert!(a.completion.as_ref().unwrap().selected.is_none());
        let cycle_id_before = a.completion.as_ref().unwrap() as *const _;
        // Now type `l` — narrowing key, cycle survives via refresh.
        type_key(&mut a, KeyCode::Char('l'));
        assert!(a.completion.is_some(), "cycle should still be alive");
        let cycle = a.completion.as_ref().unwrap();
        // The cycle was rebuilt (new pointer), but selected is still
        // None (refresh keeps pre-selection state).
        let _ = cycle_id_before; // (we don't actually compare pointers; reassuring no panic)
        assert!(cycle.selected.is_none());
        assert!(cycle.candidates.iter().any(|c| c.display == "t_user_logs"));
    }

    #[test]
    fn auto_trigger_no_matches_preserves_status() {
        // `nonsense.` — no such identifier; auto-trigger fires but
        // finds nothing and silently restores the prior status.
        let mut a = test_app_with_cache(&[("users", &["id"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT nonsense");
        a.last_status = Some("preserved status".into());
        type_key(&mut a, KeyCode::Char('.'));
        assert!(a.completion.is_none());
        assert_eq!(a.last_status.as_deref(), Some("preserved status"));
    }

    #[test]
    fn tab_on_empty_buffer_offers_statement_keywords() {
        let mut a = test_app_with_cache(&[("users", &["id"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "");
        a.editor_complete();
        let cycle = a
            .completion
            .as_ref()
            .expect("Tab on empty buffer should offer statement keywords");
        let labels: Vec<&str> = cycle.candidates.iter().map(|c| c.display.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.eq_ignore_ascii_case("SELECT")),
            "got {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.eq_ignore_ascii_case("INSERT")),
            "got {labels:?}"
        );
    }

    #[test]
    fn tab_after_from_space_offers_tables() {
        let mut a = test_app_with_cache(&[("users", &["id"]), ("orders", &["id"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT * FROM ");
        a.editor_complete();
        let cycle = a
            .completion
            .as_ref()
            .expect("Tab after `FROM ` should open tables popup");
        let labels: Vec<&str> = cycle.candidates.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.iter().any(|l| *l == "users"), "got {labels:?}");
        assert!(labels.iter().any(|l| *l == "orders"), "got {labels:?}");
    }

    #[test]
    fn auto_trigger_after_from_space_opens_tables() {
        let mut a = test_app_with_cache(&[("users", &["id"]), ("orders", &["id"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT * FROM");
        type_key(&mut a, KeyCode::Char(' '));
        assert_eq!(a.editor_buffer, "SELECT * FROM ");
        let cycle = a
            .completion
            .as_ref()
            .expect("auto-trigger should pop after `FROM `");
        let labels: Vec<&str> = cycle.candidates.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.iter().any(|l| *l == "users"));
        assert!(labels.iter().any(|l| *l == "orders"));
    }

    #[test]
    fn auto_trigger_after_and_space_opens_columns() {
        let mut a = test_app_with_cache(&[("users", &["id", "email", "name"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT * FROM users WHERE id = 1 AND");
        type_key(&mut a, KeyCode::Char(' '));
        let cycle = a
            .completion
            .as_ref()
            .expect("auto-trigger should pop after `AND `");
        let labels: Vec<&str> = cycle.candidates.iter().map(|c| c.display.as_str()).collect();
        assert!(labels.iter().any(|l| *l == "email" || *l == "name"));
    }

    #[test]
    fn auto_trigger_after_space_does_not_panic_on_multibyte_boundary_char() {
        // Regression guard: rfind on a predicate that matches a
        // multi-byte char (smart quote, en-dash, NBSP, …) would return
        // the byte index of the char's FIRST byte; `i + 1` then lands
        // in the middle of the codepoint and `&trimmed[start..]`
        // panicked. Walk char_indices.rev() instead.
        let mut a = test_app_with_cache(&[("users", &["id"])]);
        a.mode = Mode::Editor;
        // En-dash (U+2013, 3 bytes) followed by an identifier-shaped
        // word — the en-dash is the closest non-alphanumeric / non-`_`
        // char to the right of the would-be word start.
        set_editor(&mut a, "–FROM");
        // Just typing the space — we don't actually need it to fire
        // the trigger, only to not panic walking back over the en-dash.
        type_key(&mut a, KeyCode::Char(' '));
        // FROM is in the trigger list, so the popup should also open.
        assert!(
            a.completion.is_some(),
            "expected popup to open after `–FROM ` (en-dash + FROM); no panic is the main thing"
        );
    }

    #[test]
    fn auto_trigger_does_not_fire_after_arbitrary_space() {
        // After typing `5 ` (a literal followed by space), the auto-
        // trigger should NOT fire — operator is probably mid-expression
        // and a popup would be noise.
        let mut a = test_app_with_cache(&[("users", &["id"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT * FROM users WHERE id = 5");
        a.last_status = Some("preserved status".into());
        type_key(&mut a, KeyCode::Char(' '));
        assert!(
            a.completion.is_none(),
            "auto-trigger should be silent after `5 `"
        );
        assert_eq!(a.last_status.as_deref(), Some("preserved status"));
    }

    #[test]
    fn exact_match_with_only_one_candidate_still_dismisses_popup() {
        // Cache has just `users`. Operator types `FROM users` Tab.
        // cands.len() == 1, but the single-match path must NOT shadow
        // exact-match — the popup should go away because the operator
        // typed the full name.
        let mut a = test_app_with_cache(&[("users", &["id"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT * FROM users");
        a.editor_complete();
        assert_eq!(a.editor_buffer, "SELECT * FROM users");
        assert!(
            a.completion.is_none(),
            "exact match must dismiss the popup even when it's the only candidate"
        );
    }

    #[test]
    fn empty_unqualified_prefix_with_single_candidate_shows_popup_no_insert() {
        // Construct a context where empty-prefix completion yields a
        // single candidate. We can't easily get cands.len() == 1 in a
        // normal clause (the classifier extends with continuations) so
        // this is a "doesn't auto-insert" sanity check at the API
        // level: Tab on whitespace in a clean buffer offers statement
        // keywords (multiple cands), and Tab on `SELECT ` offers
        // multiple. So the property we actually want is that the
        // popup opens with selected: None — operator decides.
        let mut a = test_app_with_cache(&[("users", &["id"])]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT * FROM ");
        a.editor_complete();
        let cycle = a
            .completion
            .as_ref()
            .expect("popup should open on whitespace-Tab");
        assert!(
            cycle.selected.is_none(),
            "empty unqualified prefix must not pre-select; got {:?}",
            cycle.selected
        );
        // Buffer unchanged — no silent insert.
        assert_eq!(a.editor_buffer, "SELECT * FROM ");
    }

    // Note: auto-trigger after `.` for non-ASCII identifier endings
    // (e.g. `café.`) would benefit from the char-aware lookup in
    // `on_editor_key`, but `extract_identifier` itself walks back
    // byte-by-byte and rejects non-ASCII suffixes — so end-to-end
    // non-ASCII identifier completion is gated on widening the
    // tokenizer in a follow-up. The char-aware check here is kept
    // defensively so the auto-trigger path is correct from day one.

    #[test]
    fn backspace_to_empty_prefix_keeps_context_popup() {
        // `FROM us` Tab → popup with users-ish tables. Then the operator
        // backspaces both chars. We should NOT drop the cycle — instead
        // refresh re-extracts (empty prefix) and offers the full
        // table list for the FROM context.
        let mut a = test_app_with_cache(&[
            ("users", &["id"]),
            ("user_logs", &["id"]),
            ("orders", &["id"]),
        ]);
        a.mode = Mode::Editor;
        set_editor(&mut a, "SELECT * FROM us");
        a.editor_complete(); // LCP-expands to `user_`
        // Backspace through the entire identifier so the prefix
        // narrows to empty — the cycle should survive and broaden
        // back to the full table list for FROM.
        // After LCP-expand, buffer is `SELECT * FROM user` (the LCP
        // of users / user_logs is `user`, not `user_` — they diverge
        // at the 5th char). Four backspaces brings us to the trailing
        // space — empty prefix, still in TableRef context.
        for _ in 0..4 {
            type_key(&mut a, KeyCode::Backspace);
        }
        // Buffer should now be "SELECT * FROM " (or a substring thereof);
        // cycle should still be alive and offering tables (incl. orders).
        let cycle = a
            .completion
            .as_ref()
            .expect("cycle should survive narrowing to empty prefix");
        let labels: Vec<&str> = cycle.candidates.iter().map(|c| c.display.as_str()).collect();
        assert!(
            labels.iter().any(|l| *l == "orders"),
            "after narrowing to empty prefix, all tables should be offered; got {labels:?}"
        );
    }

    #[test]
    fn tab_with_no_candidates_falls_back_to_helpful_message() {
        // Disconnected (no cache), empty buffer: there ARE statement
        // keywords available, so the empty-cache message should NOT
        // fire — the popup opens with keywords.
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.mode = Mode::Editor;
        a.editor_complete();
        assert!(
            a.completion.is_some(),
            "empty cache + empty buffer should still offer keywords"
        );
    }

    #[test]
    fn lcp_expands_when_no_exact_match() {
        // Two tables, `user_logs` and `user_roles`. Typing `user` Tab
        // expands to the LCP `user_` (no exact match to short-circuit).
        let mut a = test_app_with_cache(&[
            ("user_logs", &["id"]),
            ("user_roles", &["id"]),
        ]);
        set_editor(&mut a, "SELECT * FROM user");
        a.editor_complete();
        assert_eq!(a.editor_buffer, "SELECT * FROM user_");
        // Cycle is in the LCP-expanded state — popup visible, nothing
        // selected yet.
        let cycle = a.completion.as_ref().expect("cycle should be alive");
        assert!(cycle.selected.is_none());
        assert_eq!(cycle.candidates.len(), 2);
    }

    #[test]
    fn query_failed_with_position_jumps_editor_cursor() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.mode = Mode::Editor;
        // Buffer with multibyte char before the position so we
        // exercise char→byte conversion. `é` is 2 bytes; `id` is at
        // chars 8..10. Postgres positions are 1-indexed chars, so
        // position 9 points at `d`.
        a.editor_buffer = "SELECT é, id FROM t".into();
        a.editor_cursor = 0;
        a.generation = 1;
        let _ = a.msg_tx.send(AppMsg::QueryFailed {
            generation: 1,
            error: "ERROR: column \"d\" does not exist".into(),
            position: Some(9),
        });
        // Pump the single queued message.
        if let Some(rx) = a.msg_rx.as_mut() {
            if let Ok(msg) = rx.try_recv() {
                a.on_msg(msg);
            }
        }
        // Position 9 (1-indexed char) → 0-indexed char 8. Byte
        // offset of char 8 in "SELECT é, id FROM t" — chars are
        // S(1) E(1) L(1) E(1) C(1) T(1) space(1) é(2)... so char 8
        // is `,` at byte 9.
        assert_eq!(a.editor_cursor, 9, "cursor should land at byte 9");
        assert!(a.last_error.as_deref().unwrap().contains("does not exist"));
    }

    #[test]
    fn history_search_finds_most_recent_match_and_walks_older() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.history = vec![
            "SELECT * FROM users".into(),
            "INSERT INTO logs VALUES (1)".into(),
            "SELECT count(*) FROM orders".into(),
            "UPDATE users SET active=true".into(),
        ];
        a.mode = Mode::Editor;
        a.editor_buffer = "draft".into();
        a.editor_cursor = a.editor_buffer.len();
        a.start_history_search();
        // Empty query → most-recent entry shown.
        assert_eq!(a.mode, Mode::HistorySearch);
        assert_eq!(a.editor_buffer, "UPDATE users SET active=true");
        // Type 'sel' through on_key so the mode dispatcher routes
        // each keystroke to the history-search handler.
        a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert_eq!(a.editor_buffer, "SELECT count(*) FROM orders");
        // Ctrl-R again → next-older match (index 0, `SELECT * FROM users`).
        a.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(a.editor_buffer, "SELECT * FROM users");
        // Enter accepts: stays in Editor with the matched buffer.
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Editor);
        assert_eq!(a.editor_buffer, "SELECT * FROM users");
        assert!(a.history_search.is_none());
    }

    #[test]
    fn history_search_esc_restores_pre_search_buffer() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.history = vec!["SELECT 1".into()];
        a.mode = Mode::Editor;
        a.editor_buffer = "draft in progress".into();
        a.editor_cursor = 5;
        a.start_history_search();
        assert_eq!(a.editor_buffer, "SELECT 1");
        // Esc: restore.
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.editor_buffer, "draft in progress");
        assert_eq!(a.editor_cursor, 5);
        assert_eq!(a.mode, Mode::Editor);
    }

    #[test]
    fn history_search_no_match_keeps_last_good_buffer() {
        // bash-like behaviour: a typo after a successful match keeps
        // the prior match on screen and surfaces the failure in status.
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.history = vec!["SELECT * FROM users".into()];
        a.mode = Mode::Editor;
        a.start_history_search();
        // 'sel' matches → buffer = SELECT...
        a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert_eq!(a.editor_buffer, "SELECT * FROM users");
        // 'selz' doesn't match → buffer unchanged; status flags failure.
        a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(a.editor_buffer, "SELECT * FROM users");
        assert!(
            a.last_status
                .as_deref()
                .unwrap_or("")
                .contains("failed reverse-i-search"),
            "expected failure status, got {:?}",
            a.last_status
        );
    }

    #[test]
    fn start_watch_uses_editor_buffer_when_set() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.mode = Mode::Editor;
        a.editor_buffer = "SELECT NOW()".into();
        a.start_watch();
        let w = a.watch.as_ref().expect("watch should be set");
        assert_eq!(w.sql, "SELECT NOW()");
        assert_eq!(w.interval.as_secs(), 2);
    }

    #[test]
    fn start_watch_falls_back_to_last_history_when_buffer_empty() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.history = vec!["SELECT 1".into(), "SELECT count(*) FROM t".into()];
        a.mode = Mode::Editor;
        a.start_watch();
        let w = a.watch.as_ref().expect("watch should be set");
        assert_eq!(w.sql, "SELECT count(*) FROM t");
    }

    #[test]
    fn start_watch_with_no_input_errors() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.mode = Mode::Editor;
        a.start_watch();
        assert!(a.watch.is_none());
        assert!(a.last_error.is_some());
    }

    #[test]
    fn start_watch_refused_during_query() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.editor_buffer = "SELECT 1".into();
        a.query_running = true;
        a.start_watch();
        assert!(a.watch.is_none());
    }

    #[test]
    fn keypress_cancels_active_watch() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.mode = Mode::Editor;
        a.watch = Some(WatchState {
            sql: "SELECT 1".into(),
            interval: std::time::Duration::from_secs(2),
            last_fire: std::time::Instant::now(),
        });
        a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(a.watch.is_none());
    }

    #[test]
    fn split_editor_command_handles_program_with_args() {
        let (p, a) = split_editor_command("code --wait --new-window");
        assert_eq!(p, "code");
        assert_eq!(a, vec!["--wait", "--new-window"]);
    }

    #[test]
    fn split_editor_command_handles_bare_program() {
        let (p, a) = split_editor_command("nvim");
        assert_eq!(p, "nvim");
        assert!(a.is_empty());
    }

    #[test]
    fn split_editor_command_defaults_to_vi_on_empty() {
        let (p, a) = split_editor_command("");
        assert_eq!(p, "vi");
        assert!(a.is_empty());
    }

    #[test]
    fn split_editor_command_collapses_internal_whitespace() {
        let (p, a) = split_editor_command("  emacs   -nw  ");
        assert_eq!(p, "emacs");
        assert_eq!(a, vec!["-nw"]);
    }

    // ---- pure decision functions ----

    #[test]
    fn watch_should_fire_respects_interval_and_blockers() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let state = WatchState {
            sql: "SELECT 1".into(),
            interval: Duration::from_secs(2),
            last_fire: now,
        };
        let clear = WatchTickInputs {
            query_running: false,
            tx_open: false,
            pending_run: false,
            mode_blocks: false,
        };
        // Same instant → interval not elapsed.
        assert!(!watch_should_fire(&state, now, clear));
        // Just past the interval → fire.
        assert!(watch_should_fire(
            &state,
            now + Duration::from_secs(2),
            clear
        ));
        // Any blocker prevents fire even past the interval.
        let fire_time = now + Duration::from_secs(10);
        for inputs in [
            WatchTickInputs { query_running: true, ..clear },
            WatchTickInputs { tx_open: true, ..clear },
            WatchTickInputs { pending_run: true, ..clear },
            WatchTickInputs { mode_blocks: true, ..clear },
        ] {
            assert!(
                !watch_should_fire(&state, fire_time, inputs),
                "blocker {inputs:?} should suppress fire"
            );
        }
    }

    #[test]
    fn next_sort_state_cycles_through_target_column() {
        assert_eq!(next_sort_state(None, 3), Some((3, true)));
        assert_eq!(next_sort_state(Some((3, true)), 3), Some((3, false)));
        assert_eq!(next_sort_state(Some((3, false)), 3), None);
        // Different column → jump to ASC on the new one.
        assert_eq!(next_sort_state(Some((3, true)), 5), Some((5, true)));
        assert_eq!(next_sort_state(Some((3, false)), 5), Some((5, true)));
    }

    #[test]
    fn compute_visible_rows_filters_case_insensitively_across_columns() {
        let rows = vec![
            vec!["1".into(), "alice".into()],
            vec!["2".into(), "BOB".into()],
            vec!["3".into(), "carol".into()],
        ];
        // No filter → all rows in order.
        assert_eq!(compute_visible_rows(&rows, None), vec![0, 1, 2]);
        // Match in column 1, case-insensitive.
        assert_eq!(compute_visible_rows(&rows, Some("bo")), vec![1]);
        // Match in column 0 (numeric column).
        assert_eq!(compute_visible_rows(&rows, Some("3")), vec![2]);
        // No matches.
        assert!(compute_visible_rows(&rows, Some("xyz")).is_empty());
    }

    #[test]
    fn history_search_next_walks_backward_case_insensitive() {
        let history: Vec<String> = vec![
            "SELECT 1".into(),
            "INSERT INTO logs VALUES (1)".into(),
            "SELECT * FROM users".into(),
            "UPDATE accounts SET balance=0".into(),
        ];
        // From end, "sel" finds idx 2 (most recent SELECT).
        assert_eq!(history_search_next(&history, "sel", None), Some(2));
        // From before that match, finds idx 0.
        assert_eq!(history_search_next(&history, "sel", Some(2)), Some(0));
        // Past the earliest match → None.
        assert_eq!(history_search_next(&history, "sel", Some(0)), None);
        // Case-insensitive match.
        assert_eq!(history_search_next(&history, "INSERT", None), Some(1));
        assert_eq!(history_search_next(&history, "insert", None), Some(1));
    }

    fn app_with_grid(grid: Grid) -> App {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.grid = grid;
        a.reset_grid_view();
        a.grid_state.select(if a.grid.is_empty() { None } else { Some(0) });
        a
    }

    fn sample_grid() -> Grid {
        Grid {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                vec!["3".into(), "carol".into()],
                vec!["1".into(), "alice".into()],
                vec!["10".into(), "bob".into()],
                vec!["2".into(), "dave".into()],
            ],
        }
    }

    #[test]
    fn cycle_sort_orders_numerically_asc_then_desc_then_off() {
        let mut a = app_with_grid(sample_grid());
        // Column cursor defaults to 0 (id). ASC: 1, 2, 3, 10.
        a.cycle_sort();
        let ids: Vec<&str> = a.grid.rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(ids, vec!["1", "2", "3", "10"]);
        assert_eq!(a.grid_sort, Some((0, true)));
        // DESC: 10, 3, 2, 1.
        a.cycle_sort();
        let ids: Vec<&str> = a.grid.rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(ids, vec!["10", "3", "2", "1"]);
        // Off: original order restored.
        a.cycle_sort();
        let ids: Vec<&str> = a.grid.rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(ids, vec!["3", "1", "10", "2"]);
        assert!(a.grid_sort.is_none());
    }

    #[test]
    fn cycle_sort_on_different_column_jumps_to_asc() {
        let mut a = app_with_grid(sample_grid());
        a.cycle_sort(); // col 0 ASC
        a.move_col_cursor(1);
        a.cycle_sort(); // col 1 ASC (NOT col 0 DESC)
        assert_eq!(a.grid_sort, Some((1, true)));
        let names: Vec<&str> = a.grid.rows.iter().map(|r| r[1].as_str()).collect();
        assert_eq!(names, vec!["alice", "bob", "carol", "dave"]);
    }

    #[test]
    fn filter_narrows_visible_rows_case_insensitively() {
        let mut a = app_with_grid(sample_grid());
        a.start_filter();
        // Type 'AL' — case-insensitive substring across all columns.
        a.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        a.on_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
        // Only `alice` (row idx 1) matches.
        assert_eq!(a.grid_visible_rows, vec![1]);
        // Enter accepts; filter persists.
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.grid_filter.as_deref(), Some("aL"));
    }

    #[test]
    fn filter_esc_clears_pattern_and_restores_visible_rows() {
        let mut a = app_with_grid(sample_grid());
        a.start_filter();
        a.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(a.grid_visible_rows.is_empty());
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.grid_visible_rows.len(), 4);
        assert!(a.grid_filter.is_none());
    }

    #[test]
    fn selected_grid_row_idx_maps_through_filter() {
        let mut a = app_with_grid(sample_grid());
        a.grid_filter = Some("a".into()); // matches alice, carol, dave
        a.rebuild_visible_rows();
        // visible_rows holds indices into grid.rows for matches in
        // original order: carol(0), alice(1), dave(3).
        assert_eq!(a.grid_visible_rows, vec![0, 1, 3]);
        a.grid_state.select(Some(1)); // second visible row → alice
        assert_eq!(a.selected_grid_row_idx(), Some(1));
    }

    fn explain_app_with_plan() -> App {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        let json = r#"[{
          "Plan": {
            "Node Type": "Hash Join",
            "Total Cost": 200.0,
            "Actual Total Time": 50.0,
            "Plans": [
              { "Node Type": "Seq Scan", "Relation Name": "a",
                "Total Cost": 100.0, "Actual Total Time": 30.0 },
              { "Node Type": "Hash", "Total Cost": 22.5,
                "Actual Total Time": 5.0,
                "Plans": [
                  { "Node Type": "Seq Scan", "Relation Name": "b",
                    "Total Cost": 22.5, "Actual Total Time": 4.0 }
                ]
              }
            ]
          }
        }]"#;
        let plan = crate::query::explain::parse(json).unwrap();
        a.explain_plan = Some(plan);
        a.mode = Mode::ExplainTree;
        a
    }

    #[test]
    fn flattened_explain_lists_each_node_once() {
        let a = explain_app_with_plan();
        let rows = a.flattened_explain_rows();
        assert_eq!(rows.len(), 4); // root + 3 descendants
        assert_eq!(rows[0].node_type, "Hash Join");
        assert_eq!(rows[1].node_type, "Seq Scan");
        assert_eq!(rows[2].node_type, "Hash");
        assert_eq!(rows[3].node_type, "Seq Scan");
        // Depths.
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].depth, 1);
        assert_eq!(rows[3].depth, 2);
    }

    #[test]
    fn explain_enter_collapses_focused_node_and_hides_children() {
        let mut a = explain_app_with_plan();
        // Focus row 2 (the "Hash" node, which has children).
        a.explain_cursor = 2;
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let rows = a.flattened_explain_rows();
        // Hash's child Seq Scan is hidden now.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].node_type, "Hash");
        assert!(rows[2].collapsed);
        // Toggle back.
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let rows = a.flattened_explain_rows();
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn explain_jk_moves_cursor_g_jumps_to_ends() {
        let mut a = explain_app_with_plan();
        // j down to last row.
        for _ in 0..10 {
            a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        assert_eq!(a.explain_cursor, 3); // clamped to last
        a.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(a.explain_cursor, 0);
        a.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        assert_eq!(a.explain_cursor, 3);
    }

    #[test]
    fn explain_esc_returns_to_normal() {
        let mut a = explain_app_with_plan();
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn explain_enter_on_leaf_node_is_a_noop() {
        let mut a = explain_app_with_plan();
        a.explain_cursor = 1; // leaf Seq Scan on `a`
        let before = a.flattened_explain_rows().len();
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let after = a.flattened_explain_rows().len();
        assert_eq!(before, after);
        assert!(a.explain_collapsed.is_empty());
    }

    #[test]
    fn start_connection_change_with_picks_opens_picker() {
        let pick = DataSourcePick {
            name: "primary".into(),
            origin: "test",
            dsn: Dsn::parse("postgres://app@db/x").unwrap(),
        };
        let mut a = App::new(
            Theme::default(),
            None,
            vec![pick],
            SafetyConfig::default(),
        );
        a.mode = Mode::Normal;
        a.start_connection_change();
        assert_eq!(a.mode, Mode::ConnPick);
        assert_eq!(a.data_source_pick_index, 0);
    }

    #[test]
    fn start_connection_change_with_no_picks_surfaces_hint() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.mode = Mode::Normal;
        a.start_connection_change();
        assert_eq!(a.mode, Mode::Normal);
        assert!(a
            .last_status
            .as_deref()
            .unwrap_or("")
            .contains("no data sources"));
    }

    // Draft persistence is exercised end-to-end via util::write_atomic
    // (which has its own roundtrip test) + the trivial wrapper here.
    // A test that touches the real `draft_path` races against parallel
    // tests since they all share the same HOME-derived location;
    // skipping in favour of the util-level coverage.

    /// Recording fake cancel-dispatcher. The actual `dispatch`
    /// closure is fire-and-forget in production; in tests we just
    /// count calls.
    #[derive(Debug, Default)]
    struct RecordingDispatcher {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl CancelDispatcher for RecordingDispatcher {
        fn dispatch(&self) {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn cancel_running_query_dispatches_through_injected_handler() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        a.cancel_dispatcher = Some(Box::new(RecordingDispatcher {
            calls: calls.clone(),
        }));
        a.query_running = true;
        a.mode = Mode::Editor;

        // Ctrl-C with a running query routes through the dispatcher.
        a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(a.last_status.as_deref(), Some("cancelling query…"));
    }

    #[test]
    fn cancel_running_query_no_dispatcher_no_op() {
        // Without a dispatcher (e.g. not connected) Ctrl-C is a
        // silent no-op rather than a panic.
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.cancel_dispatcher = None;
        a.query_running = true;
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        // No panic, status not flipped (function returned at the
        // `None` guard before setting it).
        assert!(a.last_status.is_none());
    }

    #[test]
    fn cancel_running_query_idle_skips_dispatcher() {
        // Ctrl-C only fires the cancel when `query_running` — gated
        // at the keybinding level. With no running query, the
        // dispatcher should not be called.
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        a.cancel_dispatcher = Some(Box::new(RecordingDispatcher {
            calls: calls.clone(),
        }));
        a.query_running = false;
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn pg_notice_lands_in_status_and_history() {
        use crate::conn::NoticeMsg;
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        let n = NoticeMsg {
            severity: "NOTICE".into(),
            message: "function returned: 42".into(),
            detail: None,
            hint: None,
        };
        // Tag with the App's current generation so on_msg accepts it.
        let _ = a.msg_tx.send(AppMsg::Notice {
            generation: a.generation,
            notice: n,
        });
        if let Some(rx) = a.msg_rx.as_mut() {
            if let Ok(msg) = rx.try_recv() {
                a.on_msg(msg);
            }
        }
        assert_eq!(a.notices.len(), 1);
        assert!(a
            .last_status
            .as_deref()
            .unwrap_or("")
            .contains("function returned: 42"));
    }

    #[test]
    fn notice_buffer_caps_at_50() {
        use crate::conn::NoticeMsg;
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        for i in 0..60 {
            a.on_msg(AppMsg::Notice {
                generation: a.generation,
                notice: NoticeMsg {
                    severity: "NOTICE".into(),
                    message: format!("msg #{i}"),
                    detail: None,
                    hint: None,
                },
            });
        }
        assert_eq!(a.notices.len(), 50);
        // Oldest dropped — first kept is msg #10.
        assert_eq!(a.notices.first().unwrap().message, "msg #10");
        assert_eq!(a.notices.last().unwrap().message, "msg #59");
    }

    #[test]
    fn ctrl_r_on_empty_history_no_ops_with_status() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.mode = Mode::Editor;
        // No history.
        a.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(a.mode, Mode::Editor);
        assert!(a.history_search.is_none());
        assert_eq!(a.last_status.as_deref(), Some("history is empty"));
    }

    #[test]
    fn query_failed_with_position_past_buffer_clamps_to_end() {
        let mut a = App::new(
            Theme::default(),
            None,
            Vec::new(),
            SafetyConfig::default(),
        );
        a.editor_buffer = "SELECT 1".into();
        a.editor_cursor = 0;
        a.generation = 1;
        let _ = a.msg_tx.send(AppMsg::QueryFailed {
            generation: 1,
            error: "boom".into(),
            position: Some(999),
        });
        if let Some(rx) = a.msg_rx.as_mut() {
            if let Ok(msg) = rx.try_recv() {
                a.on_msg(msg);
            }
        }
        assert_eq!(a.editor_cursor, a.editor_buffer.len());
    }
}
