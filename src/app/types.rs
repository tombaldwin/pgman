//! Supporting type definitions for the application state.
//!
//! Relocated verbatim from `app.rs` during decomposition; `app.rs` retains
//! the `App` struct, its `impl` blocks, free functions, and the run loop.

use super::*;

/// Shared list-cursor navigation for the panel state structs that own
/// both their rows and the cursor into them. Centralises the in-range
/// clamp (previously hand-written at each key handler).
pub trait ListCursorNav {
    fn nav_len(&self) -> usize;
    fn nav_cursor(&self) -> usize;
    fn nav_set(&mut self, i: usize);

    fn select_next(&mut self) {
        self.nav_set((self.nav_cursor() + 1).min(self.nav_len().saturating_sub(1)));
    }
    fn select_prev(&mut self) {
        self.nav_set(self.nav_cursor().saturating_sub(1));
    }
    fn select_first(&mut self) {
        self.nav_set(0);
    }
    fn select_last(&mut self) {
        self.nav_set(self.nav_len().saturating_sub(1));
    }
    fn page_down(&mut self) {
        self.nav_set((self.nav_cursor() + 10).min(self.nav_len().saturating_sub(1)));
    }
    fn page_up(&mut self) {
        self.nav_set(self.nav_cursor().saturating_sub(10));
    }
}

/// A borrowed `(cursor, len)` pair adapting the shared [`ListCursorNav`]
/// clamp logic to a panel that keeps its cursor on its own state struct but
/// computes its row count elsewhere (an `App` method over the schema cache or
/// the tap ring), so the length isn't a field the struct can return itself.
pub struct CursorAt<'a> {
    pub cursor: &'a mut usize,
    pub len: usize,
}

impl ListCursorNav for CursorAt<'_> {
    fn nav_len(&self) -> usize {
        self.len
    }
    fn nav_cursor(&self) -> usize {
        *self.cursor
    }
    fn nav_set(&mut self, i: usize) {
        *self.cursor = i;
    }
}

/// Which view the LogPick popup is rendering. Toggle with `c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogPickView {
    /// One row per reconstructed query, in log order. Default.
    #[default]
    AllQueries,
    /// One row per N+1 cluster, ordered by count desc. Each row
    /// shows the cluster's count + a representative SQL.
    Clusters,
}

