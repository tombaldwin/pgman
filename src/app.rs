//! Application state and the event loop.

mod cmd;
mod edit_cell;
mod editor;
mod handle;
mod history;
mod keys;
pub mod msg;
mod spawn;
mod tabs;
mod types;
mod yank;
pub use crate::app::msg::{AppMsg, PageOutcome, RunPaging};
use crate::conn::{self, Dsn};
use crate::grid::{cell_text, Cell, Grid};
use crate::inventory::patching::RunMode;
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
pub(crate) use crate::conn::BOOTSTRAP_SQL;

/// The footer line for a statement the per-database guard refuses.
/// Names the statement in words (`DELETE without WHERE`), never the
/// enum, and says where the guard lives.
///
/// One refusal is not a category guard at all: a statement that would turn
/// the session's read-only property off is refused because the profile said
/// read-only, so pointing at the `other` guard would send the operator to the
/// wrong line of `safety.toml`.
pub fn blocked_by_safety_message(decision: &Decision, db: &str) -> String {
    if decision.read_only_escape {
        return read_only_escape_refusal(safety_toml_exists());
    }
    if decision.server_control {
        return safety::SERVER_CONTROL_REFUSAL.to_string();
    }
    format!(
        "blocked by safety: {} on '{db}' · change the guard in safety.toml to allow it",
        decision.kind.describe()
    )
}

/// The refusal for turning read-only off from inside the session
/// (`:readonly off`, `SET default_transaction_read_only = off`, …).
/// With a `safety.toml` on disk the profile is the operator's own and
/// the file is where to change it ([`safety::READ_ONLY_ESCAPE_REFUSAL`]).
/// Without one, read-only is the built-in default — blaming a file
/// that does not exist sent the operator looking for it; say instead
/// how to get one ([`crate::conn::READ_ONLY_DEFAULT_HINT`], the same
/// line the server-side 25006 refusal carries). Pure / testable.
/// The escape refused in a session that is read-only by more than its
/// profile, which is writable: `:readonly on` turned it on, and
/// `:readonly off` turns it off ([`crate::app::cmd`] names a replica's
/// pin instead).
pub const SESSION_READ_ONLY_ESCAPE_REFUSAL: &str =
    "this session is read-only by :readonly on · :readonly off to write, not a SET";

pub fn read_only_escape_refusal(safety_toml_exists: bool) -> String {
    if safety_toml_exists {
        safety::READ_ONLY_ESCAPE_REFUSAL.to_string()
    } else {
        crate::conn::READ_ONLY_DEFAULT_HINT.to_string()
    }
}

/// `:readonly off` refused by the profile: where the setting lives and
/// what to change — the personal `safety.toml` (or the built-in default
/// without one), this project's `.pgman/pgman.toml`, or both, as the
/// merge recorded (`SafetyConfig::read_only_origin`). Both files are
/// read at start, so the next step ends in a restart. Pure.
pub fn readonly_off_refusal(
    cfg: &safety::SafetyConfig,
    db: &str,
    safety_toml_exists: bool,
) -> String {
    let origin = cfg.read_only_origin(db);
    let personal = || {
        match (safety_toml_exists, origin.personal_own_entry) {
        (false, _) => crate::conn::READ_ONLY_DEFAULT_HINT.to_string(),
        (true, true) => format!(
            "{db} is read-only in safety.toml ([databases.{db}]) · set read_only = false there and restart pgman to write"
        ),
        (true, false) => format!(
            "read-only is safety.toml's [default] · add [databases.{db}] with read_only = false and restart pgman to write"
        ),
    }
    };
    let Some(block) = origin.project.as_deref() else {
        return personal();
    };
    if !origin.personal {
        return format!(
            "{db} is read-only in this project's .pgman/pgman.toml ({block}) · set read_only = false there and restart pgman to write"
        );
    }
    let mine = match (safety_toml_exists, origin.personal_own_entry) {
        (false, _) => "by default (no safety.toml)".to_string(),
        (true, true) => format!("in safety.toml ([databases.{db}])"),
        (true, false) => "in safety.toml's [default]".to_string(),
    };
    format!(
        "{db} is read-only {mine} and in this project's .pgman/pgman.toml ({block}) · both need read_only = false, then restart pgman"
    )
}

/// The status line when the query Enter loaded from the log picker
/// still has `?` / `$N` placeholders: the log carried no bound values
/// for it (Hibernate's bind logging is `TRACE`, usually off), so what
/// landed in the editor is the template, not a runnable statement.
pub const LOG_PICK_UNBOUND_STATUS: &str =
    "no bound values in the log for this query — ? placeholders remain";

/// The sessions panel's title / footer line when the only backend on
/// the server is pgman's own (the panel SQL excludes it): `0 total · 0
/// blocked` read as a failed load on a quiet server.
pub const SESSIONS_ONLY_THIS_SESSION: &str = "sessions · only this session";

/// The sessions panel's status line for `total` listed backends, of
/// which `blocked` are waiting on a lock. Pure / testable.
pub fn sessions_status(total: usize, blocked: usize) -> String {
    if total == 0 {
        SESSIONS_ONLY_THIS_SESSION.to_string()
    } else {
        format!("sessions · {total} total · {blocked} blocked")
    }
}

/// The status line while a statement runs. It hides the editor's
/// `ctrl-c cancel running query` hint, so it carries the key itself;
/// a plain run is just "running" (it read `running run…`). Pure.
pub fn running_status(kind: RunKind) -> String {
    match kind {
        RunKind::Run => "running… · ctrl-c cancels".to_string(),
        other => format!("running {}… · ctrl-c cancels", other.label()),
    }
}

/// `c` with nothing to pick from. Started with `--dsn`, telling the
/// operator to pass `--dsn` was no help: `:connect` takes another one.
/// Pure.
pub fn no_picks_hint(have_dsn: bool) -> &'static str {
    if have_dsn {
        "only the --dsn connection here · :connect postgres://… opens another; [[connections]] in pgman.toml lists more for c"
    } else {
        "no data sources to pick — pass --dsn, :connect postgres://…, or add `[[connections]]` to pgman.toml"
    }
}

/// What a reload the operator asked for found. Pure / testable.
pub fn schema_reloaded_status(cache: &crate::query::schema::SchemaCache) -> String {
    if let Some(why) = &cache.incomplete {
        return format!("schema reloaded, still incomplete · {why}");
    }
    match cache.tables.len() {
        0 => "schema reloaded — still no tables this role can see".to_string(),
        n => format!(
            "schema reloaded · {n} table(s) in {} schema(s)",
            cache.schemas.len()
        ),
    }
}

/// The status a connect lands with when its schema fetch came back
/// incomplete ([`crate::query::schema::SchemaCache::incomplete`]):
/// connected, what is missing and why, and the reload that fetches it
/// again. Pure.
pub fn schema_incomplete_status(why: &str) -> String {
    format!("connected, schema incomplete · {why} · S then r reloads it")
}

/// The environments panel's status line. Pure / testable.
pub fn environments_status(envs: usize, dbs: usize, failed: usize) -> String {
    if envs == 0 {
        return "environments · none configured".to_string();
    }
    let mut s = format!(
        "{} · {}",
        crate::ui::count_label(envs as u64, "environment", "environments"),
        crate::ui::count_label(dbs as u64, "database", "databases")
    );
    if failed > 0 {
        s.push_str(&format!(" · {failed} failed"));
    }
    s
}

/// A panel: a view the operator opens over the grid / editor and
/// closes again — as opposed to the two base modes, a decision modal
/// that owns the keyboard, a text prompt, or the error-detail overlay.
/// Opening or closing one drops the footer's last error
/// (`App::on_key`). Every full-screen picker / browser / monitor
/// belongs here; a new one that is left out keeps the previous
/// error under it.
fn is_panel(mode: Mode) -> bool {
    matches!(
        mode,
        Mode::Help
            | Mode::About
            | Mode::LogPick
            | Mode::ConnPick
            | Mode::RowDetail
            | Mode::CellDetail
            | Mode::ExplainTree
            | Mode::SchemaBrowser
            | Mode::SlowQueries
            | Mode::Sessions
            | Mode::Environments
            | Mode::SchemaLint
            | Mode::Notifications
            | Mode::TapMonitor
            | Mode::SavedQueries
            | Mode::ResultDiff
    )
}

/// Is there a `safety.toml` in the config dir? Decides which
/// [`read_only_escape_refusal`] to show; checked at message time
/// because the profile itself does not record where it came from.
fn safety_toml_exists() -> bool {
    existing_safety_toml().is_some()
}

