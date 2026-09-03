//! Synthetic-data demo mode (`pgman --demo`).
//!
//! Builds a fully-populated [`App`] with no database, no network,
//! and no disk reads — a small fixture schema (`users` / `orders`
//! / `order_items`), a result grid, saved queries (including a
//! `:param` one), and a burst of JDBC-tap events that trips the
//! live N+1 detector. Used for screenshots, the README demo gif
//! (`vhs demo.tape`), and talks, so the frame is identical on every
//! launch and never touches a real server.
//!
//! `App::demo` is set so the run loop skips the connection and
//! skips persisting draft / history / saved-queries to disk — a
//! demo run can't clobber the operator's real session.

use std::collections::HashMap;

use crate::app::{App, ConnState, DatabaseInfo};
use crate::conn::Dsn;
use crate::grid::Grid;
use crate::query::schema::{ColumnMeta, ConstraintMeta, FkEdge, SchemaCache, TableMeta};
use crate::safety::SafetyConfig;
use crate::saved::SavedQuery;
use crate::tap::{TapEvent, TapKind};
use crate::theme::Theme;

/// Build the demo [`App`]. Pure (no I/O); everything is in memory.
pub fn app(theme: Theme) -> App {
    // A throwaway DSN so the chrome shows a sensible target. The
    // run loop never connects in demo mode.
    let dsn = Dsn::parse("postgres://demo@localhost:5432/shop").ok();
    let mut a = App::new(theme, dsn, Vec::new(), SafetyConfig::default());
    a.demo = true;
    // Demo runs are for screenshots / the README gif — no network,
    // no disk writes, and every frame identical. An update check
    // would violate all three.
    a.update_check_enabled = false;
    a.dsn_origin = Some("--demo (synthetic data)".into());
    a.conn_state = ConnState::Connected {
        server_version: "16.2 (demo)".into(),
    };
    a.schema_cache = schema_cache();
    let (cols, rows) = users_result();
    a.grid = Grid {
        columns: cols,
        rows,
        truncated: false,
    };
    // Populate the derived view state (visible rows + selection)
    // through the same path a real QueryOk uses, so the demo can't
    // drift from the live renderer.
    a.reset_grid_view();
    a.grid_view.source = Some(("public".into(), "users".into()));
    a.editor.buffer = "SELECT id, email, plan, created_at\n\
                       FROM users\n\
                       WHERE plan = 'pro'\n\
                       ORDER BY created_at DESC;"
        .into();
    a.editor.cursor = a.editor.buffer.len();
    for q in saved_queries() {
        a.saved_queries.upsert(q);
    }
    a.history = vec![
        "SELECT count(*) FROM orders WHERE status = 'shipped';".into(),
        "SELECT * FROM users WHERE id = 42;".into(),
    ];
    let events = tap_events();
    a.tap_health.query_count = events.len() as u64;
    if let Some(last) = events.last() {
        a.tap_health.last_event_at_unix_micros = last.received_at_unix_micros;
    }
    a.tap_events = events.into();
    a
}

/// Build the demo [`App`] open on the start card instead of a
/// pre-run result — the sixty-second story (`docs/logs-to-sql.md`)
/// starts with an empty grid so the F8 / F4 hint is the first thing
/// on screen, not a result the operator hasn't asked for yet.
///
/// Everything else — schema cache, saved queries, history, tap
/// events — is identical to [`app`]; only the grid (cleared back to
/// `Grid::default()`, same as a fresh, query-less connection) and
/// `databases` (populated, so the start card's databases line isn't
/// blank) differ.
pub fn launch_app(theme: Theme) -> App {
    let mut a = app(theme);
    a.grid = Grid::default();
    a.reset_grid_view();
    a.databases = vec![
        DatabaseInfo {
            name: "shop".into(),
            size: "812 MB".into(),
        },
        DatabaseInfo {
            name: "shop_test".into(),
            size: "94 MB".into(),
        },
        DatabaseInfo {
            name: "analytics".into(),
            size: "3.4 GB".into(),
        },
    ];
    a
}

