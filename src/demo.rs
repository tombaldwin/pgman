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

use crate::app::{App, ConnState};
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
}
