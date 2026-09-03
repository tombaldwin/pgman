//! Size-sweep snapshot suite: every reachable screen (every
//! `Mode` variant, plus a couple of Normal/TapMonitor sub-states)
//! rendered at four terminal sizes so layout regressions — text
//! clipped at a narrow width, a popup that doesn't fit a short
//! terminal, a border that collides with content — show up as
//! ordinary snapshot diffs instead of only being noticed by eye in
//! a screenshot.
//!
//! Companion to `tests/snapshots.rs` (one state per screen, one
//! fixed size). This file instead holds ~one screen per `#[test]`,
//! each rendered at [`SIZES`]. `render` / `dump` below are copied
//! from `tests/snapshots.rs` verbatim — see that file for the
//! rationale; it is not modified by this one.
//!
//! First run: `cargo test --test sizes` creates `.snap.new` files
//! under `tests/snapshots/`. `cargo insta accept` (or
//! `INSTA_UPDATE=always cargo test --test sizes` if `cargo-insta`
//! isn't installed) accepts them.
//!
//! Every screen starts from [`pgman::demo::app`] — the synthetic,
//! fully-populated app behind `pgman --demo` — so the schema cache,
//! saved queries, tap-event ring, and result grid are already
//! realistic without a live database.

use pgman::app::{
    compute_visible_rows, App, DataSourcePick, DatabaseInfo, HistorySearchState, Mode, PendingRun,
    PendingTunnel, RunKind, TapView,
};
use pgman::conn::{Dsn, NotificationMsg, QueryErrDetail};
use pgman::grid::Grid;
use pgman::query::reconstruct::{ReconstructedQuery, Source};
use pgman::query::sessions::SessionRow;
use pgman::query::slow_queries::SlowQueryRow;
use pgman::safety::{Decision, Guard, StatementKind};
use pgman::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// ---------------------------------------------------------------
// Copied from tests/snapshots.rs — do not diverge without reason;
// that file stays untouched.
// ---------------------------------------------------------------

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

// ---------------------------------------------------------------
// Sizes swept for every screen.
// ---------------------------------------------------------------

// 60x16 is the smallest size swept: it is the only one narrow enough
// to put the start card into its ONE-column layout, which is where its
// height budget was wrong, and short enough to squeeze every overlay.
const SIZES: &[(u16, u16)] = &[(60, 16), (80, 24), (100, 30), (120, 40), (200, 50)];

// ---------------------------------------------------------------
// Generic layout invariants, checked on every render before the
// snapshot assertion.
//
// The task that seeded this suite suggested checking that the four
// corner cells of "the frame area" are box-drawing corners or
// spaces. That doesn't fit pgman's actual chrome: the outermost
// layout has no full-terminal border — the header and footer are
// plain text rows (see `ui::draw_header` / `ui::draw_footer`), and
// only inner panels (editor, grid, popups) draw borders. A literal
// corner check would fail on every screen for a reason that has
// nothing to do with a layout defect.
//
// Instead:
//
// 1. No cell anywhere holds the Unicode replacement character
//    (`\u{FFFD}`) — a reliable sign that a multi-byte glyph got cut
//    mid-codepoint by a width calculation that counts bytes instead
//    of chars.
// 2. The footer (the buffer's last row) never ends on a bare
//    separator, and never ends on a very short trailing letters-only
//    token that isn't a recognised short word — the shape of the
//    "L se" defect (a hint clipped mid-word — the intended full text
//    was "...L sessions"). This is the documented fallback the task
//    allowed for when predicting every mode's exact final hint token
//    proved too fragile: several modes replace the static hint with
//    a dynamically built status string (match counts, filter text),
//    so a closed set of "the right last word for this mode" isn't
//    reliably knowable without duplicating ui.rs's formatting logic
//    here.
// ---------------------------------------------------------------