/// Top-level interaction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Each char updates `grid_view.filter` and re-filters live so the
    /// operator sees results as they type. Enter accepts; Esc
    /// clears the filter.
    GridFilter,
    /// In-grid find (`f` from Normal) — highlights matching cells
    /// and jumps the cursor between them. Distinct from
    /// `GridFilter` because it shows surrounding rows.
    GridFind,
    /// Tree view of the most recent EXPLAIN / EXPLAIN ANALYZE plan.
    /// Opened automatically when Ctrl-E / Ctrl-A succeeds and the
    /// JSON parses; j/k navigate, Enter expand/collapse, Esc closes.
    ExplainTree,
    /// Schema browser overlay (`S` from Normal). Tree of schemas →
    /// tables on the left; columns + constraints for the focused
    /// table on the right. psql `\d` equivalent, served from the
    /// schema cache so no live queries are issued.
    SchemaBrowser,
    /// In-tree search input for the schema browser (`/` from
    /// SchemaBrowser). Each typed char narrows the visible tree in
    /// place. Enter accepts (returns to SchemaBrowser with the
    /// filter still applied); Esc clears.
    SchemaBrowserFilter,
    /// Top-N slow queries from `pg_stat_statements` (`T` from Normal).
    /// One row per stored statement; Enter copies the SQL into the
    /// editor for tuning; `r` refreshes.
    SlowQueries,
    /// Active sessions + locks (`L` from Normal). Lists
    /// `pg_stat_activity` rows with `pg_blocking_pids()` joined in;
    /// blocked sessions sort to the top. `r` refreshes.
    Sessions,
    /// Schema "wizard" / lint (`W` from Normal). Runs
    /// `query::lint::run_all` against the schema cache and lists
    /// findings (missing PK, mixed-case identifiers, reserved-
    /// keyword names, naming-convention drift, …). Pure / no live
    /// queries; opens in microseconds.
    SchemaLint,
    /// Rich error-detail overlay (`F2` after a query failure).
    /// Renders the full server-side `DbError`: severity, code,
    /// message, detail, hint, where, affected schema/table/
    /// column/constraint/type. Read-only; closes on F2/esc/q.
    ErrorDetail,
    /// `pg_terminate_backend(pid)` confirmation prompt — y fires
    /// the terminate spawn, n/esc cancels. `App::pending_terminate`
    /// carries the target pid.
    ConfirmTerminate,
    /// LISTEN / NOTIFY arrivals panel (`N` from Normal). Lists
    /// `App::notifications` (channel · pid · payload). Operator
    /// subscribes via `LISTEN <channel>` in the editor; arrivals
    /// land here automatically. `c` clears the ring; `q`/`esc`
    /// close.
    Notifications,
    /// Saved-queries panel — list of named SQL snippets (`Ctrl-O`
    /// from Editor or `Q` from Normal). Enter loads the focused
    /// entry into the editor; `d` deletes (with confirm); esc/q
    /// close.
    SavedQueries,
    /// Name prompt for `Ctrl-S` save-current-buffer. Operator
    /// types a name; Enter persists; Esc cancels.
    SaveQueryPrompt,
    /// `:param` value prompt shown when loading a saved query that
    /// contains named placeholders. One prompt per distinct
    /// placeholder; Enter advances, Esc cancels back to the list.
    /// State lives in `App::saved_ui.param_prompt`.
    ParamPrompt,
    /// Live substring search over the saved-queries panel (`/`
    /// from `SavedQueries`). Each char narrows the list in place;
    /// Enter accepts (keeps the filter), Esc clears it.
    SavedQueriesFilter,
    /// Rename prompt for the focused saved query (`r` from
    /// `SavedQueries`). Edits `App::saved_ui.rename_buf`; Enter
    /// commits (refused on name collision), Esc cancels.
    RenameQueryPrompt,
    /// JDBC-tap event monitor (`F4` from anywhere). Lists
    /// `App::tap_events` in recency order — time, duration,
    /// app, sql preview. j/k move; enter drills into one event;
    /// c clears the ring; q/esc close.
    TapMonitor,
    /// Result-diff view (`D` in Normal pins A, the next `D`
    /// diffs the current grid as B). Shows added / removed /
    /// changed rows keyed by an inferred unique column (or
    /// full-row identity). j/k navigate; `r` re-pins B as the
    /// new A; `c` clears the pin; q/esc close.
    ResultDiff,
}

