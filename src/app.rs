//! Application state and the event loop.

mod cmd;
mod editor;
mod keys;
pub mod msg;
mod spawn;
pub use crate::app::msg::AppMsg;
use crate::conn::{self, Dsn};
use crate::grid::Grid;
use crate::query::complete::{self as complete_q, Candidate};
use crate::query::schema::SchemaCache;
use crate::query::{self, reconstruct::ReconstructedQuery};
use crate::safety::{self, Decision, Guard, SafetyConfig};
use crate::theme::Theme;
use crate::tui::{Tui, TuiHost};
#[cfg(test)]
use editor::*;

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

/// Tap-monitor navigation state — the active sub-view, sort, and the
/// per-view cursors. Grouped so the cursors reset together (see
/// `reset_cursors`) instead of being hand-listed at each call site.
#[derive(Debug, Default)]
pub struct TapNavState {
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
impl TapNavState {
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
    /// Tap-monitor navigation state — active sub-view, sort, and the
    /// per-view cursors (including the cursor into `tap_events`).
    pub tap_nav: TapNavState,
    /// Liveness + backpressure-loss tracker fed by tap
    /// heartbeats. Lets the chrome badge distinguish "JAR
    /// connected, no traffic" from "JAR gone."
    pub tap_health: TapHealth,
    /// Captured hotspots snapshot for the baseline-diff view.
    /// `None` until the operator presses `B`.
    pub tap_baseline: Option<TapBaseline>,
    /// Persisted saved queries — loaded at startup, written back
    /// on quit (and on save / delete during the session).
    pub saved_queries: crate::saved::SavedQueries,
    /// Modal/interaction state for the saved-queries panel.
    pub saved_ui: SavedQueriesUi,
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
    /// Schema-browser navigation/modal state (cursor, filter, expanded set).
    pub schema_browser: SchemaBrowserUi,
    /// Findings produced by `query::lint::run_all` over the
    /// current schema cache. Rebuilt on entry to `Mode::SchemaLint`
    /// (cheap — pure pass over the cache).
    pub schema_lint_findings: Vec<crate::query::lint::Finding>,
    /// Cursor into `schema_lint_findings`.
    pub schema_lint_cursor: usize,
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
            tap_nav: TapNavState::default(),
            tap_health: TapHealth::default(),
            tap_baseline: None,
            saved_queries: crate::saved::SavedQueries::default(),
            saved_ui: SavedQueriesUi::default(),
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
            schema_browser: SchemaBrowserUi::default(),
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
                    self.tap_nav.events_cursor = self.tap_nav.events_cursor.saturating_sub(1);
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

    /// Clear the tap event ring (`c` from any tap view) and re-home
    /// every per-view cursor. One ring backs all views, so a clear must
    /// reset all cursors — hand-maintained per-view copies had drifted,
    /// leaving stale cursors after a clear. The captured baseline
    /// snapshot is intentionally preserved (only the live ring is wiped).
    fn clear_tap_ring(&mut self) {
        let n = self.tap_events.len();
        self.tap_events.clear();
        self.tap_nav.reset_cursors();
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
        self.tap_nav.baseline_cursor = 0;
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
        self.tap_nav.view = self.tap_nav.view.next();
        match self.tap_nav.view {
            TapView::List => {
                self.last_status = Some(format!(
                    "tap view · list ({} event(s))",
                    self.tap_events.len()
                ));
            }
            TapView::Hotspots => {
                self.tap_nav.hotspots_cursor = 0;
                self.last_status = Some(format!(
                    "tap view · hotspots · sort: {}",
                    self.tap_nav.sort.label()
                ));
            }
            TapView::Callers => {
                self.tap_nav.callers_cursor = 0;
                self.last_status = Some(format!(
                    "tap view · callers · sort: {}",
                    self.tap_nav.sort.label()
                ));
            }
            TapView::Transactions => {
                self.tap_nav.txns_cursor = 0;
                let txns = self.current_txns();
                let open = txns.iter().filter(|t| t.is_open()).count();
                self.last_status = Some(format!(
                    "tap view · transactions · {} total · {} open",
                    txns.len(),
                    open
                ));
            }
            TapView::Pools => {
                self.tap_nav.pools_cursor = 0;
                let pools = self.current_pools();
                self.last_status = Some(format!("tap view · pools · {} pool(s)", pools.len()));
            }
            TapView::NplusOne => {
                self.tap_nav.nplus1_cursor = 0;
                let findings = self.current_nplus1();
                self.last_status = Some(format!("tap view · N+1 · {} finding(s)", findings.len()));
            }
            TapView::Baseline => {
                self.tap_nav.baseline_cursor = 0;
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
        crate::tap::group_hotspots(self.tap_events.iter(), self.tap_nav.sort)
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
        crate::tap::group_by_caller(self.tap_events.iter(), self.tap_nav.sort)
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
        self.saved_ui.save_name = default_query_name(&self.editor_buffer);
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
        self.saved_ui.filter = None;
        self.saved_ui.cursor = self
            .saved_ui
            .cursor
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
        self.saved_ui.param_prompt = Some(ParamPrompt {
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
            self.saved_ui.filter.as_ref().map(|t| t.text()),
        )
    }

    /// The real `entries` index under the panel cursor, mapped
    /// through the current filter. `None` when nothing matches.
    fn focused_saved_index(&self) -> Option<usize> {
        self.visible_saved_indices()
            .get(self.saved_ui.cursor)
            .copied()
    }

    fn start_saved_queries_filter(&mut self) {
        self.saved_ui.filter = Some(TextInput::new());
        self.saved_ui.cursor = 0;
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
        self.saved_ui.rename_from = name.clone();
        self.saved_ui.rename_buf = TextInput::with_text(name);
        self.mode = Mode::RenameQueryPrompt;
        self.last_status = Some("rename · edit name · enter save · esc cancel".into());
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

    fn cancel_running_query(&mut self) {
        let Some(dispatcher) = self.cancel_dispatcher.as_ref() else {
            return;
        };
        self.last_status = Some("cancelling query…".to_string());
        dispatcher.dispatch();
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
        let filter = self.schema_browser.filter.as_deref().unwrap_or("");
        let expanded_owned: std::collections::HashSet<String>;
        let expanded_ref: &std::collections::HashSet<String> = if filter.is_empty() {
            &self.schema_browser.expanded
        } else {
            let mut s = self.schema_browser.expanded.clone();
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
        self.schema_browser.cursor = 0;
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
        self.schema_browser.filter = Some(String::new());
        self.schema_browser.cursor = 0;
        self.last_status = Some("filter: /  · type to narrow · enter accept · esc clear".into());
        self.mode = Mode::SchemaBrowserFilter;
    }

    fn refresh_schema_browser_filter_status(&mut self) {
        let pat = self.schema_browser.filter.as_deref().unwrap_or("");
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
        match rows.get(self.schema_browser.cursor)? {
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
mod tests;