/// Screens where a known, pre-existing layout defect trips one of
/// the invariants below. Keyed by `"<screen>_<w>x<h>"`. Listing an
/// entry here means: still render and snapshot it (the defect stays
/// visible in the accepted `.snap` file), but don't fail the suite
/// on it. Other agents are fixing these defects in parallel — this
/// allowlist is meant to shrink to empty as each fix lands, not to
/// grow.
// Empty on purpose: the last two entries (footer *status* lines clipping
// mid-word at 80 columns) were fixed by routing `draw_footer`'s error and
// status branches through `fit_status` (see `src/ui.rs`). Keep this
// empty — per CLAUDE.md, widening it to make a failing sweep go quiet is
// a stop condition, not a step; a new entry means the maintainer decides,
// not the agent mid-run.
const KNOWN_DEFECTS: &[&str] = &[];

const SHORT_OK_WORDS: &[&str] = &[
    "no", "ok", "up", "in", "on", "of", "to", "at", "by", "or", "is", "it", "go", "re", "db", "id",
    "sql", "tx", "ro", "b",
];

fn footer_line(buf: &Buffer) -> String {
    let y = buf.area.height.saturating_sub(1);
    let mut s = String::new();
    for x in 0..buf.area.width {
        s.push_str(buf[(x, y)].symbol());
    }
    s.trim_end().to_string()
}

fn footer_defect(footer: &str) -> Option<String> {
    if footer.is_empty() {
        return None;
    }
    if footer.ends_with(['·', '-', '/', ':', ',', '—']) {
        return Some("footer ends with a bare separator".to_string());
    }
    let last = footer.split_whitespace().last()?;
    let letters: String = last.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    if !letters.is_empty()
        && letters.chars().count() <= 2
        && !SHORT_OK_WORDS.contains(&letters.to_lowercase().as_str())
    {
        return Some(format!(
            "footer ends with a suspiciously short trailing token {last:?}"
        ));
    }
    None
}

/// Box-drawing glyphs an overlay's border is made of. None of them
/// may appear on the footer row.
const BOX_GLYPHS: &[char] = &[
    '─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '═', '║',
];

/// The footer row belongs to `draw_footer` alone. Every screen in this
/// sweep runs the read-only demo fixture, so the row opens with the
/// `RO` badge — except `TxDecision`, which pre-empts the whole footer
/// with its own `TX OPEN` pill. Anything else means an overlay grew
/// tall enough to paint over the row that carries the mode's only
/// close hint (the About card at 80x24 used to end the screen with
/// `└──────┘`).
fn footer_overlay_defect(footer: &str) -> Option<String> {
    if let Some(bad) = footer.chars().find(|c| BOX_GLYPHS.contains(c)) {
        return Some(format!(
            "an overlay border glyph {bad:?} landed on the footer row"
        ));
    }
    if !(footer.starts_with(" RO") || footer.starts_with(" TX OPEN")) {
        return Some("footer does not start with the badge prefix \" RO\"".to_string());
    }
    None
}

fn check_invariants(screen: &str, w: u16, h: u16, buf: &Buffer) {
    let key = format!("{screen}_{w}x{h}");
    let allowed = KNOWN_DEFECTS.contains(&key.as_str());

    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if buf[(x, y)].symbol() == "\u{FFFD}" {
                let msg = format!(
                    "{screen} at {w}x{h}: replacement character at ({x},{y}) \
                     — a glyph was cut mid-codepoint"
                );
                if allowed {
                    eprintln!("KNOWN_DEFECTS (not failing): {msg}");
                    return;
                }
                panic!("{msg}");
            }
        }
    }

    let footer = footer_line(buf);
    if let Some(reason) = footer_overlay_defect(&footer) {
        let msg = format!("{screen} at {w}x{h}: {reason} (footer: {footer:?})");
        if allowed {
            eprintln!("KNOWN_DEFECTS (not failing): {msg}");
            return;
        }
        panic!("{msg}");
    }
    if let Some(reason) = footer_defect(&footer) {
        let msg = format!("{screen} at {w}x{h}: {reason} (footer: {footer:?})");
        if allowed {
            eprintln!("KNOWN_DEFECTS (not failing): {msg}");
            return;
        }
        panic!("{msg}");
    }
}

