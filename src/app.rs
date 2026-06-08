//! Application state and the event loop.

mod keys;
pub mod msg;
pub use crate::app::msg::AppMsg;
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
use tui_common::TextInput;

/// The query M0 runs on connect — a read-only database overview. Every column
/// is text, so it renders without type-specific decoding.
const BOOTSTRAP_SQL: &str = "select datname as database, \
    pg_size_pretty(pg_database_size(datname)) as size \
    from pg_database where not datistemplate order by datname";

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
    /// Each char updates `grid_filter` and re-filters live so the
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
    /// State lives in `App::param_prompt`.
    ParamPrompt,
    /// Live substring search over the saved-queries panel (`/`
    /// from `SavedQueries`). Each char narrows the list in place;
    /// Enter accepts (keeps the filter), Esc clears it.
    SavedQueriesFilter,
    /// Rename prompt for the focused saved query (`r` from
    /// `SavedQueries`). Edits `App::rename_query_buffer`; Enter
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
pub fn next_sort_state(current: Option<(usize, bool)>, target_col: usize) -> Option<(usize, bool)> {
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

/// Pure: derive a default `saved-query` name from the buffer's
/// first non-blank chars. Keeps lowercase letters / digits /
/// underscores / dashes / spaces; everything else collapses to
/// space; runs of spaces fold to a single dash. Caps the length
/// so a 5 KB buffer doesn't pick a 5 KB default.
pub fn default_query_name(buf: &str) -> String {
    let head: String = buf
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(40)
        .collect();
    let mut out = String::with_capacity(head.len());
    let mut last_was_space = false;
    for c in head.chars() {
        let mapped = if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            Some(c.to_ascii_lowercase())
        } else if c.is_whitespace() {
            Some(' ')
        } else {
            None
        };
        match mapped {
            Some(' ') if last_was_space => {}
            Some(' ') => {
                out.push('-');
                last_was_space = true;
            }
            Some(c) => {
                out.push(c);
                last_was_space = false;
            }
            None => {}
        }
    }
    out.trim_matches('-').to_string()
}

/// Per-tab snapshot of the editor + result-grid state. The
/// connection, schema cache, history, saved queries, theme,
/// notifications, and safety profile are SHARED across tabs and
/// live directly on App.
///
/// Invariant: the active tab's state lives in App's existing
/// per-session fields (`editor_buffer`, `grid`, `grid_state`,
/// …). When the operator switches, the live fields are
/// snapshot-copied into `tabs[old_active]` and `tabs[new_active]`
/// is loaded back in. Existing read sites keep using App's
/// fields unchanged — multi-tab is invisible to them.
#[derive(Debug, Clone, Default)]
pub struct TabSnapshot {
    pub editor_buffer: String,
    pub editor_cursor: usize,
    pub editor_scroll: u16,
    pub editor_preferred_col: Option<usize>,
    pub editor_undo: Vec<UndoEntry>,
    pub editor_redo: Vec<UndoEntry>,
    pub grid: crate::grid::Grid,
    pub grid_selected: Option<usize>,
    pub grid_col_cursor: usize,
    pub grid_sort: Option<(usize, bool)>,
    pub grid_raw_rows: Option<Vec<Vec<String>>>,
    pub grid_filter: Option<String>,
    pub grid_visible_rows: Vec<usize>,
    pub last_run_sql: Option<String>,
    pub grid_source: Option<(String, String)>,
    /// Diff baseline ("A") pinned with `D`. Per-tab so pinning in one
    /// tab can't leak into another (a fresh tab starts unpinned). The
    /// transient `Mode::ResultDiff` overlay is NOT snapshotted — it is
    /// dismissed on any tab change (see `dismiss_result_diff`).
    pub pinned_result: Option<PinnedResult>,
}

/// Hard cap on tab count. 9 matches `Ctrl-1..Ctrl-9` numeric
/// jumps; the operator can still close one to open another.
pub const TAB_CAP: usize = 9;

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

/// Number of individually-listed rows in a diff view: removed +
/// changed + added (unchanged rows are summarised, not listed).
/// Shared by the key handler (cursor clamp) and the renderer.
pub fn diff_row_count(diff: &crate::query::row_diff::RowDiff) -> usize {
    diff.removed.len() + diff.changed.len() + diff.added.len()
}

/// Pure: indices into `entries` matching `filter` — a
/// case-insensitive substring tested against each entry's name
/// **or** body. An absent / blank filter returns every index in
/// original order. Backs the saved-queries panel search so the
/// cursor and renderer agree on what's visible.
pub fn filter_saved_indices(
    entries: &[crate::saved::SavedQuery],
    filter: Option<&str>,
) -> Vec<usize> {
    let needle = filter.unwrap_or("").trim().to_ascii_lowercase();
    if needle.is_empty() {
        return (0..entries.len()).collect();
    }
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.name.to_ascii_lowercase().contains(&needle)
                || e.body.to_ascii_lowercase().contains(&needle)
        })
        .map(|(i, _)| i)
        .collect()
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

/// Pure: scan every cell of every visible row and return the
/// `(visible_row_index, col_index)` of each cell that
/// (case-insensitively) contains `needle`. Row-major order; ties
/// within a row are returned in column order. `needle` empty
/// returns an empty vec — caller should treat as "find inactive".
pub fn compute_grid_find_matches(
    grid: &crate::grid::Grid,
    visible_rows: &[usize],
    needle: &str,
) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let needle = needle.to_ascii_lowercase();
    let mut out = Vec::new();
    for (vi, &row_idx) in visible_rows.iter().enumerate() {
        let Some(row) = grid.rows.get(row_idx) else {
            continue;
        };
        for (ci, cell) in row.iter().enumerate() {
            if cell.to_ascii_lowercase().contains(&needle) {
                out.push((vi, ci));
            }
        }
    }
    out
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

/// Key used in the `expanded` set for a table row.
pub fn schema_browser_table_key(schema: &str, table: &str) -> String {
    format!("{schema}.{table}")
}

/// Search direction for [`next_schema_row_idx`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

/// Pure: walking from `from` in `dir`, return the byte position of
/// the next `SchemaBrowserRow::Schema` row in `rows`. Useful for
/// `[` / `]` peer-level navigation in the schema browser — jumping
/// over a fully-expanded table's columns in one keypress.
/// `None` when no further schema row exists in that direction.
pub fn next_schema_row_idx(
    rows: &[SchemaBrowserRow],
    from: usize,
    dir: Direction,
) -> Option<usize> {
    match dir {
        Direction::Forward => rows
            .iter()
            .enumerate()
            .skip(from + 1)
            .find(|(_, r)| matches!(r, SchemaBrowserRow::Schema { .. }))
            .map(|(i, _)| i),
        Direction::Backward => rows
            .iter()
            .enumerate()
            .take(from)
            .rev()
            .find(|(_, r)| matches!(r, SchemaBrowserRow::Schema { .. }))
            .map(|(i, _)| i),
    }
}

/// Filter a pre-flattened schema-browser row list to those whose
/// name OR any descendant's name (case-insensitive) contains
/// `pat`. Ancestor rows are kept so the matched-row's parent
/// schema / table still shows for context. Empty `pat` is a
/// no-op (every row kept) — the caller should not even call this
/// in that case but defensive is cheap.
pub fn filter_schema_browser_rows(rows: Vec<SchemaBrowserRow>, pat: &str) -> Vec<SchemaBrowserRow> {
    if pat.is_empty() {
        return rows;
    }
    let pat = pat.to_ascii_lowercase();
    let matches_self = |row: &SchemaBrowserRow| -> bool {
        let name = match row {
            SchemaBrowserRow::Schema { name, .. } => name.as_str(),
            SchemaBrowserRow::Table { name, .. } => name.as_str(),
            SchemaBrowserRow::Column { name, .. } => name.as_str(),
            SchemaBrowserRow::Constraint { name, .. } => name.as_str(),
        };
        name.to_ascii_lowercase().contains(&pat)
    };
    let depth = |row: &SchemaBrowserRow| -> usize {
        match row {
            SchemaBrowserRow::Schema { .. } => 0,
            SchemaBrowserRow::Table { .. } => 1,
            SchemaBrowserRow::Column { .. } | SchemaBrowserRow::Constraint { .. } => 2,
        }
    };
    let n = rows.len();
    let mut keep = vec![false; n];
    for i in 0..n {
        if matches_self(&rows[i]) {
            keep[i] = true;
            continue;
        }
        let d = depth(&rows[i]);
        // Look ahead: a descendant row sits at depth > d before we
        // hit another row at depth <= d.
        let mut j = i + 1;
        while j < n {
            if depth(&rows[j]) <= d {
                break;
            }
            if matches_self(&rows[j]) {
                keep[i] = true;
                break;
            }
            j += 1;
        }
    }
    rows.into_iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(r, _)| r)
        .collect()
}

