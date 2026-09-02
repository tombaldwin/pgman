//! `insta` snapshot tests on the render path. Each test sets up an
//! `App` in a specific mode / state, renders into a fixed-size
//! `TestBackend`, and snapshots the resulting glyph layout.
//!
//! First run: `cargo test --test snapshots` creates `.snap.new`
//! files under `tests/snapshots/`. Run `cargo insta review` to
//! accept them; `cargo insta accept` if you trust the diff. They
//! get committed to the repo so subsequent runs diff against the
//! accepted state.
//!
//! Style info is intentionally lost — these snapshots verify
//! layout, not colour. Colour invariants live in
//! `tests/render.rs` which inspects specific cells.

use pgman::app::{
    compute_visible_rows, App, CompletionCycle, ConnState, DataSourcePick, HistorySearchState,
    Mode, WatchState,
};
use pgman::conn::Dsn;
use pgman::grid::Grid;
use pgman::query::complete::{Candidate, CandidateKind};
use pgman::query::schema::TableMeta;
use pgman::safety::SafetyConfig;
use pgman::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

fn render(app: &mut App, w: u16, h: u16) -> Buffer {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).expect("terminal");
    term.draw(|f| pgman::ui::draw(f, app)).expect("draw");
    term.backend().buffer().clone()
}

/// Flatten a Buffer to `\n`-joined glyph rows, trimming trailing
/// whitespace per row (insta diffs handle the rest).
fn dump(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        out.push_str(s.trim_end());
        out.push('\n');
    }
    out
}

