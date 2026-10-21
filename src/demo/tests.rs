use super::*;

fn cache() -> SchemaCache {
    schema::schema_cache()
}

#[test]
fn demo_app_is_populated_and_marked_demo() {
    let a = app(Theme::default());
    assert!(a.demo, "demo flag must be set so persistence is skipped");
    assert!(matches!(a.conn_state, ConnState::Connected { .. }));
    assert!(!a.grid.rows.is_empty(), "grid has data");
    assert_eq!(a.grid_view.source.as_ref().unwrap().1, "invoices");
    assert!(a.schema_cache.tables.len() >= 20, "a realistic schema");
    assert!(a
        .saved_queries
        .entries
        .iter()
        .any(|q| q.body.contains(":customer_id")));
    assert!(!a.tap_events.is_empty());
    assert!(!a.notifications.items.is_empty());
}

/// The opening grid is what running the opening query answers: the
/// open invoices, in due-date order, in the columns it names.
#[test]
fn the_opening_grid_is_the_opening_querys_answer() {
    let a = app(Theme::default());
    assert_eq!(a.grid, answer(&a.editor.buffer, &a.schema_cache));
    assert_eq!(
        a.grid.columns,
        [
            "number",
            "customer_id",
            "status",
            "currency",
            "total",
            "due_at"
        ]
    );
    assert_eq!(a.grid.rows.len(), 5);
    assert!(
        a.grid.rows.iter().all(|r| r[2] == "open"),
        "{:?}",
        a.grid.rows
    );
    let due: Vec<&str> = a.grid.rows.iter().map(|r| r[5].as_str()).collect();
    let mut sorted = due.clone();
    sorted.sort();
    assert_eq!(due, sorted, "ORDER BY due_at holds without sorting");
}

#[test]
fn demo_tap_events_trip_the_nplus1_detector() {
    let a = app(Theme::default());
    // The loadLines burst should surface as at least one N+1 finding
    // via the same detector the live panel uses.
    let findings = a.current_nplus1();
    assert!(
        findings
            .iter()
            .any(|f| f.fingerprint.contains("invoice_lines")),
        "expected an invoice_lines N+1 finding; got {findings:?}"
    );
}

#[test]
fn demo_schema_cache_has_pk_constraints_and_fks() {
    let c = cache();
    assert!(c.constraints.len() > 20);
    assert_eq!(c.fk_edges.len(), schema::FOREIGN_KEYS.len());
    assert!(c
        .columns_meta_by_table
        .contains_key(&("public".into(), "invoice_lines".into())));
    assert_eq!(c.tables.len(), c.columns_meta_by_table.len());
}

/// The docs' N+1 walkthrough queries `invoice_lines`; the demo answers
/// it with rows shaped by the table's columns, for the invoice asked.
#[test]
fn answer_select_on_invoice_lines_yields_that_invoices_lines() {
    let g = answer(
        "SELECT * FROM invoice_lines WHERE invoice_id = 48207",
        &cache(),
    );
    assert_eq!(
        g.columns,
        [
            "id",
            "invoice_id",
            "description",
            "quantity",
            "unit_amount",
            "amount",
            "tax_rate_id"
        ]
    );
    assert!(!g.rows.is_empty());
    assert!(g.rows.iter().all(|r| r[1] == "48207"), "{:?}", g.rows);
    assert!(g.rows[0][3].parse::<u32>().is_ok(), "{:?}", g.rows[0]);
    assert!(g.rows[0][5].contains('.'), "money: {:?}", g.rows[0]);
}

#[test]
fn a_term_on_id_answers_one_row_with_that_id() {
    let g = answer("SELECT * FROM payments WHERE id = 39900", &cache());
    assert_eq!(g.rows.len(), 1);
    assert_eq!(g.rows[0][0], "39900");
    // The canned tables filter instead.
    let g = answer("SELECT name FROM customers WHERE id = 2107", &cache());
    assert_eq!(g.rows, [["Meridian Legal"]]);
    let g = answer("SELECT * FROM customers WHERE id = 1", &cache());
    assert!(g.rows.is_empty(), "no such customer");
}