/// Render + snapshot + invariant-check `build()` at every size in
/// [`SIZES`]. `build` runs once per size since `App` isn't `Clone`.
fn run_sizes(screen: &str, mut build: impl FnMut() -> App) {
    for &(w, h) in SIZES {
        let mut app = build();
        let buf = render(&mut app, w, h);
        insta::assert_snapshot!(format!("{screen}_{w}x{h}"), dump(&buf));
        check_invariants(screen, w, h, &buf);
    }
}

// ---------------------------------------------------------------
// Base app: the demo fixture, splash dismissed.
// ---------------------------------------------------------------

fn base() -> App {
    let mut a = pgman::demo::app(Theme::default());
    a.splash_visible = false;
    a.splash_until = None;
    a
}

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

// ---------------------------------------------------------------
// Screen builders.
// ---------------------------------------------------------------

fn scr_normal_grid() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.grid_state.select(Some(0));
    a
}

/// The start card: connected, nothing run yet. `draw_body` picks it
/// when the grid has never had columns, so the demo grid is cleared.
fn scr_landing() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.grid = Grid::default();
    a.grid_view.visible_rows = Vec::new();
    a.grid_state.select(None);
    a.last_error = None;
    // Nothing has been typed either — this is the first screen after
    // connecting, and the editor's height is what the card has left to
    // lay itself out in.
    a.editor.buffer.clear();
    a.editor.cursor = 0;
    a
}

/// The start card *with its databases line drawn*. `scr_landing` never
/// renders it: `demo::app` leaves `databases` empty, so the whole
/// width-fitting path in `landing::format_databases_line` was swept
/// without ever running. Two databases named in kana put a real
/// display-width budget under it — every glyph is two terminal columns,
/// so a line measured in `char`s paints twice as wide as it claims and
/// runs through the card's right border at 60 and 80 columns.
fn scr_landing_databases() -> App {
    let mut a = scr_landing();
    a.databases = vec![
        DatabaseInfo {
            name: "受注管理データベース".into(),
            size: "812 MB".into(),
        },
        DatabaseInfo {
            name: "分析基盤データウェアハウス".into(),
            size: "3.4 GB".into(),
        },
        DatabaseInfo {
            name: "顧客マスタ統合基盤".into(),
            size: "94 MB".into(),
        },
    ];
    a
}

fn scr_normal_filter() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.grid_view.filter = Some("pro".into());
    a.grid_view.visible_rows = compute_visible_rows(&a.grid.rows, Some("pro"));
    a.grid_state.select(Some(0));
    a
}

fn scr_editor_multiline() -> App {
    let mut a = base();
    a.mode = Mode::Editor;
    a
}

fn scr_help() -> App {
    let mut a = base();
    a.mode = Mode::Help;
    a
}

fn scr_about() -> App {
    // Pin the install channel so the snapshot doesn't depend on
    // whether `.git` exists in the tree these tests run from — see
    // `tests/snapshots.rs::about_overlay` for the full rationale.
    pgman::update_check::set_channel_override_for_tests(Some(
        pgman::update_check::InstallChannel::Standalone,
    ));
    let mut a = base();
    a.mode = Mode::About;
    a
}

fn scr_error_detail() -> App {
    let mut a = base();
    a.last_error =
        Some("duplicate key value violates unique constraint \"users_email_key\"".into());
    a.last_error_detail = Some(QueryErrDetail {
        code: Some("23505".into()),
        severity: Some("ERROR".into()),
        detail: Some("Key (email)=(ada@example.com) already exists.".into()),
        hint: None,
        r#where: Some("SQL statement \"INSERT INTO users (email, plan) VALUES ($1, $2)\"".into()),
        schema: Some("public".into()),
        table: Some("users".into()),
        column: None,
        data_type: None,
        constraint: Some("users_email_key".into()),
    });
    a.mode = Mode::ErrorDetail;
    a
}

fn scr_confirm() -> App {
    let mut a = base();
    let decision = Decision {
        kind: StatementKind::Delete { has_where: false },
        guard: Guard::Confirm,
        wrap_in_tx: true,
        blocked_by_read_only: false,
        read_only_escape: false,
    };
    a.pending_run = Some(PendingRun {
        sql: "DELETE FROM orders".into(),
        kind: RunKind::Run,
        decision,
        is_batch: false,
        summary: None,
    });
    a.mode = Mode::Confirm;
    a
}