impl Mode {
    /// True while the operator is typing literal text into a buffer —
    /// the editor or any single-line prompt. Global single-key / `Ctrl`
    /// chords (notably `Ctrl-W` close-tab) must stay inert in these
    /// modes so they don't fire mid-typing and clobber the buffer or
    /// the tab. EVERY prompt mode must appear here — a new prompt that
    /// forgets to opt in re-introduces the close-tab-while-typing bug.
    pub fn is_text_input(self) -> bool {
        matches!(
            self,
            Mode::Editor
                | Mode::HistorySearch
                | Mode::GridFilter
                | Mode::GridFind
                | Mode::SchemaBrowserFilter
                | Mode::SaveQueryPrompt
                | Mode::ParamPrompt
                | Mode::SavedQueriesFilter
                | Mode::RenameQueryPrompt
        )
    }
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
/// Grouped SQL-editor state. Shared by `App` (the live editor) and
/// `TabSnapshot` (per-tab saved editor state), so tab snapshot /
/// restore is a single struct clone.
#[derive(Debug, Clone, Default)]
pub struct EditorState {
    /// SQL editor buffer; `\n` separates lines.
    pub buffer: String,
    /// Byte offset of the cursor within `buffer`.
    pub cursor: usize,
    /// Remembered char-column for vertical motion (Up/Down). `None` outside a
    /// vertical-motion run; cleared by any other edit or horizontal move.
    pub preferred_col: Option<usize>,
    /// Vertical scroll offset (lines hidden above the viewport) for the
    /// editor pane. The renderer auto-adjusts this each frame to keep
    /// the cursor's line visible; the field is plain state (not derived)
    /// so the renderer doesn't have to recompute from scratch when the
    /// buffer changes between frames.
    pub scroll: u16,
    /// Undo ring of pre-mutation `(buffer, cursor)` snapshots. Ctrl-Z
    /// pops; Ctrl-Y / Ctrl-Shift-Z redoes. Capped at `UNDO_CAP`.
    pub undo: Vec<UndoEntry>,
    /// Redo ring — filled by `editor_undo` and drained by `editor_redo`.
    /// Any new editor mutation invalidates redo (standard editor
    /// behaviour: divergent edit = new history branch).
    pub redo: Vec<UndoEntry>,
}

/// Grid view-metadata: the derived/display state that travels with a
/// result grid (cursor column, sort, filter, row-source). Shared by
/// `App` (the live grid) and `TabSnapshot` (per-tab persistence). The
/// `Grid` data itself and the `TableState` are NOT here — they stay as
/// flat fields because they are the hot read path.
#[derive(Debug, Clone, Default)]
pub struct GridView {
    /// Column under the cursor in the results grid. h/l move it; sort
    /// + future column-aware actions operate on this column.
    pub col_cursor: usize,
    /// Sort state for the grid: `None` = display order from the
    /// query; `Some((col, asc))` = sorted by that column. Cycled by
    /// `s` in Normal mode: off → ASC → DESC → off.
    pub sort: Option<(usize, bool)>,
    /// The grid as it landed from the query — preserved so a "clear
    /// sort" can restore the original row order without re-running.
    pub raw_rows: Option<Vec<Vec<String>>>,
    /// Active row-filter pattern (case-insensitive substring across
    /// all columns). `None` = no filter; rendered rows are
    /// `visible_rows` indices into the (possibly sorted) `grid.rows`.
    pub filter: Option<String>,
    /// Indices into `grid.rows` for the currently-visible rows under
    /// the active filter. Equal to `0..rows.len()` when no filter is
    /// set. Rebuilt whenever filter / sort / grid changes.
    pub visible_rows: Vec<usize>,
    /// `Some((schema, table))` when the current grid is the result
    /// of a single-FROM-table SELECT, `None` otherwise. Drives the
    /// row-as-INSERT yank — and, eventually, cell-edit-to-UPDATE +
    /// FK navigation.
    pub source: Option<(String, String)>,
}

/// Grid find ("/" search) state. Lives on `App` only — find is a
/// transient navigation aid and is NOT persisted per-tab.
#[derive(Debug, Default)]
pub struct GridFind {
    /// Pattern being typed / accepted in `Mode::GridFind`. `Some`
    /// means find is active; the matches list below is rebuilt
    /// from this on every change.
    pub needle: Option<String>,
    /// Match cursor positions for the find, in row-major
    /// order: each pair is `(visible_row_index, col_index)`.
    pub matches: Vec<(usize, usize)>,
    /// Current position in `matches` — `n` advances, `N`
    /// retreats; both wrap.
    pub pos: usize,
}

/// Per-tab snapshot of the editor + result-grid state. The
/// connection, schema cache, history, saved queries, theme,
/// notifications, and safety profile are SHARED across tabs and
/// live directly on App.
///
/// Invariant: the active tab's state lives in App's existing
/// per-session fields (`editor`, `grid`, `grid_state`,
/// …). When the operator switches, the live fields are
/// snapshot-copied into `tabs[old_active]` and `tabs[new_active]`
/// is loaded back in. Existing read sites keep using App's
/// fields unchanged — multi-tab is invisible to them.
#[derive(Debug, Clone, Default)]
pub struct TabSnapshot {
    pub editor: EditorState,
    pub grid: crate::grid::Grid,
    pub grid_selected: Option<usize>,
    pub grid_view: GridView,
    pub last_run_sql: Option<String>,
    /// Diff baseline ("A") pinned with `D`. Per-tab so pinning in one
    /// tab can't leak into another (a fresh tab starts unpinned). The
    /// transient `Mode::ResultDiff` overlay is NOT snapshotted — it is
    /// dismissed on any tab change (see `dismiss_result_diff`).
    pub pinned_result: Option<PinnedResult>,
    /// Grid bookmarks (`m<x>` / `'<x>`). Per-tab and keyed by row index
    /// into this tab's grid, so a bookmark set in one tab can't resolve
    /// against another tab's result (different grid at the same index).
    /// Cleared when the tab's grid is replaced (see QueryOk / Booted).
    pub bookmarks: std::collections::HashMap<char, GridBookmark>,
}

/// One row of the bootstrap "every database's name + size" overview
/// (`BOOTSTRAP_SQL`). Feeds the start card's `databases` line — the
/// bootstrap result never lands in `App.grid`, so the start card
/// survives every real connect instead of being replaced by a grid of
/// database names. App-level (not per-tab): every tab shares the same
/// connection's database list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseInfo {
    pub name: String,
    pub size: String,
}