#[test]
fn where_equalities_reads_and_joined_equalities_only() {
    let eq = |s: &str| where_equalities(s);
    assert_eq!(
        eq("status = 'open' AND i.customer_id = 2107"),
        [
            ("status".to_string(), "open".to_string()),
            ("customer_id".to_string(), "2107".to_string())
        ]
    );
    // Other shapes are skipped, not misread.
    assert_eq!(
        eq("status = 'open' and due_at < now()"),
        [("status".to_string(), "open".to_string())]
    );
    assert!(eq("total >= 100").is_empty());
    assert!(eq("total <= 100").is_empty());
    assert!(eq("status != 'void'").is_empty());
    assert!(eq("status = lower('OPEN')").is_empty());
    assert!(eq("name = 'O''Brien'").is_empty());
    assert!(eq("lower(status) = 'open'").is_empty());
    // An OR anywhere at the top drops every term, the ones before it
    // too: a row the other branch keeps would be dropped.
    assert!(eq("status = 'open' OR status = 'draft'").is_empty());
    assert!(eq("status = 'open' AND kind = 'x' OR id = 5").is_empty());
    assert!(eq("a == 1").is_empty());
    // An AND inside a string or parentheses is not a separator.
    assert_eq!(
        eq("note = 'cats and dogs' and (a = 1 or b = 2)"),
        [("note".to_string(), "cats and dogs".to_string())]
    );
    // `brand` holds `and` but is one word.
    assert_eq!(eq("brand = 'x'"), [("brand".to_string(), "x".to_string())]);
}

#[test]
fn filter_rows_keeps_matching_rows_and_ignores_unknown_columns() {
    let grid = Grid {
        columns: vec!["id".into(), "status".into()],
        rows: vec![
            vec!["1".into(), "open".into()],
            vec!["2".into(), "paid".into()],
        ],
        truncated: false,
    };
    let only = filter_rows(grid.clone(), &[("STATUS".into(), "paid".into())]);
    assert_eq!(only.rows, [["2", "paid"]]);
    let all = filter_rows(grid.clone(), &[("nope".into(), "x".into())]);
    assert_eq!(all.rows, grid.rows);
}

