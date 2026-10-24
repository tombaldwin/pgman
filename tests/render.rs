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
use pgman::grid::sort_rows;
use pgman::grid::Grid;
use pgman::query::schema::TableMeta;
use pgman::safety::SafetyConfig;
use pgman::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use ratatui::Terminal;

/// Rows the boxed header takes on a terminal 24 or more rows tall
/// (`ui::header::TALL_HEADER_ROWS`).
const TALL_HEADER: u16 = 5;

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
    // Find the `S` of SELECT in the editor (the tab bar above carries
    // the same text as its label) and check its foreground colour
    // matches `theme.title`.
    let (x, y) = find_cell(&buf, "> SELECT").expect("SELECT should appear");
    let cell = &buf[(x + 2, y)];
    assert_eq!(
        cell.fg, theme.title,
        "expected SELECT in theme.title ({:?}), got {:?}",
        theme.title, cell.fg
    );
}

#[test]
fn editor_title_names_a_pasted_postgres_log_in_user_words() {
    // The title once said `pglog log` — an internal module name.
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer =
        "2026-10-21 09:12:48.001 UTC [31840] billing@ledger LOG:  statement: select 1".into();
    a.editor.cursor = 0;
    let text = dump(&render(&mut a, 120, 20));
    assert!(text.contains("editor · postgres log"), "{text}");
    assert!(!text.contains("pglog"), "{text}");
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
            vec![Some("1".into()), Some("alice".into())],
            vec![Some("2".into()), Some("bob".into())],
        ],
        truncated: false,
    };
    // Initialise the view state the way the run-loop would after a
    // QueryOk lands (visible rows + sort state).
    a.grid_view.visible_rows = (0..a.grid.rows.len()).collect();
    a.grid_view.col_cursor = 0;
    a.grid_view.sort = Some((0, true));
    sort_rows(&mut a.grid.rows, 0, true);
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
        rendered.contains("PEAK") && rendered.contains("CONNS"),
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
        vec![Some("1".to_string()), Some("alice".to_string())],
        vec![Some("2".to_string()), Some("bob".to_string())],
    ];
    let b_rows = vec![
        vec![Some("1".to_string()), Some("ALICE".to_string())],
        vec![Some("3".to_string()), Some("carol".to_string())],
    ];
    let key = pgman::query::row_diff::RowKey::Columns(vec![0]);
    let diff = pgman::query::row_diff::diff_rows(&a_rows, &b_rows, &key);
    let pinned = pgman::app::PinnedResult {
        columns: cols.clone(),
        rows: a_rows,
        label: "A-query".into(),
        source: None,
        origins: Vec::new(),
    };
    a.result_diff.active = Some(pgman::app::ResultDiffState {
        a: pinned.clone(),
        b_columns: cols,
        b_rows,
        b_label: "B-query".into(),
        b_source: None,
        b_origins: Vec::new(),
        key,
        key_is_pk: false,
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

/// A NULL that became an empty string is a change, and the diff says
/// which side was which: `NULL → ''`, not `col:  → `.
#[test]
fn result_diff_shows_a_null_to_empty_string_change() {
    let mut a = settle_app();
    let cols = vec!["id".to_string(), "note".to_string()];
    let a_rows = vec![vec![Some("1".to_string()), None]];
    let b_rows = vec![vec![Some("1".to_string()), Some(String::new())]];
    let key = pgman::query::row_diff::RowKey::Columns(vec![0]);
    let diff = pgman::query::row_diff::diff_rows(&a_rows, &b_rows, &key);
    assert_eq!(diff.changed.len(), 1, "NULL -> '' is a change");
    let pinned = pgman::app::PinnedResult {
        columns: cols.clone(),
        rows: a_rows,
        label: "A-query".into(),
        source: None,
        origins: Vec::new(),
    };
    a.result_diff.active = Some(pgman::app::ResultDiffState {
        a: pinned.clone(),
        b_columns: cols,
        b_rows,
        b_label: "B-query".into(),
        b_source: None,
        b_origins: Vec::new(),
        key,
        key_is_pk: false,
        diff,
    });
    a.result_diff.pinned = Some(pinned);
    a.mode = Mode::ResultDiff;
    let rendered = dump(&render(&mut a, 140, 30));
    assert!(rendered.contains("note: NULL → ''"), "\n{rendered}");
}

/// A diff keyed on the source table's primary key says so in its title.
#[test]
fn result_diff_title_names_a_primary_key() {
    let mut a = settle_app();
    let cols = vec!["name".to_string(), "id".to_string()];
    let rows = vec![vec![Some("ann".to_string()), Some("1".to_string())]];
    let key = pgman::query::row_diff::RowKey::Columns(vec![1]);
    let diff = pgman::query::row_diff::diff_rows(&rows, &rows, &key);
    let pinned = pgman::app::PinnedResult {
        columns: cols.clone(),
        rows: rows.clone(),
        label: "A-query".into(),
        source: None,
        origins: Vec::new(),
    };
    a.result_diff.active = Some(pgman::app::ResultDiffState {
        a: pinned.clone(),
        b_columns: cols,
        b_rows: rows,
        b_label: "B-query".into(),
        b_source: None,
        b_origins: Vec::new(),
        key,
        key_is_pk: true,
        diff,
    });
    a.mode = Mode::ResultDiff;
    let rendered = dump(&render(&mut a, 160, 30));
    assert!(rendered.contains("key: id (primary key)"), "\n{rendered}");
}

#[test]
fn result_diff_title_keeps_the_key_at_80_columns() {
    // A and B are labelled by their query text, which an 80-column
    // overlay cannot hold: the key comes first so it survives, and the
    // cut ends on `…` rather than at the border mid-word.
    let mut a = settle_app();
    let cols = vec!["name".to_string(), "id".to_string()];
    let rows = vec![vec![Some("ann".to_string()), Some("1".to_string())]];
    let key = pgman::query::row_diff::RowKey::Columns(vec![1]);
    let diff = pgman::query::row_diff::diff_rows(&rows, &rows, &key);
    let pinned = pgman::app::PinnedResult {
        columns: cols.clone(),
        rows: rows.clone(),
        label: "SELECT name, id FROM ledger.accounts ORDER BY id".into(),
        source: None,
        origins: Vec::new(),
    };
    a.result_diff.active = Some(pgman::app::ResultDiffState {
        a: pinned,
        b_columns: cols,
        b_rows: rows,
        b_label: "SELECT name, id FROM ledger.accounts WHERE active".into(),
        b_source: None,
        b_origins: Vec::new(),
        key,
        key_is_pk: true,
        diff,
    });
    a.mode = Mode::ResultDiff;
    let rendered = dump(&render(&mut a, 80, 24));
    let title = rendered
        .lines()
        .find(|l| l.contains("Result diff"))
        .unwrap_or_else(|| panic!("no title row:\n{rendered}"));
    assert!(title.contains("key: id (primary key)"), "{title}");
    assert!(title.contains(" · … "), "the cut is marked: {title}");
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
    // Normal: the open-invoices result grid.
    let rendered = dump(&render(&mut a, 140, 30));
    assert!(
        rendered.contains("INV-004822"),
        "grid data missing:\n{rendered}"
    );
    // Schema browser opens against the fixture cache (two schemas,
    // collapsed).
    a.mode = Mode::SchemaBrowser;
    let rendered = dump(&render(&mut a, 140, 30));
    assert!(
        rendered.contains("public")
            && rendered.contains("reporting")
            && rendered.contains("20 table(s)"),
        "schema browser missing:\n{rendered}"
    );
    // Tap monitor shows the synthetic events.
    a.mode = Mode::TapMonitor;
    let rendered = dump(&render(&mut a, 140, 30));
    assert!(
        rendered.contains("invoice_lines"),
        "tap events missing:\n{rendered}"
    );
}

/// An expanded table lists its indexes under its constraints; the
/// detail pane for an index shows its kind and its full definition,
/// and the table's own pane lists them with theirs.
#[test]
fn schema_browser_shows_a_tables_indexes() {
    use pgman::app::SchemaBrowserRow;
    let mut a = pgman::demo::app(Theme::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.mode = Mode::SchemaBrowser;
    a.schema_browser.expanded.insert("public".into());
    a.schema_browser.expanded.insert("public.payments".into());
    let rows = a.flattened_schema_browser();
    a.schema_browser.cursor = rows
        .iter()
        .position(|r| matches!(r, SchemaBrowserRow::Table { name, .. } if name == "payments"))
        .unwrap();
    let rendered = dump(&render(&mut a, 140, 60));
    assert!(rendered.contains("⌁ payments_invoice_id_idx"), "{rendered}");
    // A primary key's or unique constraint's index lists once, as the
    // constraint.
    assert!(rendered.contains("◆ payments_pkey"), "{rendered}");
    assert!(!rendered.contains("⌁ payments_pkey"), "{rendered}");
    assert!(
        rendered.contains("◆ payments_provider_ref_key"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("⌁ payments_provider_ref_key"),
        "{rendered}"
    );
    assert!(rendered.contains("indexes (3)"), "{rendered}");
    assert!(
        rendered.contains("· payments_pkey  primary key"),
        "{rendered}"
    );
    a.schema_browser.cursor = rows
        .iter()
        .position(
            |r| matches!(r, SchemaBrowserRow::Index { name, .. } if name == "payments_invoice_id_idx"),
        )
        .unwrap();
    let rendered = dump(&render(&mut a, 180, 60));
    assert!(
        rendered.contains(
            "CREATE INDEX payments_invoice_id_idx ON public.payments USING btree (invoice_id)"
        ),
        "{rendered}"
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
        rows: vec![vec![Some("1".into())], vec![Some("2".into())]],
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
    let rendered = dump(&render(&mut a, 80, 24));
    assert!(
        rendered.contains("capped at 1000 · more rows not fetched"),
        "{rendered}"
    );
}

#[test]
fn grid_render_shows_filtered_count_in_title() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["name".into()],
        rows: vec![
            vec![Some("alice".into())],
            vec![Some("bob".into())],
            vec![Some("carol".into())],
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
        rendered.contains("2/3 row(s) (filtered) · / esc clears"),
        "expected `2/3 row(s) (filtered) · / esc clears` in title; full render:\n{rendered}"
    );
}

/// With a filter on, a partial result and five-digit counts, the
/// result title at 80 columns is fitted to its border: whole segments
/// dropped from the end with a `…`, never a word run into the corner.
#[test]
fn grid_title_with_a_filter_and_five_digit_counts_fits_80_columns() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    let names = ["alice", "bob", "carol", "dave"];
    a.grid = Grid {
        columns: vec!["name".into(), "id".into()],
        rows: (0..12_000)
            .map(|i| vec![Some(names[i % 4].to_string()), Some(i.to_string())])
            .collect(),
        truncated: true,
    };
    a.grid_view.filter = Some("a".into());
    a.grid_view.visible_rows = compute_visible_rows(&a.grid.rows, Some("a"));
    a.grid_state.select(Some(0));
    let buf = render(&mut a, 80, 24);
    let rendered = dump(&buf);
    let title = rendered
        .lines()
        .find(|l| l.contains("result ·"))
        .unwrap_or_else(|| panic!("no result title:\n{rendered}"));
    assert!(
        title.contains("result · 9000/12000 row(s) (filtered) · / esc clears"),
        "{title}"
    );
    // The title ends on a whole segment and `…`, then the border.
    let border_from = title.find(" ◆ ]").expect("decorated title");
    assert!(
        title[..border_from].ends_with(" · …"),
        "the cut is marked: {title}"
    );
}

#[test]
fn help_overlay_lists_key_bindings() {
    let mut a = settle_app();
    // `:help all` — every section on one page. Tall viewport so the
    // whole help text fits without scrolling — the assertions below
    // cover bindings from multiple sections.
    a.open_help_all(Mode::Normal);
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
    // the row below it, inside the panel's borders and centred across
    // it.
    let lines: Vec<&str> = rendered.lines().collect();
    let panel_top = lines
        .iter()
        .position(|l| l.starts_with("╭[ ◆ pgman"))
        .expect("results panel border");
    assert!(
        !lines[panel_top].contains("pick a connection"),
        "picker landed on the panel's top border row:\n{rendered}"
    );
    let row: Vec<char> = lines[panel_top + 1].chars().collect();
    assert_eq!(row.first(), Some(&'│'), "the panel's border:\n{rendered}");
    assert_eq!(row.last(), Some(&'│'), "the panel's border:\n{rendered}");
    let left = row.iter().position(|&c| c == '╭').expect("popup corner");
    let right = row.iter().rposition(|&c| c == '╮').expect("popup corner");
    assert!(
        lines[panel_top + 1].contains("╭[ ◆ pick a connection"),
        "picker should float one row inside the panel:\n{rendered}"
    );
    let (gap_left, gap_right) = (left - 1, row.len() - 2 - right);
    assert!(
        gap_left.abs_diff(gap_right) <= 1,
        "picker should be centred ({gap_left} left, {gap_right} right):\n{rendered}"
    );

    // Every popup row keeps a blank column before its right border, so
    // the longest target doesn't read as jammed against the frame.
    let right_border = lines[panel_top + 1]
        .chars()
        .position(|c| c == '╮')
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
        .find(|l| l.contains("USER/APP"))
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
        col_of(header, "USER/APP"),
        col_of(row, "alice/psql"),
        "user/app column is not left-aligned:\n{rendered}"
    );
    assert_eq!(
        col_of(header, "STATE"),
        col_of(row, "idle in tx"),
        "state column is not left-aligned:\n{rendered}"
    );
    // Numbers: value ENDS in the same column as its label.
    assert_eq!(
        col_of(header, "PID") + "pid".chars().count(),
        col_of(row, "1234") + "1234".chars().count(),
        "pid column is not right-aligned:\n{rendered}"
    );
    assert_eq!(
        col_of(header, "AGE(S)") + "age(s)".chars().count(),
        col_of(row, "12.5") + "12.5".chars().count(),
        "age(s) column is not right-aligned:\n{rendered}"
    );
}

/// The help overlay clamps the scroll it STORES, not only the one it
/// draws. An unclamped stored scroll past `max_scroll` — once from
/// the anchor pass, now possible whenever the terminal grows under a
/// scrolled section — left the first few `k` presses walking the value
/// back down through a range that all renders identically, so the
/// overlay looked frozen.
#[test]
fn help_scroll_is_clamped_when_stored_not_only_when_rendered() {
    let mut a = settle_app();
    a.open_help_from(Mode::Editor);
    let _ = render(&mut a, 80, 24);
    assert!(a.help.max_scroll > 0, "the editor section scrolls at 80x24");
    a.on_key(KeyEvent::from(KeyCode::Char('G')));
    assert_eq!(a.help.scroll, a.help.max_scroll);
    // A bigger terminal: the same section needs less scroll.
    let _ = render(&mut a, 200, 50);
    assert!(
        a.help.scroll <= a.help.max_scroll,
        "stored an unclamped scroll: {} > max {}",
        a.help.scroll,
        a.help.max_scroll
    );
    let before = a.help.scroll;
    a.on_key(KeyEvent::from(KeyCode::Char('k')));
    if before > 0 {
        assert_eq!(a.help.scroll, before - 1, "one k is one row");
    }
}

/// Opening help from a mode scrolls to that mode's section — and the
/// section's HEADING must be on screen. The `↑ N more above` indicator
/// overlays the first visible row once the overlay is scrolled, and the
/// anchor pass used to scroll the heading itself onto that row: the
/// section opened with its body first and its name hidden.
#[test]
fn help_opened_from_a_mode_shows_that_sections_heading() {
    for (mode, heading) in [
        (Mode::Editor, "editor"),
        (Mode::SchemaBrowser, "schema browser"),
        (Mode::SavedQueries, "saved queries"),
        (Mode::TapMonitor, "jdbc tap"),
    ] {
        let mut a = settle_app();
        a.open_help_from(mode);
        let buf = render(&mut a, 80, 24);
        let rendered = dump(&buf);
        assert!(
            rendered
                .lines()
                .any(|l| l.trim_matches(|c: char| c == '│' || c.is_whitespace()) == heading),
            "{mode:?}: the {heading:?} heading must be visible:\n{rendered}"
        );
    }
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
            error: None,
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

/// A pick that failed in the log wears a `✗`, and the focused one's
/// error is on the popup's bottom border — gone when the cursor moves
/// to one that ran.
#[test]
fn log_picker_marks_a_failed_statement_and_shows_its_error() {
    let mut a = settle_app();
    a.mode = pgman::app::Mode::LogPick;
    a.log_pick.picks = pgman::query::pglog::parse(
        "[101] LOG:  statement: select 1\n\
         [101] ERROR:  relation \"invoice_lnes\" does not exist at character 15\n\
         [101] STATEMENT:  select * from invoice_lnes",
    );
    assert_eq!(a.log_pick.picks.len(), 2, "fixture check");
    a.log_pick.index = 1;
    let rendered = dump(&render(&mut a, 120, 40));
    assert!(
        rendered.contains("▶ [postgres]  ✗ select * from invoice_lnes"),
        "the failed row is not marked:\n{rendered}"
    );
    assert!(
        rendered.contains("[postgres]  select 1"),
        "the row that ran is marked:\n{rendered}"
    );
    assert!(
        rendered.contains(
            "failed in the log: relation \"invoice_lnes\" does not exist at character 15"
        ),
        "the focused pick's error is not shown:\n{rendered}"
    );

    a.log_pick.index = 0;
    let rendered = dump(&render(&mut a, 120, 40));
    assert!(
        !rendered.contains("failed in the log"),
        "the error outlived the focus:\n{rendered}"
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
            error: None,
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
    let header = rendered.lines().take(5).collect::<Vec<_>>().join("\n");
    assert!(header.contains("pg 16.15"), "{header}");
    assert!(!header.contains("Debian"), "{header}");
}

/// Redaction leaves pgman's own credit line alone even when a word in
/// it is a redacted name — here the login name — while the same word
/// elsewhere on screen is still blocked out.
#[test]
fn redacted_about_card_keeps_its_credit_line() {
    let mut a = settle_app();
    a.redact_env_names = vec!["tom".into()];
    a.last_error = Some("permission denied for tom".into());
    a.mode = Mode::About;
    a.redact = true;
    let rendered = dump(&render(&mut a, 120, 40));
    assert!(
        rendered.contains("by Tom Baldwin / Polymorphism Ltd"),
        "credit line redacted:\n{rendered}"
    );
    assert!(
        !rendered.contains("denied for tom") && rendered.contains("denied for ▓▓▓"),
        "the name elsewhere must still go:\n{rendered}"
    );
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
            server_control: false,
        },
        is_batch: false,
        summary: None,
        cell_edit: None,
    });
    a.mode = Mode::Confirm;
    let buf = render(&mut a, 80, 24);
    let text = dump(&buf);
    assert!(
        text.contains("n / esc = cancel"),
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
    // Header, tab bar and the editor's top border above: y = 3.
    assert_eq!(
        (pos.x, pos.y),
        (18, 3),
        "cursor drifted: {:?}",
        row_text(term.backend().buffer(), 3)
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
/// (`pgman.log.YYYY-MM-DD`, not `pgman.log`), wraps a long path whole
/// under its label rather than eliding it (it is there to be copied)
/// or wrapping it flush against the border, and — with no password in
/// the DSN — suggests supplying one.
#[test]
fn connection_failure_card_names_the_dated_log_and_wraps_it_whole() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.conn_state = ConnState::Failed(
        "error connecting to server: password authentication failed for user \"test\"".into(),
    );
    for width in [60u16, 100] {
        let buf = render(&mut a, width, 24);
        let text = dump(&buf);
        // The logs row and its continuation rows (padded under the
        // label): the file name may wrap onto the next one.
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("logs"))
            .unwrap_or_else(|| panic!("no logs row at {width}:\n{text}"));
        let logs_rows: String = lines[at..]
            .iter()
            .enumerate()
            .take_while(|(i, l)| *i == 0 || l.chars().skip(1).take(10).all(|c| c == ' '))
            .map(|(_, l)| l.trim_matches(|c| c == '│' || c == ' '))
            .collect();
        assert!(
            logs_rows.contains("pgman.log."),
            "the dated file name must be under the logs label at {width}: {logs_rows:?}\n{text}"
        );
        assert!(
            !text.contains("pgman.log\n") && !text.contains("pgman.log "),
            "the undated name must not appear at {width}:\n{text}"
        );
        assert!(
            text.lines()
                .any(|l| l.starts_with('│') && l.contains("r retry") && l.contains("q quit")),
            "the card's actions row was cut off at {width}x24:\n{text}"
        );
    }
    // Narrow: the path is wrapped whole — no `…` — onto continuation
    // rows padded under the label, so all of it can be copied. Rendered
    // tall enough that the card keeps its logs rows under the five-line
    // editor: at 30 columns every other row wraps too.
    let text = dump(&render(&mut a, 30, 40));
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|l| l.contains("logs")).unwrap();
    assert!(!lines[at].contains('…'), "{:?}", lines[at]);
    let mut path = String::new();
    for (i, l) in lines[at..].iter().enumerate() {
        let body: String = l.chars().skip(1).collect(); // the left border
        if i > 0 && !body.starts_with("          ") {
            break;
        }
        let value: String = body.chars().skip(10).collect();
        path.push_str(value.trim_end_matches('│').trim_end());
    }
    let dir = pgman::util::cache_dir().display().to_string();
    assert!(
        path.starts_with(&dir) && path.contains("pgman.log."),
        "the wrapped rows should rebuild the whole path under {dir}: {path:?}\n{text}"
    );
    // At 30 columns the error and the hint wrap, and every row keeps
    // its indent rather than starting flush against the border. The
    // hint knows no password was supplied.
    for heading in ["  error", "  hint"] {
        let at = lines.iter().position(|l| l.contains(heading)).unwrap();
        let rows: Vec<String> = lines[at + 1..]
            .iter()
            .map(|l| l.chars().skip(1).collect::<String>())
            .take_while(|body| !body.trim_end_matches('│').trim().is_empty())
            .collect();
        assert!(rows.len() > 1, "{heading} should wrap at 30:\n{text}");
        for body in &rows {
            assert!(
                body.starts_with("    "),
                "{heading} row unindented: {body:?}\n{text}"
            );
            assert!(
                body.trim_end_matches('│').ends_with("  "),
                "{heading} row runs into the border: {body:?}\n{text}"
            );
        }
    }
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
    // Row 3: under the header, the tab bar and the editor's top border.
    let row = row_text(&buf, 3);
    assert!(
        row.ends_with("…│"),
        "expected the cut marker before the border: {row:?}"
    );
    // A line that fits is untouched.
    a.editor.buffer = "select 1".into();
    let buf = render(&mut a, 40, 16);
    let row = row_text(&buf, 3);
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
        footer.contains(":") && footer.contains("about · connect · d · df · dn · dt"),
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

/// `Alt-Z` twice puts the screen back exactly as it was — the manual
/// size set with `Alt-=` included — and nothing but the footer's
/// status text differs between the two frames.
#[test]
fn alt_z_twice_restores_the_exact_prior_split() {
    let mut a = App::new(
        Theme::default(),
        Some(Dsn::parse("postgres://test@localhost/test").unwrap()),
        Vec::new(),
        SafetyConfig::default(),
    );
    a.splash_visible = false;
    a.splash_until = None;
    a.conn_state = ConnState::Connected {
        server_version: "16.0".into(),
    };
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT 1".into();
    a.editor.cursor = a.editor.buffer.len();
    a.grid = Grid {
        columns: vec!["a".into()],
        rows: vec![vec![Some("1".into())]],
        truncated: false,
    };
    a.grid_view.visible_rows = compute_visible_rows(&a.grid.rows, None);
    // One frame so the body height is known, then three lines taller
    // than the automatic five.
    render(&mut a, 80, 24);
    for _ in 0..3 {
        a.on_key(KeyEvent::new(KeyCode::Char('='), KeyModifiers::ALT));
    }
    let before = render(&mut a, 80, 24);
    let body_before: Vec<String> = (0..23).map(|y| row_text(&before, y)).collect();
    assert!(
        row_text(&before, 11).starts_with('╰'),
        "8 lines + 2 borders under the header and tab bar: bottom border on row 11:\n{}",
        dump(&before)
    );
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
    let zoomed = render(&mut a, 80, 24);
    assert!(
        row_text(&zoomed, 22).starts_with('╰'),
        "zoomed: the editor's bottom border is on the footer:\n{}",
        dump(&zoomed)
    );
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
    let after = render(&mut a, 80, 24);
    let body_after: Vec<String> = (0..23).map(|y| row_text(&after, y)).collect();
    assert_eq!(body_after, body_before, "the split came back different");
}

/// With the editor zoomed there is no result panel for the completion
/// popup to float in; it floats over the editor's lower half instead
/// of being placed on the footer row (or off the screen).
#[test]
fn completion_popup_floats_inside_a_zoomed_editor() {
    use pgman::app::CompletionCycle;
    use pgman::query::complete::{Candidate, CandidateKind};
    let mut a = App::new(
        Theme::default(),
        Some(Dsn::parse("postgres://test@localhost/test").unwrap()),
        Vec::new(),
        SafetyConfig::default(),
    );
    a.splash_visible = false;
    a.splash_until = None;
    a.conn_state = ConnState::Connected {
        server_version: "16.0".into(),
    };
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT * FROM us".into();
    a.editor.cursor = a.editor.buffer.len();
    a.completion = Some(CompletionCycle {
        start: 14,
        end: 16,
        origin: "us".into(),
        origin_prefix: "us".into(),
        origin_cursor: 16,
        candidates: vec![
            Candidate {
                display: "users".into(),
                insert: "users".into(),
                kind: CandidateKind::Table,
                context: Some("public".into()),
            },
            Candidate {
                display: "user_roles".into(),
                insert: "user_roles".into(),
                kind: CandidateKind::Table,
                context: Some("public".into()),
            },
        ],
        selected: Some(0),
        auto: false,
    });
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
    let buf = render(&mut a, 80, 24);
    let text = dump(&buf);
    let popup_rows: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("users") && l.contains("table"))
        .map(|(y, _)| y)
        .collect();
    assert_eq!(
        popup_rows.len(),
        1,
        "one candidate row for `users`:\n{text}"
    );
    let y = popup_rows[0];
    assert!(
        (12..22).contains(&y),
        "the popup floats in the editor's lower half (row {y}):\n{text}"
    );
    assert!(
        text.lines().nth(y).unwrap().starts_with('│'),
        "the editor's own border survives beside the popup:\n{text}"
    );
}

// ---- tab bar -------------------------------------------------------------

fn connected_app() -> App {
    let mut a = App::new(
        Theme::default(),
        Some(Dsn::parse("postgres://test@localhost/test").unwrap()),
        Vec::new(),
        SafetyConfig::default(),
    );
    a.splash_visible = false;
    a.splash_until = None;
    a.conn_state = ConnState::Connected {
        server_version: "16.0".into(),
    };
    a.mode = Mode::Normal;
    a
}

/// One tab, just connected: the bar is there, with the tab labelled
/// `empty`, and the keys that manage tabs on its right edge.
#[test]
fn tab_bar_is_visible_with_a_single_tab_once_connected() {
    let mut a = connected_app();
    let buf = render(&mut a, 80, 24);
    let bar = row_text(&buf, TALL_HEADER);
    assert!(
        bar.starts_with(" 1 empty "),
        "row 1 is the tab bar: {bar:?}"
    );
    assert!(
        bar.ends_with("[ ] prev/next tab · ctrl-t new · ctrl-w close"),
        "the tab keys are right-aligned: {bar:?}"
    );
    // Columns, not bytes: the hint's `·` separators are multi-byte.
    assert!(
        bar.chars().count() <= 80,
        "never wider than the terminal: {bar:?}"
    );
    assert!(
        row_text(&buf, TALL_HEADER + 1).starts_with('╭'),
        "the editor starts under the bar:\n{}",
        dump(&buf)
    );
}

/// Before a connection there is nothing to tab between: no bar, and
/// the body starts right under the header as it always did.
#[test]
fn tab_bar_is_hidden_before_a_connection() {
    let mut a = connected_app();
    a.conn_state = ConnState::Failed("refused".into());
    let buf = render(&mut a, 80, 24);
    assert!(
        row_text(&buf, TALL_HEADER).starts_with('╭'),
        "no tab bar on the failure card:\n{}",
        dump(&buf)
    );
    let text = dump(&buf);
    assert!(!text.contains("ctrl-w close"), "{text}");
}

/// The active tab's label follows the buffer as it is typed, the
/// other tabs keep their stashed labels, and only the active tab is
/// painted in the accent colour.
#[test]
fn tab_bar_labels_update_live_and_the_active_tab_is_accented() {
    let mut a = connected_app();
    a.editor.buffer = "SELECT 1".into();
    a.new_tab();
    a.mode = Mode::Editor;
    for c in "select count(*) from orders".chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let buf = render(&mut a, 80, 24);
    let bar = row_text(&buf, TALL_HEADER);
    assert!(
        bar.contains(" 1 SELECT 1 ") && bar.contains(" 2 select count(*) f… "),
        "both tabs labelled, the long one cut at 18 columns: {bar:?}"
    );
    let theme = Theme::default();
    let (x1, y) = find_cell(&buf, "1 SELECT 1").expect("tab 1 on the bar");
    let (x2, _) = find_cell(&buf, "2 select").expect("tab 2 on the bar");
    assert_eq!(buf[(x1, y)].fg, theme.muted, "tab 1 is not active: muted");
    assert_eq!(buf[(x2, y)].fg, theme.accent, "tab 2 is active: accent");
    // Back to tab 1 (alt-p works from the editor; alt-1 does not):
    // the accent moves with it.
    a.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
    assert_eq!(a.active_tab, 0);
    let buf = render(&mut a, 80, 24);
    assert_eq!(buf[(x1, y)].fg, theme.accent);
    assert_eq!(buf[(x2, y)].fg, theme.muted);
}

/// A cramped terminal (body under 12 rows) gives the bar's row back
/// to the panes while there is one tab; a second tab brings it back,
/// since the operator then needs to see which tab they are on.
#[test]
fn tab_bar_yields_its_row_on_a_cramped_terminal_unless_there_are_two_tabs() {
    let mut a = connected_app();
    let buf = render(&mut a, 60, 13);
    assert!(
        row_text(&buf, 1).starts_with('╭'),
        "60x13 is an 11-row body: no bar:\n{}",
        dump(&buf)
    );
    let buf = render(&mut a, 60, 14);
    assert!(
        row_text(&buf, 1).starts_with(" 1 empty"),
        "60x14 is a 12-row body: the bar is back:\n{}",
        dump(&buf)
    );
    a.new_tab();
    let buf = render(&mut a, 60, 13);
    assert!(
        row_text(&buf, 1).starts_with(" 1 empty"),
        "two tabs: the bar stays even when cramped:\n{}",
        dump(&buf)
    );
}

/// When the tabs need the room, the labels shrink so every tab stays
/// on the bar, and the right-hand hint goes as a whole rather than
/// being cut mid-word.
#[test]
fn tab_bar_shrinks_labels_and_drops_the_hint_whole_when_the_tabs_need_the_room() {
    let mut a = connected_app();
    for i in 0..4 {
        a.editor.buffer = format!("select {i} from a_long_table_name");
        a.new_tab();
    }
    let buf = render(&mut a, 60, 16);
    let bar = row_text(&buf, 1);
    assert!(!bar.contains("ctrl-"), "hint dropped whole: {bar:?}");
    assert!(
        bar.contains(" 1 selec… "),
        "labels cut to six columns: {bar:?}"
    );
    assert!(
        bar.contains(" 5 empty"),
        "the last tab is still on the bar: {bar:?}"
    );
    assert!(bar.len() <= 60, "{bar:?}");
}

/// Result headers are set apart by style, never re-cased: they are the
/// query's own column names.
#[test]
fn result_headers_are_bold_and_underlined_and_keep_their_case() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["userId".into(), "email".into()],
        rows: vec![vec![Some("1".into()), Some("a@b".into())]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_view.col_cursor = 0;
    let buf = render(&mut a, 60, 18);
    let (x, y) = find_cell(&buf, "email").expect("header cell keeps its case");
    assert!(find_cell(&buf, "EMAIL").is_none() && find_cell(&buf, "userId").is_some());
    let m = buf[(x, y)].modifier;
    assert!(
        m.contains(Modifier::BOLD) && m.contains(Modifier::UNDERLINED),
        "{m:?}"
    );
    let (x, y) = find_cell(&buf, "a@b").unwrap();
    assert!(
        !buf[(x, y)].modifier.contains(Modifier::UNDERLINED),
        "rows aren't"
    );
}

/// A header squeezed with its column ends on `…` as the column's cells
/// do: at 80x24 the demo's `billing_email` header read `billing_` over
/// cells reading `billing…`.
#[test]
fn a_squeezed_result_header_ends_on_an_ellipsis_like_its_cells() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    let long = "a-rather-long-value-that-will-not-fit@example.com";
    a.grid = Grid {
        columns: vec![
            "id".into(),
            "name".into(),
            "billing_email".into(),
            "currency".into(),
        ],
        rows: vec![vec![
            Some("1".into()),
            Some(long.into()),
            Some(long.into()),
            Some(long.into()),
        ]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_view.col_cursor = 0;
    let buf = render(&mut a, 40, 18);
    let (_, y) = find_cell(&buf, "id").expect("header row");
    let header = row_text(&buf, y);
    let (_, rest) = header
        .split_once("billing")
        .expect("billing_email header shown");
    let shown: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
    // Squeezed (the test needs it to be), and marked.
    assert!(
        shown.ends_with('…'),
        "header cut with no marker: {header:?}"
    );
}

/// An error too long for the footer row wraps onto up to two more,
/// taken from the body, with the detail pointer ending the last; on a
/// cramped terminal, and under the error overlay that already shows
/// it, the footer stays one row.
#[test]
fn a_long_error_wraps_the_footer_but_not_on_a_cramped_terminal() {
    use pgman::conn::QueryErrDetail;
    let msg = "column \"created_at\" of relation \"order_items\" does not exist \
               · hint: perhaps you meant to reference the column \"orders.created_at\"";
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.last_error = Some(msg.into());
    a.last_error_detail = Some(QueryErrDetail {
        code: Some("42703".into()),
        severity: Some("ERROR".into()),
        detail: None,
        hint: None,
        r#where: None,
        schema: None,
        table: None,
        column: None,
        data_type: None,
        constraint: None,
    });
    let buf = render(&mut a, 60, 24);
    let rows: Vec<String> = (21..24).map(|y| row_text(&buf, y)).collect();
    assert!(rows[0].contains("⚠ column \"created_at\""), "{rows:?}");
    assert!(rows[2].ends_with("! detail"), "{rows:?}");
    // Every word of the message is on screen: nothing was elided.
    let joined = rows.join(" ");
    for word in msg.split_whitespace() {
        assert!(joined.contains(word), "{word:?} missing: {rows:?}");
    }
    // The continuation rows sit under the message, not the badges.
    let byte = rows[0].find("column").unwrap();
    let text_col = rows[0][..byte].chars().count(); // `⚠` is one column
    assert!(
        rows[1].starts_with(&" ".repeat(text_col)),
        "continuation not indented: {rows:?}"
    );
    // The body gave the rows up: the result block's bottom border now
    // sits on row 20.
    assert!(row_text(&buf, 20).starts_with('╰'), "{}", dump(&buf));

    // Cramped (a body under 12 rows): one row, cut, the pointer kept.
    let buf = render(&mut a, 80, 14);
    assert!(row_text(&buf, 12).starts_with('╰'), "{}", dump(&buf));
    let footer = row_text(&buf, 13);
    assert!(
        footer.contains('…') && footer.ends_with("! detail"),
        "{footer:?}"
    );

    // Under the overlay: one row too.
    a.mode = Mode::ErrorDetail;
    let buf = render(&mut a, 80, 24);
    let footer = row_text(&buf, 23);
    assert!(
        footer.contains('⚠') && footer.ends_with("! detail"),
        "the error starts on the last row: {}",
        dump(&buf)
    );
}

/// `/` with no grid and `Q` with nothing saved open nothing; the footer
/// says why rather than leaving the key looking dead.
#[test]
fn slash_and_q_on_the_start_card_say_why_nothing_opened() {
    for (key, want) in [('/', "nothing to filter"), ('Q', "no saved queries")] {
        let mut a = settle_app();
        a.mode = Mode::Normal;
        a.on_key(KeyEvent::from(KeyCode::Char(key)));
        let text = dump(&render(&mut a, 100, 30));
        assert!(
            text.contains(want),
            "{key}: {want:?} not on screen:\n{text}"
        );
    }
}

/// Redaction mode: on a real-looking RDS environment, the host, account,
/// cluster id, user and environment name are blocked out of the whole
/// frame, the generic parts (`prod`, the region, `rds.amazonaws.com`)
/// and the schema are not, and the footer says it is on. Off, it all
/// shows — so the test can't pass on a frame that never had them.
#[test]
fn redact_blocks_out_the_infrastructure_and_leaves_the_rest() {
    use pgman::inventory::{EnvReport, Target};
    use pgman::mcp::connections::Role;
    let host = "examplecorp-prod.c9abc1.eu-west-2.rds.amazonaws.com";
    let mut a = settle_app();
    a.dsn = Some(Dsn::parse(&format!("postgres://ledger_app@{host}:5432/ledger")).unwrap());
    a.environments.reports = vec![EnvReport {
        name: "examplecorp-prod".into(),
        source: "RDS 210987654321 eu-west-2".into(),
        targets: vec![Target {
            database: "ledger".into(),
            server: "examplecorp-prod".into(),
            role: Role::Primary,
            replica_of: None,
            host: Some(host.into()),
            port: Some(5432),
            engine: Some("postgres 16.4".into()),
            status: Some("available".into()),
            class: Some("db.t4g.medium".into()),
            multi_az: Some(false),
            connect: Err("no secret".into()),
        }],
        error: None,
        profile: None,
        probe: None,
    }];
    a.environments.probes = vec![vec![pgman::app::ProbeState::Off]];
    a.mode = Mode::Environments;
    a.last_error = Some(
        "arn:aws:rds:eu-west-2:210987654321:db:examplecorp-prod refused from 10.20.30.40".into(),
    );
    let leaks = [
        "examplecorp",
        "210987654321",
        "c9abc1",
        "ledger",
        "10.20.30.40",
    ];

    let off = dump(&render(&mut a, 140, 30));
    for leak in leaks {
        assert!(off.contains(leak), "{leak} never on screen:\n{off}");
    }
    assert!(!off.contains("REDACT"));

    a.redact = true;
    let on = dump(&render(&mut a, 140, 30));
    for leak in leaks {
        assert!(!on.contains(leak), "{leak} leaked:\n{on}");
    }
    for kept in [
        "REDACT",
        "-prod",
        "eu-west-2",
        "rds.amazonaws.com",
        "arn:aws:rds",
    ] {
        assert!(on.contains(kept), "{kept} missing:\n{on}");
    }
}

/// A database named after the product: redaction blocks out its own
/// part (`acme`) but never pgman's chrome — the title kept reading
/// `▓▓▓▓▓ 0.1.0` because `pgman_test`'s `pgman` matched it.
#[test]
fn redaction_never_blocks_out_the_product_name_in_the_chrome() {
    let title = format!("pgman {}", env!("CARGO_PKG_VERSION"));
    for connected in [true, false] {
        let mut a = settle_app();
        a.dsn = Some(Dsn::parse("postgres://app@db.example.internal:5432/pgman_acme").unwrap());
        if !connected {
            a.conn_state = ConnState::Disconnected;
        }
        a.redact = true;
        for (w, h) in [(80, 24), (120, 40)] {
            let screen = dump(&render(&mut a, w, h));
            let at = format!("{w}x{h} connected={connected}");
            assert!(screen.contains(&title), "{at} title redacted:\n{screen}");
            assert!(!screen.contains("acme"), "{at} acme leaked:\n{screen}");
            if connected {
                // The name is on screen, its own part blocked out.
                assert!(screen.contains("pgman_▓▓▓▓"), "{at}:\n{screen}");
            } else {
                assert!(
                    screen.contains("start pgman with --dsn"),
                    "{at} hint redacted:\n{screen}"
                );
            }
        }
    }
}

/// Connected to a standby, the header wears a `replica` badge — tall
/// and one-row headers alike — and not on a primary or before it's known.
#[test]
fn the_header_marks_a_standby_as_a_replica() {
    for (w, h) in [(80, 24), (120, 40), (80, 16)] {
        let mut a = settle_app();
        a.in_recovery = Some(true);
        let screen = dump(&render(&mut a, w, h));
        let header: String = screen.lines().take(5).collect::<Vec<_>>().join("\n");
        assert!(header.contains("replica"), "{w}x{h}:\n{header}");
        for not in [Some(false), None] {
            a.in_recovery = not;
            let screen = dump(&render(&mut a, w, h));
            let header: String = screen.lines().take(5).collect::<Vec<_>>().join("\n");
            assert!(!header.contains("replica"), "{w}x{h} {not:?}:\n{header}");
        }
    }
}

/// A long RDS endpoint is cut in the header so the fields after it keep
/// their place: Database at 80 columns (User drops first, as before),
/// and both at 120, where the old line dropped User.
#[test]
fn header_keeps_database_and_user_beside_a_long_rds_endpoint() {
    let host = "examplecorp-prod.c9abc1def2gh.eu-west-2.rds.amazonaws.com";
    let mut a = settle_app();
    a.dsn = Some(Dsn::parse(&format!("postgres://ledger_app@{host}:5432/ledger")).unwrap());
    let line = row_text(&render(&mut a, 80, 24), 1);
    assert!(line.contains("Server: examplecorp-pro"), "{line}");
    assert!(line.contains("Database: ledger"), "{line}");
    let line = row_text(&render(&mut a, 120, 30), 1);
    assert!(line.contains("…:5432"), "{line}");
    assert!(line.contains("Database: ledger"), "{line}");
    assert!(line.contains("User: ledger_app"), "{line}");
}

#[test]
fn redacted_demo_start_card_hides_every_database_on_the_server() {
    // The start card's `databases` line lists the server's other
    // databases too, not only the one connected to.
    let mut a = pgman::demo::launch_app(Theme::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.redact = true;
    let rendered = dump(&render(&mut a, 160, 45));
    assert!(
        rendered.contains("databases"),
        "no databases line:\n{rendered}"
    );
    for name in ["ledger", "ledgerlog", "billing"] {
        assert!(
            !rendered
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|w| w == name || w.starts_with("ledger_")),
            "{name} shows in clear:\n{rendered}"
        );
    }
}

#[test]
fn redacted_overview_hides_an_rds_profile_named_in_its_error() {
    // An `rds` environment whose listing failed has no rows, so its
    // profile is known only from the source — and the aws CLI's error
    // on its heading names it.
    use pgman::inventory::EnvReport;
    let mut a = settle_app();
    a.environments.reports = vec![EnvReport {
        name: "staging".into(),
        source: "RDS us-west-1".into(),
        targets: Vec::new(),
        error: Some("The config profile (examplecorp-sso) could not be found".into()),
        profile: Some("examplecorp-sso".into()),
        probe: None,
    }];
    a.environments.probes = vec![Vec::new()];
    a.mode = Mode::Environments;
    let off = dump(&render(&mut a, 120, 30));
    assert!(off.contains("examplecorp-sso"), "never on screen:\n{off}");
    a.redact = true;
    let on = dump(&render(&mut a, 120, 30));
    assert!(!on.contains("examplecorp"), "leaked:\n{on}");
    assert!(on.contains("The config profile (▓▓▓▓▓▓▓▓▓▓▓-▓▓▓)"), "{on}");
}

#[test]
fn redacted_discovery_hides_the_project_in_its_version_table_paths() {
    // A proposal picks up the project's version table: its patch
    // directory and the config file it came from both name the
    // project's directory, which is often the client's name.
    use pgman::inventory::discover::with_version_table;
    let mut found = pgman::demo::discovery();
    let root = "/home/dev/git/examplecorp-api";
    with_version_table(
        &mut found.proposals,
        Some(&(
            std::path::PathBuf::from(format!("{root}/.pgman/pgman.toml")),
            pgman::inventory::VersionTable {
                table: "public.db_version".into(),
                column: "id".into(),
                patches: Some(format!("{root}/db/patches")),
                databases: Vec::new(),
            },
        )),
    );
    let chosen = vec![false; found.proposals.len()];
    let mut a = settle_app();
    a.environments.discovery = Some(pgman::app::DiscoveryUi {
        found: Some(found),
        chosen,
        ..Default::default()
    });
    a.mode = Mode::Environments;
    let off = dump(&render(&mut a, 160, 45));
    assert!(
        off.contains("examplecorp-api/db/patches") && off.contains("examplecorp-api/.pgman"),
        "the paths never on screen:\n{off}"
    );
    a.redact = true;
    let on = dump(&render(&mut a, 160, 45));
    assert!(!on.contains("examplecorp"), "leaked:\n{on}");
    // The generic directories still say what the path is.
    assert!(on.contains("/git/▓▓▓▓▓▓▓▓▓▓▓-api/db/patches"), "{on}");
    assert!(
        on.contains("/git/▓▓▓▓▓▓▓▓▓▓▓-api/.pgman/pgman.toml"),
        "{on}"
    );
}

#[test]
fn redacted_picker_hides_the_credential_variable_names() {
    // A picker row ends `password ← $VAR  user ← $VAR`; a project's
    // variable names tend to carry its client's name.
    let picks = vec![DataSourcePick {
        name: "staging".into(),
        origin: "project",
        dsn: Some(Dsn::parse("postgres://app@staging-db/main").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: pgman::app::CredsProvenance::new(
            vec!["EXAMPLECORP_DB_USER".into()],
            vec!["EXAMPLECORP_DB_PW".into()],
        ),
    }];
    let mut a = App::new(Theme::default(), None, picks, SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.mode = Mode::ConnPick;
    let off = dump(&render(&mut a, 160, 16));
    assert!(off.contains("password ← $EXAMPLECORP_DB_PW"), "{off}");
    assert!(off.contains("user ← $EXAMPLECORP_DB_USER"), "{off}");
    a.redact = true;
    let on = dump(&render(&mut a, 160, 16));
    assert!(!on.to_lowercase().contains("examplecorp"), "leaked:\n{on}");
    // The generic rest of the name, and the arrow, still say what it is.
    assert!(on.contains("user ← $▓▓▓▓▓▓▓▓▓▓▓_DB_USER"), "{on}");
}

// ---- grid column widths --------------------------------------------------

fn wide_result_app() -> App {
    let mut a = connected_app();
    let long = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod";
    a.grid = Grid {
        columns: ["id", "ref", "total", "note", "memo", "body", "tag"]
            .map(String::from)
            .to_vec(),
        rows: vec![vec![
            Some("1".into()),
            Some("1b4e28ba-2fa1-11d2-883f-0016d3cca427".into()),
            Some("-999999999999999999.99".into()),
            Some(long.into()),
            Some(long.into()),
            Some(long.into()),
            Some("x".into()),
        ]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a
}

#[test]
fn the_grid_never_cuts_a_number_and_uses_the_width_it_has() {
    for (w, h) in [(200, 50), (120, 30), (80, 24)] {
        let mut a = wide_result_app();
        let text = dump(&render(&mut a, w, h));
        assert!(
            text.contains(" -999999999999999999.99 "),
            "{w}x{h}: the whole number:\n{text}"
        );
        let data_row = text
            .lines()
            .find(|l| l.contains("-999999999999999999.99"))
            .unwrap();
        assert_eq!(
            data_row.chars().count(),
            w as usize,
            "{w}x{h}: the row reaches the panel's right border:\n{text}"
        );
    }
    // 200 columns: room for the whole uuid beside three squeezed text
    // columns, which say they were cut.
    let mut a = wide_result_app();
    let text = dump(&render(&mut a, 200, 50));
    assert!(
        text.contains("1b4e28ba-2fa1-11d2-883f-0016d3cca427"),
        "{text}"
    );
    assert!(text.contains("consectetur a"), "{text}");
    assert!(text.contains('…'), "{text}");
}

#[test]
fn a_grid_too_wide_even_squeezed_shows_the_columns_around_the_cursor() {
    let mut a = wide_result_app();
    let text = dump(&render(&mut a, 40, 24));
    assert!(text.contains("cols 1–"), "{text}");
    assert!(text.contains("/7 "), "{text}");
    a.grid_view.col_cursor = 6;
    let text = dump(&render(&mut a, 40, 24));
    assert!(text.contains("–7/7 "), "{text}");
    let header = text.lines().find(|l| l.contains("tag")).unwrap_or("");
    assert!(
        !header.contains(" id "),
        "the window moved right with the cursor:\n{text}"
    );
}

#[test]
fn tabs_and_newlines_in_a_value_show_in_the_grid_and_row_detail() {
    let mut a = connected_app();
    a.grid = Grid {
        columns: ["id", "note", "after"].map(String::from).to_vec(),
        rows: vec![vec![
            Some("1".into()),
            Some("tab\there\nline two\r\nthree".into()),
            Some("x".into()),
        ]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    let text = dump(&render(&mut a, 80, 24));
    assert!(
        text.contains("1   tab⇥here⏎line two⏎three  x"),
        "one grid line, the tab and the newlines marked:\n{text}"
    );
    a.mode = Mode::RowDetail;
    let text = dump(&render(&mut a, 80, 24));
    let note = text.lines().position(|l| l.contains("tab⇥here"));
    let note = note.unwrap_or_else(|| panic!("the tab is marked:\n{text}"));
    let next: Vec<&str> = text.lines().skip(note + 1).take(2).collect();
    assert!(
        next[0].contains("│ line two ") && next[1].contains("│ three "),
        "the lines wrap where the value breaks:\n{text}"
    );
    a.row_detail.field = 1;
    a.mode = Mode::CellDetail;
    let text = dump(&render(&mut a, 80, 24));
    assert!(
        text.contains("tab⇥here") && !text.contains("tabhere"),
        "the cell detail marks the tab too:\n{text}"
    );
}

// ---- the footer under a long error ---------------------------------------

const LONG_ERROR: &str =
    "function nonexistent_function_with_long_name(integer, integer, integer) does not exist";

fn app_with_a_long_error(mode: Mode) -> App {
    let mut a = connected_app();
    a.redact = true;
    a.mode = mode;
    a.last_error = Some(LONG_ERROR.into());
    a.last_error_detail = Some(pgman::conn::QueryErrDetail::default());
    a
}

/// The footer rows: everything under the last panel's bottom border.
fn footer_rows(buf: &Buffer) -> Vec<String> {
    let rows: Vec<String> = (0..buf.area.height).map(|y| row_text(buf, y)).collect();
    let last_border = rows
        .iter()
        .rposition(|r| r.starts_with('╰'))
        .expect("a panel bottom border");
    rows[last_border + 1..].to_vec()
}

#[test]
fn a_long_error_footer_keeps_badges_start_and_pointer_at_60_80_120() {
    // 24 rows: the error may wrap over a taller footer. 12 rows: the
    // body is cramped and the footer has one row, so the message is cut.
    for (w, h) in [(60, 24), (80, 24), (120, 24), (60, 12), (80, 12), (120, 12)] {
        let mut a = app_with_a_long_error(Mode::Editor);
        let buf = render(&mut a, w, h);
        let footer = footer_rows(&buf);
        let first = &footer[0];
        let last = footer.last().unwrap();
        assert!(
            first.starts_with(" REDACT ") && first.contains(" RO "),
            "{w}x{h}: badges first:\n{}",
            dump(&buf)
        );
        assert!(
            first.contains("⚠ function"),
            "{w}x{h}: the message from its start:\n{}",
            dump(&buf)
        );
        assert!(
            last.trim_end().ends_with("F2 / :error detail"),
            "{w}x{h}: the pointer ends the footer:\n{}",
            dump(&buf)
        );
        let shown: String = footer.concat();
        if !shown.contains("does not exist") {
            assert!(
                shown.contains("… · F2 / :error detail"),
                "{w}x{h}: a cut message says so:\n{}",
                dump(&buf)
            );
        }
    }
}

#[test]
fn an_overlay_leaves_a_wrapped_error_footer_whole() {
    // The help overlay is centred in the body; the body used to assume
    // a one-row footer, so the overlay's frame covered the first row
    // of a wrapped error — badges, icon and the start of the message —
    // leaving `integer) does not exist · ! detail` alone on screen.
    for mode in [Mode::Help, Mode::About] {
        let mut a = app_with_a_long_error(mode);
        let buf = render(&mut a, 80, 24);
        let footer = footer_rows(&buf);
        assert!(
            footer.len() >= 2,
            "{mode:?}: the error wraps over a taller footer:\n{}",
            dump(&buf)
        );
        assert!(
            footer[0].contains("REDACT") && footer[0].contains("RO"),
            "{mode:?}: the badges survive:\n{}",
            dump(&buf)
        );
        assert!(
            footer[0].contains("⚠ function nonexistent_function"),
            "{mode:?}: the message starts at its start:\n{}",
            dump(&buf)
        );
    }
}

/// The text run starting at `(x, y)`, `len` cells long.
fn run_at(buf: &Buffer, x: u16, y: u16, len: u16) -> String {
    (x..x + len).map(|x| buf[(x, y)].symbol()).collect()
}

/// Every place `needle` is drawn, cell-exact.
fn find_all(buf: &Buffer, needle: &str) -> Vec<(u16, u16)> {
    let n = needle.chars().count() as u16;
    let mut hits = Vec::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width.saturating_sub(n - 1) {
            if run_at(buf, x, y, n) == needle {
                hits.push((x, y));
            }
        }
    }
    hits
}

/// `SELECT '' AS e, NULL AS n, 'NULL' AS t`, as a grid.
fn null_grid_app() -> App {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["e".into(), "n".into(), "t".into()],
        rows: vec![vec![Some(String::new()), None, Some("NULL".into())]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a
}

/// The NULL is drawn as a dimmed italic `NULL` in `theme.null_cell`, the
/// text `'NULL'` as plain text, the empty string as nothing: three
/// different things on screen, where the grid used to draw the first
/// and the last the same.
#[test]
fn grid_draws_null_as_a_dimmed_marker_and_the_empty_string_as_nothing() {
    let mut a = null_grid_app();
    let theme = Theme::default();
    let buf = render(&mut a, 80, 24);
    let rendered = dump(&buf);
    let (hx, hy) = find_cell(&buf, "e  n").expect("header drawn");
    let data: Vec<(u16, u16)> = find_all(&buf, "NULL")
        .into_iter()
        .filter(|(_, y)| *y == hy + 1)
        .collect();
    assert_eq!(data.len(), 2, "the NULL and the text 'NULL':\n{rendered}");
    let marker = &buf[data[0]];
    assert_eq!(marker.fg, theme.null_cell, "NULL marker colour");
    assert!(marker.modifier.contains(Modifier::ITALIC), "NULL is italic");
    let text = &buf[data[1]];
    assert_ne!(text.fg, theme.null_cell, "the text 'NULL' is not dimmed");
    assert!(!text.modifier.contains(Modifier::ITALIC));
    // The empty string's column draws nothing.
    assert_eq!(buf[(hx, hy + 1)].symbol(), " ", "\n{rendered}");
}

#[test]
fn row_detail_and_cell_detail_say_null_and_empty_string_apart() {
    let theme = Theme::default();
    let mut a = null_grid_app();
    a.mode = Mode::RowDetail;
    let buf = render(&mut a, 80, 24);
    let rendered = dump(&buf);
    let empty = find_all(&buf, "(empty string)");
    assert_eq!(empty.len(), 1, "\n{rendered}");
    assert_eq!(buf[empty[0]].fg, theme.muted);
    // The `n` field is the marker, in the NULL colour; the `t` field's
    // `NULL` is text.
    let colours: Vec<_> = find_all(&buf, "NULL").iter().map(|p| buf[*p].fg).collect();
    assert!(colours.contains(&theme.null_cell), "\n{rendered}");
    assert!(colours.contains(&theme.text), "\n{rendered}");

    for (field, want, colour) in [
        (0, "(empty string)", theme.muted),
        (1, "NULL", theme.null_cell),
        (2, "NULL", theme.text),
    ] {
        a.row_detail.field = field;
        a.mode = Mode::CellDetail;
        let buf = render(&mut a, 80, 24);
        let at = find_all(&buf, want);
        let rendered = dump(&buf);
        assert!(!at.is_empty(), "field {field}:\n{rendered}");
        assert_eq!(buf[at[0]].fg, colour, "field {field}:\n{rendered}");
    }
}

/// The help overlay's footer is its own keys. A status left from before
/// help opened used to own the footer instead — at 200x50 a reviewer
/// read a leftover status under the help overlay as something it said.
#[test]
fn help_footer_shows_the_overlay_keys_not_a_stale_status() {
    for (w, h) in [(80, 24), (200, 50)] {
        let mut a = settle_app();
        a.last_status = Some("copied 3 rows to the clipboard".into());
        a.open_help_from(Mode::Editor);
        let buf = render(&mut a, w, h);
        let footer = row_text(&buf, h - 1);
        assert!(
            !footer.contains("copied 3 rows"),
            "{w}x{h}: stale status in the help footer: {footer:?}"
        );
        assert!(
            footer.contains("close help"),
            "{w}x{h}: the footer names the overlay's keys: {footer:?}"
        );
    }
}

// ---------------------------------------------------------------
// The sectioned help overlay (docs/backlog/help-overlay.md)
// ---------------------------------------------------------------

/// Rows of the render, with the help popup's borders and padding
/// trimmed off — what the operator reads.
fn help_rows(buf: &Buffer) -> Vec<String> {
    dump(buf)
        .lines()
        .map(|l| {
            l.trim_matches(|c: char| c == '│' || c.is_whitespace())
                .to_string()
        })
        .collect()
}

#[test]
fn narrow_help_lists_every_section_with_its_teaser_then_opens_one() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Char('?')));
    let buf = render(&mut a, 80, 24);
    let text = dump(&buf);
    assert!(text.contains("help · 23 sections"), "{text}");
    assert!(
        text.contains("▸ grid                     the results: move, sort, filter, find, yank"),
        "the selected section, marked, with its teaser:\n{text}"
    );
    assert!(
        text.contains("↓ 10 more"),
        "the list says there is more:\n{text}"
    );
    // Walk to the bottom: the list follows the selection.
    a.on_key(KeyEvent::from(KeyCode::Char('G')));
    let text = dump(&render(&mut a, 80, 24));
    assert!(text.contains("▸ help"), "{text}");
    assert!(text.contains("↑ 10 more"), "{text}");
    // Enter opens the selected section full width, heading on row 0.
    a.on_key(KeyEvent::from(KeyCode::Char('g')));
    a.on_key(KeyEvent::from(KeyCode::Char('j')));
    a.on_key(KeyEvent::from(KeyCode::Enter));
    let buf = render(&mut a, 80, 24);
    let rows = help_rows(&buf);
    let title = rows
        .iter()
        .position(|r| r.contains("help · editor"))
        .unwrap();
    assert!(rows[title + 1].starts_with("pgman — keys"), "{rows:#?}");
    assert_eq!(rows[title + 2], "editor", "{rows:#?}");
    assert!(
        !dump(&buf).contains("the results: move"),
        "a narrow section is full width, without the list"
    );
}

#[test]
fn wide_help_draws_the_list_beside_the_section() {
    let mut a = settle_app();
    a.open_help_from(Mode::SchemaBrowser);
    let buf = render(&mut a, 120, 40);
    let text = dump(&buf);
    assert!(a.help.split, "120 columns is a split overlay");
    assert!(text.contains("help · schema browser"), "{text}");
    assert!(
        text.contains("│ ▸ schema browser"),
        "the list marks the section:\n{text}"
    );
    let first = text
        .lines()
        .skip_while(|l| !l.contains("pgman — keys"))
        .nth(1)
        .expect("a row under the preamble");
    assert!(
        first.contains("│   schema browser"),
        "the section's heading heads the pane, right of the rule: {first:?}"
    );
    assert!(
        text.contains("in-tree filter"),
        "and its body below:\n{text}"
    );
    // Tab moves to the next section, list and pane together.
    a.on_key(KeyEvent::from(KeyCode::Tab));
    let text = dump(&render(&mut a, 120, 40));
    assert!(text.contains("help · EXPLAIN tree"), "{text}");
    assert!(text.contains("▸ EXPLAIN tree"), "{text}");
    assert!(!text.contains("in-tree filter"), "{text}");
}

#[test]
fn the_help_filter_shows_matching_rows_under_their_headings() {
    let mut a = settle_app();
    a.open_help_from(Mode::Editor);
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    for c in "clipboard".chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let buf = render(&mut a, 120, 40);
    let rows = help_rows(&buf);
    let text = dump(&buf);
    assert!(text.contains("help · filter"), "{text}");
    assert!(
        !text.contains("▸"),
        "the matches span sections, so the list marks none:\n{text}"
    );
    assert!(
        text.contains("filter: /clipboard"),
        "the footer is the filter prompt:\n{text}"
    );
    let summary = rows
        .iter()
        .find(|r| r.contains("match 'clipboard'"))
        .unwrap_or_else(|| panic!("a summary row:\n{text}"));
    assert!(summary.contains("sections"), "{summary}");
    // Grid, row detail, cell detail and notifications all copy to the
    // clipboard: each heading, then its matching rows only.
    let pane: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.split_once("│ ").map(|(_, right)| right.trim()))
        .collect();
    let at = |h: &str| pane.iter().position(|r| *r == h);
    let grid = at("grid").unwrap_or_else(|| panic!("grid heading:\n{text}"));
    assert!(pane[grid + 1].contains("clipboard"), "{pane:#?}");
    assert!(at("row detail").is_some(), "{pane:#?}");
    assert!(at("notifications").is_some(), "{pane:#?}");
    assert!(
        at("editor").is_none(),
        "a section with no match is not shown:\n{pane:#?}"
    );
    assert!(
        !text.contains("move column cursor"),
        "rows that don't match are not shown"
    );
    // No match says so.
    for c in "zzz".chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let text = dump(&render(&mut a, 120, 40));
    assert!(
        text.contains("no help row matches 'clipboardzzz'"),
        "{text}"
    );
}

#[test]
fn help_all_is_the_old_single_page() {
    let mut a = settle_app();
    a.open_help_all(Mode::Normal);
    let text = dump(&render(&mut a, 80, 24));
    assert!(text.contains("pgman — keys"), "{text}");
    assert!(text.contains("  grid"), "{text}");
    assert!(text.contains("more below"), "{text}");
    assert!(!text.contains("sections"), "no list:\n{text}");
    assert!(!text.contains("help · grid"), "no section title:\n{text}");
    // `]` jumps down the page heading by heading — the grid's, then the
    // editor's, which a single section (the grid's) does not have.
    a.on_key(KeyEvent::from(KeyCode::Char(']')));
    a.on_key(KeyEvent::from(KeyCode::Char(']')));
    let rows = help_rows(&render(&mut a, 80, 24));
    assert!(rows.iter().any(|r| r == "editor"), "{rows:#?}");
}

/// One completion candidate is `1 match`, not `1 matches`.
#[test]
fn completion_popup_with_one_candidate_says_one_match() {
    use pgman::query::complete::{Candidate, CandidateKind};
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT * FROM us".into();
    a.editor.cursor = a.editor.buffer.len();
    a.completion = Some(pgman::app::CompletionCycle {
        start: a.editor.cursor - 2,
        end: a.editor.cursor,
        origin: "us".into(),
        origin_prefix: "us".into(),
        origin_cursor: a.editor.cursor,
        candidates: vec![Candidate {
            display: "users".into(),
            insert: "users".into(),
            kind: CandidateKind::Table,
            context: None,
        }],
        selected: None,
        auto: false,
    });
    let screen = dump(&render(&mut a, 80, 18));
    assert!(screen.contains(" 1 match · Tab to cycle "), "{screen}");
}

/// Row detail's `↓ N more fields below` counts the fields the operator
/// can't see — including the one under the indicator itself — and says
/// fields, as the title does, not wrapped lines.
#[test]
fn row_detail_more_below_counts_the_fields_out_of_sight() {
    let mut a = settle_app();
    let columns: Vec<String> = (1..=30).map(|i| format!("col{i:02}")).collect();
    let row: Vec<Option<String>> = (1..=30).map(|i| Some(format!("value{i:02}"))).collect();
    a.grid = Grid {
        columns,
        rows: vec![row],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.mode = Mode::RowDetail;
    for (w, h) in [(80, 24), (120, 40)] {
        let screen = dump(&render(&mut a, w, h));
        let seen = (1..=30)
            .filter(|i| screen.contains(&format!("col{i:02} ")))
            .count();
        let want = format!("↓ {} more fields below", 30 - seen);
        assert!(screen.contains(&want), "{w}x{h} want {want}:\n{screen}");
        assert!(screen.contains("field 1 of 30"), "{w}x{h}:\n{screen}");
    }
}

/// Render and return the buffer with where the terminal cursor was put.
fn render_with_cursor(app: &mut App, w: u16, h: u16) -> (Buffer, (u16, u16)) {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).expect("terminal");
    term.draw(|f| pgman::ui::draw(f, app)).expect("draw");
    let pos = term.get_cursor_position().expect("cursor position");
    (term.backend().buffer().clone(), (pos.x, pos.y))
}

/// The demo app, settled, with `keys` pressed from Normal mode.
fn demo_after(keys: &[KeyCode]) -> App {
    let mut a = pgman::demo::app(Theme::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.mode = Mode::Normal;
    for k in keys {
        a.on_key(KeyEvent::from(*k));
    }
    a
}

/// A long pattern typed into a footer filter lost its `filter: /`
/// prompt: `fit_status` shrank the leading segment and then dropped
/// it, and the cursor — placed by counting from the prompt — landed
/// past the end of the line. The prompt keeps its head, the pattern
/// shows its tail, and the cursor sits just after it, at 60 and 80
/// columns, in each mode that types into the footer.
#[test]
fn a_long_footer_filter_keeps_its_prompt_and_cursor() {
    const PATTERN: &str = "customer_invoice_line_items_archive";
    let screens: [(&str, &[KeyCode], Mode, &str); 4] = [
        (
            "schema browser",
            &[KeyCode::Char('S'), KeyCode::Char('/')],
            Mode::SchemaBrowserFilter,
            "filter: /",
        ),
        (
            "help",
            &[KeyCode::F(1), KeyCode::Char('/')],
            Mode::HelpFilter,
            "filter: /",
        ),
        (
            "grid filter",
            &[KeyCode::Char('/')],
            Mode::GridFilter,
            "filter: /",
        ),
        ("grid find", &[KeyCode::Char('f')], Mode::GridFind, "find: "),
    ];
    for (name, keys, mode, head) in screens {
        for (w, h) in [(60u16, 16u16), (80, 24)] {
            let mut a = demo_after(keys);
            assert_eq!(a.mode, mode, "{name}");
            for c in PATTERN.chars() {
                a.on_key(KeyEvent::from(KeyCode::Char(c)));
            }
            let (buf, (cx, cy)) = render_with_cursor(&mut a, w, h);
            let footer = row_text(&buf, h - 1);
            assert_eq!(cy, h - 1, "{name} {w}x{h}: cursor off the footer");
            assert!(
                footer.contains(head),
                "{name} {w}x{h}: prompt lost: {footer:?}"
            );
            assert!(
                footer.contains("_archive"),
                "{name} {w}x{h}: the pattern's tail is missing: {footer:?}"
            );
            // The cell left of the cursor is the pattern's last char.
            assert!(cx > 0 && cx < w, "{name} {w}x{h}: cursor x={cx}");
            assert_eq!(
                buf[(cx - 1, cy)].symbol(),
                "e",
                "{name} {w}x{h}: cursor not after the pattern: {footer:?} cursor x={cx}"
            );
        }
    }
}

/// A failed statement with a three-deep cause chain and a caller stack.
fn failed_tap_event() -> pgman::tap::TapEvent {
    pgman::tap::TapEvent {
        v: 1,
        kind: pgman::tap::TapKind::Query,
        ts_unix_micros: 1_790_000_042_118_204,
        received_at_unix_micros: 1,
        app: Some("billing-service".into()),
        pool: Some("primary".into()),
        conn: Some("primary-3".into()),
        txn: Some("primary-3#118".into()),
        sql: Some("INSERT INTO invoice_lines (invoice_id, sku) VALUES (?, ?)".into()),
        params: Some(vec!["4833".into(), "SKU-2201".into()]),
        params_redacted: false,
        duration_micros: Some(2_318),
        rows: None,
        error: Some(vec![
            "DataIntegrityViolationException: could not execute statement".into(),
            "ConstraintViolationException: could not execute statement".into(),
            "PSQLException: duplicate key value violates unique constraint".into(),
        ]),
        caller: Some(vec![
            "com.example.billing.InvoiceLineRepository.save:41".into(),
            "com.example.billing.InvoiceService.addLine:112".into(),
            "com.example.billing.InvoiceController.addLine:57".into(),
        ]),
        dropped_events_total: None,
        txn_outcome: None,
    }
}

/// `Enter` on a failed List row shows what the row could only colour
/// red: the cause chain to its root, the whole caller stack, and the
/// pool / conn / txn. At 120x40 it all fits; at 80x24 the title says
/// how far down the overlay is, and `G` reaches the last frame.
#[test]
fn tap_event_detail_shows_the_cause_chain_and_caller_stack() {
    let mut a = settle_app();
    a.on_tap_event(failed_tap_event());
    a.mode = Mode::TapMonitor;
    let list = dump(&render(&mut a, 80, 24));
    assert!(list.contains("enter detail"), "footer hint:\n{list}");
    // The monitor's status takes the footer while it is open, so the
    // strip's rule names the key too.
    assert!(list.contains("── enter: the whole event ──"), "{list}");
    a.on_key(KeyEvent::from(KeyCode::Enter));

    let big = dump(&render(&mut a, 120, 40));
    for want in [
        "query · failed",
        "pool        primary",
        "conn        primary-3",
        "txn         primary-3#118",
        "error       DataIntegrityViolationException",
        "caused by   ConstraintViolationException",
        "root cause  PSQLException: duplicate key",
        "caller      com.example.billing.InvoiceLineRepository.save:41",
        "com.example.billing.InvoiceController.addLine:57",
        "esc / enter back",
    ] {
        assert!(big.contains(want), "120x40 missing {want:?}:\n{big}");
    }
    assert!(!big.contains(" of "), "nothing to scroll at 120x40:\n{big}");

    let small = dump(&render(&mut a, 80, 24));
    assert!(small.contains("rows 1–"), "80x24 position:\n{small}");
    assert!(!small.contains("InvoiceController"), "80x24:\n{small}");
    a.on_key(KeyEvent::from(KeyCode::Char('G')));
    let bottom = dump(&render(&mut a, 80, 24));
    assert!(
        bottom.contains("com.example.billing.InvoiceController.addLine:57"),
        "80x24 after G:\n{bottom}"
    );
    // Back to the list, still in the monitor.
    a.on_key(KeyEvent::from(KeyCode::Esc));
    let back = dump(&render(&mut a, 80, 24));
    assert!(back.contains("enter detail"), "{back}");
    assert_eq!(a.mode, Mode::TapMonitor);
}

/// In the overlay, as in the params strip, a label saying some values
/// are the producer's `[redacted]` marker is in the warning colour, and
/// the root cause is in the error colour.
#[test]
fn tap_event_detail_colours_the_redaction_label_and_the_causes() {
    let mut a = settle_app();
    let mut e = failed_tap_event();
    e.params = Some(vec!["4833".into(), "[redacted]".into()]);
    e.params_redacted = true;
    a.on_tap_event(e);
    a.mode = Mode::TapMonitor;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    let buf = render(&mut a, 120, 40);
    let theme = Theme::default();
    // By column, not byte offset: the borders before it are multi-byte.
    let at = |needle: &str| {
        (0..buf.area.height)
            .find_map(|y| {
                let line = row_text(&buf, y);
                line.find(needle)
                    .map(|b| (line[..b].chars().count() as u16, y))
            })
            .unwrap_or_else(|| panic!("{needle} not drawn"))
    };
    let (x, y) = at("params (some redacted)");
    assert_eq!(buf[(x, y)].fg, theme.health_yellow);
    let (x, y) = at("PSQLException");
    assert_eq!(buf[(x, y)].fg, theme.health_red);
}

// ---------------------------------------------------------------
// Cell edit (docs/backlog/cell-editing.md)
// ---------------------------------------------------------------

/// `u`'s editor open on `public.users.name where id = 2`, with `value`
/// (`None`: NULL).
fn cell_edit_app(value: Option<&str>) -> App {
    let mut a = settle_app();
    a.grid = Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![vec![Some("2".into()), value.map(str::to_string)]],
        truncated: false,
    };
    let edit = pgman::query::cell_edit::CellEdit {
        schema: "public".into(),
        table: "users".into(),
        column: "name".into(),
        key: vec![("id".into(), "2".into())],
        old: value.map(str::to_string),
        row: 0,
        col: 1,
        table_oid: 100,
        attnum: 2,
        key_attnums: vec![1],
    };
    a.cell_edit = Some(pgman::app::CellEditUi {
        input: tui_common::TextInput::with_text(value.unwrap_or_default()),
        null: value.is_none(),
        row_cells: a.grid.rows[0].clone(),
        edit,
        return_to: Mode::Normal,
    });
    a.mode = Mode::CellEdit;
    a
}

/// The editor names the cell and the row, shows a NULL as the grid's
/// dimmed `NULL`, and says its keys — at 80 columns, and in the footer.
#[test]
fn the_cell_editor_names_the_cell_and_shows_null_as_the_grid_does() {
    for (w, h) in [(80, 24), (120, 40)] {
        let mut a = cell_edit_app(None);
        let buf = render(&mut a, w, h);
        let text = dump(&buf);
        assert!(text.contains(" edit cell "), "{w}x{h}:\n{text}");
        assert!(text.contains("public.users.name"), "{w}x{h}:\n{text}");
        assert!(text.contains("where id = 2"), "{w}x{h}:\n{text}");
        assert!(
            text.contains("enter save · ctrl-n NULL · esc cancel"),
            "{w}x{h}:\n{text}"
        );
        // The value row reads `NULL`, dimmed italic like the grid's
        // marker — not the text "NULL".
        let (y, line) = text
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("│NULL "))
            .unwrap_or_else(|| panic!("{w}x{h}: no NULL value row:\n{text}"));
        let x: Vec<char> = line.chars().collect();
        let col = x
            .windows(5)
            .position(|win| win.iter().collect::<String>() == "│NULL")
            .expect("the marker")
            + 1;
        let cell = &buf[(col as u16, y as u16)];
        assert_eq!(cell.symbol(), "N");
        assert!(
            cell.modifier.contains(Modifier::ITALIC),
            "{w}x{h}: the NULL marker is italic"
        );
        let footer: String = (0..buf.area.width)
            .map(|x| buf[(x, buf.area.height - 1)].symbol().to_string())
            .collect();
        assert!(footer.contains("enter save"), "{w}x{h}: {footer}");
    }
}

/// An empty string reads as one, not as nothing, and not as NULL.
#[test]
fn the_cell_editor_shows_an_empty_string_as_one() {
    let mut a = cell_edit_app(Some(""));
    let text = dump(&render(&mut a, 80, 24));
    assert!(text.contains("│(empty string)"), "{text}");
    assert!(!text.contains("│NULL"), "{text}");
}

/// A value wider than the box scrolls to keep the cursor (at its end)
/// in view: the tail shows, not the head.
#[test]
fn a_long_value_scrolls_to_the_cursor() {
    let long = format!("{}TAIL", "x".repeat(200));
    let mut a = cell_edit_app(Some(&long));
    let text = dump(&render(&mut a, 80, 24));
    assert!(text.contains("xxxTAIL"), "{text}");
}

/// The confirm for an edit names the cell, says it is one row or
/// nothing, and shows the statement it will send.
#[test]
fn the_confirm_for_a_cell_edit_shows_the_update_and_the_one_row_rule() {
    let mut a = cell_edit_app(Some("bo"));
    a.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::Confirm);
    let text = dump(&render(&mut a, 120, 40));
    assert!(
        text.contains("edit public.users.name where id = 2"),
        "{text}"
    );
    assert!(text.contains("one row, or rolled back"), "{text}");
    assert!(
        text.contains("UPDATE public.users SET name = NULL WHERE id = '2' RETURNING name"),
        "{text}"
    );
}