/// A vim-style bookmark on the result grid — snapshot of the
/// `(visible row index, column cursor)` at the time `m<x>` was
/// pressed. Persistence intentionally narrow: tracking the
/// underlying SQL / row identity would let bookmarks survive
/// re-runs, but is overkill for the analytical-session use
/// case the feature targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridBookmark {
    pub row: usize,
    pub col: usize,
}

/// A result set frozen as the diff baseline ("A"). Holds the
/// columns + rows needed to render a diff against a later result
/// without touching the live grid, plus a short header label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Short label for the header — the SQL that produced A
    /// (collapsed / truncated) or a fallback.
    pub label: String,
}

/// Frozen state behind `Mode::ResultDiff`: the baseline (A), a
/// snapshot of the current result (B), the key strategy chosen,
/// and the computed diff. Snapshotting B keeps the view stable
/// and lets the renderer pull full rows by index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultDiffState {
    pub a: PinnedResult,
    pub b_columns: Vec<String>,
    pub b_rows: Vec<Vec<String>>,
    pub b_label: String,
    pub key: crate::query::row_diff::RowKey,
    pub diff: crate::query::row_diff::RowDiff,
}
/// In-progress `:param` collection while loading a saved query
/// that carries named placeholders. The operator answers one
/// prompt per distinct placeholder; when the last is filled the
/// template is substituted and loaded into the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamPrompt {
    /// Saved-query name, for the modal title / status.
    pub query_name: String,
    /// The SQL template with `:name` placeholders still in it.
    pub template: String,
    /// Distinct placeholder names, in first-appearance order.
    pub params: Vec<String>,
    /// Index of the placeholder currently being entered.
    pub idx: usize,
    /// Values already entered (aligned with `params[0..idx]`).
    pub values: Vec<String>,
    /// Current input buffer for `params[idx]`.
    pub input: TextInput,
}
/// One visible row in the flattened schema browser. Tree shape:
///   Schema (level 0)
///     └─ Table (level 1)
///          └─ Column (level 2)
///          └─ Constraint (level 2)
/// Schemas and tables both collapse/expand; the `expanded`-set keys
/// are `"schema"` for schemas and `"schema.table"` for tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaBrowserRow {
    Schema {
        name: String,
        expanded: bool,
        table_count: usize,
    },
    Table {
        schema: String,
        name: String,
        expanded: bool,
        column_count: usize,
        constraint_count: usize,
    },
    Column {
        schema: String,
        table: String,
        name: String,
    },
    Constraint {
        schema: String,
        table: String,
        name: String,
    },
}
/// Search direction for [`next_schema_row_idx`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
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
    /// For a Spring pick with an unresolved `${...}` in its url/username,
    /// this already carries a trailing `— unresolved ${NAME}` note (see
    /// `main.rs::discover_spring_datasources`) so the picker row surfaces
    /// it without the row-rendering code needing to know about
    /// `unresolved` itself.
    pub name: String,
    /// Where this pick came from, for the operator's benefit
    /// (e.g. "IntelliJ" / "Spring").
    pub origin: &'static str,
    /// Resolved DSN, ready to hand to `connect_and_bootstrap`. For a
    /// Spring pick with entries in `unresolved`, the url/username may
    /// still carry the literal `${NAME}` text — connecting is refused
    /// before this DSN is used (see `App::refuse_if_unresolved`).
    pub dsn: Dsn,
    /// `${NAME}` placeholders (from url / username only — never the
    /// password, see below) that discovery couldn't resolve from the
    /// environment. Non-empty means this pick must not be connected to
    /// as-is. Password-only unresolved placeholders are deliberately
    /// NOT recorded here: `PGPASSWORD` / a project's `password_env`
    /// are pgman's own, already-documented way to supply a password,
    /// so an unresolved `${db.password}` isn't a discovery failure.
    pub unresolved: Vec<String>,
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