fn scr_tx_decision() -> App {
    let mut a = base();
    a.tx_open = true;
    a.mode = Mode::TxDecision;
    a
}

fn scr_log_pick() -> App {
    let mut a = base();
    a.log_pick.picks = vec![
        ReconstructedQuery {
            raw_sql: "select * from users where id = ?".into(),
            params: Vec::new(),
            runnable_sql: "select * from users where id = 42".into(),
            source: Source::HibernateLog,
            src_line: 12,
        },
        ReconstructedQuery {
            raw_sql: "select * from orders where user_id = ?".into(),
            params: Vec::new(),
            runnable_sql: "select * from orders where user_id = 42".into(),
            source: Source::PostgresLog,
            src_line: 45,
        },
    ];
    a.log_pick.index = 0;
    a.mode = Mode::LogPick;
    a
}

fn scr_conn_pick() -> App {
    let mut a = base();
    a.conn_pick.picks = vec![
        DataSourcePick {
            name: "prod".into(),
            origin: "project",
            dsn: Some(Dsn::parse("postgres://app@prod-db/main").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
        },
        DataSourcePick {
            name: "staging".into(),
            origin: "IntelliJ",
            dsn: Some(Dsn::parse("postgres://app@staging-db/main").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
        },
    ];
    a.conn_pick.index = 0;
    a.mode = Mode::ConnPick;
    a
}

/// The picker's ssh-tunnel confirmation: a discovered pick carrying
/// `?ssh_tunnel=` was chosen and the prompt is asking for an explicit
/// `y` before pgman spawns `ssh`. Drawn in place of the candidate
/// list, so it is a distinct screen with its own width budget.
fn scr_tunnel_prompt() -> App {
    let mut a = scr_conn_pick();
    a.pending_tunnel = Some(PendingTunnel {
        dsn: Dsn::parse("postgres://app@db.internal:5432/main?ssh_tunnel=tom@bastion.example.com")
            .unwrap(),
        origin: "picked project data source 'via-bastion'".into(),
    });
    a
}

fn scr_row_detail() -> App {
    let mut a = base();
    a.grid_state.select(Some(0));
    a.row_detail.field = 0;
    a.row_detail.field_count = a.grid.columns.len();
    a.mode = Mode::RowDetail;
    a
}

fn scr_cell_detail() -> App {
    let mut a = base();
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
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(
        a.mode,
        Mode::CellDetail,
        "Enter on a JSON field should open CellDetail"
    );
    a
}

fn scr_history_search() -> App {
    let mut a = base();
    a.mode = Mode::HistorySearch;
    a.history_search = Some(HistorySearchState {
        query: "sel".into(),
        matched: Some(0),
        saved_buffer: String::new(),
        saved_cursor: 0,
    });
    a.last_status = Some("(reverse-i-search) 'sel'".into());
    a.editor.buffer = "SELECT count(*) FROM orders WHERE status = 'shipped';".into();
    a.editor.cursor = a.editor.buffer.len();
    a
}

fn scr_grid_filter() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.on_key(key('/'));
    a.on_key(key('p'));
    a.on_key(key('r'));
    a.on_key(key('o'));
    assert_eq!(a.mode, Mode::GridFilter);
    a
}

fn scr_grid_find() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.on_key(key('f'));
    a.on_key(key('p'));
    a.on_key(key('r'));
    a.on_key(key('o'));
    assert_eq!(a.mode, Mode::GridFind);
    a
}

fn scr_explain_tree() -> App {
    let mut a = base();
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
    a.explain.cursor = 0;
    a.mode = Mode::ExplainTree;
    a
}

fn scr_schema_browser() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.on_key(key('S'));
    assert_eq!(a.mode, Mode::SchemaBrowser);
    a.on_key(key('+')); // expand every schema + table
    a
}

fn scr_schema_browser_filter() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.on_key(key('S'));
    a.on_key(key('/'));
    a.on_key(key('u'));
    a.on_key(key('s'));
    assert_eq!(a.mode, Mode::SchemaBrowserFilter);
    a
}