/// EXPLAIN plans the table it was asked about: an index scan when the
/// WHERE pins an indexed column, a filtered seq scan otherwise, an
/// Append over a partitioned table's partitions, a Hash Join on the
/// foreign key for two related tables.
#[test]
fn explain_plans_the_table_asked_about() {
    let cache = cache();
    let plan = |sql: &str, analyze: bool| {
        let g = explain_plan_grid(analyze, sql, &cache);
        crate::query::explain::parse(&g.rows[0][0]).expect("valid plan JSON")
    };
    let extra = |p: &crate::query::explain::PlanNode, key: &str| {
        p.extras
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let p = plan(
        "EXPLAIN SELECT * FROM invoices WHERE status = 'open' LIMIT 5;",
        false,
    );
    assert_eq!(p.node_type, "Seq Scan");
    assert_eq!(p.relation_name.as_deref(), Some("invoices"));
    assert_eq!(extra(&p, "Filter").as_deref(), Some("(status = 'open')"));
    let p = plan(
        "select *\nfrom invoice_lines\nwhere invoice_id = 48207",
        true,
    );
    assert_eq!(p.node_type, "Index Scan");
    assert_eq!(
        extra(&p, "Index Name").as_deref(),
        Some("invoice_lines_invoice_id_idx")
    );
    // Rows per invoice: the lines' estimate over the invoices'.
    assert_eq!(p.actual_rows, Some(3.0));
    let p = plan("SELECT * FROM payments WHERE id = 1", false);
    assert_eq!(extra(&p, "Index Name").as_deref(), Some("payments_pkey"));
    // A partial index isn't taken for a WHERE that may not imply its
    // predicate.
    let p = plan(
        "SELECT * FROM webhook_deliveries WHERE created_at = '2026-10-01'",
        false,
    );
    assert_eq!(p.node_type, "Seq Scan");
    let p = plan("SELECT * FROM refunds", false);
    assert_eq!(p.node_type, "Seq Scan");
    assert_eq!(extra(&p, "Filter"), None);
    let p = plan("SELECT * FROM usage_events WHERE metric = 'seats'", true);
    assert_eq!(p.node_type, "Append");
    assert_eq!(p.children.len(), 3, "one scan per partition");
    let p = plan(
        "SELECT * FROM invoice_lines l JOIN invoices i ON i.id = l.invoice_id",
        false,
    );
    assert_eq!(p.node_type, "Hash Join");
    assert_eq!(
        extra(&p, "Hash Cond").as_deref(),
        Some("(l.invoice_id = i.id)")
    );
    assert_eq!(p.children[1].node_type, "Hash");
    assert_eq!(extra(&p.children[0], "Filter"), None);
    // A WHERE filters the child side, and the join returns what it kept.
    let p = plan(
        "SELECT * FROM invoices i JOIN customers c ON c.id = i.customer_id WHERE i.status = 'open'",
        false,
    );
    assert_eq!(
        extra(&p.children[0], "Filter").as_deref(),
        Some("(i.status = 'open')")
    );
    assert_eq!(p.plan_rows, p.children[0].plan_rows);
    // Anything else is the invoices ⋈ customers join.
    let p = plan("SELECT * FROM a JOIN b ON true", false);
    assert_eq!(
        extra(&p, "Hash Cond").as_deref(),
        Some("(invoices.customer_id = customers.id)")
    );
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
    assert!(
        a.editor.buffer.is_empty() && a.editor.cursor == 0,
        "launch_app must start with an EMPTY editor — docs/logs-to-sql.md says \
         `e` then paste, and a paste appended to the pre-filled SELECT: {:?}",
        a.editor.buffer
    );
    // Everything else app() sets up is untouched, bar the tap.
    assert_eq!(a.schema_cache, app(Theme::default()).schema_cache);
}

/// `--demo` opens with the tap quiet — no events, no TAP / N+1
/// badges — and the burst arrives after `DEMO_TAP_DELAY`, one event
/// a frame, until the N+1 finding is there and the feed is gone.
#[test]
fn launch_app_tap_is_quiet_until_the_burst_arrives() {
    let mut a = launch_app(Theme::default());
    assert!(a.tap_events.is_empty() && a.tap_health.query_count == 0);
    assert!(a.current_nplus1().is_empty());
    let feed = a.demo_tap_feed.as_ref().expect("a pending burst");
    let (due, total) = (feed.due, feed.pending.len());
    assert_eq!(total, tap_events().len());

    // Before it's due: nothing arrives.
    a.tick_demo_tap(due - std::time::Duration::from_millis(1));
    assert!(a.tap_events.is_empty());

    // From then on, one event per frame.
    a.tick_demo_tap(due);
    assert_eq!(a.tap_events.len(), 1);
    for _ in 1..total {
        a.tick_demo_tap(due);
    }
    assert_eq!(a.tap_events.len(), total);
    assert!(a.tap_health.query_count > 0);
    assert!(
        a.demo_tap_feed.is_none(),
        "the feed must go with its last event, or it holds the frame clock awake"
    );
    assert!(
        a.current_nplus1()
            .iter()
            .any(|f| f.fingerprint.contains("invoice_lines")),
        "the delivered burst trips the detector, as app()'s pre-filled ring does"
    );
}

#[test]
fn answer_select_on_invoices_yields_the_written_out_rows() {
    let grid = answer("SELECT * FROM invoices", &cache());
    let (expected_cols, expected_rows) = schema::invoices();
    assert_eq!(grid.columns, expected_cols);
    assert_eq!(grid.rows, expected_rows);
}

#[test]
fn answer_select_on_a_generated_table_yields_shaped_deterministic_rows() {
    let sql = "SELECT * FROM payments WHERE status = 'failed'";
    let grid = answer(sql, &cache());
    assert_eq!(
        grid.columns,
        [
            "id",
            "invoice_id",
            "amount",
            "currency",
            "method",
            "status",
            "provider_ref",
            "created_at"
        ]
    );
    assert_eq!(grid.rows.len(), GENERATED_ROW_COUNT);
    for row in &grid.rows {
        assert_eq!(row.len(), grid.columns.len());
        // Every column here is NOT NULL.
        assert!(row.iter().all(|c| !c.is_empty()), "{row:?}");
        assert_eq!(row[5], "failed", "the WHERE holds in every row");
    }
    assert_eq!(grid.rows, answer(sql, &cache()).rows);
}

/// Every relation answers, every NOT NULL column has a value in every
/// row, and every foreign key points inside its parent's id range —
/// screenshots are read closely.
#[test]
fn every_relation_answers_with_its_not_null_columns_filled() {
    let cache = cache();
    for t in &cache.tables {
        let g = answer(&format!("SELECT * FROM {}.{}", t.schema, t.name), &cache);
        let meta = &cache.columns_meta_by_table[&(t.schema.clone(), t.name.clone())];
        assert_eq!(g.columns.len(), meta.len(), "{}", t.name);
        assert!(!g.rows.is_empty(), "{}", t.name);
        for row in &g.rows {
            for (cell, col) in row.iter().zip(meta) {
                if col.not_null {
                    assert!(!cell.is_empty(), "{}.{} is NOT NULL", t.name, col.name);
                }
            }
        }
    }
}

#[test]
fn answer_unknown_statement_yields_one_row_notice() {
    let cache = cache();
    let grid = answer("SELECT 1", &cache);
    assert_eq!(grid.columns, vec!["demo".to_string()]);
    assert_eq!(grid.rows.len(), 1);
    assert_eq!(grid.rows[0][0], "this is --demo mode, no database");

    // A join (not a single-table FROM) also falls back to the notice.
    let grid = answer(
        "SELECT * FROM invoices i JOIN customers c ON c.id = i.customer_id",
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
    let cache = cache();
    for sql in [
        "SELECT fé€ FROM invoices",
        "select f€ from invoices",
        "SELECT 中文 FROM payments p",
        "SELECT * FROM payments WHERE método = 'é' AND año = 1",
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
    let cache = cache();
    let full = answer("select * from payments", &cache);
    assert_eq!(full.columns.len(), 8, "payments has eight columns");
    let narrowed = answer("select p.id, p.amount from payments p", &cache);
    assert_eq!(
        narrowed.columns,
        vec!["id".to_string(), "amount".to_string()],
        "the demo must answer the columns that were asked for"
    );
    assert_eq!(narrowed.rows.len(), full.rows.len());
    assert!(narrowed.rows.iter().all(|r| r.len() == 2));
    // The canned invoices project the same way.
    let invoices = answer("select number from invoices", &cache);
    assert_eq!(invoices.columns, vec!["number".to_string()]);
    // And the "no table I know" notice is never projected.
    let notice = answer("select nothing from mystery", &cache);
    assert_eq!(notice.columns, vec!["demo".to_string()]);
}

#[test]
fn answer_honours_a_trailing_limit() {
    let cache = cache();
    assert_eq!(answer("select * from customers", &cache).rows.len(), 12);
    assert_eq!(
        answer("SELECT * FROM customers LIMIT 2;", &cache)
            .rows
            .len(),
        2
    );
    assert_eq!(
        answer("select * from payments limit 1", &cache).rows.len(),
        1
    );
    assert_eq!(
        answer("select * from customers limit 50", &cache)
            .rows
            .len(),
        12
    );
    assert_eq!(trailing_limit("select 1"), None);
    assert_eq!(trailing_limit("select * from t limit x"), None);
    assert_eq!(trailing_limit("select * from t where id = 3"), None);
}

#[test]
fn functions_grid_has_the_live_columns() {
    let g = functions_grid(None);
    assert_eq!(g.columns, ["schema", "name", "result", "arguments", "type"]);
    assert!(g.rows.iter().all(|r| r.len() == g.columns.len()));
    assert!(g.rows.iter().all(|r| r[0] == "public"));
    assert_eq!(functions_grid(Some("public.invoice_*")).rows.len(), 1);
    assert_eq!(functions_grid(Some("reporting.invoice_*")).rows.len(), 0);
    assert_eq!(functions_grid(Some("reporting.*")).rows.len(), 2);
}

#[test]
fn the_view_ddl_is_a_create_view() {
    let ddl = crate::query::ddl::create_statement(
        &table_ddl("reporting", "revenue_daily").expect("a demo view"),
    );
    assert!(
        ddl.starts_with("CREATE MATERIALIZED VIEW reporting.revenue_daily AS\n"),
        "{ddl}"
    );
    let ddl = crate::query::ddl::create_statement(
        &table_ddl("public", "usage_events_2026_09").expect("a partition"),
    );
    assert!(
        ddl.starts_with(
            "CREATE TABLE public.usage_events_2026_09 PARTITION OF public.usage_events"
        ),
        "{ddl}"
    );
    assert!(table_ddl("public", "orders").is_none());
}

/// Each environment's probes read as the demo's story: production
/// through its replica and behind, staging a Flyway migration ahead of
/// production on ledgerlog, the stopped dev server unprobed, the local
/// container with every patch.
#[test]
fn environment_probes_differ_by_environment() {
    use crate::app::ProbeState;
    let envs = environments();
    let probes: Vec<Vec<ProbeState>> = envs
        .iter()
        .map(|r| match &r.probe {
            Some(_) => environment_probes(r)
                .into_iter()
                .map(|o| match o {
                    Some(r) => ProbeState::Done(r.map(Box::new)),
                    None => ProbeState::ViaReplica,
                })
                .collect(),
            None => vec![ProbeState::Off; r.targets.len()],
        })
        .collect();
    let version = |e: usize, t: usize| match &probes[e][t] {
        ProbeState::Done(Ok(p)) => p
            .schema_version
            .map(|v| v.to_string())
            .or(p.schema_label.clone()),
        _ => None,
    };
    assert_eq!(version(0, 1).as_deref(), Some("318"), "prod replica");
    assert_eq!(version(0, 3).as_deref(), Some("1.42"));
    assert_eq!(version(1, 1).as_deref(), Some("1.43"));
    assert_eq!(version(3, 0).as_deref(), Some("321"));
    assert!(matches!(probes[0][0], ProbeState::ViaReplica));
    assert!(matches!(probes[2][0], ProbeState::Off));
}

#[test]
fn patch_plans_hold_what_each_environment_has_not_recorded() {
    let p = patch_plan("production", "ledger-prod", "ledger", false);
    assert!(p.protected);
    assert_eq!(p.applied_max, Some(318));
    assert_eq!(p.patches.len(), 2, "the CONCURRENTLY one is left out");
    assert!(p.stopped_at.is_some() && p.outside_would_help);
    assert!(p.patches[0].file.file.starts_with("319-APP-101-"));
    let p = patch_plan("staging", "ledger-staging", "ledger", true);
    assert!(!p.protected);
    assert_eq!(p.patches.len(), 3);
    assert_eq!(p.patches[2].outside, Some("CONCURRENTLY"));
    assert!(p.stopped_at.is_none() && !p.outside_would_help);
    let p = patch_plan("local", "ledger-db-1", "ledger", false);
    assert!(p.patches.is_empty() && p.stopped_at.is_none());
    assert!(!p.outside_would_help);
}

#[test]
fn log_errors_group_the_repeats() {
    let errors = log_errors();
    let dup = errors
        .iter()
        .find(|e| e.message.contains("payments_provider_ref_key"))
        .expect("the duplicate-key error");
    assert_eq!(dup.count, 3);
    assert!(errors.iter().any(|e| e.level == "FATAL"));
}