/// Tap-monitor navigation state — the active sub-view, sort, and the
/// per-view cursors. Grouped so the cursors reset together (see
/// `reset_cursors`) instead of being hand-listed at each call site.
#[derive(Debug, Default)]
pub struct TapNavUi {
    pub view: TapView,                 // was tap_view
    pub sort: crate::tap::HotspotSort, // was tap_sort
    pub events_cursor: usize,          // was tap_events_cursor
    pub hotspots_cursor: usize,
    pub callers_cursor: usize,
    pub txns_cursor: usize,
    pub pools_cursor: usize,
    pub baseline_cursor: usize,
    pub nplus1_cursor: usize,
}
impl TapNavUi {
    /// Reset every per-view cursor to the top (the ring backs all views).
    pub fn reset_cursors(&mut self) {
        self.events_cursor = 0;
        self.hotspots_cursor = 0;
        self.callers_cursor = 0;
        self.txns_cursor = 0;
        self.pools_cursor = 0;
        self.baseline_cursor = 0;
        self.nplus1_cursor = 0;
    }
}

/// Modal/interaction state for the saved-queries panel and its prompts.
/// The store itself (`App::saved_queries`) stays separate — only the UI
/// cursor / typed buffers / active prompts live here.
#[derive(Debug, Default)]
pub struct SavedQueriesUi {
    /// Cursor into `saved_queries.entries` for the panel.
    pub cursor: usize,
    /// Name being typed in `Mode::SaveQueryPrompt`.
    pub save_name: String,
    /// Active `:param` collection while loading a parameterised
    /// saved query (`Mode::ParamPrompt`). `None` otherwise.
    pub param_prompt: Option<ParamPrompt>,
    /// Live substring filter for the saved-queries panel
    /// (`Mode::SavedQueriesFilter`). `None` = show everything.
    /// Matches case-insensitively on name OR body.
    pub filter: Option<TextInput>,
    /// Input buffer for `Mode::RenameQueryPrompt` (the new name
    /// being typed).
    pub rename_buf: TextInput,
    /// The original name being renamed.
    pub rename_from: String,
}

/// Schema-browser navigation/modal state — cursor, in-tree filter, and
/// the set of expanded schema/table nodes.
#[derive(Debug, Default)]
pub struct SchemaBrowserUi {
    /// Schema browser cursor — index into the flattened
    /// (post-expand-state) row list.
    pub cursor: usize,
    /// Active in-tree filter; `Some` when typing or accepted (Enter
    /// keeps it applied). `None` = no filter. Empty string while in
    /// SchemaBrowserFilter mode is fine — it just means "show
    /// everything until the operator types something."
    pub filter: Option<String>,
    /// Names of schemas the operator has expanded. Schemas start
    /// collapsed; the operator picks which to drill into.
    pub expanded: std::collections::HashSet<String>,
}

/// Log-import pick state — the reconstructed queries from the most recent
/// import, the active view, the cached cluster list, and the selected entry.
#[derive(Debug, Default)]
pub struct LogPickUi {
    /// Reconstructed queries from the most recent log-import; `Mode::LogPick`
    /// browses these.
    pub picks: Vec<ReconstructedQuery>,
    /// Which view LogPick is currently rendering — toggle with `c`.
    pub view: LogPickView,
    /// Cached cluster list for the Clusters view. Rebuilt on
    /// `picks` set and on view toggle so repeated j/k keystrokes
    /// don't re-cluster on each frame.
    pub clusters: Vec<crate::query::nplus1::Cluster>,
    /// Selected entry in `picks`.
    pub index: usize,
}