/// Pure: walk the cache and emit visible rows in display order
/// (schemas alphabetical; under each expanded schema, its tables
/// alphabetical; under each expanded table, its columns in catalog
/// order followed by its unique/PK constraints alphabetical).
/// `expanded` is keyed by schema name and `"schema.table"` for
/// tables — see `schema_browser_table_key`. Empty `expanded` →
/// every node collapsed.
pub fn flatten_schema_browser(
    cache: &crate::query::schema::SchemaCache,
    expanded: &std::collections::HashSet<String>,
) -> Vec<SchemaBrowserRow> {
    let mut by_schema: std::collections::HashMap<&str, Vec<&str>> =
        std::collections::HashMap::new();
    for t in &cache.tables {
        by_schema
            .entry(t.schema.as_str())
            .or_default()
            .push(t.name.as_str());
    }
    for tables in by_schema.values_mut() {
        tables.sort_unstable();
    }
    // Constraints grouped by (schema, table), names sorted.
    let mut cons_by_table: std::collections::HashMap<(&str, &str), Vec<&str>> =
        std::collections::HashMap::new();
    for c in &cache.constraints {
        cons_by_table
            .entry((c.schema.as_str(), c.table.as_str()))
            .or_default()
            .push(c.name.as_str());
    }
    for v in cons_by_table.values_mut() {
        v.sort_unstable();
    }
    let mut out = Vec::new();
    let mut schemas: Vec<&str> = cache.schemas.iter().map(String::as_str).collect();
    // Some schemas (e.g. `pg_catalog`) only appear in the cache's
    // tables list, not in `schemas`. Fold those in so the operator
    // sees every namespace they can query.
    for &s in by_schema.keys() {
        if !schemas.contains(&s) {
            schemas.push(s);
        }
    }
    schemas.sort_unstable();
    for s in schemas {
        let tables = by_schema.get(s).cloned().unwrap_or_default();
        let schema_expanded = expanded.contains(s);
        out.push(SchemaBrowserRow::Schema {
            name: s.to_string(),
            expanded: schema_expanded,
            table_count: tables.len(),
        });
        if !schema_expanded {
            continue;
        }
        for t in tables {
            let cols = cache
                .columns_by_table
                .get(&(s.to_string(), t.to_string()))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let constraints = cons_by_table.get(&(s, t)).cloned().unwrap_or_default();
            let table_key = schema_browser_table_key(s, t);
            let table_expanded = expanded.contains(&table_key);
            out.push(SchemaBrowserRow::Table {
                schema: s.to_string(),
                name: t.to_string(),
                expanded: table_expanded,
                column_count: cols.len(),
                constraint_count: constraints.len(),
            });
            if !table_expanded {
                continue;
            }
            for c in cols {
                out.push(SchemaBrowserRow::Column {
                    schema: s.to_string(),
                    table: t.to_string(),
                    name: c.clone(),
                });
            }
            for cn in &constraints {
                out.push(SchemaBrowserRow::Constraint {
                    schema: s.to_string(),
                    table: t.to_string(),
                    name: cn.to_string(),
                });
            }
        }
    }
    out
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

/// Inspect `sql` for its FROM clause and return the single source
/// table when there's exactly one. Joins / subqueries / no-FROM
/// statements all return `None` — the row-as-INSERT yank only
/// makes sense for a single-table read.
pub fn infer_single_source_table(sql: &str) -> Option<(String, String)> {
    let refs = crate::query::from_parse::parse_from_tables(sql);
    if refs.len() != 1 {
        return None;
    }
    let t = &refs[0];
    // CTE / subquery aliases have an empty schema field too —
    // but only catalog-rooted refs are useful for INSERT INTO.
    // A bare table name with no resolved schema falls through;
    // we use "public" as the default (matching what Postgres does
    // when there's no search_path override).
    let schema = t.schema.clone().unwrap_or_else(|| "public".to_string());
    Some((schema, t.name.clone()))
}

/// Render a rendered-cell string as a SQL literal. Empty → `NULL`.
/// Numeric strings pass through unquoted. Anything else is
/// single-quoted with embedded quotes doubled. Best-effort — types
/// aren't tracked through the renderer, so JSON / arrays / bytea
/// would all hit the string branch and get quoted; the operator
/// edits the literal in the editor afterwards if needed.
pub fn format_sql_literal(s: &str) -> String {
    if s.is_empty() {
        return "NULL".to_string();
    }
    // Numeric? Try parsing both integer and float forms.
    if s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok() {
        return s.to_string();
    }
    // Booleans pass through as identifiers (Postgres accepts them).
    if matches!(s.to_ascii_lowercase().as_str(), "true" | "false") {
        return s.to_ascii_lowercase();
    }
    // Default: SQL string literal with `'` doubled.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Format a Postgres row-estimate (`Plan Rows` from EXPLAIN JSON)
/// for the status footer. Uses comma separators for readability
/// and clamps to integer.
pub fn format_row_estimate(rows: f64) -> String {
    let n = rows.round() as i64;
    let neg = n < 0;
    let s = n.unsigned_abs().to_string();
    let bytes: Vec<u8> = s.bytes().collect();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

/// Run `EXPLAIN (FORMAT JSON) …` and pluck the top node's `Plan
/// Rows` estimate. Async; returns the estimate or a stringified
/// error suitable for the status footer / log.
async fn run_cost_explain(
    client: &tokio_postgres::Client,
    explain_sql: &str,
) -> Result<f64, String> {
    let row = client
        .query_one(explain_sql, &[])
        .await
        .map_err(|e| e.to_string())?;
    let json_str: String = row.try_get::<_, String>(0).map_err(|e| e.to_string())?;
    let parsed: serde_json::Value = serde_json::from_str(&json_str).map_err(|e| e.to_string())?;
    // EXPLAIN JSON output is an array with one entry per plan; we
    // care about the first plan's top node.
    let top = parsed
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.get("Plan"))
        .ok_or_else(|| "no Plan in EXPLAIN output".to_string())?;
    top.get("Plan Rows")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| "no Plan Rows on top node".to_string())
}

/// Pure predicate: should `sql` be subjected to a pre-flight cost
/// preview? True for plain SELECT / WITH queries (where row-count
/// estimates are useful) when the SQL doesn't already cap itself
/// with a `LIMIT`. False for EXPLAIN-wrapped queries, writes, and
/// multi-statement scripts (callers should also skip the check for
/// batch runs).
pub fn is_cost_checkable(sql: &str) -> bool {
    // Strip comments AND string-literal bodies so a word match
    // inside a string (`SELECT 'over the limit' …` /
    // `WITH x AS (SELECT 'DELETE me') …`) doesn't trigger a
    // false-positive on `limit` or a false-positive write reject.
    let stripped = strip_strings(&crate::safety::strip_sql_comments(sql));
    let trimmed = stripped.trim_start();
    let first: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    if !matches!(first.as_str(), "select" | "with" | "table" | "values") {
        return false;
    }
    // CTE-wrapped writes (`WITH x AS (DELETE … RETURNING …) SELECT …`)
    // are technically allowed by Postgres but emitting a "cost
    // preview" Confirm with row-estimate text misleads about what's
    // really happening. Reject if any write verb appears as a word
    // anywhere in the stripped SQL.
    for verb in ["delete", "update", "insert"] {
        if crate::safety::word_present(&stripped, verb) {
            return false;
        }
    }
    // A buffer that already wraps itself in `LIMIT <n>` is
    // self-bounded. Word-boundary check via `safety::word_present`.
    if crate::safety::word_present(&stripped, "limit") {
        return false;
    }
    true
}

/// Replace every SQL string literal and quoted identifier body
/// with the same number of `_` chars so they can't trip word
/// matching (e.g. `'over the limit'`). Preserves length so byte
/// offsets stay aligned with the original. Tolerates Postgres-
/// doubled `''` / `""` escapes inside.
fn strip_strings(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out: Vec<u8> = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' || b == b'"' {
            let quote = b;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == quote {
                    // Doubled (`''` / `""`) is an embedded quote —
                    // erase both and keep going.
                    if bytes.get(i + 1) == Some(&quote) {
                        out[i] = b'_';
                        out[i + 1] = b'_';
                        i += 2;
                        continue;
                    }
                    // End of literal.
                    i += 1;
                    break;
                }
                out[i] = b'_';
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    // Safe: we only replace ASCII bytes with ASCII `_`; multi-byte
    // chars inside a literal get individually replaced byte-wise,
    // which still leaves valid UTF-8 (each replaced byte was a
    // continuation byte of a sequence whose start was also replaced).
    String::from_utf8(out).unwrap_or_else(|_| sql.to_string())
}

/// Quote a Postgres identifier only when it needs it: lowercase
/// ASCII identifiers passing the unquoted-identifier rule render
/// bare; anything else (mixed case, leading digit, embedded space,
/// non-ASCII letter, reserved keyword) gets `"…"`-wrapped with
/// inner `"` doubled. Conservative: matches Postgres's own
/// `quote_ident` semantics closely enough for templates.
pub fn quote_ident(name: &str) -> String {
    fn needs_quote(name: &str) -> bool {
        let mut chars = name.chars();
        let first = match chars.next() {
            Some(c) => c,
            None => return true, // empty — pathological; quote to fail loudly
        };
        if !(first.is_ascii_lowercase() || first == '_') {
            return true;
        }
        chars.any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
    }
    if !needs_quote(name) {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for c in name.chars() {
        if c == '"' {
            out.push_str("\"\"");
        } else {
            out.push(c);
        }
    }
    out.push('"');
    out
}

/// Build a `SELECT * FROM <schema>.<table> LIMIT 100;` template
/// the operator can paste into the editor as a starting point. The
/// `LIMIT 100` is a deliberate sample-size guard — the user can
/// strip it if they really want every row.
pub fn build_select_all_template(schema: &str, table: &str) -> String {
    format!(
        "SELECT * FROM {}.{} LIMIT 100;",
        quote_ident(schema),
        quote_ident(table),
    )
}

/// Build an `INSERT INTO <schema>.<table> (cols…) VALUES (NULL,
/// NULL, …);` template. One placeholder per column; the operator
/// fills in real values in the editor. Returns an empty string if
/// `columns` is empty (defensive — the schema cache should always
/// have at least one column for a real table).
pub fn build_insert_template(schema: &str, table: &str, columns: &[String]) -> String {
    if columns.is_empty() {
        return String::new();
    }
    let cols: Vec<String> = columns.iter().map(|c| quote_ident(c)).collect();
    let placeholders = vec!["NULL"; columns.len()].join(", ");
    format!(
        "INSERT INTO {}.{}\n  ({})\nVALUES\n  ({});",
        quote_ident(schema),
        quote_ident(table),
        cols.join(", "),
        placeholders,
    )
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
    (0..start)
        .rev()
        .find(|&i| history[i].to_ascii_lowercase().contains(&n))
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
    /// Synthetic-data demo mode (`pgman --demo`). When set, `run`
    /// skips the live connection and the loop skips persisting the
    /// editor draft / history to disk — so a demo / screenshot run
    /// can't clobber the operator's real session state. Populated
    /// by `crate::demo::app`.
    pub demo: bool,
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
    /// Undo ring of pre-mutation `(buffer, cursor)` snapshots. Ctrl-Z
    /// pops; Ctrl-Y / Ctrl-Shift-Z redoes. Capped at `UNDO_CAP`.
    pub editor_undo: Vec<UndoEntry>,
    /// Redo ring — filled by `editor_undo` and drained by `editor_redo`.
    /// Any new editor mutation invalidates redo (standard editor
    /// behaviour: divergent edit = new history branch).
    pub editor_redo: Vec<UndoEntry>,
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
    /// Which view LogPick is currently rendering — toggle with `c`.
    pub log_pick_view: LogPickView,
    /// Cached cluster list for the Clusters view. Rebuilt on
    /// `log_picks` set and on view toggle so repeated j/k keystrokes
    /// don't re-cluster on each frame.
    pub log_pick_clusters: Vec<crate::query::nplus1::Cluster>,
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
    /// Mode the operator came from when opening help. Used to
    /// restore that mode on close, so F1 from inside Editor /
    /// SchemaBrowser / etc. doesn't dump them back to Normal.
    /// `None` for the legacy `?`-from-Normal path.
    pub help_origin: Option<Mode>,
    /// Modes the operator has already entered this session. Used by
    /// `note_mode_entry` to flash a one-time "key hint" status the
    /// first time each mode opens — discoverability nudge without
    /// becoming nagware on repeat visits.
    pub mode_seen: std::collections::HashSet<Mode>,
    /// psql `\timing` toggle — when on, the QueryOk handler
    /// appends an elapsed-ms marker to the status footer.
    pub timing_on: bool,
    /// Rich detail from the most-recent failed query. Surfaced
    /// by `Mode::ErrorDetail` (Ctrl-E after a failure). Cleared
    /// on the next successful run.
    pub last_error_detail: Option<crate::conn::QueryErrDetail>,
    /// Pid the operator is being prompted to terminate (set by
    /// `K` in Sessions panel; consumed on Confirm). `None`
    /// outside `Mode::ConfirmTerminate`.
    pub pending_terminate: Option<i32>,
    /// Auto-refresh toggle for the SlowQueries / Sessions panels.
    /// When `true`, the main loop re-fires the panel's load every
    /// `AUTO_REFRESH_INTERVAL` while the operator is in that mode.
    pub auto_refresh: bool,
    /// Last auto-refresh fire — used to gate the next tick. `None`
    /// means "fire as soon as eligible".
    pub auto_refresh_last: Option<Instant>,
    /// Vim-style grid bookmarks. Set with `m<a-z>` (focused
    /// row + col snapshot), jumped to with `'<a-z>`. Session-
    /// local; not persisted with the draft. 26-slot fixed map
    /// would do, but `HashMap` keeps the slot key open for any
    /// printable char operators might want later.
    pub bookmarks: std::collections::HashMap<char, GridBookmark>,
    /// Ring buffer of recent `NOTIFY` arrivals from the server.
    /// Newest at the end. Capped at `NOTIFICATION_CAP` so a
    /// chatty channel can't grow unbounded.
    pub notifications: Vec<crate::conn::NotificationMsg>,
    /// Cursor into `notifications` for the `N` panel.
    pub notifications_cursor: usize,
    /// Ring buffer of recent JDBC-tap events (queries +
    /// txn boundaries from the pgman-tap JAR). Newest at the
    /// end. Heartbeat events don't land here — they update
    /// `tap_health` instead. Capped at `TAP_CAP`.
    pub tap_events: std::collections::VecDeque<crate::tap::TapEvent>,
    /// Cursor into `tap_events` for the `Mode::TapMonitor` panel.
    pub tap_events_cursor: usize,
    /// Liveness + backpressure-loss tracker fed by tap
    /// heartbeats. Lets the chrome badge distinguish "JAR
    /// connected, no traffic" from "JAR gone."
    pub tap_health: TapHealth,
    /// Which TapMonitor view the operator is on — list of recent
    /// events (default) or grouped hotspots. Toggled with `G`.
    pub tap_view: TapView,
    /// Sort mode for the hotspots view. Cycles via `s`.
    pub tap_sort: crate::tap::HotspotSort,
    /// Cursor into the rendered hotspots list (not the raw ring).
    /// Re-clamped each frame against the current grouping.
    pub tap_hotspots_cursor: usize,
    /// Cursor into the rendered N+1 findings list.
    pub tap_nplus1_cursor: usize,
    /// Cursor into the rendered per-caller rollup list.
    pub tap_callers_cursor: usize,
    /// Cursor into the rendered transaction-stats list.
    pub tap_txns_cursor: usize,
    /// Cursor into the rendered per-pool stats list.
    pub tap_pools_cursor: usize,
    /// Captured hotspots snapshot for the baseline-diff view.
    /// `None` until the operator presses `B`.
    pub tap_baseline: Option<TapBaseline>,
    /// Cursor into the rendered baseline-diff list.
    pub tap_baseline_cursor: usize,
    /// Persisted saved queries — loaded at startup, written back
    /// on quit (and on save / delete during the session).
    pub saved_queries: crate::saved::SavedQueries,
    /// Cursor into `saved_queries.entries` for the panel.
    pub saved_queries_cursor: usize,
    /// Name being typed in `Mode::SaveQueryPrompt`.
    pub save_query_name: String,
    /// Active `:param` collection while loading a parameterised
    /// saved query (`Mode::ParamPrompt`). `None` otherwise.
    pub param_prompt: Option<ParamPrompt>,
    /// Live substring filter for the saved-queries panel
    /// (`Mode::SavedQueriesFilter`). `None` = show everything.
    /// Matches case-insensitively on name OR body.
    pub saved_queries_filter: Option<TextInput>,
    /// Input buffer for `Mode::RenameQueryPrompt` (the new name
    /// being typed), and the original name being renamed.
    pub rename_query_buffer: TextInput,
    pub rename_query_from: String,
    /// Saved state for the non-active tabs. The active tab's
    /// state always lives in the per-session fields above; on
    /// switch we snapshot out / load in.
    pub tabs: Vec<TabSnapshot>,
    /// Index into `tabs` for the currently-active tab. Invariant:
    /// `tabs` always has at least one entry and `active_tab <
    /// tabs.len()`.
    pub active_tab: usize,
    /// `true` after the operator pressed `m` and the next key
    /// is interpreted as the bookmark letter. Cleared after one
    /// dispatch (success or not).
    pub pending_mark_set: bool,
    /// Mirror of `pending_mark_set` for `'<x>` jumps.
    pub pending_mark_jump: bool,
    /// When the current query started — captured in `spawn_run`
    /// and read in `QueryOk` / `QueryFailed` to surface elapsed
    /// time when `\timing` is on.
    pub query_started: Option<Instant>,
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
    /// Pattern being typed / accepted in `Mode::GridFind`. `Some`
    /// means find is active; the matches list below is rebuilt
    /// from this on every change.
    pub grid_find: Option<String>,
    /// Match cursor positions for `grid_find`, in row-major
    /// order: each pair is `(visible_row_index, col_index)`.
    pub grid_find_matches: Vec<(usize, usize)>,
    /// Current position in `grid_find_matches` — `n` advances, `N`
    /// retreats; both wrap.
    pub grid_find_pos: usize,
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
    /// Schema browser cursor — index into the flattened
    /// (post-expand-state) row list.
    pub schema_browser_cursor: usize,
    /// Active in-tree filter; `Some` when typing or accepted (Enter
    /// keeps it applied). `None` = no filter. Empty string while in
    /// SchemaBrowserFilter mode is fine — it just means "show
    /// everything until the operator types something."
    pub schema_browser_filter: Option<String>,
    /// Findings produced by `query::lint::run_all` over the
    /// current schema cache. Rebuilt on entry to `Mode::SchemaLint`
    /// (cheap — pure pass over the cache).
    pub schema_lint_findings: Vec<crate::query::lint::Finding>,
    /// Cursor into `schema_lint_findings`.
    pub schema_lint_cursor: usize,
    /// Names of schemas the operator has expanded. Schemas start
    /// collapsed; the operator picks which to drill into.
    pub schema_browser_expanded: std::collections::HashSet<String>,
    /// Most recent `pg_stat_statements` snapshot, when
    /// `Mode::SlowQueries` is active.
    pub slow_queries: Vec<crate::query::slow_queries::SlowQueryRow>,
    pub slow_queries_cursor: usize,
    /// Most recent `pg_stat_activity` snapshot, when
    /// `Mode::Sessions` is active.
    pub sessions: Vec<crate::query::sessions::SessionRow>,
    pub sessions_cursor: usize,
    /// SQL of the most recent successful `Run` query, kept so the
    /// grid post-load step can re-parse it to infer the single
    /// source table (when there is one). Not set for batch /
    /// EXPLAIN / EXPLAIN ANALYZE runs.
    pub last_run_sql: Option<String>,
    /// Parsed JSON value of the focused cell, when CellDetail is
    /// active AND the cell parses as a JSON object or array. `None`
    /// triggers the existing wrapped-text renderer (scalar /
    /// not-JSON cells).
    pub json_cell_rows: Vec<crate::query::json_cell::JsonRow>,
    pub json_cell_cursor: usize,
    pub json_cell_collapsed: std::collections::HashSet<String>,
    pub json_cell_value: Option<serde_json::Value>,
    /// `Some((schema, table))` when the current grid is the result
    /// of a single-FROM-table SELECT, `None` otherwise. Drives the
    /// row-as-INSERT yank — and, eventually, cell-edit-to-UPDATE +
    /// FK navigation.
    pub grid_source: Option<(String, String)>,
    /// Result pinned as the diff baseline ("A") by `D` in Normal
    /// mode. The next `D` diffs the current grid against this.
    /// Persists across diffs so the operator can iterate
    /// (tweak → run → D) against a fixed baseline.
    pub pinned_result: Option<PinnedResult>,
    /// The computed diff currently shown in `Mode::ResultDiff`.
    /// Snapshots both sides so the view is stable while open.
    pub result_diff: Option<ResultDiffState>,
    /// Cursor into the rendered diff row list.
    pub result_diff_cursor: usize,

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
    pub read_only: bool,
    statement_timeout_ms: u64,
    msg_tx: UnboundedSender<AppMsg>,
    msg_rx: Option<UnboundedReceiver<AppMsg>>,
}

impl App {
    /// Clone of the message sender, for spawning listeners
    /// that need to push events back into the run loop from
    /// outside the App (the JDBC-tap listener, primarily).
    pub fn msg_tx_clone(&self) -> UnboundedSender<AppMsg> {
        self.msg_tx.clone()
    }

    pub fn new(
        theme: Theme,
        dsn: Option<Dsn>,
        data_source_picks: Vec<DataSourcePick>,
        safety_config: SafetyConfig,
    ) -> Self {
        let db = dsn.as_ref().map(|d| d.dbname.as_str()).unwrap_or("default");
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
        let mode = if show_picker {
            Mode::ConnPick
        } else {
            Mode::Normal
        };
        Self {
            theme,
            mode,
            demo: false,
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
            editor_undo: Vec::new(),
            editor_redo: Vec::new(),
            history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            pending_run: None,
            tx_open: false,
            log_picks: Vec::new(),
            log_pick_view: LogPickView::AllQueries,
            log_pick_clusters: Vec::new(),
            log_pick_index: 0,
            last_status: None,
            last_error: None,
            query_running: false,
            help_scroll: 0,
            help_max_scroll: 0,
            help_origin: None,
            mode_seen: std::collections::HashSet::new(),
            timing_on: false,
            last_error_detail: None,
            pending_terminate: None,
            auto_refresh: false,
            auto_refresh_last: None,
            bookmarks: std::collections::HashMap::new(),
            notifications: Vec::new(),
            notifications_cursor: 0,
            tap_events: std::collections::VecDeque::new(),
            tap_events_cursor: 0,
            tap_health: TapHealth::default(),
            tap_view: TapView::default(),
            tap_sort: crate::tap::HotspotSort::default(),
            tap_hotspots_cursor: 0,
            tap_nplus1_cursor: 0,
            tap_callers_cursor: 0,
            tap_txns_cursor: 0,
            tap_pools_cursor: 0,
            tap_baseline: None,
            tap_baseline_cursor: 0,
            saved_queries: crate::saved::SavedQueries::default(),
            saved_queries_cursor: 0,
            save_query_name: String::new(),
            param_prompt: None,
            saved_queries_filter: None,
            rename_query_buffer: TextInput::new(),
            rename_query_from: String::new(),
            // Start with a single tab whose state IS the per-
            // session fields. The Vec entry is a placeholder that
            // gets refreshed on every tab switch.
            tabs: vec![TabSnapshot::default()],
            active_tab: 0,
            pending_mark_set: false,
            pending_mark_jump: false,
            query_started: None,
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
            grid_find: None,
            grid_find_matches: Vec::new(),
            grid_find_pos: 0,
            grid_visible_rows: Vec::new(),
            explain_plan: None,
            explain_cursor: 0,
            explain_collapsed: std::collections::HashSet::new(),
            schema_browser_cursor: 0,
            schema_browser_expanded: std::collections::HashSet::new(),
            schema_browser_filter: None,
            schema_lint_findings: Vec::new(),
            schema_lint_cursor: 0,
            slow_queries: Vec::new(),
            slow_queries_cursor: 0,
            sessions: Vec::new(),
            sessions_cursor: 0,
            last_run_sql: None,
            json_cell_rows: Vec::new(),
            json_cell_cursor: 0,
            json_cell_collapsed: std::collections::HashSet::new(),
            json_cell_value: None,
            grid_source: None,
            pinned_result: None,
            result_diff: None,
            result_diff_cursor: 0,
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

        // Demo mode renders a synthetic, pre-populated app — never
        // open a real connection.
        if self.dsn.is_some() && !self.demo {
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
                    self.tick_auto_refresh();
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
            if !self.demo
                && self.draft_dirty
                && draft_save_due(
                    self.draft_last_save,
                    Instant::now(),
                    Duration::from_millis(500),
                )
            {
                let _ = persist_draft(&self.editor_buffer);
                self.draft_last_save = Some(Instant::now());
                self.draft_dirty = false;
            }
        }
        // Persist session state on exit — but never in demo mode,
        // where the buffer / history / saved-queries are synthetic
        // fixtures that must not overwrite the operator's real files.
        if !self.demo {
            // Persist the editor draft so the next launch can restore
            // whatever the operator had in flight. Best-effort: failure
            // logs and moves on — the loop is already finishing.
            if let Err(e) = persist_draft(&self.editor_buffer) {
                tracing::warn!("could not save editor draft: {e}");
            }
            // Persist the query history ring too. Same best-effort
            // stance — history is a convenience, not source of truth.
            if let Err(e) = persist_history(&self.history) {
                tracing::warn!("could not save query history: {e}");
            }
            if let Err(e) = crate::saved::save_to(&saved_queries_path(), &self.saved_queries) {
                tracing::warn!("could not save 'saved queries': {e}");
            }
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
                self.last_status = Some(format!(
                    "loaded {} char(s) from $EDITOR",
                    self.editor_buffer.len()
                ));
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
            || (self.auto_refresh && matches!(self.mode, Mode::SlowQueries | Mode::Sessions))
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
        let (notice_tx, mut notice_rx) = tokio::sync::mpsc::unbounded_channel::<conn::NoticeMsg>();
        let (notification_tx, mut notification_rx) =
            tokio::sync::mpsc::unbounded_channel::<conn::NotificationMsg>();
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
        // Same forwarding shape for LISTEN/NOTIFY arrivals. Each
        // connect gets its own channel pair so stale notifications
        // from a prior session can't leak into the new ring.
        let notify_forward_tx = tx.clone();
        let notify_generation = generation;
        tokio::spawn(async move {
            while let Some(n) = notification_rx.recv().await {
                let msg = AppMsg::Notification {
                    generation: notify_generation,
                    notification: n,
                };
                if notify_forward_tx.send(msg).is_err() {
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
                notification_tx,
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

    /// Apply a finished message from a spawned task. Tap events
    /// bypass the generation filter — the tap listener is bound
    /// at startup and lives independently of the DB connection,
    /// so events arriving across a reconnect are still meaningful.
    fn on_msg(&mut self, msg: AppMsg) {
        if !matches!(msg, AppMsg::TapEvent { .. }) && msg.generation() != self.generation {
            tracing::debug!(
                "dropping stale message from generation {}",
                msg.generation()
            );
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
                // Infer the source table for the new grid — used by
                // row-as-INSERT yank (and, eventually, cell-edit-to-
                // UPDATE / FK nav). Single-table SELECT only; anything
                // else clears the source so the feature gates self-
                // disable.
                self.grid_source = self
                    .last_run_sql
                    .as_deref()
                    .and_then(infer_single_source_table);
                self.query_running = false;
                self.last_error = None;
                self.last_error_detail = None;
                let elapsed = self.query_started.take().map(|t0| t0.elapsed());
                let timing_suffix = if self.timing_on {
                    elapsed
                        .map(|d| format!(" · {:.0} ms", d.as_secs_f64() * 1000.0))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let cap_suffix = if self.grid.truncated {
                    format!(" · capped at {}", crate::grid::MAX_ROWS)
                } else {
                    String::new()
                };
                self.last_status = Some(format!(
                    "{kind_label} ok · {} row(s){cap_suffix}{timing_suffix}",
                    self.grid.row_count()
                ));
                // EXPLAIN / EXPLAIN ANALYZE: parse the JSON we asked
                // for and pop the tree visualiser. On parse failure
                // we fall back to the raw grid (the JSON text is
                // still readable that way), surface the parse error
                // in last_status so the operator sees what happened.
                if kind_label == "EXPLAIN" || kind_label == "EXPLAIN ANALYZE" {
                    if let Some(text) = self.grid.rows.first().and_then(|r| r.first()).cloned() {
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
                error,
                position,
                detail,
                ..
            } => {
                self.query_running = false;
                self.query_started = None;
                self.last_status = None;
                self.last_error = Some(error);
                self.last_error_detail = detail;
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
                    let trimmed_prefix_bytes =
                        self.editor_buffer.len() - self.editor_buffer.trim_start().len();
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
            AppMsg::SlowQueriesLoaded { result, .. } => match result {
                Ok(rows) => {
                    self.slow_queries = rows;
                    self.slow_queries_cursor = 0;
                    self.last_status =
                        Some(format!("slow queries · {} row(s)", self.slow_queries.len()));
                }
                Err(e) => {
                    // pg_stat_statements not installed is the most
                    // common failure — point the operator at the
                    // `CREATE EXTENSION` they need.
                    let hint = if e.contains("pg_stat_statements") {
                        " (try `CREATE EXTENSION pg_stat_statements`)"
                    } else {
                        ""
                    };
                    self.last_error = Some(format!("slow queries load failed: {e}{hint}"));
                    self.mode = Mode::Normal;
                }
            },
            AppMsg::SessionsLoaded { result, .. } => match result {
                Ok(rows) => {
                    let blocked = rows.iter().filter(|r| r.is_blocked()).count();
                    self.sessions = rows;
                    self.sessions_cursor = 0;
                    self.last_status = Some(format!(
                        "sessions · {} total · {} blocked",
                        self.sessions.len(),
                        blocked
                    ));
                }
                Err(e) => {
                    self.last_error = Some(format!("sessions load failed: {e}"));
                    self.mode = Mode::Normal;
                }
            },
            AppMsg::LiveLintLoaded { result, .. } => {
                // Merge live findings into the existing pure list.
                // If the operator already left the lint panel,
                // silently drop — a fresh open re-fires the fetch.
                if self.mode != Mode::SchemaLint {
                    return;
                }
                match result {
                    Ok(live) => {
                        let added = live.len();
                        self.schema_lint_findings.extend(live);
                        // Re-sort to keep severity ordering after
                        // merge. Same sort as `lint::run_all`.
                        self.schema_lint_findings.sort_by(|a, b| {
                            a.severity
                                .cmp(&b.severity)
                                .then_with(|| a.code.cmp(b.code))
                                .then_with(|| a.object.cmp(&b.object))
                        });
                        // Clamp the cursor — re-sort may have moved
                        // the focused row's index.
                        let last = self.schema_lint_findings.len().saturating_sub(1);
                        if self.schema_lint_cursor > last {
                            self.schema_lint_cursor = last;
                        }
                        let total = self.schema_lint_findings.len();
                        let high = self
                            .schema_lint_findings
                            .iter()
                            .filter(|f| f.severity == crate::query::lint::Severity::High)
                            .count();
                        self.last_status = Some(format!(
                            "schema lint · {total} finding(s) · {high} high · live: +{added}"
                        ));
                    }
                    Err(e) => {
                        // Live check failed — leave the pure
                        // findings in place. Surface the failure
                        // so the operator knows the FK-index
                        // check didn't run.
                        self.last_status = Some(format!(
                            "schema lint · live check failed: {e} (showing cached-only)"
                        ));
                    }
                }
            }
            AppMsg::CostPreviewLoaded {
                sql,
                decision,
                estimated,
                threshold,
                ..
            } => {
                // Clear the pre-flight busy flag — spawn_run sets
                // its own when the real query goes; the Confirm
                // modal doesn't run a query so it should also clear.
                self.query_running = false;
                match estimated {
                    Ok(rows) if rows > threshold as f64 => {
                        // Over threshold — gate behind Confirm. Reuse
                        // the existing pending_run machinery so y/n
                        // wiring stays in one place.
                        let summary = format!(
                            "cost preview: estimated {} rows (threshold {threshold}) — proceed?",
                            format_row_estimate(rows),
                        );
                        self.last_status = Some(summary.clone());
                        self.pending_run = Some(PendingRun {
                            sql,
                            kind: RunKind::Run,
                            decision,
                            is_batch: false,
                            summary: Some(summary),
                        });
                        self.mode = Mode::Confirm;
                    }
                    Ok(rows) => {
                        // Under threshold — proceed silently.
                        self.last_status = Some(format!(
                            "pre-flight ok · est {} rows",
                            format_row_estimate(rows)
                        ));
                        self.spawn_run(sql, RunKind::Run, decision, false);
                    }
                    Err(e) => {
                        // EXPLAIN itself failed — don't block; surface
                        // and proceed (the real query will fail too if
                        // it's e.g. a syntax error).
                        tracing::warn!("cost preview EXPLAIN failed: {e}");
                        self.last_status = Some(format!("pre-flight skipped: {e}"));
                        self.spawn_run(sql, RunKind::Run, decision, false);
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
                    notice
                        .detail
                        .as_deref()
                        .map(|d| format!(" · detail: {d}"))
                        .unwrap_or_default(),
                    notice
                        .hint
                        .as_deref()
                        .map(|h| format!(" · hint: {h}"))
                        .unwrap_or_default(),
                );
                self.notices.push(notice);
                if self.notices.len() > 50 {
                    self.notices.remove(0);
                }
            }
            AppMsg::Notification { notification, .. } => {
                // Brief status flash so the operator notices an
                // arrival even without the `N` panel open. The
                // ring buffer carries the full history for the
                // panel to render later.
                let preview: String = notification.payload.chars().take(40).collect();
                self.last_status = Some(format!(
                    "NOTIFY {} (pid {}): {preview}",
                    notification.channel, notification.pid
                ));
                tracing::info!(
                    "pg notify · channel={} pid={} payload={}",
                    notification.channel,
                    notification.pid,
                    notification.payload,
                );
                self.notifications.push(notification);
                if self.notifications.len() > NOTIFICATION_CAP {
                    let drop = self.notifications.len() - NOTIFICATION_CAP;
                    self.notifications.drain(..drop);
                }
            }
            AppMsg::TapEvent { event } => self.on_tap_event(event),
        }
    }

    /// Fold one [`crate::tap::TapEvent`] into the app state.
    /// Pure-ish (does not perform I/O) so the run-loop tests
    /// can drive it directly. Behaviour per kind:
    /// - Query / TxnBoundary: push into `tap_events` ring,
    ///   evict oldest past `TAP_CAP`, update `tap_health`.
    /// - Heartbeat: never push into the ring (chatter); just
    ///   update `tap_health` with the dropped-events counter
    ///   and bump the heartbeat-count tally.
    pub fn on_tap_event(&mut self, event: crate::tap::TapEvent) {
        self.tap_health.last_event_at_unix_micros = event.received_at_unix_micros;
        match event.kind {
            crate::tap::TapKind::Heartbeat => {
                self.tap_health.heartbeat_count = self.tap_health.heartbeat_count.saturating_add(1);
                if let Some(d) = event.dropped_events_total {
                    self.tap_health.dropped_events_total = d;
                }
            }
            crate::tap::TapKind::Query | crate::tap::TapKind::TxnBoundary => {
                if matches!(event.kind, crate::tap::TapKind::Query) {
                    self.tap_health.query_count = self.tap_health.query_count.saturating_add(1);
                }
                self.tap_events.push_back(event);
                while self.tap_events.len() > TAP_CAP {
                    self.tap_events.pop_front();
                    // Cursor follows the eviction so a viewer
                    // parked on the oldest row doesn't suddenly
                    // jump forward in content.
                    self.tap_events_cursor = self.tap_events_cursor.saturating_sub(1);
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
            && self.mode != Mode::Editor
        {
            self.should_quit = true;
            return;
        }
        // Fall through to the editor's on_editor_key for the
        // cancel-or-no-op logic.
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

        // F1 opens help from ANY mode (except Help itself, which closes
        // on F1 / esc / ? / q). The cheat-sheet auto-scrolls to the
        // section for the mode we came from so the operator sees the
        // relevant keys immediately.
        if matches!(key.code, KeyCode::F(1)) && self.mode != Mode::Help {
            self.open_help_from(self.mode);
            return;
        }
        // F2 expands the most-recent query failure into the rich
        // error overlay (severity / code / detail / hint / affected
        // schema/table/column/constraint). No-op when there's
        // nothing to show.
        if matches!(key.code, KeyCode::F(2)) && self.mode != Mode::ErrorDetail {
            if self.last_error_detail.is_some() || self.last_error.is_some() {
                self.mode = Mode::ErrorDetail;
            } else {
                self.last_status = Some("no error to expand".into());
            }
            return;
        }
        // F3 opens the NOTIFY arrivals panel from anywhere — the
        // operator may have run `LISTEN` then walked off to do
        // something else; new arrivals shouldn't require a
        // round-trip back to Normal mode to inspect.
        if matches!(key.code, KeyCode::F(3)) && self.mode != Mode::Notifications {
            self.start_notifications();
            return;
        }
        // F4: open the JDBC-tap monitor. Same universal pattern
        // as F1/F2/F3 — the JAR may be feeding events from the
        // operator's app any time pgman is open; one keystroke
        // gets you to the live stream from any mode.
        if matches!(key.code, KeyCode::F(4)) && self.mode != Mode::TapMonitor {
            self.start_tap_monitor();
            return;
        }
        // Tab management. Universal so the operator can switch
        // tabs from anywhere except input modes where the chord
        // would conflict with typing. Ctrl-T new, Ctrl-W close
        // the current tab, Ctrl-Tab / Ctrl-Shift-Tab cycle,
        // Alt-1..Alt-9 jump directly (Ctrl-N collides with
        // history-next, so we use Alt-N for the jump shortcut).
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let typing_mode = self.mode.is_text_input();
        if ctrl && matches!(key.code, KeyCode::Char('t')) {
            self.new_tab();
            return;
        }
        if ctrl && matches!(key.code, KeyCode::Char('w')) && !typing_mode {
            // `w` conflicts with `\watch` in editor mode — gate
            // close-tab to non-editor modes. Operators close tabs
            // from Normal (most common) or from any non-typing
            // panel.
            self.close_active_tab();
            return;
        }
        if ctrl && matches!(key.code, KeyCode::Tab) {
            self.cycle_tab(!shift);
            return;
        }
        if ctrl && shift && matches!(key.code, KeyCode::BackTab) {
            // Some terminals deliver Ctrl-Shift-Tab as BackTab.
            self.cycle_tab(false);
            return;
        }
        if alt {
            if let KeyCode::Char(c) = key.code {
                if let Some(d) = c.to_digit(10) {
                    if d >= 1 && (d as usize) <= self.tabs.len() {
                        self.switch_to_tab((d as usize) - 1);
                        return;
                    }
                }
            }
        }

        let pre_mode = self.mode;
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
            Mode::GridFind => self.on_grid_find_key(key),
            Mode::ExplainTree => self.on_explain_tree_key(key),
            Mode::SchemaBrowser => self.on_schema_browser_key(key),
            Mode::SchemaBrowserFilter => self.on_schema_browser_filter_key(key),
            Mode::SlowQueries => self.on_slow_queries_key(key),
            Mode::Sessions => self.on_sessions_key(key),
            Mode::SchemaLint => self.on_schema_lint_key(key),
            Mode::ErrorDetail => self.on_error_detail_key(key),
            Mode::ConfirmTerminate => self.on_confirm_terminate_key(key),
            Mode::Notifications => self.on_notifications_key(key),
            Mode::TapMonitor => self.on_tap_monitor_key(key),
            Mode::SavedQueries => self.on_saved_queries_key(key),
            Mode::SaveQueryPrompt => self.on_save_query_prompt_key(key),
            Mode::SavedQueriesFilter => self.on_saved_queries_filter_key(key),
            Mode::RenameQueryPrompt => self.on_rename_query_key(key),
            Mode::ParamPrompt => self.on_param_prompt_key(key),
            Mode::ResultDiff => self.on_result_diff_key(key),
            Mode::Editor => self.on_editor_key(key),
            Mode::Normal => self.on_normal_key(key),
        }
        if self.mode != pre_mode {
            self.note_mode_entry(self.mode);
        }
    }

    /// First time a mode opens in this session, flash a one-line
    /// hint into the status footer surfacing the keys that are
    /// most useful in there. After the first visit the hint is
    /// suppressed so it doesn't nag.
    fn note_mode_entry(&mut self, mode: Mode) {
        if !self.mode_seen.insert(mode) {
            return; // already shown
        }
        let hint = match mode {
            Mode::SchemaBrowser => Some(
                "tip · / filter · [ ] jump schema · + / − expand-all/collapse-all · F1 full keys",
            ),
            Mode::ExplainTree => {
                Some("tip · j/k navigate · enter expand · hottest node is red · F1 full keys")
            }
            Mode::SlowQueries => Some(
                "tip · enter copy SQL to editor · r refresh from pg_stat_statements · F1 full keys",
            ),
            Mode::Sessions => Some(
                "tip · blocked sessions sort top in red · K terminate · r refresh · F1 full keys",
            ),
            Mode::SchemaLint => {
                Some("tip · severity-sorted (HIGH first) · y yanks SQL suggestion · F1 full keys")
            }
            Mode::LogPick => {
                Some("tip · c toggles cluster view · enter loads selected · F1 full keys")
            }
            Mode::RowDetail => {
                Some("tip · enter zooms a field · y yanks · j/k between fields · F1 full keys")
            }
            Mode::CellDetail => {
                Some("tip · JSON cells render as a tree · y yanks the value (or jq path)")
            }
            // Modes where the footer's primary content is already
            // an instruction (typing prompts) or where the hint
            // would be noise:
            _ => None,
        };
        if let Some(h) = hint {
            self.last_status = Some(h.into());
        }
    }

    /// `N` from Normal — open the LISTEN/NOTIFY arrivals panel.
    /// Unlike SlowQueries / Sessions, there's nothing to fetch
    /// here — the ring is populated passively by the connection
    /// driver as notifications arrive. Even an empty ring opens
    /// (with a hint to LISTEN to a channel first).
    fn start_notifications(&mut self) {
        self.notifications_cursor = self
            .notifications_cursor
            .min(self.notifications.len().saturating_sub(1));
        self.last_status = Some(format!(
            "NOTIFY arrivals · {} stashed · LISTEN <chan> from the editor to subscribe",
            self.notifications.len()
        ));
        self.mode = Mode::Notifications;
    }

    /// Enter `Mode::TapMonitor` and surface a one-line status
    /// summarising what the tap listener has seen so far. The
    /// status text covers both the "JAR connected, no traffic"
    /// case (when the ring is empty but heartbeats arrived) and
    /// the dominant "live stream" case.
    fn start_tap_monitor(&mut self) {
        self.tap_events_cursor = self
            .tap_events_cursor
            .min(self.tap_events.len().saturating_sub(1));
        let queries = self.tap_health.query_count;
        let beats = self.tap_health.heartbeat_count;
        let dropped = self.tap_health.dropped_events_total;
        self.last_status = Some(if queries == 0 && beats == 0 {
            "JDBC tap · no events yet · start pgman with --tap-listen and configure pgman-tap in the JVM".into()
        } else {
            let dropped_suffix = if dropped > 0 {
                format!(" · {dropped} dropped (JAR backpressure)")
            } else {
                String::new()
            };
            format!("JDBC tap · {queries} queries · {beats} heartbeats{dropped_suffix}")
        });
        self.mode = Mode::TapMonitor;
    }

    /// Clear the tap event ring (`c` from any tap view) and re-home
    /// every per-view cursor. One ring backs all views, so a clear must
    /// reset all cursors — hand-maintained per-view copies had drifted,
    /// leaving stale cursors after a clear. The captured baseline
    /// snapshot is intentionally preserved (only the live ring is wiped).
    fn clear_tap_ring(&mut self) {
        let n = self.tap_events.len();
        self.tap_events.clear();
        self.tap_events_cursor = 0;
        self.tap_hotspots_cursor = 0;
        self.tap_callers_cursor = 0;
        self.tap_txns_cursor = 0;
        self.tap_pools_cursor = 0;
        self.tap_nplus1_cursor = 0;
        self.tap_baseline_cursor = 0;
        self.last_status = Some(format!("cleared {n} tap event(s)"));
    }

    /// Compute the current per-transaction stats. Cheap —
    /// one pass over the ring.
    pub fn current_txns(&self) -> Vec<crate::tap::TxnStats> {
        crate::tap::group_by_txn(self.tap_events.iter())
    }

    /// Compute the current per-pool saturation stats. Cheap —
    /// one pass over the ring (plus a per-pool endpoint sweep
    /// for peak concurrency).
    pub fn current_pools(&self) -> Vec<crate::tap::PoolStats> {
        crate::tap::group_by_pool(self.tap_events.iter())
    }

    /// Freeze the current hotspots list as the diff baseline
    /// and flash a status confirming it. Re-pressing `B`
    /// recaptures (the snapshot is always vs the most recent
    /// capture, never additive).
    fn capture_tap_baseline(&mut self) {
        let hotspots = self.current_hotspots();
        let summary = format!(
            "tap baseline captured · {} fingerprint(s) · {} event(s)",
            hotspots.len(),
            self.tap_events.len()
        );
        self.tap_baseline = Some(TapBaseline {
            captured_at_unix_micros: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros() as u64)
                .unwrap_or(0),
            captured_event_count: self.tap_events.len(),
            captured_listener_dropped: crate::tap::dropped_at_listener(),
            hotspots,
        });
        self.tap_baseline_cursor = 0;
        self.last_status = Some(summary);
    }

    /// Number of events the listener dropped between baseline
    /// capture and now. Surfaced in the Baseline view header
    /// — non-zero means the diff is operating on an incomplete
    /// view of the workload (the missing events would have
    /// landed in `current_hotspots` but never did).
    pub fn baseline_listener_drops_since_capture(&self) -> Option<u64> {
        let captured = self.tap_baseline.as_ref()?.captured_listener_dropped;
        let current = crate::tap::dropped_at_listener();
        Some(current.saturating_sub(captured))
    }

    /// Compute the current baseline diff. Returns an empty
    /// vec when no baseline has been captured — the renderer
    /// detects that case and prompts the operator to press
    /// `Shift-B`.
    pub fn current_baseline_diff(&self) -> Vec<crate::tap::HotspotDiff> {
        let Some(baseline) = self.tap_baseline.as_ref() else {
            return Vec::new();
        };
        let current = self.current_hotspots();
        crate::tap::diff_hotspots(&baseline.hotspots, &current, false)
    }

    /// Cycle the TapMonitor view (List → Hotspots → Callers
    /// → NplusOne) and flash the new view's name in the status
    /// line so the operator confirms the switch happened.
    /// Each view re-clamps its own cursor.
    fn cycle_tap_view(&mut self) {
        self.tap_view = self.tap_view.next();
        match self.tap_view {
            TapView::List => {
                self.last_status = Some(format!(
                    "tap view · list ({} event(s))",
                    self.tap_events.len()
                ));
            }
            TapView::Hotspots => {
                self.tap_hotspots_cursor = 0;
                self.last_status = Some(format!(
                    "tap view · hotspots · sort: {}",
                    self.tap_sort.label()
                ));
            }
            TapView::Callers => {
                self.tap_callers_cursor = 0;
                self.last_status = Some(format!(
                    "tap view · callers · sort: {}",
                    self.tap_sort.label()
                ));
            }
            TapView::Transactions => {
                self.tap_txns_cursor = 0;
                let txns = self.current_txns();
                let open = txns.iter().filter(|t| t.is_open()).count();
                self.last_status = Some(format!(
                    "tap view · transactions · {} total · {} open",
                    txns.len(),
                    open
                ));
            }
            TapView::Pools => {
                self.tap_pools_cursor = 0;
                let pools = self.current_pools();
                self.last_status = Some(format!("tap view · pools · {} pool(s)", pools.len()));
            }
            TapView::NplusOne => {
                self.tap_nplus1_cursor = 0;
                let findings = self.current_nplus1();
                self.last_status = Some(format!("tap view · N+1 · {} finding(s)", findings.len()));
            }
            TapView::Baseline => {
                self.tap_baseline_cursor = 0;
                let summary = match self.tap_baseline.as_ref() {
                    Some(b) => format!(
                        "tap view · baseline diff · {} fingerprint(s) captured · {} changed",
                        b.hotspots.len(),
                        self.current_baseline_diff().len()
                    ),
                    None => "tap view · baseline diff · press Shift-B to capture a baseline".into(),
                };
                self.last_status = Some(summary);
            }
        }
    }

    /// Compute the current hotspot list per `tap_sort`. Called
    /// each frame from the renderer and from the key handler.
    /// Cheap relative to the rest of the frame budget — ~2k
    /// events × one fingerprint each is sub-millisecond.
    pub fn current_hotspots(&self) -> Vec<crate::tap::Hotspot> {
        crate::tap::group_hotspots(self.tap_events.iter(), self.tap_sort)
    }

    /// Compute the current N+1 findings — called by the panel
    /// renderer on demand. Uses the defaults
    /// (`NPLUS1_WINDOW_MICROS`, `NPLUS1_MIN_REPEATS`) which
    /// match the offline classifier's operating point.
    pub fn current_nplus1(&self) -> Vec<crate::tap::NplusOneFinding> {
        crate::tap::detect_nplus1(
            self.tap_events.iter(),
            crate::tap::NPLUS1_WINDOW_MICROS,
            crate::tap::NPLUS1_MIN_REPEATS,
        )
    }

    /// Compute the current per-caller rollup per `tap_sort`.
    /// Same shape as `current_hotspots` but the grouping key
    /// is the innermost caller frame instead of the SQL
    /// fingerprint.
    pub fn current_callers(&self) -> Vec<crate::tap::CallerStats> {
        crate::tap::group_by_caller(self.tap_events.iter(), self.tap_sort)
    }

    /// Copy the focused notification's payload to the clipboard.
    fn yank_focused_notification(&mut self) {
        let Some(n) = self.notifications.get(self.notifications_cursor) else {
            return;
        };
        let text = n.payload.clone();
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text.clone())) {
            Ok(()) => {
                self.last_status =
                    Some(format!("yanked payload ({} char(s))", text.chars().count()));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }

    /// `Ctrl-S` in editor — prompt for a name (with the existing
    /// saved entry's name pre-filled if the buffer matches one
    /// — otherwise empty). Enter persists; Esc cancels.
    /// Snapshot the currently-active per-session fields into
    /// `tabs[active_tab]`. Called before every switch and on
    /// every tab-close. Pure mechanical copy — no side effects.
    fn snapshot_active_tab(&mut self) {
        let snap = TabSnapshot {
            editor_buffer: self.editor_buffer.clone(),
            editor_cursor: self.editor_cursor,
            editor_scroll: self.editor_scroll,
            editor_preferred_col: self.editor_preferred_col,
            editor_undo: self.editor_undo.clone(),
            editor_redo: self.editor_redo.clone(),
            grid: self.grid.clone(),
            grid_selected: self.grid_state.selected(),
            grid_col_cursor: self.grid_col_cursor,
            grid_sort: self.grid_sort,
            grid_raw_rows: self.grid_raw_rows.clone(),
            grid_filter: self.grid_filter.clone(),
            grid_visible_rows: self.grid_visible_rows.clone(),
            last_run_sql: self.last_run_sql.clone(),
            grid_source: self.grid_source.clone(),
            pinned_result: self.pinned_result.clone(),
        };
        if let Some(slot) = self.tabs.get_mut(self.active_tab) {
            *slot = snap;
        }
    }

    /// Restore `tabs[active_tab]` into the per-session fields.
    /// Mirror of `snapshot_active_tab`.
    fn load_active_tab(&mut self) {
        let snap = match self.tabs.get(self.active_tab) {
            Some(s) => s.clone(),
            None => return,
        };
        self.editor_buffer = snap.editor_buffer;
        self.editor_cursor = snap.editor_cursor;
        self.editor_scroll = snap.editor_scroll;
        self.editor_preferred_col = snap.editor_preferred_col;
        self.editor_undo = snap.editor_undo;
        self.editor_redo = snap.editor_redo;
        self.grid = snap.grid;
        self.grid_state.select(snap.grid_selected);
        self.grid_col_cursor = snap.grid_col_cursor;
        self.grid_sort = snap.grid_sort;
        self.grid_raw_rows = snap.grid_raw_rows;
        self.grid_filter = snap.grid_filter;
        self.grid_visible_rows = snap.grid_visible_rows;
        self.last_run_sql = snap.last_run_sql;
        self.grid_source = snap.grid_source;
        self.pinned_result = snap.pinned_result;
    }

    /// Close the transient result-diff overlay if one is open. Called
    /// on every tab change: the overlay is bound to the tab's live grid,
    /// which is about to swap out from under it, so it must not survive
    /// the switch. The per-tab `pinned_result` baseline is preserved
    /// (snapshotted/restored separately).
    fn dismiss_result_diff(&mut self) {
        if self.mode == Mode::ResultDiff {
            self.mode = Mode::Normal;
        }
        self.result_diff = None;
        self.result_diff_cursor = 0;
    }

    /// `Ctrl-T` — push a fresh tab and switch to it. Refuses
    /// past `TAB_CAP` with an actionable status.
    pub fn new_tab(&mut self) {
        if self.tabs.len() >= TAB_CAP {
            self.last_status = Some(format!("max tabs reached ({TAB_CAP}) — close one first"));
            return;
        }
        if self.query_running {
            self.last_status = Some("can't switch tabs while a query is running".into());
            return;
        }
        self.dismiss_result_diff();
        self.snapshot_active_tab();
        self.tabs.push(TabSnapshot::default());
        self.active_tab = self.tabs.len() - 1;
        self.load_active_tab();
        // Reset transient state that doesn't belong to a tab
        // (completion popup, history nav).
        self.completion = None;
        self.history_pos = None;
        self.last_status = Some(format!(
            "new tab · now on tab {}/{}",
            self.active_tab + 1,
            self.tabs.len()
        ));
    }

    /// `Ctrl-W` — close the active tab. The next tab becomes
    /// active (or the previous if active was last). No-op when
    /// only one tab exists (closing the last is a quit-via-q).
    pub fn close_active_tab(&mut self) {
        if self.tabs.len() <= 1 {
            self.last_status = Some("only one tab open · q to quit".into());
            return;
        }
        if self.query_running {
            self.last_status = Some("can't close tab while a query is running".into());
            return;
        }
        self.dismiss_result_diff();
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.load_active_tab();
        self.completion = None;
        self.last_status = Some(format!(
            "closed tab · now on tab {}/{}",
            self.active_tab + 1,
            self.tabs.len()
        ));
    }

    /// Jump to `idx` (0-based). No-op out-of-range / same tab.
    pub fn switch_to_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() || idx == self.active_tab {
            return;
        }
        if self.query_running {
            self.last_status = Some("can't switch tabs while a query is running".into());
            return;
        }
        self.dismiss_result_diff();
        self.snapshot_active_tab();
        self.active_tab = idx;
        self.load_active_tab();
        self.completion = None;
        self.history_pos = None;
        self.last_status = Some(format!("tab {}/{}", self.active_tab + 1, self.tabs.len()));
    }

    /// Step forward / backward through the tab list, wrapping.
    pub fn cycle_tab(&mut self, forward: bool) {
        if self.tabs.len() <= 1 {
            return;
        }
        let n = self.tabs.len();
        let next = if forward {
            (self.active_tab + 1) % n
        } else {
            (self.active_tab + n - 1) % n
        };
        self.switch_to_tab(next);
    }

    fn start_save_query_prompt(&mut self) {
        if self.editor_buffer.trim().is_empty() {
            self.last_status = Some("editor empty — nothing to save".into());
            return;
        }
        // Default name: derive from the first ~40 chars of the
        // buffer with a non-identifier sanitisation, so the
        // operator has a starting point. They can backspace and
        // type their own.
        self.save_query_name = default_query_name(&self.editor_buffer);
        self.last_status = Some("save query · type a name · enter persist · esc cancel".into());
        self.mode = Mode::SaveQueryPrompt;
    }

    /// `Q` from Normal / `Ctrl-O` from Editor — open the list of
    /// persisted queries.
    fn open_saved_queries(&mut self) {
        if self.saved_queries.entries.is_empty() {
            self.last_status = Some("no saved queries · Ctrl-S in editor to save one".into());
            return;
        }
        self.saved_queries_filter = None;
        self.saved_queries_cursor = self
            .saved_queries_cursor
            .min(self.saved_queries.entries.len() - 1);
        self.last_status = Some(format!(
            "saved queries · {} entries",
            self.saved_queries.entries.len()
        ));
        self.mode = Mode::SavedQueries;
    }

    /// Load a saved query into the editor. If its body contains
    /// `:param` placeholders, start the [`Mode::ParamPrompt`] flow
    /// to collect values first; otherwise load it directly.
    fn load_saved_query(&mut self, q: crate::saved::SavedQuery) {
        let params = crate::query::params::extract_params(&q.body);
        if params.is_empty() {
            self.load_sql_into_editor(q.body, format!("loaded saved query '{}'", q.name));
            return;
        }
        let n = params.len();
        self.last_status = Some(format!(
            "'{}' needs {n} value(s) · enter each · esc cancels",
            q.name
        ));
        self.param_prompt = Some(ParamPrompt {
            query_name: q.name,
            template: q.body,
            params,
            idx: 0,
            values: Vec::new(),
            input: TextInput::new(),
        });
        self.mode = Mode::ParamPrompt;
    }

    /// Drop `sql` into the editor buffer and switch to the editor.
    /// Shared by the direct and post-`:param`-prompt load paths.
    fn load_sql_into_editor(&mut self, sql: String, status: String) {
        self.editor_buffer = sql;
        self.editor_cursor = self.editor_buffer.len();
        self.editor_preferred_col = None;
        self.history_pos = None;
        self.last_status = Some(status);
        self.mode = Mode::Editor;
    }

    /// Indices into `saved_queries.entries` currently visible
    /// under the active filter (all, in order, when unfiltered).
    pub fn visible_saved_indices(&self) -> Vec<usize> {
        filter_saved_indices(
            &self.saved_queries.entries,
            self.saved_queries_filter.as_ref().map(|t| t.text()),
        )
    }

    /// The real `entries` index under the panel cursor, mapped
    /// through the current filter. `None` when nothing matches.
    fn focused_saved_index(&self) -> Option<usize> {
        self.visible_saved_indices()
            .get(self.saved_queries_cursor)
            .copied()
    }

    fn start_saved_queries_filter(&mut self) {
        self.saved_queries_filter = Some(TextInput::new());
        self.saved_queries_cursor = 0;
        self.mode = Mode::SavedQueriesFilter;
        self.last_status = Some("filter saved queries · type to narrow".into());
    }

    fn start_rename_query(&mut self) {
        let Some(name) = self
            .focused_saved_index()
            .and_then(|i| self.saved_queries.entries.get(i))
            .map(|q| q.name.clone())
        else {
            self.last_status = Some("nothing to rename".into());
            return;
        };
        self.rename_query_from = name.clone();
        self.rename_query_buffer = TextInput::with_text(name);
        self.mode = Mode::RenameQueryPrompt;
        self.last_status = Some("rename · edit name · enter save · esc cancel".into());
    }

    /// Fire `SELECT pg_terminate_backend(<pid>)` against the
    /// live client. Result lands as a sessions refresh on
    /// success; error surfaces in `last_error` via the standard
    /// error pipeline. Routes around the safety guard because
    /// the operator just confirmed in the modal.
    fn spawn_terminate_session(&mut self, pid: i32) {
        let Some(client) = self.client.clone() else {
            self.last_error = Some("not connected".into());
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        self.last_status = Some(format!("terminating pid {pid}…"));
        tokio::spawn(async move {
            let sql = "SELECT pg_terminate_backend($1)";
            match client.query_opt(sql, &[&pid]).await {
                Ok(_) => {
                    // Re-fetch sessions so the panel reflects the
                    // termination. Same panel SQL the `r` refresh
                    // uses.
                    let result =
                        match conn::run_query(&client, crate::query::sessions::PANEL_SQL).await {
                            Ok(grid) => Ok(crate::query::sessions::parse(&grid)),
                            Err(e) => Err(e),
                        };
                    let _ = tx.send(AppMsg::SessionsLoaded { generation, result });
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::QueryFailed {
                        generation,
                        error: format!("terminate pid {pid} failed: {e}"),
                        position: None,
                        detail: None,
                    });
                }
            }
        });
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
    /// Zoom into the currently-focused field. No-op when the row or
    /// field cursor is out of bounds. When the cell parses as a JSON
    /// object or array, also primes the tree-navigator state; scalars
    /// and non-JSON content fall back to the wrapped-text renderer.
    fn open_cell_detail(&mut self) {
        let Some(idx) = self.selected_grid_row_idx() else {
            return;
        };
        let Some(row) = self.grid.rows.get(idx) else {
            return;
        };
        let Some(value) = row.get(self.row_detail_field) else {
            return;
        };
        self.cell_detail_scroll = 0;
        self.json_cell_rows.clear();
        self.json_cell_cursor = 0;
        self.json_cell_collapsed.clear();
        if let Some(parsed) = crate::query::json_cell::parse_jsonb_cell(value) {
            self.json_cell_rows =
                crate::query::json_cell::flatten(&parsed, &self.json_cell_collapsed);
            // Stash the parsed value so collapse/expand can re-flatten.
            self.json_cell_value = Some(parsed);
        } else {
            self.json_cell_value = None;
        }
        self.mode = Mode::CellDetail;
    }

    /// Cell-detail modal. Two key maps depending on whether the cell
    /// parses as a JSON container:
    ///   - JSON view: j/k move the tree cursor, Enter / Space / h / l
    ///     toggle collapse on the focused container, `y` yanks the
    ///     jq-style path of the focused node.
    ///   - Text view: j/k scroll the wrapped value, `y` yanks the
    ///     whole value. Same shortcut, different semantics.
    /// Esc/q always pops back to the row view.
    /// Toggle expand/collapse of the focused JSON node. Scalars are
    /// a no-op. Re-flattens the row list and clamps the cursor to
    /// remain on the same path (or, if the path vanished because a
    /// parent collapsed, on its parent's row).
    fn toggle_json_cell_node(&mut self) {
        let Some(row) = self.json_cell_rows.get(self.json_cell_cursor).cloned() else {
            return;
        };
        if !matches!(
            row.display,
            crate::query::json_cell::JsonDisplay::Container { .. }
        ) {
            return;
        }
        if self.json_cell_collapsed.contains(&row.path) {
            self.json_cell_collapsed.remove(&row.path);
        } else {
            self.json_cell_collapsed.insert(row.path.clone());
        }
        if let Some(v) = &self.json_cell_value {
            self.json_cell_rows = crate::query::json_cell::flatten(v, &self.json_cell_collapsed);
        }
        // Try to keep the cursor on the same path; fall back to the
        // tail if the row list shrank past it.
        let new_idx = self
            .json_cell_rows
            .iter()
            .position(|r| r.path == row.path)
            .unwrap_or_else(|| self.json_cell_rows.len().saturating_sub(1));
        self.json_cell_cursor = new_idx;
    }

    /// Yank the focused JSON node's jq-style path (`.foo[0].bar`) to
    /// the clipboard. The root node yanks `.` for convenience.
    fn yank_json_cell_path(&mut self) {
        let Some(row) = self.json_cell_rows.get(self.json_cell_cursor) else {
            return;
        };
        let path = if row.path.is_empty() {
            ".".to_string()
        } else {
            row.path.clone()
        };
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(path.clone())) {
            Ok(()) => {
                self.last_status = Some(format!("yanked path '{path}'"));
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("yank failed: {e}"));
            }
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
    /// Open the help overlay from `from`. Captures `from` so the
    /// close path restores that mode (instead of always going to
    /// Normal), and pre-scrolls the help body to the section that
    /// matches `from` — operators see the relevant keys without
    /// hunting for them.
    pub fn open_help_from(&mut self, from: Mode) {
        self.help_origin = Some(from);
        self.help_scroll = 0; // Renderer-side anchor pass will adjust.
        self.mode = Mode::Help;
    }

    /// Pure: anchor heading text that `draw_help` uses to position
    /// the initial scroll for a given source mode. `None` falls
    /// back to top-of-document.
    pub fn help_anchor_for(mode: Mode) -> Option<&'static str> {
        match mode {
            Mode::Normal => Some("grid"),
            Mode::Editor => Some("editor"),
            Mode::HistorySearch => Some("editor"),
            Mode::Confirm => Some("confirm"),
            Mode::TxDecision => Some("tx open"),
            Mode::LogPick => Some("log pick"),
            Mode::ConnPick => Some("conn pick"),
            Mode::RowDetail => Some("row detail"),
            Mode::CellDetail => Some("cell detail"),
            Mode::SchemaBrowser | Mode::SchemaBrowserFilter => Some("schema browser"),
            Mode::SchemaLint => Some("schema wizard"),
            Mode::ErrorDetail => Some("editor"),
            Mode::ConfirmTerminate => Some("active sessions"),
            Mode::Notifications => Some("notifications"),
            Mode::TapMonitor => Some("jdbc tap"),
            Mode::SavedQueries => Some("saved queries"),
            Mode::SaveQueryPrompt => Some("editor"),
            Mode::SavedQueriesFilter => Some("saved queries"),
            Mode::RenameQueryPrompt => Some("saved queries"),
            Mode::ParamPrompt => Some("saved queries"),
            Mode::ResultDiff => Some("result diff"),
            Mode::ExplainTree => Some("EXPLAIN tree"),
            Mode::SlowQueries => Some("slow queries"),
            Mode::Sessions => Some("active sessions"),
            Mode::GridFilter => Some("grid"),
            Mode::GridFind => Some("grid"),
            Mode::About => Some("grid"),
            Mode::Help => None,
        }
    }

    /// Inner editor-key handler. Wrapper above adds undo/redo
    /// snapshotting around it; this body holds the original key
    /// dispatch.
    fn on_editor_key_inner(&mut self, key: KeyEvent) {
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
        if matches!(key.code, KeyCode::Char(' ')) && key.modifiers.contains(KeyModifiers::CONTROL) {
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
            // Ctrl-S — prompt for a name and persist the editor
            // buffer as a saved query.
            KeyCode::Char('s') if ctrl => self.start_save_query_prompt(),
            // Ctrl-O — open the saved-queries panel for loading.
            KeyCode::Char('o') if ctrl => self.open_saved_queries(),
            // Ctrl-/ — toggle a `-- ` line comment on the
            // current line. Some terminals deliver this as
            // Char('/') with CONTROL, others as Char('_') (the
            // ASCII control code for /) — accept either.
            KeyCode::Char('/') | KeyCode::Char('_') if ctrl => {
                self.editor_dirty();
                editor_toggle_line_comment(&mut self.editor_buffer, &mut self.editor_cursor);
            }
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

            // Plain typing — only when no Ctrl/Alt. Includes
            // bracket autoclose: `(` / `[` / `{` insert a pair
            // with the cursor between; `)` / `]` / `}` skip over
            // a matching close immediately after the cursor so
            // typing `(` then `)` exits the pair cleanly. Quote
            // autoclose (`'` / `"`) follows the same shape but
            // with a conservative neighbour-check so it doesn't
            // interfere with SQL `''` escaping or in-word
            // apostrophes.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.editor_dirty();
                if matches!(c, '(' | '[' | '{') {
                    editor_insert_pair(&mut self.editor_buffer, &mut self.editor_cursor, c);
                } else if matches!(c, ')' | ']' | '}')
                    && editor_maybe_skip_close(&self.editor_buffer, &mut self.editor_cursor, c)
                {
                    // Skipped over the matching close.
                } else if matches!(c, '\'' | '"')
                    && editor_maybe_skip_quote(&self.editor_buffer, &mut self.editor_cursor, c)
                {
                    // Skipped over the matching quote.
                } else if matches!(c, '\'' | '"')
                    && editor_maybe_pair_quote(&mut self.editor_buffer, &mut self.editor_cursor, c)
                {
                    // Paired and placed cursor between the quotes.
                } else {
                    editor_insert(&mut self.editor_buffer, &mut self.editor_cursor, c);
                }
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
            "FROM", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "INTO", "WHERE", "AND",
            "OR", "ON",
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
        let Some(id) = complete_q::extract_identifier(&self.editor_buffer, self.editor_cursor)
        else {
            return;
        };
        // Empty prefix is fine — the candidate set falls back to "all
        // identifier-shaped candidates for the surrounding clause"
        // (matches the Tab-on-whitespace UX). The cycle drops naturally
        // when those produce no matches.
        let cands =
            complete_q::candidates_for(&self.editor_buffer, self.editor_cursor, &self.schema_cache);
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

    /// Push a pre-mutation snapshot onto the undo ring. Coalesces
    /// consecutive char-inserts within `UNDO_COALESCE_WINDOW` so
    /// typing `qwerty` is one undo, not six. Any push invalidates
    /// the redo ring (divergent edit = new history branch).
    fn push_undo(&mut self, buffer: String, cursor: usize, kind: EditorActionKind) {
        let now = std::time::Instant::now();
        if let Some(last) = self.editor_undo.last_mut() {
            if should_coalesce_undo(
                last.kind,
                last.merge_window_end,
                kind,
                now,
                UNDO_COALESCE_WINDOW,
            ) {
                // Same typing run — extend the existing entry's
                // window. The "before" state we want to restore on
                // undo is the one already on the stack, NOT this
                // intermediate one.
                last.merge_window_end = now;
                self.editor_redo.clear();
                return;
            }
        }
        self.editor_redo.clear();
        self.editor_undo.push(UndoEntry {
            buffer,
            cursor,
            kind,
            merge_window_end: now,
        });
        if self.editor_undo.len() > UNDO_CAP {
            // Drop the oldest. `Vec::remove(0)` is O(N) but N is
            // small (UNDO_CAP) and undos are rare keys.
            self.editor_undo.remove(0);
        }
    }

    /// Pop the most recent undo entry and restore. Push the current
    /// state to the redo ring so Ctrl-Y can flip back.
    pub fn editor_undo(&mut self) {
        let Some(prev) = self.editor_undo.pop() else {
            self.last_status = Some("nothing to undo".into());
            return;
        };
        let now = std::time::Instant::now();
        self.editor_redo.push(UndoEntry {
            buffer: std::mem::take(&mut self.editor_buffer),
            cursor: self.editor_cursor,
            kind: EditorActionKind::Other,
            merge_window_end: now,
        });
        self.editor_buffer = prev.buffer;
        self.editor_cursor = prev.cursor.min(self.editor_buffer.len());
        self.editor_preferred_col = None;
        self.history_pos = None;
        self.draft_dirty = true;
    }

    /// Pop the most recent redo entry and restore. Push the current
    /// state to the undo ring so Ctrl-Z can flip back. Mirror of
    /// [`Self::editor_undo`].
    pub fn editor_redo(&mut self) {
        let Some(next) = self.editor_redo.pop() else {
            self.last_status = Some("nothing to redo".into());
            return;
        };
        let now = std::time::Instant::now();
        self.editor_undo.push(UndoEntry {
            buffer: std::mem::take(&mut self.editor_buffer),
            cursor: self.editor_cursor,
            kind: EditorActionKind::Other,
            merge_window_end: now,
        });
        self.editor_buffer = next.buffer;
        self.editor_cursor = next.cursor.min(self.editor_buffer.len());
        self.editor_preferred_col = None;
        self.history_pos = None;
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
        let cands =
            complete_q::candidates_for(&self.editor_buffer, self.editor_cursor, &self.schema_cache);
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
            self.last_status = Some(format!("completion · exact match · {}", cand.kind.label()));
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
    /// Tx-open prompt: `y` commits, `n` / `esc` rolls back.
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
        self.log_pick_clusters = crate::query::nplus1::detect(&picks);
        self.log_picks = picks;
        self.log_pick_view = LogPickView::AllQueries;
        self.log_pick_index = 0;
        self.mode = Mode::LogPick;
    }

    /// Number of rows the LogPick popup is currently rendering.
    /// Folds the view choice (all queries vs. cluster summary).
    pub fn log_pick_visible_len(&self) -> usize {
        match self.log_pick_view {
            LogPickView::AllQueries => self.log_picks.len(),
            LogPickView::Clusters => self.log_pick_clusters.len(),
        }
    }

    /// Toggle the LogPick view between all-queries and cluster
    /// summary. Resets the cursor to row 0 so a stale index from
    /// the previous view doesn't render out-of-range.
    fn toggle_log_pick_view(&mut self) {
        self.log_pick_view = match self.log_pick_view {
            LogPickView::AllQueries => LogPickView::Clusters,
            LogPickView::Clusters => LogPickView::AllQueries,
        };
        self.log_pick_index = 0;
        self.last_status = Some(match self.log_pick_view {
            LogPickView::AllQueries => format!("all queries · {}", self.log_picks.len()),
            LogPickView::Clusters => format!(
                "N+1 clusters · {} (of {} queries)",
                self.log_pick_clusters.len(),
                self.log_picks.len()
            ),
        });
    }

    /// Resolve the focused row's runnable SQL — `runnable_sql` for
    /// the AllQueries view, the cluster's `example` for Clusters.
    fn focused_log_pick_sql(&self) -> Option<String> {
        match self.log_pick_view {
            LogPickView::AllQueries => self
                .log_picks
                .get(self.log_pick_index)
                .map(|q| q.runnable_sql.clone()),
            LogPickView::Clusters => self
                .log_pick_clusters
                .get(self.log_pick_index)
                .map(|c| c.example.clone()),
        }
    }

    /// Log-pick browser: j/k navigate, Enter loads the selection into the
    /// editor, Esc cancels, `c` toggles cluster view.
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
                self.last_status = Some(format!("formatted via pg_format · {chars} char(s)"));
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
    /// Toggle the SlowQueries / Sessions auto-refresh. Re-arms
    /// the "last fired" timestamp so the next tick is one full
    /// interval away — no immediate fire on toggle.
    fn toggle_auto_refresh(&mut self) {
        self.auto_refresh = !self.auto_refresh;
        self.auto_refresh_last = Some(Instant::now());
        self.last_status = Some(format!(
            "auto-refresh {} ({}s)",
            if self.auto_refresh { "on" } else { "off" },
            AUTO_REFRESH_INTERVAL.as_secs(),
        ));
    }

    /// Fire a refresh of the focused panel if auto-refresh is on
    /// and the interval has elapsed. No-op outside SlowQueries /
    /// Sessions modes and while a query is in flight (refresh
    /// would queue behind it and arrive cluttered).
    fn tick_auto_refresh(&mut self) {
        if !self.auto_refresh || self.query_running {
            return;
        }
        let now = Instant::now();
        let due = match self.auto_refresh_last {
            Some(t) => now.saturating_duration_since(t) >= AUTO_REFRESH_INTERVAL,
            None => true,
        };
        if !due {
            return;
        }
        self.auto_refresh_last = Some(now);
        match self.mode {
            Mode::SlowQueries => self.refresh_slow_queries(),
            Mode::Sessions => self.refresh_sessions(),
            _ => {}
        }
    }

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
    /// Route a parsed backslash command to the corresponding
    /// interactive action. Called from `request_run` ahead of the
    /// regular safety / spawn path. After dispatch, the editor
    /// buffer is cleared so the next Run press doesn't re-fire
    /// the same command (psql's behaviour too).
    fn dispatch_backslash(&mut self, cmd: crate::query::backslash::BackslashCmd) {
        use crate::query::backslash::BackslashCmd;
        // Clear the buffer immediately so a second F5 doesn't
        // run the same command twice. `\timing` is the exception:
        // operators often toggle it back off in the same buffer.
        let clear_buffer = !matches!(cmd, BackslashCmd::Timing(_));
        if clear_buffer {
            self.editor_buffer.clear();
            self.editor_cursor = 0;
            self.draft_dirty = true;
        }
        match cmd {
            BackslashCmd::Describe(target) => {
                if self.schema_cache.is_empty() {
                    self.last_status =
                        Some("schema cache empty — connect to a database first".into());
                    return;
                }
                // `\d <name>` → open browser with the name as
                // filter; the schema/table/column whose name
                // matches surfaces with its ancestors visible.
                // `\d` alone → open with no filter (default view).
                self.schema_browser_filter = target.clone();
                self.schema_browser_cursor = 0;
                self.mode = Mode::SchemaBrowser;
                self.last_status = Some(match target {
                    Some(t) => format!("\\d {t} → schema browser filtered to '{t}'"),
                    None => "\\d → schema browser".into(),
                });
            }
            BackslashCmd::ListTables | BackslashCmd::ListSchemas => {
                if self.schema_cache.is_empty() {
                    self.last_status =
                        Some("schema cache empty — connect to a database first".into());
                    return;
                }
                self.schema_browser_filter = None;
                self.schema_browser_cursor = 0;
                self.mode = Mode::SchemaBrowser;
                self.last_status = Some("schema browser".into());
            }
            BackslashCmd::Help => self.open_help_from(Mode::Editor),
            BackslashCmd::Quit => self.should_quit = true,
            BackslashCmd::Timing(target) => {
                // Toggle if no explicit value supplied.
                let new = target.unwrap_or(!self.timing_on);
                self.timing_on = new;
                self.last_status = Some(format!("\\timing {}", if new { "on" } else { "off" }));
            }
            BackslashCmd::Report(target) => self.dispatch_report(target),
            BackslashCmd::Fixture(target) => self.dispatch_fixture(target),
            BackslashCmd::Unknown(raw) => {
                self.last_error = Some(format!("unknown backslash command: {raw}"));
            }
        }
    }

    /// `\report` / `\report <path>` handler. Snapshots current
    /// App state, renders as Markdown or HTML per the path
    /// extension, and writes atomically. Default path lives
    /// under the cache dir with a wall-clock-stamped filename.
    fn dispatch_report(&mut self, target: Option<String>) {
        let path = match target {
            Some(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
            _ => default_report_path(),
        };
        let snapshot = self.report_snapshot();
        let body = match crate::report::format_for_path(&path) {
            crate::report::ReportFormat::Markdown => crate::report::render_markdown(&snapshot),
            crate::report::ReportFormat::Html => crate::report::render_html(&snapshot),
        };
        let ok = format!("wrote report to {}", path.display());
        self.write_export(&path, &body, "\\report", ok);
    }

    /// Shared write path for `\report` / `\fixture`: create the parent
    /// directory if needed, write atomically, and set the status (on
    /// success, `ok_status`) or error line. `cmd` names the backslash
    /// command for the error message.
    fn write_export(&mut self, path: &std::path::Path, body: &str, cmd: &str, ok_status: String) {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        match tui_common::util::write_atomic(path, body) {
            Ok(()) => self.last_status = Some(ok_status),
            Err(e) => {
                self.last_error = Some(format!("{cmd} failed: {} ({e})", path.display()));
            }
        }
    }

    /// `\fixture` / `\fixture <path>` handler. Captures the
    /// current result grid as a DBUnit FlatXmlDataSet — the
    /// reverse of the apply script. Requires a non-empty,
    /// single-table result (the source table is the element
    /// name). Writes atomically; default path lives under the
    /// cache dir with a wall-clock-stamped filename.
    fn dispatch_fixture(&mut self, target: Option<String>) {
        if self.grid.rows.is_empty() {
            self.last_error = Some("no result to capture — run a query first".into());
            return;
        }
        let Some((_schema, table)) = self.grid_source.clone() else {
            self.last_error = Some(
                "fixture capture needs a single-table result (no source table inferred)".into(),
            );
            return;
        };
        let fixture = crate::dbunit::fixture_from_rows(&table, &self.grid.columns, &self.grid.rows);
        let xml = crate::dbunit::generate_flat_xml(&fixture);
        let path = match target {
            Some(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
            _ => default_fixture_path(&table),
        };
        let ok = format!("wrote {} row(s) to {}", fixture.rows.len(), path.display());
        self.write_export(&path, &xml, "\\fixture", ok);
    }

    /// Build a [`crate::report::ReportSnapshot`] from the
    /// current App state. Pure (modulo wall-clock for the
    /// generated_at field) so the dispatcher stays small.
    pub fn report_snapshot(&self) -> crate::report::ReportSnapshot {
        let generated_at = chrono_like_now_utc();
        let connection = self.dsn.as_ref().map(|d| d.redacted());
        crate::report::ReportSnapshot {
            title: "pgman report".into(),
            generated_at,
            connection,
            lint_findings: self.schema_lint_findings.clone(),
            hotspots: self.current_hotspots(),
            callers: self.current_callers(),
            transactions: self.current_txns(),
            nplus1: self.current_nplus1(),
            baseline_diff: self
                .tap_baseline
                .as_ref()
                .map(|_| self.current_baseline_diff()),
            listener_dropped: crate::tap::dropped_at_listener(),
            jar_dropped: self.tap_health.dropped_events_total,
        }
    }

    fn request_run(&mut self, kind: RunKind) {
        let sql = self.editor_buffer.trim().to_string();
        if sql.is_empty() {
            self.last_error = Some("editor is empty".to_string());
            return;
        }
        // psql-style `\` commands intercept here, before the
        // not-connected guard — `\?` / `\q` work without a live
        // session. `\d`-class commands DO need a cache to be
        // useful, but the schema-browser opener has its own
        // empty-cache hint.
        if let Some(cmd) = crate::query::backslash::parse_backslash_command(&sql) {
            self.dispatch_backslash(cmd);
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
                self.last_error = Some(format!("blocked by safety: {:?} on '{db}'", decision.kind));
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
            Guard::Allow => {
                // Pre-flight cost preview: for plain `RunKind::Run`
                // SELECTs above the profile's threshold, send an
                // `EXPLAIN (FORMAT JSON)` first and gate on the row
                // estimate. EXPLAIN-class runs (early-return above)
                // and writes (Confirm branch) already have their
                // own gating. Threshold = 0 disables it (default).
                let threshold = self
                    .safety_config
                    .profile_for(db)
                    .cost_preview_threshold_rows;
                if kind == RunKind::Run && threshold > 0 && is_cost_checkable(&sql) {
                    self.spawn_cost_preview(sql, decision, threshold);
                    return;
                }
                self.spawn_run(sql, kind, decision, false);
            }
        }
    }

    /// Send `EXPLAIN (FORMAT JSON)` for `sql`; the result lands as
    /// `AppMsg::CostPreviewLoaded`. The handler decides whether to
    /// confirm or proceed based on the row estimate vs threshold.
    fn spawn_cost_preview(&mut self, sql: String, decision: Decision, threshold: u64) {
        let Some(client) = self.client.clone() else {
            self.last_error = Some("not connected".to_string());
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let explain_sql = format!("EXPLAIN (FORMAT JSON) {sql}");
        self.last_status = Some(format!(
            "pre-flight: explaining (threshold {threshold} rows)…"
        ));
        // Mark busy so the spinner shows, Ctrl-C cancel is offered,
        // and a second F5 doesn't fire while we're awaiting the
        // EXPLAIN. The CostPreviewLoaded handler clears the flag
        // before either spawning the real run (which sets it again)
        // or opening the Confirm modal.
        self.query_running = true;
        tokio::spawn(async move {
            let estimated = run_cost_explain(&client, &explain_sql).await;
            let _ = tx.send(AppMsg::CostPreviewLoaded {
                sql,
                decision,
                estimated,
                threshold,
                generation,
            });
        });
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
                self.last_error = Some(format!("batch blocked by safety: {summary}"));
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
            self.last_error =
                Some("editor is empty — type a fixture file path then ctrl-d".to_string());
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
        // Clean strategy is per-database (safety profile), defaulting
        // to TRUNCATE. Lets a db without TRUNCATE privilege opt into
        // DELETE FROM via `clean_mode = "delete_from"`.
        let db = self
            .dsn
            .as_ref()
            .map(|d| d.dbname.as_str())
            .unwrap_or("default");
        let clean_mode = self.safety_config.profile_for(db).clean_mode;
        self.editor_buffer = dbunit::generate_apply_script(&fixture, clean_mode);
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
        // Push to history (skip consecutive duplicates, cap at
        // HISTORY_CAP entries — shared with the persistence side
        // so the in-memory + on-disk rings can never drift).
        if self.history.last() != Some(&sql) {
            self.history.push(sql.clone());
            if self.history.len() > HISTORY_CAP {
                self.history.remove(0);
            }
        }
        self.history_pos = None;
        // Track the SQL of the most recent plain-Run so the
        // QueryOk handler can re-parse it for the source table.
        // EXPLAIN-wrapped runs hand back a JSON cell whose FROM is
        // the user's query — not the EXPLAIN itself — so we skip
        // them too; same for batch.
        self.last_run_sql = if matches!(kind, RunKind::Run) && !is_batch {
            Some(sql.clone())
        } else {
            None
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        let wrap_in_tx = decision.wrap_in_tx;
        let is_run = matches!(kind, RunKind::Run);
        self.query_running = true;
        self.query_started = Some(Instant::now());
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
                    detail: err.detail,
                },
            };
            let _ = tx.send(msg);
        });
    }

    // -- grid nav --

    /// Reset the per-grid view state — sort / filter / column cursor
    /// — so a fresh result set starts clean. Called whenever a new
    /// `Grid` lands on the App via `QueryOk` or `Booted`.
    pub(crate) fn reset_grid_view(&mut self) {
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
        self.grid_visible_rows = compute_visible_rows(&self.grid.rows, self.grid_filter.as_deref());
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
                self.last_status = Some(format!("sorted by {} {dir}", self.grid.columns[col]));
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

    /// `I` in Normal — copy the focused row to the clipboard as
    /// `INSERT INTO schema.table (col, …) VALUES (lit, …);`. Refused
    /// when the source table isn't a single-FROM-table SELECT
    /// (joins, no-FROM expressions, CTEs that don't resolve to one
    /// table) since we can't safely name the target.
    /// `F` in Normal — if the focused cell sits in an FK column
    /// of the result's source table, open a new tab with
    /// `SELECT * FROM <parent> WHERE <parent_col> = <value>
    /// LIMIT 100` pre-loaded into the editor. Operator hits F5
    /// to run. Multi-tab keeps the originating result behind for
    /// "go back" (close the new tab).
    fn navigate_fk_from_focused_cell(&mut self) {
        let Some((schema, table)) = self.grid_source.clone() else {
            self.last_error =
                Some("FK navigation needs a single-table SELECT for source inference".into());
            return;
        };
        let Some(col_name) = self.grid.columns.get(self.grid_col_cursor).cloned() else {
            self.last_error = Some("no focused column".into());
            return;
        };
        let Some(idx) = self.selected_grid_row_idx() else {
            return;
        };
        let Some(row) = self.grid.rows.get(idx) else {
            return;
        };
        let Some(value) = row.get(self.grid_col_cursor).cloned() else {
            return;
        };
        let edge = match self
            .schema_cache
            .fk_edge_for_child(&schema, &table, &col_name)
            .cloned()
        {
            Some(e) => e,
            None => {
                self.last_error = Some(format!(
                    "no FK from {schema}.{table}.{col_name} — column isn't a foreign key"
                ));
                return;
            }
        };
        // Build the parent SELECT. Use the same literal-quoting
        // path as row-as-INSERT for safe value rendering.
        let sql = format!(
            "SELECT * FROM {parent_schema}.{parent_table} WHERE {parent_column} = {value_lit} LIMIT 100;",
            parent_schema = quote_ident(&edge.parent_schema),
            parent_table = quote_ident(&edge.parent_table),
            parent_column = quote_ident(&edge.parent_column),
            value_lit = format_sql_literal(&value),
        );
        // Open in a new tab so the operator can close it to "go
        // back" to the originating result. Multi-tab refuses if
        // we're past the cap; surface that and bail.
        if self.tabs.len() >= TAB_CAP {
            self.last_error = Some(format!(
                "max tabs reached ({TAB_CAP}) — close one before navigating"
            ));
            return;
        }
        if self.query_running {
            self.last_error = Some("can't open a new tab while a query is running".into());
            return;
        }
        self.snapshot_active_tab();
        self.tabs.push(TabSnapshot::default());
        self.active_tab = self.tabs.len() - 1;
        self.load_active_tab();
        self.completion = None;
        self.history_pos = None;
        self.editor_buffer = sql;
        self.editor_cursor = self.editor_buffer.len();
        self.mode = Mode::Editor;
        self.last_status = Some(format!(
            "→ {}.{} (F5 to run)",
            edge.parent_schema, edge.parent_table
        ));
    }

    /// Short label for the current grid — the SQL that produced
    /// it (whitespace-collapsed, truncated) or a source/row-count
    /// fallback. Used in the diff header so the operator remembers
    /// which result each side is.
    fn current_result_label(&self) -> String {
        if let Some(sql) = self.last_run_sql.as_deref() {
            let one = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            if !one.is_empty() {
                return crate::grid::truncate_cell(&one, 60);
            }
        }
        match &self.grid_source {
            Some((schema, table)) => format!("{schema}.{table}"),
            None => format!("{} row(s)", self.grid.rows.len()),
        }
    }

    /// `D` in Normal mode. With no baseline pinned, freeze the
    /// current grid as A. With a baseline already pinned, diff the
    /// current grid (B) against it and open `Mode::ResultDiff`.
    fn pin_or_diff_result(&mut self) {
        if self.grid.rows.is_empty() {
            self.last_error = Some("no result to diff — run a query first".into());
            return;
        }
        match self.pinned_result.take() {
            None => {
                let n = self.grid.rows.len();
                self.pinned_result = Some(PinnedResult {
                    columns: self.grid.columns.clone(),
                    rows: self.grid.rows.clone(),
                    label: self.current_result_label(),
                });
                self.last_status = Some(format!(
                    "pinned result A · {n} row(s) · run another query, then D to diff"
                ));
            }
            Some(a) => {
                // Baseline present — current grid is B. Put A back
                // (it persists across diffs for iterative work).
                let b_columns = self.grid.columns.clone();
                let b_rows = self.grid.rows.clone();
                let b_label = self.current_result_label();
                let key = Self::choose_diff_key(&a, &b_columns, &b_rows);
                let diff = crate::query::row_diff::diff_rows(&a.rows, &b_rows, &key);
                let summary = format!(
                    "diff · +{} -{} ~{} ={}",
                    diff.added.len(),
                    diff.removed.len(),
                    diff.changed.len(),
                    diff.unchanged
                );
                self.result_diff = Some(ResultDiffState {
                    a: a.clone(),
                    b_columns,
                    b_rows,
                    b_label,
                    key,
                    diff,
                });
                self.pinned_result = Some(a);
                self.result_diff_cursor = 0;
                self.mode = Mode::ResultDiff;
                self.last_status = Some(summary);
            }
        }
    }

    /// Pick the diff key for A-vs-B. Use a single inferred unique
    /// column (strong mode, with change-detection) only when the
    /// column layouts match — comparing non-key cells positionally
    /// requires aligned columns. Otherwise fall back to full-row
    /// identity.
    fn choose_diff_key(
        a: &PinnedResult,
        b_columns: &[String],
        b_rows: &[Vec<String>],
    ) -> crate::query::row_diff::RowKey {
        use crate::query::row_diff::{infer_key_column, RowKey};
        if a.columns == b_columns {
            if let Some(col) = infer_key_column(&a.rows, b_rows, a.columns.len()) {
                return RowKey::Columns(vec![col]);
            }
        }
        RowKey::FullRow
    }

    fn yank_row_as_insert(&mut self) {
        let Some((schema, table)) = self.grid_source.clone() else {
            self.last_error = Some(
                "can't infer source table — row-as-INSERT only works for single-table SELECTs"
                    .into(),
            );
            return;
        };
        let Some(idx) = self.selected_grid_row_idx() else {
            return;
        };
        let Some(row) = self.grid.rows.get(idx) else {
            return;
        };
        let cols = self
            .grid
            .columns
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let vals = row
            .iter()
            .map(|s| format_sql_literal(s))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("INSERT INTO {schema}.{table} ({cols}) VALUES ({vals});");
        match arboard::Clipboard::new() {
            Ok(mut cb) => match cb.set_text(sql.clone()) {
                Ok(()) => {
                    self.last_status = Some(format!(
                        "copied INSERT for {schema}.{table} · {} char(s)",
                        sql.len()
                    ));
                }
                Err(e) => self.last_error = Some(format!("clipboard write: {e}")),
            },
            Err(e) => self.last_error = Some(format!("clipboard init: {e}")),
        }
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
            truncated: false,
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
            self.last_status = Some("no active filter (press `/` to start one)".into());
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

    /// Re-flatten the schema browser against the live cache +
    /// expand state. The renderer and the key handler both consult
    /// this; pulling it onto App keeps the two in sync.
    pub fn flattened_schema_browser(&self) -> Vec<SchemaBrowserRow> {
        // When a filter is active, force-expand every schema and
        // table so matches deep in the tree are visible without the
        // operator having to manually expand each ancestor. Then
        // run the row-level filter.
        let filter = self.schema_browser_filter.as_deref().unwrap_or("");
        let expanded_owned: std::collections::HashSet<String>;
        let expanded_ref: &std::collections::HashSet<String> = if filter.is_empty() {
            &self.schema_browser_expanded
        } else {
            let mut s = self.schema_browser_expanded.clone();
            for t in &self.schema_cache.tables {
                s.insert(t.schema.clone());
                s.insert(schema_browser_table_key(&t.schema, &t.name));
            }
            expanded_owned = s;
            &expanded_owned
        };
        let rows = flatten_schema_browser(&self.schema_cache, expanded_ref);
        if filter.is_empty() {
            rows
        } else {
            filter_schema_browser_rows(rows, filter)
        }
    }

    /// `S` from Normal — opens the schema browser. No-op when the
    /// cache is empty (disconnected / permission failure on the
    /// `pg_catalog` fetch).
    fn start_schema_browser(&mut self) {
        if self.schema_cache.is_empty() {
            self.last_status = Some("schema cache empty — connect to a database first".into());
            return;
        }
        self.schema_browser_cursor = 0;
        self.mode = Mode::SchemaBrowser;
    }

    /// `W` from Normal — open the schema "wizard" (lint).
    /// Re-runs every check on entry so a fresh re-connect picks
    /// up the latest cache. Pure; sub-millisecond on a normal
    /// catalog so there's no async hop.
    fn start_schema_lint(&mut self) {
        if self.schema_cache.is_empty() {
            self.last_status = Some("schema cache empty — connect to a database first".into());
            return;
        }
        let findings = crate::query::lint::run_all(&self.schema_cache);
        let n = findings.len();
        let high = findings
            .iter()
            .filter(|f| f.severity == crate::query::lint::Severity::High)
            .count();
        self.last_status = Some(if n == 0 {
            "schema lint · checking live (FK indexes)…".into()
        } else {
            format!("schema lint · {n} finding(s) · {high} high · checking live…")
        });
        self.schema_lint_findings = findings;
        self.schema_lint_cursor = 0;
        self.mode = Mode::SchemaLint;
        // Kick off the live-query checks (LINT101+). Results land
        // via `AppMsg::LiveLintLoaded` and get merged into
        // `schema_lint_findings` — see the handler.
        if let Some(client) = self.client.clone() {
            let tx = self.msg_tx.clone();
            let generation = self.generation;
            tokio::spawn(async move {
                let result = crate::query::lint::fetch_live(&client).await;
                let _ = tx.send(AppMsg::LiveLintLoaded { generation, result });
            });
        }
    }

    /// Yank the focused finding's `suggestion` (an SQL snippet)
    /// to the clipboard so the operator can paste it into the
    /// editor. Surfaces an actionable status when the finding has
    /// no suggestion (LINT002 / LINT003 / LINT004 are advisory).
    fn yank_schema_lint_suggestion(&mut self) {
        let Some(finding) = self.schema_lint_findings.get(self.schema_lint_cursor) else {
            return;
        };
        let Some(snippet) = finding.suggestion.clone() else {
            self.last_status = Some(format!(
                "{}: no SQL suggestion — advisory finding",
                finding.code
            ));
            return;
        };
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(snippet.clone())) {
            Ok(()) => {
                self.last_status = Some(format!("yanked {} suggestion", finding.code));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }

    fn start_schema_browser_filter(&mut self) {
        self.schema_browser_filter = Some(String::new());
        self.schema_browser_cursor = 0;
        self.last_status = Some("filter: /  · type to narrow · enter accept · esc clear".into());
        self.mode = Mode::SchemaBrowserFilter;
    }

    fn refresh_schema_browser_filter_status(&mut self) {
        let pat = self.schema_browser_filter.as_deref().unwrap_or("");
        let n = self.flattened_schema_browser().len();
        self.last_status = Some(format!(
            "filter: /{pat}  · {n} row(s) · enter accept · esc clear"
        ));
    }

    /// Resolve the focused schema-browser row to its owning
    /// (schema, table) — returns None when focused on a Schema row
    /// (no table context) or when the cursor is out-of-bounds.
    /// Column / Constraint rows resolve to their parent table.
    fn focused_schema_browser_table(&self) -> Option<(String, String)> {
        let rows = self.flattened_schema_browser();
        match rows.get(self.schema_browser_cursor)? {
            SchemaBrowserRow::Table { schema, name, .. } => Some((schema.clone(), name.clone())),
            SchemaBrowserRow::Column { schema, table, .. } => Some((schema.clone(), table.clone())),
            SchemaBrowserRow::Constraint { schema, table, .. } => {
                Some((schema.clone(), table.clone()))
            }
            SchemaBrowserRow::Schema { .. } => None,
        }
    }

    fn yank_schema_browser_select(&mut self) {
        let Some((schema, table)) = self.focused_schema_browser_table() else {
            self.last_error = Some("focus a table, column, or constraint first".into());
            return;
        };
        let sql = build_select_all_template(&schema, &table);
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(sql.clone())) {
            Ok(()) => {
                self.last_status = Some(format!("yanked SELECT template for {schema}.{table}"));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }

    fn yank_schema_browser_insert(&mut self) {
        let Some((schema, table)) = self.focused_schema_browser_table() else {
            self.last_error = Some("focus a table, column, or constraint first".into());
            return;
        };
        let cols = self
            .schema_cache
            .columns_by_table
            .get(&(schema.clone(), table.clone()))
            .cloned()
            .unwrap_or_default();
        if cols.is_empty() {
            self.last_error = Some(format!("no column info cached for {schema}.{table}"));
            return;
        }
        let sql = build_insert_template(&schema, &table, &cols);
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(sql.clone())) {
            Ok(()) => {
                self.last_status = Some(format!(
                    "yanked INSERT template for {schema}.{table} · {} col(s)",
                    cols.len()
                ));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
        }
    }

    /// `T` from Normal — load + open the slow-queries panel.
    fn start_slow_queries(&mut self) {
        let Some(client) = self.client.clone() else {
            self.last_error = Some("not connected".into());
            return;
        };
        self.slow_queries_cursor = 0;
        self.mode = Mode::SlowQueries;
        self.last_status = Some("loading pg_stat_statements…".into());
        self.spawn_slow_queries_load(client);
    }

    /// `r` inside the slow-queries panel — refresh in place.
    fn refresh_slow_queries(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.last_status = Some("refreshing pg_stat_statements…".into());
        self.spawn_slow_queries_load(client);
    }

    fn spawn_slow_queries_load(&self, client: std::sync::Arc<tokio_postgres::Client>) {
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        tokio::spawn(async move {
            let result = match conn::run_query(&client, crate::query::slow_queries::PANEL_SQL).await
            {
                Ok(grid) => Ok(crate::query::slow_queries::parse(&grid)),
                Err(e) => Err(e),
            };
            let _ = tx.send(AppMsg::SlowQueriesLoaded { generation, result });
        });
    }

    /// `L` from Normal — load + open the active-sessions panel.
    fn start_sessions(&mut self) {
        let Some(client) = self.client.clone() else {
            self.last_error = Some("not connected".into());
            return;
        };
        self.sessions_cursor = 0;
        self.mode = Mode::Sessions;
        self.last_status = Some("loading pg_stat_activity…".into());
        self.spawn_sessions_load(client);
    }

    fn refresh_sessions(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.last_status = Some("refreshing pg_stat_activity…".into());
        self.spawn_sessions_load(client);
    }

    fn spawn_sessions_load(&self, client: std::sync::Arc<tokio_postgres::Client>) {
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        tokio::spawn(async move {
            let result = match conn::run_query(&client, crate::query::sessions::PANEL_SQL).await {
                Ok(grid) => Ok(crate::query::sessions::parse(&grid)),
                Err(e) => Err(e),
            };
            let _ = tx.send(AppMsg::SessionsLoaded { generation, result });
        });
    }

    /// Capital-K in the Sessions panel — open a confirmation
    /// modal for `pg_terminate_backend(<pid>)`. On confirm the
    /// spawn fires; the panel auto-refreshes when the result
    /// lands.
    fn start_terminate_focused_session(&mut self) {
        let Some(row) = self.sessions.get(self.sessions_cursor) else {
            return;
        };
        let pid = row.pid;
        let query_preview: String = row
            .query
            .chars()
            .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
            .take(60)
            .collect();
        self.pending_terminate = Some(pid);
        self.last_status = Some(format!(
            "terminate pid {pid}? \"{query_preview}\" · y confirm · n cancel"
        ));
        // Reuse the Confirm modal but with a custom message —
        // since `pending_run` is None, the modal's "Confirm" key
        // arm would no-op. Use a new `Mode::ConfirmTerminate`
        // with its own handler instead.
        self.mode = Mode::ConfirmTerminate;
    }

    /// Move the grid selection + column cursor to a bookmarked
    /// position. Clamps to the current visible-row count so a
    /// jump after the grid has shrunk doesn't land out-of-range.
    fn jump_to_bookmark(&mut self, bm: GridBookmark) {
        if self.grid.row_count() == 0 {
            return;
        }
        // The bookmark's `row` was the underlying grid index at
        // the time of set. Find its position in the current
        // visible-rows list (filter may have moved / dropped it).
        let target_visible = self.grid_visible_rows.iter().position(|&i| i == bm.row);
        let visible_idx = match target_visible {
            Some(i) => i,
            None => {
                self.last_status = Some("bookmark row not visible in current filter".into());
                return;
            }
        };
        self.grid_state.select(Some(visible_idx));
        let last_col = self.grid.columns.len().saturating_sub(1);
        self.grid_col_cursor = bm.col.min(last_col);
    }

    /// `f` from Normal — open the grid-find input. Re-uses the
    /// existing visible_rows / grid_cursor; matches are computed
    /// across them, NOT across hidden filtered-out rows.
    fn start_find(&mut self) {
        if self.grid.rows.is_empty() {
            self.last_status = Some("nothing to find · grid is empty".into());
            return;
        }
        self.grid_find = Some(String::new());
        self.grid_find_matches.clear();
        self.grid_find_pos = 0;
        self.last_status =
            Some("find:    · type to search · n/N jump · enter accept · esc cancel".into());
        self.mode = Mode::GridFind;
    }

    fn refresh_grid_find_status(&mut self) {
        let pat = self.grid_find.as_deref().unwrap_or("");
        let n = self.grid_find_matches.len();
        if pat.is_empty() {
            self.last_status = Some("find:    · type to search · enter accept · esc cancel".into());
            return;
        }
        let pos = if n == 0 { 0 } else { self.grid_find_pos + 1 };
        self.last_status = Some(format!(
            "find: {pat}  · {pos}/{n} match · n/N jump · enter accept · esc cancel"
        ));
    }

    /// Recompute the match list and jump the cursor to the first
    /// match. Called on every keystroke while the find input is
    /// live.
    fn rebuild_grid_find(&mut self) {
        let pat = self.grid_find.clone().unwrap_or_default();
        self.grid_find_matches =
            compute_grid_find_matches(&self.grid, &self.grid_visible_rows, &pat);
        self.grid_find_pos = 0;
        if let Some(&(vi, ci)) = self.grid_find_matches.first() {
            self.grid_state.select(Some(vi));
            self.grid_col_cursor = ci;
        }
    }

    /// Step to the next / previous match (wrapping). No-op when
    /// no matches.
    fn step_grid_find(&mut self, forward: bool) {
        if self.grid_find_matches.is_empty() {
            return;
        }
        let n = self.grid_find_matches.len();
        self.grid_find_pos = if forward {
            (self.grid_find_pos + 1) % n
        } else {
            (self.grid_find_pos + n - 1) % n
        };
        let (vi, ci) = self.grid_find_matches[self.grid_find_pos];
        self.grid_state.select(Some(vi));
        self.grid_col_cursor = ci;
        self.refresh_grid_find_status();
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
/// One step in the editor's undo / redo ring. Captures the buffer
/// + cursor state BEFORE a mutation, plus a `kind` tag so
/// consecutive char-inserts can be coalesced (otherwise typing
/// `qwerty` would be six undos).
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

/// Cap on the undo + redo rings. Roughly 100 reverse-points is a
/// generous amount of headroom without unbounded memory growth on
/// big buffers (each entry clones the whole string).
pub const UNDO_CAP: usize = 100;

/// Window during which consecutive char-inserts coalesce into one
/// undo step. Mirrors the "typing run" most editors recognise —
/// pause for 500 ms and the next character starts a fresh undo
/// step.
pub const UNDO_COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// Pure: decide whether a new char-insert mutation should
/// coalesce into the top-of-undo-stack entry. The two `Instant`s
/// are passed in so tests can drive synthetic timelines.
pub fn should_coalesce_undo(
    last_kind: EditorActionKind,
    last_window_end: std::time::Instant,
    new_kind: EditorActionKind,
    now: std::time::Instant,
    window: std::time::Duration,
) -> bool {
    last_kind == EditorActionKind::CharInsert
        && new_kind == EditorActionKind::CharInsert
        && now.saturating_duration_since(last_window_end) < window
}

fn editor_insert(buffer: &mut String, cursor: &mut usize, c: char) {
    buffer.insert(*cursor, c);
    *cursor += c.len_utf8();
}

/// Bracket autoclose: insert the matching close-char after `c`
/// and leave the cursor between them. Pure / testable. Returns
/// `true` when the pair was inserted (so the caller knows the
/// edit happened); `false` for chars that aren't openers.
pub fn editor_insert_pair(buffer: &mut String, cursor: &mut usize, c: char) -> bool {
    let close = match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => return false,
    };
    buffer.insert(*cursor, c);
    buffer.insert(*cursor + 1, close);
    // Cursor sits BETWEEN the pair: just past the opener.
    *cursor += 1;
    true
}

/// Skip-over for close-brackets: if the character immediately
/// after the cursor matches `c`, advance the cursor past it
/// instead of inserting a literal. Mirrors what most editors do
/// for `()` autoclose — typing `(` then `)` yields `()` with the
/// cursor between, then the second `)` just exits the pair.
/// Returns `true` when the skip happened (no insert needed).
pub fn editor_maybe_skip_close(buffer: &str, cursor: &mut usize, c: char) -> bool {
    if !matches!(c, ')' | ']' | '}') {
        return false;
    }
    let bytes = buffer.as_bytes();
    if *cursor < bytes.len() && bytes[*cursor] == c as u8 {
        *cursor += 1;
        return true;
    }
    false
}

/// Quote autoclose: when `c` is `'` or `"`, decide whether to
/// insert a paired quote (cursor between) versus a single literal
/// character. The gate is conservative: only pair when both
/// neighbours look like quote-boundaries (whitespace, EOB, or
/// punctuation that isn't `_`). That keeps the feature out of
/// SQL string-literal escaping (`'don''t'`) and out of mid-word
/// contractions in comments (`it's`), where a paired quote
/// would just be in the way.
///
/// Returns `true` when the pair was inserted; `false` lets the
/// caller fall back to inserting the literal character.
pub fn editor_maybe_pair_quote(buffer: &mut String, cursor: &mut usize, c: char) -> bool {
    if !matches!(c, '\'' | '"') {
        return false;
    }
    let prev_ok = match char_before(buffer, *cursor) {
        None => true,
        Some(p) => !p.is_alphanumeric() && p != '_',
    };
    let next_ok = match char_after(buffer, *cursor) {
        None => true,
        Some(n) => !n.is_alphanumeric() && n != '_' && n != c,
    };
    if !(prev_ok && next_ok) {
        return false;
    }
    buffer.insert(*cursor, c);
    buffer.insert(*cursor + 1, c);
    *cursor += 1;
    true
}

/// Skip-over for quotes: same idea as `editor_maybe_skip_close`
/// but for `'` / `"`. Advances past a matching next-char quote
/// **only** when the previous char is also a quote-boundary
/// (EOB / whitespace / non-word punctuation). The prev gate is
/// what keeps SQL `''` escaping intact — inside a string literal
/// (`'don|'`) the prev char is alphanumeric, so we fall through
/// to a literal insert and let the operator build `'don''t'`.
pub fn editor_maybe_skip_quote(buffer: &str, cursor: &mut usize, c: char) -> bool {
    if !matches!(c, '\'' | '"') {
        return false;
    }
    let bytes = buffer.as_bytes();
    if !(*cursor < bytes.len() && bytes[*cursor] == c as u8) {
        return false;
    }
    let prev_ok = match char_before(buffer, *cursor) {
        None => true,
        Some(p) => !p.is_alphanumeric() && p != '_',
    };
    if !prev_ok {
        return false;
    }
    *cursor += 1;
    true
}

fn char_before(buffer: &str, cursor: usize) -> Option<char> {
    if cursor == 0 {
        return None;
    }
    let mut i = cursor - 1;
    while !buffer.is_char_boundary(i) {
        i -= 1;
    }
    buffer[i..cursor].chars().next()
}

fn char_after(buffer: &str, cursor: usize) -> Option<char> {
    if cursor >= buffer.len() {
        return None;
    }
    buffer[cursor..].chars().next()
}

/// Toggle a `-- ` line-comment at the start of the line
/// containing `cursor`. Pure: works on the (buffer, cursor)
/// pair the editor already has. The cursor is preserved
/// relative to its original line content (i.e., if removing
/// `-- ` shifts text left by 3 cols, the cursor shifts too).
pub fn editor_toggle_line_comment(buffer: &mut String, cursor: &mut usize) {
    let line_start = line_start_byte(buffer, *cursor);
    // Inspect the leading characters of the line.
    let rest = &buffer[line_start..];
    if let Some(stripped) = rest.strip_prefix("-- ") {
        // Drop 3 chars.
        let drop = 3;
        let _ = stripped; // unused — using `drop` length only.
        buffer.replace_range(line_start..line_start + drop, "");
        if *cursor >= line_start + drop {
            *cursor -= drop;
        } else if *cursor > line_start {
            // Cursor was inside the `-- ` prefix — clamp to start.
            *cursor = line_start;
        }
    } else if rest.starts_with("--") {
        // No trailing space — drop 2.
        let drop = 2;
        buffer.replace_range(line_start..line_start + drop, "");
        if *cursor >= line_start + drop {
            *cursor -= drop;
        } else if *cursor > line_start {
            *cursor = line_start;
        }
    } else {
        // Comment in — insert `-- ` at line start.
        buffer.insert_str(line_start, "-- ");
        if *cursor >= line_start {
            *cursor += 3;
        }
    }
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
    let formatted =
        String::from_utf8(output.stdout).map_err(|e| format!("{binary} produced non-UTF8: {e}"))?;
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
    let path = std::env::temp_dir().join(format!("pgman-edit-{}-{}.sql", std::process::id(), seq));
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
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("could not read {prog} output: {e}"))?;
    let _ = std::fs::remove_file(&path);
    Ok(text.trim_end_matches('\n').to_string())
}

/// Path to the auto-saved editor draft. Lives under
/// `util::data_dir()` (persistent across upgrades), separate from
/// the log cache.
fn draft_path() -> std::path::PathBuf {
    crate::util::data_dir().join("draft.sql")
}

/// Path to the persisted query-history file. Bash-style one entry
/// per line. Multi-line SQL is escaped to `\n` so each entry stays
/// on one line.
fn history_path() -> std::path::PathBuf {
    crate::util::data_dir().join("history.log")
}

/// Path to the persisted saved-queries file.
pub fn saved_queries_path() -> std::path::PathBuf {
    crate::util::data_dir().join("saved.toml")
}

/// Default file path for `\report` when the operator doesn't
/// pass one. Lives under the cache dir with a sortable
/// timestamp prefix so successive reports don't overwrite
/// each other.
///
/// Includes the process id + nanosecond fraction in the
/// filename so two `\report` invocations within the same
/// second (or by two pgman instances at once) don't clobber
/// each other.
pub fn default_report_path() -> std::path::PathBuf {
    cache_stamped_path("report", "md")
}

/// `<cache>/<stem>-<secs>-<nanos>-<pid>.<ext>` — a wall-clock + pid
/// stamped path under the cache dir. The pid + nanosecond fraction
/// keep two writes within the same second (or from two pgman
/// instances at once) from clobbering each other.
fn cache_stamped_path(stem: &str, ext: &str) -> std::path::PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let nanos = now.subsec_nanos();
    let pid = std::process::id();
    crate::util::cache_dir().join(format!("{stem}-{secs}-{nanos:09}-{pid}.{ext}"))
}

/// Default path for `\fixture` when the operator gives none:
/// `<cache>/<table>-fixture-<secs>-<nanos>-<pid>.xml`. The table
/// name is sanitised so a schema-qualified or odd identifier
/// can't escape the cache dir.
pub fn default_fixture_path(table: &str) -> std::path::PathBuf {
    let safe: String = table
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_stamped_path(&format!("{safe}-fixture"), "xml")
}

/// Minimal "current UTC moment" formatted as
/// `YYYY-MM-DDTHH:MM:SSZ`. No external chrono dep — we
/// only need this for the report header, and absolute
/// precision isn't required. Pure-ish: uses `SystemTime`
/// internally but never panics.
pub fn chrono_like_now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix_secs_utc(secs)
}

/// Pure: format a unix timestamp as `YYYY-MM-DDTHH:MM:SSZ`.
/// Civil-calendar conversion via the standard "days since
/// 1970-01-01 → year/month/day" algorithm; no leap-second
/// handling.
pub fn format_unix_secs_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let secs_of_day = (secs % 86_400) as u32;
    let hours = secs_of_day / 3600;
    let mins = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{mins:02}:{s:02}Z")
}

/// Howard Hinnant's "civil from days" algorithm — converts a
/// signed day count (days since 1970-01-01) to (year, month,
/// day). Pure / proptest-friendly.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

/// Cap on the in-memory history ring AND on what we persist to
/// disk. Matches the existing `push to history (cap 50)` guard.
pub const HISTORY_CAP: usize = 50;

/// Polling interval for the SlowQueries / Sessions auto-refresh
/// when toggled on with `R`. 5 s matches the rule of thumb that
/// pg_stat_activity / pg_stat_statements stats stay stable
/// enough at that cadence to not whiplash the UI, while still
/// being live-enough for "find the blocker" workflows.
pub const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Cap on the in-memory notification ring. Operator can see the
/// most recent 200 NOTIFY arrivals; older ones drop off the
/// front. 200 covers a few minutes of a moderately chatty
/// channel without growing memory unboundedly.
pub const NOTIFICATION_CAP: usize = 200;

/// Cap on the in-memory tap ring. Higher than the notification
/// cap because tap events arrive at JDBC-statement cadence, not
/// LISTEN/NOTIFY cadence — a busy app fires hundreds per second
/// and we want a useful window without ballooning the process.
/// 2000 events ≈ a few seconds at 1000 QPS or several minutes
/// at typical interactive rates.
pub const TAP_CAP: usize = 2000;

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

/// Encode a multi-line SQL entry to a single line: `\\` → `\\\\`,
/// `\n` → `\\n`. Operation is reversible via [`decode_history_line`].
fn encode_history_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Reverse of [`encode_history_line`]. Tolerant of trailing
/// backslashes — `\\` at end of string keeps the literal `\`.
fn decode_history_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            // Unknown escape: emit literally so we don't lose bytes.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Best-effort load of the persisted query history. Returns the
/// last `HISTORY_CAP` non-empty entries. Missing / unreadable file
/// yields an empty vec — the operator never sees a startup error.
pub fn load_history() -> Vec<String> {
    load_history_from(&history_path())
}

/// Path-parameterised core of [`load_history`].
pub fn load_history_from(path: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(decode_history_line)
        .collect();
    if out.len() > HISTORY_CAP {
        let drop = out.len() - HISTORY_CAP;
        out.drain(..drop);
    }
    out
}

/// Persist the history ring atomically. Each entry on one line,
/// multi-line SQL escaped. Empty input still rewrites the file (as
/// empty) so deliberate clearing survives.
pub(crate) fn persist_history(entries: &[String]) -> std::io::Result<()> {
    persist_history_to(&history_path(), entries)
}

/// Path-parameterised core of [`persist_history`].
///
/// Keep the LAST `HISTORY_CAP` entries (newest), symmetric with
/// `load_history_from` which drops from the head. Today the
/// in-memory ring is already bounded by [`HISTORY_CAP`]; this
/// extra clamp makes the persistence side robust if the caps ever
/// drift.
pub fn persist_history_to(path: &std::path::Path, entries: &[String]) -> std::io::Result<()> {
    let start = entries.len().saturating_sub(HISTORY_CAP);
    let mut text = String::new();
    for e in &entries[start..] {
        text.push_str(&encode_history_line(e));
        text.push('\n');
    }
    tui_common::util::write_atomic(path, &text)
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

/// Write the buffer atomically (via `tui_common::util::write_atomic`)
/// on quit. Empty buffers still get written so a deliberate
/// Ctrl-U + quit clears the saved draft.
pub(crate) fn persist_draft(buf: &str) -> std::io::Result<()> {
    persist_draft_to(&draft_path(), buf)
}

/// Path-parameterised core of [`persist_draft`]. Same atomic-rename
/// guarantee — a crash mid-write leaves either the old file intact
/// or the new file complete, never a truncated half-write.
pub fn persist_draft_to(path: &std::path::Path, buf: &str) -> std::io::Result<()> {
    tui_common::util::write_atomic(path, buf)
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
            conn::run_statement(client, &wrapped)
                .await
                .map_err(|mut e| {
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
    fn should_coalesce_undo_merges_consecutive_char_inserts_inside_window() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(50);
        let window = Duration::from_millis(500);
        assert!(should_coalesce_undo(
            EditorActionKind::CharInsert,
            t0,
            EditorActionKind::CharInsert,
            t1,
            window,
        ));
    }

    #[test]
    fn should_coalesce_undo_refuses_after_window_expires() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(600);
        let window = Duration::from_millis(500);
        assert!(!should_coalesce_undo(
            EditorActionKind::CharInsert,
            t0,
            EditorActionKind::CharInsert,
            t1,
            window,
        ));
    }

    #[test]
    fn should_coalesce_undo_refuses_non_charinsert_neighbours() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let window = Duration::from_millis(500);
        assert!(!should_coalesce_undo(
            EditorActionKind::Other,
            t0,
            EditorActionKind::CharInsert,
            t0,
            window,
        ));
        assert!(!should_coalesce_undo(
            EditorActionKind::CharInsert,
            t0,
            EditorActionKind::Other,
            t0,
            window,
        ));
    }

    #[test]
    fn f1_from_editor_opens_help() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "select 1".into();
        a.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Help);
        assert_eq!(a.help_origin, Some(Mode::Editor));
    }

    #[test]
    fn f1_from_help_closes_back_to_origin_mode() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Help);
        a.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        // Restored to the source mode, not Normal.
        assert_eq!(a.mode, Mode::Editor);
        assert!(a.help_origin.is_none());
    }

    #[test]
    fn help_anchor_for_known_modes_picks_their_section() {
        assert_eq!(
            App::help_anchor_for(Mode::SchemaBrowser),
            Some("schema browser")
        );
        assert_eq!(
            App::help_anchor_for(Mode::ExplainTree),
            Some("EXPLAIN tree")
        );
        assert_eq!(App::help_anchor_for(Mode::Editor), Some("editor"));
        assert_eq!(App::help_anchor_for(Mode::LogPick), Some("log pick"));
        assert_eq!(App::help_anchor_for(Mode::Help), None);
    }

    #[test]
    fn mode_entry_hint_fires_only_first_time_per_session() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // Synthetic transition: schema cache empty, but
        // note_mode_entry is mode-aware and doesn't care about
        // contents. We call it directly to bypass the schema-empty
        // guard on start_schema_browser.
        a.note_mode_entry(Mode::SchemaBrowser);
        let first = a.last_status.clone();
        assert!(
            first
                .as_deref()
                .map(|s| s.starts_with("tip"))
                .unwrap_or(false),
            "first entry should set a tip; got {first:?}"
        );
        // Second entry: hint suppressed (status stays at whatever
        // the caller left it as). We mimic that by clearing the
        // status and re-entering.
        a.last_status = None;
        a.note_mode_entry(Mode::SchemaBrowser);
        assert!(
            a.last_status.is_none(),
            "second entry should NOT re-fire the hint; got {:?}",
            a.last_status
        );
    }

    #[test]
    fn ctrl_enter_in_editor_attempts_to_run_query() {
        // Reproduces: "ctrl-enter doesn't execute" after the undo
        // wrapper landed. With no client connected, `request_run`
        // surfaces "not connected" — we use that signal to confirm
        // the run path was reached.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "select 1".into();
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        // request_run rejects with "not connected" — that's the
        // intended signal here. If we still see "editor is empty"
        // or nothing happened, the run path wasn't reached.
        let err = a.last_error.as_deref().unwrap_or("");
        assert!(
            err.contains("not connected"),
            "Ctrl-Enter should hit request_run; last_error = {err:?}"
        );
    }

    #[test]
    fn ctrl_j_in_editor_attempts_to_run_query() {
        // Some terminals report Ctrl-Enter as Ctrl-J.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "select 1".into();
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        let err = a.last_error.as_deref().unwrap_or("");
        assert!(
            err.contains("not connected"),
            "Ctrl-J should hit request_run; last_error = {err:?}"
        );
    }

    #[test]
    fn editor_undo_restores_pre_typing_buffer() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "select 1".into();
        a.editor_cursor = a.editor_buffer.len();
        // Type a char — pushes the prior state to undo.
        a.on_key(KeyEvent::from(KeyCode::Char(';')));
        assert_eq!(a.editor_buffer, "select 1;");
        // Undo: buffer returns to its pre-typing value.
        a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(a.editor_buffer, "select 1");
        // Redo: forward to the typed state.
        a.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(a.editor_buffer, "select 1;");
    }

    #[test]
    fn editor_undo_when_empty_surfaces_status_not_crash() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(a.last_status.as_deref(), Some("nothing to undo"));
    }

    #[test]
    fn editor_redo_invalidated_by_a_new_edit() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "a".into();
        a.editor_cursor = 1;
        a.on_key(KeyEvent::from(KeyCode::Char('b'))); // buf = "ab"
        a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)); // undo → "a"
        assert!(!a.editor_redo.is_empty(), "undo should populate redo");
        a.on_key(KeyEvent::from(KeyCode::Char('c'))); // new edit invalidates redo
        assert!(a.editor_redo.is_empty(), "new mutation must clear redo");
        assert_eq!(a.editor_buffer, "ac");
    }

    #[test]
    fn editor_consecutive_char_inserts_coalesce_into_one_undo_step() {
        // Type `xyz` in quick succession (synthetic — the test runs
        // well inside UNDO_COALESCE_WINDOW). One undo should drop
        // ALL THREE characters at once.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::from(KeyCode::Char('x')));
        a.on_key(KeyEvent::from(KeyCode::Char('y')));
        a.on_key(KeyEvent::from(KeyCode::Char('z')));
        assert_eq!(a.editor_buffer, "xyz");
        // One undo unwinds the whole typing run.
        a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(a.editor_buffer, "");
    }

    #[test]
    fn editor_backspace_does_not_coalesce_with_char_inserts() {
        // Typing then backspacing should be two distinct undo
        // steps. Otherwise an undo after a backspace would also
        // unwind the preceding char-insert run, which is wrong.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::from(KeyCode::Char('a')));
        a.on_key(KeyEvent::from(KeyCode::Char('b'))); // buf = "ab"
        a.on_key(KeyEvent::from(KeyCode::Backspace)); // buf = "a"
        assert_eq!(a.editor_buffer, "a");
        // First undo restores the pre-backspace state ("ab").
        a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(a.editor_buffer, "ab");
        // Second undo unwinds the typing run.
        a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(a.editor_buffer, "");
    }

    #[test]
    fn editor_undo_caps_at_undo_cap_entries() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        // Each Backspace is a non-coalescing edit. Drive past the cap.
        for i in 0..(UNDO_CAP + 20) {
            a.editor_buffer = format!("buf{i}");
            a.editor_cursor = a.editor_buffer.len();
            a.push_undo("prev".to_string(), 0, EditorActionKind::Other);
        }
        assert!(
            a.editor_undo.len() <= UNDO_CAP,
            "undo ring grew past cap: {}",
            a.editor_undo.len()
        );
    }

    #[test]
    fn editor_insert_pair_places_cursor_between_brackets() {
        let mut buf = String::new();
        let mut cur = 0;
        assert!(editor_insert_pair(&mut buf, &mut cur, '('));
        assert_eq!(buf, "()");
        assert_eq!(cur, 1);
        // Squares + braces work the same.
        let mut buf = String::from("a");
        let mut cur = 1;
        assert!(editor_insert_pair(&mut buf, &mut cur, '['));
        assert_eq!(buf, "a[]");
        assert_eq!(cur, 2);
        let mut buf = String::new();
        let mut cur = 0;
        assert!(editor_insert_pair(&mut buf, &mut cur, '{'));
        assert_eq!(buf, "{}");
        assert_eq!(cur, 1);
    }

    #[test]
    fn editor_insert_pair_refuses_non_opener_chars() {
        let mut buf = String::new();
        let mut cur = 0;
        assert!(!editor_insert_pair(&mut buf, &mut cur, 'x'));
        assert_eq!(buf, "");
        assert_eq!(cur, 0);
    }

    #[test]
    fn editor_maybe_skip_close_advances_over_matching_char() {
        // Buffer is `()`, cursor between → typing `)` advances past.
        let buf = String::from("()");
        let mut cur = 1;
        assert!(editor_maybe_skip_close(&buf, &mut cur, ')'));
        assert_eq!(cur, 2);
    }

    #[test]
    fn editor_maybe_skip_close_passes_through_when_no_match() {
        // `(x` with cursor at end — typing `)` should NOT skip
        // (and the caller falls back to a literal insert).
        let buf = String::from("(x");
        let mut cur = 2;
        assert!(!editor_maybe_skip_close(&buf, &mut cur, ')'));
        assert_eq!(cur, 2);
    }

    #[test]
    fn editor_maybe_pair_quote_pairs_single_quote_at_token_boundary() {
        // Empty buffer — both neighbours are EOB, prev/next ok.
        let mut buf = String::new();
        let mut cur = 0;
        assert!(editor_maybe_pair_quote(&mut buf, &mut cur, '\''));
        assert_eq!(buf, "''");
        assert_eq!(cur, 1);
    }

    #[test]
    fn editor_maybe_pair_quote_pairs_double_quote_after_whitespace() {
        // `SELECT ` with cursor at end — prev is space, next is EOB.
        let mut buf = String::from("SELECT ");
        let mut cur = buf.len();
        assert!(editor_maybe_pair_quote(&mut buf, &mut cur, '"'));
        assert_eq!(buf, "SELECT \"\"");
        assert_eq!(cur, 8); // between the two quotes
    }

    #[test]
    fn editor_maybe_pair_quote_refuses_inside_word() {
        // `it` cursor at end — prev is alpha. Don't pair.
        let mut buf = String::from("it");
        let mut cur = buf.len();
        assert!(!editor_maybe_pair_quote(&mut buf, &mut cur, '\''));
        // Buffer untouched so caller can fall back to literal insert.
        assert_eq!(buf, "it");
        assert_eq!(cur, 2);
    }

    #[test]
    fn editor_maybe_pair_quote_refuses_when_next_is_word() {
        // `abc` cursor at start — next is alpha. Don't pair.
        let mut buf = String::from("abc");
        let mut cur = 0;
        assert!(!editor_maybe_pair_quote(&mut buf, &mut cur, '\''));
        assert_eq!(buf, "abc");
        assert_eq!(cur, 0);
    }

    #[test]
    fn editor_maybe_pair_quote_refuses_when_next_is_same_quote() {
        // `'` cursor between — typing `'` again should NOT pair
        // (the skip-quote branch handles this, but pair_quote
        // alone must also refuse so the caller's fallback order
        // is correct).
        let mut buf = String::from("''");
        let mut cur = 1;
        assert!(!editor_maybe_pair_quote(&mut buf, &mut cur, '\''));
        assert_eq!(buf, "''");
        assert_eq!(cur, 1);
    }

    #[test]
    fn editor_maybe_pair_quote_refuses_non_quote_chars() {
        let mut buf = String::new();
        let mut cur = 0;
        assert!(!editor_maybe_pair_quote(&mut buf, &mut cur, 'x'));
        assert_eq!(buf, "");
    }

    #[test]
    fn editor_maybe_skip_quote_advances_over_matching_quote() {
        // Buffer `''` with cursor between — typing `'` advances past.
        let buf = String::from("''");
        let mut cur = 1;
        assert!(editor_maybe_skip_quote(&buf, &mut cur, '\''));
        assert_eq!(cur, 2);
    }

    #[test]
    fn editor_maybe_skip_quote_passes_through_when_no_match() {
        // `'x` with cursor between — typing `'` should NOT skip.
        let buf = String::from("'x");
        let mut cur = 1;
        assert!(!editor_maybe_skip_quote(&buf, &mut cur, '\''));
        assert_eq!(cur, 1);
    }

    #[test]
    fn editor_maybe_skip_quote_does_not_skip_when_prev_is_word_char() {
        // Buffer `'don'` with cursor at 4 (between `n` and `'`).
        // Operator is mid-literal trying to escape — refusing to
        // skip lets pair_quote's prev-gate also refuse, so the
        // typing path falls through to a literal `'` insert and
        // builds `'don''` toward `'don''t'`.
        let buf = String::from("'don'");
        let mut cur = 4;
        assert!(!editor_maybe_skip_quote(&buf, &mut cur, '\''));
        assert_eq!(cur, 4);
    }

    #[test]
    fn typing_quote_inside_sql_literal_inserts_escape_not_skip() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        // Start from the state pair_quote would leave us in after
        // typing `'`, then typing `don` literally: `'don'` with
        // cursor=4 between `n` and the closer.
        a.editor_buffer = "'don'".into();
        a.editor_cursor = 4;
        a.on_key(KeyEvent::from(KeyCode::Char('\'')));
        // Inserts an escape apostrophe instead of skipping past
        // the existing closer — the buffer grows by one char.
        assert_eq!(a.editor_buffer, "'don''");
        assert_eq!(a.editor_cursor, 5);
    }

    #[test]
    fn typing_quote_at_eof_pairs_and_leaves_cursor_between() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::from(KeyCode::Char('\'')));
        assert_eq!(a.editor_buffer, "''");
        assert_eq!(a.editor_cursor, 1);
        // Typing another `'` skips past the pair instead of stacking.
        a.on_key(KeyEvent::from(KeyCode::Char('\'')));
        assert_eq!(a.editor_buffer, "''");
        assert_eq!(a.editor_cursor, 2);
    }

    #[test]
    fn typing_quote_inside_word_inserts_literal_not_pair() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "it".into();
        a.editor_cursor = 2;
        a.on_key(KeyEvent::from(KeyCode::Char('\'')));
        // Falls through to literal insert — covers contractions
        // like `it's` in -- comments.
        assert_eq!(a.editor_buffer, "it'");
        assert_eq!(a.editor_cursor, 3);
    }

    #[test]
    fn editor_toggle_line_comment_adds_marker_on_first_press() {
        let mut buf = String::from("select 1");
        let mut cur = 3; // mid-line
        editor_toggle_line_comment(&mut buf, &mut cur);
        assert_eq!(buf, "-- select 1");
        // Cursor shifts right by 3 (for `-- `).
        assert_eq!(cur, 6);
    }

    #[test]
    fn editor_toggle_line_comment_removes_marker_on_second_press() {
        let mut buf = String::from("-- select 1");
        let mut cur = 6;
        editor_toggle_line_comment(&mut buf, &mut cur);
        assert_eq!(buf, "select 1");
        assert_eq!(cur, 3);
    }

    #[test]
    fn editor_toggle_line_comment_handles_lines_with_no_trailing_space() {
        // `--select` (no space) — remove just 2 chars.
        let mut buf = String::from("--select 1");
        let mut cur = 4;
        editor_toggle_line_comment(&mut buf, &mut cur);
        assert_eq!(buf, "select 1");
        assert_eq!(cur, 2);
    }

    #[test]
    fn editor_toggle_line_comment_operates_per_line_in_multiline_buffer() {
        let mut buf = String::from("select 1;\nselect 2;");
        // Cursor in the second line.
        let mut cur = "select 1;\n".len() + 2;
        editor_toggle_line_comment(&mut buf, &mut cur);
        assert_eq!(buf, "select 1;\n-- select 2;");
        // Cursor shifted right by 3 in the second line.
        assert_eq!(cur, "select 1;\n-- se".len());
    }

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
            cache.columns_by_table.insert(
                ("public".into(), (*name).into()),
                cols.iter().map(|s| s.to_string()).collect(),
            );
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
        let mut a = test_app_with_cache(&[("users", &["id"]), ("users_archived", &["id"])]);
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
            cycle
                .candidates
                .iter()
                .map(|c| &c.display)
                .collect::<Vec<_>>()
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
        let mut a = test_app_with_cache(&[("t_user_logs", &["id"]), ("t_user_roles", &["id"])]);
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
        let labels: Vec<&str> = cycle
            .candidates
            .iter()
            .map(|c| c.display.as_str())
            .collect();
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
        let labels: Vec<&str> = cycle
            .candidates
            .iter()
            .map(|c| c.display.as_str())
            .collect();
        assert!(labels.contains(&"users"), "got {labels:?}");
        assert!(labels.contains(&"orders"), "got {labels:?}");
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
        let labels: Vec<&str> = cycle
            .candidates
            .iter()
            .map(|c| c.display.as_str())
            .collect();
        assert!(labels.contains(&"users"));
        assert!(labels.contains(&"orders"));
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
        let labels: Vec<&str> = cycle
            .candidates
            .iter()
            .map(|c| c.display.as_str())
            .collect();
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
        let labels: Vec<&str> = cycle
            .candidates
            .iter()
            .map(|c| c.display.as_str())
            .collect();
        assert!(
            labels.contains(&"orders"),
            "after narrowing to empty prefix, all tables should be offered; got {labels:?}"
        );
    }

    #[test]
    fn tab_with_no_candidates_falls_back_to_helpful_message() {
        // Disconnected (no cache), empty buffer: there ARE statement
        // keywords available, so the empty-cache message should NOT
        // fire — the popup opens with keywords.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = test_app_with_cache(&[("user_logs", &["id"]), ("user_roles", &["id"])]);
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
            detail: None,
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
    fn history_search_ctrl_d_deletes_focused_entry() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.history = vec![
            "SELECT * FROM users".into(),
            "DELETE FROM tmp WHERE secret = 'abc123'".into(),
            "SELECT count(*) FROM orders".into(),
        ];
        a.mode = Mode::Editor;
        a.start_history_search();
        // Type 'secret' — narrows to the leak entry (index 1).
        for c in "secret".chars() {
            a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(a.editor_buffer.contains("secret"));
        // Ctrl-D deletes it.
        a.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(a.history.len(), 2);
        assert!(!a.history.iter().any(|e| e.contains("secret")));
    }

    #[test]
    fn history_search_finds_most_recent_match_and_walks_older() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "SELECT NOW()".into();
        a.start_watch();
        let w = a.watch.as_ref().expect("watch should be set");
        assert_eq!(w.sql, "SELECT NOW()");
        assert_eq!(w.interval.as_secs(), 2);
    }

    #[test]
    fn start_watch_falls_back_to_last_history_when_buffer_empty() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.history = vec!["SELECT 1".into(), "SELECT count(*) FROM t".into()];
        a.mode = Mode::Editor;
        a.start_watch();
        let w = a.watch.as_ref().expect("watch should be set");
        assert_eq!(w.sql, "SELECT count(*) FROM t");
    }

    #[test]
    fn start_watch_with_no_input_errors() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.start_watch();
        assert!(a.watch.is_none());
        assert!(a.last_error.is_some());
    }

    #[test]
    fn start_watch_refused_during_query() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.editor_buffer = "SELECT 1".into();
        a.query_running = true;
        a.start_watch();
        assert!(a.watch.is_none());
    }

    #[test]
    fn keypress_cancels_active_watch() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
            WatchTickInputs {
                query_running: true,
                ..clear
            },
            WatchTickInputs {
                tx_open: true,
                ..clear
            },
            WatchTickInputs {
                pending_run: true,
                ..clear
            },
            WatchTickInputs {
                mode_blocks: true,
                ..clear
            },
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = grid;
        a.reset_grid_view();
        a.grid_state
            .select(if a.grid.is_empty() { None } else { Some(0) });
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
            truncated: false,
        }
    }

    fn grid_of(columns: &[&str], rows: &[&[&str]]) -> Grid {
        Grid {
            columns: columns.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect(),
            truncated: false,
        }
    }

    #[test]
    fn result_diff_d_with_empty_grid_errors() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = grid_of(&["id"], &[]);
        a.pin_or_diff_result();
        assert!(a.pinned_result.is_none());
        assert!(a.last_error.as_deref().unwrap_or("").contains("no result"));
    }

    #[test]
    fn result_diff_first_d_pins_baseline() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = sample_grid();
        a.pin_or_diff_result();
        let p = a.pinned_result.as_ref().expect("baseline pinned");
        assert_eq!(p.rows.len(), 4);
        assert_eq!(p.columns, vec!["id".to_string(), "name".to_string()]);
        // Pinning alone doesn't open the diff view.
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.result_diff.is_none());
        assert!(a
            .last_status
            .as_deref()
            .unwrap_or("")
            .contains("pinned result A"));
    }

    #[test]
    fn result_diff_second_d_opens_diff_with_inferred_key() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = sample_grid(); // ids 3,1,10,2
        a.pin_or_diff_result(); // pins A
                                // B: id 3 renamed, id 2 removed, id 99 added; 1 and 10 unchanged.
        a.grid = grid_of(
            &["id", "name"],
            &[
                &["3", "CAROL"],
                &["1", "alice"],
                &["10", "bob"],
                &["99", "new"],
            ],
        );
        a.pin_or_diff_result(); // diffs
        assert_eq!(a.mode, Mode::ResultDiff);
        let d = a.result_diff.as_ref().expect("diff computed");
        // id column (0) is unique on both sides → strong key.
        assert!(matches!(
            &d.key,
            crate::query::row_diff::RowKey::Columns(c) if c == &vec![0]
        ));
        assert_eq!(d.diff.changed.len(), 1, "id 3 name changed");
        assert_eq!(d.diff.removed.len(), 1, "id 2 gone");
        assert_eq!(d.diff.added.len(), 1, "id 99 new");
        assert_eq!(d.diff.unchanged, 2, "ids 1 and 10");
        // Baseline persists for iterative diffing.
        assert!(a.pinned_result.is_some());
    }

    #[test]
    fn result_diff_falls_back_to_full_row_when_columns_differ() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = grid_of(&["id", "name"], &[&["1", "x"]]);
        a.pin_or_diff_result();
        // B has a different column layout — cell-level keying is unsafe.
        a.grid = grid_of(&["id", "name", "extra"], &[&["1", "x", "y"]]);
        a.pin_or_diff_result();
        let d = a.result_diff.as_ref().expect("diff computed");
        assert!(matches!(d.key, crate::query::row_diff::RowKey::FullRow));
    }

    #[test]
    fn result_diff_r_repins_b_as_new_baseline() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = grid_of(&["id"], &[&["1"]]);
        a.pin_or_diff_result();
        a.grid = grid_of(&["id"], &[&["1"], &["2"]]);
        a.pin_or_diff_result();
        assert_eq!(a.mode, Mode::ResultDiff);
        a.on_key(KeyEvent::from(KeyCode::Char('r')));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.result_diff.is_none());
        // New baseline = the B side (two rows).
        assert_eq!(a.pinned_result.as_ref().unwrap().rows.len(), 2);
    }

    #[test]
    fn result_diff_c_clears_pin() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = grid_of(&["id"], &[&["1"]]);
        a.pin_or_diff_result();
        a.grid = grid_of(&["id"], &[&["2"]]);
        a.pin_or_diff_result();
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(a.pinned_result.is_none());
        assert!(a.result_diff.is_none());
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn result_diff_d_keybinding_pins_from_normal_mode() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid = sample_grid();
        a.on_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
        assert!(a.pinned_result.is_some());
    }

    fn saved(name: &str, body: &str) -> crate::saved::SavedQuery {
        crate::saved::SavedQuery {
            name: name.into(),
            body: body.into(),
        }
    }

    fn type_str(a: &mut App, s: &str) {
        for c in s.chars() {
            a.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn param_prompt_no_params_loads_directly() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.load_saved_query(saved("plain", "SELECT 1"));
        assert_eq!(a.mode, Mode::Editor);
        assert_eq!(a.editor_buffer, "SELECT 1");
        assert!(a.param_prompt.is_none());
    }

    #[test]
    fn param_prompt_with_params_enters_prompt_mode() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.load_saved_query(saved("byid", "SELECT * FROM t WHERE id = :id"));
        assert_eq!(a.mode, Mode::ParamPrompt);
        assert_eq!(
            a.param_prompt.as_ref().unwrap().params,
            vec!["id".to_string()]
        );
    }

    #[test]
    fn param_prompt_collects_values_and_substitutes() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.load_saved_query(saved(
            "two",
            "SELECT * FROM t WHERE id = :id AND org = :org",
        ));
        type_str(&mut a, "42");
        a.on_key(KeyEvent::from(KeyCode::Enter));
        // First value taken; still prompting for the second.
        assert_eq!(a.mode, Mode::ParamPrompt);
        assert_eq!(a.param_prompt.as_ref().unwrap().idx, 1);
        type_str(&mut a, "7");
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Editor);
        assert_eq!(a.editor_buffer, "SELECT * FROM t WHERE id = 42 AND org = 7");
        assert!(a.param_prompt.is_none());
    }

    #[test]
    fn param_prompt_same_param_twice_fills_both() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.load_saved_query(saved("dup", "SELECT :x WHERE a = :x"));
        // Only one prompt (distinct param), substituted everywhere.
        assert_eq!(a.param_prompt.as_ref().unwrap().params.len(), 1);
        type_str(&mut a, "9");
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.editor_buffer, "SELECT 9 WHERE a = 9");
    }

    #[test]
    fn param_prompt_rejects_empty_value() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.load_saved_query(saved("byid", "WHERE id = :id"));
        a.on_key(KeyEvent::from(KeyCode::Enter)); // empty
        assert_eq!(a.mode, Mode::ParamPrompt);
        assert!(a
            .last_status
            .as_deref()
            .unwrap_or("")
            .contains("value required"));
    }

    #[test]
    fn param_prompt_esc_cancels_back_to_list() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.load_saved_query(saved("byid", "WHERE id = :id"));
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.mode, Mode::SavedQueries);
        assert!(a.param_prompt.is_none());
    }

    #[test]
    fn param_prompt_backspace_edits_input() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.load_saved_query(saved("byid", "WHERE id = :id"));
        type_str(&mut a, "49");
        a.on_key(KeyEvent::from(KeyCode::Backspace));
        type_str(&mut a, "2");
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.editor_buffer, "WHERE id = 42");
    }

    fn app_with_saved(entries: &[(&str, &str)]) -> App {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        for (n, b) in entries {
            a.saved_queries.upsert(saved(n, b));
        }
        a
    }

    #[test]
    fn filter_saved_indices_blank_returns_all() {
        let e = vec![saved("a", "x"), saved("b", "y")];
        assert_eq!(filter_saved_indices(&e, None), vec![0, 1]);
        assert_eq!(filter_saved_indices(&e, Some("   ")), vec![0, 1]);
    }

    #[test]
    fn filter_saved_indices_matches_name_case_insensitive() {
        let e = vec![saved("ActiveUsers", "..."), saved("revenue", "...")];
        assert_eq!(filter_saved_indices(&e, Some("active")), vec![0]);
        assert_eq!(filter_saved_indices(&e, Some("REV")), vec![1]);
    }

    #[test]
    fn filter_saved_indices_matches_body_too() {
        let e = vec![
            saved("a", "SELECT * FROM orders"),
            saved("b", "SELECT * FROM users"),
        ];
        assert_eq!(filter_saved_indices(&e, Some("orders")), vec![0]);
    }

    #[test]
    fn filter_saved_indices_no_match_is_empty() {
        let e = vec![saved("a", "x")];
        assert!(filter_saved_indices(&e, Some("zzz")).is_empty());
    }

    #[test]
    fn saved_filter_narrows_live_and_maps_focus_to_real_index() {
        let mut a = app_with_saved(&[("users", "a"), ("orders", "b"), ("revenue", "c")]);
        a.open_saved_queries();
        a.on_key(KeyEvent::from(KeyCode::Char('/')));
        assert_eq!(a.mode, Mode::SavedQueriesFilter);
        type_str(&mut a, "ord");
        assert_eq!(
            a.saved_queries_filter.as_ref().map(|t| t.text()),
            Some("ord")
        );
        assert_eq!(a.visible_saved_indices(), vec![1]);
        // Cursor 0 in the filtered view maps to real entry index 1.
        assert_eq!(a.focused_saved_index(), Some(1));
        // Enter keeps the filter applied and returns to navigation.
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::SavedQueries);
        assert_eq!(
            a.saved_queries_filter.as_ref().map(|t| t.text()),
            Some("ord")
        );
    }

    #[test]
    fn saved_filter_esc_clears_filter() {
        let mut a = app_with_saved(&[("users", "a"), ("orders", "b")]);
        a.open_saved_queries();
        a.on_key(KeyEvent::from(KeyCode::Char('/')));
        type_str(&mut a, "ord");
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.mode, Mode::SavedQueries);
        assert!(a.saved_queries_filter.is_none());
    }

    #[test]
    fn saved_filter_backspace_widens() {
        let mut a = app_with_saved(&[("users", "a"), ("orders", "b")]);
        a.open_saved_queries();
        a.on_key(KeyEvent::from(KeyCode::Char('/')));
        type_str(&mut a, "ordz");
        assert!(a.visible_saved_indices().is_empty());
        a.on_key(KeyEvent::from(KeyCode::Backspace)); // back to "ord"
        assert_eq!(a.visible_saved_indices(), vec![1]);
    }

    #[test]
    fn rename_prompt_prefills_current_name() {
        let mut a = app_with_saved(&[("old", "x")]);
        a.open_saved_queries();
        a.on_key(KeyEvent::from(KeyCode::Char('r')));
        assert_eq!(a.mode, Mode::RenameQueryPrompt);
        assert_eq!(a.rename_query_buffer.text(), "old");
        assert_eq!(a.rename_query_from, "old");
    }

    #[test]
    fn rename_rejects_empty_name() {
        let mut a = app_with_saved(&[("old", "x")]);
        a.open_saved_queries();
        a.on_key(KeyEvent::from(KeyCode::Char('r')));
        for _ in 0..8 {
            a.on_key(KeyEvent::from(KeyCode::Backspace));
        }
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::RenameQueryPrompt);
        assert!(a
            .last_status
            .as_deref()
            .unwrap_or("")
            .contains("name required"));
        // Original name untouched.
        assert_eq!(a.saved_queries.entries[0].name, "old");
    }

    #[test]
    fn rename_refuses_collision_without_changing_entries() {
        let mut a = app_with_saved(&[("a", "x"), ("b", "y")]);
        a.open_saved_queries(); // cursor on "a"
        a.on_key(KeyEvent::from(KeyCode::Char('r')));
        for _ in 0..8 {
            a.on_key(KeyEvent::from(KeyCode::Backspace));
        }
        type_str(&mut a, "b"); // collide with existing "b"
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::RenameQueryPrompt); // stayed put
        assert!(a
            .last_status
            .as_deref()
            .unwrap_or("")
            .contains("already exists"));
        assert_eq!(a.saved_queries.entries[0].name, "a");
        assert_eq!(a.saved_queries.entries[1].name, "b");
    }

    #[test]
    fn rename_esc_cancels_without_changing_entries() {
        let mut a = app_with_saved(&[("a", "x")]);
        a.open_saved_queries();
        a.on_key(KeyEvent::from(KeyCode::Char('r')));
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.mode, Mode::SavedQueries);
        assert_eq!(a.saved_queries.entries[0].name, "a");
    }

    #[test]
    fn dispatch_fixture_writes_parseable_dataset_to_explicit_path() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = grid_of(&["id", "name"], &[&["1", "alice"], &["2", "bob"]]);
        a.grid_source = Some(("public".into(), "users".into()));
        let dir = std::env::temp_dir().join(format!("pgman-fixture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("users.xml");
        a.dispatch_fixture(Some(path.to_string_lossy().to_string()));
        assert!(a.last_status.as_deref().unwrap_or("").contains("2 row(s)"));
        let xml = std::fs::read_to_string(&path).unwrap();
        let parsed = crate::dbunit::parse_flat_xml(&xml).unwrap();
        assert_eq!(parsed.rows.len(), 2);
        assert_eq!(parsed.rows[0].table, "users");
        assert_eq!(
            parsed.rows[0].columns,
            vec![("id".into(), "1".into()), ("name".into(), "alice".into())]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dispatch_fixture_errors_without_source_table() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = grid_of(&["id"], &[&["1"]]);
        a.grid_source = None;
        a.dispatch_fixture(None);
        assert!(a
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("single-table"));
    }

    #[test]
    fn dispatch_fixture_errors_on_empty_grid() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = grid_of(&["id"], &[]);
        a.grid_source = Some(("public".into(), "users".into()));
        a.dispatch_fixture(None);
        assert!(a.last_error.as_deref().unwrap_or("").contains("no result"));
    }

    fn write_temp_fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pgman-clean-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fx = dir.join("f.xml");
        std::fs::write(&fx, r#"<dataset><users id="1"/></dataset>"#).unwrap();
        fx
    }

    #[test]
    fn load_dbunit_fixture_uses_per_db_clean_mode() {
        let fx = write_temp_fixture("delete");
        let mut cfg = SafetyConfig::default();
        cfg.databases.insert(
            "legacy".into(),
            crate::safety::SafetyProfile {
                clean_mode: crate::dbunit::CleanMode::DeleteFrom,
                ..Default::default()
            },
        );
        let dsn = crate::conn::Dsn::parse("postgres://u@h/legacy").ok();
        let mut a = App::new(Theme::default(), dsn, Vec::new(), cfg);
        a.editor_buffer = fx.to_string_lossy().to_string();
        a.load_dbunit_fixture();
        assert!(
            a.editor_buffer.contains("DELETE FROM users"),
            "expected DELETE FROM; got:\n{}",
            a.editor_buffer
        );
        assert!(!a.editor_buffer.contains("TRUNCATE"));
        let _ = std::fs::remove_file(&fx);
    }

    #[test]
    fn load_dbunit_fixture_defaults_to_truncate() {
        let fx = write_temp_fixture("trunc");
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.editor_buffer = fx.to_string_lossy().to_string();
        a.load_dbunit_fixture();
        assert!(
            a.editor_buffer.contains("TRUNCATE TABLE users"),
            "expected TRUNCATE; got:\n{}",
            a.editor_buffer
        );
        let _ = std::fs::remove_file(&fx);
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

    #[test]
    fn infer_single_source_table_picks_one_from_simple_select() {
        let got = infer_single_source_table("SELECT * FROM users WHERE active = true");
        assert_eq!(got, Some(("public".into(), "users".into())));
    }

    #[test]
    fn infer_single_source_table_returns_none_for_joins() {
        assert!(infer_single_source_table("SELECT * FROM users u JOIN orders o ON true").is_none());
    }

    #[test]
    fn infer_single_source_table_returns_none_for_no_from() {
        assert!(infer_single_source_table("SELECT 1").is_none());
    }

    #[test]
    fn infer_single_source_table_keeps_explicit_schema() {
        assert_eq!(
            infer_single_source_table("SELECT * FROM audit.events"),
            Some(("audit".into(), "events".into()))
        );
    }

    #[test]
    fn format_sql_literal_nulls_empty_strings() {
        assert_eq!(format_sql_literal(""), "NULL");
    }

    #[test]
    fn format_sql_literal_passes_numerics_unquoted() {
        assert_eq!(format_sql_literal("42"), "42");
        assert_eq!(format_sql_literal("3.14"), "3.14");
        assert_eq!(format_sql_literal("-1"), "-1");
    }

    #[test]
    fn format_sql_literal_lowercases_booleans() {
        assert_eq!(format_sql_literal("TRUE"), "true");
        assert_eq!(format_sql_literal("False"), "false");
    }

    #[test]
    fn format_sql_literal_quotes_strings_and_doubles_internal_quotes() {
        assert_eq!(format_sql_literal("alice"), "'alice'");
        assert_eq!(format_sql_literal("it's fine"), "'it''s fine'");
    }

    #[test]
    fn yank_row_as_insert_no_source_surfaces_actionable_error() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid_source = None;
        a.grid = Grid {
            columns: vec!["id".into()],
            rows: vec![vec!["1".into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.yank_row_as_insert();
        let err = a.last_error.as_deref().unwrap_or("");
        assert!(
            err.contains("single-table SELECTs"),
            "expected actionable error; got: {err}"
        );
    }

    #[test]
    fn normal_mode_esc_is_a_noop_does_not_quit() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(!a.should_quit);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn conn_pick_esc_is_a_noop_does_not_quit() {
        let dsn = Dsn::parse("postgres://test@localhost/test").unwrap();
        let picks = vec![
            DataSourcePick {
                name: "a".into(),
                origin: "test",
                dsn: dsn.clone(),
            },
            DataSourcePick {
                name: "b".into(),
                origin: "test",
                dsn,
            },
        ];
        let mut a = App::new(Theme::default(), None, picks, SafetyConfig::default());
        a.mode = Mode::ConnPick;
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(!a.should_quit);
        assert_eq!(a.mode, Mode::ConnPick);
    }

    #[test]
    fn open_cell_detail_parses_json_object_and_primes_tree() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = Grid {
            columns: vec!["data".into()],
            rows: vec![vec![r#"{"id":1,"name":"alice"}"#.into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.row_detail_field = 0;
        a.open_cell_detail();
        assert_eq!(a.mode, Mode::CellDetail);
        // Root + 2 members.
        assert_eq!(a.json_cell_rows.len(), 3);
        assert!(a.json_cell_value.is_some());
    }

    #[test]
    fn open_cell_detail_leaves_tree_empty_for_non_json_cells() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = Grid {
            columns: vec!["note".into()],
            rows: vec![vec!["hello world".into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.row_detail_field = 0;
        a.open_cell_detail();
        assert_eq!(a.mode, Mode::CellDetail);
        assert!(a.json_cell_rows.is_empty());
        assert!(a.json_cell_value.is_none());
    }

    #[test]
    fn cell_detail_json_jk_moves_cursor_within_bounds() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = Grid {
            columns: vec!["data".into()],
            rows: vec![vec![r#"{"a":1,"b":2}"#.into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.row_detail_field = 0;
        a.open_cell_detail();
        // 3 rows: root, .a, .b. Start at 0.
        assert_eq!(a.json_cell_cursor, 0);
        a.on_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(a.json_cell_cursor, 1);
        a.on_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(a.json_cell_cursor, 2);
        // Clamp at last row.
        a.on_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(a.json_cell_cursor, 2);
        // k walks back.
        a.on_key(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(a.json_cell_cursor, 1);
    }

    #[test]
    fn cell_detail_json_enter_collapses_focused_container() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = Grid {
            columns: vec!["data".into()],
            rows: vec![vec![r#"{"a":{"x":1},"b":2}"#.into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.row_detail_field = 0;
        a.open_cell_detail();
        // Walk to .a (the nested object).
        a.on_key(KeyEvent::from(KeyCode::Char('j')));
        let path_at_cursor = a.json_cell_rows[a.json_cell_cursor].path.clone();
        assert_eq!(path_at_cursor, ".a");
        // Expanded → collapsed reduces row count.
        let expanded_count = a.json_cell_rows.len();
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(a.json_cell_rows.len() < expanded_count);
        assert!(a.json_cell_collapsed.contains(".a"));
        // Cursor stayed on .a (didn't drift to a sibling).
        assert_eq!(a.json_cell_rows[a.json_cell_cursor].path, ".a");
        // Toggle back: row count restored.
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.json_cell_rows.len(), expanded_count);
        assert!(!a.json_cell_collapsed.contains(".a"));
    }

    #[test]
    fn cell_detail_json_esc_returns_to_row_detail() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.grid = Grid {
            columns: vec!["data".into()],
            rows: vec![vec![r#"{"a":1}"#.into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.row_detail_field = 0;
        a.open_cell_detail();
        assert_eq!(a.mode, Mode::CellDetail);
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.mode, Mode::RowDetail);
    }

    #[test]
    fn slow_queries_enter_copies_focused_sql_to_editor_and_returns() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::SlowQueries;
        a.slow_queries = vec![
            crate::query::slow_queries::SlowQueryRow {
                query: "SELECT 1".into(),
                calls: 100,
                total_ms: 500.0,
                mean_ms: 5.0,
                rows: 100,
            },
            crate::query::slow_queries::SlowQueryRow {
                query: "UPDATE x SET y=1".into(),
                calls: 10,
                total_ms: 200.0,
                mean_ms: 20.0,
                rows: 10,
            },
        ];
        a.slow_queries_cursor = 1;
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Editor);
        assert_eq!(a.editor_buffer, "UPDATE x SET y=1");
    }

    #[test]
    fn slow_queries_jk_clamps_to_row_range() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::SlowQueries;
        a.slow_queries = vec![
            crate::query::slow_queries::SlowQueryRow {
                query: "a".into(),
                calls: 1,
                total_ms: 1.0,
                mean_ms: 1.0,
                rows: 1,
            },
            crate::query::slow_queries::SlowQueryRow {
                query: "b".into(),
                calls: 2,
                total_ms: 2.0,
                mean_ms: 1.0,
                rows: 2,
            },
        ];
        for _ in 0..10 {
            a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        assert_eq!(a.slow_queries_cursor, 1);
    }

    #[test]
    fn sessions_esc_returns_to_normal() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Sessions;
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn start_slow_queries_without_client_surfaces_not_connected() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.start_slow_queries();
        assert_eq!(a.mode, Mode::Normal);
        assert!(a
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("not connected"));
    }

    #[test]
    fn slow_queries_loaded_failure_with_missing_extension_hints_install() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::SlowQueries;
        a.generation = 1;
        a.on_msg(AppMsg::SlowQueriesLoaded {
            generation: 1,
            result: Err("ERROR: relation \"pg_stat_statements\" does not exist".into()),
        });
        // Back to Normal + actionable hint in the error.
        assert_eq!(a.mode, Mode::Normal);
        let err = a.last_error.as_deref().unwrap_or("");
        assert!(
            err.contains("CREATE EXTENSION pg_stat_statements"),
            "expected install hint; got: {err}"
        );
    }

    fn app_with_schemas() -> App {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        let mut cache = crate::query::schema::SchemaCache::default();
        cache.schemas = vec!["audit".into(), "public".into()];
        cache.tables = vec![
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "users".into(),
            },
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "orders".into(),
            },
            crate::query::schema::TableMeta {
                schema: "audit".into(),
                name: "events".into(),
            },
        ];
        cache.columns_by_table.insert(
            ("public".into(), "users".into()),
            vec!["id".into(), "email".into()],
        );
        a.schema_cache = cache;
        a
    }

    #[test]
    fn schema_browser_flat_starts_with_schemas_collapsed() {
        let a = app_with_schemas();
        let rows = a.flattened_schema_browser();
        assert_eq!(rows.len(), 2);
        assert!(matches!(
            rows[0],
            SchemaBrowserRow::Schema {
                ref name,
                expanded: false,
                ..
            } if name == "audit"
        ));
        assert!(matches!(
            rows[1],
            SchemaBrowserRow::Schema {
                ref name,
                ..
            } if name == "public"
        ));
    }

    #[test]
    fn schema_browser_enter_expands_focused_schema() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        // Focus row 1 (public).
        a.schema_browser_cursor = 1;
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let rows = a.flattened_schema_browser();
        // Now: audit (collapsed), public (expanded), orders, users.
        assert_eq!(rows.len(), 4);
        assert!(matches!(
            rows[1],
            SchemaBrowserRow::Schema { expanded: true, .. }
        ));
        assert!(matches!(
            rows[2],
            SchemaBrowserRow::Table { ref name, .. } if name == "orders"
        ));
        assert!(matches!(
            rows[3],
            SchemaBrowserRow::Table { ref name, .. } if name == "users"
        ));
    }

    #[test]
    fn schema_browser_jk_nav_clamps_to_visible() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        for _ in 0..10 {
            a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        // Only 2 visible rows (schemas collapsed); cursor at 1.
        assert_eq!(a.schema_browser_cursor, 1);
    }

    #[test]
    fn schema_browser_esc_returns_to_normal() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn schema_browser_enter_on_table_expands_to_columns_and_constraints() {
        let mut a = app_with_schemas();
        // Add a constraint so the third level isn't just columns.
        a.schema_cache.constraints = vec![crate::query::schema::ConstraintMeta {
            schema: "public".into(),
            table: "users".into(),
            name: "users_pkey".into(),
        }];
        a.mode = Mode::SchemaBrowser;
        // Expand "public" first, then drill into "users".
        a.schema_browser_cursor = 1; // public
        a.on_key(KeyEvent::from(KeyCode::Enter));
        // Now rows: audit, public(expanded), orders, users.
        // Move to "users" (row 3) and toggle.
        a.schema_browser_cursor = 3;
        a.on_key(KeyEvent::from(KeyCode::Enter));
        let rows = a.flattened_schema_browser();
        // audit, public, orders, users(expanded), id, email, users_pkey.
        assert_eq!(rows.len(), 7);
        assert!(matches!(
            rows[3],
            SchemaBrowserRow::Table {
                ref name,
                expanded: true,
                ..
            } if name == "users"
        ));
        assert!(matches!(
            rows[4],
            SchemaBrowserRow::Column { ref name, .. } if name == "id"
        ));
        assert!(matches!(
            rows[5],
            SchemaBrowserRow::Column { ref name, .. } if name == "email"
        ));
        assert!(matches!(
            rows[6],
            SchemaBrowserRow::Constraint { ref name, .. }
                if name == "users_pkey"
        ));
    }

    #[test]
    fn schema_browser_collapsing_schema_hides_its_table_drilldown() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        // Expand "public", expand "public.users".
        a.schema_browser_expanded.insert("public".into());
        a.schema_browser_expanded
            .insert(schema_browser_table_key("public", "users"));
        // Now collapse "public" again.
        a.schema_browser_cursor = 1;
        a.on_key(KeyEvent::from(KeyCode::Enter));
        let rows = a.flattened_schema_browser();
        // Only the two schema rows are visible.
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .all(|r| matches!(r, SchemaBrowserRow::Schema { .. })));
        // The "public.users" key is still set — re-expanding the schema
        // restores the open table drilldown.
        a.on_key(KeyEvent::from(KeyCode::Enter));
        let rows = a.flattened_schema_browser();
        assert!(rows.iter().any(|r| matches!(
            r,
            SchemaBrowserRow::Column { name, .. } if name == "id"
        )));
    }

    #[test]
    fn quote_ident_passes_simple_snake_case_unquoted() {
        assert_eq!(quote_ident("users"), "users");
        assert_eq!(quote_ident("user_id"), "user_id");
        assert_eq!(quote_ident("_internal"), "_internal");
        assert_eq!(quote_ident("a1"), "a1");
    }

    #[test]
    fn quote_ident_wraps_anything_unusual() {
        assert_eq!(quote_ident("User"), "\"User\"");
        assert_eq!(quote_ident("1col"), "\"1col\"");
        assert_eq!(quote_ident("with space"), "\"with space\"");
        assert_eq!(quote_ident("café"), "\"café\"");
        assert_eq!(quote_ident("evil\"name"), "\"evil\"\"name\"");
    }

    #[test]
    fn build_select_all_template_uses_quoted_idents_only_when_needed() {
        assert_eq!(
            build_select_all_template("public", "users"),
            "SELECT * FROM public.users LIMIT 100;"
        );
        assert_eq!(
            build_select_all_template("Audit", "Events"),
            "SELECT * FROM \"Audit\".\"Events\" LIMIT 100;"
        );
    }

    #[test]
    fn build_insert_template_emits_one_null_per_column() {
        let sql = build_insert_template(
            "public",
            "users",
            &["id".into(), "email".into(), "active".into()],
        );
        assert_eq!(
            sql,
            "INSERT INTO public.users\n  (id, email, active)\nVALUES\n  (NULL, NULL, NULL);"
        );
    }

    #[test]
    fn build_insert_template_returns_empty_when_no_columns() {
        assert!(build_insert_template("public", "t", &[]).is_empty());
    }

    fn log_picks_with_an_n_plus_one_cluster() -> Vec<crate::query::reconstruct::ReconstructedQuery>
    {
        use crate::query::reconstruct::{ReconstructedQuery, Source};
        let make = |sql: &str| ReconstructedQuery {
            raw_sql: sql.into(),
            params: Vec::new(),
            runnable_sql: sql.into(),
            source: Source::HibernateLog,
            src_line: 0,
        };
        vec![
            make("select * from item where order_id = 1"),
            make("select * from item where order_id = 2"),
            make("select * from item where order_id = 3"),
            make("select * from orders where id = 1"),
        ]
    }

    #[test]
    fn log_pick_visible_len_reflects_view() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.log_picks = log_picks_with_an_n_plus_one_cluster();
        a.log_pick_clusters = crate::query::nplus1::detect(&a.log_picks);
        a.mode = Mode::LogPick;
        assert_eq!(a.log_pick_view, LogPickView::AllQueries);
        assert_eq!(a.log_pick_visible_len(), 4);
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert_eq!(a.log_pick_view, LogPickView::Clusters);
        assert_eq!(a.log_pick_visible_len(), 1); // one repeated shape
    }

    #[test]
    fn log_pick_toggle_resets_cursor() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.log_picks = log_picks_with_an_n_plus_one_cluster();
        a.log_pick_clusters = crate::query::nplus1::detect(&a.log_picks);
        a.mode = Mode::LogPick;
        // Cursor at row 3 in AllQueries view.
        a.log_pick_index = 3;
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        // Clusters view has only 1 row → cursor must clamp.
        assert_eq!(a.log_pick_index, 0);
    }

    #[test]
    fn log_pick_enter_in_cluster_view_loads_example_sql() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.log_picks = log_picks_with_an_n_plus_one_cluster();
        a.log_pick_clusters = crate::query::nplus1::detect(&a.log_picks);
        a.mode = Mode::LogPick;
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Editor);
        assert!(
            a.editor_buffer.contains("from item where order_id"),
            "buffer should be the cluster's example; got: {:?}",
            a.editor_buffer
        );
    }

    #[test]
    fn schema_browser_s_on_schema_row_surfaces_error_not_garbage() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        a.schema_browser_cursor = 0; // a schema row
        a.on_key(KeyEvent::from(KeyCode::Char('s')));
        assert!(a.last_error.is_some());
    }

    #[test]
    fn schema_browser_i_with_no_cached_columns_surfaces_error() {
        let mut a = app_with_schemas();
        // public.orders has no columns_by_table entry.
        a.schema_browser_expanded.insert("public".into());
        a.mode = Mode::SchemaBrowser;
        // Walk to orders.
        let rows = a.flattened_schema_browser();
        let idx = rows
            .iter()
            .position(|r| matches!(r, SchemaBrowserRow::Table { name, .. } if name == "orders"))
            .unwrap();
        a.schema_browser_cursor = idx;
        a.on_key(KeyEvent::from(KeyCode::Char('i')));
        let err = a.last_error.as_deref().unwrap_or("");
        assert!(err.contains("no column info"), "got: {err}");
    }

    #[test]
    fn is_cost_checkable_accepts_plain_selects_and_ctes() {
        assert!(is_cost_checkable("SELECT * FROM users"));
        assert!(is_cost_checkable("  select 1"));
        assert!(is_cost_checkable("WITH x AS (SELECT 1) SELECT * FROM x"));
        assert!(is_cost_checkable("TABLE users"));
        assert!(is_cost_checkable("VALUES (1, 2)"));
    }

    #[test]
    fn is_cost_checkable_rejects_writes_and_explain() {
        assert!(!is_cost_checkable("INSERT INTO t VALUES (1)"));
        assert!(!is_cost_checkable("UPDATE t SET a = 1"));
        assert!(!is_cost_checkable("DELETE FROM t"));
        assert!(!is_cost_checkable("EXPLAIN SELECT 1"));
        assert!(!is_cost_checkable("CREATE TABLE t (id int)"));
    }

    #[test]
    fn is_cost_checkable_skips_self_bounded_limit_queries() {
        // A LIMIT means the query already self-bounds its result —
        // pre-flight gating would be noisy.
        assert!(!is_cost_checkable("SELECT * FROM events LIMIT 100"));
        assert!(!is_cost_checkable("select * from t LIMIT 5"));
    }

    #[test]
    fn is_cost_checkable_ignores_limit_inside_string_literal() {
        // The token `limit` only counts when it's actually a clause.
        // A literal value with the word in it must NOT skip the gate.
        assert!(is_cost_checkable(
            "SELECT 'over the limit' AS reason FROM t"
        ));
        // Same with doubled-quote escapes inside.
        assert!(is_cost_checkable("SELECT 'it''s past the limit' FROM t"));
    }

    #[test]
    fn is_cost_checkable_rejects_cte_wrapped_writes() {
        // CTE-wrapped writes look like SELECT but are really DML.
        // Reject so the cost-preview Confirm doesn't misleadingly
        // call them "estimated N rows — proceed?".
        assert!(!is_cost_checkable(
            "WITH d AS (DELETE FROM t RETURNING id) SELECT count(*) FROM d"
        ));
        assert!(!is_cost_checkable(
            "WITH u AS (UPDATE t SET x=1 RETURNING *) SELECT * FROM u"
        ));
        assert!(!is_cost_checkable(
            "WITH i AS (INSERT INTO t VALUES (1) RETURNING id) SELECT * FROM i"
        ));
    }

    #[test]
    fn is_cost_checkable_keeps_delete_keyword_in_string_safe() {
        // The CTE-write check would false-reject a query with
        // 'DELETE' inside a string literal if string-stripping
        // weren't applied. Verify the stripping rescues it.
        assert!(is_cost_checkable("SELECT 'DELETE me later' AS note FROM t"));
    }

    #[test]
    fn strip_strings_replaces_literal_bodies_preserving_length() {
        let s = "SELECT 'hello' FROM t WHERE x = \"a\\b\"";
        let out = strip_strings(s);
        assert_eq!(out.len(), s.len());
        // The 'hello' body got replaced; the quoting char stays.
        assert!(out.contains("'_____'"));
        // The double-quoted body got replaced too.
        assert!(out.contains("\"___\""));
    }

    #[test]
    fn strip_strings_handles_doubled_quote_escapes() {
        let s = "SELECT 'it''s ok'";
        let out = strip_strings(s);
        assert_eq!(out.len(), s.len());
        // The whole body including the `''` escape becomes `_`s
        // (the embedded quote was treated as part of the literal,
        // not a terminator).
        assert!(out.contains("'________'"));
    }

    #[test]
    fn format_row_estimate_uses_commas() {
        assert_eq!(format_row_estimate(0.0), "0");
        assert_eq!(format_row_estimate(999.0), "999");
        assert_eq!(format_row_estimate(1_000.0), "1,000");
        assert_eq!(format_row_estimate(1_234_567.0), "1,234,567");
        assert_eq!(format_row_estimate(4_200_000.5), "4,200,001");
    }

    #[test]
    fn history_encode_decode_round_trips_multiline() {
        let sample = "select 1\nfrom t\nwhere x = 'a\\b'";
        let encoded = encode_history_line(sample);
        assert!(
            !encoded.contains('\n'),
            "encoded must be one line: {encoded:?}"
        );
        let decoded = decode_history_line(&encoded);
        assert_eq!(decoded, sample);
    }

    #[test]
    fn history_decode_tolerates_unknown_escapes() {
        // Unknown `\?` sequences emit literally so we never lose bytes.
        assert_eq!(decode_history_line("\\?"), "\\?");
        // Trailing lone `\` at end of string keeps the literal.
        assert_eq!(decode_history_line("foo\\"), "foo\\");
    }

    #[test]
    fn history_persist_then_load_round_trips_via_temp_file() {
        let dir = std::env::temp_dir().join(format!("pgman-history-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.log");
        let entries: Vec<String> = vec![
            "select 1".into(),
            "select *\nfrom users".into(),
            "select now()".into(),
        ];
        persist_history_to(&path, &entries).unwrap();
        let loaded = load_history_from(&path);
        assert_eq!(loaded, entries);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn history_persist_caps_to_history_cap_entries() {
        let dir = std::env::temp_dir().join(format!("pgman-history-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.log");
        // 200 entries → file gets capped at HISTORY_CAP.
        let many: Vec<String> = (0..200).map(|i| format!("query {i}")).collect();
        persist_history_to(&path, &many).unwrap();
        let loaded = load_history_from(&path);
        assert_eq!(loaded.len(), HISTORY_CAP);
        // Persist keeps the NEWEST cap entries — symmetric with
        // load_history_from which also drops from the head.
        // For 200 entries, the kept window is [150..199].
        assert_eq!(loaded[0], format!("query {}", 200 - HISTORY_CAP));
        assert_eq!(loaded[HISTORY_CAP - 1], "query 199");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn history_load_from_missing_file_returns_empty() {
        let path = std::env::temp_dir().join("definitely-not-a-real-file-xyz");
        let _ = std::fs::remove_file(&path);
        assert!(load_history_from(&path).is_empty());
    }

    #[test]
    fn filter_schema_browser_rows_matches_self() {
        let rows = vec![
            SchemaBrowserRow::Schema {
                name: "public".into(),
                expanded: false,
                table_count: 1,
            },
            SchemaBrowserRow::Schema {
                name: "audit".into(),
                expanded: false,
                table_count: 1,
            },
        ];
        let out = filter_schema_browser_rows(rows, "aud");
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            SchemaBrowserRow::Schema { name, .. } if name == "audit"
        ));
    }

    #[test]
    fn filter_schema_browser_rows_keeps_ancestor_of_match() {
        let rows = vec![
            SchemaBrowserRow::Schema {
                name: "public".into(),
                expanded: true,
                table_count: 2,
            },
            SchemaBrowserRow::Table {
                schema: "public".into(),
                name: "orders".into(),
                expanded: false,
                column_count: 0,
                constraint_count: 0,
            },
            SchemaBrowserRow::Table {
                schema: "public".into(),
                name: "users".into(),
                expanded: false,
                column_count: 0,
                constraint_count: 0,
            },
        ];
        let out = filter_schema_browser_rows(rows, "users");
        // public schema (ancestor) + users table.
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0], SchemaBrowserRow::Schema { name, .. } if name == "public"));
        assert!(matches!(&out[1], SchemaBrowserRow::Table { name, .. } if name == "users"));
    }

    #[test]
    fn filter_schema_browser_rows_keeps_path_to_deep_match() {
        let rows = vec![
            SchemaBrowserRow::Schema {
                name: "public".into(),
                expanded: true,
                table_count: 1,
            },
            SchemaBrowserRow::Table {
                schema: "public".into(),
                name: "users".into(),
                expanded: true,
                column_count: 2,
                constraint_count: 0,
            },
            SchemaBrowserRow::Column {
                schema: "public".into(),
                table: "users".into(),
                name: "email".into(),
            },
            SchemaBrowserRow::Column {
                schema: "public".into(),
                table: "users".into(),
                name: "id".into(),
            },
        ];
        let out = filter_schema_browser_rows(rows, "email");
        // schema + table + email column.
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[2], SchemaBrowserRow::Column { name, .. } if name == "email"));
    }

    #[test]
    fn filter_schema_browser_rows_is_case_insensitive() {
        let rows = vec![SchemaBrowserRow::Schema {
            name: "PUBLIC".into(),
            expanded: false,
            table_count: 0,
        }];
        assert_eq!(filter_schema_browser_rows(rows, "pub").len(), 1);
    }

    #[test]
    fn schema_browser_slash_starts_filter_mode_with_empty_pattern() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        a.on_key(KeyEvent::from(KeyCode::Char('/')));
        assert_eq!(a.mode, Mode::SchemaBrowserFilter);
        assert_eq!(a.schema_browser_filter.as_deref(), Some(""));
    }

    #[test]
    fn schema_browser_filter_typing_narrows_tree_live() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        a.on_key(KeyEvent::from(KeyCode::Char('/')));
        a.on_key(KeyEvent::from(KeyCode::Char('a')));
        a.on_key(KeyEvent::from(KeyCode::Char('u')));
        // Filter is "au"; only the `audit` schema matches.
        let rows = a.flattened_schema_browser();
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], SchemaBrowserRow::Schema { name, .. } if name == "audit"));
    }

    #[test]
    fn schema_browser_filter_enter_accepts_keeps_filter_applied() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        a.on_key(KeyEvent::from(KeyCode::Char('/')));
        a.on_key(KeyEvent::from(KeyCode::Char('a')));
        a.on_key(KeyEvent::from(KeyCode::Char('u')));
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::SchemaBrowser);
        assert_eq!(a.schema_browser_filter.as_deref(), Some("au"));
    }

    #[test]
    fn schema_browser_filter_esc_clears() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        a.on_key(KeyEvent::from(KeyCode::Char('/')));
        a.on_key(KeyEvent::from(KeyCode::Char('a')));
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.mode, Mode::SchemaBrowser);
        assert!(a.schema_browser_filter.is_none());
    }

    fn synthetic_browser_rows() -> Vec<SchemaBrowserRow> {
        // schema A (expanded) → tA1 (expanded) → col1, col2 → schema B
        vec![
            SchemaBrowserRow::Schema {
                name: "a".into(),
                expanded: true,
                table_count: 1,
            },
            SchemaBrowserRow::Table {
                schema: "a".into(),
                name: "tA1".into(),
                expanded: true,
                column_count: 2,
                constraint_count: 0,
            },
            SchemaBrowserRow::Column {
                schema: "a".into(),
                table: "tA1".into(),
                name: "col1".into(),
            },
            SchemaBrowserRow::Column {
                schema: "a".into(),
                table: "tA1".into(),
                name: "col2".into(),
            },
            SchemaBrowserRow::Schema {
                name: "b".into(),
                expanded: false,
                table_count: 0,
            },
        ]
    }

    #[test]
    fn next_schema_row_idx_skips_past_table_internals_forward() {
        let rows = synthetic_browser_rows();
        // From schema "a" at index 0 → next schema is "b" at 4,
        // jumping over its table + columns in one move.
        assert_eq!(next_schema_row_idx(&rows, 0, Direction::Forward), Some(4));
        // From a column row (depth 2) the next schema is still "b".
        assert_eq!(next_schema_row_idx(&rows, 3, Direction::Forward), Some(4));
        // From the last schema → no next.
        assert_eq!(next_schema_row_idx(&rows, 4, Direction::Forward), None);
    }

    #[test]
    fn next_schema_row_idx_walks_back_skipping_internals() {
        let rows = synthetic_browser_rows();
        // From schema "b" at 4 → previous schema is "a" at 0.
        assert_eq!(next_schema_row_idx(&rows, 4, Direction::Backward), Some(0));
        // From a column (index 2) → previous schema is "a".
        assert_eq!(next_schema_row_idx(&rows, 2, Direction::Backward), Some(0));
        // From the first schema → no previous.
        assert_eq!(next_schema_row_idx(&rows, 0, Direction::Backward), None);
    }

    #[test]
    fn schema_browser_bracket_keys_jump_by_schema() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        // Expand "public" so we have schema + tables + (collapsed)
        // schema below for an interesting jump.
        a.schema_browser_expanded.insert("public".into());
        // Cursor at row 0 (audit schema, first).
        a.schema_browser_cursor = 0;
        // `]` jumps to the next schema row.
        a.on_key(KeyEvent::from(KeyCode::Char(']')));
        let rows = a.flattened_schema_browser();
        assert!(matches!(
            rows.get(a.schema_browser_cursor),
            Some(SchemaBrowserRow::Schema { name, .. }) if name == "public"
        ));
        // `[` goes back.
        a.on_key(KeyEvent::from(KeyCode::Char('[')));
        let rows = a.flattened_schema_browser();
        assert!(matches!(
            rows.get(a.schema_browser_cursor),
            Some(SchemaBrowserRow::Schema { name, .. }) if name == "audit"
        ));
    }

    #[test]
    fn schema_browser_plus_expands_everything() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        assert_eq!(a.flattened_schema_browser().len(), 2); // only schemas
        a.on_key(KeyEvent::from(KeyCode::Char('+')));
        let rows = a.flattened_schema_browser();
        // Both schemas expanded → schemas + tables visible.
        // audit(1 table) + public(2 tables): 2 + 1 + 2 = 5 rows
        // minimum (tables aren't expanded — they have no columns
        // in the test fixture for `audit.events` / `public.orders`,
        // so toggling them doesn't add rows). public.users has 2
        // columns → +2 rows when its table-key is expanded. Total = 7.
        assert!(
            rows.len() >= 5,
            "expected expansion; got {} rows",
            rows.len()
        );
        // Every schema is marked expanded.
        for row in &rows {
            if let SchemaBrowserRow::Schema { expanded, .. } = row {
                assert!(*expanded, "schema not expanded: {row:?}");
            }
        }
    }

    #[test]
    fn schema_browser_minus_collapses_everything() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        // First expand, then collapse, verify back to just schemas.
        a.on_key(KeyEvent::from(KeyCode::Char('+')));
        assert!(a.flattened_schema_browser().len() > 2);
        a.on_key(KeyEvent::from(KeyCode::Char('-')));
        // Back to one row per schema.
        assert_eq!(a.flattened_schema_browser().len(), 2);
        assert!(a.schema_browser_expanded.is_empty());
    }

    #[test]
    fn schema_browser_pagedown_jumps_ten_rows() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        // Synthetic: drive enough rows by expanding everything.
        a.on_key(KeyEvent::from(KeyCode::Char('+')));
        a.schema_browser_cursor = 0;
        a.on_key(KeyEvent::from(KeyCode::PageDown));
        let rows_len = a.flattened_schema_browser().len();
        let expected = 10usize.min(rows_len.saturating_sub(1));
        assert_eq!(a.schema_browser_cursor, expected);
    }

    #[test]
    fn start_schema_lint_with_empty_cache_surfaces_hint() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.start_schema_lint();
        assert_ne!(a.mode, Mode::SchemaLint);
        assert!(a
            .last_status
            .as_deref()
            .unwrap_or("")
            .contains("schema cache empty"));
    }

    #[test]
    fn start_schema_lint_with_findings_opens_panel_and_summarises() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        let mut cache = crate::query::schema::SchemaCache::default();
        cache.schemas = vec!["public".into()];
        // Two LINT001s (no constraints), one LINT002 (mixed-case).
        cache.tables = vec![
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "events".into(),
            },
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "OrderItems".into(),
            },
        ];
        a.schema_cache = cache;
        a.start_schema_lint();
        assert_eq!(a.mode, Mode::SchemaLint);
        assert!(!a.schema_lint_findings.is_empty());
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("finding(s)") && status.contains("high"),
            "status should summarise count + severity; got: {status}"
        );
    }

    #[test]
    fn m_then_letter_sets_grid_bookmark_at_focus() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid = Grid {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec!["1".into(), "2".into()], vec!["3".into(), "4".into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0, 1];
        a.grid_state.select(Some(1));
        a.grid_col_cursor = 1;
        // m, then 'q'.
        a.on_key(KeyEvent::from(KeyCode::Char('m')));
        assert!(a.pending_mark_set);
        a.on_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(!a.pending_mark_set);
        let bm = a.bookmarks.get(&'q').copied().expect("bookmark set");
        assert_eq!(bm.row, 1);
        assert_eq!(bm.col, 1);
    }

    #[test]
    fn jump_to_bookmark_moves_selection_and_col_cursor() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid = Grid {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec!["1".into(), "2".into()], vec!["3".into(), "4".into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0, 1];
        a.bookmarks.insert('a', GridBookmark { row: 1, col: 1 });
        // 'a → jumps.
        a.on_key(KeyEvent::from(KeyCode::Char('\'')));
        a.on_key(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(a.grid_state.selected(), Some(1));
        assert_eq!(a.grid_col_cursor, 1);
    }

    #[test]
    fn jump_to_unset_bookmark_surfaces_status_no_op() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid = Grid {
            columns: vec!["a".into()],
            rows: vec![vec!["1".into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.on_key(KeyEvent::from(KeyCode::Char('\'')));
        a.on_key(KeyEvent::from(KeyCode::Char('z')));
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(status.contains("no bookmark"));
    }

    #[test]
    fn m_followed_by_non_letter_clears_pending_silently() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.on_key(KeyEvent::from(KeyCode::Char('m')));
        assert!(a.pending_mark_set);
        a.on_key(KeyEvent::from(KeyCode::Char('1')));
        // Pending cleared, no bookmark set.
        assert!(!a.pending_mark_set);
        assert!(a.bookmarks.is_empty());
    }

    #[test]
    fn fk_navigate_with_no_grid_source_surfaces_actionable_error() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid_source = None;
        a.grid = Grid {
            columns: vec!["id".into()],
            rows: vec![vec!["1".into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.navigate_fk_from_focused_cell();
        let err = a.last_error.as_deref().unwrap_or("");
        assert!(err.contains("single-table SELECT"));
    }

    #[test]
    fn fk_navigate_with_non_fk_column_surfaces_hint() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid_source = Some(("public".into(), "orders".into()));
        a.grid = Grid {
            columns: vec!["id".into()],
            rows: vec![vec!["1".into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.grid_col_cursor = 0;
        a.navigate_fk_from_focused_cell();
        let err = a.last_error.as_deref().unwrap_or("");
        assert!(err.contains("isn't a foreign key"));
    }

    #[test]
    fn fk_navigate_opens_new_tab_with_parent_select() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid_source = Some(("public".into(), "orders".into()));
        a.grid = Grid {
            columns: vec!["id".into(), "user_id".into()],
            rows: vec![vec!["1".into(), "42".into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0];
        a.grid_state.select(Some(0));
        a.grid_col_cursor = 1; // user_id
        a.schema_cache.fk_edges.push(crate::query::schema::FkEdge {
            child_schema: "public".into(),
            child_table: "orders".into(),
            child_column: "user_id".into(),
            parent_schema: "public".into(),
            parent_table: "users".into(),
            parent_column: "id".into(),
        });
        a.navigate_fk_from_focused_cell();
        // New tab opened.
        assert_eq!(a.tabs.len(), 2);
        assert_eq!(a.active_tab, 1);
        // Editor in the new tab holds the parent SELECT.
        assert!(
            a.editor_buffer
                .contains("SELECT * FROM public.users WHERE id = 42"),
            "expected parent select; got: {}",
            a.editor_buffer
        );
        // We're in the editor ready to F5.
        assert_eq!(a.mode, Mode::Editor);
    }

    #[test]
    fn new_tab_pushes_a_fresh_state_and_switches() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "tab one".into();
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(a.tabs.len(), 2);
        assert_eq!(a.active_tab, 1);
        // New tab's editor is empty.
        assert_eq!(a.editor_buffer, "");
    }

    #[test]
    fn cycle_tab_round_trips_state_per_tab() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "one".into();
        a.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        a.editor_buffer = "two".into();
        // Cycle back to first tab.
        a.cycle_tab(false);
        assert_eq!(a.editor_buffer, "one");
        assert_eq!(a.active_tab, 0);
        // Forward to second.
        a.cycle_tab(true);
        assert_eq!(a.editor_buffer, "two");
        assert_eq!(a.active_tab, 1);
    }

    #[test]
    fn all_prompt_modes_count_as_text_input() {
        // Every text-entry mode must opt into is_text_input so the
        // global Ctrl-W (close-tab) chord stays inert while typing.
        for m in [
            Mode::Editor,
            Mode::ParamPrompt,
            Mode::SavedQueriesFilter,
            Mode::RenameQueryPrompt,
            Mode::SaveQueryPrompt,
            Mode::GridFilter,
            Mode::GridFind,
            Mode::HistorySearch,
            Mode::SchemaBrowserFilter,
        ] {
            assert!(m.is_text_input(), "{m:?} should be a text-input mode");
        }
        assert!(!Mode::Normal.is_text_input());
        assert!(!Mode::ResultDiff.is_text_input());
        assert!(!Mode::TapMonitor.is_text_input());
    }

    #[test]
    fn ctrl_w_in_a_prompt_does_not_close_the_tab() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.new_tab(); // two tabs, so close_active_tab would otherwise fire
        assert_eq!(a.tabs.len(), 2);
        a.mode = Mode::ParamPrompt;
        a.param_prompt = Some(ParamPrompt {
            query_name: "q".into(),
            template: "SELECT :x".into(),
            params: vec!["x".into()],
            idx: 0,
            values: Vec::new(),
            input: TextInput::new(),
        });
        a.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(
            a.tabs.len(),
            2,
            "Ctrl-W must not close a tab while typing in a prompt"
        );
        assert_eq!(a.mode, Mode::ParamPrompt);
    }

    #[test]
    fn result_diff_pin_is_per_tab_and_does_not_leak() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid = Grid {
            columns: vec!["id".into()],
            rows: vec![vec!["1".into()]],
            truncated: false,
        };
        // Pin A on tab 1.
        a.pin_or_diff_result();
        assert!(a.pinned_result.is_some(), "tab 1 should have a pinned A");
        // A fresh tab must NOT inherit the pin — otherwise the first D
        // there diffs against an unrelated baseline.
        a.new_tab();
        assert!(
            a.pinned_result.is_none(),
            "a fresh tab must start with no pinned baseline"
        );
        // Returning to tab 1 restores its pin.
        a.cycle_tab(false);
        assert!(
            a.pinned_result.is_some(),
            "returning to tab 1 should restore its pinned baseline"
        );
    }

    #[test]
    fn tab_switch_dismisses_an_open_result_diff_overlay() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid = Grid {
            columns: vec!["id".into()],
            rows: vec![vec!["1".into()]],
            truncated: false,
        };
        a.pin_or_diff_result(); // pin A
        a.grid.rows = vec![vec!["2".into()]];
        a.pin_or_diff_result(); // diff → opens the overlay
        assert_eq!(a.mode, Mode::ResultDiff);
        a.new_tab();
        // The transient overlay must not survive onto the new tab.
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.result_diff.is_none());
    }

    #[test]
    fn close_tab_drops_current_and_loads_neighbour() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.editor_buffer = "first".into();
        a.new_tab(); // → tab 2
        a.editor_buffer = "second".into();
        // Close the active (2nd) tab → load first.
        a.close_active_tab();
        assert_eq!(a.tabs.len(), 1);
        assert_eq!(a.active_tab, 0);
        assert_eq!(a.editor_buffer, "first");
    }

    #[test]
    fn close_tab_on_only_tab_is_a_noop_with_hint() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.editor_buffer = "lonely".into();
        a.close_active_tab();
        assert_eq!(a.tabs.len(), 1);
        assert_eq!(a.editor_buffer, "lonely");
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(status.contains("only one tab"));
    }

    #[test]
    fn new_tab_refuses_past_cap() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        // Drive up to the cap.
        for _ in 1..TAB_CAP {
            a.new_tab();
        }
        assert_eq!(a.tabs.len(), TAB_CAP);
        a.new_tab(); // refuse
        assert_eq!(a.tabs.len(), TAB_CAP);
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(status.contains("max tabs"));
    }

    #[test]
    fn alt_digit_jumps_directly_to_tab() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.editor_buffer = "t1".into();
        a.new_tab();
        a.editor_buffer = "t2".into();
        a.new_tab();
        a.editor_buffer = "t3".into();
        // Alt-1 → jump to the first tab.
        a.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
        assert_eq!(a.active_tab, 0);
        assert_eq!(a.editor_buffer, "t1");
    }

    #[test]
    fn tab_switch_blocked_during_query_running() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "one".into();
        a.new_tab();
        a.editor_buffer = "two".into();
        a.query_running = true;
        // Try to switch back — should be blocked.
        let before = a.active_tab;
        a.switch_to_tab(0);
        assert_eq!(a.active_tab, before);
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(status.contains("query is running"));
    }

    #[test]
    fn default_query_name_sanitises_to_kebab() {
        assert_eq!(
            default_query_name("SELECT * FROM users"),
            "select-from-users"
        );
        // 40-char take from the line, sanitised + trimmed.
        assert_eq!(
            default_query_name("WITH active AS (SELECT 1) SELECT * FROM active"),
            "with-active-as-select-1-select-from"
        );
        // Leading whitespace skipped.
        assert_eq!(default_query_name("  \n\n select 1"), "select-1");
        // Symbols collapse to nothing; runs of space collapse.
        assert_eq!(default_query_name("a    b"), "a-b");
    }

    #[test]
    fn save_query_prompt_persists_buffer_under_name() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "select 1".into();
        a.editor_cursor = a.editor_buffer.len();
        // Ctrl-S — open the prompt.
        a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(a.mode, Mode::SaveQueryPrompt);
        // Type a name (the default is pre-filled but we overwrite).
        a.save_query_name.clear();
        for c in "mine".chars() {
            a.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        // Enter persists.
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Editor);
        let q = a.saved_queries.get("mine").expect("entry saved");
        assert_eq!(q.body, "select 1");
    }

    #[test]
    fn save_query_prompt_esc_cancels_without_persist() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.editor_buffer = "select 1".into();
        a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Editor);
        assert!(a.saved_queries.entries.is_empty());
    }

    #[test]
    fn saved_queries_panel_enter_loads_into_editor() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.saved_queries.upsert(crate::saved::SavedQuery {
            name: "ru".into(),
            body: "SELECT now();".into(),
        });
        a.mode = Mode::Normal;
        a.editor_buffer = "draft".into();
        // Q opens.
        a.on_key(KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT));
        assert_eq!(a.mode, Mode::SavedQueries);
        // Enter loads into editor.
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Editor);
        assert_eq!(a.editor_buffer, "SELECT now();");
    }

    #[test]
    fn saved_queries_panel_d_deletes_focused_entry() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.saved_queries.upsert(crate::saved::SavedQuery {
            name: "a".into(),
            body: "select 1".into(),
        });
        a.saved_queries.upsert(crate::saved::SavedQuery {
            name: "b".into(),
            body: "select 2".into(),
        });
        a.mode = Mode::SavedQueries;
        a.saved_queries_cursor = 0;
        a.on_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(a.saved_queries.entries.len(), 1);
        assert_eq!(a.saved_queries.entries[0].name, "b");
    }

    #[test]
    fn open_saved_queries_with_empty_list_surfaces_hint() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.open_saved_queries();
        assert_ne!(a.mode, Mode::SavedQueries);
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(status.contains("Ctrl-S"));
    }

    #[test]
    fn notification_message_appends_to_ring_and_caps() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // Drive past the cap.
        for i in 0..(NOTIFICATION_CAP + 50) {
            a.on_msg(AppMsg::Notification {
                generation: a.generation,
                notification: crate::conn::NotificationMsg {
                    channel: "users".into(),
                    pid: 1234,
                    payload: format!("event-{i}"),
                },
            });
        }
        assert_eq!(a.notifications.len(), NOTIFICATION_CAP);
        // Newest at the end.
        let last = a.notifications.last().unwrap();
        assert_eq!(last.payload, format!("event-{}", NOTIFICATION_CAP + 49));
    }

    // --- tap-event ring tests ------------------------

    fn tap_query(sql: &str, received_at: u64) -> crate::tap::TapEvent {
        crate::tap::TapEvent {
            v: 1,
            kind: crate::tap::TapKind::Query,
            ts_unix_micros: received_at,
            received_at_unix_micros: received_at,
            app: Some("billing-service".into()),
            pool: None,
            conn: Some("primary-7".into()),
            txn: None,
            sql: Some(sql.into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(100),
            rows: Some(1),
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        }
    }

    fn tap_heartbeat(dropped: u64, received_at: u64) -> crate::tap::TapEvent {
        crate::tap::TapEvent {
            v: 1,
            kind: crate::tap::TapKind::Heartbeat,
            ts_unix_micros: received_at,
            received_at_unix_micros: received_at,
            app: Some("billing-service".into()),
            pool: None,
            conn: None,
            txn: None,
            sql: None,
            params: None,
            params_redacted: false,
            duration_micros: None,
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: Some(dropped),
            txn_outcome: None,
        }
    }

    #[test]
    fn tap_query_event_lands_in_ring_and_bumps_count() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 1", 1_000_000),
        });
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 2", 2_000_000),
        });
        assert_eq!(a.tap_events.len(), 2);
        assert_eq!(a.tap_health.query_count, 2);
        // Newest at the back.
        assert_eq!(
            a.tap_events.back().and_then(|e| e.sql.as_deref()),
            Some("SELECT 2")
        );
        assert_eq!(a.tap_health.last_event_at_unix_micros, 2_000_000);
    }

    #[test]
    fn tap_heartbeat_does_not_pollute_ring_but_updates_health() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 1", 1_000_000),
        });
        a.on_msg(AppMsg::TapEvent {
            event: tap_heartbeat(17, 1_500_000),
        });
        // Ring only carries the query — heartbeat stays out.
        assert_eq!(a.tap_events.len(), 1);
        assert_eq!(a.tap_health.heartbeat_count, 1);
        assert_eq!(a.tap_health.dropped_events_total, 17);
        // Heartbeat still counts as a "we heard from the JAR" signal.
        assert_eq!(a.tap_health.last_event_at_unix_micros, 1_500_000);
    }

    #[test]
    fn tap_ring_evicts_oldest_past_cap() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        for i in 0..(TAP_CAP + 50) {
            a.on_msg(AppMsg::TapEvent {
                event: tap_query(&format!("q{i}"), i as u64),
            });
        }
        assert_eq!(a.tap_events.len(), TAP_CAP);
        // First event surviving the eviction is q50 (the first 50 were dropped).
        assert_eq!(
            a.tap_events.front().and_then(|e| e.sql.as_deref()),
            Some("q50")
        );
        // Newest at the back.
        assert_eq!(
            a.tap_events.back().and_then(|e| e.sql.as_deref()),
            Some(format!("q{}", TAP_CAP + 49).as_str())
        );
        assert_eq!(a.tap_health.query_count, (TAP_CAP + 50) as u64);
    }

    #[test]
    fn tap_ring_eviction_keeps_cursor_aligned_with_content() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // Fill exactly to cap.
        for i in 0..TAP_CAP {
            a.on_msg(AppMsg::TapEvent {
                event: tap_query(&format!("q{i}"), i as u64),
            });
        }
        // Cursor parked on the oldest row.
        a.tap_events_cursor = 0;
        let oldest_sql = a.tap_events.front().and_then(|e| e.sql.clone());
        assert_eq!(oldest_sql.as_deref(), Some("q0"));
        // One more event evicts q0; cursor stays in-bounds and
        // points at "what used to be the second row".
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("new", 9_999),
        });
        assert_eq!(a.tap_events.len(), TAP_CAP);
        assert_eq!(
            a.tap_events.front().and_then(|e| e.sql.as_deref()),
            Some("q1")
        );
        // Cursor decremented to follow the eviction.
        assert_eq!(a.tap_events_cursor, 0);
    }

    #[test]
    fn f4_opens_tap_monitor_from_any_mode() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::TapMonitor);
    }

    #[test]
    fn tap_monitor_status_distinguishes_no_traffic_from_no_jar() {
        // Empty case.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.start_tap_monitor();
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("no events yet"),
            "expected no-events hint; got {status}"
        );
        // After traffic.
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 1", 1),
        });
        // Pretend the JAR also sent a heartbeat.
        a.on_msg(AppMsg::TapEvent {
            event: tap_heartbeat(0, 2),
        });
        a.mode = Mode::Normal;
        a.start_tap_monitor();
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("1 queries") && status.contains("1 heartbeats"),
            "expected counters in status; got {status}"
        );
    }

    #[test]
    fn tap_monitor_q_closes_to_normal_and_clears_status() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.start_tap_monitor();
        a.on_key(KeyEvent::from(KeyCode::Char('q')));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.last_status.is_none());
    }

    #[test]
    fn tap_monitor_c_clears_the_ring_and_resets_cursor() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        for i in 0..3 {
            a.on_msg(AppMsg::TapEvent {
                event: tap_query(&format!("q{i}"), i),
            });
        }
        a.start_tap_monitor();
        a.tap_events_cursor = 2;
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(a.tap_events.is_empty());
        assert_eq!(a.tap_events_cursor, 0);
        assert_eq!(a.last_status.as_deref(), Some("cleared 3 tap event(s)"));
    }

    #[test]
    fn tap_monitor_v_cycles_through_seven_views() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 1", 1),
        });
        a.start_tap_monitor();
        assert_eq!(a.tap_view, TapView::List);
        a.on_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(a.tap_view, TapView::Hotspots);
        a.on_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(a.tap_view, TapView::Callers);
        a.on_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(a.tap_view, TapView::Transactions);
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("transactions"),
            "expected transactions in status: {status}"
        );
        a.on_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(a.tap_view, TapView::Pools);
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("pools"),
            "expected pools in status: {status}"
        );
        a.on_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(a.tap_view, TapView::NplusOne);
        a.on_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(a.tap_view, TapView::Baseline);
        a.on_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(a.tap_view, TapView::List);
    }

    #[test]
    fn tap_monitor_pools_view_navigates_and_clears() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // Two pools: primary (two conns) and replica (one conn).
        for (pool, conn, i) in [
            ("primary", "p-1", 0u64),
            ("primary", "p-2", 1),
            ("replica", "r-1", 2),
        ] {
            let mut e = tap_query("SELECT 1", i);
            e.pool = Some(pool.into());
            e.conn = Some(conn.into());
            e.ts_unix_micros = i;
            e.received_at_unix_micros = i;
            a.on_msg(AppMsg::TapEvent { event: e });
        }
        a.start_tap_monitor();
        a.tap_view = TapView::Pools;
        let pools = a.current_pools();
        assert_eq!(pools.len(), 2);
        // Navigation clamps to the last row.
        a.on_key(KeyEvent::from(KeyCode::Char('G')));
        assert_eq!(a.tap_pools_cursor, 1);
        a.on_key(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(a.tap_pools_cursor, 0);
        // `c` clears the ring from the pools view too.
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(a.tap_events.is_empty());
        assert_eq!(a.tap_pools_cursor, 0);
        assert!(a.current_pools().is_empty());
    }

    #[test]
    fn tap_monitor_txns_view_navigates_and_clears() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // Two transactions: c-1#a (3 stmts, open), c-1#b (1 stmt, open).
        for i in 0..3u64 {
            let mut e = tap_query("SELECT a", i);
            e.txn = Some("c-1#a".into());
            e.ts_unix_micros = i;
            e.received_at_unix_micros = i;
            a.on_msg(AppMsg::TapEvent { event: e });
        }
        let mut e = tap_query("SELECT b", 100);
        e.txn = Some("c-1#b".into());
        e.ts_unix_micros = 100;
        e.received_at_unix_micros = 100;
        a.on_msg(AppMsg::TapEvent { event: e });
        a.start_tap_monitor();
        a.tap_view = TapView::Transactions;
        let txns = a.current_txns();
        assert_eq!(txns.len(), 2);
        assert!(txns.iter().all(|t| t.is_open()));
        // c-1#a has the bigger span (0..2 = 2µs) so sorts first.
        assert_eq!(txns[0].txn.as_deref(), Some("c-1#a"));
        // c clears the ring → 0 transactions.
        a.tap_txns_cursor = 1;
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(a.current_txns().is_empty());
        assert_eq!(a.tap_txns_cursor, 0);
    }

    #[test]
    fn tap_monitor_callers_view_navigates_sorts_and_clears() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // Three events from two different callers.
        for (i, caller) in [
            ("OrderService.findById:42", 100),
            ("OrderService.findById:42", 200),
            ("UserService.lookup:7", 50),
        ]
        .into_iter()
        .enumerate()
        {
            let (frame, dur) = caller;
            let mut e = tap_query(&format!("SELECT {i}"), i as u64);
            e.caller = Some(vec![frame.into()]);
            e.duration_micros = Some(dur);
            a.on_msg(AppMsg::TapEvent { event: e });
        }
        a.start_tap_monitor();
        a.tap_view = TapView::Callers;
        let groups = a.current_callers();
        assert_eq!(groups.len(), 2);
        // TotalTime sort default — OrderService bucket wins (100+200=300 > 50).
        assert_eq!(groups[0].caller, "OrderService.findById:42");
        // `s` cycles to CallCount; OrderService also wins (2 > 1).
        a.on_key(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(a.tap_sort, crate::tap::HotspotSort::CallCount);
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(status.contains("callers · sort"), "got: {status}");
        // `c` clears; cursors reset.
        a.tap_callers_cursor = 1;
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(a.current_callers().is_empty());
        assert_eq!(a.tap_callers_cursor, 0);
    }

    #[test]
    fn shift_b_captures_baseline_from_any_tap_view() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // Seed two distinct fingerprints so the hotspots
        // bucket count is meaningful.
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT a FROM t1", 1),
        });
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT b FROM t2", 2),
        });
        a.start_tap_monitor();
        assert!(a.tap_baseline.is_none());
        // Shift-B from the default List view captures.
        a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        let baseline = a.tap_baseline.as_ref().expect("baseline captured");
        assert_eq!(baseline.hotspots.len(), 2);
        assert_eq!(baseline.captured_event_count, 2);
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("baseline captured"),
            "expected confirmation status: {status}"
        );
    }

    #[test]
    fn baseline_diff_flags_new_fingerprint_after_capture() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT a FROM t1", 1),
        });
        a.start_tap_monitor();
        a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        // New fingerprint arrives post-baseline.
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT b FROM t2", 2),
        });
        let diff = a.current_baseline_diff();
        // Old "select a from t?" is unchanged (filtered);
        // new "select b from t?" surfaces.
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].kind, crate::tap::DiffKind::New);
    }

    #[test]
    fn baseline_clear_keeps_snapshot_clears_ring() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 1", 1),
        });
        a.start_tap_monitor();
        a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        a.tap_view = TapView::Baseline;
        // c clears the ring; the captured snapshot survives
        // (operator might want to re-fill the ring against
        // the same baseline post-deploy).
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(a.tap_events.is_empty());
        assert!(a.tap_baseline.is_some(), "baseline must persist across `c`");
    }

    #[test]
    fn baseline_records_listener_drop_watermark_at_capture() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 1", 1),
        });
        // Snapshot the global atomic before capture so the
        // assertion is robust to whatever other tests
        // contributed (cumulative-counter semantics).
        let baseline_drops = crate::tap::dropped_at_listener();
        a.start_tap_monitor();
        a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        let captured = a.tap_baseline.as_ref().unwrap().captured_listener_dropped;
        assert!(
            captured >= baseline_drops,
            "captured_listener_dropped must reflect a counter snapshot at-or-after baseline read"
        );
        // delta-since-capture starts at zero (or whatever
        // concurrent tests added between capture and this read).
        let delta = a.baseline_listener_drops_since_capture().unwrap();
        assert_eq!(delta, crate::tap::dropped_at_listener() - captured);
    }

    #[test]
    fn baseline_recapture_overwrites_previous_snapshot() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 1", 1),
        });
        a.start_tap_monitor();
        a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        let first_count = a.tap_baseline.as_ref().unwrap().captured_event_count;
        assert_eq!(first_count, 1);
        // Two more events arrive; recapture.
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 2", 2),
        });
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 3", 3),
        });
        a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        let second_count = a.tap_baseline.as_ref().unwrap().captured_event_count;
        assert_eq!(second_count, 3, "recapture must reflect the larger ring");
    }

    #[test]
    fn tap_monitor_v_cycle_includes_baseline_as_last_view() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.start_tap_monitor();
        a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Hotspots
        a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Callers
        a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Transactions
        a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Pools
        a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → NplusOne
        a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Baseline
        assert_eq!(a.tap_view, TapView::Baseline);
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("baseline diff"),
            "expected baseline-diff status: {status}"
        );
        a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → back to List
        assert_eq!(a.tap_view, TapView::List);
    }

    #[test]
    fn tap_monitor_nplus1_view_navigates_and_clears() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // 6 same-shape events in one txn within 200ms → 1
        // finding fires.
        for i in 0..6 {
            let mut e = tap_query("SELECT * FROM users WHERE id = ?", i * 20_000);
            e.txn = Some("c-1#1".into());
            e.ts_unix_micros = i * 20_000;
            e.received_at_unix_micros = i * 20_000;
            a.on_msg(AppMsg::TapEvent { event: e });
        }
        a.start_tap_monitor();
        a.tap_view = TapView::NplusOne;
        let findings = a.current_nplus1();
        assert_eq!(findings.len(), 1);
        // Down past the end clamps.
        for _ in 0..5 {
            a.on_key(KeyEvent::from(KeyCode::Char('j')));
        }
        assert_eq!(a.tap_nplus1_cursor, 0);
        // c clears the ring → no findings.
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(a.current_nplus1().is_empty());
        assert_eq!(a.tap_nplus1_cursor, 0);
    }

    #[test]
    fn tap_monitor_g_capital_jumps_to_end_in_both_views() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        for i in 0..5 {
            a.on_msg(AppMsg::TapEvent {
                event: tap_query(&format!("SELECT * FROM t{i}"), i),
            });
        }
        a.start_tap_monitor();
        // List view: `G` jumps to last row.
        a.on_key(KeyEvent::from(KeyCode::Char('G')));
        assert_eq!(a.tap_events_cursor, 4);
        // Toggle to hotspots; `G` jumps within the hotspot list.
        a.tap_view = TapView::Hotspots;
        a.on_key(KeyEvent::from(KeyCode::Char('G')));
        let hotspots = a.current_hotspots();
        assert_eq!(a.tap_hotspots_cursor, hotspots.len().saturating_sub(1));
    }

    #[test]
    fn tap_monitor_s_cycles_sort_in_hotspots_view() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT 1", 1),
        });
        a.start_tap_monitor();
        a.tap_view = TapView::Hotspots;
        assert_eq!(a.tap_sort, crate::tap::HotspotSort::TotalTime);
        a.on_key(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(a.tap_sort, crate::tap::HotspotSort::CallCount);
        a.on_key(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(a.tap_sort, crate::tap::HotspotSort::P95Latency);
        a.on_key(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(a.tap_sort, crate::tap::HotspotSort::TotalTime);
    }

    #[test]
    fn tap_monitor_s_in_list_view_is_a_noop() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.start_tap_monitor();
        let sort_before = a.tap_sort;
        a.on_key(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(a.tap_sort, sort_before, "list view ignores `s`");
        assert_eq!(a.tap_view, TapView::List);
    }

    #[test]
    fn tap_monitor_hotspots_clear_resets_both_cursors() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        for i in 0..3 {
            a.on_msg(AppMsg::TapEvent {
                event: tap_query(&format!("q{i}"), i),
            });
        }
        a.start_tap_monitor();
        a.tap_view = TapView::Hotspots;
        a.tap_hotspots_cursor = 2;
        a.tap_events_cursor = 2;
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(a.tap_events.is_empty());
        assert_eq!(a.tap_hotspots_cursor, 0);
        assert_eq!(a.tap_events_cursor, 0);
    }

    #[test]
    fn current_hotspots_reflects_current_sort() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // Bucket A: many cheap calls.
        for _ in 0..50 {
            a.on_msg(AppMsg::TapEvent {
                event: tap_query("SELECT a FROM t_a", 1),
            });
        }
        // Bucket B: one expensive call.
        let mut spike = tap_query("SELECT b FROM t_b", 1_000_000);
        spike.duration_micros = Some(1_000_000);
        a.on_msg(AppMsg::TapEvent { event: spike });
        a.tap_sort = crate::tap::HotspotSort::TotalTime;
        let by_total = a.current_hotspots();
        assert_eq!(by_total[0].count, 1, "expensive spike wins on total time");
        a.tap_sort = crate::tap::HotspotSort::CallCount;
        let by_count = a.current_hotspots();
        assert_eq!(by_count[0].count, 50, "cheap bucket wins on call count");
    }

    #[test]
    fn tap_monitor_jk_navigation_clamps_at_ends() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        for i in 0..3 {
            a.on_msg(AppMsg::TapEvent {
                event: tap_query(&format!("q{i}"), i),
            });
        }
        a.start_tap_monitor();
        // Down past the end clamps to last.
        for _ in 0..10 {
            a.on_key(KeyEvent::from(KeyCode::Char('j')));
        }
        assert_eq!(a.tap_events_cursor, 2);
        // Up past the start clamps to 0.
        for _ in 0..10 {
            a.on_key(KeyEvent::from(KeyCode::Char('k')));
        }
        assert_eq!(a.tap_events_cursor, 0);
    }

    #[test]
    fn tap_message_is_not_generation_gated() {
        // Tap listener is independent of the DB connection; a
        // reconnect (which bumps generation) shouldn't drop tap
        // events that arrived after.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.generation = 42;
        let msg = AppMsg::TapEvent {
            event: tap_query("SELECT 1", 1_000),
        };
        // Generation accessor returns 0 (not 42) — the dispatcher
        // doesn't filter this.
        assert_eq!(msg.generation(), 0);
        a.on_msg(msg);
        assert_eq!(a.tap_events.len(), 1);
    }

    #[test]
    fn f3_opens_notifications_from_any_mode() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Notifications);
    }

    #[test]
    fn notifications_c_clears_the_ring() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Notifications;
        a.notifications = vec![crate::conn::NotificationMsg {
            channel: "x".into(),
            pid: 1,
            payload: "p".into(),
        }];
        a.on_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(a.notifications.is_empty());
        assert_eq!(a.notifications_cursor, 0);
    }

    #[test]
    fn capital_r_in_sessions_toggles_auto_refresh() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode_seen.insert(Mode::Sessions);
        a.mode = Mode::Sessions;
        assert!(!a.auto_refresh);
        a.on_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
        assert!(a.auto_refresh);
        // Status reflects the toggle.
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(status.contains("auto-refresh on"));
        // Toggle off.
        a.on_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
        assert!(!a.auto_refresh);
    }

    #[test]
    fn tick_auto_refresh_noop_when_disabled() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Sessions;
        a.auto_refresh = false;
        a.auto_refresh_last = Some(std::time::Instant::now() - std::time::Duration::from_secs(60));
        a.tick_auto_refresh();
        // No refresh fired (client is None → refresh_sessions would
        // surface an error; we just check no panic / status change).
        assert!(a.last_error.is_none());
    }

    #[test]
    fn tick_auto_refresh_noop_when_query_running() {
        // The tick must not stack a refresh on top of an in-flight
        // query — would surface stale-generation noise.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Sessions;
        a.auto_refresh = true;
        a.query_running = true;
        a.auto_refresh_last = Some(std::time::Instant::now() - std::time::Duration::from_secs(60));
        a.tick_auto_refresh();
        // last unchanged because we bailed.
        let elapsed = a.auto_refresh_last.unwrap().elapsed();
        assert!(elapsed >= std::time::Duration::from_secs(60));
    }

    #[test]
    fn capital_k_in_sessions_opens_confirm_terminate_with_pid() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Sessions;
        a.sessions = vec![crate::query::sessions::SessionRow {
            pid: 12345,
            user: "app".into(),
            application: "service-x".into(),
            state: "active".into(),
            age_secs: 42.0,
            blocked_by: String::new(),
            query: "SELECT * FROM events".into(),
            wait_event: None,
        }];
        a.sessions_cursor = 0;
        a.on_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT));
        assert_eq!(a.mode, Mode::ConfirmTerminate);
        assert_eq!(a.pending_terminate, Some(12345));
        // Status should mention the pid.
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("12345"),
            "expected pid in status; got: {status}"
        );
    }

    #[test]
    fn confirm_terminate_n_cancels_and_clears_pending() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        // Seed `mode_seen` for Sessions so the cancel path's
        // status isn't overwritten by the first-entry tip.
        // Production flow always enters Sessions before opening
        // ConfirmTerminate.
        a.mode_seen.insert(Mode::Sessions);
        a.mode = Mode::ConfirmTerminate;
        a.pending_terminate = Some(999);
        a.on_key(KeyEvent::from(KeyCode::Char('n')));
        assert_eq!(a.mode, Mode::Sessions);
        assert!(a.pending_terminate.is_none());
        assert_eq!(a.last_status.as_deref(), Some("terminate cancelled"));
    }

    #[test]
    fn confirm_terminate_esc_cancels() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode_seen.insert(Mode::Sessions);
        a.mode = Mode::ConfirmTerminate;
        a.pending_terminate = Some(123);
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Sessions);
        assert!(a.pending_terminate.is_none());
    }

    #[test]
    fn capital_k_with_empty_session_list_does_not_open_confirm() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Sessions;
        a.sessions.clear();
        a.on_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT));
        // No session to terminate → stay in Sessions, no pending.
        assert_eq!(a.mode, Mode::Sessions);
        assert!(a.pending_terminate.is_none());
    }

    #[test]
    fn compute_grid_find_matches_finds_all_hits_in_row_major_order() {
        let grid = Grid {
            columns: vec!["name".into(), "city".into()],
            rows: vec![
                vec!["alice".into(), "London".into()],
                vec!["bob".into(), "Berlin".into()],
                vec!["carol".into(), "London".into()],
            ],
            truncated: false,
        };
        let visible: Vec<usize> = (0..grid.rows.len()).collect();
        let matches = compute_grid_find_matches(&grid, &visible, "lon");
        // Cells "London" in rows 0 and 2 at col 1.
        assert_eq!(matches, vec![(0, 1), (2, 1)]);
    }

    #[test]
    fn compute_grid_find_matches_honours_visible_subset() {
        let grid = Grid {
            columns: vec!["name".into()],
            rows: vec![
                vec!["alice".into()],
                vec!["bob".into()],
                vec!["alex".into()],
            ],
            truncated: false,
        };
        // Filter has hidden row 1 ("bob"). visible_rows is the
        // post-filter index list.
        let visible = vec![0, 2];
        let matches = compute_grid_find_matches(&grid, &visible, "al");
        // Visible-row indices: 0 → grid row 0 (alice), 1 → grid row 2 (alex).
        assert_eq!(matches, vec![(0, 0), (1, 0)]);
    }

    #[test]
    fn compute_grid_find_matches_empty_needle_returns_empty() {
        let grid = Grid {
            columns: vec!["x".into()],
            rows: vec![vec!["any".into()]],
            truncated: false,
        };
        assert!(compute_grid_find_matches(&grid, &[0], "").is_empty());
    }

    #[test]
    fn grid_find_f_key_opens_mode_and_jumps_on_match() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid = Grid {
            columns: vec!["name".into(), "city".into()],
            rows: vec![
                vec!["a".into(), "London".into()],
                vec!["b".into(), "Berlin".into()],
            ],
            truncated: false,
        };
        a.grid_visible_rows = vec![0, 1];
        a.grid_state.select(Some(0));
        a.on_key(KeyEvent::from(KeyCode::Char('f')));
        assert_eq!(a.mode, Mode::GridFind);
        // Type "ber" — should jump to row 1 col 1.
        a.on_key(KeyEvent::from(KeyCode::Char('b')));
        a.on_key(KeyEvent::from(KeyCode::Char('e')));
        a.on_key(KeyEvent::from(KeyCode::Char('r')));
        assert_eq!(a.grid_state.selected(), Some(1));
        assert_eq!(a.grid_col_cursor, 1);
        // Enter accepts.
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn grid_find_n_and_capital_n_step_through_matches() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.grid = Grid {
            columns: vec!["c".into()],
            rows: vec![vec!["aa".into()], vec!["bb".into()], vec!["aa".into()]],
            truncated: false,
        };
        a.grid_visible_rows = vec![0, 1, 2];
        a.grid_state.select(Some(0));
        a.on_key(KeyEvent::from(KeyCode::Char('f')));
        // Type "a" — two matches (rows 0 and 2). Cursor jumps to first.
        a.on_key(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(a.grid_state.selected(), Some(0));
        // `n` cycles to second match.
        a.on_key(KeyEvent::from(KeyCode::Char('n')));
        assert_eq!(a.grid_state.selected(), Some(2));
        // `n` again wraps to first.
        a.on_key(KeyEvent::from(KeyCode::Char('n')));
        assert_eq!(a.grid_state.selected(), Some(0));
        // `N` (capital) walks back.
        a.on_key(KeyEvent::from(KeyCode::Char('N')));
        assert_eq!(a.grid_state.selected(), Some(2));
    }

    #[test]
    fn f2_after_failure_opens_error_detail_with_rich_fields() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.last_error = Some("duplicate key value violates unique constraint".into());
        a.last_error_detail = Some(crate::conn::QueryErrDetail {
            code: Some("23505".into()),
            severity: Some("ERROR".into()),
            constraint: Some("users_email_key".into()),
            table: Some("users".into()),
            schema: Some("public".into()),
            ..Default::default()
        });
        a.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::ErrorDetail);
        // Close → back to Normal.
        a.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn f2_with_no_error_surfaces_status_not_overlay() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        // No last_error / last_error_detail.
        a.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Editor);
        assert_eq!(a.last_status.as_deref(), Some("no error to expand"));
    }

    #[test]
    fn query_ok_clears_last_error_detail() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.last_error_detail = Some(crate::conn::QueryErrDetail {
            code: Some("23505".into()),
            ..Default::default()
        });
        a.on_msg(AppMsg::QueryOk {
            generation: a.generation,
            grid: crate::grid::Grid {
                columns: vec!["x".into()],
                rows: vec![],
                truncated: false,
            },
            kind_label: "SELECT".into(),
            tx_open_after: false,
        });
        assert!(a.last_error_detail.is_none());
    }

    #[test]
    fn query_ok_status_flags_truncated_grids() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        let rows: Vec<Vec<String>> = (0..crate::grid::MAX_ROWS)
            .map(|i| vec![i.to_string()])
            .collect();
        a.on_msg(AppMsg::QueryOk {
            generation: a.generation,
            grid: crate::grid::Grid {
                columns: vec!["id".into()],
                rows,
                truncated: true,
            },
            kind_label: "SELECT".into(),
            tx_open_after: false,
        });
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains(&format!("capped at {}", crate::grid::MAX_ROWS)),
            "expected truncation hint in status, got: {status}"
        );
        assert!(a.grid.truncated);
    }

    #[test]
    fn query_ok_status_omits_cap_when_not_truncated() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.on_msg(AppMsg::QueryOk {
            generation: a.generation,
            grid: crate::grid::Grid {
                columns: vec!["id".into()],
                rows: vec![vec!["1".into()]],
                truncated: false,
            },
            kind_label: "SELECT".into(),
            tx_open_after: false,
        });
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(!status.contains("capped"), "unexpected cap hint: {status}");
    }

    #[test]
    fn backslash_d_with_target_opens_browser_with_filter() {
        let mut a = app_with_schemas();
        a.mode = Mode::Editor;
        a.editor_buffer = "\\d users".into();
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::from(KeyCode::F(5)));
        assert_eq!(a.mode, Mode::SchemaBrowser);
        assert_eq!(a.schema_browser_filter.as_deref(), Some("users"));
        // Buffer cleared so a second F5 doesn't re-fire.
        assert!(a.editor_buffer.is_empty());
    }

    #[test]
    fn backslash_d_without_target_just_opens_browser() {
        let mut a = app_with_schemas();
        a.mode = Mode::Editor;
        a.editor_buffer = "\\d".into();
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::from(KeyCode::F(5)));
        assert_eq!(a.mode, Mode::SchemaBrowser);
        assert!(a.schema_browser_filter.is_none());
    }

    #[test]
    fn backslash_help_routes_to_help_overlay() {
        let mut a = app_with_schemas();
        a.mode = Mode::Editor;
        a.editor_buffer = "\\?".into();
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::from(KeyCode::F(5)));
        assert_eq!(a.mode, Mode::Help);
        // The Editor section is the active anchor since we came
        // from Editor.
        assert_eq!(a.help_origin, Some(Mode::Editor));
    }

    #[test]
    fn backslash_quit_sets_should_quit() {
        let mut a = app_with_schemas();
        a.mode = Mode::Editor;
        a.editor_buffer = "\\q".into();
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::from(KeyCode::F(5)));
        assert!(a.should_quit);
    }

    #[test]
    fn backslash_timing_toggles_state() {
        let mut a = app_with_schemas();
        a.mode = Mode::Editor;
        a.editor_buffer = "\\timing".into();
        a.editor_cursor = a.editor_buffer.len();
        assert!(!a.timing_on);
        a.on_key(KeyEvent::from(KeyCode::F(5)));
        assert!(a.timing_on);
        // Buffer preserved (operator commonly toggles back).
        assert_eq!(a.editor_buffer, "\\timing");
        // Toggle again → off.
        a.on_key(KeyEvent::from(KeyCode::F(5)));
        assert!(!a.timing_on);
    }

    #[test]
    fn backslash_unknown_surfaces_actionable_error() {
        let mut a = app_with_schemas();
        a.mode = Mode::Editor;
        a.editor_buffer = "\\xyz".into();
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::from(KeyCode::F(5)));
        let err = a.last_error.as_deref().unwrap_or("");
        assert!(err.contains("unknown backslash command"));
        // Stay in Editor — no useful destination to route to.
        assert_eq!(a.mode, Mode::Editor);
    }

    #[test]
    fn backslash_report_writes_markdown_to_explicit_path() {
        let mut a = app_with_schemas();
        let tmp = std::env::temp_dir().join(format!("pgman-report-test-{}.md", std::process::id()));
        a.mode = Mode::Editor;
        a.editor_buffer = format!("\\report {}", tmp.display());
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::from(KeyCode::F(5)));
        let contents = std::fs::read_to_string(&tmp).expect("report written");
        assert!(
            contents.starts_with("# pgman report"),
            "got: {contents:.120}"
        );
        assert!(contents.contains("## Schema lint findings"));
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("wrote report to"),
            "expected status flash; got: {status}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn backslash_report_writes_html_when_extension_matches() {
        let mut a = app_with_schemas();
        let tmp =
            std::env::temp_dir().join(format!("pgman-report-test-{}.html", std::process::id()));
        a.mode = Mode::Editor;
        a.editor_buffer = format!("\\report {}", tmp.display());
        a.editor_cursor = a.editor_buffer.len();
        a.on_key(KeyEvent::from(KeyCode::F(5)));
        let contents = std::fs::read_to_string(&tmp).expect("html report written");
        assert!(contents.starts_with("<!doctype html>"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn format_unix_secs_utc_pins_the_epoch_anchor() {
        // 1970-01-01T00:00:00Z
        assert_eq!(format_unix_secs_utc(0), "1970-01-01T00:00:00Z");
        // 2000-01-01T00:00:00Z = 946684800
        assert_eq!(format_unix_secs_utc(946_684_800), "2000-01-01T00:00:00Z");
        // 2023-11-14T22:13:20Z = 1700000000 (a common
        // fixture timestamp).
        assert_eq!(format_unix_secs_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn format_unix_secs_utc_handles_leap_year() {
        // 2024-02-29T00:00:00Z = 1709164800
        assert_eq!(format_unix_secs_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn live_lint_loaded_merges_into_findings_and_resorts() {
        // Open the lint panel with a pre-populated (Medium)
        // finding, then deliver a successful LiveLintLoaded with
        // a High LINT101. The merged list must sort with the new
        // High entry first.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::SchemaLint;
        a.schema_lint_findings = vec![crate::query::lint::Finding {
            severity: crate::query::lint::Severity::Medium,
            code: "LINT002",
            title: "mixed-case".into(),
            object: "public.Foo".into(),
            detail: "…".into(),
            suggestion: None,
        }];
        a.on_msg(AppMsg::LiveLintLoaded {
            generation: a.generation,
            result: Ok(vec![crate::query::lint::fk_without_index_finding(
                "public",
                "orders",
                "orders_user_id_fkey",
                "user_id",
            )]),
        });
        assert_eq!(a.schema_lint_findings.len(), 2);
        // High entry now first.
        assert_eq!(a.schema_lint_findings[0].code, "LINT101");
        // Status reflects the count + live delta.
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("live: +1"),
            "expected live-merge status; got: {status}"
        );
    }

    #[test]
    fn live_lint_loaded_after_panel_closed_is_dropped_silently() {
        // The operator opens the panel, then immediately closes
        // it. The async live-fetch completes after the close —
        // we must not mutate findings or status in that case.
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal; // not on the lint panel
        a.schema_lint_findings.clear();
        a.on_msg(AppMsg::LiveLintLoaded {
            generation: a.generation,
            result: Ok(vec![crate::query::lint::fk_without_index_finding(
                "public", "t", "fk", "user_id",
            )]),
        });
        // Findings untouched.
        assert!(a.schema_lint_findings.is_empty());
    }

    #[test]
    fn live_lint_failure_surfaces_status_keeps_pure_findings() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::SchemaLint;
        let pure = crate::query::lint::Finding {
            severity: crate::query::lint::Severity::High,
            code: "LINT001",
            title: "missing PK".into(),
            object: "public.events".into(),
            detail: "…".into(),
            suggestion: None,
        };
        a.schema_lint_findings = vec![pure.clone()];
        a.on_msg(AppMsg::LiveLintLoaded {
            generation: a.generation,
            result: Err("LINT101: permission denied for pg_constraint".into()),
        });
        // Pure findings still there.
        assert_eq!(a.schema_lint_findings.len(), 1);
        assert_eq!(a.schema_lint_findings[0].code, pure.code);
        // Status surfaces the failure.
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("live check failed"),
            "expected failure status; got: {status}"
        );
    }

    #[test]
    fn schema_lint_jk_navigation_clamps_to_findings() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        let mut cache = crate::query::schema::SchemaCache::default();
        cache.schemas = vec!["public".into()];
        cache.tables = vec![
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "a".into(),
            },
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "b".into(),
            },
        ];
        a.schema_cache = cache;
        a.start_schema_lint();
        let n = a.schema_lint_findings.len();
        assert!(n >= 2);
        for _ in 0..(n * 2) {
            a.on_key(KeyEvent::from(KeyCode::Char('j')));
        }
        assert_eq!(a.schema_lint_cursor, n - 1);
    }

    #[test]
    fn schema_browser_close_with_accepted_filter_clears_for_next_open() {
        // Accept a filter via Enter, then close the browser. The
        // filter must NOT survive across opens — the next `S` should
        // show the full tree again.
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        a.on_key(KeyEvent::from(KeyCode::Char('/')));
        a.on_key(KeyEvent::from(KeyCode::Char('a')));
        a.on_key(KeyEvent::from(KeyCode::Char('u')));
        a.on_key(KeyEvent::from(KeyCode::Enter)); // accept filter
        assert_eq!(a.schema_browser_filter.as_deref(), Some("au"));
        // Now close the browser via Esc from SchemaBrowser mode.
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Normal);
        assert!(
            a.schema_browser_filter.is_none(),
            "filter should be cleared on browser close"
        );
    }

    #[test]
    fn schema_browser_collapse_clamps_cursor_inside_visible() {
        let mut a = app_with_schemas();
        a.mode = Mode::SchemaBrowser;
        // Expand public so we have 4 visible rows; focus the last one.
        a.schema_browser_expanded.insert("public".into());
        a.schema_browser_cursor = 3;
        // Collapse public (focused on "public" row at index 1 won't
        // collapse if we're focused on a Table — move focus first).
        a.schema_browser_cursor = 1;
        a.on_key(KeyEvent::from(KeyCode::Enter));
        // After collapse, only the 2 schema rows remain. Cursor must
        // be inside [0, 1], not the stale 3.
        let rows = a.flattened_schema_browser();
        assert!(a.schema_browser_cursor < rows.len());
    }

    #[test]
    fn schema_browser_table_row_carries_column_and_constraint_counts() {
        let mut a = app_with_schemas();
        a.schema_cache.constraints = vec![
            crate::query::schema::ConstraintMeta {
                schema: "public".into(),
                table: "users".into(),
                name: "users_pkey".into(),
            },
            crate::query::schema::ConstraintMeta {
                schema: "public".into(),
                table: "users".into(),
                name: "users_email_uk".into(),
            },
        ];
        a.schema_browser_expanded.insert("public".into());
        let rows = a.flattened_schema_browser();
        let users = rows
            .iter()
            .find(|r| matches!(r, SchemaBrowserRow::Table { name, .. } if name == "users"))
            .expect("users row");
        match users {
            SchemaBrowserRow::Table {
                column_count,
                constraint_count,
                ..
            } => {
                assert_eq!(*column_count, 2);
                assert_eq!(*constraint_count, 2);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn start_schema_browser_with_empty_cache_surfaces_hint() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Normal;
        a.start_schema_browser();
        assert_eq!(a.mode, Mode::Normal);
        assert!(a
            .last_status
            .as_deref()
            .unwrap_or("")
            .contains("schema cache empty"));
    }

    fn explain_app_with_plan() -> App {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
        a.mode = Mode::Normal;
        a.start_connection_change();
        assert_eq!(a.mode, Mode::ConnPick);
        assert_eq!(a.data_source_pick_index, 0);
    }

    #[test]
    fn start_connection_change_with_no_picks_surfaces_hint() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn cancel_running_query_dispatches_through_injected_handler() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
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
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        // No history.
        a.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(a.mode, Mode::Editor);
        assert!(a.history_search.is_none());
        assert_eq!(a.last_status.as_deref(), Some("history is empty"));
    }

    #[test]
    fn query_failed_with_position_past_buffer_clamps_to_end() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.editor_buffer = "SELECT 1".into();
        a.editor_cursor = 0;
        a.generation = 1;
        let _ = a.msg_tx.send(AppMsg::QueryFailed {
            generation: 1,
            error: "boom".into(),
            position: Some(999),
            detail: None,
        });
        if let Some(rx) = a.msg_rx.as_mut() {
            if let Ok(msg) = rx.try_recv() {
                a.on_msg(msg);
            }
        }
        assert_eq!(a.editor_cursor, a.editor_buffer.len());
    }
}