/// Baseline `App` used by most snapshot tests below. The result panel
/// carries a grid with columns but no rows — i.e. "a query ran and came
/// back empty" — so tests that pop a modal over the body (help, slow
/// queries, …) keep seeing the plain `(no rows)` placeholder behind it,
/// same as before the start card existed. Tests that specifically want
/// the "nothing has run yet" start card reset `a.grid` back to
/// `Grid::default()` (see `landing_after_connect`).
fn settle_app() -> App {
    let dsn = Some(Dsn::parse("postgres://test@localhost/test").unwrap());
    let picks: Vec<DataSourcePick> = Vec::new();
    let mut a = App::new(Theme::default(), dsn, picks, SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.conn_state = ConnState::Connected {
        server_version: "16.0".into(),
    };
    a.grid = Grid {
        columns: vec!["placeholder".into()],
        rows: vec![],
        truncated: false,
    };
    a
}

/// Replaces the old `empty_normal_mode` snapshot: with nothing ever run
/// (`grid` at its `App::new` default — empty columns, empty rows), the
/// body shows the start card instead of a bare `(no rows)`.
#[test]
fn landing_after_connect() {
    let mut a = settle_app();
    a.grid = Grid::default();
    a.mode = Mode::Normal;
    let buf = render(&mut a, 80, 16);
    insta::assert_snapshot!(dump(&buf));
}

/// A query that legitimately returns zero rows must still show
/// `(no rows)`, not the start card — `Grid.columns` being non-empty is
/// what tells "ran a query, got nothing" apart from "nothing run yet".
#[test]
fn no_rows_after_empty_query_result() {
    let mut a = settle_app();
    a.grid = Grid {
        columns: vec!["id".into(), "email".into()],
        rows: vec![],
        truncated: false,
    };
    a.mode = Mode::Normal;
    let buf = render(&mut a, 80, 16);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn editor_with_sql_buffer() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT id, email\nFROM users\nWHERE active = true".into();
    a.editor.cursor = 0;
    a.schema_cache.tables.push(TableMeta {
        schema: "public".into(),
        name: "users".into(),
    });
    let buf = render(&mut a, 80, 16);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn grid_with_data_and_sort() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![
            vec!["1".into(), "alice".into()],
            vec!["2".into(), "bob".into()],
            vec!["3".into(), "carol".into()],
        ],
        truncated: false,
    };
    a.grid_view.visible_rows = (0..a.grid.rows.len()).collect();
    a.grid_view.col_cursor = 0;
    a.grid_view.sort = Some((0, true));
    a.grid_state.select(Some(0));
    let buf = render(&mut a, 60, 14);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn grid_with_filter_active() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["name".into()],
        rows: vec![
            vec!["alice".into()],
            vec!["bob".into()],
            vec!["carol".into()],
        ],
        truncated: false,
    };
    a.grid_view.filter = Some("a".into());
    a.grid_view.visible_rows = compute_visible_rows(&a.grid.rows, Some("a"));
    a.grid_state.select(Some(0));
    let buf = render(&mut a, 60, 14);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn help_overlay() {
    let mut a = settle_app();
    a.mode = Mode::Help;
    // Tall viewport — the help popup is centered_pct(area, 70, 70)
    // so the visible content area is ~70% of the test height. With
    // ~70 help lines we need ~100 rows to fit them all.
    let buf = render(&mut a, 100, 110);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn about_overlay() {
    let mut a = settle_app();
    a.mode = Mode::About;
    let buf = render(&mut a, 100, 28);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn history_search_in_progress() {
    let mut a = settle_app();
    a.mode = Mode::HistorySearch;
    a.history.push("SELECT * FROM users".into());
    a.history_search = Some(HistorySearchState {
        query: "sel".into(),
        matched: Some(0),
        saved_buffer: String::new(),
        saved_cursor: 0,
    });
    a.last_status = Some("(reverse-i-search) 'sel'".into());
    a.editor.buffer = "SELECT * FROM users".into();
    a.editor.cursor = a.editor.buffer.len();
    let buf = render(&mut a, 80, 16);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn watch_mode_status_visible() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT count(*) FROM users".into();
    a.editor.cursor = a.editor.buffer.len();
    a.watch = Some(WatchState {
        sql: "SELECT count(*) FROM users".into(),
        interval: std::time::Duration::from_secs(2),
        last_fire: std::time::Instant::now(),
    });
    a.last_status = Some("\\watch every 2s · any key to stop".into());
    let buf = render(&mut a, 80, 16);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn completion_popup_with_candidates() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT * FROM us".into();
    a.editor.cursor = a.editor.buffer.len();
    a.completion = Some(CompletionCycle {
        start: a.editor.cursor - 2,
        end: a.editor.cursor,
        origin: "us".into(),
        origin_prefix: "us".into(),
        origin_cursor: a.editor.cursor,
        candidates: vec![
            Candidate {
                display: "users".into(),
                insert: "users".into(),
                kind: CandidateKind::Table,
                context: None,
            },
            Candidate {
                display: "user_logs".into(),
                insert: "user_logs".into(),
                kind: CandidateKind::Table,
                context: None,
            },
        ],
        selected: None,
    });
    let buf = render(&mut a, 80, 18);
    insta::assert_snapshot!(dump(&buf));
}

/// Narrow terminal + long candidate/context strings: the popup's
/// *desired* width (content + title) exceeds what the result panel has
/// room for, so it must shrink to fit rather than spill past the
/// panel's own right border. Pins the clamping path in
/// `draw_completion_popup` alongside the happy-path test above.
#[test]
fn completion_popup_narrow_terminal_clamps_width() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT * FROM au".into();
    a.editor.cursor = a.editor.buffer.len();
    a.completion = Some(CompletionCycle {
        start: a.editor.cursor - 2,
        end: a.editor.cursor,
        origin: "au".into(),
        origin_prefix: "au".into(),
        origin_cursor: a.editor.cursor,
        candidates: vec![
            Candidate {
                display: "authentication_audit_log_entries".into(),
                insert: "authentication_audit_log_entries".into(),
                kind: CandidateKind::Table,
                context: Some("public_analytics_reporting_schema".into()),
            },
            Candidate {
                display: "auth_sessions".into(),
                insert: "auth_sessions".into(),
                kind: CandidateKind::Table,
                context: Some("public".into()),
            },
        ],
        selected: None,
    });
    let buf = render(&mut a, 40, 12);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn slow_queries_renders_top_n_panel() {
    use pgman::query::slow_queries::SlowQueryRow;
    let mut a = settle_app();
    a.mode = Mode::SlowQueries;
    a.slow_queries.rows = vec![
        SlowQueryRow {
            query: "SELECT * FROM users WHERE active = true".into(),
            calls: 1_000_000,
            total_ms: 12345.6,
            mean_ms: 12.34,
            rows: 1_000_000,
        },
        SlowQueryRow {
            query: "UPDATE orders SET status = $1 WHERE id = $2".into(),
            calls: 250,
            total_ms: 800.0,
            mean_ms: 3.2,
            rows: 250,
        },
    ];
    a.slow_queries.cursor = 0;
    let buf = render(&mut a, 110, 26);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn sessions_renders_blocked_then_idle() {
    use pgman::query::sessions::SessionRow;
    let mut a = settle_app();
    a.mode = Mode::Sessions;
    a.sessions.rows = vec![
        SessionRow {
            pid: 1234,
            user: "alice".into(),
            application: "psql".into(),
            state: "active".into(),
            wait_event: Some("Lock:transactionid".into()),
            blocked_by: "5678".into(),
            query: "UPDATE accounts SET balance = 0".into(),
            age_secs: 12.5,
        },
        SessionRow {
            pid: 5678,
            user: "bob".into(),
            application: "pgman".into(),
            state: "idle in transaction".into(),
            wait_event: None,
            blocked_by: String::new(),
            query: "BEGIN".into(),
            age_secs: 300.0,
        },
    ];
    a.sessions.cursor = 0;
    let buf = render(&mut a, 110, 18);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn schema_browser_renders_focused_table_details() {
    use pgman::query::schema::{ConstraintMeta, SchemaCache, TableMeta};
    let mut a = settle_app();
    let mut cache = SchemaCache {
        schemas: vec!["audit".into(), "public".into()],
        tables: vec![
            TableMeta {
                schema: "public".into(),
                name: "users".into(),
            },
            TableMeta {
                schema: "public".into(),
                name: "orders".into(),
            },
            TableMeta {
                schema: "audit".into(),
                name: "events".into(),
            },
        ],
        ..Default::default()
    };
    cache.columns_by_table.insert(
        ("public".into(), "users".into()),
        vec!["id".into(), "email".into(), "active".into()],
    );
    cache.constraints.push(ConstraintMeta {
        schema: "public".into(),
        table: "users".into(),
        name: "users_email_key".into(),
    });
    a.schema_cache = cache;
    a.mode = Mode::SchemaBrowser;
    a.schema_browser.expanded.insert("public".into());
    // Focus the `users` table — row order: audit, public, orders, users.
    a.schema_browser.cursor = 3;
    let buf = render(&mut a, 100, 24);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn explain_tree_renders_hash_join_plan() {
    let mut a = settle_app();
    let json = r#"[{
      "Plan": {
        "Node Type": "Hash Join",
        "Total Cost": 200.0,
        "Actual Total Time": 50.0,
        "Plan Rows": 5000,
        "Actual Rows": 4500,
        "Hash Cond": "(o.user_id = u.id)",
        "Plans": [
          { "Node Type": "Seq Scan", "Relation Name": "orders",
            "Alias": "o", "Total Cost": 100.0,
            "Actual Total Time": 30.0, "Plan Rows": 10000 },
          { "Node Type": "Hash", "Total Cost": 22.5,
            "Actual Total Time": 5.0,
            "Plans": [
              { "Node Type": "Seq Scan", "Relation Name": "users",
                "Alias": "u", "Total Cost": 22.5,
                "Actual Total Time": 4.0 }
            ]
          }
        ]
      }
    }]"#;
    a.explain.plan = Some(pgman::query::explain::parse(json).unwrap());
    a.mode = Mode::ExplainTree;
    a.explain.cursor = 0; // root highlighted
    let buf = render(&mut a, 100, 24);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn cell_detail_json_tree_renders_object_with_cursor_on_root() {
    let mut a = settle_app();
    a.grid = Grid {
        columns: vec!["data".into()],
        rows: vec![vec![r#"{"id":1,"tags":["a","b"],"meta":{"k":"v"}}"#.into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.row_detail.field = 0;
    a.row_detail.field_count = 1;
    a.mode = Mode::RowDetail;
    // Drive the open path so json_cell_rows / value get populated.
    a.on_key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Enter,
    ));
    assert_eq!(a.mode, Mode::CellDetail);
    let buf = render(&mut a, 80, 20);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn connection_picker_with_two_entries() {
    let theme = Theme::default();
    let picks = vec![
        DataSourcePick {
            name: "prod".into(),
            origin: "project",
            dsn: Dsn::parse("postgres://app@prod-db/main").unwrap(),
        },
        DataSourcePick {
            name: "staging".into(),
            origin: "IntelliJ",
            dsn: Dsn::parse("postgres://app@staging-db/main").unwrap(),
        },
    ];
    let mut a = App::new(theme, None, picks, SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.mode = Mode::ConnPick;
    let buf = render(&mut a, 80, 16);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn schema_wizard_renders_findings_sorted_by_severity() {
    let mut a = settle_app();
    let cache = pgman::query::schema::SchemaCache {
        schemas: vec!["public".into()],
        tables: vec![
            // events → no constraints (LINT001 High)
            pgman::query::schema::TableMeta {
                schema: "public".into(),
                name: "events".into(),
            },
            // OrderItems → mixed-case (LINT002 Med) AND in a schema
            // that mixes naming with `events` (LINT004 Low).
            pgman::query::schema::TableMeta {
                schema: "public".into(),
                name: "OrderItems".into(),
            },
            // user → reserved keyword (LINT003 Med)
            pgman::query::schema::TableMeta {
                schema: "public".into(),
                name: "user".into(),
            },
        ],
        ..Default::default()
    };
    a.schema_cache = cache;
    a.schema_lint.findings = pgman::query::lint::run_all(&a.schema_cache);
    a.schema_lint.cursor = 0;
    a.mode = Mode::SchemaLint;
    let buf = render(&mut a, 110, 24);
    insta::assert_snapshot!(dump(&buf));
}
