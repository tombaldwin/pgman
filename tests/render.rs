//! Render-path snapshots using ratatui's `TestBackend`. These don't
//! use `insta` (deliberately — avoid the dep); instead they assert
//! on specific invariants: which cells carry which colours, what
//! string appears where. That's more verbose than a full snapshot
//! but survives terminal-size tweaks and minor layout shifts.
//!
//! The tests build a real `App` via `App::new`, force splash off,
//! seed any state the scenario needs, then render once into a
//! `TestBackend` and inspect the resulting buffer.

use pgman::app::{
    compute_visible_rows, App, ConnState, DataSourcePick, HistorySearchState, Mode,
};
use pgman::grid::cmp_cells;
use pgman::conn::Dsn;
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
fn find_cell<'a>(buf: &'a Buffer, needle: &str) -> Option<(u16, u16)> {
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
    a.editor_buffer = "SELECT * FROM users".into();
    a.editor_cursor = 0;
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
    a.editor_buffer = "SELECT * FROM zzz_definitely_not_a_table".into();
    a.editor_cursor = 0;
    // Non-empty cache so the classifier runs (without a cache the
    // renderer skips classify and falls back to default colours).
    a.schema_cache.tables.push(TableMeta {
        schema: "public".into(),
        name: "users".into(),
    });
    let theme = a.theme.clone();
    let buf = render(&mut a, 80, 16);
    let (x, y) = find_cell(&buf, "zzz_definitely_not_a_table")
        .expect("unknown identifier should appear");
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
    };
    // Initialise the view state the way the run-loop would after a
    // QueryOk lands (visible rows + sort state).
    a.grid_visible_rows = (0..a.grid.rows.len()).collect();
    a.grid_col_cursor = 0;
    a.grid_sort = Some((0, true));
    a.grid.rows.sort_by(|x, y| cmp_cells(&x[0], &y[0]));
    let buf = render(&mut a, 60, 18);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("id ▲"),
        "expected sort marker after `id`; full render:\n{rendered}"
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
    };
    a.grid_filter = Some("a".into());
    a.grid_visible_rows = compute_visible_rows(&a.grid.rows, Some("a"));
    a.grid_state.select(if a.grid_visible_rows.is_empty() {
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
    let buf = render(&mut a, 120, 60);
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
    a.editor_buffer = "SELECT * FROM users".into();
    a.editor_cursor = a.editor_buffer.len();
    let buf = render(&mut a, 80, 16);
    let rendered = dump(&buf);
    assert!(
        rendered.contains("(reverse-i-search) 'sel'"),
        "footer should show the reverse-i-search prompt; full render:\n{rendered}"
    );
}
