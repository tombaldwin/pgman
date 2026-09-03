//! Render-path snapshots using ratatui's `TestBackend`. These don't
//! use `insta` (deliberately — avoid the dep); instead they assert
//! on specific invariants: which cells carry which colours, what
//! string appears where. That's more verbose than a full snapshot
//! but survives terminal-size tweaks and minor layout shifts.
//!
//! The tests build a real `App` via `App::new`, force splash off,
//! seed any state the scenario needs, then render once into a
//! `TestBackend` and inspect the resulting buffer.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

/// Cell-exact (not byte-offset) search for a two-character needle like
/// `"RO"`: `row_text` + `str::find` mis-locate the column whenever a
/// multi-byte glyph (e.g. `·`) appears earlier in the same row, since
/// `find` returns a byte offset, not a terminal column.
fn find_two_char_cells(buf: &Buffer, needle: [&str; 2]) -> Vec<(u16, u16)> {
    let mut hits = Vec::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width.saturating_sub(1) {
            if buf[(x, y)].symbol() == needle[0] && buf[(x + 1, y)].symbol() == needle[1] {
                hits.push((x, y));
            }
        }
    }
    hits
}

#[test]
fn landing_ro_badge_reuses_footer_style() {
    // A freshly-connected App (grid still at its `App::new` default —
    // nothing has run) renders the start card, whose connection line
    // ends in an inline `RO` token. It must reuse the exact style the
    // footer's `[RO]` pill uses, not a separately hardcoded colour.
    let mut a = settle_app();
    a.mode = Mode::Normal;
    let buf = render(&mut a, 80, 16);
    let ro_cells = find_two_char_cells(&buf, ["R", "O"]);
    assert!(
        ro_cells.len() >= 2,
        "expected both the start card's header and the footer badge to show RO: {ro_cells:?}"
    );
    let header_cell = &buf[ro_cells[0]];
    let footer_cell = &buf[ro_cells[ro_cells.len() - 1]];
    assert_eq!(
        header_cell.fg, footer_cell.fg,
        "start card RO should reuse the footer badge's fg, not a separate colour"
    );
    assert_eq!(
        header_cell.bg, footer_cell.bg,
        "start card RO should reuse the footer badge's bg, not a separate colour"
    );
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
    // Schema browser opens against the fixture cache (4 tables,
    // collapsed under the public schema node).
    a.mode = Mode::SchemaBrowser;
    let rendered = dump(&render(&mut a, 140, 30));
    assert!(
        rendered.contains("public") && rendered.contains("4 table(s)"),
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
    // Route 2's heading must say what it is — unreleased — and must
    // NOT print a Gradle coordinate: the one that used to be here
    // (`co.polymorphism:pgman-tap-spring-boot-starter:0.1.0`) resolves
    // to nothing, so a build file edited to use it fails.
    assert!(
        rendered.contains("Route 2: pgman-tap — not yet released"),
        "expected the unreleased Route 2 heading; full render:\n{rendered}"
    );
    assert!(
        !rendered.contains("pgman-tap-spring-boot-starter"),
        "a version-pinned coordinate that resolves to nothing was \
         printed as if it were installable; full render:\n{rendered}"
    );
    assert!(
        !rendered.contains("pgman.tap.endpoint"),
        "an application.yml snippet for an unreleased JAR was printed; \
         full render:\n{rendered}"
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

/// The connection picker floats one row and one column inside the
/// results panel, and pads its origin column outside the brackets.
/// Centred, the popup's own top border landed on the panel's border
/// and title (`┌ pgman┌ pick a connection`); padded inside, the tag
/// read `[ project]`, fencing off a run of spaces that belongs to the
/// column, not to the tag.
#[test]
fn connection_picker_floats_inside_the_panel_and_pads_outside_brackets() {
    let picks = vec![
        DataSourcePick {
            name: "prod".into(),
            origin: "project",
            dsn: Some(Dsn::parse("postgres://app@prod-db/main").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
        DataSourcePick {
            name: "staging".into(),
            origin: "IntelliJ",
            dsn: Some(Dsn::parse("postgres://app@staging-db/main").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
    ];
    let mut a = App::new(Theme::default(), None, picks, SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.mode = Mode::ConnPick;
    let buf = render(&mut a, 80, 16);
    let rendered = dump(&buf);

    // Padding outside the brackets, never inside.
    assert!(
        rendered.contains("[project]") && rendered.contains("[IntelliJ]"),
        "origin tags should be padded outside the brackets:\n{rendered}"
    );
    assert!(
        !rendered.contains("[ project]"),
        "origin tag was padded inside its brackets:\n{rendered}"
    );

    // The panel's own top border row is intact — the popup starts on
    // the row below it, and one column in.
    let lines: Vec<&str> = rendered.lines().collect();
    let panel_top = lines
        .iter()
        .position(|l| l.starts_with("┌ pgman"))
        .expect("results panel border");
    assert!(
        !lines[panel_top].contains("pick a connection"),
        "picker landed on the panel's top border row:\n{rendered}"
    );
    assert!(
        lines[panel_top + 1].starts_with("│┌ pick a connection"),
        "picker should float one row/column inside the panel:\n{rendered}"
    );

    // Every popup row keeps a blank column before its right border, so
    // the longest target doesn't read as jammed against the frame.
    let right_border = lines[panel_top + 1]
        .chars()
        .position(|c| c == '┐')
        .expect("popup right border");
    for line in &lines[panel_top + 2..] {
        let chars: Vec<char> = line.chars().collect();
        if chars.get(right_border) != Some(&'│') {
            continue;
        }
        assert_eq!(
            chars.get(right_border - 1),
            Some(&' '),
            "picker row touches the popup's right border:\n{rendered}"
        );
    }
}

/// The sessions panel left-aligns its text columns and right-aligns
/// its numbers. Right-aligned, `alice/psql` and `idle in tx` sat flush
/// against the column's far edge, nowhere near the header label above
/// them.
#[test]
fn sessions_panel_left_aligns_text_columns_and_right_aligns_numbers() {
    use pgman::query::sessions::SessionRow;
    let mut a = settle_app();
    a.mode = Mode::Sessions;
    a.sessions.rows = vec![SessionRow {
        pid: 1234,
        user: "alice".into(),
        application: "psql".into(),
        state: "idle in transaction".into(),
        wait_event: None,
        blocked_by: String::new(),
        query: "BEGIN".into(),
        age_secs: 12.5,
    }];
    a.sessions.cursor = 0;
    let buf = render(&mut a, 110, 18);
    let rendered = dump(&buf);
    let lines: Vec<&str> = rendered.lines().collect();
    let header = lines
        .iter()
        .find(|l| l.contains("user/app"))
        .expect("sessions header");
    let row = lines
        .iter()
        .find(|l| l.contains("alice/psql"))
        .expect("sessions row");

    let col_of = |hay: &str, needle: &str| -> usize {
        let byte = hay.find(needle).expect(needle);
        hay[..byte].chars().count()
    };
    // Text columns: value starts in the same column as its label.
    assert_eq!(
        col_of(header, "user/app"),
        col_of(row, "alice/psql"),
        "user/app column is not left-aligned:\n{rendered}"
    );
    assert_eq!(
        col_of(header, "state"),
        col_of(row, "idle in tx"),
        "state column is not left-aligned:\n{rendered}"
    );
    // Numbers: value ENDS in the same column as its label.
    assert_eq!(
        col_of(header, "pid") + "pid".chars().count(),
        col_of(row, "1234") + "1234".chars().count(),
        "pid column is not right-aligned:\n{rendered}"
    );
    assert_eq!(
        col_of(header, "age(s)") + "age(s)".chars().count(),
        col_of(row, "12.5") + "12.5".chars().count(),
        "age(s) column is not right-aligned:\n{rendered}"
    );
}

/// `draw_help` pre-scrolls to the section matching the mode help was
/// opened from. It used to STORE that anchor row unclamped and only
/// clamp it for display, so an anchor near the end of the document
/// left `help.scroll` above `help.max_scroll` — and the first few `k`
/// presses walked the stored value back down through a range that all
/// renders identically, so the overlay looked frozen.
#[test]
fn help_anchor_is_clamped_when_stored_not_only_when_rendered() {
    // "schema wizard" is the last anchored section in `help_body`, so
    // its row is past `max_scroll` at any realistic overlay height.
    // A tall terminal makes the viewport big enough that the last
    // section's row sits past the end of the scroll range — exactly
    // the case the unclamped store got wrong.
    let mut a = settle_app();
    a.open_help_from(Mode::SchemaLint);
    let _ = render(&mut a, 200, 80);
    assert!(a.help.max_scroll > 0, "help body should be scrollable");
    assert!(
        a.help.scroll <= a.help.max_scroll,
        "anchor stored an unclamped scroll: {} > max {}",
        a.help.scroll,
        a.help.max_scroll
    );
    // And it really did anchor somewhere near the end, rather than
    // passing by accident because it never scrolled at all.
    assert_eq!(
        a.help.scroll, a.help.max_scroll,
        "expected the last section to anchor at the bottom of the range"
    );
}

/// The log picker scrolls its rows to follow the cursor. Without an
/// offset only the first screenful of picks was ever drawn, so `j` /
/// `G` walked the index — and the title's `n/total` — off the bottom
/// of a popup that never moved.
#[test]
fn log_picker_scrolls_to_keep_the_focused_row_visible() {
    use pgman::query::reconstruct::{ReconstructedQuery, Source};
    let mut a = settle_app();
    a.mode = pgman::app::Mode::LogPick;
    a.log_pick.picks = (0..100)
        .map(|i| ReconstructedQuery {
            raw_sql: format!("select * from t where id = {i}"),
            runnable_sql: format!("select * from t{i} where id = {i}"),
            params: Vec::new(),
            source: Source::HibernateLog,
            src_line: i,
        })
        .collect();
    a.log_pick.index = 50;

    let buf = render(&mut a, 120, 40);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("select * from t50 where id = 50"),
        "row 50 of 100 is off-screen:\n{rendered}"
    );
    // The focused row carries the ▶ marker, and it is the one shown.
    assert!(
        rendered.contains("▶ [hibernate] select * from t50"),
        "the focused row is not the marked one:\n{rendered}"
    );
    // The title agrees with what is drawn.
    assert!(rendered.contains("log picks · 51/100"), "{rendered}");
    // And the rows above the window really did scroll away.
    assert!(
        !rendered.contains("select * from t0 where id = 0"),
        "row 0 should have scrolled out of view:\n{rendered}"
    );
}

/// The picker's triage header reads the `log_pick.clusters` cache —
/// written when the picks are set and on every view toggle — instead
/// of re-running `nplus1::summarize`, which re-fingerprints and
/// re-clusters the whole import on every frame, animation ticks
/// included. Pinned by giving the cache a value that a live re-detect
/// would not produce.
#[test]
fn log_picker_summary_comes_from_the_cluster_cache() {
    use pgman::query::reconstruct::{ReconstructedQuery, Source};
    let mut a = settle_app();
    a.mode = pgman::app::Mode::LogPick;
    // Three copies of one statement: a live re-detect finds one
    // cluster of three.
    a.log_pick.picks = (0..3)
        .map(|i| ReconstructedQuery {
            raw_sql: "select * from item where order_id = ?".into(),
            runnable_sql: format!("select * from item where order_id = {i}"),
            params: Vec::new(),
            source: Source::HibernateLog,
            src_line: i,
        })
        .collect();
    // The cache says otherwise. Rendering from it is the whole point.
    a.log_pick.clusters = Vec::new();

    let buf = render(&mut a, 120, 40);
    let rendered = dump(&buf);
    assert!(rendered.contains("3 queries"), "{rendered}");
    assert!(
        !rendered.contains("N+1 cluster"),
        "the summary re-detected instead of reading the cache:\n{rendered}"
    );
}

/// `--log` accepts a file that is not a log at all. Tokenising a
/// 200 MB one produced 68 M highlight spans and a 1.9 GB RSS — every
/// frame, for a pane that shows ten lines. Above 256 KiB the editor
/// renders plain and says so in its title.
#[test]
fn large_editor_buffer_renders_plain_with_a_note_in_the_title() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    // 300 KiB of SQL-ish text: past the cap, and every line would
    // otherwise be tokenised and classified against the schema cache.
    a.editor.buffer = "select id, name from users where id = 1;\n".repeat(7_800);
    assert!(a.editor.buffer.len() > 300 * 1024);
    a.editor.cursor = 0;

    let buf = render(&mut a, 120, 40);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("(large buffer — highlighting off)"),
        "expected the plain-render note in the editor title:\n{}",
        rendered.lines().take(3).collect::<Vec<_>>().join("\n")
    );
    // The buffer still renders — plain, not blank.
    assert!(
        rendered.contains("select id, name from users where id = 1;"),
        "the buffer did not render at all"
    );
    // And nothing was cached for it.
    assert!(
        a.editor_highlight_cache.is_none(),
        "a 300 KiB buffer was tokenised and cached anyway"
    );
}

/// Under the cap nothing changes: highlighting stays on and the title
/// carries no note.
#[test]
fn ordinary_editor_buffer_keeps_its_highlighting() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = "select id from users;".into();
    a.editor.cursor = 0;
    let buf = render(&mut a, 120, 40);
    let rendered = dump(&buf);
    assert!(!rendered.contains("large buffer"), "{rendered}");
    assert!(a.editor_highlight_cache.is_some());
}

/// While the ssh-tunnel confirmation is up, the picker's list keys are
/// wrong: `enter` does nothing and `q` cancels along with every other
/// key. The footer said `enter connect · q quit` under a prompt asking
/// whether to run `ssh` with the operator's keys.
#[test]
fn conn_pick_footer_states_the_tunnel_prompt_keys() {
    use crossterm::event::{KeyCode, KeyEvent};
    let picks = vec![DataSourcePick {
        name: "via-bastion".into(),
        origin: "project",
        dsn: Some(
            Dsn::parse("postgres://app@db.internal:5432/main?ssh_tunnel=tom@bastion.example.com")
                .unwrap(),
        ),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    }];
    let mut a = App::new(Theme::default(), None, picks, SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.mode = Mode::ConnPick;

    // Before: the list keys are the right ones.
    let before = dump(&render(&mut a, 100, 20));
    let footer_before = before.lines().last().unwrap().to_string();
    assert!(footer_before.contains("enter connect"), "{footer_before}");

    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(a.pending_tunnel.is_some(), "expected the tunnel prompt");

    let after = dump(&render(&mut a, 100, 20));
    let footer = after.lines().last().unwrap().to_string();
    assert!(
        footer.contains("y proceed · any other key cancels"),
        "footer should state the prompt's keys: {footer:?}"
    );
    assert!(
        !footer.contains("enter connect") && !footer.contains("q quit"),
        "footer still offers the list keys: {footer:?}"
    );
    // No `F1 help` pointer either — F1 is "any other key" here, and it
    // would cancel rather than open help.
    assert!(!footer.contains("F1 help"), "{footer:?}");
}

/// The header and the start card show `pg 16.15`; the About card is
/// where the packager's full string can still be read, which is what
/// makes shortening it elsewhere a trim rather than a loss.
#[test]
fn about_card_carries_the_full_server_version() {
    let mut a = settle_app();
    a.conn_state = ConnState::Connected {
        server_version: "16.15 (Debian 16.15-1.pgdg13+2)".into(),
    };
    a.mode = Mode::About;
    let rendered = dump(&render(&mut a, 120, 40));
    assert!(
        rendered.contains("server: pg 16.15 (Debian 16.15-1.pgdg13+2)"),
        "About card should carry the full server version:\n{rendered}"
    );
    // And the header above it shows only the release number.
    let header = rendered.lines().next().unwrap();
    assert!(header.contains("pg 16.15"), "{header}");
    assert!(!header.contains("Debian"), "{header}");
}

/// The footer renders the `:` bar only while the mode IS `CommandBar`.
/// An async `TxClosed` landing mid-bar moves the mode to Editor
/// without taking the bar; until the next key drops it, its text must
/// not hide the editor's own footer hints. (TxDecision has its own
/// pre-empting footer, so the Editor case is the one that shows.)
#[test]
fn stale_command_bar_does_not_hide_the_editor_footer() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    for c in "rea".chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    assert_eq!(a.mode, Mode::CommandBar);
    // Exactly what `on_msg(TxClosed { .. })` does to the mode —
    // without touching the bar (the app-level test in
    // `src/app/tests.rs` drives a real message; `on_msg` is
    // crate-private).
    a.mode = Mode::Editor;
    assert!(
        a.command_bar.is_some(),
        "precondition: the bar is stale, not taken"
    );
    let buf = render(&mut a, 80, 24);
    let footer = row_text(&buf, 23);
    assert!(
        !footer.contains(":rea"),
        "stale command bar drawn over the editor footer: {footer:?}"
    );
    assert!(
        footer.contains("⏎ runs after ;"),
        "editor footer hint missing: {footer:?}"
    );
}

/// The confirm modal sizes itself to its wrapped content. Counting a
/// CJK literal in chars under-budgeted it by a row, and the row that
/// fell off the bottom was the `y = run · n / esc = cancel` line — the
/// only thing the modal is asking for.
#[test]
fn confirm_modal_with_a_wide_literal_keeps_its_y_n_line() {
    use pgman::app::{PendingRun, RunKind};
    use pgman::safety::{Decision, Guard, StatementKind};
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.pending_run = Some(PendingRun {
        sql: format!("UPDATE notes SET body = '{}'", "東".repeat(60)),
        kind: RunKind::Run,
        decision: Decision {
            kind: StatementKind::Update { has_where: false },
            guard: Guard::Confirm,
            wrap_in_tx: true,
            blocked_by_read_only: false,
            read_only_escape: false,
        },
        is_batch: false,
        summary: None,
    });
    a.mode = Mode::Confirm;
    let buf = render(&mut a, 80, 24);
    let text = dump(&buf);
    assert!(
        text.contains("y = run"),
        "the modal's y/n line fell off the bottom:\n{text}"
    );
}

/// The terminal cursor is painted at a display column. `cursor_position`
/// counts chars, so every wide glyph before the cursor left it one
/// cell short — over the closing quote instead of after it.
#[test]
fn editor_cursor_sits_after_a_wide_literal_not_inside_it() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT '東京都'".into();
    a.editor.cursor = a.editor.buffer.len();
    let backend = TestBackend::new(80, 16);
    let mut term = Terminal::new(backend).expect("terminal");
    term.draw(|f| pgman::ui::draw(f, &mut a)).expect("draw");
    let pos = term.get_cursor_position().expect("cursor position");
    // Editor border at x=0, "> " prompt at x=1..2, text from x=3:
    // "SELECT '" (8) + 東京都 (6) + "'" (1) = 15 columns → x = 18.
    assert_eq!(
        (pos.x, pos.y),
        (18, 2),
        "cursor drifted: {:?}",
        row_text(term.backend().buffer(), 2)
    );
}

/// A panel load that fails (`T` on a server without
/// `pg_stat_statements`) puts its error in the footer; the start card
/// underneath stays. It used to turn into an empty `result · (no
/// rows)` block for a query nobody ran.
#[test]
fn a_failed_panel_load_leaves_the_start_card_in_place() {
    let mut a = settle_app();
    assert!(a.grid.columns.is_empty(), "nothing has run yet");
    a.mode = Mode::Normal;
    a.last_error = Some("slow queries load failed: relation \"pg_stat_statements\" does not exist (try `CREATE EXTENSION pg_stat_statements`)".into());
    let buf = render(&mut a, 120, 30);
    let text = dump(&buf);
    assert!(text.contains("write a query"), "start card gone:\n{text}");
    assert!(
        !text.contains("(no rows)"),
        "an empty result block replaced the start card:\n{text}"
    );
    assert!(
        text.contains("slow queries load failed"),
        "footer lost the error:\n{text}"
    );
}

/// The connection-failure card names the log file that exists
/// (`pgman.log.YYYY-MM-DD`, not `pgman.log`), keeps it on one fitted
/// row rather than wrapping the path across the border, and — with no
/// password in the DSN — suggests supplying one.
#[test]
fn connection_failure_card_names_the_dated_log_on_one_fitted_row() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.conn_state = ConnState::Failed(
        "error connecting to server: password authentication failed for user \"test\"".into(),
    );
    for width in [60u16, 100] {
        let buf = render(&mut a, width, 24);
        let text = dump(&buf);
        let logs_row = text
            .lines()
            .find(|l| l.contains("logs"))
            .unwrap_or_else(|| panic!("no logs row at {width}:\n{text}"));
        assert!(
            logs_row.contains("pgman.log."),
            "the dated file name must be on the logs row at {width}: {logs_row:?}"
        );
        assert!(
            !text.contains("pgman.log\n") && !text.contains("pgman.log "),
            "the undated name must not appear at {width}:\n{text}"
        );
        assert!(
            text.lines()
                .any(|l| l.contains("r retry") && l.contains("q quit")),
            "actions row lost at {width}:\n{text}"
        );
    }
    // Narrow: the path is middle-ellipsised on its row, never wrapped
    // onto an unpadded continuation row.
    let text = dump(&render(&mut a, 30, 24));
    let logs_row = text.lines().find(|l| l.contains("logs")).unwrap();
    assert!(logs_row.contains('…'), "{logs_row:?}");
    // The hint knows no password was supplied.
    let text = dump(&render(&mut a, 100, 24));
    assert!(text.contains("PGPASSWORD"), "{text}");
}

/// An editor line wider than the pane ends in `…` at the border, not
/// in a silent cut.
#[test]
fn editor_marks_a_line_cut_at_the_right_border() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = format!("select {} from t", "x".repeat(80));
    a.editor.cursor = 0;
    let buf = render(&mut a, 40, 16);
    let row = row_text(&buf, 2);
    assert!(
        row.ends_with("…│"),
        "expected the cut marker before the border: {row:?}"
    );
    // A line that fits is untouched.
    a.editor.buffer = "select 1".into();
    let buf = render(&mut a, 40, 16);
    let row = row_text(&buf, 2);
    assert!(row.contains("select 1") && !row.contains('…'), "{row:?}");
}

/// `:` then Tab shows the command list beside the bar — the bar owns
/// the footer, so the old status-line listing was never visible.
#[test]
fn command_bar_tab_shows_the_candidates_beside_the_input() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Char(':')));
    a.on_key(KeyEvent::from(KeyCode::Tab));
    let buf = render(&mut a, 120, 16);
    let footer = row_text(&buf, 15);
    assert!(
        footer.contains(":") && footer.contains("about · connect · d · dn · dt"),
        "{footer:?}"
    );
}

/// `L` on a quiet server: the panel body and the footer both say
/// "only this session" rather than `0 total · 0 blocked`, which read
/// as a failed load.
#[test]
fn sessions_panel_on_a_quiet_server_says_only_this_session() {
    let mut a = settle_app();
    a.mode = Mode::Sessions;
    // The state `SessionsLoaded { result: Ok(vec![]) }` leaves behind
    // (the handler itself is covered in the unit tests).
    a.sessions.rows = Vec::new();
    a.last_status = Some(pgman::app::sessions_status(0, 0));
    let text = dump(&render(&mut a, 120, 30));
    assert!(
        text.matches("only this session").count() >= 2,
        "expected the phrase in the panel body and the footer:\n{text}"
    );
    assert!(!text.contains("0 total"), "{text}");
}