impl ListCursorNav for LogPickUi {
    fn nav_len(&self) -> usize {
        match self.view {
            LogPickView::AllQueries => self.picks.len(),
            LogPickView::Clusters => self.clusters.len(),
        }
    }
    fn nav_cursor(&self) -> usize {
        self.index
    }
    fn nav_set(&mut self, i: usize) {
        self.index = i;
    }
}

/// EXPLAIN-tree state — the parsed plan, the cursor into the flattened
/// (visible-after-collapses) list, and the set of collapsed node paths.
#[derive(Debug, Default)]
pub struct ExplainUi {
    /// Most recent EXPLAIN / EXPLAIN ANALYZE plan, when `Mode::ExplainTree`
    /// is active. Built from `EXPLAIN (FORMAT JSON)` output on a
    /// successful run.
    pub plan: Option<crate::query::explain::PlanNode>,
    /// Cursor into the flattened (visible-after-collapses) plan list.
    /// j/k move it; Enter toggles collapse on the focused node.
    pub cursor: usize,
    /// Paths (chains of child-array indices from the root) of nodes
    /// the operator has collapsed. The renderer hides anything below
    /// these.
    pub collapsed: std::collections::HashSet<Vec<usize>>,
}

/// Slow-queries panel state — the most recent `pg_stat_statements`
/// snapshot and the cursor into it.
#[derive(Debug, Default)]
pub struct SlowQueriesUi {
    /// Most recent `pg_stat_statements` snapshot, when
    /// `Mode::SlowQueries` is active.
    pub rows: Vec<crate::query::slow_queries::SlowQueryRow>,
    pub cursor: usize,
}

impl ListCursorNav for SlowQueriesUi {
    fn nav_len(&self) -> usize {
        self.rows.len()
    }
    fn nav_cursor(&self) -> usize {
        self.cursor
    }
    fn nav_set(&mut self, i: usize) {
        self.cursor = i;
    }
}

/// Sessions panel state — the most recent `pg_stat_activity` snapshot
/// and the cursor into it.
#[derive(Debug, Default)]
pub struct SessionsUi {
    /// Most recent `pg_stat_activity` snapshot, when
    /// `Mode::Sessions` is active.
    pub rows: Vec<crate::query::sessions::SessionRow>,
    pub cursor: usize,
}

impl ListCursorNav for SessionsUi {
    fn nav_len(&self) -> usize {
        self.rows.len()
    }
    fn nav_cursor(&self) -> usize {
        self.cursor
    }
    fn nav_set(&mut self, i: usize) {
        self.cursor = i;
    }
}

/// Notifications panel state — the ring buffer of recent `NOTIFY`
/// arrivals and the cursor into it.
#[derive(Debug, Default)]
pub struct NotificationsUi {
    /// Ring buffer of recent `NOTIFY` arrivals from the server.
    /// Newest at the end. Capped at `NOTIFICATION_CAP` so a
    /// chatty channel can't grow unbounded.
    pub items: Vec<crate::conn::NotificationMsg>,
    /// Cursor into `items` for the `N` panel.
    pub cursor: usize,
}

impl ListCursorNav for NotificationsUi {
    fn nav_len(&self) -> usize {
        self.items.len()
    }
    fn nav_cursor(&self) -> usize {
        self.cursor
    }
    fn nav_set(&mut self, i: usize) {
        self.cursor = i;
    }
}

/// Schema-lint panel state — the findings over the current schema cache
/// and the cursor into them.
#[derive(Debug, Default)]
pub struct SchemaLintUi {
    /// Findings produced by `query::lint::run_all` over the
    /// current schema cache. Rebuilt on entry to `Mode::SchemaLint`
    /// (cheap — pure pass over the cache).
    pub findings: Vec<crate::query::lint::Finding>,
    /// Cursor into `findings`.
    pub cursor: usize,
}

impl ListCursorNav for SchemaLintUi {
    fn nav_len(&self) -> usize {
        self.findings.len()
    }
    fn nav_cursor(&self) -> usize {
        self.cursor
    }
    fn nav_set(&mut self, i: usize) {
        self.cursor = i;
    }
}