fn scr_slow_queries() -> App {
    let mut a = base();
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
    a
}

fn scr_sessions() -> App {
    let mut a = base();
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
    a
}

fn scr_schema_lint() -> App {
    let mut a = base();
    a.schema_lint.findings = pgman::query::lint::run_all(&a.schema_cache);
    a.schema_lint.cursor = 0;
    a.mode = Mode::SchemaLint;
    a
}

fn scr_confirm_terminate() -> App {
    let mut a = scr_sessions();
    a.on_key(key('K'));
    assert_eq!(a.mode, Mode::ConfirmTerminate);
    a
}

fn scr_notifications() -> App {
    let mut a = base();
    a.notifications.items = vec![
        NotificationMsg {
            channel: "orders_channel".into(),
            pid: 4321,
            payload: "order:9001 shipped".into(),
        },
        NotificationMsg {
            channel: "cache_invalidate".into(),
            pid: 4322,
            payload: "users".into(),
        },
    ];
    a.notifications.cursor = 0;
    a.mode = Mode::Notifications;
    a
}

fn scr_saved_queries() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.on_key(key('Q'));
    assert_eq!(a.mode, Mode::SavedQueries);
    a
}

fn scr_saved_queries_filter() -> App {
    let mut a = scr_saved_queries();
    a.on_key(key('/'));
    a.on_key(key('d'));
    assert_eq!(a.mode, Mode::SavedQueriesFilter);
    a
}

fn scr_save_query_prompt() -> App {
    let mut a = base();
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(a.mode, Mode::SaveQueryPrompt);
    a
}

fn scr_param_prompt() -> App {
    let mut a = scr_saved_queries();
    // Demo's saved queries are inserted pro-users, order-by-id,
    // daily-revenue — `j` once moves onto `order-by-id`, which has
    // a `:order_id` placeholder.
    a.on_key(key('j'));
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::ParamPrompt);
    a
}

fn scr_rename_query_prompt() -> App {
    let mut a = scr_saved_queries();
    a.on_key(key('r'));
    assert_eq!(a.mode, Mode::RenameQueryPrompt);
    a
}

fn scr_result_diff() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.on_key(key('D')); // pin A
                        // Mutate the grid so B differs from A: change a cell, drop a
                        // row, add a row.
    a.grid.rows[0][1] = "ada+updated@example.com".into();
    a.grid.rows.remove(1);
    a.grid.rows.push(vec![
        "7".into(),
        "nathan@example.com".into(),
        "free".into(),
        "2026-04-01 00:00:00+00".into(),
    ]);
    a.on_key(key('D')); // diff B against A
    assert_eq!(a.mode, Mode::ResultDiff);
    a
}

fn scr_tap(view: TapView) -> App {
    let mut a = base();
    a.mode = Mode::TapMonitor;
    a.tap_nav.view = view;
    a
}

fn scr_tap_baseline() -> App {
    let mut a = scr_tap(TapView::Baseline);
    // Shift-B is a global chord (works from any mode) — capture a
    // real baseline so the view renders its populated table rather
    // than only the "no baseline yet" placeholder.
    a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
    a
}

// ---------------------------------------------------------------
// One #[test] per screen.
// ---------------------------------------------------------------

#[test]
fn normal_grid() {
    run_sizes("normal_grid", scr_normal_grid);
}

#[test]
fn landing() {
    run_sizes("landing", scr_landing);
}

#[test]
fn landing_databases() {
    run_sizes("landing_databases", scr_landing_databases);
}

#[test]
fn normal_filter() {
    run_sizes("normal_filter", scr_normal_filter);
}

#[test]
fn editor_multiline() {
    run_sizes("editor_multiline", scr_editor_multiline);
}

#[test]
fn help() {
    run_sizes("help", scr_help);
}

#[test]
fn about() {
    run_sizes("about", scr_about);
}

#[test]
fn error_detail() {
    run_sizes("error_detail", scr_error_detail);
}

#[test]
fn confirm() {
    run_sizes("confirm", scr_confirm);
}

#[test]
fn tx_decision() {
    run_sizes("tx_decision", scr_tx_decision);
}

