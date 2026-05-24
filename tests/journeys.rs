//! End-to-end user-journey tests. Drive an `App` via a sequence of
//! key events and assert on the resulting state — same as if an
//! operator had typed those keys in a real session. These pair with
//! the snapshot tests (`tests/snapshots.rs`): snapshots verify the
//! pixels after a fixture state, journeys verify the state after a
//! fixture sequence of keys.
//!
//! No async runtime needed — `on_key` is synchronous; the only
//! things that need the runtime are spawned tasks (connect, query
//! run), and those aren't exercised here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pgman::app::{App, ConnState, DataSourcePick, Mode};
use pgman::conn::Dsn;
use pgman::grid::Grid;
use pgman::safety::SafetyConfig;
use pgman::theme::Theme;

/// One keystroke spec: `(KeyCode, KeyModifiers)`.
type Stroke = (KeyCode, KeyModifiers);

fn k(c: char) -> Stroke {
    (KeyCode::Char(c), KeyModifiers::NONE)
}
fn ctrl(c: char) -> Stroke {
    (KeyCode::Char(c), KeyModifiers::CONTROL)
}
fn special(code: KeyCode) -> Stroke {
    (code, KeyModifiers::NONE)
}

/// Feed each stroke through `App::on_key` in order.
fn drive(app: &mut App, strokes: &[Stroke]) {
    for (code, mods) in strokes {
        app.on_key(KeyEvent::new(*code, *mods));
    }
}

fn settle_app() -> App {
    let dsn = Some(Dsn::parse("postgres://test@localhost/test").unwrap());
    let mut a = App::new(Theme::default(), dsn, Vec::new(), SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.conn_state = ConnState::Connected {
        server_version: "16.0".into(),
    };
    a
}

// ---- journeys --------------------------------------------------------------

#[test]
fn focus_editor_type_select_and_back_to_normal() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    drive(
        &mut a,
        &[
            k('e'),                       // Normal → Editor
            k('S'), k('E'), k('L'), k('E'), k('C'), k('T'),
            k(' '), k('1'),
            special(KeyCode::Esc),        // Editor → Normal
        ],
    );
    assert_eq!(a.mode, Mode::Normal);
    assert_eq!(a.editor_buffer, "SELECT 1");
}

#[test]
fn ctrl_u_clears_editor_buffer() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.editor_buffer = "DROP TABLE everything".into();
    a.editor_cursor = a.editor_buffer.len();
    drive(&mut a, &[ctrl('u')]);
    assert_eq!(a.editor_buffer, "");
    assert_eq!(a.editor_cursor, 0);
}

#[test]
fn enter_editor_then_backspace_back_to_empty() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    drive(
        &mut a,
        &[
            k('i'), // Normal → Editor (alternate binding)
            k('x'), k('y'), k('z'),
            special(KeyCode::Backspace),
            special(KeyCode::Backspace),
            special(KeyCode::Backspace),
        ],
    );
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor_buffer, "");
}

#[test]
fn help_open_via_question_mark_close_via_esc() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    drive(&mut a, &[k('?')]);
    assert_eq!(a.mode, Mode::Help);
    drive(&mut a, &[special(KeyCode::Esc)]);
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn history_search_opens_and_loads_matching_entry() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.history = vec![
        "SELECT 1".into(),
        "INSERT INTO logs (id) VALUES (1)".into(),
        "SELECT count(*) FROM users".into(),
    ];
    a.editor_buffer = "in-progress".into();
    a.editor_cursor = a.editor_buffer.len();
    drive(
        &mut a,
        &[
            ctrl('r'),                  // open reverse-i-search
            k('s'), k('e'), k('l'),     // narrows to most-recent SELECT
            special(KeyCode::Enter),    // accept
        ],
    );
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor_buffer, "SELECT count(*) FROM users");
}

#[test]
fn history_search_esc_restores_pre_search_buffer() {
    let mut a = settle_app();
    a.mode = Mode::Editor;
    a.history = vec!["SELECT 1".into()];
    a.editor_buffer = "scratch".into();
    a.editor_cursor = a.editor_buffer.len();
    drive(&mut a, &[ctrl('r'), special(KeyCode::Esc)]);
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor_buffer, "scratch");
}

#[test]
fn grid_filter_typing_narrows_visible_rows() {
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
    a.grid_visible_rows = (0..a.grid.rows.len()).collect();
    a.grid_state.select(Some(0));
    drive(
        &mut a,
        &[
            k('/'),                  // enter filter
            k('o'),                  // matches `bob` and `carol`
        ],
    );
    assert_eq!(a.mode, Mode::GridFilter);
    assert_eq!(a.grid_visible_rows, vec![1, 2]);
    drive(&mut a, &[special(KeyCode::Esc)]);
    assert!(a.grid_filter.is_none());
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn change_connection_opens_picker_when_picks_exist() {
    let pick = DataSourcePick {
        name: "primary".into(),
        origin: "project",
        dsn: Dsn::parse("postgres://app@db/x").unwrap(),
    };
    let mut a = App::new(
        Theme::default(),
        Some(Dsn::parse("postgres://test@localhost/test").unwrap()),
        vec![pick],
        SafetyConfig::default(),
    );
    a.splash_visible = false;
    a.splash_until = None;
    a.mode = Mode::Normal;
    drive(&mut a, &[k('c')]);
    assert_eq!(a.mode, Mode::ConnPick);
}

#[test]
fn grid_sort_cycle_through_three_states() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["id".into()],
        rows: vec![vec!["3".into()], vec!["1".into()], vec!["2".into()]],
    };
    a.grid_visible_rows = (0..a.grid.rows.len()).collect();
    a.grid_state.select(Some(0));
    // ASC.
    drive(&mut a, &[k('s')]);
    assert_eq!(a.grid_sort, Some((0, true)));
    let ids: Vec<&str> = a.grid.rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(ids, vec!["1", "2", "3"]);
    // DESC.
    drive(&mut a, &[k('s')]);
    assert_eq!(a.grid_sort, Some((0, false)));
    let ids: Vec<&str> = a.grid.rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(ids, vec!["3", "2", "1"]);
    // Off — restores original order.
    drive(&mut a, &[k('s')]);
    assert_eq!(a.grid_sort, None);
    let ids: Vec<&str> = a.grid.rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(ids, vec!["3", "1", "2"]);
}

#[test]
fn quit_via_q_in_normal_mode() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    drive(&mut a, &[k('q')]);
    assert!(a.should_quit);
}

#[test]
fn ctrl_c_quits_in_normal_mode_but_not_in_editor() {
    let mut a = settle_app();
    a.mode = Mode::Normal;
    drive(&mut a, &[ctrl('c')]);
    assert!(a.should_quit, "Ctrl-C in Normal should quit");

    let mut a = settle_app();
    a.mode = Mode::Editor;
    drive(&mut a, &[ctrl('c')]);
    assert!(!a.should_quit, "Ctrl-C in Editor with no running query should NOT quit");
}