/// Answer a query synthetically — no client, no network, called from
/// `App::spawn_run_demo` on `Guard::Allow` in `--demo` mode.
///
/// A `SELECT` whose single `FROM` table is in `cache` gets plausible
/// rows shaped by that table's real columns: the canned
/// [`users_result`] for `users`, deterministic generated rows for
/// anything else (`orders`, `order_items`). Everything else — writes
/// that reached `Allow` under a customised safety profile, joins,
/// unknown tables, `SELECT 1`-style queries with no table at all —
/// gets a one-row notice so the operator sees *something* rather than
/// a raw error.
pub fn answer(sql: &str, cache: &SchemaCache) -> Grid {
    let table = crate::app::infer_single_source_table(sql).filter(|(schema, name)| {
        cache
            .columns_meta_by_table
            .contains_key(&(schema.clone(), name.clone()))
    });
    let full = match table {
        Some((_, name)) if name == "users" => {
            let (columns, rows) = users_result();
            Grid {
                columns,
                rows,
                truncated: false,
            }
        }
        Some((schema, name)) => {
            let cols = cache
                .columns_meta_by_table
                .get(&(schema, name.clone()))
                .cloned()
                .unwrap_or_default();
            generated_rows(&name, &cols)
        }
        // The notice is a one-column answer about the demo itself, not
        // about the statement — projecting it would be nonsense.
        None => return notice_grid(),
    };
    match select_list_of(sql) {
        Some(list) => project_columns(list, full),
        None => full,
    }
}

/// The SELECT list of `sql` — the text between a leading `SELECT` and
/// the first top-level `FROM` — or `None` when `sql` isn't a plain
/// SELECT. A SELECT with no FROM at all (`SELECT 1`) yields the whole
/// remainder.
///
/// Only top-level `FROM` counts: parenthesised subqueries and string
/// literals are skipped, so `SELECT (SELECT 1 FROM t) AS x FROM u`
/// still cuts at the outer one.
fn select_list_of(sql: &str) -> Option<&str> {
    let after_select = {
        let s = sql.trim_start();
        let head = s.get(..6)?;
        if !head.eq_ignore_ascii_case("select") {
            return None;
        }
        let rest = &s[6..];
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        rest
    };
    let mut depth = 0usize;
    let mut in_string = false;
    for (i, c) in after_select.char_indices() {
        match c {
            '\'' => in_string = !in_string,
            '(' if !in_string => depth += 1,
            ')' if !in_string => depth = depth.saturating_sub(1),
            'f' | 'F' if !in_string && depth == 0 && word_at(after_select, i, "from") => {
                return Some(after_select[..i].trim());
            }
            _ => {}
        }
    }
    Some(after_select.trim())
}

/// True when `word` sits at byte offset `at` in `text` as a whole
/// word — i.e. neither neighbour is an identifier character.
fn word_at(text: &str, at: usize, word: &str) -> bool {
    let before_ok = text[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !is_ident_char(c));
    let Some(rest) = text.get(at..) else {
        return false;
    };
    // `get`, not a byte slice: `word.len()` need not be a char boundary
    // in `rest` (`fé€` straddles byte 4). Once the head is ASCII-equal
    // to `word`, `word.len()` IS a boundary, so the slice below is safe.
    let Some(head) = rest.get(..word.len()) else {
        return false;
    };
    if !head.eq_ignore_ascii_case(word) {
        return false;
    }
    let after_ok = rest[word.len()..]
        .chars()
        .next()
        .is_none_or(|c| !is_ident_char(c));
    before_ok && after_ok
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Narrow `grid` to the columns a simple SELECT list names, so
/// `SELECT id, status FROM orders` doesn't answer with every column
/// the table has.
///
/// Deliberately conservative — the whole grid comes back unchanged
/// unless every item is a bare (optionally alias-qualified) column
/// name that the grid actually carries. `*`, an expression, a
/// function call, a `DISTINCT`, a name the grid doesn't have: all
/// keep every column, because a demo that quietly drops a column the
/// operator asked for teaches the wrong thing about the real one.
pub fn project_columns(select_list: &str, grid: Grid) -> Grid {
    let items = split_top_level_commas(select_list);
    if items.is_empty() {
        return grid;
    }
    let mut wanted = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = column_name_of(item) else {
            return grid;
        };
        let Some(idx) = grid
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
        else {
            return grid;
        };
        wanted.push(idx);
    }
    Grid {
        columns: wanted.iter().map(|&i| grid.columns[i].clone()).collect(),
        rows: grid
            .rows
            .iter()
            .map(|row| {
                wanted
                    .iter()
                    .map(|&i| row.get(i).cloned().unwrap_or_default())
                    .collect()
            })
            .collect(),
        truncated: grid.truncated,
    }
}