#[test]
fn log_pick() {
    run_sizes("log_pick", scr_log_pick);
}

#[test]
fn conn_pick() {
    run_sizes("conn_pick", scr_conn_pick);
}

#[test]
fn tunnel_prompt() {
    run_sizes("tunnel_prompt", scr_tunnel_prompt);
}

/// The prompt's box had a hard 40-column floor: `clamp(40, width - 2)`
/// panics once the terminal is 41 columns or narrower (`min > max`),
/// so a resize mid-prompt aborted the whole TUI. Every width down to
/// nothing must render without panicking — the sweep above only
/// starts at 60.
#[test]
fn tunnel_prompt_survives_every_narrow_width() {
    for w in 1..=60u16 {
        let mut a = scr_tunnel_prompt();
        let buf = render(&mut a, w, 12);
        assert_eq!(buf.area.width, w);
    }
}

#[test]
fn row_detail() {
    run_sizes("row_detail", scr_row_detail);
}

#[test]
fn cell_detail() {
    run_sizes("cell_detail", scr_cell_detail);
}

#[test]
fn history_search() {
    run_sizes("history_search", scr_history_search);
}

#[test]
fn grid_filter() {
    run_sizes("grid_filter", scr_grid_filter);
}

#[test]
fn grid_find() {
    run_sizes("grid_find", scr_grid_find);
}

#[test]
fn explain_tree() {
    run_sizes("explain_tree", scr_explain_tree);
}

#[test]
fn schema_browser() {
    run_sizes("schema_browser", scr_schema_browser);
}

#[test]
fn schema_browser_filter() {
    run_sizes("schema_browser_filter", scr_schema_browser_filter);
}

#[test]
fn slow_queries() {
    run_sizes("slow_queries", scr_slow_queries);
}

#[test]
fn sessions() {
    run_sizes("sessions", scr_sessions);
}

#[test]
fn schema_lint() {
    run_sizes("schema_lint", scr_schema_lint);
}

#[test]
fn confirm_terminate() {
    run_sizes("confirm_terminate", scr_confirm_terminate);
}

#[test]
fn notifications() {
    run_sizes("notifications", scr_notifications);
}

#[test]
fn saved_queries() {
    run_sizes("saved_queries", scr_saved_queries);
}

#[test]
fn saved_queries_filter() {
    run_sizes("saved_queries_filter", scr_saved_queries_filter);
}

#[test]
fn save_query_prompt() {
    run_sizes("save_query_prompt", scr_save_query_prompt);
}

#[test]
fn param_prompt() {
    run_sizes("param_prompt", scr_param_prompt);
}

#[test]
fn rename_query_prompt() {
    run_sizes("rename_query_prompt", scr_rename_query_prompt);
}

#[test]
fn result_diff() {
    run_sizes("result_diff", scr_result_diff);
}

#[test]
fn tap_list() {
    run_sizes("tap_list", || scr_tap(TapView::List));
}

#[test]
fn tap_hotspots() {
    run_sizes("tap_hotspots", || scr_tap(TapView::Hotspots));
}

#[test]
fn tap_callers() {
    run_sizes("tap_callers", || scr_tap(TapView::Callers));
}

#[test]
fn tap_transactions() {
    run_sizes("tap_transactions", || scr_tap(TapView::Transactions));
}

#[test]
fn tap_pools() {
    run_sizes("tap_pools", || scr_tap(TapView::Pools));
}

#[test]
fn tap_nplus1() {
    run_sizes("tap_nplus1", || scr_tap(TapView::NplusOne));
}

#[test]
fn tap_baseline() {
    run_sizes("tap_baseline", scr_tap_baseline);
}

/// The `:` command bar, mid-word: the prompt takes over the footer
/// row that normally carries the hints, so this pins that the badges
/// still lead and the typed text follows the `:` prefix.
fn scr_command_bar() -> App {
    let mut a = base();
    a.mode = Mode::Normal;
    a.grid_state.select(Some(0));
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    for c in "rea".chars() {
        a.on_key(key(c));
    }
    a
}

#[test]
fn command_bar() {
    run_sizes("command_bar", scr_command_bar);
}