/// Help-overlay state — vertical scroll, the mode to restore on close,
/// and the last-rendered max scroll used to clamp incremental scrolls.
#[derive(Debug, Default)]
pub struct HelpUi {
    /// Vertical scroll offset for the help overlay (number of leading lines
    /// hidden above the viewport).
    pub scroll: u16,
    /// Mode the operator came from when opening help. Used to
    /// restore that mode on close, so F1 from inside Editor /
    /// SchemaBrowser / etc. doesn't dump them back to Normal.
    /// `None` for the legacy `?`-from-Normal path.
    pub origin: Option<Mode>,
    /// Last-rendered max scroll for the help overlay. Written by `draw_help`
    /// each frame and read by the j/k handler so an incremental scroll past
    /// the bottom doesn't accumulate phantom offsets.
    pub max_scroll: u16,
}

/// Connection-picker state — the candidate data sources surfaced at
/// startup and the selected entry. Drives `Mode::ConnPick`.
#[derive(Debug, Default)]
pub struct ConnPickUi {
    /// Candidate data sources surfaced at startup. Populated when the operator
    /// didn't pass `--dsn` and we found multiple sources via discovery (e.g.
    /// IntelliJ). Drives `Mode::ConnPick`.
    pub picks: Vec<DataSourcePick>,
    /// Selected entry in `picks`.
    pub index: usize,
}

impl ListCursorNav for ConnPickUi {
    fn nav_len(&self) -> usize {
        self.picks.len()
    }
    fn nav_cursor(&self) -> usize {
        self.index
    }
    fn nav_set(&mut self, i: usize) {
        self.index = i;
    }
}

/// Result-diff state — the pinned baseline ("A"), the computed diff
/// currently shown in `Mode::ResultDiff`, and the cursor into it.
#[derive(Debug, Default)]
pub struct ResultDiffUi {
    /// Result pinned as the diff baseline ("A") by `D` in Normal
    /// mode. The next `D` diffs the current grid against this.
    /// Persists across diffs so the operator can iterate
    /// (tweak → run → D) against a fixed baseline.
    pub pinned: Option<PinnedResult>,
    /// The computed diff currently shown in `Mode::ResultDiff`.
    /// Snapshots both sides so the view is stable while open.
    pub active: Option<ResultDiffState>,
    /// Cursor into the rendered diff row list.
    pub cursor: usize,
}

/// Row-detail modal state — scroll / clamp and the focused field.
#[derive(Debug, Default)]
pub struct RowDetailUi {
    /// Scroll / clamp state for the row-detail modal — same shape as
    /// `HelpUi::scroll` / `HelpUi::max_scroll`. `scroll` is normally
    /// driven by the renderer's auto-scroll (so the focused field stays in
    /// view); the key handler only nudges it as a side-effect of moving
    /// `field`.
    pub scroll: u16,
    pub max_scroll: u16,
    /// Currently-focused field (column index) inside the row-detail modal.
    /// Bounded by `field_count` which the renderer writes each
    /// frame (it's just `grid.columns.len()` today, but kept as a separate
    /// field so the clamp matches what's actually rendered).
    pub field: usize,
    pub field_count: usize,
}

/// Per-cell zoom (`Mode::CellDetail`) state — scroll / clamp plus the
/// flattened JSON tree, its cursor, the collapsed-path set, and the
/// parsed value when the cell is a JSON object / array.
#[derive(Debug, Default)]
pub struct CellDetailUi {
    /// Scroll / clamp state for the per-cell zoom view (`Mode::CellDetail`).
    pub scroll: u16,
    pub max_scroll: u16,
    /// Parsed JSON value of the focused cell, when CellDetail is
    /// active AND the cell parses as a JSON object or array. `None`
    /// triggers the existing wrapped-text renderer (scalar /
    /// not-JSON cells).
    pub json_rows: Vec<crate::query::json_cell::JsonRow>,
    pub json_cursor: usize,
    pub json_collapsed: std::collections::HashSet<String>,
    pub json_value: Option<serde_json::Value>,
}
/// Insert a character at `*cursor`, advancing the cursor by the character's
/// UTF-8 length.
/// One step in the editor's undo / redo ring. Captures the buffer
/// + cursor state BEFORE a mutation, plus a `kind` tag so
///   consecutive char-inserts can be coalesced (otherwise typing
///   `qwerty` would be six undos).
#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub buffer: String,
    pub cursor: usize,
    /// What kind of mutation this entry guards against. Used to
    /// decide whether the next mutation should coalesce.
    pub kind: EditorActionKind,
    /// When the entry's coalescing window last extended (initially
    /// the snapshot time; bumped each time a subsequent char-insert
    /// merges in).
    pub merge_window_end: std::time::Instant,
}

