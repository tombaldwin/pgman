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

fn settle_app() -> App {
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
fn empty_normal_mode() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    let buf = render(&mut a, 80, 16);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn editor_with_sql_buffer() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor_buffer = "SELECT id, email\nFROM users\nWHERE active = true".into();
    a.editor_cursor = 0;
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
    };
    a.grid_visible_rows = (0..a.grid.rows.len()).collect();
    a.grid_col_cursor = 0;
    a.grid_sort = Some((0, true));
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
    };
    a.grid_filter = Some("a".into());
    a.grid_visible_rows = compute_visible_rows(&a.grid.rows, Some("a"));
    a.grid_state.select(Some(0));
    let buf = render(&mut a, 60, 14);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn help_overlay() {
    let mut a = settle_app();
    a.mode = Mode::Help;
    let buf = render(&mut a, 100, 50);
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
    a.editor_buffer = "SELECT * FROM users".into();
    a.editor_cursor = a.editor_buffer.len();
    let buf = render(&mut a, 80, 16);
    insta::assert_snapshot!(dump(&buf));
}

#[test]
fn watch_mode_status_visible() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor_buffer = "SELECT count(*) FROM users".into();
    a.editor_cursor = a.editor_buffer.len();
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
    a.editor_buffer = "SELECT * FROM us".into();
    a.editor_cursor = a.editor_buffer.len();
    a.completion = Some(CompletionCycle {
        start: a.editor_cursor - 2,
        end: a.editor_cursor,
        origin: "us".into(),
        origin_prefix: "us".into(),
        origin_cursor: a.editor_cursor,
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
