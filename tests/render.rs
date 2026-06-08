//! Render-path snapshots using ratatui's `TestBackend`. These don't
//! use `insta` (deliberately — avoid the dep); instead they assert
//! on specific invariants: which cells carry which colours, what
//! string appears where. That's more verbose than a full snapshot
//! but survives terminal-size tweaks and minor layout shifts.
//!
//! The tests build a real `App` via `App::new`, force splash off,
//! seed any state the scenario needs, then render once into a
//! `TestBackend` and inspect the resulting buffer.

use pgman::app::{compute_visible_rows, App, ConnState, DataSourcePick, HistorySearchState, Mode};
use pgman::conn::Dsn;
use pgman::grid::cmp_cells;
use pgman::grid::Grid;
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

/// Concatenate all cells on row `y` into a single line of glyphs.
fn row_text(buf: &Buffer, y: u16) -> String {
    let area = buf.area;
    let mut s = String::new();
    for x in 0..area.width {
        let cell = &buf[(x, y)];
        s.push_str(cell.symbol());
    }
    s.trim_end().to_string()
}

/// Full rendered buffer as `\n`-joined rows. Useful for grep-style
/// asserts (`assert!(rendered.contains("…"))`).
fn dump(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        out.push_str(&row_text(buf, y));
        out.push('\n');
    }
    out
}

/// Find the FIRST cell whose symbol matches `needle`. Returns
/// `(x, y, &Cell)`. Used to look up "where on screen did `users` get
/// rendered, and what colour is it?".
fn find_cell(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    for y in 0..buf.area.height {
        let line = row_text(buf, y);
        if let Some(col) = line.find(needle) {
            return Some((col as u16, y));
        }
    }
    None
}

fn settle_app() -> App {
    // Build an App with a placeholder DSN so the bootstrap path
    // doesn't sit on splash forever. Disconnect immediately so
    // nothing tries to phone home from inside a render test.
    let dsn = Some(Dsn::parse("postgres://test@localhost/test").unwrap());
    let picks: Vec<DataSourcePick> = Vec::new();
    let mut a = App::new(Theme::default(), dsn, picks, SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.conn_state = ConnState::Connected {
        server_version: "16.0".into(),
    };
    a
}

#[test]
fn editor_renders_keyword_in_title_colour() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT * FROM users".into();
    a.editor.cursor = 0;
    let theme = a.theme.clone();
    // Seed schema cache so `users` resolves as a known identifier.
    a.schema_cache.tables.push(TableMeta {
        schema: "public".into(),
        name: "users".into(),
    });
    let buf = render(&mut a, 80, 16);
    // Find the `S` of SELECT and check its foreground colour matches
    // `theme.title`.
    let (x, y) = find_cell(&buf, "SELECT").expect("SELECT should appear");
    let cell = &buf[(x, y)];
    assert_eq!(
        cell.fg, theme.title,
        "expected SELECT in theme.title ({:?}), got {:?}",
        theme.title, cell.fg
    );
}

#[test]
fn editor_flags_unknown_identifier_in_syn_unknown() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT * FROM zzz_definitely_not_a_table".into();
    a.editor.cursor = 0;
    // Non-empty cache so the classifier runs (without a cache the
    // renderer skips classify and falls back to default colours).
    a.schema_cache.tables.push(TableMeta {
        schema: "public".into(),
        name: "users".into(),
    });
    let theme = a.theme.clone();
    let buf = render(&mut a, 80, 16);
    let (x, y) =
        find_cell(&buf, "zzz_definitely_not_a_table").expect("unknown identifier should appear");
    let cell = &buf[(x, y)];
    assert_eq!(
        cell.fg, theme.syn_unknown,
        "unknown identifier should render in syn_unknown ({:?}), got {:?}",
        theme.syn_unknown, cell.fg
    );
}

#[test]
fn grid_render_shows_sort_marker_on_focused_column() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![
            vec!["1".into(), "alice".into()],
            vec!["2".into(), "bob".into()],
        ],
        truncated: false,
    };
    // Initialise the view state the way the run-loop would after a
    // QueryOk lands (visible rows + sort state).
    a.grid_view.visible_rows = (0..a.grid.rows.len()).collect();
    a.grid_view.col_cursor = 0;
    a.grid_view.sort = Some((0, true));
    a.grid.rows.sort_by(|x, y| cmp_cells(&x[0], &y[0]));
    let buf = render(&mut a, 60, 18);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("id ▲"),
        "expected sort marker after `id`; full render:\n{rendered}"
    );
}