/// Coarse classification of the action that produced an undo
/// snapshot. Only `CharInsert` merges with adjacent entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorActionKind {
    CharInsert,
    Other,
}
/// View the TapMonitor panel is currently in. Cycled by `v`.
/// `List` is the recency-ordered event stream (default,
/// shipped in L1). `Hotspots` aggregates by SQL fingerprint.
/// `Callers` aggregates by innermost caller frame.
/// `Transactions` aggregates by synthetic `txn` id (open vs
/// committed/rolled-back). `Pools` aggregates by connection-
/// pool name (saturation gauge). `NplusOne` is the live N+1
/// detector. `Baseline` is the diff vs the captured snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TapView {
    #[default]
    List,
    Hotspots,
    Callers,
    Transactions,
    Pools,
    NplusOne,
    Baseline,
}

impl TapView {
    /// Order: List → Hotspots → Callers → Transactions →
    /// Pools → NplusOne → Baseline → List. Entity views
    /// (Hotspots / Callers / Transactions / Pools) come before
    /// the analytical views (NplusOne / Baseline); within
    /// entities we cycle SQL-side → app-side → transaction-
    /// side → resource-side which mirrors the diagnostic
    /// narrowing path.
    pub fn next(self) -> Self {
        match self {
            TapView::List => TapView::Hotspots,
            TapView::Hotspots => TapView::Callers,
            TapView::Callers => TapView::Transactions,
            TapView::Transactions => TapView::Pools,
            TapView::Pools => TapView::NplusOne,
            TapView::NplusOne => TapView::Baseline,
            TapView::Baseline => TapView::List,
        }
    }
}

/// Captured hotspots snapshot used by the baseline-diff view.
/// `B` from any TapMonitor view freezes the current hotspots
/// list here; the Baseline view then renders the diff between
/// these and the live ring (regressions / new / disappeared).
/// Persists across view switches; cleared on `c` (with the
/// rest of the tap state) or on a second `B` press (recapture).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapBaseline {
    /// Host wall-clock when the snapshot was taken, in unix
    /// micros. Used in the panel title so the operator
    /// remembers when their baseline is from.
    pub captured_at_unix_micros: u64,
    /// Number of events in the ring at capture time.
    pub captured_event_count: usize,
    /// `tap::dropped_at_listener()` at capture time. The
    /// baseline panel renders the *delta* between this and
    /// the current counter — non-zero deltas mean events were
    /// shed between capture and diff, which is precisely the
    /// scenario that makes a "did my deploy regress?" view
    /// untrustworthy.
    pub captured_listener_dropped: u64,
    /// The snapshot — Hotspots already aggregated so the diff
    /// computation is cheap even when the ring has churned.
    pub hotspots: Vec<crate::tap::Hotspot>,
}

/// Liveness + backpressure-loss snapshot, fed by tap heartbeats
/// and updated each time any tap event lands. Lets the chrome
/// badge distinguish "JAR connected, no traffic" from "JAR
/// gone" — the badge goes amber when `last_event_at_unix_micros`
/// is older than the heartbeat interval × 2.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TapHealth {
    /// Host-clock receive time of the most recent event of any
    /// kind. 0 = no event ever seen this session.
    pub last_event_at_unix_micros: u64,
    /// Cumulative dropped-events count from the most recent
    /// heartbeat. The diff against an earlier heartbeat gives
    /// the drop rate; an increase means the JAR's in-process
    /// ring is filling under load.
    pub dropped_events_total: u64,
    /// Count of query events seen this session (across all
    /// `app` names). Drives the chrome "X queries" counter.
    pub query_count: u64,
    /// Count of heartbeat events seen — for diagnostics.
    pub heartbeat_count: u64,
}