/// `item` as a plain column name: `id`, `o.id`, `public.o.id` — the
/// last dot-separated segment. `None` for anything else (`*`, `o.*`,
/// `count(*)`, `a + b`, `id AS x`), which is the caller's signal to
/// keep every column.
fn column_name_of(item: &str) -> Option<&str> {
    let item = item.trim();
    if item.is_empty() || !item.chars().all(|c| is_ident_char(c) || c == '.') {
        return None;
    }
    let last = item.rsplit('.').next()?;
    (!last.is_empty() && !last.chars().next()?.is_ascii_digit()).then_some(last)
}

/// Split a SELECT list on commas that aren't inside parentheses or a
/// string literal.
fn split_top_level_commas(list: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut start = 0usize;
    for (i, c) in list.char_indices() {
        match c {
            '\'' => in_string = !in_string,
            '(' if !in_string => depth += 1,
            ')' if !in_string => depth = depth.saturating_sub(1),
            ',' if !in_string && depth == 0 => {
                out.push(&list[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = &list[start..];
    if !tail.trim().is_empty() || !out.is_empty() {
        out.push(tail);
    }
    out
}

/// A one-row notice: what `answer` falls back to for anything it
/// can't shape a plausible result for.
fn notice_grid() -> Grid {
    Grid {
        columns: vec!["demo".to_string()],
        rows: vec![vec!["this is --demo mode, no database".to_string()]],
        truncated: false,
    }
}

/// Number of synthetic rows `generated_rows` fabricates per table —
/// within the 3–5 range asked for, deterministic across runs.
const GENERATED_ROW_COUNT: usize = 4;

/// Fabricate `GENERATED_ROW_COUNT` deterministic rows shaped by
/// `cols` — every value is a pure function of `(table, column name /
/// type, row index)`, so the same query always answers the same way.
fn generated_rows(table: &str, cols: &[ColumnMeta]) -> Grid {
    let columns = cols.iter().map(|c| c.name.clone()).collect();
    let rows = (0..GENERATED_ROW_COUNT)
        .map(|i| cols.iter().map(|c| synth_cell(table, c, i)).collect())
        .collect();
    Grid {
        columns,
        rows,
        truncated: false,
    }
}

/// One synthesized cell for `col` at 0-indexed `row_idx`, biased by
/// the demo schema's own naming (`_id` foreign keys, `status`,
/// `*_cents`, `sku`, `qty`) and falling back to the column's Postgres
/// type for anything it doesn't recognise by name.
fn synth_cell(table: &str, col: &ColumnMeta, row_idx: usize) -> String {
    let n = row_idx + 1;
    let name = col.name.as_str();
    let ty = col.type_name.as_str();
    if name == "id" {
        return n.to_string();
    }
    if name.ends_with("_id") {
        // Foreign key: cycle through a small parent-id range so it
        // reads as plausible rather than monotonically increasing
        // alongside the row's own id.
        return ((row_idx % 3) + 1).to_string();
    }
    match (table, name) {
        ("orders", "status") => {
            ["pending", "shipped", "delivered", "cancelled"][row_idx % 4].to_string()
        }
        ("orders", "total_cents") => (1_999 + row_idx * 550).to_string(),
        ("order_items", "sku") => format!("SKU-{:04}", 1000 + row_idx * 7),
        ("order_items", "qty") => ((row_idx % 4) + 1).to_string(),
        ("order_items", "price_cents") => (499 + row_idx * 250).to_string(),
        _ if ty.starts_with("timestamp") => {
            format!(
                "2026-0{}-{:02} {:02}:00:00+00",
                (row_idx % 6) + 1,
                4 + row_idx,
                8 + row_idx
            )
        }
        _ if ty.starts_with("bigint") || ty.starts_with("integer") => n.to_string(),
        _ => format!("{name}-{n}"),
    }
}

/// `(schema, table)` keys live a lot here.
fn key(table: &str) -> (String, String) {
    ("public".to_string(), table.to_string())
}

fn col(name: &str, ty: &str, not_null: bool) -> ColumnMeta {
    ColumnMeta {
        name: name.into(),
        type_name: ty.into(),
        not_null,
    }
}

/// A small but realistic e-commerce schema: users 1—* orders 1—*
/// order_items, with primary keys and FK edges so the schema
/// browser and FK-navigation light up.
fn schema_cache() -> SchemaCache {
    let mut columns_meta_by_table: HashMap<(String, String), Vec<ColumnMeta>> = HashMap::new();
    columns_meta_by_table.insert(
        key("users"),
        vec![
            col("id", "bigint", true),
            col("email", "varchar(255)", true),
            col("plan", "varchar(20)", true),
            col("created_at", "timestamptz", true),
        ],
    );
    columns_meta_by_table.insert(
        key("orders"),
        vec![
            col("id", "bigint", true),
            col("user_id", "bigint", true),
            col("status", "varchar(20)", true),
            col("total_cents", "integer", true),
            col("created_at", "timestamptz", true),
        ],
    );
    columns_meta_by_table.insert(
        key("order_items"),
        vec![
            col("id", "bigint", true),
            col("order_id", "bigint", true),
            col("sku", "varchar(40)", true),
            col("qty", "integer", true),
            col("price_cents", "integer", true),
        ],
    );

    let columns_by_table: HashMap<(String, String), Vec<String>> = columns_meta_by_table
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().map(|c| c.name.clone()).collect()))
        .collect();

    let tables = vec![
        TableMeta {
            schema: "public".into(),
            name: "order_items".into(),
        },
        TableMeta {
            schema: "public".into(),
            name: "orders".into(),
        },
        TableMeta {
            schema: "public".into(),
            name: "users".into(),
        },
    ];

    let constraints = vec![
        ConstraintMeta {
            schema: "public".into(),
            table: "users".into(),
            name: "users_pkey".into(),
        },
        ConstraintMeta {
            schema: "public".into(),
            table: "orders".into(),
            name: "orders_pkey".into(),
        },
        ConstraintMeta {
            schema: "public".into(),
            table: "order_items".into(),
            name: "order_items_pkey".into(),
        },
    ];

    let fk_edges = vec![
        FkEdge {
            child_schema: "public".into(),
            child_table: "orders".into(),
            child_column: "user_id".into(),
            parent_schema: "public".into(),
            parent_table: "users".into(),
            parent_column: "id".into(),
        },
        FkEdge {
            child_schema: "public".into(),
            child_table: "order_items".into(),
            child_column: "order_id".into(),
            parent_schema: "public".into(),
            parent_table: "orders".into(),
            parent_column: "id".into(),
        },
    ];

    SchemaCache {
        schemas: vec!["public".into()],
        tables,
        columns_by_table,
        columns_meta_by_table,
        constraints,
        fk_edges,
        ..SchemaCache::default()
    }
}

/// The result grid: a slice of `users`.
fn users_result() -> (Vec<String>, Vec<Vec<String>>) {
    let cols = vec![
        "id".to_string(),
        "email".to_string(),
        "plan".to_string(),
        "created_at".to_string(),
    ];
    let rows = vec![
        row(&["1", "ada@example.com", "pro", "2026-01-04 09:12:00+00"]),
        row(&["2", "linus@example.com", "free", "2026-01-09 14:48:00+00"]),
        row(&["3", "grace@example.com", "pro", "2026-02-01 08:30:00+00"]),
        row(&["4", "alan@example.com", "team", "2026-02-17 19:05:00+00"]),
        row(&["5", "edsger@example.com", "free", "2026-03-03 11:22:00+00"]),
        row(&[
            "6",
            "katherine@example.com",
            "pro",
            "2026-03-21 16:40:00+00",
        ]),
    ];
    (cols, rows)
}

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|s| s.to_string()).collect()
}

fn saved_queries() -> Vec<SavedQuery> {
    vec![
        SavedQuery {
            name: "pro-users".into(),
            body: "SELECT id, email, created_at\nFROM users\nWHERE plan = 'pro'\nORDER BY created_at DESC;".into(),
        },
        SavedQuery {
            name: "order-by-id".into(),
            body: "SELECT * FROM orders WHERE id = :order_id;".into(),
        },
        SavedQuery {
            name: "daily-revenue".into(),
            body: "SELECT date_trunc('day', created_at) AS day,\n       sum(total_cents) / 100.0 AS revenue\nFROM orders\nGROUP BY 1 ORDER BY 1 DESC;".into(),
        },
    ]
}

/// A JDBC-tap event burst: one `OrderService.loadItems` N+1 (six
/// `SELECT … order_items WHERE order_id = ?` in one transaction
/// within ~150ms) plus a couple of one-off statements, so the
/// TapMonitor's hotspots / callers / N+1 views all render with
/// real-looking data.
fn tap_events() -> Vec<TapEvent> {
    // Fixed base time so the demo is deterministic (well before
    // "now"; the views key off relative ordering, not wall clock).
    const BASE: u64 = 1_730_000_000_000_000;
    let mut events = Vec::new();

    events.push(query(
        BASE,
        "select-orders",
        None,
        "SELECT id, user_id, status FROM orders WHERE status = ?",
        1840,
        Some("OrderService.listOpen:54"),
    ));

    // The N+1: six near-identical selects in one txn, ~25ms apart.
    for i in 0..6u64 {
        events.push(query(
            BASE + 60_000 + i * 25_000,
            "conn-7",
            Some("conn-7#42"),
            "SELECT * FROM order_items WHERE order_id = ?",
            420 + i * 15,
            Some("OrderService.loadItems:88"),
        ));
    }

    events.push(query(
        BASE + 400_000,
        "select-user",
        None,
        "SELECT id, email, plan FROM users WHERE id = ?",
        310,
        Some("UserController.show:31"),
    ));

    events
}

#[allow(clippy::too_many_arguments)]
fn query(
    ts: u64,
    conn: &str,
    txn: Option<&str>,
    sql: &str,
    dur: u64,
    caller: Option<&str>,
) -> TapEvent {
    TapEvent {
        v: 1,
        kind: TapKind::Query,
        ts_unix_micros: ts,
        received_at_unix_micros: ts,
        app: Some("shop-api".into()),
        pool: Some("primary".into()),
        conn: Some(conn.into()),
        txn: txn.map(str::to_string),
        sql: Some(sql.into()),
        params: None,
        params_redacted: false,
        duration_micros: Some(dur),
        rows: Some(1),
        error: None,
        caller: caller.map(|c| vec![c.into()]),
        dropped_events_total: None,
        txn_outcome: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_app_is_populated_and_marked_demo() {
        let a = app(Theme::default());
        assert!(a.demo, "demo flag must be set so persistence is skipped");
        assert!(matches!(a.conn_state, ConnState::Connected { .. }));
        assert!(!a.grid.rows.is_empty(), "grid has data");
        assert_eq!(a.grid_view.source.as_ref().unwrap().1, "users");
        assert_eq!(a.schema_cache.tables.len(), 3);
        assert!(a
            .saved_queries
            .entries
            .iter()
            .any(|q| q.body.contains(":order_id")));
        assert!(!a.tap_events.is_empty());
    }

    #[test]
    fn demo_tap_events_trip_the_nplus1_detector() {
        let a = app(Theme::default());
        // The loadItems burst should surface as at least one N+1
        // finding via the same detector the live panel uses.
        let findings = a.current_nplus1();
        assert!(
            findings
                .iter()
                .any(|f| f.fingerprint.contains("order_items")),
            "expected an order_items N+1 finding; got {findings:?}"
        );
    }

    #[test]
    fn demo_schema_cache_has_pk_constraints_and_fks() {
        let c = schema_cache();
        assert_eq!(c.constraints.len(), 3);
        assert_eq!(c.fk_edges.len(), 2);
        assert!(c.columns_meta_by_table.contains_key(&key("users")));
    }

    #[test]
    fn launch_app_opens_empty_with_databases_populated() {
        let a = launch_app(Theme::default());
        assert!(a.demo);
        assert!(
            a.grid.columns.is_empty() && a.grid.rows.is_empty(),
            "launch_app must start on an EMPTY grid so draw_landing (the \
             start card) renders instead of a pre-run result: {:?}",
            a.grid
        );
        assert!(
            !a.databases.is_empty(),
            "start card's databases line needs data to show"
        );
        // Everything else app() sets up is untouched.
        assert_eq!(a.schema_cache.tables.len(), 3);
        assert!(!a.tap_events.is_empty());
    }

    #[test]
    fn app_is_unchanged_by_launch_app_existing() {
        // app() itself still opens with its pre-populated users grid —
        // tests/sizes.rs and tests/render.rs depend on that shape.
        let a = app(Theme::default());
        assert!(!a.grid.rows.is_empty());
        assert_eq!(a.grid_view.source.as_ref().unwrap().1, "users");
    }

    #[test]
    fn answer_select_on_users_yields_the_users_grid() {
        let cache = schema_cache();
        let grid = answer("SELECT id, email, plan, created_at FROM users", &cache);
        let (expected_cols, expected_rows) = users_result();
        assert_eq!(grid.columns, expected_cols);
        assert_eq!(grid.rows, expected_rows);
    }

    #[test]
    fn answer_select_on_orders_yields_rows_with_orders_columns() {
        let cache = schema_cache();
        let grid = answer("SELECT * FROM orders WHERE status = 'shipped'", &cache);
        assert_eq!(
            grid.columns,
            vec!["id", "user_id", "status", "total_cents", "created_at"]
        );
        assert!(
            grid.rows.len() >= 3 && grid.rows.len() <= 5,
            "expected 3-5 generated rows, got {}",
            grid.rows.len()
        );
        // Every row is fully shaped (one cell per column) with
        // non-empty, deterministic values.
        for row in &grid.rows {
            assert_eq!(row.len(), grid.columns.len());
            assert!(row.iter().all(|c| !c.is_empty()));
        }
        // Deterministic: same SQL answers the same way every time.
        let again = answer("SELECT * FROM orders WHERE status = 'shipped'", &cache);
        assert_eq!(grid.rows, again.rows);
    }

    #[test]
    fn answer_unknown_statement_yields_one_row_notice() {
        let cache = schema_cache();
        let grid = answer("SELECT 1", &cache);
        assert_eq!(grid.columns, vec!["demo".to_string()]);
        assert_eq!(grid.rows.len(), 1);
        assert_eq!(grid.rows[0][0], "this is --demo mode, no database");

        // A join (not a single-table FROM) also falls back to the notice.
        let grid = answer(
            "SELECT * FROM orders o JOIN users u ON u.id = o.user_id",
            &cache,
        );
        assert_eq!(grid.rows[0][0], "this is --demo mode, no database");
    }

    #[test]
    fn select_list_of_cuts_at_the_top_level_from() {
        assert_eq!(select_list_of("select a, b from t"), Some("a, b"));
        assert_eq!(
            select_list_of("  SELECT  o.id  FROM orders o"),
            Some("o.id")
        );
        // No FROM at all — the whole remainder is the list.
        assert_eq!(select_list_of("select 1"), Some("1"));
        // A FROM inside a subquery or a string literal must not cut.
        assert_eq!(
            select_list_of("select (select 1 from t) as x from u"),
            Some("(select 1 from t) as x")
        );
        assert_eq!(
            select_list_of("select 'from me' as note from t"),
            Some("'from me' as note")
        );
        // `fromage` is not the keyword.
        assert_eq!(select_list_of("select fromage from t"), Some("fromage"));
        // Not a SELECT.
        assert_eq!(select_list_of("update users set a = 1"), None);
        assert_eq!(select_list_of("selected"), None);
        assert_eq!(select_list_of(""), None);
    }

    #[test]
    fn answer_does_not_panic_on_multibyte_sql() {
        // `word_at` sliced `rest[..word.len()]` by byte; `fé€` puts a
        // multi-byte char across byte 4 and the TUI died with
        // "end byte index 4 is not a char boundary".
        let cache = schema_cache();
        for sql in [
            "SELECT fé€ FROM users",
            "select f€ from users",
            "SELECT 中文 FROM orders o",
        ] {
            let _ = answer(sql, &cache);
        }
    }

    #[test]
    fn project_columns_narrows_to_the_named_columns() {
        let grid = Grid {
            columns: vec!["id".into(), "user_id".into(), "status".into()],
            rows: vec![
                vec!["1".into(), "7".into(), "shipped".into()],
                vec!["2".into(), "8".into(), "pending".into()],
            ],
            truncated: false,
        };
        let out = project_columns("id, status", grid);
        assert_eq!(out.columns, vec!["id".to_string(), "status".to_string()]);
        assert_eq!(out.rows[0], vec!["1".to_string(), "shipped".to_string()]);
        assert_eq!(out.rows[1], vec!["2".to_string(), "pending".to_string()]);
    }

    #[test]
    fn project_columns_keeps_every_column_for_anything_it_cannot_read() {
        let grid = || Grid {
            columns: vec!["id".into(), "user_id".into(), "status".into()],
            rows: vec![vec!["1".into(), "7".into(), "shipped".into()]],
            truncated: false,
        };
        let all = vec![
            "id".to_string(),
            "user_id".to_string(),
            "status".to_string(),
        ];
        // `*` and `t.*`
        assert_eq!(project_columns("*", grid()).columns, all);
        assert_eq!(project_columns("o.*", grid()).columns, all);
        // An expression, a function call, an alias.
        assert_eq!(project_columns("id + 1", grid()).columns, all);
        assert_eq!(project_columns("count(*)", grid()).columns, all);
        assert_eq!(project_columns("id AS pk", grid()).columns, all);
        assert_eq!(project_columns("distinct id", grid()).columns, all);
        // A column the grid doesn't have: dropping the ones it DOES
        // have would answer a different question than the one asked.
        assert_eq!(project_columns("id, total", grid()).columns, all);
        assert_eq!(project_columns("", grid()).columns, all);
    }

    #[test]
    fn project_columns_reads_alias_qualified_names_case_insensitively() {
        let grid = Grid {
            columns: vec!["id".into(), "Status".into()],
            rows: vec![vec!["1".into(), "shipped".into()]],
            truncated: true,
        };
        let out = project_columns("o.ID , o.status", grid);
        assert_eq!(out.columns, vec!["id".to_string(), "Status".to_string()]);
        assert!(out.truncated, "the cap flag travels with the projection");
    }

    #[test]
    fn answer_projects_the_select_list_of_a_generated_table() {
        let cache = schema_cache();
        let full = answer("select * from orders", &cache);
        assert_eq!(full.columns.len(), 5, "orders has five columns");
        let narrowed = answer("select o.id, o.total_cents from orders o", &cache);
        assert_eq!(
            narrowed.columns,
            vec!["id".to_string(), "total_cents".to_string()],
            "the demo must answer the columns that were asked for"
        );
        assert_eq!(narrowed.rows.len(), full.rows.len());
        assert!(narrowed.rows.iter().all(|r| r.len() == 2));
        // The canned users result projects the same way.
        let users = answer("select email from users", &cache);
        assert_eq!(users.columns, vec!["email".to_string()]);
        // And the "no table I know" notice is never projected.
        let notice = answer("select nothing from mystery", &cache);
        assert_eq!(notice.columns, vec!["demo".to_string()]);
    }
}