#[test]
fn tap_monitor_baseline_empty_prompts_for_shift_b() {
    let mut a = settle_app();
    a.mode = Mode::TapMonitor;
    a.tap_nav.view = pgman::app::TapView::Baseline;
    let buf = render(&mut a, 120, 30);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("Shift-B"),
        "expected baseline-capture prompt; full render:\n{rendered}"
    );
}

#[test]
fn tap_monitor_baseline_view_after_capture_shows_diff_columns() {
    let mut a = settle_app();
    // Seed two events, capture, then add a new one.
    a.on_tap_event(pgman::tap::TapEvent {
        v: 1,
        kind: pgman::tap::TapKind::Query,
        ts_unix_micros: 1,
        received_at_unix_micros: 1,
        app: Some("svc".into()),
        pool: None,
        conn: None,
        txn: None,
        sql: Some("SELECT * FROM accounts".into()),
        params: None,
        params_redacted: false,
        duration_micros: Some(50),
        rows: None,
        error: None,
        caller: None,
        dropped_events_total: None,
        txn_outcome: None,
    });
    a.tap_baseline = Some(pgman::app::TapBaseline {
        captured_at_unix_micros: 1,
        captured_event_count: 1,
        captured_listener_dropped: 0,
        hotspots: a.current_hotspots(),
    });
    a.on_tap_event(pgman::tap::TapEvent {
        v: 1,
        kind: pgman::tap::TapKind::Query,
        ts_unix_micros: 2,
        received_at_unix_micros: 2,
        app: Some("svc".into()),
        pool: None,
        conn: None,
        txn: None,
        sql: Some("SELECT * FROM new_table".into()),
        params: None,
        params_redacted: false,
        duration_micros: Some(50),
        rows: None,
        error: None,
        caller: None,
        dropped_events_total: None,
        txn_outcome: None,
    });
    a.mode = Mode::TapMonitor;
    a.tap_nav.view = pgman::app::TapView::Baseline;
    let buf = render(&mut a, 140, 30);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("baseline captured"),
        "expected capture summary; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("change"),
        "expected diff column header; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("new"),
        "expected `new` change-label for the post-baseline fingerprint; full render:\n{rendered}"
    );
}

#[test]
fn tap_monitor_pools_view_renders_pool_rows() {
    let mut a = settle_app();
    // Two pools' worth of traffic.
    for (pool, conn, ts) in [
        ("primary", "p-1", 1u64),
        ("primary", "p-2", 2),
        ("replica", "r-1", 3),
    ] {
        a.on_tap_event(pgman::tap::TapEvent {
            v: 1,
            kind: pgman::tap::TapKind::Query,
            ts_unix_micros: ts,
            received_at_unix_micros: ts,
            app: Some("svc".into()),
            pool: Some(pool.into()),
            conn: Some(conn.into()),
            txn: None,
            sql: Some("SELECT 1".into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(50),
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        });
    }
    a.mode = Mode::TapMonitor;
    a.tap_nav.view = pgman::app::TapView::Pools;
    let buf = render(&mut a, 140, 30);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("peak") && rendered.contains("conns"),
        "expected pool column headers; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("primary") && rendered.contains("replica"),
        "expected both pool names; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("view: pools"),
        "expected pools view label in title; full render:\n{rendered}"
    );
}

#[test]
fn result_diff_view_renders_added_removed_changed() {
    let mut a = settle_app();
    let cols = vec!["id".to_string(), "name".to_string()];
    let a_rows = vec![
        vec!["1".to_string(), "alice".to_string()],
        vec!["2".to_string(), "bob".to_string()],
    ];
    let b_rows = vec![
        vec!["1".to_string(), "ALICE".to_string()],
        vec!["3".to_string(), "carol".to_string()],
    ];
    let key = pgman::query::row_diff::RowKey::Columns(vec![0]);
    let diff = pgman::query::row_diff::diff_rows(&a_rows, &b_rows, &key);
    let pinned = pgman::app::PinnedResult {
        columns: cols.clone(),
        rows: a_rows,
        label: "A-query".into(),
    };
    a.result_diff.active = Some(pgman::app::ResultDiffState {
        a: pinned.clone(),
        b_columns: cols,
        b_rows,
        b_label: "B-query".into(),
        key,
        diff,
    });
    a.result_diff.pinned = Some(pinned);
    a.mode = Mode::ResultDiff;
    let buf = render(&mut a, 140, 30);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("Result diff"),
        "title missing:\n{rendered}"
    );
    assert!(
        rendered.contains("added") && rendered.contains("removed") && rendered.contains("changed"),
        "summary line missing:\n{rendered}"
    );
    // id 2 (bob) removed, id 3 (carol) added, id 1 changed alice→ALICE.
    assert!(rendered.contains("bob"), "removed row missing:\n{rendered}");
    assert!(rendered.contains("carol"), "added row missing:\n{rendered}");
    assert!(
        rendered.contains("ALICE"),
        "changed cell missing:\n{rendered}"
    );
}