/// The personal `safety.toml`, when there is one.
pub(crate) fn existing_safety_toml() -> Option<std::path::PathBuf> {
    let path = crate::util::config_file("safety.toml");
    path.exists().then_some(path)
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
            let name = cell_text(row.first()?).to_string();
            let size = crate::query::schema::format_size_cell(cell_text(row.get(1)?));
            // No size means no CONNECT (RDS's `rdsadmin`): not one to
            // offer on the start card.
            (!size.is_empty()).then_some(DatabaseInfo { name, size })
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
/// The first connection-critical field of `dsn` still carrying a
/// literal `${…}`, as `(field name, placeholder body)`. `None` for a
/// clean DSN, or for no DSN at all.
///
/// Only the field *name* and the placeholder *body* come back — never
/// the field's value — so a caller can put this straight into an error
/// message without a password reaching the log or the UI.
///
/// Pure. The last line of defence behind each discovery source's own
/// placeholder marking (`DataSourcePick::unresolved` /
/// `unresolved_host`): an unmarked `${…}` is not inert — as a hostname
/// it becomes a DNS lookup to whatever the resolver makes of the
/// literal text, and as a password it goes on the wire. Shared by the
/// TUI's `App::refuse_if_unresolved` and `--batch`'s
/// `batch_dsn_from_picks`, so the two can't drift.
pub fn dsn_placeholder_field(dsn: Option<&conn::Dsn>) -> Option<(&'static str, String)> {
    let dsn = dsn?;
    let mut fields: Vec<(&'static str, &str)> =
        vec![("host", &dsn.host), ("database name", &dsn.dbname)];
    if let Some(u) = dsn.user.as_deref() {
        fields.push(("username", u));
    }
    if let Some(p) = dsn.password.as_deref() {
        fields.push(("password", p));
    }
    if let Some(t) = dsn.ssh_tunnel.as_ref() {
        fields.push(("ssh tunnel host", &t.host));
        if let Some(u) = t.user.as_deref() {
            fields.push(("ssh tunnel user", u));
        }
    }
    for (name, value) in fields {
        if let Some(body) = crate::creds::spring::placeholder_bodies(value)
            .into_iter()
            .next()
        {
            return Some((name, body));
        }
    }
    for (k, v) in &dsn.params {
        for text in [k.as_str(), v.as_str()] {
            if let Some(body) = crate::creds::spring::placeholder_bodies(text)
                .into_iter()
                .next()
            {
                return Some(("url parameters", body));
            }
        }
    }
    None
}

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
/// original order. `None` pattern, or an empty one, means
/// "everything". A NULL cell matches only the whole word `null`
/// ([`cell_matches`]).
///
/// ```
/// use pgman::app::compute_visible_rows;
/// let rows = vec![
///     vec![Some("1".into()), Some("alice".into())],
///     vec![Some("2".into()), Some("BOB".into())],
/// ];
/// assert_eq!(compute_visible_rows(&rows, None), vec![0, 1]);
/// assert_eq!(compute_visible_rows(&rows, Some("bo")), vec![1]); // case-insens
/// ```
pub fn compute_visible_rows(rows: &[Vec<Cell>], pattern: Option<&str>) -> Vec<usize> {
    match pattern {
        None | Some("") => (0..rows.len()).collect(),
        Some(pat) => {
            let needle = pat.to_ascii_lowercase();
            (0..rows.len())
                .filter(|i| rows[*i].iter().any(|c| cell_matches(c, &needle)))
                .collect()
        }
    }
}

/// Does `cell` match the filter / find `needle` (already lowercased)?
/// A value matches when it contains the needle, case-insensitively. A
/// NULL has no text to contain anything, but the grid draws it as
/// `NULL`, so it matches the whole word `null` (any case) — and only
/// that: `/nu`, looking for `number`, must not light up every row
/// with a NULL in it. Pure / testable.
pub fn cell_matches(cell: &Cell, needle: &str) -> bool {
    match cell {
        Some(text) => text.to_ascii_lowercase().contains(needle),
        None => needle == "null",
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
/// The match after (or before) `here`, wrapping; `None` when there are
/// none. `matches` is in row-major order. Pure.
pub fn next_match(
    matches: &[(usize, usize)],
    here: (usize, usize),
    forward: bool,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(if forward {
        matches.iter().position(|m| *m > here).unwrap_or(0)
    } else {
        matches
            .iter()
            .rposition(|m| *m < here)
            .unwrap_or(matches.len() - 1)
    })
}

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
            if cell_matches(cell, &needle) {
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

/// Expand-state key for a table's folded partitions group.
pub fn schema_browser_partitions_key(schema: &str, table: &str) -> String {
    format!("{schema}.{table}#partitions")
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
            SchemaBrowserRow::Index { name, .. } => name.as_str(),
            SchemaBrowserRow::Partitions { .. } => "",
            SchemaBrowserRow::Partition { name, .. } => name.as_str(),
        };
        !name.is_empty() && name.to_ascii_lowercase().contains(&pat)
    };
    let depth = |row: &SchemaBrowserRow| -> usize {
        match row {
            SchemaBrowserRow::Schema { .. } => 0,
            SchemaBrowserRow::Table { .. } => 1,
            SchemaBrowserRow::Column { .. }
            | SchemaBrowserRow::Constraint { .. }
            | SchemaBrowserRow::Index { .. }
            | SchemaBrowserRow::Partitions { .. } => 2,
            SchemaBrowserRow::Partition { .. } => 3,
        }
    };
    let n = rows.len();
    let mut keep = vec![false; n];
    for i in 0..n {
        if matches_self(&rows[i]) {
            keep[i] = true;
            // A matched table (or anything under one) keeps what is
            // under it: `/orders` showed the `orders` row with only
            // its `orders_pkey` constraint, every column filtered
            // away. A matched schema does not — the filter expands
            // everything, and `/public` would be the whole database.
            let d = depth(&rows[i]);
            if d >= 1 {
                let mut j = i + 1;
                while j < n && depth(&rows[j]) > d {
                    keep[j] = true;
                    j += 1;
                }
            }
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
    let mut out: Vec<SchemaBrowserRow> = rows
        .into_iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(r, _)| r)
        .collect();
    // A schema's table count is what the filter left under it: the
    // detail pane said `7 table(s)` over the 3 on screen.
    for i in 0..out.len() {
        if !matches!(out[i], SchemaBrowserRow::Schema { .. }) {
            continue;
        }
        let shown = out[i + 1..]
            .iter()
            .take_while(|r| !matches!(r, SchemaBrowserRow::Schema { .. }))
            .filter(|r| matches!(r, SchemaBrowserRow::Table { .. }))
            .count();
        if let SchemaBrowserRow::Schema { table_count, .. } = &mut out[i] {
            *table_count = shown;
        }
    }
    out
}

/// Pure: walk the cache and emit visible rows in display order
/// (schemas alphabetical; under each expanded schema, its tables
/// alphabetical; under each expanded table, its columns in catalog
/// order, then its unique/PK constraints alphabetical, then its
/// indexes alphabetical).
/// `expanded` is keyed by schema name and `"schema.table"` for
/// tables — see `schema_browser_table_key`. Empty `expanded` →
/// every node collapsed.
pub fn flatten_schema_browser(
    cache: &crate::query::schema::SchemaCache,
    expanded: &std::collections::HashSet<String>,
) -> Vec<SchemaBrowserRow> {
    // Partitions fold under the table they belong to (the top of the
    // tree for sub-partitions) instead of listing beside it.
    let mut partitions: std::collections::HashMap<(String, String), Vec<(String, String)>> =
        std::collections::HashMap::new();
    for (s, n) in cache.partition_parent.keys() {
        if let Some(root) = cache.partition_root(s, n) {
            partitions
                .entry(root)
                .or_default()
                .push((s.clone(), n.clone()));
        }
    }
    for v in partitions.values_mut() {
        v.sort();
    }
    let mut by_schema: std::collections::HashMap<&str, Vec<&str>> =
        std::collections::HashMap::new();
    for t in cache.tables.iter().filter(|t| {
        !cache
            .partition_parent
            .contains_key(&(t.schema.clone(), t.name.clone()))
    }) {
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
            // A unique or primary-key constraint's own index has the
            // constraint's name; it would list twice. The table's detail
            // pane still lists every index.
            let indexes = cache
                .indexes_by_table
                .get(&(s.to_string(), t.to_string()))
                .map(|v| v.as_slice())
                .unwrap_or(&[])
                .iter()
                .filter(|ix| !constraints.contains(&ix.name.as_str()));
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
            for ix in indexes {
                out.push(SchemaBrowserRow::Index {
                    schema: s.to_string(),
                    table: t.to_string(),
                    name: ix.name.clone(),
                });
            }
            if let Some(parts) = partitions.get(&(s.to_string(), t.to_string())) {
                let group_expanded = expanded.contains(&schema_browser_partitions_key(s, t));
                out.push(SchemaBrowserRow::Partitions {
                    schema: s.to_string(),
                    table: t.to_string(),
                    count: parts.len(),
                    expanded: group_expanded,
                });
                if group_expanded {
                    for (ps, pn) in parts {
                        out.push(SchemaBrowserRow::Partition {
                            schema: ps.clone(),
                            table: t.to_string(),
                            name: pn.clone(),
                        });
                    }
                }
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

/// Pick the diff key for A against B, and whether it is the primary
/// key. Keyed comparison compares non-key cells positionally, so it
/// needs the same columns on both sides; then, when both were read from
/// the same columns of one table (`origins` equal) whose primary key is
/// all in the result, the key is that primary key
/// (`query::cell_edit::resolve_key`). Otherwise the leftmost column
/// unique on both sides, then the full row. Pure.
pub fn choose_diff_key(
    cache: &SchemaCache,
    a: &PinnedResult,
    b: &PinnedResult,
) -> (crate::query::row_diff::RowKey, bool) {
    use crate::query::row_diff::{infer_key_column, RowKey};
    if a.columns != b.columns {
        return (RowKey::FullRow, false);
    }
    if a.origins == b.origins {
        let key = |p: &PinnedResult| {
            crate::query::cell_edit::resolve_key(
                cache,
                p.source.as_ref(),
                &p.columns,
                &p.origins,
                None,
            )
        };
        if let (Ok(ka), Ok(kb)) = (key(a), key(b)) {
            if ka == kb {
                let cols = ka.pk.iter().map(|(idx, ..)| *idx).collect();
                return (RowKey::Columns(cols), true);
            }
        }
    }
    match infer_key_column(&a.rows, &b.rows, a.columns.len()) {
        Some(col) => (RowKey::Columns(vec![col]), false),
        None => (RowKey::FullRow, false),
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

/// Render a grid cell as a SQL literal: SQL NULL → `NULL`, and every
/// value — the empty string included — a single-quoted string with
/// embedded quotes doubled. NULL and `''` are different values: writing
/// `NULL` for `''` changed the row the yank was copying.
///
/// A value is quoted even when it looks like a number or a boolean. The
/// grid holds text, not types, so it cannot know a column is numeric;
/// a quoted literal is an untyped one, which Postgres reads as the
/// target column's type (`'42'` into an `integer`, `'t'` into a
/// `boolean`). Bare, `007` in a text column came back as `7`, `1e3` as
/// `1000`, `TRUE` as `true`, and `NaN` was read as a column name.
///
/// A value holding a backslash is written as an escape string,
/// `E'…'`, with each backslash doubled: a plain `'…'` means the same
/// value only while `standard_conforming_strings` is on, and with it
/// off a bytea's `\x0102` text read as the escape `\x01`. `E'…'`
/// reads the same under either setting.
pub fn format_sql_literal(cell: Option<&str>) -> String {
    let Some(s) = cell else {
        return "NULL".to_string();
    };
    let mut out = String::with_capacity(s.len() + 3);
    if s.contains('\\') {
        out.push('E');
    }
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
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

/// Postgres's keywords that cannot be a bare column or table name: the
/// "reserved" and "reserved (can be function or type)" rows of the SQL
/// key words appendix. Sorted, for `binary_search`. A column named
/// `order` or `user` is legal, quoted; bare, it is a syntax error.
const RESERVED_KEYWORDS: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "binary",
    "both",
    "case",
    "cast",
    "check",
    "collate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "desc",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "fetch",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "group",
    "having",
    "ilike",
    "in",
    "initially",
    "inner",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "localtime",
    "localtimestamp",
    "natural",
    "not",
    "notnull",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "overlaps",
    "placing",
    "primary",
    "references",
    "returning",
    "right",
    "select",
    "session_user",
    "similar",
    "some",
    "symmetric",
    "system_user",
    "table",
    "tablesample",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "user",
    "using",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
];

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
            || RESERVED_KEYWORDS.binary_search(&name).is_ok()
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

/// How many rows `p` in the schema browser previews.
pub const SAMPLE_ROWS: usize = 5;

/// The schema browser's sample-rows query (`p`): the first
/// [`SAMPLE_ROWS`] rows of `schema.table`, identifiers quoted only
/// where they need it.
pub fn build_sample_rows_sql(schema: &str, table: &str) -> String {
    format!(
        "SELECT * FROM {}.{} LIMIT {SAMPLE_ROWS}",
        quote_ident(schema),
        quote_ident(table),
    )
}

/// `I`'s `INSERT INTO schema.table (cols…) VALUES (literals…);` for one
/// grid row, each cell through [`format_sql_literal`]. Column names are
/// the server's own, case and all, so they go through [`quote_ident`]
/// (`createdAt` bare is `createdat`, a column the table lacks). The
/// schema and table are the server's names too: the FROM parse folds
/// `FROM Users` to `users` and keeps `FROM "Users"` as `Users`, so
/// quoting them names exactly the relation the query read. Bare, a
/// mixed-case `"PgRevMixed"` landed in a lowercase twin `pgrevmixed`.
/// Pure / testable.
pub fn row_insert_sql(schema: &str, table: &str, columns: &[String], row: &[Cell]) -> String {
    let cols = columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let vals = row
        .iter()
        .map(|c| format_sql_literal(c.as_deref()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {}.{} ({cols}) VALUES ({vals});",
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

/// The draft auto-save throttle: at most one write per window.
const DRAFT_SAVE_GAP: Duration = Duration::from_millis(500);

/// Pure: when a dirty draft is next due to be saved — the end of the
/// throttle window after `last_save`, or at once when it has never been
/// saved. The run loop sleeps until then so the save happens even when
/// no key or message arrives to wake it.
///
/// ```
/// use std::time::{Duration, Instant};
/// use pgman::app::draft_save_at;
/// let t0 = Instant::now();
/// let gap = Duration::from_millis(500);
/// assert_eq!(draft_save_at(Some(t0), gap), t0 + gap);
/// assert!(draft_save_at(None, gap) <= Instant::now()); // never saved → now
/// ```
pub fn draft_save_at(last_save: Option<Instant>, min_gap: Duration) -> Instant {
    match last_save {
        Some(t) => t + min_gap,
        None => Instant::now(),
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
    /// `--demo` only: the tap burst still to arrive (see
    /// [`DemoTapFeed`]). `None` everywhere else.
    pub demo_tap_feed: Option<DemoTapFeed>,
    pub conn_state: ConnState,
    /// The reason a `ConnectionLost` gave while this connect was still
    /// in flight: unheeded then (there was no session to drop), it is
    /// what the card says if the `Booted` that lands carries the closed
    /// client. Cleared by each connect.
    pub(crate) lost_while_connecting: Option<String>,
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
    /// Closers (`)` `]` `}` `'` `"`) autoclose inserted into the
    /// editor buffer and the operator has not typed over yet — the
    /// only ones typing the closer skips. See [`editor::AutoClosers`].
    pub auto_closers: editor::AutoClosers,
    /// Indent width and keyword case for Ctrl-F's built-in formatter,
    /// and the indent width auto-indent on Enter uses. From the
    /// project's `[editor]` block (`project::load_editor_options`),
    /// defaults otherwise.
    pub format_opts: query::format::FormatOptions,
    /// The mode F2 was pressed in; `Mode::ErrorDetail` closes back
    /// into it. `None` outside the overlay.
    pub error_detail_return_to: Option<Mode>,
    /// What Tab in the `:` bar had to say — the candidate commands
    /// (` · `-joined) or "no command starts with …" — shown beside the
    /// bar's input. The bar owns the footer while it is open, so a
    /// status line would never be seen; cleared by the next keystroke.
    pub command_bar_completions: Option<String>,
    /// Past run statements, newest at the end.
    pub history: Vec<String>,
    /// Position in `history` while navigating with Ctrl-P/Ctrl-N. `None` =
    /// editing the live draft.
    pub history_pos: Option<usize>,
    /// A guarded run waiting on confirmation.
    pub pending_run: Option<PendingRun>,
    /// `u`'s editor while it is open (`Mode::CellEdit`).
    pub cell_edit: Option<CellEditUi>,
    /// A cell edit inside the `auto_tx` transaction awaiting commit or
    /// rollback: a rollback puts the old value back in the grid.
    pub edit_awaiting_tx: Option<PendingCellEdit>,
    /// True while an explicit transaction is open (auto_tx write succeeded —
    /// waiting on the user to commit or rollback).
    pub tx_open: bool,
    /// Log-import pick state (reconstructed queries, view, clusters, cursor).
    pub log_pick: LogPickUi,
    /// The log picker as it was when last closed (Enter or Esc). Loading
    /// a pick replaces the log in the buffer, so a later Ctrl-L that
    /// finds no log reopens this instead; a new log replaces it.
    pub last_log_pick: Option<LogPickUi>,
    /// A short status line shown in the footer after a run (e.g. "EXPLAIN ok").
    pub last_status: Option<String>,
    /// A query / safety error to surface to the user.
    pub last_error: Option<String>,
    /// True while a query is in flight (drives the spinner).
    pub query_running: bool,
    /// Connection-picker state (candidate data sources + selected index).
    pub conn_pick: ConnPickUi,
    /// A discovered pick that carries an `ssh_tunnel`, waiting on an
    /// explicit `y`. Set by `connect_to_discovered_pick`, consumed by
    /// `on_conn_pick_key`; `Some` means the picker draws the tunnel
    /// confirmation in place of the candidate list.
    pub pending_tunnel: Option<PendingTunnel>,

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
    /// Redaction mode (`--redact`, `:redact`, `redact_default`): every
    /// frame goes through `redact::apply` before it is drawn.
    pub redact: bool,
    /// `redact::env_names()`, read once at startup: redaction's names
    /// from the process environment (`AWS_PROFILE`, the login name).
    pub redact_env_names: Vec<String>,
    /// `environments.toml` exists but didn't load (a parse error, or
    /// unreadable). Set at startup and by each listing, so the
    /// no-connection card can point at `E` instead of looking as if
    /// no environments were configured.
    pub environments_config_broken: bool,
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
    /// Memoised "does the whole buffer look like a log" verdict, keyed
    /// on a [`BufferFingerprint`] of the text it was computed for.
    /// Recomputing `logdetect::detect_log` from scratch every render
    /// frame would rescan the buffer at ≈9fps — but keying it on a
    /// clone of the buffer (as it was) meant every keystroke in a
    /// 300 KiB paste copied 300 KiB. Refreshed through
    /// `App::editor_log_kind`, which both the editor block title and
    /// the completion auto-trigger read.
    pub editor_log_kind_cache:
        Option<(BufferFingerprint, Option<crate::query::logdetect::LogKind>)>,
    /// Notifications panel state (ring buffer + cursor).
    pub notifications: NotificationsUi,
    /// Ring buffer of recent JDBC-tap events (queries +
    /// txn boundaries from the pgman-tap JAR). Newest at the
    /// end. Heartbeat events don't land here — they update
    /// `tap_health` instead. Capped at `TAP_CAP`.
    pub tap_events: std::collections::VecDeque<crate::tap::TapEvent>,
    /// Running `TapEvent::approx_bytes` total of `tap_events`, kept by
    /// `tap::push_bounded` for the `tap::TAP_RING_MAX_BYTES` budget.
    /// Zero means "unseeded" and is recounted on the next push.
    pub tap_ring_bytes: usize,
    /// Bumped on every mutation of `tap_events` (push, eviction,
    /// clear, `--demo` fixture load). Keys `tap_cache`, so a frame
    /// whose ring hasn't moved is a cache hit and one whose ring has
    /// is never served a stale aggregate. Use
    /// [`App::bump_tap_generation`] rather than touching it directly.
    pub tap_generation: u64,
    /// Memoised hotspot / caller / N+1 aggregates over `tap_events`,
    /// keyed by `tap_generation`. Per-`App` on purpose — see the note
    /// in `tap::insights`.
    tap_cache: crate::tap::TapInsightsCache,
    /// Tap-monitor navigation state — active sub-view, sort, and the
    /// per-view cursors (including the cursor into `tap_events`).
    pub tap_nav: TapNavUi,
    /// Liveness + backpressure-loss tracker fed by tap
    /// heartbeats. Lets the chrome badge distinguish "JAR
    /// connected, no traffic" from "JAR gone."
    pub tap_health: TapHealth,
    /// The tap listeners that bound at startup (set by `main.rs`), so
    /// the monitor's setup hint and status say where pgman is really
    /// listening. Empty when no listener bound.
    pub tap_listeners: Vec<crate::tap::TapListener>,
    /// Captured hotspots snapshot for the baseline-diff view.
    /// `None` until the operator presses `B`.
    pub tap_baseline: Option<TapBaseline>,
    /// Persisted saved queries — loaded at startup, written back
    /// on quit (and on save / delete during the session).
    pub saved_queries: crate::saved::SavedQueries,
    /// Where `saved_queries` is written back. `saved_queries_path()`
    /// for a real session; tests inject a temp file so pressing
    /// Ctrl-S / `d` / rename in a test can never touch the operator's
    /// own `saved.toml`. Nothing is written under `--demo` regardless
    /// (see `persist_saved_queries`).
    pub saved_queries_file: std::path::PathBuf,
    /// Where the editor draft and the query history are persisted.
    /// Injectable so a run-loop test never touches the operator's real
    /// files (one did, and left an empty `saved.toml` behind).
    pub draft_file: std::path::PathBuf,
    pub history_file: std::path::PathBuf,
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
    /// Per-tab editor height set by hand, in content lines — see
    /// `TabSnapshot::editor_lines`. `None` is the automatic split.
    pub editor_lines: Option<u16>,
    /// Per-tab `Alt-Z` zoom — see `TabSnapshot::zoomed`.
    pub zoomed: bool,
    /// Body height (rows between the header/tab-bar and the footer)
    /// of the last frame, recorded by `ui::draw`. `Alt-=` / `Alt--`
    /// clamp against it; 0 until the first frame.
    pub body_rows: u16,
    /// `true` after the operator pressed `m` and the next key
    /// is interpreted as the bookmark letter. Cleared after one
    /// dispatch (success or not).
    pub pending_mark_set: bool,
    /// A chord that acts on its second press (Ctrl-C quit, Ctrl-W
    /// close a tab with SQL in it); any other key disarms it.
    pub armed_chord: Option<ArmedChord>,
    /// A reload the operator asked for is in flight: say what landed.
    pub schema_reload_announce: bool,
    /// Ctrl-C sent a cancel for the running statement: the
    /// `57014 query_canceled` that comes back is the answer to it, not
    /// a failure.
    pub cancel_requested: bool,
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
    /// Did the last frame actually draw the completion popup? Enter
    /// accepts a candidate only from a popup the operator could see:
    /// one the renderer skipped (no room, wrong mode) is not a
    /// question on screen, and Enter stays a newline / run. Written by
    /// `ui::draw` every frame.
    pub completion_popup_drawn: bool,
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
    /// The draft text this instance last read from, or wrote to,
    /// `draft_file`. A save only writes when the buffer differs from
    /// it (this window changed the draft), and only to `draft_file`
    /// when the file still holds it (no other window wrote since) —
    /// see [`plan_draft_write`].
    pub draft_synced: String,
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
    pub environments: EnvironmentsUi,
    /// What fills the environments panel; `None` until `main.rs` (or
    /// `--demo`) installs one.
    pub environments_loader: Option<EnvironmentsLoader>,
    /// Reads an environment database's secret at connect; `None` until
    /// `main.rs` installs the aws CLI one.
    pub secret_fetcher: Option<SecretFetcher>,
    /// Environment discovery for the overview's `d`; `None` until
    /// `main.rs` (or `--demo`) installs one.
    pub env_discoverer: Option<EnvDiscoverer>,
    /// Probes environments that opted in; `None` until `main.rs` (or
    /// `--demo`) installs one.
    pub env_prober: Option<EnvProber>,
    /// Where the overview's last-used row is kept between runs; `None`
    /// (the default, and every test) remembers nothing on disk.
    pub environments_state_file: Option<std::path::PathBuf>,
    /// Reads RDS log errors for the overview's `l`; `None` until
    /// `main.rs` installs one.
    pub env_log_fetcher: Option<EnvLogFetcher>,
    /// Plans and applies patches for the overview's `P`.
    pub env_patcher: Option<EnvPatcher>,
    /// SQL of the most recent successful `Run` query, kept so the
    /// grid post-load step can re-parse it to infer the single
    /// source table (when there is one). Not set for batch /
    /// EXPLAIN / EXPLAIN ANALYZE runs.
    pub last_run_sql: Option<String>,
    /// Result-diff state (pinned baseline, active diff, cursor).
    pub result_diff: ResultDiffUi,
    /// `:` command-bar state — `Some` exactly while
    /// `mode == Mode::CommandBar`. Opened by `:` from any
    /// non-typing mode; see `App::dispatch_command`.
    pub command_bar: Option<CommandBarUi>,

    /// Saved working buffer while navigating history (restored on Ctrl-N past
    /// the newest entry).
    history_draft: String,
    /// The live connection. Database work reaches it only through
    /// [`App::db`], which queues the work behind `pager` — see
    /// `the_client_is_reached_only_through_db` in the tests.
    client: Option<Arc<tokio_postgres::Client>>,
    /// The live connection's pager (`query::paging`): owns the result
    /// cursor and orders every piece of database work behind it. Set
    /// with `client` on every `Booted`; dropping it ends the task.
    pager: Option<crate::query::paging::Pager>,
    /// The open result cursor, as the UI sees it. Taken by [`App::db`]
    /// (the barrier closes the real one) and by every other close.
    pub(crate) cursor: Option<CursorMirror>,
    /// How the active tab's result can continue (`M`).
    pub(crate) result_paging: Option<ResultPaging>,
    /// The last [`ResultPaging::id`] handed out.
    result_ids: u64,
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
    /// The connected server is a standby (`pg_is_in_recovery()`, asked
    /// at every connect). `None` until known. The header wears a
    /// `replica` badge for it, as the environments overview names one.
    pub in_recovery: Option<bool>,
    pub read_only: bool,
    /// What `:readonly on|off` last said, or `None` if the operator
    /// never said anything. Sticky across reconnects: `read_only`
    /// itself is recomputed from the new database's safety profile at
    /// every connect (`App::connect_to_pick`), which silently threw a
    /// `:readonly on` away the moment the operator switched data
    /// source — the opposite of what a safety toggle should do.
    ///
    /// It can only ever *tighten*: the effective flag is
    /// `profile.read_only || override == Some(true)`. `Some(false)`
    /// therefore means "no session opinion", not "force writable" —
    /// a profile that pins the database read-only stays read-only,
    /// and `:readonly off` against such a profile is refused outright
    /// before it gets set (`dispatch_read_only`).
    pub read_only_override: Option<bool>,
    /// The connected server is a replica — opened from the environments
    /// overview as one, or a standby by `pg_is_in_recovery()` at
    /// connect (`App::note_server_role`): read-only for the whole
    /// session, `\c <db>` included, and `:readonly off` is refused.
    pub replica_pin: bool,
    statement_timeout_ms: u64,
    msg_tx: UnboundedSender<AppMsg>,
    msg_rx: Option<UnboundedReceiver<AppMsg>>,
    /// The tap's own channel into the run loop ([`App::tap_tx_clone`]).
    tap_tx: mpsc::Sender<crate::tap::TapEvent>,
    tap_rx: Option<mpsc::Receiver<crate::tap::TapEvent>>,

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
    /// Clone of the message sender, for pushing messages into the run
    /// loop from outside the App.
    pub fn msg_tx_clone(&self) -> UnboundedSender<AppMsg> {
        self.msg_tx.clone()
    }

    /// Sender for the run loop's tap channel — bounded at
    /// [`TAP_APP_CHANNEL_CAPACITY`], unlike the message channel. The
    /// tap adapter in `main.rs` awaits on it, so an App that falls
    /// behind backs the events up into the listeners' own bounded
    /// channel, where an overflow is dropped and counted
    /// (`tap::dropped_at_listener`) instead of queueing without limit.
    pub fn tap_tx_clone(&self) -> mpsc::Sender<crate::tap::TapEvent> {
        self.tap_tx.clone()
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
        let (tap_tx, tap_rx) = mpsc::channel(TAP_APP_CHANNEL_CAPACITY);
        // When the operator hasn't pre-selected a DSN and we discovered
        // *any* candidates, the post-splash mode is the picker — that's
        // the "lovely discovery" surface. Splash shows first, but never
        // beyond `SPLASH_MIN` (`splash_until` sets that deadline); a picker
        // landing dismisses it straight away — `conn_state` stays
        // `Disconnected` there (no connect attempt is made until a pick is
        // confirmed), so the fast-resolve early-dismiss branch would
        // otherwise never fire and the operator would sit through the full
        // minimum before reaching the surface they actually need.
        //
        // One candidate lands here too, and that is the point: everything
        // in `data_source_picks` was read out of the working tree, so a
        // repo the operator didn't write chose the host. A lone candidate
        // used to be auto-connected before the TUI even drew, which made
        // `git clone && cd && pgman` enough to open a connection (and,
        // before the sibling fixes, to hand over `$PGPASSWORD`). Nothing
        // discovered connects without a keypress now.
        let show_picker = dsn.is_none() && !data_source_picks.is_empty();
        let mode = if show_picker {
            Mode::ConnPick
        } else {
            Mode::Normal
        };
        Self {
            theme,
            mode,
            demo: false,
            demo_tap_feed: None,
            conn_state: ConnState::Disconnected,
            lost_while_connecting: None,
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
            cell_edit: None,
            edit_awaiting_tx: None,
            tx_open: false,
            log_pick: LogPickUi::default(),
            last_log_pick: None,
            last_status: None,
            last_error: None,
            query_running: false,
            help: HelpUi::default(),
            mode_seen: std::collections::HashSet::new(),
            timing_on: false,
            redact: false,
            redact_env_names: Vec::new(),
            environments_config_broken: false,
            expanded_on: false,
            auto_closers: editor::AutoClosers::default(),
            // Read here rather than threaded in from `main` — the
            // project file is already discovered from the cwd there,
            // and this is the one consumer of its `[editor]` block.
            // A missing or unreadable cwd is the defaults.
            format_opts: std::env::current_dir()
                .map(|cwd| crate::project::load_editor_options(&cwd))
                .unwrap_or_default(),
            error_detail_return_to: None,
            command_bar_completions: None,
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
            tap_ring_bytes: 0,
            tap_generation: 0,
            tap_cache: crate::tap::TapInsightsCache::default(),
            tap_nav: TapNavUi::default(),
            tap_health: TapHealth::default(),
            tap_listeners: Vec::new(),
            tap_baseline: None,
            saved_queries: crate::saved::SavedQueries::default(),
            saved_queries_file: saved_queries_path(),
            draft_file: draft_path(),
            history_file: history_path(),
            saved_ui: SavedQueriesUi::default(),
            // Start with a single tab whose state IS the per-
            // session fields. The Vec entry is a placeholder that
            // gets refreshed on every tab switch.
            tabs: vec![TabSnapshot::default()],
            active_tab: 0,
            editor_lines: None,
            zoomed: false,
            body_rows: 0,
            pending_mark_set: false,
            armed_chord: None,
            schema_reload_announce: false,
            cancel_requested: false,
            pending_mark_jump: false,
            query_started: None,
            row_detail: RowDetailUi::default(),
            cell_detail: CellDetailUi::default(),
            schema_cache: SchemaCache::default(),
            databases: Vec::new(),
            completion: None,
            completion_popup_drawn: false,
            history_search: None,
            watch: None,
            notices: Vec::new(),
            external_edit_pending: false,
            draft_last_save: None,
            draft_dirty: false,
            draft_synced: String::new(),
            grid_view: GridView::default(),
            grid_find: GridFind::default(),
            explain: ExplainUi::default(),
            schema_browser: SchemaBrowserUi::default(),
            schema_lint: SchemaLintUi::default(),
            slow_queries: SlowQueriesUi::default(),
            sessions: SessionsUi::default(),
            environments: EnvironmentsUi::default(),
            environments_loader: None,
            secret_fetcher: None,
            replica_pin: false,
            env_discoverer: None,
            env_prober: None,
            environments_state_file: None,
            env_log_fetcher: None,
            env_patcher: None,
            last_run_sql: None,
            result_diff: ResultDiffUi::default(),
            command_bar: None,
            conn_pick: ConnPickUi {
                picks: data_source_picks,
                index: 0,
            },
            pending_tunnel: None,
            client: None,
            pager: None,
            cursor: None,
            result_paging: None,
            result_ids: 0,
            cancel_dispatcher: None,
            tunnel: None,
            safety_config,
            in_recovery: None,
            read_only,
            read_only_override: None,
            statement_timeout_ms,
            msg_tx,
            msg_rx: Some(msg_rx),
            tap_tx,
            tap_rx: Some(tap_rx),
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
        self.spawn_quit_on_signal();
        self.run_with(tui, event_rx).await
    }

    /// Turn SIGHUP (the terminal closed) and SIGTERM into
    /// [`AppMsg::Terminate`]. Their default action kills the process
    /// on the spot, skipping the exit path that saves the session.
    #[cfg(unix)]
    fn spawn_quit_on_signal(&self) {
        use tokio::signal::unix::{signal, SignalKind};
        let tx = self.msg_tx.clone();
        let (Ok(mut hup), Ok(mut term)) = (
            signal(SignalKind::hangup()),
            signal(SignalKind::terminate()),
        ) else {
            tracing::warn!("could not listen for SIGHUP / SIGTERM");
            return;
        };
        tokio::spawn(async move {
            tokio::select! {
                _ = hup.recv() => {}
                _ = term.recv() => {}
            }
            let _ = tx.send(AppMsg::Terminate);
        });
    }

    #[cfg(not(unix))]
    fn spawn_quit_on_signal(&self) {}

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
        let mut tap_rx = self
            .tap_rx
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
            // A dirty draft inside the save throttle needs the loop to
            // wake when the window closes: without this, the tail of a
            // typing burst sat unsaved until the next key, and closing
            // the terminal lost it.
            let draft_save_at = (!self.demo && self.draft_dirty)
                .then(|| draft_save_at(self.draft_last_save, DRAFT_SAVE_GAP));
            // An open result cursor closes on its deadline even when
            // nothing else wakes the loop: an idle pgman must not leave
            // the session idle in transaction past it.
            let cursor_close_at = self.cursor_deadline();
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
                    self.tick_demo_tap(Instant::now());
                }
                Some(msg) = msg_rx.recv() => {
                    self.on_msg(msg);
                    // Everything else already queued lands before the
                    // next draw, rather than one draw per message.
                    self.drain_pending_msgs(&mut msg_rx);
                }
                Some(event) = tap_rx.recv() => {
                    // Likewise every tap event already queued, as one
                    // batch: a frame per event held a burst in the
                    // channel, redrawing — and re-aggregating — for
                    // each.
                    self.on_tap_events(drain_tap_batch(event, &mut tap_rx));
                }
                _ = tokio::time::sleep_until(
                    tokio::time::Instant::from_std(draft_save_at.unwrap_or_else(Instant::now)),
                ), if draft_save_at.is_some() => {}
                _ = tokio::time::sleep_until(
                    tokio::time::Instant::from_std(cursor_close_at.unwrap_or_else(Instant::now)),
                ), if cursor_close_at.is_some() => {}
            }
            self.tick_cursor_timeout(Instant::now());
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
                && draft_save_due(self.draft_last_save, Instant::now(), DRAFT_SAVE_GAP)
            {
                if let Err(e) = self.save_draft() {
                    tracing::warn!("could not auto-save editor draft: {e}");
                }
                self.draft_last_save = Some(Instant::now());
                self.draft_dirty = false;
            }
        }
        // Persist session state on exit — but never in demo mode,
        // where the buffer / history / saved-queries are synthetic
        // fixtures that must not overwrite the operator's real files.
        if !self.demo {
            self.remember_env_selection();
            // Persist the editor draft so the next launch can restore
            // whatever the operator had in flight. Best-effort: failure
            // logs and moves on — the loop is already finishing. A
            // window that never changed the draft writes nothing, so
            // it can't overwrite what another window saved meanwhile.
            if let Err(e) = self.save_draft() {
                tracing::warn!("could not save editor draft: {e}");
            }
            // Persist the query history ring too. Same best-effort
            // stance — history is a convenience, not source of truth.
            if let Err(e) = persist_history_to(&self.history_file, &self.history) {
                tracing::warn!("could not save query history: {e}");
            }
            if let Err(e) = crate::saved::save_to(&self.saved_queries_file, &self.saved_queries) {
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
        // The picker and the environments overview are both places to
        // choose a connection from: neither waits for one to resolve.
        let is_picker = matches!(self.mode, Mode::ConnPick | Mode::Environments);
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
            || (self.environments.auto_probe && self.mode == Mode::Environments)
            || self.demo_tap_feed.is_some()
    }

    /// Apply up to [`MSG_DRAIN_PER_FRAME`] − 1 messages already waiting
    /// on `rx`, without blocking, so a backlog costs one draw rather
    /// than one per message. The cap keeps a flood from starving key
    /// input: the loop goes back to `select!` — and the key channel —
    /// after each batch.
    fn drain_pending_msgs(&mut self, rx: &mut UnboundedReceiver<AppMsg>) {
        for _ in 1..MSG_DRAIN_PER_FRAME {
            if self.should_quit {
                break;
            }
            match rx.try_recv() {
                Ok(msg) => self.on_msg(msg),
                Err(_) => break,
            }
        }
    }

    /// One frame of `--demo`'s tap feed: from its `due` time on, the
    /// next event arrives through [`Self::on_tap_event`], as a live
    /// tap's would. The feed is dropped with its last event, which
    /// also stops it holding the frame clock awake.
    pub fn tick_demo_tap(&mut self, now: Instant) {
        let Some(feed) = self.demo_tap_feed.as_mut() else {
            return;
        };
        if now < feed.due {
            return;
        }
        let next = feed.pending.pop_front();
        if feed.pending.is_empty() {
            self.demo_tap_feed = None;
        }
        if let Some(event) = next {
            self.on_tap_event(event);
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
        self.on_tap_events(std::iter::once(event));
    }

    /// Fold a batch of tap events — what the run loop hands over when
    /// it drains everything queued behind one frame
    /// ([`drain_tap_batch`]). The ring generation is bumped
    /// once for the whole batch, not per event, so the memoised
    /// aggregates (Hotspots, Callers, N+1) are recomputed at most once
    /// per drawn frame however many events the batch carried.
    pub fn on_tap_events(&mut self, events: impl IntoIterator<Item = crate::tap::TapEvent>) {
        // The monitor writes its summary into the status line when it
        // opens; an event landing while it is open must refresh that
        // line, or "no events yet" outlives the events. Only the
        // summary is replaced — any other status (a sort flash, an
        // error) is the operator's and stays.
        let shown_summary = (self.mode == Mode::TapMonitor)
            .then(|| self.tap_health.monitor_status(&self.tap_listeners))
            .filter(|s| self.last_status.as_deref() == Some(s.as_str()));
        let mut ring_changed = false;
        for event in events {
            ring_changed |= self.fold_tap_event(event);
        }
        if ring_changed {
            self.bump_tap_generation();
        }
        if shown_summary.is_some() {
            self.last_status = Some(self.tap_health.monitor_status(&self.tap_listeners));
        }
    }

    /// Fold one event; `true` when it went onto the ring, so the caller
    /// owes a generation bump.
    fn fold_tap_event(&mut self, event: crate::tap::TapEvent) -> bool {
        self.tap_health.last_event_at_unix_micros = event.received_at_unix_micros;
        match event.kind {
            crate::tap::TapKind::Heartbeat => {
                self.tap_health.heartbeat_count = self.tap_health.heartbeat_count.saturating_add(1);
                if let Some(d) = event.dropped_events_total {
                    self.tap_health.dropped_events_total = d;
                }
                false
            }
            crate::tap::TapKind::Query | crate::tap::TapKind::TxnBoundary => {
                if matches!(event.kind, crate::tap::TapKind::Query) {
                    self.tap_health.query_count = self.tap_health.query_count.saturating_add(1);
                } else {
                    self.tap_health.boundary_count =
                        self.tap_health.boundary_count.saturating_add(1);
                }
                // Two caps, oldest out first: `TAP_CAP` events and
                // `TAP_RING_MAX_BYTES` of text — the count alone let
                // 2 000 fully-capped events hold ~400 MiB.
                let evicted = crate::tap::push_bounded(
                    &mut self.tap_events,
                    &mut self.tap_ring_bytes,
                    event,
                    TAP_CAP,
                    crate::tap::TAP_RING_MAX_BYTES,
                );
                // Cursor follows the eviction so a viewer parked on the
                // oldest row doesn't suddenly jump forward in content.
                self.tap_nav.events_cursor = self.tap_nav.events_cursor.saturating_sub(evicted);
                true
            }
        }
    }

    /// Mark the tap ring as changed, invalidating every memoised
    /// aggregate over it. Must be called by anything that pushes to,
    /// evicts from, clears or replaces `tap_events` — the aggregates
    /// are keyed on this and nothing else about the ring's contents.
    pub fn bump_tap_generation(&mut self) {
        self.tap_generation = self.tap_generation.wrapping_add(1);
    }

    /// The error overlay over the most recent failure (F2, `!`,
    /// `:error`). No-op with a status when there's nothing to show.
    pub(super) fn open_error_detail(&mut self) {
        if self.last_error_detail.is_some() || self.last_error.is_some() {
            // Remember where it was opened so closing the overlay
            // lands back there — the way help does. Landing in
            // Normal from the editor turned the next keys typed
            // into hotkeys.
            self.error_detail_return_to = Some(self.mode);
            self.mode = Mode::ErrorDetail;
        } else {
            self.last_status = Some("no error to expand".into());
        }
    }

    /// True while a prompt inside a non-typing mode owns every
    /// character, so the global plain keys (`?`, `:`, `!`, the tab
    /// digits and brackets) step aside and the key reaches it:
    ///
    /// - the picker asking to authorise an `ssh` session to a bastion
    ///   a committed file named, which says "any other key cancels"
    ///   and means it, rather than leaving the question armed behind
    ///   an overlay;
    /// - the patch view while a protected environment's name is
    ///   typed, where `prod2` switched to tab 2 at the `2`.
    fn prompt_owns_keys(&self) -> bool {
        let tunnel = self.mode == Mode::ConnPick && self.pending_tunnel.is_some();
        let naming = self.mode == Mode::Environments
            && self.environments.patch.as_ref().is_some_and(|v| {
                matches!(&v.stage, PatchStage::Plan(Ok(p)) if p.needs_typed() && !p.patches.is_empty())
            });
        tunnel || naming
    }

    /// Key dispatch — [`Self::on_key_inner`] wrapped in one invariant the
    /// individual handlers cannot each be trusted to remember: a key
    /// handler only ever produces a *client-side* error (server errors
    /// arrive through `on_msg`), so when a key changes `last_error` the
    /// SQLSTATE / hint detail of whatever server error came before it is
    /// stale. Left in place, F2 on `:readonly off`'s refusal opened an
    /// overlay headed by the previous query's `SQLSTATE 42P01`.
    pub fn on_key(&mut self, key: KeyEvent) {
        let error_before = self.last_error.clone();
        let status_before = self.last_status.clone();
        let mode_before = self.mode;
        self.on_key_inner(key);
        if self.last_error.is_some() && self.last_error != error_before {
            self.last_error_detail = None;
        }
        // The footer shows an error over a status, so an error the key
        // left alone outlived every later success: `s` sorted, `y`
        // yanked, `:timing on` answered, all under the old red line.
        // A status this key set is newer news. A second-press prompt
        // (Ctrl-C, Ctrl-W) is a question, not news, and a panel's
        // first-visit tip is about the keys; both leave it.
        if self.last_error.is_some()
            && self.last_error == error_before
            && self
                .last_status
                .as_deref()
                .is_some_and(|s| !s.starts_with("tip · "))
            && self.last_status != status_before
            && self.armed_chord.is_none()
        {
            self.last_error = None;
            self.last_error_detail = None;
        }
        // Opening or closing a panel changes the subject: a failed `T`
        // does not belong under the sessions panel the operator opened
        // next, nor in the footer after they left it — it stayed
        // through `L`, `W`, `S` and every Esc. Only an error this key
        // did not itself produce is dropped, and only across a panel
        // edge: the error-detail overlay *shows* the error, and leaving
        // the editor for Normal is not a change of subject.
        if self.mode != mode_before
            && self.last_error == error_before
            && self.mode != Mode::ErrorDetail
            && mode_before != Mode::ErrorDetail
            && (is_panel(mode_before) || is_panel(self.mode))
        {
            self.last_error = None;
            self.last_error_detail = None;
        }
    }

    fn on_key_inner(&mut self, key: KeyEvent) {
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
        //
        // It takes a second press: one reflex Ctrl-C (meant for a
        // query, or for the shell) used to drop the session with no
        // word. Ctrl-W's close-tab arms the same way below; any other
        // key disarms both.
        let armed = self.armed_chord.take();
        if armed.is_some()
            && self
                .last_status
                .as_deref()
                .is_some_and(|s| s.starts_with("ctrl-c again") || s.starts_with("ctrl-w again"))
        {
            self.last_status = None;
        }
        if !was_watching
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
            && self.mode != Mode::Editor
        {
            // From the results, a running query or `M` fetch is what
            // Ctrl-C stops, as it does from the editor; quitting stays
            // a second press once nothing runs (and `q` either way).
            if self.mode == Mode::Normal && self.query_running {
                self.cancel_running_query();
                return;
            }
            if armed == Some(ArmedChord::Quit) {
                self.should_quit = true;
            } else {
                self.armed_chord = Some(ArmedChord::Quit);
                self.last_status = Some("ctrl-c again to quit".into());
            }
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
        // The `:` bar is only ever `take()`n by its own key handler,
        // but the mode can leave `CommandBar` underneath it — an async
        // `QueryOk { tx_open_after }` / `TxClosed` / `CostPreviewLoaded`
        // landing mid-bar, or a chord that opened a panel. A bar with
        // no mode behind it is stale text (`:rea`) that would otherwise
        // sit in the footer for the rest of the session; drop it on the
        // next key.
        if self.mode != Mode::CommandBar && self.command_bar.is_some() {
            self.command_bar = None;
        }

        // F1 opens help from ANY mode (except Help itself, which closes
        // on F1 / esc / ? / q). The cheat-sheet auto-scrolls to the
        // section for the mode we came from so the operator sees the
        // relevant keys immediately.
        if matches!(key.code, KeyCode::F(1)) && !matches!(self.mode, Mode::Help | Mode::HelpFilter)
        {
            self.open_help_from(self.mode);
            return;
        }
        // `?` is F1's twin — the key operators reach for first, and the
        // one ebman binds globally. It works from every mode EXCEPT
        // those taking literal text, where `?` is a character the
        // operator meant to type (a `LIKE '%?%'` in the editor, a `?`
        // in a filter). Help itself closes on `?`, handled by
        // `on_help_key`.
        if matches!(key.code, KeyCode::Char('?'))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && self.mode != Mode::Help
            && !self.mode.is_text_input()
            && !self.prompt_owns_keys()
        {
            self.open_help_from(self.mode);
            return;
        }
        // A decision modal owns the keyboard. `Confirm` holds a
        // guarded statement, `TxDecision` an OPEN transaction and
        // `ConfirmTerminate` a pid: every global chord below (F2/F3/F4,
        // `:`, the tab chords) used to walk out of them, and the panel
        // it opened then closed into Normal with `tx_open` still true
        // and no way back to the y/n — `\watch` refused for the rest
        // of the session. Only the modal's own keys (and F1 / `?`,
        // which return to it) get through. `:` from `Confirm` is the
        // one exception: `open_command_bar` explicitly cancels the
        // pending run on the way in, so nothing dangles.
        let modal_open = matches!(
            self.mode,
            Mode::TxDecision | Mode::Confirm | Mode::ConfirmTerminate
        );
        if modal_open {
            let colon = !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && matches!(key.code, KeyCode::Char(':'));
            if !(colon && self.mode == Mode::Confirm) {
                let pre_mode = self.mode;
                match self.mode {
                    Mode::Confirm => self.on_confirm_key(key),
                    Mode::TxDecision => self.on_tx_decision_key(key),
                    _ => self.on_confirm_terminate_key(key),
                }
                if self.mode != pre_mode {
                    self.note_mode_entry(self.mode);
                }
                return;
            }
        }
        // F2 expands the most-recent query failure into the rich
        // error overlay (severity / code / detail / hint / affected
        // schema/table/column/constraint). `!` is its plain-key twin
        // wherever a character isn't being typed — F2 takes fn on a
        // Mac keyboard — and `:error` the command-bar spelling.
        let bang = matches!(key.code, KeyCode::Char('!'))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && !self.mode.is_text_input()
            && !self.prompt_owns_keys();
        if (matches!(key.code, KeyCode::F(2)) || bang) && self.mode != Mode::ErrorDetail {
            self.open_error_detail();
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
        // `:` opens the command bar — the one-line footer prompt that
        // dispatches `:about`, `:readonly off`, `:dt`, … (see
        // `App::dispatch_command`). Gated on `typing_mode` so a literal
        // colon still types into the editor and every other prompt
        // (`Mode::CommandBar` is itself a typing mode, so the bar can't
        // re-open on top of itself either).
        if !ctrl
            && !alt
            && !typing_mode
            && !self.prompt_owns_keys()
            && matches!(key.code, KeyCode::Char(':'))
        {
            self.open_command_bar();
            return;
        }
        // A new tab or a cycle swaps the editor buffer: from a prompt
        // (save-as, rename, param, filter) Enter would then act on the
        // other tab's SQL. The editor and the panels are fine.
        let tab_keys_ok = !typing_mode || self.mode == Mode::Editor;
        if ctrl && matches!(key.code, KeyCode::Char('t')) && tab_keys_ok {
            self.new_tab();
            return;
        }
        if ctrl && matches!(key.code, KeyCode::Char('w')) && !typing_mode {
            // `w` conflicts with `\watch` in editor mode — gate
            // close-tab to non-editor modes. Operators close tabs
            // from Normal (most common) or from any non-typing
            // panel. A tab holding SQL takes a second press: the
            // chord is `\watch` in the editor, and reaching for that
            // from the grid used to throw the buffer away.
            let holds_sql = !self.editor.buffer.trim().is_empty();
            if holds_sql && self.tabs.len() > 1 && armed != Some(ArmedChord::CloseTab) {
                self.armed_chord = Some(ArmedChord::CloseTab);
                self.last_status = Some(format!(
                    "ctrl-w again to close tab {} and its SQL",
                    self.active_tab + 1
                ));
            } else {
                self.close_active_tab();
            }
            return;
        }
        if ctrl && matches!(key.code, KeyCode::Tab) && tab_keys_ok {
            self.cycle_tab(!shift);
            return;
        }
        if ctrl && shift && matches!(key.code, KeyCode::BackTab) && tab_keys_ok {
            // Some terminals deliver Ctrl-Shift-Tab as BackTab.
            self.cycle_tab(false);
            return;
        }
        // `Ctrl-]` — next tab from the editor and every non-typing
        // panel. Alt chords are optional on a Mac (iTerm sends them as
        // Esc or as an accented letter unless told otherwise) and
        // Ctrl-Tab is not delivered by every terminal, but the 0x1D
        // byte Ctrl-] produces reaches every program. crossterm's
        // legacy parser reports that byte as Ctrl-5 (the classic
        // Ctrl-4..7 → 0x1C..0x1F encoding) and the kitty protocol as
        // Ctrl-]: both shapes are the same key. Off in the prompts and
        // filters for the reason the digit jump below is.
        if ctrl
            && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5'))
            && (!typing_mode || self.mode == Mode::Editor)
            && !self.prompt_owns_keys()
        {
            self.cycle_tab(true);
            return;
        }
        // Gated like Ctrl-W: from a typing prompt (save-as name,
        // rename, param, filter…) a tab jump swapped the editor buffer
        // out from under the prompt, and Enter then saved the OTHER
        // tab's SQL under the name just typed.
        if alt && !typing_mode {
            if let KeyCode::Char(c) = key.code {
                if self.mode != Mode::Help && self.jump_to_tab_digit(c) {
                    return;
                }
            }
        }
        // Tabs without a modifier: `]` / `[` next / previous and
        // `1`..`9` jump, from the grid and every non-typing panel.
        // These are the primary bindings — the chords above are kept
        // for terminals that deliver them. The schema browser keeps
        // `[` / `]` for its own schema-to-schema jump; the digits still
        // switch tabs there (the browser is not per-tab state). Letters
        // type in the editor and the prompts, so those are out — and
        // so is the keypress after `m` / `'`, which names a bookmark
        // (or cancels the pending one) rather than a tab.
        let mark_pending = self.pending_mark_set || self.pending_mark_jump;
        if !ctrl && !alt && !typing_mode && !mark_pending && !self.prompt_owns_keys() {
            if let KeyCode::Char(c) = key.code {
                // Help uses them for next / previous section.
                let bracket_is_mine = !matches!(self.mode, Mode::SchemaBrowser | Mode::Help);
                if c == ']' && bracket_is_mine {
                    self.cycle_tab(true);
                    return;
                }
                if c == '[' && bracket_is_mine {
                    self.cycle_tab(false);
                    return;
                }
                // Not behind the help overlay: the tab would change
                // unseen, under the page being read.
                if self.mode != Mode::Help && self.jump_to_tab_digit(c) {
                    return;
                }
            }
        }
        // Pane zoom and editor size — Alt-Z / Alt-= / Alt-- / Alt-0 —
        // and Alt-N / Alt-P for next / previous tab (Ctrl-Tab is not
        // delivered by every terminal; Alt chords are). Global, and
        // live in the editor too (that is where the operator is when
        // the grid is too short, and where Ctrl-Tab already works), but
        // off in every other text input: a prompt or filter has the
        // keyboard, and reshaping the panes — or swapping the buffer —
        // under it is not what a chord typed there meant. The decision
        // modals returned above.
        if alt && (!typing_mode || self.mode == Mode::Editor) && !self.prompt_owns_keys() {
            match key.code {
                KeyCode::Char('n') => {
                    self.cycle_tab(true);
                    return;
                }
                KeyCode::Char('p') => {
                    self.cycle_tab(false);
                    return;
                }
                KeyCode::Char('z') => {
                    self.toggle_zoom();
                    return;
                }
                KeyCode::Char('=') | KeyCode::Char('+') => {
                    self.resize_editor(1);
                    return;
                }
                KeyCode::Char('-') | KeyCode::Char('_') => {
                    self.resize_editor(-1);
                    return;
                }
                KeyCode::Char('0') => {
                    self.reset_editor_size();
                    return;
                }
                _ => {}
            }
        }

        let pre_mode = self.mode;
        match self.mode {
            Mode::Help => self.on_help_key(key),
            Mode::HelpFilter => self.on_help_filter_key(key),
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
            Mode::Environments => self.on_environments_key(key),
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
            Mode::CommandBar => self.on_command_bar_key(key),
            Mode::CellEdit => self.on_cell_edit_key(key),
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
                "tip · / filter · [ ] jump schema · + / − expand-all/collapse-all · ? full keys",
            ),
            Mode::ExplainTree => {
                Some("tip · j/k navigate · enter expand · hottest node is red · ? full keys")
            }
            Mode::SlowQueries => Some(
                "tip · enter copy SQL to editor · r refresh from pg_stat_statements · ? full keys",
            ),
            Mode::Sessions => Some(
                "tip · blocked sessions sort top in red · K terminate · r refresh · ? full keys",
            ),
            Mode::Environments => Some(
                "tip · from environments.toml · replicas under their primary · r refresh · ? full keys",
            ),
            Mode::SchemaLint => {
                Some("tip · severity-sorted (HIGH first) · y yanks SQL suggestion · e evidence query · ? full keys")
            }
            Mode::LogPick => {
                Some("tip · c toggles cluster view · enter loads selected · ? full keys")
            }
            Mode::RowDetail => {
                Some("tip · J/K next/prev row · enter zooms a field · y yanks · ? full keys")
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
        self.tap_ring_bytes = 0;
        self.bump_tap_generation();
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

    /// `Ctrl-S` in editor — prompt for a name, a suggestion derived
    /// from the buffer pre-filled and selected (typing replaces it).
    /// Enter persists; Esc cancels.
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
        self.saved_ui.save_name_suggested = true;
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
        self.last_status = Some(crate::ui::count_label(
            self.saved_queries.entries.len() as u64,
            "saved query",
            "saved queries",
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
    /// Shared by the direct and post-`:param`-prompt load paths. The
    /// status ends with the key that runs it now — after a prompt for
    /// values the next question is how to send it.
    fn load_sql_into_editor(&mut self, sql: String, status: String) {
        let run_key = if editor::enter_runs(&sql) {
            "⏎ runs it"
        } else {
            "alt-⏎ runs it"
        };
        self.editor.buffer = sql;
        self.editor.cursor = self.editor.buffer.len();
        self.editor.preferred_col = None;
        self.history_pos = None;
        self.last_status = Some(format!("{status} · {run_key}"));
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

    /// `J` / `K` (and PageDown / PageUp) in the row detail — move the
    /// grid selection to the next / previous visible row (the filtered,
    /// sorted order `j` / `k` walk on the grid) and keep the detail
    /// open on it, with the highlighted field preserved — clamped if
    /// the new row has fewer fields. The selection moves for real, so
    /// closing the detail leaves the operator on the row they were
    /// reading. At either end: a status, no move.
    fn step_row_detail(&mut self, delta: isize) {
        let count = self.visible_row_count();
        let Some(current) = self.grid_state.selected() else {
            return;
        };
        let target = current as isize + delta;
        if target < 0 || target >= count as isize {
            let end = if delta < 0 { "first" } else { "last" };
            self.last_status = Some(format!(
                "already at the {end} row ({} / {count})",
                current.min(count.saturating_sub(1)) + 1
            ));
            return;
        }
        let target = target as usize;
        self.grid_state.select(Some(target));
        if let Some(row) = self
            .grid_view
            .visible_rows
            .get(target)
            .and_then(|&i| self.grid.rows.get(i))
        {
            self.row_detail.field = self.row_detail.field.min(row.len().saturating_sub(1));
        }
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
        if let Some(parsed) = crate::query::json_cell::parse_jsonb_cell(cell_text(value)) {
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
    /// Normal), and shows the section that matches `from` — operators
    /// see the relevant keys without hunting for them. A mode with no
    /// section of its own (the grid's start) opens the section list.
    pub fn open_help_from(&mut self, from: Mode) {
        let section = Self::help_anchor_for(from).and_then(crate::ui::help_section_index);
        self.open_help_section(section, from);
    }

    /// Open help at section `section` (an index into
    /// `ui::HELP_SECTIONS`; `None` opens the list) but return to
    /// `return_to` on close. `:help <topic>` is the reason the two
    /// can differ: the topic names the section to show, not the mode
    /// the operator was in. Collapsing them meant `:help tap` closed
    /// into the tap monitor and `:help commands` into
    /// `Mode::CommandBar` — a mode with no command bar behind it.
    pub fn open_help_section(&mut self, section: Option<usize>, return_to: Mode) {
        self.help.section = section.unwrap_or(0);
        self.help.view = if section.is_some() {
            HelpView::Section
        } else {
            HelpView::List
        };
        self.open_help_view(return_to);
    }

    /// `:help all` — every section on one scrolling page, as help was
    /// before it had sections.
    pub fn open_help_all(&mut self, return_to: Mode) {
        self.help.section = 0;
        self.help.view = HelpView::All;
        self.open_help_view(return_to);
    }

    fn open_help_view(&mut self, return_to: Mode) {
        self.help.return_to = Some(return_to);
        self.help.scroll = 0;
        self.help.filter = None;
        self.mode = Mode::Help;
    }

    /// Pure: the heading of the help section a given source mode
    /// opens at (`ui::HELP_SECTIONS`). `None` opens the section list.
    pub fn help_anchor_for(mode: Mode) -> Option<&'static str> {
        match mode {
            Mode::Normal => None,
            Mode::Editor => Some("editor"),
            Mode::HistorySearch => Some("editor"),
            Mode::Confirm => Some("confirm"),
            Mode::TxDecision => Some("tx open"),
            Mode::LogPick => Some("log pick"),
            Mode::ConnPick => Some("conn pick"),
            Mode::RowDetail => Some("row detail"),
            Mode::CellDetail => Some("cell detail"),
            Mode::CellEdit => Some("cell edit"),
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
            Mode::Environments => Some("environments"),
            Mode::GridFilter => Some("grid"),
            Mode::GridFind => Some("grid"),
            Mode::About => Some("grid"),
            Mode::CommandBar => Some(": commands"),
            Mode::Help | Mode::HelpFilter => None,
        }
    }

    /// Drop a status a completion popup left (`completion: 3 matches ·
    /// Tab to pick`, …) once the popup is gone; any other status stays.
    fn clear_completion_status(&mut self) {
        if self
            .last_status
            .as_deref()
            .is_some_and(|s| s.starts_with("completion"))
        {
            self.last_status = None;
        }
    }

    /// Re-extract candidates from the current buffer/cursor. Called
    /// after a narrowing key while the completion popup is in
    /// pre-selection state so the popup stays live and narrows as the
    /// operator types. Preserves the cycle's `origin` fields so a
    /// later Esc still undoes back to the pre-Tab state.
    fn refresh_completion(&mut self) {
        let Some(old) = self.completion.take() else {
            return;
        };
        let Some(id) = complete_q::extract_identifier(&self.editor.buffer, self.editor.cursor)
        else {
            // The popup closed (a `;`, a `(`): its `completion: 1 match
            // · Tab to pick` would otherwise outlive it.
            self.clear_completion_status();
            return;
        };
        // Empty prefix is fine — the candidate set falls back to "all
        // identifier-shaped candidates for the surrounding clause"
        // (matches the Tab-on-whitespace UX). The cycle drops naturally
        // when those produce no matches.
        let cands =
            complete_q::candidates_for(&self.editor.buffer, self.editor.cursor, &self.schema_cache);
        if cands.is_empty() {
            // A popup the editor opened on its own closes as quietly as
            // it opened: the operator typing on through it never asked
            // for completion, and `no matches for t.…` stayed in the
            // footer while they went on writing ordinary SQL.
            if old.auto {
                self.clear_completion_status();
                return;
            }
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
        // What the operator typed to narrow is theirs: Esc goes back to
        // it, not to the text the popup opened on. Keeping the opening
        // text lost every character typed since — the popup a space
        // after `FROM` opens starts on an empty prefix, so `us` then
        // Tab then Esc left nothing.
        self.completion = Some(CompletionCycle {
            start: prefix_start,
            end: self.editor.cursor,
            origin: self.editor.buffer[prefix_start..self.editor.cursor].to_string(),
            origin_prefix: id.prefix,
            origin_cursor: self.editor.cursor,
            candidates: cands,
            selected: None,
            auto: old.auto,
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
        let Some(db) = self.db() else {
            self.tx_open = false;
            self.mode = match self.edit_awaiting_tx.take() {
                Some(edit) => {
                    self.revert_cell_edit(&edit);
                    edit.return_to
                }
                None => Mode::Editor,
            };
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
            let client = db.ready().await;
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
    pub fn preload_log(&mut self, text: String) {
        self.editor.buffer = text;
        self.editor.cursor = self.editor.buffer.len();
        self.mode = Mode::Editor;
        self.start_log_import();
        // This entry bypasses `on_key`, so spend the picker's
        // first-visit tip here — on the first view, where it helps —
        // rather than on a later Ctrl-L, where it would hide the
        // "reopened the last log" status.
        self.note_mode_entry(self.mode);
    }

    /// F8 in editor mode — parse the editor buffer through `hibernate::parse`
    /// and `pglog::parse`, then enter `Mode::LogPick` if anything was found.
    /// Ctrl-L / F8 outside the editor: reconstruct the log already in it,
    /// reopen the last log picker, or open the editor to paste one.
    pub(super) fn log_import_from_normal(&mut self) {
        if query::logdetect::detect_log(&self.editor.buffer).is_some()
            || self.last_log_pick.is_some()
        {
            return self.start_log_import();
        }
        self.mode = Mode::Editor;
        self.last_error = None;
        self.last_status = Some("paste a Hibernate or Postgres log here, then ctrl-l".to_string());
    }

    fn start_log_import(&mut self) {
        let log = &self.editor.buffer;
        let mut picks: Vec<ReconstructedQuery> = Vec::new();
        picks.extend(query::hibernate::parse(log));
        picks.extend(query::pglog::parse(log));
        picks.extend(query::proxylog::parse(log));
        // Third way in: neither log parser found anything, but the
        // buffer has the "?-statement, blank line, typed param list"
        // shape a pasted JDBC statement makes — same shape
        // `logdetect::detect_log` hints at, so the two can't disagree.
        if picks.is_empty() {
            if let Some((head, tail)) = query::logdetect::split_jdbc_paste(log) {
                picks.extend(query::jdbc::parse(&head, &tail));
            }
        }
        if picks.is_empty() {
            if let Some(last) = self.last_log_pick.take() {
                self.last_error = None;
                self.last_status = Some(format!(
                    "reopened the last log · {}",
                    crate::ui::count_label(last.picks.len() as u64, "query", "queries")
                ));
                self.log_pick = last;
                self.mode = Mode::LogPick;
                return;
            }
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

    /// Resolve the focused row's SQL to load, and whether it still has
    /// unbound placeholders. `runnable_sql` for the AllQueries view; for
    /// Clusters, the first member whose values are all bound
    /// (`nplus1::runnable_member`) — the cluster's `example` is the `?`
    /// template the view groups by, and loading it handed the operator a
    /// statement that cannot run. When no member is bound (the log had
    /// no bind lines for this shape) the template loads and the flag
    /// lets Enter say so instead of calling it `loaded query`.
    /// The focused pick's error when it failed where it was logged —
    /// the all-queries view only: a cluster row is a shape, not a run.
    pub fn focused_log_pick_error(&self) -> Option<&str> {
        match self.log_pick.view {
            LogPickView::AllQueries => self
                .log_pick
                .picks
                .get(self.log_pick.index)?
                .error
                .as_deref(),
            LogPickView::Clusters => None,
        }
    }

    fn focused_log_pick_sql(&self) -> Option<(String, bool)> {
        let sql = match self.log_pick.view {
            LogPickView::AllQueries => self
                .log_pick
                .picks
                .get(self.log_pick.index)
                .map(|q| q.runnable_sql.clone()),
            LogPickView::Clusters => self.log_pick.clusters.get(self.log_pick.index).map(|c| {
                crate::query::nplus1::runnable_member(c, &self.log_pick.picks)
                    .map(|q| q.runnable_sql.clone())
                    .unwrap_or_else(|| c.example.clone())
            }),
        }?;
        let unbound = crate::query::nplus1::has_unbound_placeholder(&sql);
        // Terminated, so the wedge ends paste → ctrl-l → pick → Enter →
        // Enter: a bare Enter runs a statement that ends with `;`.
        Some((editor::ensure_terminated(&sql), unbound))
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
    /// Ctrl-F → reformat the editor buffer in place. `pg_format` (the
    /// widely-deployed Perl `pgformatter`) is used when it is on
    /// `PATH`; otherwise the built-in `query::format` — same two
    /// knobs (`[editor]` in `pgman.toml`), and a refusal rather than
    /// a changed statement when it cannot keep every literal intact.
    /// The status line says which ran. This is the only place
    /// formatting happens: never on run, never on paste.
    ///
    /// Done inline — both are sub-second on any realistic buffer; the
    /// alternative (`spawn_blocking` + message round-trip) adds more
    /// plumbing than the operation deserves. The undo entry is pushed
    /// by `on_editor_key` around this call, so Ctrl-Z restores the
    /// pre-format buffer.
    fn reformat_buffer(&mut self) {
        let path = std::env::var_os("PATH");
        self.reformat_buffer_with_path(path.as_deref());
    }

    /// [`Self::reformat_buffer`] with the `PATH` to look `pg_format`
    /// up on passed in, so tests can point it at a directory holding
    /// a stub `pg_format` (installed) or an empty one (absent)
    /// without touching the process environment.
    pub fn reformat_buffer_with_path(&mut self, path_var: Option<&std::ffi::OsStr>) {
        if self.editor.buffer.trim().is_empty() {
            self.last_status = Some("nothing to format".into());
            return;
        }
        let pg_format = path_var.and_then(|p| find_on_path_in("pg_format", p));
        let result = match &pg_format {
            Some(binary) => pg_format_via(&self.editor.buffer, &binary.to_string_lossy())
                .map(|s| (s, "formatted via pg_format")),
            None => match query::format::format_sql(&self.editor.buffer, &self.format_opts) {
                Ok(s) => Ok((s, "formatted (built-in)")),
                // A refusal, not a failure: the buffer is left alone
                // and the footer says why.
                Err(skipped) => {
                    self.last_status = Some(skipped.to_string());
                    return;
                }
            },
        };
        match result {
            Ok((formatted, how)) => {
                let chars = formatted.len();
                self.editor.buffer = formatted;
                self.editor.cursor = self.editor.buffer.len();
                self.editor.preferred_col = None;
                self.history_pos = None;
                self.draft_dirty = true;
                self.last_status = Some(format!("{how} · {chars} char(s)"));
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
        self.tick_env_auto_probe();
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

    /// `R` in the environments overview: probe the listed environments
    /// again, no oftener than `ENV_AUTO_PROBE_INTERVAL`, while the
    /// overview is open and showing its list.
    fn toggle_env_auto_probe(&mut self) {
        if !self.environments.reports.iter().any(|r| r.probe.is_some()) {
            self.last_status =
                Some("no environment probes — set probe = true on one in environments.toml".into());
            return;
        }
        let ui = &mut self.environments;
        ui.auto_probe = !ui.auto_probe;
        ui.auto_probe_last = Some(Instant::now());
        self.last_status = Some(format!(
            "auto-probe {} ({}s)",
            if ui.auto_probe { "on" } else { "off" },
            ENV_AUTO_PROBE_INTERVAL.as_secs(),
        ));
    }

    fn tick_env_auto_probe(&mut self) {
        let ui = &self.environments;
        if !ui.auto_probe
            || self.mode != Mode::Environments
            || ui.discovery.is_some()
            || ui.loading
            // The last round hasn't answered yet: don't pile another on.
            || ui.probes_in_flight > 0
        {
            return;
        }
        let now = Instant::now();
        if ui
            .auto_probe_last
            .is_some_and(|t| now.saturating_duration_since(t) < ENV_AUTO_PROBE_INTERVAL)
        {
            return;
        }
        self.environments.auto_probe_last = Some(now);
        self.start_env_probes();
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
        self.cancel_requested = true;
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
    /// No session to work on: never connected, or the connection is gone
    /// and the failure card is up (the dead client stays in `client`
    /// until a reconnect replaces it). A statement refused here never
    /// reaches the guard's prompt.
    pub(super) fn no_session(&self) -> bool {
        self.client.is_none() || matches!(self.conn_state, ConnState::Failed(_))
    }

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

    /// The client for one piece of database work, queued behind the
    /// pager so that it never runs inside the result cursor's
    /// transaction: the pager closes the cursor first
    /// (docs/backlog/paging.md, "one session, many tasks"). The place in
    /// the queue is taken here, on the UI thread, so the order of calls
    /// is the order on the wire. The spawned task awaits
    /// [`crate::query::paging::Db::ready`] before its first statement.
    ///
    /// The only reader of `self.client` for database work; the cursor's
    /// own requests go to the pager directly.
    pub(crate) fn db(&mut self) -> Option<crate::query::paging::Db> {
        // The barrier closes the cursor (and with no client there is
        // none); the title follows.
        self.cursor = None;
        if matches!(self.conn_state, ConnState::Failed(_)) {
            // The connection-failed card is up: the session was lost
            // (`App::on_connection_lost`), or a reconnect failed and the
            // client still held is the session that was left. Neither is
            // one to send work to; `r` reconnects.
            return None;
        }
        let client = self.client.clone()?;
        Some(crate::query::paging::Db::new(client, self.pager.as_ref()))
    }

    /// Close the open result cursor, if any, without waiting — the
    /// pager queues the `COMMIT` ahead of whatever comes next.
    pub(crate) fn close_cursor(&mut self) {
        if self.cursor.take().is_some() {
            if let Some(pager) = &self.pager {
                pager.close();
            }
        }
    }

    /// The grid was replaced by something that is not a run's result
    /// (`\l`, the bootstrap after a connect, the demo's `\df`): drop
    /// the old result's paging record, and its cursor, which now feeds
    /// nothing.
    pub(crate) fn forget_result_paging(&mut self) {
        if self.result_more() == crate::grid::More::CursorOpen {
            self.close_cursor();
        }
        self.result_paging = None;
    }

    /// How the active result can continue past the rows it holds.
    pub fn result_more(&self) -> crate::grid::More {
        result_more(
            self.result_paging.as_ref(),
            self.cursor.map(|c| c.result_id),
        )
    }

    /// When the main loop must wake to close an idle cursor. `None`
    /// while a fetch runs: the pager is busy with it, and the limits
    /// restart when it lands.
    pub(crate) fn cursor_deadline(&self) -> Option<Instant> {
        (!self.query_running)
            .then_some(self.cursor)
            .flatten()
            .map(|c| c.deadline())
    }

    /// Close a cursor that has outlived its limits — the session is idle
    /// in transaction while it is open, holding its tables' locks — and
    /// say so.
    pub(crate) fn tick_cursor_timeout(&mut self, now: Instant) {
        if self.query_running {
            return;
        }
        let Some(why) = self.cursor.and_then(|c| c.expired(now)) else {
            return;
        };
        let limits = self.cursor.map(|c| c.limits);
        self.close_cursor();
        let after = match (why, limits) {
            (CursorExpiry::Idle, Some(l)) => format!("{}s idle", l.idle.as_secs().max(1)),
            (CursorExpiry::Lifetime, Some(l)) => {
                format!("{} min open", (l.lifetime.as_secs() / 60).max(1))
            }
            (_, None) => String::new(),
        };
        self.last_status = Some(format!(
            "result cursor closed after {after} · M re-runs the query for more rows"
        ));
    }

    /// `M` in the grid: the next page from the open cursor, or — once
    /// it has closed — a re-run that fetches as many rows as are held
    /// plus a page (docs/backlog/paging.md).
    pub(crate) fn fetch_more(&mut self) {
        use crate::grid::More;
        use crate::query::paging::{MAX_HELD_ROWS, PAGE_ROWS};
        if !self.grid.truncated {
            self.last_status = Some(if self.grid.columns.is_empty() {
                "no result to fetch more of".into()
            } else {
                "all rows are already fetched".into()
            });
            return;
        }
        if self.query_running {
            self.last_status = Some("a query is already running · ctrl-c cancels it".into());
            return;
        }
        let held = self.grid.row_count();
        match self.result_more() {
            More::Unpaged => {
                self.last_status = Some(
                    "only a plain SELECT from the editor, calling no function but read-only built-ins, pages · pgman query '…' --json returns every row"
                        .into(),
                );
            }
            More::InTransaction => {
                self.last_status = Some(
                    "a transaction was open when this ran, so it can't page · commit or roll back, then run it again"
                        .into(),
                );
            }
            More::Ceiling => {
                self.last_status = Some(format!(
                    "pgman holds at most {MAX_HELD_ROWS} rows · pgman query '…' --json returns every row"
                ));
            }
            More::CursorOpen => {
                let want = PAGE_ROWS.min(MAX_HELD_ROWS.saturating_sub(held));
                self.spawn_page_request(PageRequest::Fetch(want));
                self.last_status = Some(format!("fetching rows {}–{}…", held + 1, held + want));
            }
            More::CursorClosed | More::Standby => {
                // The statement passed `is_pageable_select` when it first
                // ran — a plain SELECT, which every guard allows — and that
                // classification is a pure function of the text.
                let Some(sql) = self.result_paging.as_ref().map(|p| p.sql.clone()) else {
                    return;
                };
                let want = (held + PAGE_ROWS).min(MAX_HELD_ROWS);
                self.spawn_page_request(PageRequest::Rerun(sql, want));
                self.last_status = Some(if self.result_more() == More::Standby {
                    format!("on a standby · re-running for {want} rows…")
                } else {
                    format!("the cursor had closed · re-running for {want} rows…")
                });
            }
        }
    }

    /// Send a `M` request to the pager; the answer lands as
    /// `AppMsg::PageLoaded` for the active result.
    fn spawn_page_request(&mut self, req: PageRequest) {
        // Behind the connection-failed card the pager, if any, is the
        // session a failed reconnect left: nothing to page on (as `db`).
        let failed = matches!(self.conn_state, ConnState::Failed(_));
        let (Some(pager), Some(result_id), false) = (
            self.pager.clone(),
            self.result_paging.as_ref().map(|p| p.id),
            failed,
        ) else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        let tx = self.msg_tx.clone();
        let generation = self.generation;
        self.query_running = true;
        self.query_started = Some(Instant::now());
        self.last_error = None;
        let gone = || conn::QueryErr::msg("the connection's pager stopped");
        match req {
            PageRequest::Fetch(want) => {
                let rx = pager.fetch(want);
                tokio::spawn(async move {
                    let outcome = match rx.await {
                        Ok(result) => result.map(PageOutcome::More),
                        Err(_) => Err(gone()),
                    };
                    let _ = tx.send(AppMsg::PageLoaded {
                        generation,
                        result_id,
                        outcome,
                    });
                });
            }
            PageRequest::Rerun(sql, want) => {
                self.cursor = None;
                let rx = pager.open(sql, want);
                tokio::spawn(async move {
                    let outcome = match rx.await {
                        Ok(result) => result.map(PageOutcome::Rerun),
                        Err(_) => Err(gone()),
                    };
                    let _ = tx.send(AppMsg::PageLoaded {
                        generation,
                        result_id,
                        outcome,
                    });
                });
            }
        }
    }

    /// `M`'s answer landed: add the page, or replace the rows with the
    /// re-run, and say where that leaves the result.
    pub(crate) fn on_page_loaded(
        &mut self,
        result_id: u64,
        outcome: Result<PageOutcome, conn::QueryErr>,
    ) {
        use crate::query::paging::MAX_HELD_ROWS;
        self.query_running = false;
        self.query_started = None;
        let asked = std::mem::take(&mut self.cancel_requested);
        if self.result_paging.as_ref().map(|p| p.id) != Some(result_id) {
            // The result changed under the fetch (tab switches and new
            // runs wait for it, so this is a stale answer): nothing to
            // add it to, and its cursor feeds nothing.
            if self.cursor.is_some_and(|c| c.result_id == result_id) {
                self.close_cursor();
            }
            return;
        }
        let before = self.grid.row_count();
        let rerun = matches!(outcome, Ok(PageOutcome::Rerun(_)));
        let more = match outcome {
            Err(e) => {
                // The pager rolled the cursor's transaction back.
                self.cursor = None;
                let cancelled =
                    asked && e.detail.as_ref().and_then(|d| d.code.as_deref()) == Some("57014");
                if cancelled {
                    self.last_status =
                        Some("fetch cancelled · the cursor is closed · M re-runs the query".into());
                } else {
                    self.last_error = Some(format!(
                        "fetching more rows failed: {} · M re-runs the query",
                        e.msg
                    ));
                    self.last_error_detail = e.detail;
                }
                return;
            }
            Ok(PageOutcome::More(page)) => {
                self.append_rows(page.rows);
                self.grid.truncated = page.more;
                page.more
            }
            Ok(PageOutcome::Rerun(opened)) => {
                let in_transaction = opened.in_transaction;
                let standby = opened.standby;
                let shadowed = opened.shadowed;
                self.replace_rows(opened.grid);
                if let Some(p) = self.result_paging.as_mut() {
                    p.in_transaction = in_transaction;
                    p.standby = standby;
                }
                if shadowed {
                    // A schema has its own function by a name it calls
                    // now: no more re-runs (see the pager route).
                    self.result_paging = None;
                }
                let now = Instant::now();
                self.cursor = opened.cursor.map(|limits| CursorMirror {
                    result_id,
                    opened: now,
                    last_fetch: now,
                    limits,
                });
                self.grid.truncated
            }
        };
        if !more {
            self.cursor = None;
        } else if let Some(c) = self.cursor.as_mut() {
            c.last_fetch = Instant::now();
        }
        let held = self.grid.row_count();
        if more && held >= MAX_HELD_ROWS {
            self.close_cursor();
            if let Some(p) = self.result_paging.as_mut() {
                p.ceiling = true;
            }
        }
        let added = held.saturating_sub(before);
        let tail = if self.grid.truncated {
            format!(
                " · {}",
                crate::grid::partial_status_note(self.result_more())
            )
        } else {
            " · that is every row".to_string()
        };
        self.last_status = Some(if rerun {
            format!("re-ran the query · {held} row(s){tail}")
        } else {
            format!("+{added} row(s) · {held} held{tail}")
        });
    }

    /// Add a fetched page to the held rows, keeping the active sort and
    /// filter: the sort's original-order copy grows too, and the rows
    /// are re-sorted, so the page lands where the sort says. Without a
    /// sort or filter the selection moves to the first new row.
    fn append_rows(&mut self, rows: Vec<Vec<Cell>>) {
        let first_new = self.grid.rows.len();
        if rows.is_empty() {
            return;
        }
        if let Some(raw) = self.grid_view.raw_rows.as_mut() {
            raw.extend(rows.iter().cloned());
        }
        self.grid.rows.extend(rows);
        self.resort_rows();
        self.rebuild_visible_rows();
        if self.grid_view.sort.is_none() && self.grid_view.filter.is_none() {
            self.grid_state.select(Some(first_new));
        }
    }

    /// A re-run's rows in place of the held ones. The same columns keep
    /// the sort, filter and column cursor; different ones are a new
    /// result to the view.
    fn replace_rows(&mut self, grid: Grid) {
        if grid.columns != self.grid.columns {
            self.grid = grid;
            self.grid_state
                .select(if self.grid.is_empty() { None } else { Some(0) });
            self.reset_grid_view();
            return;
        }
        self.grid_view.raw_rows = self.grid_view.sort.map(|_| grid.rows.clone());
        self.grid = grid;
        self.resort_rows();
        // Bookmarks name rows of the old snapshot.
        self.bookmarks.clear();
        self.rebuild_visible_rows();
    }

    /// Re-apply the active sort to `grid.rows` from its original-order
    /// copy. Bookmarks key on row indices, which a re-sort moves, so
    /// they go when there is a sort to re-apply.
    fn resort_rows(&mut self) {
        let Some((col, asc)) = self.grid_view.sort else {
            return;
        };
        if let Some(raw) = &self.grid_view.raw_rows {
            self.grid.rows = raw.clone();
        }
        crate::grid::sort_rows(&mut self.grid.rows, col, asc);
        self.bookmarks.clear();
    }

    /// A fresh id for a result that ran through the cursor.
    fn next_result_id(&mut self) -> u64 {
        self.result_ids = self.result_ids.wrapping_add(1);
        self.result_ids
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
        // One execution at a time. A second F5 while the first is in
        // flight used to pipeline a second run on the same client — an
        // INSERT pressed twice ran twice. Ctrl-C is the way to get the
        // client back.
        if self.query_running {
            self.last_status = Some("a query is already running · ctrl-c cancels it".into());
            return;
        }
        // `--demo` never has a real client — but a statement typed or
        // pasted there should still go through the exact same guard,
        // batch-split, and pending-confirm machinery a live session
        // uses (a DELETE without WHERE is refused exactly like it
        // would be against a real database — see `spawn_run_demo`).
        // Only the final execution differs: demo answers from the
        // fixture schema instead of a Postgres connection.
        if self.no_session() && !self.demo {
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
        // The single-statement path classifies and executes the VERIFIED
        // piece too, never the raw buffer: `; DROP TABLE t` is one command
        // to the server (it discards the empty statement) but was `Other`
        // to a classifier fed the raw text — a Block downgraded to Confirm.
        let statements = match safety::split_verified(&sql) {
            Ok(statements) => statements,
            Err(e) => {
                tracing::warn!("refusing to run the editor buffer: {e:?}");
                self.last_error = Some(safety::SPLIT_REFUSAL.to_string());
                return;
            }
        };
        if statements.len() > 1 {
            return self.request_run_batch(sql, kind, db.to_string());
        }
        let Some(sql) = statements.into_iter().next() else {
            self.last_error = Some(
                "editor has no statement · e to focus it, then type SQL or paste a log".into(),
            );
            return;
        };

        let decision = safety::evaluate_in_session(&self.safety_config, db, &sql, self.read_only);

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
                self.last_error = Some(self.blocked_message(&decision, db));
            }
            Guard::Confirm => {
                self.pending_run = Some(PendingRun {
                    sql,
                    kind,
                    decision,
                    is_batch: false,
                    summary: None,
                    cell_edit: None,
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
    ///
    /// Two invariants hold here, and the bypass this fixes broke both. The
    /// script is split by `safety::split_verified`, which refuses a script it
    /// cannot account for rather than guessing at the boundaries; and what
    /// gets executed is the **re-joined verified statements**, not the
    /// original buffer, so the server runs exactly the text the classifier
    /// saw.
    fn request_run_batch(&mut self, sql: String, kind: RunKind, db: String) {
        if !matches!(kind, RunKind::Run) {
            self.last_error = Some(format!(
                "{} not supported for multi-statement scripts",
                kind.label()
            ));
            return;
        }
        let statements = match safety::split_verified(&sql) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("batch run refused: {e:?}");
                self.last_error = Some(safety::SPLIT_REFUSAL.to_string());
                return;
            }
        };
        // From here on, `sql` is the checked text — never the raw buffer.
        let sql = safety::join_verified(&statements);
        let decisions: Vec<Decision> = statements
            .iter()
            .map(|s| safety::evaluate_in_session(&self.safety_config, &db, s, self.read_only))
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
            read_only_escape: decisions.iter().any(|d| d.read_only_escape),
            server_control: decisions.iter().any(|d| d.server_control),
        };

        match max_guard {
            Guard::Block => {
                // A script that opens by turning read-only off gets told so:
                // the per-kind summary would blame the wrong statement.
                self.last_error = Some(if synthesized.read_only_escape {
                    self.read_only_escape_message(&db)
                } else if synthesized.server_control {
                    safety::SERVER_CONTROL_REFUSAL.to_string()
                } else {
                    format!("batch blocked by safety: {summary}")
                });
            }
            Guard::Confirm => {
                self.pending_run = Some(PendingRun {
                    sql,
                    kind,
                    decision: synthesized,
                    is_batch: true,
                    summary: Some(summary),
                    cell_edit: None,
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
    /// query. NULLs go where Postgres puts them by default: last
    /// ascending, first descending ([`crate::grid::cmp_cells`]).
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
                crate::grid::sort_rows(&mut self.grid.rows, col, asc);
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
        let Some(cell) = row.get(self.grid_view.col_cursor).cloned() else {
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
        // `WHERE id = NULL` matches nothing: a NULL references no row.
        let Some(value) = cell else {
            self.last_status = Some(format!("{col_name} is NULL here — it references no row"));
            return;
        };
        // Build the parent SELECT. Use the same literal-quoting
        // path as row-as-INSERT for safe value rendering.
        let sql = format!(
            "SELECT * FROM {parent_schema}.{parent_table} WHERE {parent_column} = {value_lit} LIMIT 100;",
            parent_schema = quote_ident(&edge.parent_schema),
            parent_table = quote_ident(&edge.parent_table),
            parent_column = quote_ident(&edge.parent_column),
            value_lit = format_sql_literal(Some(&value)),
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
    ///
    /// Nothing to diff is a notice, not an error: it goes in
    /// `last_status`, so the footer does not grow a `· F2 detail`
    /// pointer to an overlay with nothing in it.
    fn pin_or_diff_result(&mut self) {
        if self.grid.rows.is_empty() {
            self.last_status = Some("no result to diff — run a query first".into());
            return;
        }
        match self.result_diff.pinned.take() {
            None => {
                let n = self.grid.rows.len();
                self.result_diff.pinned = Some(self.current_result_pin());
                self.last_status = Some(format!(
                    "pinned result A · {n} row(s) · run another query, then D to diff"
                ));
            }
            Some(a) => {
                // Baseline present — current grid is B. Put A back
                // (it persists across diffs for iterative work).
                let b = self.current_result_pin();
                let (key, key_is_pk) = choose_diff_key(&self.schema_cache, &a, &b);
                let diff = crate::query::row_diff::diff_rows(&a.rows, &b.rows, &key);
                let summary = format!(
                    "diff · +{} -{} ~{} ={}",
                    diff.added.len(),
                    diff.removed.len(),
                    diff.changed.len(),
                    diff.unchanged
                );
                self.result_diff.active = Some(ResultDiffState {
                    a: a.clone(),
                    b_columns: b.columns,
                    b_rows: b.rows,
                    b_label: b.label,
                    b_source: b.source,
                    b_origins: b.origins,
                    key,
                    key_is_pk,
                    diff,
                });
                self.result_diff.pinned = Some(a);
                self.result_diff.cursor = 0;
                self.mode = Mode::ResultDiff;
                self.last_status = Some(summary);
            }
        }
    }

    /// The current grid as a diff side: its columns, rows, label, and
    /// the source table and column origins the PK key needs.
    fn current_result_pin(&self) -> PinnedResult {
        PinnedResult {
            columns: self.grid.columns.clone(),
            rows: self.grid.rows.clone(),
            label: self.current_result_label(),
            source: self.grid_view.source.clone(),
            origins: self.grid_view.origins.clone(),
        }
    }

    /// `Y` in Normal mode — copy the (sorted, filtered) grid to the
    /// clipboard as CSV. Written through `yank::write_clipboard`.
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
        match yank::write_clipboard(&csv) {
            Ok(()) => {
                self.last_status = Some(format!(
                    "copied {} row(s) to clipboard as CSV{}",
                    export.rows.len(),
                    crate::grid::fetched_only_note(self.grid.truncated)
                ));
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("yank failed: {e}")),
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
    /// `n` / `N` in the grid: through the live filter, else the accepted
    /// find, from the cursor on.
    fn filter_or_find_step(&mut self, forward: bool) {
        if self.grid_view.filter.is_some() {
            return self.filter_step(forward);
        }
        let Some(pat) = self.grid_find.accepted.clone() else {
            self.last_status = Some("nothing to step through · / filter or f find first".into());
            return;
        };
        let matches = compute_grid_find_matches(&self.grid, &self.grid_view.visible_rows, &pat);
        let here = (
            self.grid_state.selected().unwrap_or(0),
            self.grid_view.col_cursor,
        );
        let Some(i) = next_match(&matches, here, forward) else {
            self.last_status = Some(format!("find: {pat} · no match in this result"));
            return;
        };
        let (vi, ci) = matches[i];
        self.grid_state.select(Some(vi));
        self.grid_view.col_cursor = ci;
        self.last_status = Some(format!(
            "find: {pat} · {}/{} match · n/N step",
            i + 1,
            matches.len()
        ));
    }

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
                s.insert(schema_browser_partitions_key(&t.schema, &t.name));
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

    /// The schema cache has no tables. Disconnected, that is the
    /// answer; connected, the cache is stale (tables another session
    /// made never appeared) or the database is empty, so reload it.
    pub(super) fn schema_cache_missing(&mut self) {
        if self.client.is_some() {
            self.reload_schema();
            self.last_status = Some(
                "no tables in the schema cache — reloading it from the server; try again once it lands".into(),
            );
        } else {
            self.last_status = Some("schema cache empty — connect to a database first".into());
        }
    }

    /// Re-read the catalog (`r` in the schema browser): picks up tables
    /// other sessions made since connect. The result is announced.
    pub(super) fn reload_schema(&mut self) {
        if self.no_session() {
            self.last_status = Some("not connected — nothing to reload".into());
            return;
        }
        self.schema_reload_announce = true;
        self.spawn_schema_refresh();
        self.last_status = Some("reloading the schema…".into());
    }

    /// `S` from Normal — opens the schema browser. No-op when the
    /// cache is empty (disconnected / permission failure on the
    /// `pg_catalog` fetch).
    fn start_schema_browser(&mut self) {
        if self.schema_cache.is_empty() {
            self.schema_cache_missing();
            return;
        }
        self.schema_browser.cursor = 0;
        // One schema has nothing to choose between: open it, cursor on
        // its first table.
        if let [only] = self.schema_cache.schemas.as_slice() {
            self.schema_browser.expanded.insert(only.clone());
            if self.schema_cache.tables.iter().any(|t| &t.schema == only) {
                self.schema_browser.cursor = 1;
            }
        }
        self.mode = Mode::SchemaBrowser;
    }

    /// `W` from Normal — open the schema "wizard" (lint).
    /// Re-runs every check on entry so a fresh re-connect picks
    /// up the latest cache. Pure; sub-millisecond on a normal
    /// catalog so there's no async hop.
    fn start_schema_lint(&mut self) {
        if self.schema_cache.is_empty() {
            self.schema_cache_missing();
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
        self.schema_lint.live_note = None;
        self.schema_lint.advisor_note = None;
        self.mode = Mode::SchemaLint;
        // Kick off the live-query checks (LINT101+) and the config
        // advisor (ADV…). Results land via `AppMsg::LiveLintLoaded` /
        // `AppMsg::AdvisorLoaded` and get merged into
        // `schema_lint.findings` — see the handlers. `--demo` has no
        // server to ask: its fixtures answer for one.
        if self.demo {
            self.on_live_lint_loaded(Ok(crate::demo::live_lint()));
            self.on_advisor_loaded(Ok(crate::demo::advisor()));
        } else if let Some(db) = self.db() {
            let tx = self.msg_tx.clone();
            let generation = self.generation;
            tokio::spawn(async move {
                let client = db.ready().await;
                let result = crate::query::lint::fetch_live(&client).await;
                let _ = tx.send(AppMsg::LiveLintLoaded { generation, result });
                let th = crate::query::advisor::Thresholds::default();
                let result = crate::query::advisor::fetch(&client, &th).await;
                let _ = tx.send(AppMsg::AdvisorLoaded { generation, result });
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
        if self.demo {
            self.slow_queries.cursor = 0;
            self.slow_queries.error = None;
            self.mode = Mode::SlowQueries;
            return self.on_slow_queries_loaded(Ok(crate::demo::slow_queries()));
        }
        let Some(db) = self.db() else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        self.slow_queries.cursor = 0;
        self.slow_queries.error = None;
        self.mode = Mode::SlowQueries;
        self.last_status = Some("loading pg_stat_statements…".into());
        self.spawn_slow_queries_load(db);
    }

    /// `r` inside the slow-queries panel — refresh in place.
    fn refresh_slow_queries(&mut self) {
        if self.demo {
            return self.start_slow_queries();
        }
        let Some(db) = self.db() else {
            return;
        };
        self.last_status = Some("refreshing pg_stat_statements…".into());
        self.spawn_slow_queries_load(db);
    }

    /// `E` from Normal: open the environments overview and gather. Needs
    /// no connection — the sources are the config and the aws CLI.
    pub(super) fn start_environments(&mut self) {
        // The cursor stays where it was: the overview reopens on the row
        // last used, as ebman's main screen does.
        self.environments.opened_from = (self.mode == Mode::ConnPick).then_some(Mode::ConnPick);
        self.mode = Mode::Environments;
        self.refresh_environments();
    }

    fn refresh_environments(&mut self) {
        if self.environments.loading {
            return;
        }
        self.environments.loading = true;
        self.last_status = Some("listing environments…".into());
        if !self.spawn_environments_load() {
            self.environments.loading = false;
            self.environments.error =
                Some("environments are not available in this session".to_string());
            self.last_status = None;
        }
    }

    /// Enter in the environments overview: connect to the database under
    /// the cursor. A replica opens read-only; anything else gets its
    /// database's safety profile, as any connection does. With a
    /// secret, the password is read first and the connect waits for it.
    fn connect_to_environment_target(&mut self) {
        if self.environments.connecting.is_some() {
            self.last_status = Some("already connecting — wait, or q to cancel".into());
            return;
        }
        let Some((env, target)) = self.environments.selected() else {
            return;
        };
        let what = format!("{} {}/{}", env.name, target.server, target.database);
        self.remember_env_selection();
        let spec = match &target.connect {
            Ok(spec) => spec.clone(),
            Err(why) => {
                self.last_error = Some(format!("can't connect to {what}: {why}"));
                return;
            }
        };
        let read_only = target.role == crate::mcp::connections::Role::Replica;
        let mut origin = format!(
            "environment {what} ({}{})",
            target.role.label(),
            if read_only { ", read-only" } else { "" }
        );
        let Some(secret) = spec.secret.clone() else {
            let mut dsn = spec.dsn.clone();
            if let Err(e) = spec.apply_password_env(&mut dsn) {
                self.last_error = Some(format!("can't connect to {what}: {e}"));
                return;
            }
            if let Some(var) = &spec.password_env {
                origin.push_str(&format!(" · password from ${var}"));
            }
            self.connect_from_environments(dsn, origin, read_only);
            return;
        };
        origin.push_str(&format!(" · creds from {}", secret.provenance()));
        let token = self.environments.connect_token.wrapping_add(1);
        self.environments.connect_token = token;
        self.environments.connecting = Some(PendingEnvConnect {
            token,
            dsn: spec.dsn,
            origin,
            read_only,
        });
        if self.spawn_secret_fetch(token, secret.clone()) {
            self.last_status = Some(format!("{what}: reading secret {}…", secret.id));
        } else {
            self.environments.connecting = None;
            self.last_error = Some(format!(
                "can't connect to {what}: secrets are not available in this session"
            ));
        }
    }

    /// Leave the overview for the database chosen in it. The footer's
    /// overview text (`environments · 2 · 3 databases`, or the secret
    /// being read) goes with it: it would sit under the start card,
    /// describing a screen that's gone.
    fn connect_from_environments(&mut self, dsn: Dsn, origin: String, read_only: bool) {
        self.environments.opened_from = None;
        self.last_status = None;
        self.connect_to_target(dsn, origin, read_only);
    }

    /// `l` in the overview: recent errors from the focused server's RDS
    /// log, in a view over the list.
    pub(super) fn start_env_logs(&mut self) {
        let Some((env, t)) = self.environments.selected() else {
            return;
        };
        if !env.source.starts_with("RDS") {
            self.last_status = Some(format!(
                "logs come from RDS; {} is a {} environment",
                env.name, env.source
            ));
            return;
        }
        let Some(fetcher) = self.env_log_fetcher.clone() else {
            self.last_error = Some("logs are not available in this session".into());
            return;
        };
        let (environment, server) = (env.name.clone(), t.server.clone());
        self.environments.logs = Some(LogsView {
            environment: environment.clone(),
            server: server.clone(),
            loading: true,
            ..LogsView::default()
        });
        self.last_status = Some(format!("reading {server}'s log…"));
        if fetcher.immediate {
            let result = (fetcher.fetch)(&environment, &server);
            self.on_env_logs(&environment, &server, result);
        } else {
            let tx = self.msg_tx.clone();
            tokio::task::spawn_blocking(move || {
                let result = (fetcher.fetch)(&environment, &server);
                let _ = tx.send(AppMsg::EnvLogs {
                    environment,
                    server,
                    result,
                });
            });
        }
    }

    /// `P` in the overview: plan the focused database's pending patches,
    /// in a view that asks before anything runs.
    pub(super) fn start_env_patch(&mut self) {
        let Some((env, t)) = self.environments.selected() else {
            return;
        };
        let Some(patcher) = self.env_patcher.clone() else {
            self.last_error = Some("applying patches is not available in this session".into());
            return;
        };
        let (environment, server, database) =
            (env.name.clone(), t.server.clone(), t.database.clone());
        self.plan_env_patch(patcher, environment, server, database, false);
    }

    /// `o` on a plan that stopped at a patch needing no transaction: plan
    /// again, letting such patches run outside one.
    fn replan_env_patch_outside(&mut self) {
        let (Some(patcher), Some(v)) = (self.env_patcher.clone(), self.environments.patch.as_ref())
        else {
            return;
        };
        let (environment, server, database) =
            (v.environment.clone(), v.server.clone(), v.database.clone());
        self.plan_env_patch(patcher, environment, server, database, true);
    }

    fn plan_env_patch(
        &mut self,
        patcher: crate::app::EnvPatcher,
        environment: String,
        server: String,
        database: String,
        outside: bool,
    ) {
        self.environments.patch = Some(PatchView {
            environment: environment.clone(),
            server: server.clone(),
            database: database.clone(),
            stage: PatchStage::Planning,
            typed: String::new(),
            scroll: 0,
            cancel: Default::default(),
            cancel_armed: false,
            mode: Default::default(),
        });
        self.last_status = Some(format!("planning {server}/{database}'s patches…"));
        let fut = (patcher.plan)(
            environment.clone(),
            server.clone(),
            database.clone(),
            outside,
            self.safety_config.clone(),
        );
        if patcher.immediate {
            let result = futures::executor::block_on(fut);
            self.on_env_patch_planned(&environment, &server, &database, result);
        } else {
            let tx = self.msg_tx.clone();
            tokio::spawn(async move {
                let result = fut.await;
                let _ = tx.send(AppMsg::EnvPatchPlanned {
                    environment,
                    server,
                    database,
                    result,
                });
            });
        }
    }

    fn on_env_patch_planned(
        &mut self,
        environment: &str,
        server: &str,
        database: &str,
        result: Result<crate::inventory::patching::PatchPlan, String>,
    ) {
        // Closed, or another database's, while it planned: not ours.
        let Some(v) = self.environments.patch.as_mut() else {
            return;
        };
        if (
            v.environment.as_str(),
            v.server.as_str(),
            v.database.as_str(),
        ) != (environment, server, database)
            || !matches!(v.stage, PatchStage::Planning)
        {
            return;
        }
        self.last_status = Some(patch_hint(&result));
        v.stage = PatchStage::Plan(result);
        v.scroll = 0;
    }

    /// The operator confirmed the plan in the view: run it.
    fn confirm_env_patch(&mut self) {
        let Some(patcher) = self.env_patcher.clone() else {
            return;
        };
        let Some(v) = self.environments.patch.as_mut() else {
            return;
        };
        let PatchStage::Plan(Ok(plan)) = &v.stage else {
            return;
        };
        if plan.patches.is_empty() {
            return;
        }
        if plan.needs_typed() && v.typed != plan.environment {
            self.last_status = Some(format!(
                "that isn't {} — type the name exactly, or Esc to cancel",
                plan.environment
            ));
            return;
        }
        let plan = plan.clone();
        v.stage = PatchStage::Applying(plan.clone());
        v.cancel = crate::inventory::patching::CancelSlot::default();
        v.cancel_armed = false;
        let (cancel, mode) = (v.cancel.clone(), v.mode);
        let doing = match mode {
            RunMode::Apply => "applying",
            RunMode::DryRun => "dry-running",
            RunMode::Copy => "copying the database, then applying",
        };
        self.last_status = Some(format!(
            "{doing} {} patch(es) to {}/{}… · Esc twice cancels",
            plan.patches.len(),
            plan.server,
            plan.database
        ));
        tracing::info!(target: "pgman::patching", environment = %plan.environment, server = %plan.server, database = %plan.database, patches = plan.patches.len(), mode = mode.name(), "patches confirmed");
        let fut = (patcher.apply)(plan.clone(), mode, cancel, self.safety_config.clone());
        if patcher.immediate {
            let report = futures::executor::block_on(fut);
            self.on_env_patched(plan, report);
        } else {
            let tx = self.msg_tx.clone();
            tokio::spawn(async move {
                // A panic in the run still ends the Applying stage.
                let report = tokio::spawn(fut).await.unwrap_or_else(|e| {
                    crate::inventory::patching::ApplyReport {
                        error: Some(format!("the apply task failed: {e}")),
                        mode,
                        ..Default::default()
                    }
                });
                let _ = tx.send(AppMsg::EnvPatched { plan, report });
            });
        }
    }

    fn on_env_patched(
        &mut self,
        plan: crate::inventory::patching::PatchPlan,
        report: crate::inventory::patching::ApplyReport,
    ) {
        let summary = format!("{}/{}: {}", plan.server, plan.database, report.summary());
        match self.environments.patch.as_mut() {
            Some(v) if matches!(&v.stage, PatchStage::Applying(p) if *p == plan) => {
                v.stage = PatchStage::Done(plan, report.clone());
                v.scroll = 0;
            }
            // The view can't close while applying, but say so anyway.
            _ => {}
        }
        if report.failed().is_some() || report.error.is_some() {
            self.last_error = Some(summary);
        } else {
            // An earlier run's failure would otherwise sit over this one.
            self.last_error = None;
            self.last_error_detail = None;
            self.last_status = Some(summary);
        }
        // The pending counts moved.
        if self.environments.reports.iter().any(|r| r.probe.is_some()) {
            self.start_env_probes();
        }
    }

    /// A key while the patch view is open.
    pub(super) fn on_env_patch_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Some(v) = self.environments.patch.as_mut() else {
            return;
        };
        let typing =
            matches!(&v.stage, PatchStage::Plan(Ok(p)) if p.needs_typed() && !p.patches.is_empty());
        let confirmable = matches!(&v.stage, PatchStage::Plan(Ok(p)) if !p.patches.is_empty());
        let outside_helps = matches!(&v.stage, PatchStage::Plan(Ok(p)) if p.outside_would_help);
        let ctrl = key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Down => v.scroll = v.scroll.saturating_add(1),
            KeyCode::Up => v.scroll = v.scroll.saturating_sub(1),
            // Esc twice cancels the running statement; anything else
            // between disarms.
            KeyCode::Esc if matches!(v.stage, PatchStage::Applying(_)) && !v.cancel_armed => {
                v.cancel_armed = true;
                self.last_status = Some(
                    "Esc again cancels the running statement: in a transaction it rolls back, \
                     outside one what ran stays · any other key keeps going"
                        .into(),
                );
            }
            KeyCode::Esc if matches!(v.stage, PatchStage::Applying(_)) => {
                v.cancel_armed = false;
                self.last_status = Some(if v.cancel.cancel() {
                    tracing::warn!(target: "pgman::patching", environment = %v.environment, server = %v.server, database = %v.database, "operator cancelled the running patch statement");
                    "cancel sent — the run stops when the server gives the statement up".into()
                } else {
                    "cancel requested — the run stops before its first patch".into()
                });
            }
            _ if matches!(v.stage, PatchStage::Applying(_)) => {
                v.cancel_armed = false;
                self.last_status =
                    Some("applying — wait for it to finish, or Esc twice to cancel".into());
            }
            KeyCode::Esc => {
                self.environments.patch = None;
                self.last_status = None;
            }
            // While a name is typed, `o` is a letter of it: ctrl-o then.
            KeyCode::Char('o') if outside_helps && (ctrl || !typing) => {
                self.replan_env_patch_outside()
            }
            KeyCode::Tab if confirmable => {
                v.mode = v.mode.next();
                self.last_status = Some(mode_hint(v.mode).into());
            }
            KeyCode::Backspace if typing => {
                v.typed.pop();
            }
            KeyCode::Enter if typing => self.confirm_env_patch(),
            KeyCode::Char(c) if typing => v.typed.push(c),
            KeyCode::Char('y') if confirmable => self.confirm_env_patch(),
            KeyCode::Char('q') | KeyCode::Char('n') => {
                self.environments.patch = None;
                self.last_status = None;
            }
            KeyCode::Char('j') => v.scroll = v.scroll.saturating_add(1),
            KeyCode::Char('k') => v.scroll = v.scroll.saturating_sub(1),
            _ => {}
        }
    }

    fn on_env_logs(
        &mut self,
        environment: &str,
        server: &str,
        result: Result<Vec<crate::inventory::rds_logs::LogError>, String>,
    ) {
        // Closed, or moved to another server, while it read: not ours.
        let Some(v) = self.environments.logs.as_mut() else {
            return;
        };
        if v.environment != environment || v.server != server {
            return;
        }
        v.loading = false;
        self.last_status = Some(match &result {
            Ok(e) if e.is_empty() => format!("{server}: no errors in its recent log"),
            Ok(e) => format!(
                "{server}: {} distinct error(s) · j/k scroll · r again · q back",
                e.len()
            ),
            Err(_) => format!("{server}: log not read"),
        });
        v.result = Some(result);
        v.scroll = 0;
    }

    /// `d` in the environments overview: discover environments from the
    /// ambient AWS account and open the discovery view over the overview.
    fn start_env_discovery(&mut self, scope: crate::aws_cli::AwsScope) {
        if self
            .environments
            .discovery
            .as_ref()
            .is_some_and(|d| d.loading)
        {
            return;
        }
        let Some(discoverer) = self.env_discoverer.clone() else {
            self.last_error = Some("discovery is not available in this session".into());
            return;
        };
        let scopes = self
            .environments
            .discovery
            .take()
            .map(|d| d.scopes)
            .unwrap_or_default();
        self.last_status = Some(format!(
            "discovering with {}: RDS, secret names, EC2 hosts…",
            crate::inventory::discover::scope_label(&scope)
        ));
        let token = self.environments.discovery_token.wrapping_add(1);
        self.environments.discovery_token = token;
        self.environments.discovery = Some(DiscoveryUi {
            token,
            loading: true,
            scope: scope.clone(),
            scopes,
            ..DiscoveryUi::default()
        });
        if discoverer.immediate {
            self.on_env_discovered(token, (discoverer.discover)(&scope));
        } else {
            let tx = self.msg_tx.clone();
            tokio::task::spawn_blocking(move || {
                let result = (discoverer.discover)(&scope);
                let _ = tx.send(AppMsg::EnvDiscovered { token, result });
            });
        }
    }

    fn on_env_discovered(
        &mut self,
        token: u64,
        result: Result<crate::inventory::discover::Found, String>,
    ) {
        // Closed while it ran (nothing to show it in), or closed and
        // opened again (it belongs to a discovery that's gone).
        let Some(d) = self
            .environments
            .discovery
            .as_mut()
            .filter(|d| d.token == token)
        else {
            return;
        };
        d.loading = false;
        match result {
            Ok(found) => {
                let new = found
                    .proposals
                    .iter()
                    .filter(|p| p.covered_by.is_none())
                    .count();
                self.last_status = Some(format!(
                    "found {} · {new} new · space choose · s save",
                    found.proposals.len()
                ));
                d.chosen = vec![false; found.proposals.len()];
                d.cursor = 0;
                if !found.scopes.is_empty() {
                    d.scopes = found.scopes.clone();
                }
                d.found = Some(found);
                d.error = None;
            }
            Err(e) => {
                self.last_status = None;
                d.error = Some(e);
            }
        }
    }

    fn toggle_env_proposal(&mut self) {
        let Some(d) = self.environments.discovery.as_mut() else {
            return;
        };
        let Some(p) = d.selected() else {
            return;
        };
        if let Some(by) = &p.covered_by {
            self.last_status = Some(format!(
                "{} is already covered by environment {by}",
                p.env.name
            ));
            return;
        }
        if let Some(c) = d.chosen.get_mut(d.cursor) {
            *c = !*c;
        }
    }

    /// `s` in the discovery view: append the chosen proposals, close the
    /// view and list the environments again.
    fn save_env_proposals(&mut self) {
        let Some(d) = self.environments.discovery.as_ref() else {
            return;
        };
        let chosen: Vec<&crate::inventory::discover::Proposal> = d
            .proposals()
            .iter()
            .zip(&d.chosen)
            .filter(|(_, c)| **c)
            .map(|(p, _)| p)
            .collect();
        if chosen.is_empty() {
            self.last_status = Some("nothing chosen — space chooses an environment".into());
            return;
        }
        let Some(discoverer) = self.env_discoverer.clone() else {
            return;
        };
        let names: Vec<String> = chosen.iter().map(|p| p.env.name.clone()).collect();
        match (discoverer.save)(&chosen) {
            Ok(path) => {
                self.environments.discovery = None;
                self.refresh_environments();
                self.last_status =
                    Some(format!("added {} to {}", names.join(", "), path.display()));
            }
            Err(e) => self.last_error = Some(format!("not saved: {e}")),
        }
    }

    /// After a listing: probe every environment that opted in, marking
    /// its rows probing (or via-replica) until the answers land.
    fn start_env_probes(&mut self) {
        use crate::inventory::probe::{plan, Step};
        self.environments.probe_seq = self.environments.probe_seq.wrapping_add(1);
        let seq = self.environments.probe_seq;
        let prober = self.env_prober.clone();
        let mut states = Vec::new();
        let mut to_run = Vec::new();
        for (i, r) in self.environments.reports.iter().enumerate() {
            if r.probe.is_none() {
                states.push(vec![ProbeState::Off; r.targets.len()]);
                continue;
            }
            if prober.is_none() {
                states.push(vec![
                    ProbeState::Done(Err(
                        "probes are not available in this session".into()
                    ));
                    r.targets.len()
                ]);
                continue;
            }
            let last = &self.environments.last_probe;
            states.push(
                plan(&r.targets)
                    .into_iter()
                    .zip(&r.targets)
                    .map(|(s, t)| match s {
                        Step::Connect => last
                            .get(&(r.name.clone(), t.server.clone(), t.database.clone()))
                            .cloned()
                            .unwrap_or(ProbeState::Probing),
                        Step::ViaReplica => ProbeState::ViaReplica,
                        // Nothing to read: the row's status says why.
                        Step::NotRunning => ProbeState::Off,
                    })
                    .collect(),
            );
            to_run.push(i);
        }
        self.environments.probes = states;
        let Some(prober) = prober else {
            self.environments.probes_in_flight = 0;
            return;
        };
        self.environments.probes_in_flight = to_run.len();
        for i in to_run {
            let report = self.environments.reports[i].clone();
            match &prober {
                EnvProber::Immediate(f) => {
                    let outcomes = f(&report);
                    self.on_env_probed(seq, i, outcomes);
                }
                EnvProber::Async(f) => {
                    let fut = f(report);
                    let tx = self.msg_tx.clone();
                    tokio::spawn(async move {
                        let outcomes = fut.await;
                        let _ = tx.send(AppMsg::EnvProbed {
                            seq,
                            report: i,
                            outcomes,
                        });
                    });
                }
            }
        }
    }

    fn on_env_probed(
        &mut self,
        seq: u64,
        report: usize,
        outcomes: Vec<crate::inventory::probe::Outcome>,
    ) {
        if seq != self.environments.probe_seq {
            return;
        }
        self.environments.probes_in_flight = self.environments.probes_in_flight.saturating_sub(1);
        let Some(states) = self.environments.probes.get_mut(report) else {
            return;
        };
        let Some(r) = self.environments.reports.get(report) else {
            return;
        };
        for ((state, outcome), t) in states.iter_mut().zip(outcomes).zip(&r.targets) {
            let Some(result) = outcome else { continue };
            // Only a standby reports lag (it is NULL on a primary), so
            // every reading here is a replica's, whatever its role says.
            if let Ok(p) = &result {
                if let Some(lag) = p.lag_secs {
                    let h = self
                        .environments
                        .lag_history
                        .entry((r.name.clone(), t.server.clone(), t.database.clone()))
                        .or_default();
                    h.push(lag);
                    if h.len() > LAG_HISTORY {
                        h.remove(0);
                    }
                }
            }
            *state = ProbeState::done(result);
            self.environments.last_probe.insert(
                (r.name.clone(), t.server.clone(), t.database.clone()),
                state.clone(),
            );
        }
    }

    /// The secret for a pending environment connect arrived.
    fn on_env_secret(
        &mut self,
        token: u64,
        result: Result<crate::creds::aws_secrets::SecretCreds, String>,
    ) {
        let Some(pending) = self.environments.connecting.take_if(|p| p.token == token) else {
            return;
        };
        match result {
            Ok(creds) => {
                let mut dsn = pending.dsn;
                crate::creds::aws_secrets::apply(&mut dsn, creds);
                self.connect_from_environments(dsn, pending.origin, pending.read_only);
            }
            Err(e) => {
                self.last_status = None;
                self.last_error = Some(format!("secret not read: {e}"));
            }
        }
    }

    /// Start on the overview — pgman's home when environments are
    /// configured — with the cursor going to `last` once the listing
    /// lands.
    pub fn open_environments_at_start(&mut self, last: Option<EnvSelection>) {
        self.environments.restore = last;
        self.start_environments();
    }

    /// Keep the row under the cursor for the next run. Best effort: a
    /// write that fails costs only the convenience.
    pub(super) fn remember_env_selection(&self) {
        let (Some(path), Some(sel)) =
            (&self.environments_state_file, self.environments.selection())
        else {
            return;
        };
        if let Err(e) = crate::util::write_private(path, &sel.to_line()) {
            tracing::warn!("could not save the environments selection: {e}");
        }
    }

    /// `L` from Normal — load + open the active-sessions panel.
    fn start_sessions(&mut self) {
        if self.demo {
            self.sessions.cursor = 0;
            self.mode = Mode::Sessions;
            return self.on_sessions_loaded(Ok(crate::demo::sessions()));
        }
        let Some(db) = self.db() else {
            self.last_error = Some(self.not_connected_message());
            return;
        };
        self.sessions.cursor = 0;
        self.mode = Mode::Sessions;
        self.last_status = Some("loading pg_stat_activity…".into());
        self.spawn_sessions_load(db);
    }

    fn refresh_sessions(&mut self) {
        if self.demo {
            return self.start_sessions();
        }
        let Some(db) = self.db() else {
            return;
        };
        self.last_status = Some("refreshing pg_stat_activity…".into());
        self.spawn_sessions_load(db);
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
            Some("find:    · type to search · ↑↓ jump · enter accept · esc cancel".into());
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
            "find: {pat}  · {pos}/{n} match · ↑↓ jump · enter accept · esc cancel"
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

    /// Rows the grid is currently showing — the filtered set, not
    /// `grid.row_count()`. The selection indexes into THIS list (the
    /// renderer walks `visible_rows`), so clamping to the unfiltered
    /// count let `j` step past the last visible row onto rows that
    /// weren't drawn: the highlight vanished and Enter opened nothing.
    fn visible_row_count(&self) -> usize {
        self.grid_view.visible_rows.len()
    }

    fn scroll(&mut self, delta: isize) {
        let count = self.visible_row_count();
        if count == 0 {
            return;
        }
        let current = self.grid_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, count as isize - 1);
        self.grid_state.select(Some(next as usize));
    }

    fn select_row(&mut self, idx: usize) {
        let count = self.visible_row_count();
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

/// `(line_index, display_column)` of `cursor` within `buffer` — the
/// terminal column the cursor should be painted at, summing each
/// char's `UnicodeWidthChar::width`. `cursor_position` counts chars,
/// which is what indexing into the line needs; painted as a column it
/// left the cursor one cell short of every wide glyph before it
/// (`SELECT '東京都'` put the cursor over the closing quote).
pub(crate) fn cursor_display_position(buffer: &str, cursor: usize) -> (usize, usize) {
    use unicode_width::UnicodeWidthChar;
    let prefix = &buffer[..cursor.min(buffer.len())];
    let mut line = 0;
    let mut col = 0;
    for c in prefix.chars() {
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += UnicodeWidthChar::width(c).unwrap_or(0);
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

/// The first executable file named `name` in the `PATH`-shaped
/// `path_var` (`:`-separated on unix), or `None`. What `which` does,
/// with the search list passed in so a test can stub a binary
/// present or absent without rebinding the process's `$PATH`.
pub fn find_on_path_in(name: &str, path_var: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    std::env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// RAII guard for the `$EDITOR` scratch directory: removes the whole
/// directory (and anything the editor left in it, e.g. a swap file)
/// on drop, so every exit path — success, the read-back failure that
/// used to `return` before its `remove_file`, a spawn failure — gets
/// cleanup without each branch having to remember it. Best-effort:
/// a failed removal (directory already gone, permissions changed
/// under us) is not itself an error worth surfacing.
struct EditScratchDir(std::path::PathBuf);

impl Drop for EditScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Write `input` to a temp file, invoke `editor_cmd` (with optional
/// args, whitespace-split) against the file, read it back. The
/// extracted core of `\e` external-editor handling — same shape as
/// `pg_format_via`: takes the editor command as a parameter so the
/// integration test can drive a stubbed editor without `$EDITOR` env
/// gymnastics.
///
/// The buffer can hold literal query text — table data, `WHERE`
/// clause values — so it gets the same treatment as everything else
/// under `util::write_private`: a private (`0700`) per-invocation
/// scratch directory rather than a predictable
/// `$TMPDIR/pgman-edit-<pid>-<seq>.sql` name any local user could
/// read while the editor has it open, holding one `0600` file
/// written atomically. `EditScratchDir` removes the directory on
/// every exit path, including ones a plain `return` used to skip.
pub fn external_edit_via(input: &str, editor_cmd: &str) -> Result<String, String> {
    use std::process::Command;
    let (prog, args) = split_editor_command(editor_cmd);
    // Per-call scratch dir: pid + wall-clock nanos + a monotonic
    // counter so parallel invocations (cargo's test runner spawns
    // several test threads in one process) don't collide.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir_path = std::env::temp_dir().join(format!(
        "pgman-edit-{}-{}-{}",
        std::process::id(),
        nanos,
        seq
    ));
    crate::util::create_dir_all_private(&dir_path)
        .map_err(|e| format!("could not create scratch dir for {prog}: {e}"))?;
    // From here on, every return path — the `?`s below included —
    // runs this guard's `Drop` and removes `dir_path`.
    let _scratch = EditScratchDir(dir_path.clone());
    let path = dir_path.join("buffer.sql");

    crate::util::write_private(&path, input)
        .map_err(|e| format!("could not write temp file for {prog}: {e}"))?;
    let status = Command::new(&prog)
        .args(&args)
        .arg(&path)
        .status()
        .map_err(|e| format!("could not run {prog}: {e}"))?;
    if !status.success() {
        return Err(format!(
            "{prog} exited with status {status} — buffer unchanged"
        ));
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("could not read {prog} output: {e}"))?;
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

/// How often `R` in the environments overview probes again. Each round
/// connects to every probed database, so it's slower than the panels'.
pub const ENV_AUTO_PROBE_INTERVAL: Duration = Duration::from_secs(30);

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
///
/// This bounds event COUNT only. Per-event SIZE is bounded
/// separately, at ingest — `tap::TAP_MAX_SQL_BYTES` /
/// `tap::TAP_MAX_FIELD_BYTES` truncate every string-shaped
/// field on the way in (`tap::enforce_field_caps`), and the
/// transport frame/body caps (`tap::TAP_MAX_FRAME_BYTES`,
/// `tap::OTLP_MAX_BODY_BYTES`) bound a single frame before that
/// even runs. Before both existed, this cap alone still allowed
/// a multi-GB ring: 2000 events x a 1 MiB `sql` string each.
/// And with every field at its cap, 2000 events is still ~400
/// MiB — so the ring's *text* is budgeted too, by
/// `tap::TAP_RING_MAX_BYTES` (`tap::push_bounded`).
pub const TAP_CAP: usize = 2000;

/// Most messages the run loop applies between two draws: the one it
/// waited for, plus up to this many − 1 already queued behind it
/// ([`App::drain_pending_msgs`]). Large enough that a tap burst lands
/// in a handful of frames, small enough that a key press waits at most
/// one batch. Tap events are batched to the same cap
/// ([`drain_tap_batch`]).
pub const MSG_DRAIN_PER_FRAME: usize = 1024;

/// Capacity of the tap adapter → run loop channel
/// ([`App::tap_tx_clone`]). Bounded so an App that falls behind holds
/// at most this many events here; the backlog past it waits in the
/// listeners' own channel (`tap::TAP_CHANNEL_CAPACITY`), whose
/// overflow is dropped and counted. A quarter of a frame's batch, so a
/// full channel is emptied in one wake.
pub const TAP_APP_CHANNEL_CAPACITY: usize = 256;

/// `first`, plus the tap events already queued behind it, up to
/// [`MSG_DRAIN_PER_FRAME`] in all — one frame's worth for
/// [`App::on_tap_events`]. Never waits.
fn drain_tap_batch(
    first: crate::tap::TapEvent,
    rx: &mut mpsc::Receiver<crate::tap::TapEvent>,
) -> Vec<crate::tap::TapEvent> {
    let mut batch = vec![first];
    while batch.len() < MSG_DRAIN_PER_FRAME {
        match rx.try_recv() {
            Ok(event) => batch.push(event),
            Err(_) => break,
        }
    }
    batch
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

/// Best-effort read of a saved editor draft. `None` when the file is
/// absent, unreadable or empty — the operator gets a fresh blank
/// buffer in each case rather than a startup error. Path-parameterised
/// so tests use a unique temp file, never the real `draft.sql`.
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
/// so a deliberate Ctrl-U + quit clears the saved draft. Same atomic-rename
/// guarantee — a crash mid-write leaves either the old file intact
/// or the new file complete, never a truncated half-write.
pub fn persist_draft_to(path: &std::path::Path, buf: &str) -> std::io::Result<()> {
    crate::util::write_private(path, buf)
}

/// What a draft save should do — see [`plan_draft_write`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftWrite {
    /// This window hasn't changed the draft: write nothing.
    Skip,
    /// Write the buffer to the draft file.
    Write,
    /// Another window wrote the draft file since this one last read
    /// or wrote it, and this window has edits of its own: write them
    /// to a sibling file instead of clobbering the other window's.
    Divert,
}

/// Pure: decide a draft save. `synced` is the text this window last
/// read from / wrote to the draft file, `on_disk` what the file holds
/// now (`""` when it's missing).
///
/// ```
/// use pgman::app::{plan_draft_write, DraftWrite};
/// // Untouched window: never writes, even if another window did.
/// assert_eq!(plan_draft_write("old", "old", "new"), DraftWrite::Skip);
/// // Edited, nobody else wrote: write.
/// assert_eq!(plan_draft_write("mine", "old", "old"), DraftWrite::Write);
/// // Edited, and another window wrote meanwhile: don't clobber.
/// assert_eq!(plan_draft_write("mine", "old", "theirs"), DraftWrite::Divert);
/// ```
pub fn plan_draft_write(buffer: &str, synced: &str, on_disk: &str) -> DraftWrite {
    if buffer == synced {
        DraftWrite::Skip
    } else if on_disk == synced || on_disk == buffer {
        DraftWrite::Write
    } else {
        DraftWrite::Divert
    }
}

/// Pure: the sibling a diverted draft goes to —
/// `draft-YYYYMMDD-HHMMSS.sql` beside `draft_file`, with `-N` appended
/// for `n > 1` so two diverts in the same second don't collide.
pub fn draft_sibling_path(
    draft_file: &std::path::Path,
    unix_secs: u64,
    n: u32,
) -> std::path::PathBuf {
    let stamp: String = format_unix_secs_utc(unix_secs)
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let (date, time) = stamp.split_at(8.min(stamp.len()));
    let suffix = if n > 1 {
        format!("-{n}")
    } else {
        String::new()
    };
    draft_file.with_file_name(format!("draft-{date}-{time}{suffix}.sql"))
}

impl App {
    /// Restore the editor buffer from `draft_file` (best-effort) and
    /// remember it as the synced text, so quitting without touching
    /// it writes nothing.
    pub fn restore_draft(&mut self) {
        if let Some(draft) = load_draft_from(&self.draft_file) {
            self.editor.cursor = draft.len();
            self.editor.buffer = draft.clone();
            self.draft_synced = draft;
        }
    }

    /// Save the editor buffer per [`plan_draft_write`]. On a divert
    /// the sibling becomes this window's draft file from then on, so
    /// later saves keep going there rather than spawning one per tick.
    pub fn save_draft(&mut self) -> std::io::Result<()> {
        let on_disk = std::fs::read_to_string(&self.draft_file).unwrap_or_default();
        match plan_draft_write(&self.editor.buffer, &self.draft_synced, &on_disk) {
            DraftWrite::Skip => return Ok(()),
            DraftWrite::Write => persist_draft_to(&self.draft_file, &self.editor.buffer)?,
            DraftWrite::Divert => {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let sibling = (1..)
                    .map(|n| draft_sibling_path(&self.draft_file, secs, n))
                    .find(|p| !p.exists())
                    .expect("an unbounded range always finds a free name");
                persist_draft_to(&sibling, &self.editor.buffer)?;
                let name = sibling
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                tracing::warn!(
                    "{} changed in another pgman window; this window's draft saved to {} instead",
                    self.draft_file.display(),
                    sibling.display()
                );
                self.last_status = Some(format!(
                    "another pgman window saved the draft — this one's is kept in {name}"
                ));
                self.draft_file = sibling;
            }
        }
        self.draft_synced = self.editor.buffer.clone();
        Ok(())
    }
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

/// How a result can continue: from its paging record and the result the
/// open cursor (if any) belongs to. Pure.
pub(crate) fn result_more(
    paging: Option<&ResultPaging>,
    cursor_result: Option<u64>,
) -> crate::grid::More {
    use crate::grid::More;
    match paging {
        None => More::Unpaged,
        Some(p) if p.in_transaction => More::InTransaction,
        Some(p) if p.ceiling => More::Ceiling,
        Some(p) if p.standby => More::Standby,
        Some(p) if cursor_result == Some(p.id) => More::CursorOpen,
        Some(_) => More::CursorClosed,
    }
}

/// What `M` asks the pager for.
enum PageRequest {
    /// The next `n` rows of the open cursor.
    Fetch(usize),
    /// The statement again, behind a fresh cursor, for its first `n` rows.
    Rerun(String, usize),
}

/// [`execute`], keeping the column origins of the one kind of result a
/// cell can be edited in: a plain single-statement run outside a
/// transaction. Every other run's are empty.
async fn execute_with_origins(
    client: &tokio_postgres::Client,
    sql: &str,
    kind: RunKind,
    decision: &Decision,
    is_batch: bool,
) -> Result<(Grid, Vec<Option<crate::grid::ColumnOrigin>>), conn::QueryErr> {
    if kind == RunKind::Run && !is_batch && !decision.wrap_in_tx {
        return conn::run_statement_with_origins(client, sql).await;
    }
    execute(client, sql, kind, decision, is_batch)
        .await
        .map(|grid| (grid, Vec::new()))
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
    /// Once tap events could carry arbitrarily large fields
    /// (pre-truncation-at-ingest, see `tap::enforce_field_caps`),
    /// re-aggregating a full `TAP_CAP`-sized ring from scratch
    /// every frame stopped being the "sub-millisecond" cost this
    /// comment used to promise — measured ~1.7s/frame at 200 x
    /// 1 MiB events. `tap_cache` memoises the result per
    /// `tap_generation`, so a frame with an unchanged ring is a
    /// cache hit, not a re-walk.
    pub fn current_hotspots(&self) -> Vec<crate::tap::Hotspot> {
        self.tap_cache
            .hotspots(&self.tap_events, self.tap_generation, self.tap_nav.sort)
    }

    /// Compute the current N+1 findings — called by the panel
    /// renderer on demand. Uses the defaults
    /// (`NPLUS1_WINDOW_MICROS`, `NPLUS1_MIN_REPEATS`) which
    /// match the offline classifier's operating point. Memoised
    /// the same way as `current_hotspots` — see
    /// `tap::TapInsightsCache`.
    pub fn current_nplus1(&self) -> Vec<crate::tap::NplusOneFinding> {
        self.tap_cache.nplus1(
            &self.tap_events,
            self.tap_generation,
            crate::tap::NPLUS1_WINDOW_MICROS,
            crate::tap::NPLUS1_MIN_REPEATS,
        )
    }

    /// Compute the current per-caller rollup per `tap_sort`.
    /// Same shape as `current_hotspots` but the grouping key
    /// is the innermost caller frame instead of the SQL
    /// fingerprint. Memoised — see `tap::TapInsightsCache`.
    pub fn current_callers(&self) -> Vec<crate::tap::CallerStats> {
        self.tap_cache
            .callers(&self.tap_events, self.tap_generation, self.tap_nav.sort)
    }
}

#[cfg(test)]
mod tests;

/// The status-bar line for a run mode Tab chose. Pure.
fn mode_hint(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Apply => "for real: each patch committed in turn · Tab dry run",
        RunMode::DryRun => {
            "dry run: one transaction, rolled back — same locks as the real run · Tab copy"
        }
        RunMode::Copy => "on a copy: the database copied, patched, dropped · Tab for real",
    }
}

/// The status-bar hint for a plan: what to press. Pure.
fn patch_hint(plan: &Result<crate::inventory::patching::PatchPlan, String>) -> String {
    match plan {
        Err(_) => "no plan · q back".into(),
        Ok(p) if p.patches.is_empty() && p.outside_would_help => {
            "nothing to apply in a transaction · o plan outside one · q back".into()
        }
        Ok(p) if p.patches.is_empty() => "nothing to apply · q back".into(),
        Ok(p) => {
            let first = if p.protected {
                format!(
                    "{} is protected: type its name, then Enter · Esc cancel",
                    p.environment
                )
            } else if p.needs_typed() {
                format!(
                    "outside a transaction: type {}, then Enter · Esc cancel",
                    p.environment
                )
            } else {
                "y apply · q cancel".into()
            };
            match (p.outside_would_help, p.needs_typed()) {
                (true, true) => format!("{first} · ctrl-o plan outside a transaction"),
                (true, false) => format!("{first} · o plan outside a transaction"),
                (false, _) => first,
            }
        }
    }
}
