//! Application state and the event loop.

mod cmd;
mod editor;
mod handle;
mod history;
mod keys;
pub mod msg;
mod spawn;
mod tabs;
mod types;
mod yank;
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
pub use types::*;

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

/// The footer line for a statement the per-database guard refuses.
/// Names the statement in words (`DELETE without WHERE`), never the
/// enum, and says where the guard lives.
pub fn blocked_by_safety_message(kind: &safety::StatementKind, db: &str) -> String {
    format!(
        "blocked by safety: {} on '{db}' · change the guard in safety.toml to allow it",
        kind.describe()
    )
}

/// Parse `BOOTSTRAP_SQL`'s result grid (`database`, `size` columns) into
/// the start card's per-database summary. Positional, not name-keyed —
/// `BOOTSTRAP_SQL` is the only producer and always emits `(name, size)`
/// in that order. Rows with fewer than two columns are skipped rather
/// than panicking; `BOOTSTRAP_SQL` never emits one, but this stays total.
fn parse_bootstrap_databases(grid: &Grid) -> Vec<DatabaseInfo> {
    grid.rows
        .iter()
        .filter_map(|row| {
            let name = row.first()?.clone();
            let size = row.get(1)?.clone();
            Some(DatabaseInfo { name, size })
        })
        .collect()
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

/// Hard cap on tab count. 9 matches `Ctrl-1..Ctrl-9` numeric
/// jumps; the operator can still close one to open another.
pub const TAB_CAP: usize = 9;

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

/// Key used in the `expanded` set for a table row.
pub fn schema_browser_table_key(schema: &str, table: &str) -> String {
    format!("{schema}.{table}")
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

/// The splash's minimum hold: long enough that the elephant isn't a
/// flash, short enough that it isn't a wait. Startup sets `splash_until`
/// to `Instant::now() + SPLASH_MIN`; nothing holds the splash past this
/// deadline regardless of connection state or landing mode.
pub const SPLASH_MIN: Duration = Duration::from_millis(600);

/// Pure decision: should the splash dismiss at `now`? `until` is
/// the absolute deadline set at App start (`SPLASH_MIN` after
/// launch). `conn_resolved` reflects whether the connection is no
/// longer in the Connecting state (any other state lets us drop the
/// splash early so a fast failure isn't hidden behind the elephant).
/// `is_picker` reflects whether the landing mode is the connection
/// picker (`Mode::ConnPick`) — there, `conn_resolved` never fires
/// (no connect attempt is made until a pick is confirmed, so
/// `conn_state` sits at `Disconnected` indefinitely), and the picker
/// is what the operator actually needs to see, so it must not wait
/// beyond the minimum either.
///
/// ```
/// use std::time::{Duration, Instant};
/// use pgman::app::{splash_should_dismiss, SPLASH_MIN};
/// let t0 = Instant::now();
/// let until = Some(t0 + SPLASH_MIN);
/// // Invisible → never dismisses (nothing to do).
/// assert!(!splash_should_dismiss(false, until, false, false, t0));
/// // Past deadline → dismiss.
/// assert!(splash_should_dismiss(true, until, false, false, t0 + SPLASH_MIN));
/// // Connection resolved before deadline → dismiss anyway.
/// assert!(splash_should_dismiss(true, until, true, false, t0));
/// // Landing on the picker → dismiss anyway, even while Disconnected.
/// assert!(splash_should_dismiss(true, until, false, true, t0));
/// // Still connecting, not the picker, before deadline → hold.
/// assert!(!splash_should_dismiss(true, until, false, false, t0));
/// ```
pub fn splash_should_dismiss(
    visible: bool,
    until: Option<std::time::Instant>,
    conn_resolved: bool,
    is_picker: bool,
    now: std::time::Instant,
) -> bool {
    if !visible {
        return false;
    }
    match until {
        Some(deadline) => now >= deadline || conn_resolved || is_picker,
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
    /// Deadline set at startup, `SPLASH_MIN` after launch: `tick_splash`
    /// never holds the splash past it. A fast-resolving connection or a
    /// picker landing (see [`splash_should_dismiss`]) can dismiss the
    /// splash earlier than this; a keypress dismisses it immediately,
    /// bypassing this deadline entirely.
    pub splash_until: Option<Instant>,
    pub anim_tick: usize,
    pub generation: u64,
    pub should_quit: bool,

    /// SQL editor state (buffer, cursor, scroll, undo/redo). Grouped so
    /// tab snapshot / restore is a single struct clone.
    pub editor: EditorState,
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
    /// Log-import pick state (reconstructed queries, view, clusters, cursor).
    pub log_pick: LogPickUi,
    /// A short status line shown in the footer after a run (e.g. "EXPLAIN ok").
    pub last_status: Option<String>,
    /// A query / safety error to surface to the user.
    pub last_error: Option<String>,
    /// True while a query is in flight (drives the spinner).
    pub query_running: bool,
    /// Connection-picker state (candidate data sources + selected index).
    pub conn_pick: ConnPickUi,

    /// Help-overlay state (scroll, origin mode, max scroll).
    pub help: HelpUi,
    /// Modes the operator has already entered this session. Used by
    /// `note_mode_entry` to flash a one-time "key hint" status the
    /// first time each mode opens — discoverability nudge without
    /// becoming nagware on repeat visits.
    pub mode_seen: std::collections::HashSet<Mode>,
    /// psql `\timing` toggle — when on, the QueryOk handler
    /// appends an elapsed-ms marker to the status footer.
    pub timing_on: bool,
    /// psql `\x` toggle — when on, the QueryOk handler opens the
    /// existing row-detail view (`Mode::RowDetail`) for the first
    /// row of a new result instead of leaving it in the grid.
    pub expanded_on: bool,
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
    /// Set by `spawn_run` when the statement(s) about to run change the
    /// schema (CREATE / ALTER / DROP). On a successful `QueryOk` this
    /// triggers a background `schema::fetch` so completion / browser /
    /// lint / FK-nav don't go stale until the next reconnect. Taken
    /// (reset) when consumed.
    pub schema_dirty_after_run: bool,
    /// Memoised editor syntax-highlight spans, keyed on the buffer text they
    /// were computed for. The lex + schema-resolving classify is otherwise
    /// re-run every frame (≈9fps during any animation) for an unchanged
    /// buffer; this caches it across frames. Invalidated by a buffer edit
    /// (key mismatch) or a schema-cache change (cleared on Booted /
    /// SchemaRefreshed).
    pub editor_highlight_cache: Option<(String, Vec<crate::query::highlight::Span>)>,
    /// Memoised "does the whole buffer look like a log" verdict, keyed on
    /// the buffer text it was computed for — same shape and reasoning as
    /// `editor_highlight_cache`. Read (and refreshed on a key mismatch) by
    /// the editor block title; recomputing `logdetect::detect_log` from
    /// scratch every render frame would rescan the whole buffer at ≈9fps.
    pub editor_log_kind_cache: Option<(String, Option<crate::query::logdetect::LogKind>)>,
    /// Notifications panel state (ring buffer + cursor).
    pub notifications: NotificationsUi,
    /// Ring buffer of recent JDBC-tap events (queries +
    /// txn boundaries from the pgman-tap JAR). Newest at the
    /// end. Heartbeat events don't land here — they update
    /// `tap_health` instead. Capped at `TAP_CAP`.
    pub tap_events: std::collections::VecDeque<crate::tap::TapEvent>,
    /// Tap-monitor navigation state — active sub-view, sort, and the
    /// per-view cursors (including the cursor into `tap_events`).
    pub tap_nav: TapNavUi,
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
    /// Row-detail modal state (scroll / clamp + focused field).
    pub row_detail: RowDetailUi,
    /// Per-cell zoom (`Mode::CellDetail`) state (scroll / clamp + JSON tree).
    pub cell_detail: CellDetailUi,

    /// Snapshot of the database catalog used by Tab-completion in the
    /// editor. Refilled on every successful `Booted`. Empty before
    /// connect (or after a failed catalog fetch).
    pub schema_cache: SchemaCache,
    /// Every database on the server + its on-disk size, from the
    /// bootstrap query. Refilled on every successful `Booted`; empty
    /// before connect. App-level (shared across tabs), and rendered
    /// by the start card's `databases` line rather than the results
    /// grid — see `parse_bootstrap_databases`.
    pub databases: Vec<DatabaseInfo>,
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
    /// Grid view-metadata for the live result grid: cursor column,
    /// sort, raw rows, filter, visible-row indices, and row source.
    /// Shared shape with `TabSnapshot` (snapshotted per tab).
    pub grid_view: GridView,
    /// Grid find ("/" search) state: needle, match positions, and the
    /// current match cursor. Live-only — not persisted per tab.
    pub grid_find: GridFind,
    /// EXPLAIN-tree state (plan, cursor, collapsed node paths).
    pub explain: ExplainUi,
    /// Schema-browser navigation/modal state (cursor, filter, expanded set).
    pub schema_browser: SchemaBrowserUi,
    /// Schema-lint panel state (findings + cursor).
    pub schema_lint: SchemaLintUi,
    /// Slow-queries panel state (snapshot + cursor).
    pub slow_queries: SlowQueriesUi,
    /// Sessions panel state (snapshot + cursor).
    pub sessions: SessionsUi,
    /// SQL of the most recent successful `Run` query, kept so the
    /// grid post-load step can re-parse it to infer the single
    /// source table (when there is one). Not set for batch /
    /// EXPLAIN / EXPLAIN ANALYZE runs.
    pub last_run_sql: Option<String>,
    /// Result-diff state (pinned baseline, active diff, cursor).
    pub result_diff: ResultDiffUi,

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

    /// Whether an update check may run this session at all.
    /// Defaults to `false` — `App::new` alone (as every test harness
    /// and `--demo` use it) must never make a real network call.
    /// `main.rs` opts the real interactive run in explicitly, unless
    /// `--no-update-check` or `PGMAN_NO_UPDATE_CHECK` says otherwise;
    /// `--batch` never reaches the run loop at all.
    pub update_check_enabled: bool,
    /// Result of the crates.io check, once it lands. `None` until
    /// then, and stays `None` when there's no newer release —
    /// `update_check_done` is what distinguishes "no update" from
    /// "haven't checked yet".
    pub update_available: Option<crate::update_check::LatestRelease>,
    /// Set once `AppMsg::UpdateCheck` has landed, so the About
    /// overlay can say "up to date" instead of staying silent.
    pub update_check_done: bool,
    /// Injectable spawn hook for the update check. `None` (the
    /// production default) spawns a real `tokio::spawn` that awaits
    /// `update_check::check_async()` and reports the result back via
    /// `msg_tx`. Tests substitute a synchronous recorder so
    /// `run_with` can prove the check is spawned strictly after the
    /// first draw without touching the network.
    update_check_spawn: Option<Box<dyn Fn(UnboundedSender<AppMsg>) + Send + Sync>>,
    /// Set once the update check has been spawned for this run, so a
    /// later loop iteration doesn't fire it again.
    update_check_spawned: bool,
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
        // the "lovely discovery" surface. Splash shows first, but never
        // beyond `SPLASH_MIN` (`splash_until` sets that deadline); a picker
        // landing dismisses it straight away — `conn_state` stays
        // `Disconnected` there (no connect attempt is made until a pick is
        // confirmed), so the fast-resolve early-dismiss branch would
        // otherwise never fire and the operator would sit through the full
        // minimum before reaching the surface they actually need.
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
            splash_until: Some(Instant::now() + SPLASH_MIN),
            anim_tick: 0,
            generation: 0,
            should_quit: false,
            editor: EditorState::default(),
            history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            pending_run: None,
            tx_open: false,
            log_pick: LogPickUi::default(),
            last_status: None,
            last_error: None,
            query_running: false,
            help: HelpUi::default(),
            mode_seen: std::collections::HashSet::new(),
            timing_on: false,
            expanded_on: false,
            last_error_detail: None,
            pending_terminate: None,
            auto_refresh: false,
            auto_refresh_last: None,
            bookmarks: std::collections::HashMap::new(),
            schema_dirty_after_run: false,
            editor_highlight_cache: None,
            editor_log_kind_cache: None,
            notifications: NotificationsUi::default(),
            tap_events: std::collections::VecDeque::new(),
            tap_nav: TapNavUi::default(),
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
            row_detail: RowDetailUi::default(),
            cell_detail: CellDetailUi::default(),
            schema_cache: SchemaCache::default(),
            databases: Vec::new(),
            completion: None,
            history_search: None,
            watch: None,
            notices: Vec::new(),
            external_edit_pending: false,
            draft_last_save: None,
            draft_dirty: false,
            grid_view: GridView::default(),
            grid_find: GridFind::default(),
            explain: ExplainUi::default(),
            schema_browser: SchemaBrowserUi::default(),
            schema_lint: SchemaLintUi::default(),
            slow_queries: SlowQueriesUi::default(),
            sessions: SessionsUi::default(),
            last_run_sql: None,
            result_diff: ResultDiffUi::default(),
            conn_pick: ConnPickUi {
                picks: data_source_picks,
                index: 0,
            },
            client: None,
            cancel_dispatcher: None,
            tunnel: None,
            safety_config,
            read_only,
            statement_timeout_ms,
            msg_tx,
            msg_rx: Some(msg_rx),
            update_check_enabled: false,
            update_available: None,
            update_check_done: false,
            update_check_spawn: None,
            update_check_spawned: false,
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
            // Never block the first frame on a network round-trip:
            // the update check is spawned as its own task right
            // after the FIRST draw lands, and only once per session
            // (`update_check_spawned` guards every later iteration).
            if self.update_check_enabled && !self.update_check_spawned {
                self.update_check_spawned = true;
                self.spawn_update_check();
            }
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
                let _ = persist_draft(&self.editor.buffer);
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
            if let Err(e) = persist_draft(&self.editor.buffer) {
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
        let result = external_edit_via(&self.editor.buffer, &editor_cmd);
        // Resume the TUI even if the editor errored — leaving the
        // operator stuck in a half-suspended terminal would be much
        // worse than a slightly delayed error message.
        let resume_err = tui.resume().err();
        match result {
            Ok(text) => {
                self.editor.buffer = text;
                self.editor.cursor = self.editor.buffer.len();
                self.editor.preferred_col = None;
                self.history_pos = None;
                self.draft_dirty = true;
                self.last_status = Some(format!(
                    "loaded {} char(s) from $EDITOR",
                    self.editor.buffer.len()
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

    /// Auto-dismiss the splash as soon as any of: its `SPLASH_MIN`
    /// minimum has elapsed; the connection resolves (Connected or
    /// Failed) — otherwise a fast failure / fast bootstrap would be
    /// hidden behind the elephant for the full minimum; or the
    /// landing mode is the connection picker, which the operator
    /// needs to see and which never resolves a connection on its own
    /// (`conn_state` sits at `Disconnected` there, so the
    /// connection-resolved branch alone would never fire). Cheap to
    /// call every loop iteration — a single `Instant::now`.
    fn tick_splash(&mut self) {
        if !self.splash_visible {
            return;
        }
        let connection_resolved = matches!(
            self.conn_state,
            ConnState::Connected { .. } | ConnState::Failed(_)
        );
        let is_picker = matches!(self.mode, Mode::ConnPick);
        if splash_should_dismiss(
            self.splash_visible,
            self.splash_until,
            connection_resolved,
            is_picker,
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
        self.notifications.cursor = self
            .notifications
            .cursor
            .min(self.notifications.items.len().saturating_sub(1));
        self.last_status = Some(format!(
            "NOTIFY arrivals · {} stashed · LISTEN <chan> from the editor to subscribe",
            self.notifications.items.len()
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

    /// `Ctrl-S` in editor — prompt for a name (with the existing
    /// saved entry's name pre-filled if the buffer matches one
    /// — otherwise empty). Enter persists; Esc cancels.
    fn start_save_query_prompt(&mut self) {
        if self.editor.buffer.trim().is_empty() {
            self.last_status = Some("editor empty — nothing to save".into());
            return;
        }
        // Default name: derive from the first ~40 chars of the
        // buffer with a non-identifier sanitisation, so the
        // operator has a starting point. They can backspace and
        // type their own.
        self.saved_ui.save_name = default_query_name(&self.editor.buffer);
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
        self.editor.buffer = sql;
        self.editor.cursor = self.editor.buffer.len();
        self.editor.preferred_col = None;
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
        self.row_detail.scroll = 0;
        self.row_detail.field = 0;
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
        let Some(value) = row.get(self.row_detail.field) else {
            return;
        };
        self.cell_detail.scroll = 0;
        self.cell_detail.json_rows.clear();
        self.cell_detail.json_cursor = 0;
        self.cell_detail.json_collapsed.clear();
        if let Some(parsed) = crate::query::json_cell::parse_jsonb_cell(value) {
            self.cell_detail.json_rows =
                crate::query::json_cell::flatten(&parsed, &self.cell_detail.json_collapsed);
            // Stash the parsed value so collapse/expand can re-flatten.
            self.cell_detail.json_value = Some(parsed);
        } else {
            self.cell_detail.json_value = None;
        }
        self.mode = Mode::CellDetail;
    }

    /// Toggle expand/collapse of the focused JSON node. Scalars are
    /// a no-op. Re-flattens the row list and clamps the cursor to
    /// remain on the same path (or, if the path vanished because a
    /// parent collapsed, on its parent's row).
    fn toggle_json_cell_node(&mut self) {
        let Some(row) = self
            .cell_detail
            .json_rows
            .get(self.cell_detail.json_cursor)
            .cloned()
        else {
            return;
        };
        if !matches!(
            row.display,
            crate::query::json_cell::JsonDisplay::Container { .. }
        ) {
            return;
        }
        if self.cell_detail.json_collapsed.contains(&row.path) {
            self.cell_detail.json_collapsed.remove(&row.path);
        } else {
            self.cell_detail.json_collapsed.insert(row.path.clone());
        }
        if let Some(v) = &self.cell_detail.json_value {
            self.cell_detail.json_rows =
                crate::query::json_cell::flatten(v, &self.cell_detail.json_collapsed);
        }
        // Try to keep the cursor on the same path; fall back to the
        // tail if the row list shrank past it.
        let new_idx = self
            .cell_detail
            .json_rows
            .iter()
            .position(|r| r.path == row.path)
            .unwrap_or_else(|| self.cell_detail.json_rows.len().saturating_sub(1));
        self.cell_detail.json_cursor = new_idx;
    }

    /// Open the help overlay from `from`. Captures `from` so the
    /// close path restores that mode (instead of always going to
    /// Normal), and pre-scrolls the help body to the section that
    /// matches `from` — operators see the relevant keys without
    /// hunting for them.
    pub fn open_help_from(&mut self, from: Mode) {
        self.help.origin = Some(from);
        self.help.scroll = 0; // Renderer-side anchor pass will adjust.
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
        let Some(id) = complete_q::extract_identifier(&self.editor.buffer, self.editor.cursor)
        else {
            return;
        };
        // Empty prefix is fine — the candidate set falls back to "all
        // identifier-shaped candidates for the surrounding clause"
        // (matches the Tab-on-whitespace UX). The cycle drops naturally
        // when those produce no matches.
        let cands =
            complete_q::candidates_for(&self.editor.buffer, self.editor.cursor, &self.schema_cache);
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
        let prefix_start = self.editor.cursor.saturating_sub(id.prefix.len());
        let cand_count = cands.len();
        self.completion = Some(CompletionCycle {
            start: prefix_start,
            end: self.editor.cursor,
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
        if let Some(last) = self.editor.undo.last_mut() {
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
                self.editor.redo.clear();
                return;
            }
        }
        self.editor.redo.clear();
        self.editor.undo.push(UndoEntry {
            buffer,
            cursor,
            kind,
            merge_window_end: now,
        });
        if self.editor.undo.len() > UNDO_CAP {
            // Drop the oldest. `Vec::remove(0)` is O(N) but N is
            // small (UNDO_CAP) and undos are rare keys.
            self.editor.undo.remove(0);
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

    /// Preload the editor buffer with `text` and immediately run the log
    /// importer — the same path F8 / ctrl-l take from `Mode::Editor`. Backs
    /// `--log PATH` (`src/main.rs`): reconstruction needs no database, so
    /// this runs regardless of whether the connection has resolved yet,
    /// and lands pgman straight in `Mode::LogPick` once the splash clears.
    /// If nothing is found, leaves the log in the buffer in `Mode::Editor`
    /// with `start_log_import`'s "no queries found" error — same result as
    /// pasting the text and pressing F8 by hand. Either way this overrides
    /// whatever startup mode `App::new` picked (e.g. `Mode::ConnPick` for
    /// multiple detected data sources) — an explicit `--log` wins.
    pub fn preload_log(&mut self, text: &str) {
        self.editor.buffer = text.to_string();
        self.editor.cursor = self.editor.buffer.len();
        self.mode = Mode::Editor;
        self.start_log_import();
    }

    /// F8 in editor mode — parse the editor buffer through `hibernate::parse`
    /// and `pglog::parse`, then enter `Mode::LogPick` if anything was found.
    fn start_log_import(&mut self) {
        let log = &self.editor.buffer;
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
        self.log_pick.clusters = crate::query::nplus1::detect(&picks);
        self.log_pick.picks = picks;
        self.log_pick.view = LogPickView::AllQueries;
        self.log_pick.index = 0;
        self.mode = Mode::LogPick;
    }

    /// Number of rows the LogPick popup is currently rendering.
    /// Folds the view choice (all queries vs. cluster summary).
    pub fn log_pick_visible_len(&self) -> usize {
        match self.log_pick.view {
            LogPickView::AllQueries => self.log_pick.picks.len(),
            LogPickView::Clusters => self.log_pick.clusters.len(),
        }
    }

    /// Toggle the LogPick view between all-queries and cluster
    /// summary. Resets the cursor to row 0 so a stale index from
    /// the previous view doesn't render out-of-range.
    fn toggle_log_pick_view(&mut self) {
        self.log_pick.view = match self.log_pick.view {
            LogPickView::AllQueries => LogPickView::Clusters,
            LogPickView::Clusters => LogPickView::AllQueries,
        };
        self.log_pick.index = 0;
        self.last_status = Some(match self.log_pick.view {
            LogPickView::AllQueries => format!("all queries · {}", self.log_pick.picks.len()),
            LogPickView::Clusters => format!(
                "N+1 clusters · {} (of {} queries)",
                self.log_pick.clusters.len(),
                self.log_pick.picks.len()
            ),
        });
    }

    /// Resolve the focused row's runnable SQL — `runnable_sql` for
    /// the AllQueries view, the cluster's `example` for Clusters.
    fn focused_log_pick_sql(&self) -> Option<String> {
        match self.log_pick.view {
            LogPickView::AllQueries => self
                .log_pick
                .picks
                .get(self.log_pick.index)
                .map(|q| q.runnable_sql.clone()),
            LogPickView::Clusters => self
                .log_pick
                .clusters
                .get(self.log_pick.index)
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
        if self.editor.buffer.trim().is_empty() {
            self.last_status = Some("nothing to format".into());
            return;
        }
        match pg_format_via(&self.editor.buffer, "pg_format") {
            Ok(formatted) => {
                let chars = formatted.len();
                self.editor.buffer = formatted;
                self.editor.cursor = self.editor.buffer.len();
                self.editor.preferred_col = None;
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
        let sql = self.editor.buffer.trim().to_string();
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
        let saved_buffer = std::mem::replace(&mut self.editor.buffer, sql);
        let saved_cursor = self.editor.cursor;
        self.editor.cursor = self.editor.buffer.len();
        self.request_run(RunKind::Run);
        self.editor.buffer = saved_buffer;
        self.editor.cursor = saved_cursor.min(self.editor.buffer.len());
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
            lint_findings: self.schema_lint.findings.clone(),
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

    /// "not connected", plus the one thing the operator can do about
    /// it from where they are.
    pub fn not_connected_message(&self) -> String {
        if matches!(self.conn_state, ConnState::Failed(_)) {
            "not connected · r to retry · c to choose a connection".to_string()
        } else if !self.conn_pick.picks.is_empty() {
            "not connected · c to choose a connection".to_string()
        } else {
            "not connected · start pgman with --dsn postgres://… or inside a Spring project"
                .to_string()
        }
    }

    fn request_run(&mut self, kind: RunKind) {
        let sql = self.editor.buffer.trim().to_string();
        if sql.is_empty() {
            self.last_error =
                Some("editor is empty · e to focus it, then type SQL or paste a log".to_string());
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
        // `--demo` never has a real client — but a statement typed or
        // pasted there should still go through the exact same guard,
        // batch-split, and pending-confirm machinery a live session
        // uses (a DELETE without WHERE is refused exactly like it
        // would be against a real database — see `spawn_run_demo`).
        // Only the final execution differs: demo answers from the
        // fixture schema instead of a Postgres connection.
        if self.client.is_none() && !self.demo {
            self.last_error = Some(self.not_connected_message());
            return;
        }
        let demo_no_client = self.demo && self.client.is_none();

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
            if demo_no_client {
                self.spawn_run_demo(sql, kind);
            } else {
                self.spawn_run(sql, kind, decision, false);
            }
            return;
        }

        // Run / EXPLAIN ANALYZE both execute (ANALYZE on DML is wrapped in a
        // rollback transaction inside `spawn_run`).
        match decision.guard {
            Guard::Block => {
                self.last_error = Some(blocked_by_safety_message(&decision.kind, db));
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
                if demo_no_client {
                    self.spawn_run_demo(sql, kind);
                    return;
                }
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
            Guard::Allow => {
                if self.demo && self.client.is_none() {
                    self.spawn_run_demo(sql, kind);
                } else {
                    self.spawn_run(sql, kind, synthesized, true);
                }
            }
        }
    }

    /// Load the editor buffer as a path to a DBUnit FlatXmlDataSet, replace
    /// the buffer with the generated clean+insert script. The user reviews,
    /// then runs via Ctrl-R (which takes the multi-statement batch path).
    fn load_dbunit_fixture(&mut self) {
        use crate::dbunit;
        let path_str = self.editor.buffer.trim().to_string();
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
        self.editor.buffer = dbunit::generate_apply_script(&fixture, clean_mode);
        self.editor.cursor = 0;
        self.editor.preferred_col = None;
        self.history_pos = None;
        self.last_error = None;
        self.last_status = Some(format!(
            "fixture loaded · {row_count} row(s), {table_count} table(s) · ctrl-r to apply"
        ));
    }

    /// Recompute `grid_view.visible_rows` against the current `grid.rows`
    /// and `grid_view.filter`. Filter is a case-insensitive substring
    /// match across every column of each row. With no filter the
    /// visible set is just `0..rows.len()` (kept materialised so the
    /// render path has one code path).
    fn rebuild_visible_rows(&mut self) {
        self.grid_view.visible_rows =
            compute_visible_rows(&self.grid.rows, self.grid_view.filter.as_deref());
        // Keep the selection in range.
        let visible = self.grid_view.visible_rows.len();
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
        let col = self.grid_view.col_cursor.min(self.grid.columns.len() - 1);
        let next = next_sort_state(self.grid_view.sort, col);
        match next {
            Some((col, asc)) => {
                if self.grid_view.raw_rows.is_none() {
                    self.grid_view.raw_rows = Some(self.grid.rows.clone());
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
                self.grid_view.sort = Some((col, asc));
                let dir = if asc { "ASC" } else { "DESC" };
                self.last_status = Some(format!("sorted by {} {dir}", self.grid.columns[col]));
            }
            None => {
                if let Some(raw) = self.grid_view.raw_rows.take() {
                    self.grid.rows = raw;
                }
                self.grid_view.sort = None;
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
        let cur = self.grid_view.col_cursor as isize;
        let next = (cur + delta).clamp(0, n as isize - 1);
        self.grid_view.col_cursor = next as usize;
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
        let Some((schema, table)) = self.grid_view.source.clone() else {
            self.last_error =
                Some("FK navigation needs a single-table SELECT for source inference".into());
            return;
        };
        let Some(col_name) = self.grid.columns.get(self.grid_view.col_cursor).cloned() else {
            self.last_error = Some("no focused column".into());
            return;
        };
        let Some(idx) = self.selected_grid_row_idx() else {
            return;
        };
        let Some(row) = self.grid.rows.get(idx) else {
            return;
        };
        let Some(value) = row.get(self.grid_view.col_cursor).cloned() else {
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
        self.editor.buffer = sql;
        self.editor.cursor = self.editor.buffer.len();
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
        match &self.grid_view.source {
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
        match self.result_diff.pinned.take() {
            None => {
                let n = self.grid.rows.len();
                self.result_diff.pinned = Some(PinnedResult {
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
                self.result_diff.active = Some(ResultDiffState {
                    a: a.clone(),
                    b_columns,
                    b_rows,
                    b_label,
                    key,
                    diff,
                });
                self.result_diff.pinned = Some(a);
                self.result_diff.cursor = 0;
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
            rows: Vec::with_capacity(self.grid_view.visible_rows.len()),
            truncated: false,
        };
        for &i in &self.grid_view.visible_rows {
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
        self.grid_view.filter = Some(String::new());
        self.rebuild_visible_rows();
        self.mode = Mode::GridFilter;
        self.refresh_filter_status();
    }

    /// `n` / `N` in Normal — step the row cursor to the next / prev
    /// matching row (only meaningful while a filter is active).
    fn filter_step(&mut self, forward: bool) {
        if self.grid_view.filter.is_none() {
            // Make the no-op visible — vim muscle memory expects
            // `n` to do something useful, and silent failure feels
            // like the terminal is stuck.
            self.last_status = Some("no active filter (press `/` to start one)".into());
            return;
        }
        // visible_rows is already the filtered set in display order;
        // step the existing cursor through it.
        let visible = self.grid_view.visible_rows.len();
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
        let pat = self.grid_view.filter.as_deref().unwrap_or("");
        let n = self.grid_view.visible_rows.len();
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
        let Some(plan) = self.explain.plan.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut path = Vec::new();
        flatten_plan(plan, &mut path, 0, &self.explain.collapsed, &mut out);
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
        self.schema_lint.findings = findings;
        self.schema_lint.cursor = 0;
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

    /// `T` from Normal — load + open the slow-queries panel.
    fn start_slow_queries(&mut self) {
        let Some(client) = self.client.clone() else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        self.slow_queries.cursor = 0;
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
            self.last_error = Some(self.not_connected_message());
            return;
        };
        self.sessions.cursor = 0;
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
        let Some(row) = self.sessions.rows.get(self.sessions.cursor) else {
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
        let target_visible = self
            .grid_view
            .visible_rows
            .iter()
            .position(|&i| i == bm.row);
        let visible_idx = match target_visible {
            Some(i) => i,
            None => {
                self.last_status = Some("bookmark row not visible in current filter".into());
                return;
            }
        };
        self.grid_state.select(Some(visible_idx));
        let last_col = self.grid.columns.len().saturating_sub(1);
        self.grid_view.col_cursor = bm.col.min(last_col);
    }

    /// `f` from Normal — open the grid-find input. Re-uses the
    /// existing visible_rows / grid_cursor; matches are computed
    /// across them, NOT across hidden filtered-out rows.
    fn start_find(&mut self) {
        if self.grid.rows.is_empty() {
            self.last_status = Some("nothing to find · grid is empty".into());
            return;
        }
        self.grid_find.needle = Some(String::new());
        self.grid_find.matches.clear();
        self.grid_find.pos = 0;
        self.last_status =
            Some("find:    · type to search · n/N jump · enter accept · esc cancel".into());
        self.mode = Mode::GridFind;
    }

    fn refresh_grid_find_status(&mut self) {
        let pat = self.grid_find.needle.as_deref().unwrap_or("");
        let n = self.grid_find.matches.len();
        if pat.is_empty() {
            self.last_status = Some("find:    · type to search · enter accept · esc cancel".into());
            return;
        }
        let pos = if n == 0 { 0 } else { self.grid_find.pos + 1 };
        self.last_status = Some(format!(
            "find: {pat}  · {pos}/{n} match · n/N jump · enter accept · esc cancel"
        ));
    }

    /// Recompute the match list and jump the cursor to the first
    /// match. Called on every keystroke while the find input is
    /// live.
    fn rebuild_grid_find(&mut self) {
        let pat = self.grid_find.needle.clone().unwrap_or_default();
        self.grid_find.matches =
            compute_grid_find_matches(&self.grid, &self.grid_view.visible_rows, &pat);
        self.grid_find.pos = 0;
        if let Some(&(vi, ci)) = self.grid_find.matches.first() {
            self.grid_state.select(Some(vi));
            self.grid_view.col_cursor = ci;
        }
    }

    /// Step to the next / previous match (wrapping). No-op when
    /// no matches.
    fn step_grid_find(&mut self, forward: bool) {
        if self.grid_find.matches.is_empty() {
            return;
        }
        let n = self.grid_find.matches.len();
        self.grid_find.pos = if forward {
            (self.grid_find.pos + 1) % n
        } else {
            (self.grid_find.pos + n - 1) % n
        };
        let (vi, ci) = self.grid_find.matches[self.grid_find.pos];
        self.grid_state.select(Some(vi));
        self.grid_view.col_cursor = ci;
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
    crate::util::write_private(path, &text)
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

/// Write the buffer atomically (via `crate::util::write_private`, so
/// it also lands owner-only) on quit. Empty buffers still get written
/// so a deliberate Ctrl-U + quit clears the saved draft.
pub(crate) fn persist_draft(buf: &str) -> std::io::Result<()> {
    persist_draft_to(&draft_path(), buf)
}

/// Path-parameterised core of [`persist_draft`]. Same atomic-rename
/// guarantee — a crash mid-write leaves either the old file intact
/// or the new file complete, never a truncated half-write.
pub fn persist_draft_to(path: &std::path::Path, buf: &str) -> std::io::Result<()> {
    crate::util::write_private(path, buf)
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
                    // `EXPLAIN (FORMAT JSON) ` is 22 chars; positions
                    // ≤ that point inside the wrapper itself, so drop
                    // them.
                    e.position = e.position.and_then(|p| p.checked_sub(22));
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

impl App {
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
}

#[cfg(test)]
mod tests;