#[test]
fn saved_queries_panel_filters_live_and_shows_count() {
    let mut a = settle_app();
    for (n, b) in [
        ("users", "SELECT * FROM users"),
        ("orders", "SELECT * FROM orders"),
        ("revenue", "SELECT sum(amount)"),
    ] {
        a.saved_queries.upsert(pgman::saved::SavedQuery {
            name: n.into(),
            body: b.into(),
        });
    }
    a.saved_ui.filter = Some("ord".into());
    a.mode = Mode::SavedQueriesFilter;
    let buf = render(&mut a, 120, 24);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("/ord"),
        "filter not in title:\n{rendered}"
    );
    assert!(rendered.contains("1/3 shown"), "count missing:\n{rendered}");
    assert!(rendered.contains("orders"), "match missing:\n{rendered}");
    // Non-matching entries are filtered out of the list + detail.
    assert!(
        !rendered.contains("revenue"),
        "filtered entry leaked:\n{rendered}"
    );
}

#[test]
fn rename_prompt_renders_prefilled_name() {
    let mut a = settle_app();
    a.saved_queries.upsert(pgman::saved::SavedQuery {
        name: "old-name".into(),
        body: "SELECT 1".into(),
    });
    a.saved_ui.rename_from = "old-name".into();
    a.saved_ui.rename_buf = "new-name".into();
    a.mode = Mode::RenameQueryPrompt;
    let buf = render(&mut a, 120, 24);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("rename 'old-name'"),
        "title missing:\n{rendered}"
    );
    assert!(
        rendered.contains("new-name"),
        "edited buffer missing:\n{rendered}"
    );
}

#[test]
fn demo_app_renders_grid_schema_and_tap_without_panic() {
    let mut a = pgman::demo::app(Theme::default());
    a.splash_visible = false;
    a.splash_until = None;
    // Normal: the users result grid.
    let rendered = dump(&render(&mut a, 140, 30));
    assert!(
        rendered.contains("ada@example.com"),
        "grid data missing:\n{rendered}"
    );
    // Schema browser opens against the fixture cache (3 tables,
    // collapsed under the public schema node).
    a.mode = Mode::SchemaBrowser;
    let rendered = dump(&render(&mut a, 140, 30));
    assert!(
        rendered.contains("public") && rendered.contains("3 table(s)"),
        "schema browser missing:\n{rendered}"
    );
    // Tap monitor shows the synthetic events.
    a.mode = Mode::TapMonitor;
    let rendered = dump(&render(&mut a, 140, 30));
    assert!(
        rendered.contains("order_items"),
        "tap events missing:\n{rendered}"
    );
}

#[test]
fn param_prompt_renders_progress_and_entered_values() {
    let mut a = settle_app();
    a.saved_ui.param_prompt = Some(pgman::app::ParamPrompt {
        query_name: "by-id".into(),
        template: "WHERE id = :id AND org = :org".into(),
        params: vec!["id".into(), "org".into()],
        idx: 1,
        values: vec!["42".into()],
        input: "acme".into(),
    });
    a.mode = Mode::ParamPrompt;
    let buf = render(&mut a, 120, 20);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("by-id"),
        "query name missing:\n{rendered}"
    );
    assert!(
        rendered.contains("param 2/2"),
        "progress missing:\n{rendered}"
    );
    // The already-entered first value is echoed back.
    assert!(
        rendered.contains(":id = 42"),
        "entered value missing:\n{rendered}"
    );
    // The current prompt names the second placeholder.
    assert!(
        rendered.contains(":org"),
        "current prompt missing:\n{rendered}"
    );
    assert!(
        rendered.contains("acme"),
        "input buffer missing:\n{rendered}"
    );
}

#[test]
fn tap_monitor_empty_state_renders_setup_hint_with_both_routes() {
    let mut a = settle_app();
    a.mode = Mode::TapMonitor;
    // No events and no heartbeats — the "no JAR connected"
    // branch, which must render the setup hint.
    let buf = render(&mut a, 120, 40);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("Route 1: OpenTelemetry"),
        "expected OTel route hint; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("--tap-otlp :4318"),
        "expected pgman flag hint; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("pgman-tap-spring-boot-starter"),
        "expected Spring Boot starter snippet; full render:\n{rendered}"
    );
}

#[test]
fn tap_monitor_list_renders_recent_events_with_title() {
    let mut a = settle_app();
    let evt = pgman::tap::TapEvent {
        v: 1,
        kind: pgman::tap::TapKind::Query,
        ts_unix_micros: 0,
        received_at_unix_micros: 1,
        app: Some("billing-service".into()),
        pool: None,
        conn: None,
        txn: None,
        sql: Some("SELECT * FROM accounts WHERE id = ?".into()),
        params: None,
        params_redacted: false,
        duration_micros: Some(4_521),
        rows: Some(17),
        error: None,
        caller: None,
        dropped_events_total: None,
        txn_outcome: None,
    };
    a.on_tap_event(evt);
    a.mode = Mode::TapMonitor;
    let buf = render(&mut a, 120, 24);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("JDBC tap"),
        "expected title; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("view: list"),
        "expected view label in title; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("billing-service"),
        "expected app name; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("4.5ms"),
        "expected formatted duration; full render:\n{rendered}"
    );
}

#[test]
fn tap_monitor_hotspots_view_groups_and_shows_sort() {
    let mut a = settle_app();
    // Three events that fingerprint to one shape.
    for i in 0..3 {
        a.on_tap_event(pgman::tap::TapEvent {
            v: 1,
            kind: pgman::tap::TapKind::Query,
            ts_unix_micros: i,
            received_at_unix_micros: i,
            app: Some("svc".into()),
            pool: None,
            conn: None,
            txn: None,
            sql: Some(format!("SELECT * FROM t WHERE id = {i}")),
            params: None,
            params_redacted: false,
            duration_micros: Some(100 * (i + 1)),
            rows: Some(1),
            error: None,
            caller: Some(vec!["svc.Foo.bar:1".into()]),
            dropped_events_total: None,
            txn_outcome: None,
        });
    }
    a.mode = Mode::TapMonitor;
    a.tap_nav.view = pgman::app::TapView::Hotspots;
    a.tap_nav.sort = pgman::tap::HotspotSort::CallCount;
    let buf = render(&mut a, 140, 24);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("view: hotspots"),
        "expected hotspots view label; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("sort: call count"),
        "expected sort label; full render:\n{rendered}"
    );
    assert!(
        rendered.contains("svc.Foo.bar:1"),
        "expected last-caller column; full render:\n{rendered}"
    );
    // Three events collapsed into one bucket → one rendered
    // row with calls=3 alongside the caller frame. The popup
    // border chars sit at the line start, so check for the
    // calls/err columns appearing together with the caller.
    let has_three_row = rendered
        .lines()
        .any(|l| l.contains("svc.Foo.bar:1") && l.contains("3 ") && l.contains("200µs"));
    assert!(
        has_three_row,
        "expected one row with calls=3 and the caller; full render:\n{rendered}"
    );
}

#[test]
fn grid_render_shows_capped_hint_when_truncated() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["id".into()],
        rows: vec![vec!["1".into()], vec!["2".into()]],
        truncated: true,
    };
    a.grid_view.visible_rows = (0..a.grid.rows.len()).collect();
    let buf = render(&mut a, 60, 18);
    let rendered = dump(&buf);
    assert!(
        rendered.contains(&format!("capped at {}", pgman::grid::MAX_ROWS)),
        "expected `capped at {}` in title; full render:\n{rendered}",
        pgman::grid::MAX_ROWS,
    );
}

#[test]
fn grid_render_shows_filtered_count_in_title() {
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
    a.grid_state.select(if a.grid_view.visible_rows.is_empty() {
        None
    } else {
        Some(0)
    });
    let buf = render(&mut a, 60, 18);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("2/3 row(s) (filtered)"),
        "expected `2/3 row(s) (filtered)` in title; full render:\n{rendered}"
    );
}

#[test]
fn help_overlay_lists_key_bindings() {
    let mut a = settle_app();
    a.mode = Mode::Help;
    // Tall viewport so the whole help text fits without scrolling —
    // the assertions below cover bindings from multiple sections.
    // Help body grew with the schema wizard / lint / bookmarks etc.
    // — bump the height to accommodate.
    let buf = render(&mut a, 120, 200);
    let rendered = dump(&buf);
    for binding in &["ctrl-r", "ctrl-w", "ctrl-x", "ctrl-f", "F5"] {
        assert!(
            rendered.contains(binding),
            "help should mention `{binding}`; full render:\n{rendered}"
        );
    }
}

#[test]
fn history_search_status_renders_bash_style_prompt() {
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
    let rendered = dump(&buf);
    assert!(
        rendered.contains("(reverse-i-search) 'sel'"),
        "footer should show the reverse-i-search prompt; full render:\n{rendered}"
    );
}
