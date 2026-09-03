use super::*;

#[test]
fn should_coalesce_undo_merges_consecutive_char_inserts_inside_window() {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(50);
    let window = Duration::from_millis(500);
    assert!(should_coalesce_undo(
        EditorActionKind::CharInsert,
        t0,
        EditorActionKind::CharInsert,
        t1,
        window,
    ));
}

#[test]
fn should_coalesce_undo_refuses_after_window_expires() {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(600);
    let window = Duration::from_millis(500);
    assert!(!should_coalesce_undo(
        EditorActionKind::CharInsert,
        t0,
        EditorActionKind::CharInsert,
        t1,
        window,
    ));
}

#[test]
fn should_coalesce_undo_refuses_non_charinsert_neighbours() {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let window = Duration::from_millis(500);
    assert!(!should_coalesce_undo(
        EditorActionKind::Other,
        t0,
        EditorActionKind::CharInsert,
        t0,
        window,
    ));
    assert!(!should_coalesce_undo(
        EditorActionKind::CharInsert,
        t0,
        EditorActionKind::Other,
        t0,
        window,
    ));
}

#[test]
fn f1_from_editor_opens_help() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "select 1".into();
    a.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Help);
    assert_eq!(a.help.origin, Some(Mode::Editor));
}

#[test]
fn f1_from_help_closes_back_to_origin_mode() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Help);
    a.on_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    // Restored to the source mode, not Normal.
    assert_eq!(a.mode, Mode::Editor);
    assert!(a.help.origin.is_none());
}

#[test]
fn help_anchor_for_known_modes_picks_their_section() {
    assert_eq!(
        App::help_anchor_for(Mode::SchemaBrowser),
        Some("schema browser")
    );
    assert_eq!(
        App::help_anchor_for(Mode::ExplainTree),
        Some("EXPLAIN tree")
    );
    assert_eq!(App::help_anchor_for(Mode::Editor), Some("editor"));
    assert_eq!(App::help_anchor_for(Mode::LogPick), Some("log pick"));
    assert_eq!(App::help_anchor_for(Mode::Help), None);
    // `?` from the start card opens the document at the top — not two
    // rows into it with an `↑ 2 more above` marker.
    assert_eq!(App::help_anchor_for(Mode::Normal), None);
}

#[test]
fn mode_entry_hint_fires_only_first_time_per_session() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // Synthetic transition: schema cache empty, but
    // note_mode_entry is mode-aware and doesn't care about
    // contents. We call it directly to bypass the schema-empty
    // guard on start_schema_browser.
    a.note_mode_entry(Mode::SchemaBrowser);
    let first = a.last_status.clone();
    assert!(
        first
            .as_deref()
            .map(|s| s.starts_with("tip"))
            .unwrap_or(false),
        "first entry should set a tip; got {first:?}"
    );
    // Second entry: hint suppressed (status stays at whatever
    // the caller left it as). We mimic that by clearing the
    // status and re-entering.
    a.last_status = None;
    a.note_mode_entry(Mode::SchemaBrowser);
    assert!(
        a.last_status.is_none(),
        "second entry should NOT re-fire the hint; got {:?}",
        a.last_status
    );
}

#[test]
fn ctrl_enter_in_editor_attempts_to_run_query() {
    // Reproduces: "ctrl-enter doesn't execute" after the undo
    // wrapper landed. With no client connected, `request_run`
    // surfaces "not connected" — we use that signal to confirm
    // the run path was reached.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "select 1".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    // request_run rejects with "not connected" — that's the
    // intended signal here. If we still see "editor is empty"
    // or nothing happened, the run path wasn't reached.
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("not connected"),
        "Ctrl-Enter should hit request_run; last_error = {err:?}"
    );
}

#[test]
fn ctrl_j_in_editor_attempts_to_run_query() {
    // Some terminals report Ctrl-Enter as Ctrl-J.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "select 1".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("not connected"),
        "Ctrl-J should hit request_run; last_error = {err:?}"
    );
}

#[test]
fn editor_undo_restores_pre_typing_buffer() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "select 1".into();
    a.editor.cursor = a.editor.buffer.len();
    // Type a char — pushes the prior state to undo.
    a.on_key(KeyEvent::from(KeyCode::Char(';')));
    assert_eq!(a.editor.buffer, "select 1;");
    // Undo: buffer returns to its pre-typing value.
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "select 1");
    // Redo: forward to the typed state.
    a.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "select 1;");
}

#[test]
fn editor_undo_when_empty_surfaces_status_not_crash() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(a.last_status.as_deref(), Some("nothing to undo"));
}

#[test]
fn editor_redo_invalidated_by_a_new_edit() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "a".into();
    a.editor.cursor = 1;
    a.on_key(KeyEvent::from(KeyCode::Char('b'))); // buf = "ab"
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)); // undo → "a"
    assert!(!a.editor.redo.is_empty(), "undo should populate redo");
    a.on_key(KeyEvent::from(KeyCode::Char('c'))); // new edit invalidates redo
    assert!(a.editor.redo.is_empty(), "new mutation must clear redo");
    assert_eq!(a.editor.buffer, "ac");
}

#[test]
fn editor_consecutive_char_inserts_coalesce_into_one_undo_step() {
    // Type `xyz` in quick succession (synthetic — the test runs
    // well inside UNDO_COALESCE_WINDOW). One undo should drop
    // ALL THREE characters at once.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::from(KeyCode::Char('x')));
    a.on_key(KeyEvent::from(KeyCode::Char('y')));
    a.on_key(KeyEvent::from(KeyCode::Char('z')));
    assert_eq!(a.editor.buffer, "xyz");
    // One undo unwinds the whole typing run.
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "");
}

#[test]
fn editor_backspace_does_not_coalesce_with_char_inserts() {
    // Typing then backspacing should be two distinct undo
    // steps. Otherwise an undo after a backspace would also
    // unwind the preceding char-insert run, which is wrong.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::from(KeyCode::Char('a')));
    a.on_key(KeyEvent::from(KeyCode::Char('b'))); // buf = "ab"
    a.on_key(KeyEvent::from(KeyCode::Backspace)); // buf = "a"
    assert_eq!(a.editor.buffer, "a");
    // First undo restores the pre-backspace state ("ab").
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "ab");
    // Second undo unwinds the typing run.
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "");
}

#[test]
fn editor_undo_caps_at_undo_cap_entries() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    // Each Backspace is a non-coalescing edit. Drive past the cap.
    for i in 0..(UNDO_CAP + 20) {
        a.editor.buffer = format!("buf{i}");
        a.editor.cursor = a.editor.buffer.len();
        a.push_undo("prev".to_string(), 0, EditorActionKind::Other);
    }
    assert!(
        a.editor.undo.len() <= UNDO_CAP,
        "undo ring grew past cap: {}",
        a.editor.undo.len()
    );
}

#[test]
fn editor_insert_pair_places_cursor_between_brackets() {
    let mut buf = String::new();
    let mut cur = 0;
    assert!(editor_insert_pair(&mut buf, &mut cur, '('));
    assert_eq!(buf, "()");
    assert_eq!(cur, 1);
    // Squares + braces work the same.
    let mut buf = String::from("a");
    let mut cur = 1;
    assert!(editor_insert_pair(&mut buf, &mut cur, '['));
    assert_eq!(buf, "a[]");
    assert_eq!(cur, 2);
    let mut buf = String::new();
    let mut cur = 0;
    assert!(editor_insert_pair(&mut buf, &mut cur, '{'));
    assert_eq!(buf, "{}");
    assert_eq!(cur, 1);
}

#[test]
fn editor_insert_pair_refuses_non_opener_chars() {
    let mut buf = String::new();
    let mut cur = 0;
    assert!(!editor_insert_pair(&mut buf, &mut cur, 'x'));
    assert_eq!(buf, "");
    assert_eq!(cur, 0);
}

#[test]
fn editor_maybe_pair_quote_pairs_single_quote_at_token_boundary() {
    // Empty buffer — both neighbours are EOB, prev/next ok.
    let mut buf = String::new();
    let mut cur = 0;
    assert!(editor_maybe_pair_quote(&mut buf, &mut cur, '\''));
    assert_eq!(buf, "''");
    assert_eq!(cur, 1);
}

#[test]
fn editor_maybe_pair_quote_pairs_double_quote_after_whitespace() {
    // `SELECT ` with cursor at end — prev is space, next is EOB.
    let mut buf = String::from("SELECT ");
    let mut cur = buf.len();
    assert!(editor_maybe_pair_quote(&mut buf, &mut cur, '"'));
    assert_eq!(buf, "SELECT \"\"");
    assert_eq!(cur, 8); // between the two quotes
}

#[test]
fn editor_maybe_pair_quote_refuses_inside_word() {
    // `it` cursor at end — prev is alpha. Don't pair.
    let mut buf = String::from("it");
    let mut cur = buf.len();
    assert!(!editor_maybe_pair_quote(&mut buf, &mut cur, '\''));
    // Buffer untouched so caller can fall back to literal insert.
    assert_eq!(buf, "it");
    assert_eq!(cur, 2);
}

#[test]
fn editor_maybe_pair_quote_refuses_when_next_is_word() {
    // `abc` cursor at start — next is alpha. Don't pair.
    let mut buf = String::from("abc");
    let mut cur = 0;
    assert!(!editor_maybe_pair_quote(&mut buf, &mut cur, '\''));
    assert_eq!(buf, "abc");
    assert_eq!(cur, 0);
}

#[test]
fn editor_maybe_pair_quote_refuses_when_next_is_same_quote() {
    // `'` cursor between — typing `'` again should NOT pair
    // (the skip-quote branch handles this, but pair_quote
    // alone must also refuse so the caller's fallback order
    // is correct).
    let mut buf = String::from("''");
    let mut cur = 1;
    assert!(!editor_maybe_pair_quote(&mut buf, &mut cur, '\''));
    assert_eq!(buf, "''");
    assert_eq!(cur, 1);
}

#[test]
fn editor_maybe_pair_quote_refuses_non_quote_chars() {
    let mut buf = String::new();
    let mut cur = 0;
    assert!(!editor_maybe_pair_quote(&mut buf, &mut cur, 'x'));
    assert_eq!(buf, "");
}

#[test]
fn typing_quote_inside_sql_literal_inserts_escape_not_skip() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    // Start from the state pair_quote would leave us in after
    // typing `'`, then typing `don` literally: `'don'` with
    // cursor=4 between `n` and the closer.
    a.editor.buffer = "'don'".into();
    a.editor.cursor = 4;
    a.on_key(KeyEvent::from(KeyCode::Char('\'')));
    // Inserts an escape apostrophe instead of skipping past
    // the existing closer — the buffer grows by one char.
    assert_eq!(a.editor.buffer, "'don''");
    assert_eq!(a.editor.cursor, 5);
}

#[test]
fn typing_quote_at_eof_pairs_and_leaves_cursor_between() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::from(KeyCode::Char('\'')));
    assert_eq!(a.editor.buffer, "''");
    assert_eq!(a.editor.cursor, 1);
    // Typing another `'` skips past the pair instead of stacking.
    a.on_key(KeyEvent::from(KeyCode::Char('\'')));
    assert_eq!(a.editor.buffer, "''");
    assert_eq!(a.editor.cursor, 2);
}

#[test]
fn typing_quote_inside_word_inserts_literal_not_pair() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "it".into();
    a.editor.cursor = 2;
    a.on_key(KeyEvent::from(KeyCode::Char('\'')));
    // Falls through to literal insert — covers contractions
    // like `it's` in -- comments.
    assert_eq!(a.editor.buffer, "it'");
    assert_eq!(a.editor.cursor, 3);
}

#[test]
fn editor_toggle_line_comment_adds_marker_on_first_press() {
    let mut buf = String::from("select 1");
    let mut cur = 3; // mid-line
    editor_toggle_line_comment(&mut buf, &mut cur);
    assert_eq!(buf, "-- select 1");
    // Cursor shifts right by 3 (for `-- `).
    assert_eq!(cur, 6);
}

#[test]
fn editor_toggle_line_comment_removes_marker_on_second_press() {
    let mut buf = String::from("-- select 1");
    let mut cur = 6;
    editor_toggle_line_comment(&mut buf, &mut cur);
    assert_eq!(buf, "select 1");
    assert_eq!(cur, 3);
}

#[test]
fn editor_toggle_line_comment_handles_lines_with_no_trailing_space() {
    // `--select` (no space) — remove just 2 chars.
    let mut buf = String::from("--select 1");
    let mut cur = 4;
    editor_toggle_line_comment(&mut buf, &mut cur);
    assert_eq!(buf, "select 1");
    assert_eq!(cur, 2);
}

#[test]
fn editor_toggle_line_comment_operates_per_line_in_multiline_buffer() {
    let mut buf = String::from("select 1;\nselect 2;");
    // Cursor in the second line.
    let mut cur = "select 1;\n".len() + 2;
    editor_toggle_line_comment(&mut buf, &mut cur);
    assert_eq!(buf, "select 1;\n-- select 2;");
    // Cursor shifted right by 3 in the second line.
    assert_eq!(cur, "select 1;\n-- se".len());
}

#[test]
fn editor_insert_advances_cursor_by_utf8_length() {
    let mut buf = String::from("ab");
    let mut cur = 1;
    editor_insert(&mut buf, &mut cur, 'X');
    assert_eq!(buf, "aXb");
    assert_eq!(cur, 2);

    // Multi-byte char: 'é' is 2 bytes.
    editor_insert(&mut buf, &mut cur, 'é');
    assert_eq!(buf, "aXéb");
    assert_eq!(cur, 4);
}

#[test]
fn editor_backspace_steps_to_a_char_boundary() {
    let mut buf = String::from("aé"); // a=1 byte, é=2 bytes
    let mut cur = buf.len(); // 3
    editor_backspace(&mut buf, &mut cur);
    assert_eq!(buf, "a");
    assert_eq!(cur, 1);
    editor_backspace(&mut buf, &mut cur);
    assert_eq!(buf, "");
    assert_eq!(cur, 0);
    // Backspace at start is a no-op.
    editor_backspace(&mut buf, &mut cur);
    assert_eq!(cur, 0);
}

#[test]
fn editor_delete_steps_to_a_char_boundary() {
    let mut buf = String::from("éb"); // é=2 bytes, b=1
    let mut cur = 0;
    editor_delete(&mut buf, &mut cur);
    assert_eq!(buf, "b");
    assert_eq!(cur, 0);
    // Delete at end is a no-op.
    let mut buf = String::from("ab");
    let mut cur = 2;
    editor_delete(&mut buf, &mut cur);
    assert_eq!(buf, "ab");
    assert_eq!(cur, 2);
}

#[test]
fn editor_move_left_and_right_respect_utf8_boundaries() {
    let buf = String::from("aéb"); // bytes: a(1), é(2), b(1) = 4 bytes
    let mut cur = buf.len();
    editor_move_left(&buf, &mut cur);
    assert_eq!(cur, 3); // before 'b'
    editor_move_left(&buf, &mut cur);
    assert_eq!(cur, 1); // before 'é'
    editor_move_left(&buf, &mut cur);
    assert_eq!(cur, 0);
    editor_move_left(&buf, &mut cur);
    assert_eq!(cur, 0); // saturates
    editor_move_right(&buf, &mut cur);
    assert_eq!(cur, 1);
    editor_move_right(&buf, &mut cur);
    assert_eq!(cur, 3); // past 'é'
}

#[test]
fn cursor_position_walks_newlines() {
    assert_eq!(cursor_position("hello", 3), (0, 3));
    let buf = "abc\nde\nf";
    assert_eq!(cursor_position(buf, 0), (0, 0));
    assert_eq!(cursor_position(buf, 3), (0, 3));
    assert_eq!(cursor_position(buf, 4), (1, 0));
    assert_eq!(cursor_position(buf, 6), (1, 2));
    assert_eq!(cursor_position(buf, 7), (2, 0));
    assert_eq!(cursor_position(buf, 8), (2, 1));
}

#[test]
fn cursor_display_position_sums_glyph_widths_not_chars() {
    // "SELECT '" is 8 columns; each CJK glyph paints two.
    let buf = "SELECT '東京都'";
    assert_eq!(cursor_position(buf, buf.len()), (0, 12), "chars");
    assert_eq!(cursor_display_position(buf, buf.len()), (0, 15), "columns");
    // Mid-glyph run: after 東 (8 + 2).
    let after_first = "SELECT '東".len();
    assert_eq!(cursor_display_position(buf, after_first), (0, 10));
    // Lines reset the column; ASCII is one per char either way.
    let two = "東京
ab";
    assert_eq!(cursor_display_position(two, two.len()), (1, 2));
    assert_eq!(cursor_display_position("abc", 2), cursor_position("abc", 2));
    // Past the end clamps to the end rather than panicking.
    assert_eq!(cursor_display_position(buf, 999), (0, 15));
}

#[test]
fn byte_offset_at_line_col_clamps_past_line_end() {
    let buf = "abc\nde\nf";
    assert_eq!(byte_offset_at_line_col(buf, 0, 0), 0);
    assert_eq!(byte_offset_at_line_col(buf, 0, 3), 3);
    assert_eq!(byte_offset_at_line_col(buf, 1, 0), 4);
    assert_eq!(byte_offset_at_line_col(buf, 1, 99), 6); // clamps to line end
    assert_eq!(byte_offset_at_line_col(buf, 2, 0), 7);
    assert_eq!(byte_offset_at_line_col(buf, 5, 0), 8); // line out of range
}

#[test]
fn editor_move_up_down_track_preferred_column() {
    let buf = String::from("abc\nde\nfgh");
    // Start at end of "fgh" (line 2, col 3).
    let mut cur = buf.len();
    let mut pref = None;
    editor_move_up(&buf, &mut cur, &mut pref);
    // Line 1 is "de" — only 2 cols, so cursor clamps to its end.
    assert_eq!(cur, 6);
    assert_eq!(pref, Some(3));
    editor_move_up(&buf, &mut cur, &mut pref);
    // Line 0 is "abc" — 3 cols, preferred 3 lands at the end.
    assert_eq!(cur, 3);
    editor_move_down(&buf, &mut cur, &mut pref);
    assert_eq!(cur, 6); // back to "de" end, preferred still 3
    editor_move_down(&buf, &mut cur, &mut pref);
    assert_eq!(cur, buf.len()); // "fgh" end (col 3)
    editor_move_down(&buf, &mut cur, &mut pref);
    assert_eq!(cur, buf.len()); // no further down — no change
}

#[test]
fn line_start_and_end_bytes_find_line_edges() {
    let buf = "abc\nde\nf";
    // cursor in the middle of "de" (byte 5)
    assert_eq!(line_start_byte(buf, 5), 4);
    assert_eq!(line_end_byte(buf, 5), 6);
    // cursor on line 0
    assert_eq!(line_start_byte(buf, 2), 0);
    assert_eq!(line_end_byte(buf, 2), 3);
    // cursor at last char
    assert_eq!(line_start_byte(buf, 8), 7);
    assert_eq!(line_end_byte(buf, 8), 8);
}

// ---- editor_complete (UI glue) -----------------------------------------

use crate::query::schema::{SchemaCache, TableMeta};
use crate::safety::SafetyConfig;
use crate::theme::Theme;

fn test_app_with_cache(tables: &[(&str, &[&str])]) -> App {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let mut cache = SchemaCache::default();
    for (name, cols) in tables {
        cache.tables.push(TableMeta {
            schema: "public".into(),
            name: (*name).into(),
        });
        cache.columns_by_table.insert(
            ("public".into(), (*name).into()),
            cols.iter().map(|s| s.to_string()).collect(),
        );
    }
    cache.schemas.push("public".into());
    a.schema_cache = cache;
    a
}

fn set_editor(a: &mut App, text: &str) {
    a.editor.buffer = text.into();
    a.editor.cursor = a.editor.buffer.len();
}

#[test]
fn exact_match_commits_and_dismisses_popup() {
    // Cache has `user`, `users`, `user_logs`. Operator types
    // `FROM user` and Tab: the exact match commits, no popup.
    let mut a = test_app_with_cache(&[
        ("user", &["id"]),
        ("users", &["id"]),
        ("user_logs", &["id"]),
    ]);
    set_editor(&mut a, "SELECT * FROM user");
    a.editor_complete();
    assert_eq!(a.editor.buffer, "SELECT * FROM user");
    assert!(
        a.completion.is_none(),
        "exact match should dismiss the popup; got {:?}",
        a.completion.as_ref().map(|c| c.candidates.len())
    );
}

#[test]
fn exact_match_is_case_insensitive_and_canonicalises_case() {
    // Operator typed `USERS`, cache has `users`, plus a sibling that
    // doesn't share the LCP `users` so the exact-match branch (not
    // single-match) is exercised.
    let mut a = test_app_with_cache(&[("users", &["id"]), ("users_archived", &["id"])]);
    set_editor(&mut a, "SELECT * FROM USERS");
    a.editor_complete();
    assert_eq!(a.editor.buffer, "SELECT * FROM users");
    assert!(
        a.completion.is_none(),
        "exact match should dismiss the popup"
    );
}

fn type_key(a: &mut App, code: KeyCode) {
    a.on_editor_key(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn auto_trigger_after_dot_opens_popup() {
    // Two columns means LCP-popup, no auto-commit — perfect for
    // checking the auto-trigger actually opens the cycle.
    let mut a = test_app_with_cache(&[("users", &["id", "email"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT  FROM users u WHERE u");
    a.editor.cursor = 7; // between the two spaces, no cycle yet
                         // Move the cursor to just after `u` of `u WHERE u` — actually,
                         // type `.` at end (cursor positioned after the second `u`).
    a.editor.cursor = a.editor.buffer.len();
    type_key(&mut a, KeyCode::Char('.'));
    assert_eq!(a.editor.buffer, "SELECT  FROM users u WHERE u.");
    let cycle = a
        .completion
        .as_ref()
        .expect("auto-trigger should open a cycle after typing `.` post-identifier");
    // Columns of users via alias u.
    assert!(
        cycle.candidates.iter().any(|c| c.display == "email"),
        "expected `email` in candidates, got {:?}",
        cycle
            .candidates
            .iter()
            .map(|c| &c.display)
            .collect::<Vec<_>>()
    );
}

#[test]
fn auto_trigger_skipped_for_numeric_literals() {
    // `3.` — the char before `.` is a digit, so auto-trigger is
    // suppressed. (No popup; status preserved.)
    let mut a = test_app_with_cache(&[("users", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT 3");
    a.last_status = Some("preserved status".into());
    type_key(&mut a, KeyCode::Char('.'));
    assert_eq!(a.editor.buffer, "SELECT 3.");
    assert!(
        a.completion.is_none(),
        "should not auto-trigger on numeric `3.`"
    );
    assert_eq!(a.last_status.as_deref(), Some("preserved status"));
}

#[test]
fn dot_after_lcp_expansion_narrows_via_refresh_not_auto_trigger() {
    // Operator types `t_` Tab (expands LCP to `t_user_`), then
    // narrows by typing more chars. If they happen to type `.`
    // (unlikely but possible if a name has a `.`-shaped suffix
    // in some dialect), the live-narrowing path takes precedence
    // over auto-trigger — the existing cycle stays alive.
    let mut a = test_app_with_cache(&[("t_user_logs", &["id"]), ("t_user_roles", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT * FROM t_");
    // First Tab: LCP-expands to `t_user_`, popup with 2 candidates.
    a.editor_complete();
    assert_eq!(a.editor.buffer, "SELECT * FROM t_user_");
    assert!(a.completion.as_ref().unwrap().selected.is_none());
    let cycle_id_before = a.completion.as_ref().unwrap() as *const _;
    // Now type `l` — narrowing key, cycle survives via refresh.
    type_key(&mut a, KeyCode::Char('l'));
    assert!(a.completion.is_some(), "cycle should still be alive");
    let cycle = a.completion.as_ref().unwrap();
    // The cycle was rebuilt (new pointer), but selected is still
    // None (refresh keeps pre-selection state).
    let _ = cycle_id_before; // (we don't actually compare pointers; reassuring no panic)
    assert!(cycle.selected.is_none());
    assert!(cycle.candidates.iter().any(|c| c.display == "t_user_logs"));
}

#[test]
fn undo_after_tab_completion_does_not_panic_on_next_tab() {
    // Regression: Tab arms a completion cycle whose byte offsets index the
    // GROWN buffer; Ctrl-Z restored the shorter pre-Tab buffer but left the
    // cycle populated, so the next Tab's `replace_range(start..end)` ran past
    // the buffer end and panicked, killing the TUI. Undo must drop the cycle.
    let mut a = test_app_with_cache(&[("t_user_logs", &["id"]), ("t_user_roles", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT * FROM t_");
    // Tab: LCP-expands `t_` → `t_user_` and arms a cycle.
    a.on_key(KeyEvent::from(KeyCode::Tab));
    assert!(a.completion.is_some(), "Tab should arm a completion cycle");
    assert_eq!(a.editor.buffer, "SELECT * FROM t_user_");
    // Ctrl-Z: restores the shorter buffer — and must clear the cycle.
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "SELECT * FROM t_");
    assert!(
        a.completion.is_none(),
        "undo must clear the stale completion cycle"
    );
    // The previously-panicking second Tab now just re-arms a fresh cycle.
    a.on_key(KeyEvent::from(KeyCode::Tab));
    assert_eq!(a.editor.buffer, "SELECT * FROM t_user_");
}

#[test]
fn reset_grid_view_clears_source_and_bookmarks() {
    // Regression: a stale grid_view.source survived a reconnect, so `I`
    // (row→INSERT) built SQL against the PREVIOUS database's table; and
    // bookmarks keyed by row index resolved against an unrelated grid.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid_view.source = Some(("public".into(), "orders".into()));
    a.bookmarks.insert('a', GridBookmark { row: 3, col: 1 });
    a.reset_grid_view();
    assert!(
        a.grid_view.source.is_none(),
        "source must clear when a new grid lands"
    );
    assert!(
        a.bookmarks.is_empty(),
        "bookmarks must clear when a new grid lands"
    );
}

#[test]
fn auto_trigger_no_matches_preserves_status() {
    // `nonsense.` — no such identifier; auto-trigger fires but
    // finds nothing and silently restores the prior status.
    let mut a = test_app_with_cache(&[("users", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT nonsense");
    a.last_status = Some("preserved status".into());
    type_key(&mut a, KeyCode::Char('.'));
    assert!(a.completion.is_none());
    assert_eq!(a.last_status.as_deref(), Some("preserved status"));
}

#[test]
fn tab_on_empty_buffer_offers_statement_keywords() {
    let mut a = test_app_with_cache(&[("users", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "");
    a.editor_complete();
    let cycle = a
        .completion
        .as_ref()
        .expect("Tab on empty buffer should offer statement keywords");
    let labels: Vec<&str> = cycle
        .candidates
        .iter()
        .map(|c| c.display.as_str())
        .collect();
    assert!(
        labels.iter().any(|l| l.eq_ignore_ascii_case("SELECT")),
        "got {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.eq_ignore_ascii_case("INSERT")),
        "got {labels:?}"
    );
}

#[test]
fn tab_after_from_space_offers_tables() {
    let mut a = test_app_with_cache(&[("users", &["id"]), ("orders", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT * FROM ");
    a.editor_complete();
    let cycle = a
        .completion
        .as_ref()
        .expect("Tab after `FROM ` should open tables popup");
    let labels: Vec<&str> = cycle
        .candidates
        .iter()
        .map(|c| c.display.as_str())
        .collect();
    assert!(labels.contains(&"users"), "got {labels:?}");
    assert!(labels.contains(&"orders"), "got {labels:?}");
}

#[test]
fn auto_trigger_after_from_space_opens_tables() {
    let mut a = test_app_with_cache(&[("users", &["id"]), ("orders", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT * FROM");
    type_key(&mut a, KeyCode::Char(' '));
    assert_eq!(a.editor.buffer, "SELECT * FROM ");
    let cycle = a
        .completion
        .as_ref()
        .expect("auto-trigger should pop after `FROM `");
    let labels: Vec<&str> = cycle
        .candidates
        .iter()
        .map(|c| c.display.as_str())
        .collect();
    assert!(labels.contains(&"users"));
    assert!(labels.contains(&"orders"));
}

#[test]
fn auto_trigger_after_and_space_opens_columns() {
    let mut a = test_app_with_cache(&[("users", &["id", "email", "name"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT * FROM users WHERE id = 1 AND");
    type_key(&mut a, KeyCode::Char(' '));
    let cycle = a
        .completion
        .as_ref()
        .expect("auto-trigger should pop after `AND `");
    let labels: Vec<&str> = cycle
        .candidates
        .iter()
        .map(|c| c.display.as_str())
        .collect();
    assert!(labels.iter().any(|l| *l == "email" || *l == "name"));
}

#[test]
fn auto_trigger_after_space_does_not_panic_on_multibyte_boundary_char() {
    // Regression guard: rfind on a predicate that matches a
    // multi-byte char (smart quote, en-dash, NBSP, …) would return
    // the byte index of the char's FIRST byte; `i + 1` then lands
    // in the middle of the codepoint and `&trimmed[start..]`
    // panicked. Walk char_indices.rev() instead.
    let mut a = test_app_with_cache(&[("users", &["id"])]);
    a.mode = Mode::Editor;
    // En-dash (U+2013, 3 bytes) followed by an identifier-shaped
    // word — the en-dash is the closest non-alphanumeric / non-`_`
    // char to the right of the would-be word start.
    set_editor(&mut a, "–FROM");
    // Just typing the space — we don't actually need it to fire
    // the trigger, only to not panic walking back over the en-dash.
    type_key(&mut a, KeyCode::Char(' '));
    // FROM is in the trigger list, so the popup should also open.
    assert!(
        a.completion.is_some(),
        "expected popup to open after `–FROM ` (en-dash + FROM); no panic is the main thing"
    );
}

#[test]
fn auto_trigger_does_not_fire_after_arbitrary_space() {
    // After typing `5 ` (a literal followed by space), the auto-
    // trigger should NOT fire — operator is probably mid-expression
    // and a popup would be noise.
    let mut a = test_app_with_cache(&[("users", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT * FROM users WHERE id = 5");
    a.last_status = Some("preserved status".into());
    type_key(&mut a, KeyCode::Char(' '));
    assert!(
        a.completion.is_none(),
        "auto-trigger should be silent after `5 `"
    );
    assert_eq!(a.last_status.as_deref(), Some("preserved status"));
}

#[test]
fn exact_match_with_only_one_candidate_still_dismisses_popup() {
    // Cache has just `users`. Operator types `FROM users` Tab.
    // cands.len() == 1, but the single-match path must NOT shadow
    // exact-match — the popup should go away because the operator
    // typed the full name.
    let mut a = test_app_with_cache(&[("users", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT * FROM users");
    a.editor_complete();
    assert_eq!(a.editor.buffer, "SELECT * FROM users");
    assert!(
        a.completion.is_none(),
        "exact match must dismiss the popup even when it's the only candidate"
    );
}

#[test]
fn empty_unqualified_prefix_with_single_candidate_shows_popup_no_insert() {
    // Construct a context where empty-prefix completion yields a
    // single candidate. We can't easily get cands.len() == 1 in a
    // normal clause (the classifier extends with continuations) so
    // this is a "doesn't auto-insert" sanity check at the API
    // level: Tab on whitespace in a clean buffer offers statement
    // keywords (multiple cands), and Tab on `SELECT ` offers
    // multiple. So the property we actually want is that the
    // popup opens with selected: None — operator decides.
    let mut a = test_app_with_cache(&[("users", &["id"])]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT * FROM ");
    a.editor_complete();
    let cycle = a
        .completion
        .as_ref()
        .expect("popup should open on whitespace-Tab");
    assert!(
        cycle.selected.is_none(),
        "empty unqualified prefix must not pre-select; got {:?}",
        cycle.selected
    );
    // Buffer unchanged — no silent insert.
    assert_eq!(a.editor.buffer, "SELECT * FROM ");
}

// Note: auto-trigger after `.` for non-ASCII identifier endings
// (e.g. `café.`) would benefit from the char-aware lookup in
// `on_editor_key`, but `extract_identifier` itself walks back
// byte-by-byte and rejects non-ASCII suffixes — so end-to-end
// non-ASCII identifier completion is gated on widening the
// tokenizer in a follow-up. The char-aware check here is kept
// defensively so the auto-trigger path is correct from day one.

#[test]
fn backspace_to_empty_prefix_keeps_context_popup() {
    // `FROM us` Tab → popup with users-ish tables. Then the operator
    // backspaces both chars. We should NOT drop the cycle — instead
    // refresh re-extracts (empty prefix) and offers the full
    // table list for the FROM context.
    let mut a = test_app_with_cache(&[
        ("users", &["id"]),
        ("user_logs", &["id"]),
        ("orders", &["id"]),
    ]);
    a.mode = Mode::Editor;
    set_editor(&mut a, "SELECT * FROM us");
    a.editor_complete(); // LCP-expands to `user_`
                         // Backspace through the entire identifier so the prefix
                         // narrows to empty — the cycle should survive and broaden
                         // back to the full table list for FROM.
                         // After LCP-expand, buffer is `SELECT * FROM user` (the LCP
                         // of users / user_logs is `user`, not `user_` — they diverge
                         // at the 5th char). Four backspaces brings us to the trailing
                         // space — empty prefix, still in TableRef context.
    for _ in 0..4 {
        type_key(&mut a, KeyCode::Backspace);
    }
    // Buffer should now be "SELECT * FROM " (or a substring thereof);
    // cycle should still be alive and offering tables (incl. orders).
    let cycle = a
        .completion
        .as_ref()
        .expect("cycle should survive narrowing to empty prefix");
    let labels: Vec<&str> = cycle
        .candidates
        .iter()
        .map(|c| c.display.as_str())
        .collect();
    assert!(
        labels.contains(&"orders"),
        "after narrowing to empty prefix, all tables should be offered; got {labels:?}"
    );
}

#[test]
fn tab_with_no_candidates_falls_back_to_helpful_message() {
    // Disconnected (no cache), empty buffer: there ARE statement
    // keywords available, so the empty-cache message should NOT
    // fire — the popup opens with keywords.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor_complete();
    assert!(
        a.completion.is_some(),
        "empty cache + empty buffer should still offer keywords"
    );
}

#[test]
fn lcp_expands_when_no_exact_match() {
    // Two tables, `user_logs` and `user_roles`. Typing `user` Tab
    // expands to the LCP `user_` (no exact match to short-circuit).
    let mut a = test_app_with_cache(&[("user_logs", &["id"]), ("user_roles", &["id"])]);
    set_editor(&mut a, "SELECT * FROM user");
    a.editor_complete();
    assert_eq!(a.editor.buffer, "SELECT * FROM user_");
    // Cycle is in the LCP-expanded state — popup visible, nothing
    // selected yet.
    let cycle = a.completion.as_ref().expect("cycle should be alive");
    assert!(cycle.selected.is_none());
    assert_eq!(cycle.candidates.len(), 2);
}

#[test]
fn query_failed_with_position_jumps_editor_cursor() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    // Buffer with multibyte char before the position so we
    // exercise char→byte conversion. `é` is 2 bytes; `id` is at
    // chars 8..10. Postgres positions are 1-indexed chars, so
    // position 9 points at `d`.
    a.editor.buffer = "SELECT é, id FROM t".into();
    a.editor.cursor = 0;
    a.generation = 1;
    let _ = a.msg_tx.send(AppMsg::QueryFailed {
        generation: 1,
        error: "ERROR: column \"d\" does not exist".into(),
        position: Some(9),
        detail: None,
    });
    // Pump the single queued message.
    if let Some(rx) = a.msg_rx.as_mut() {
        if let Ok(msg) = rx.try_recv() {
            a.on_msg(msg);
        }
    }
    // Position 9 (1-indexed char) → 0-indexed char 8. Byte
    // offset of char 8 in "SELECT é, id FROM t" — chars are
    // S(1) E(1) L(1) E(1) C(1) T(1) space(1) é(2)... so char 8
    // is `,` at byte 9.
    assert_eq!(a.editor.cursor, 9, "cursor should land at byte 9");
    assert!(a.last_error.as_deref().unwrap().contains("does not exist"));
}

#[test]
fn history_search_ctrl_d_deletes_focused_entry() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.history = vec![
        "SELECT * FROM users".into(),
        "DELETE FROM tmp WHERE secret = 'abc123'".into(),
        "SELECT count(*) FROM orders".into(),
    ];
    a.mode = Mode::Editor;
    a.start_history_search();
    // Type 'secret' — narrows to the leak entry (index 1).
    for c in "secret".chars() {
        a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(a.editor.buffer.contains("secret"));
    // Ctrl-D deletes it.
    a.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(a.history.len(), 2);
    assert!(!a.history.iter().any(|e| e.contains("secret")));
}

#[test]
fn history_search_finds_most_recent_match_and_walks_older() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.history = vec![
        "SELECT * FROM users".into(),
        "INSERT INTO logs VALUES (1)".into(),
        "SELECT count(*) FROM orders".into(),
        "UPDATE users SET active=true".into(),
    ];
    a.mode = Mode::Editor;
    a.editor.buffer = "draft".into();
    a.editor.cursor = a.editor.buffer.len();
    a.start_history_search();
    // Empty query → most-recent entry shown.
    assert_eq!(a.mode, Mode::HistorySearch);
    assert_eq!(a.editor.buffer, "UPDATE users SET active=true");
    // Type 'sel' through on_key so the mode dispatcher routes
    // each keystroke to the history-search handler.
    a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    a.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    a.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    assert_eq!(a.editor.buffer, "SELECT count(*) FROM orders");
    // Ctrl-R again → next-older match (index 0, `SELECT * FROM users`).
    a.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "SELECT * FROM users");
    // Enter accepts: stays in Editor with the matched buffer.
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor.buffer, "SELECT * FROM users");
    assert!(a.history_search.is_none());
}

#[test]
fn history_search_esc_restores_pre_search_buffer() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.history = vec!["SELECT 1".into()];
    a.mode = Mode::Editor;
    a.editor.buffer = "draft in progress".into();
    a.editor.cursor = 5;
    a.start_history_search();
    assert_eq!(a.editor.buffer, "SELECT 1");
    // Esc: restore.
    a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(a.editor.buffer, "draft in progress");
    assert_eq!(a.editor.cursor, 5);
    assert_eq!(a.mode, Mode::Editor);
}

#[test]
fn history_search_no_match_keeps_last_good_buffer() {
    // bash-like behaviour: a typo after a successful match keeps
    // the prior match on screen and surfaces the failure in status.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.history = vec!["SELECT * FROM users".into()];
    a.mode = Mode::Editor;
    a.start_history_search();
    // 'sel' matches → buffer = SELECT...
    a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    a.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    a.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    assert_eq!(a.editor.buffer, "SELECT * FROM users");
    // 'selz' doesn't match → buffer unchanged; status flags failure.
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert_eq!(a.editor.buffer, "SELECT * FROM users");
    assert!(
        a.last_status
            .as_deref()
            .unwrap_or("")
            .contains("failed reverse-i-search"),
        "expected failure status, got {:?}",
        a.last_status
    );
}

#[test]
fn start_watch_uses_editor_buffer_when_set() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT NOW()".into();
    a.start_watch();
    let w = a.watch.as_ref().expect("watch should be set");
    assert_eq!(w.sql, "SELECT NOW()");
    assert_eq!(w.interval.as_secs(), 2);
}

#[test]
fn start_watch_falls_back_to_last_history_when_buffer_empty() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.history = vec!["SELECT 1".into(), "SELECT count(*) FROM t".into()];
    a.mode = Mode::Editor;
    a.start_watch();
    let w = a.watch.as_ref().expect("watch should be set");
    assert_eq!(w.sql, "SELECT count(*) FROM t");
}

#[test]
fn start_watch_with_no_input_errors() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.start_watch();
    assert!(a.watch.is_none());
    assert!(a.last_error.is_some());
}

#[test]
fn start_watch_refused_during_query() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.editor.buffer = "SELECT 1".into();
    a.query_running = true;
    a.start_watch();
    assert!(a.watch.is_none());
}

#[test]
fn keypress_cancels_active_watch() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.watch = Some(WatchState {
        sql: "SELECT 1".into(),
        interval: std::time::Duration::from_secs(2),
        last_fire: std::time::Instant::now(),
    });
    a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    assert!(a.watch.is_none());
}

#[test]
fn split_editor_command_handles_program_with_args() {
    let (p, a) = split_editor_command("code --wait --new-window");
    assert_eq!(p, "code");
    assert_eq!(a, vec!["--wait", "--new-window"]);
}

#[test]
fn split_editor_command_handles_bare_program() {
    let (p, a) = split_editor_command("nvim");
    assert_eq!(p, "nvim");
    assert!(a.is_empty());
}

#[test]
fn split_editor_command_defaults_to_vi_on_empty() {
    let (p, a) = split_editor_command("");
    assert_eq!(p, "vi");
    assert!(a.is_empty());
}

#[test]
fn split_editor_command_collapses_internal_whitespace() {
    let (p, a) = split_editor_command("  emacs   -nw  ");
    assert_eq!(p, "emacs");
    assert_eq!(a, vec!["-nw"]);
}

// ---- pure decision functions ----

#[test]
fn watch_should_fire_respects_interval_and_blockers() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let state = WatchState {
        sql: "SELECT 1".into(),
        interval: Duration::from_secs(2),
        last_fire: now,
    };
    let clear = WatchTickInputs {
        query_running: false,
        tx_open: false,
        pending_run: false,
        mode_blocks: false,
    };
    // Same instant → interval not elapsed.
    assert!(!watch_should_fire(&state, now, clear));
    // Just past the interval → fire.
    assert!(watch_should_fire(
        &state,
        now + Duration::from_secs(2),
        clear
    ));
    // Any blocker prevents fire even past the interval.
    let fire_time = now + Duration::from_secs(10);
    for inputs in [
        WatchTickInputs {
            query_running: true,
            ..clear
        },
        WatchTickInputs {
            tx_open: true,
            ..clear
        },
        WatchTickInputs {
            pending_run: true,
            ..clear
        },
        WatchTickInputs {
            mode_blocks: true,
            ..clear
        },
    ] {
        assert!(
            !watch_should_fire(&state, fire_time, inputs),
            "blocker {inputs:?} should suppress fire"
        );
    }
}

#[test]
fn next_sort_state_cycles_through_target_column() {
    assert_eq!(next_sort_state(None, 3), Some((3, true)));
    assert_eq!(next_sort_state(Some((3, true)), 3), Some((3, false)));
    assert_eq!(next_sort_state(Some((3, false)), 3), None);
    // Different column → jump to ASC on the new one.
    assert_eq!(next_sort_state(Some((3, true)), 5), Some((5, true)));
    assert_eq!(next_sort_state(Some((3, false)), 5), Some((5, true)));
}

#[test]
fn compute_visible_rows_filters_case_insensitively_across_columns() {
    let rows = vec![
        vec!["1".into(), "alice".into()],
        vec!["2".into(), "BOB".into()],
        vec!["3".into(), "carol".into()],
    ];
    // No filter → all rows in order.
    assert_eq!(compute_visible_rows(&rows, None), vec![0, 1, 2]);
    // Match in column 1, case-insensitive.
    assert_eq!(compute_visible_rows(&rows, Some("bo")), vec![1]);
    // Match in column 0 (numeric column).
    assert_eq!(compute_visible_rows(&rows, Some("3")), vec![2]);
    // No matches.
    assert!(compute_visible_rows(&rows, Some("xyz")).is_empty());
}

#[test]
fn history_search_next_walks_backward_case_insensitive() {
    let history: Vec<String> = vec![
        "SELECT 1".into(),
        "INSERT INTO logs VALUES (1)".into(),
        "SELECT * FROM users".into(),
        "UPDATE accounts SET balance=0".into(),
    ];
    // From end, "sel" finds idx 2 (most recent SELECT).
    assert_eq!(history_search_next(&history, "sel", None), Some(2));
    // From before that match, finds idx 0.
    assert_eq!(history_search_next(&history, "sel", Some(2)), Some(0));
    // Past the earliest match → None.
    assert_eq!(history_search_next(&history, "sel", Some(0)), None);
    // Case-insensitive match.
    assert_eq!(history_search_next(&history, "INSERT", None), Some(1));
    assert_eq!(history_search_next(&history, "insert", None), Some(1));
}

#[test]
fn splash_should_dismiss_picker_landing_dismisses_even_while_disconnected() {
    use std::time::Instant;
    let t0 = Instant::now();
    let until = Some(t0 + SPLASH_MIN);
    // Picker landing: `conn_state` is `Disconnected` (never resolves on
    // its own), but the picker is what the operator needs — dismiss
    // regardless, both at the minimum...
    assert!(splash_should_dismiss(
        true,
        until,
        false,
        true,
        t0 + SPLASH_MIN
    ));
    // ...and well before it, since the picker shouldn't force a wait
    // for a connection resolution that will never come.
    assert!(splash_should_dismiss(true, until, false, true, t0));
}

#[test]
fn splash_should_dismiss_keypress_dismisses_before_the_minimum() {
    // A keypress dismisses the splash directly (App::on_key sets
    // `splash_visible = false` unconditionally) rather than going
    // through `splash_should_dismiss` — so simulate that path here
    // and confirm the splash is down before the minimum would have
    // elapsed on its own.
    use std::time::Instant;
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let t0 = Instant::now();
    assert!(a.splash_visible);
    assert!(a.splash_until.unwrap() > t0);
    a.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(!a.splash_visible);
    assert!(a.splash_until.is_none());
    // Confirm this really did happen before the minimum — the app was
    // just constructed, so `now` is still well inside the window.
    assert!(Instant::now() < t0 + SPLASH_MIN);
}

#[test]
fn splash_should_dismiss_connecting_with_no_keypress_holds_until_the_minimum() {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let until = Some(t0 + SPLASH_MIN);
    // Connecting, not the picker: neither early-dismiss condition
    // applies, so the splash holds right up to the deadline...
    assert!(!splash_should_dismiss(
        true,
        until,
        false,
        false,
        t0 + Duration::from_millis(1),
    ));
    // ...and dismisses once the minimum has elapsed.
    assert!(splash_should_dismiss(
        true,
        until,
        false,
        false,
        t0 + SPLASH_MIN
    ));
}

fn app_with_grid(grid: Grid) -> App {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = grid;
    a.reset_grid_view();
    a.grid_state
        .select(if a.grid.is_empty() { None } else { Some(0) });
    a
}

fn sample_grid() -> Grid {
    Grid {
        columns: vec!["id".into(), "name".into()],
        rows: vec![
            vec!["3".into(), "carol".into()],
            vec!["1".into(), "alice".into()],
            vec!["10".into(), "bob".into()],
            vec!["2".into(), "dave".into()],
        ],
        truncated: false,
    }
}

fn grid_of(columns: &[&str], rows: &[&[&str]]) -> Grid {
    Grid {
        columns: columns.iter().map(|s| s.to_string()).collect(),
        rows: rows
            .iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect(),
        truncated: false,
    }
}

#[test]
fn result_diff_d_with_empty_grid_is_a_status_not_an_error() {
    // pgman's own "nothing to do" notice is not a failure: as an error
    // it grew a `· F2 detail` pointer to an overlay with nothing in it.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = grid_of(&["id"], &[]);
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Char('D')));
    assert!(a.result_diff.pinned.is_none());
    assert_eq!(
        a.last_status.as_deref(),
        Some("no result to diff — run a query first")
    );
    assert!(a.last_error.is_none(), "{:?}", a.last_error);
}

#[test]
fn a_client_side_refusal_after_a_server_error_drops_the_stale_sqlstate() {
    // A server error leaves its SQLSTATE for F2. A refusal pgman itself
    // produces afterwards (`:readonly off`) carries no server detail —
    // F2 on it used to open an overlay headed by the previous query's
    // `SQLSTATE 42P01` under pgman's own message.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.generation = 1;
    a.on_msg(AppMsg::QueryFailed {
        generation: 1,
        error: "relation \"t\" does not exist".into(),
        position: None,
        detail: Some(crate::conn::QueryErrDetail {
            code: Some("42P01".into()),
            severity: Some("ERROR".into()),
            ..Default::default()
        }),
    });
    assert_eq!(
        a.last_error_detail.as_ref().and_then(|d| d.code.as_deref()),
        Some("42P01"),
        "fixture check: the server error left its detail"
    );
    run_command(&mut a, "readonly off");
    let err = a.last_error.clone().unwrap_or_default();
    assert!(
        err == read_only_escape_refusal(true) || err == read_only_escape_refusal(false),
        "fixture check: the refusal landed: {err}"
    );
    assert!(
        a.last_error_detail.is_none(),
        "F2 would show the previous query's SQLSTATE under pgman's own refusal: {:?}",
        a.last_error_detail
    );
}

#[test]
fn a_failed_panel_load_does_not_haunt_the_next_panel() {
    // `T` failed (no pg_stat_statements). The error stayed in the
    // footer through every later panel open and close: `S`, its Esc,
    // and on.
    let mut a = test_app_with_cache(&[("users", &["id"])]);
    a.generation = 1;
    a.mode = Mode::SlowQueries;
    a.on_msg(AppMsg::SlowQueriesLoaded {
        generation: 1,
        result: Err(crate::conn::QueryErr::msg(
            "relation \"pg_stat_statements\" does not exist",
        )),
    });
    assert_eq!(a.mode, Mode::Normal);
    assert!(
        a.last_error.is_some(),
        "fixture check: the failed load is an error"
    );
    a.on_key(KeyEvent::from(KeyCode::Char('S')));
    assert_eq!(a.mode, Mode::SchemaBrowser);
    assert!(
        a.last_error.is_none(),
        "opening a panel must drop the previous error: {:?}",
        a.last_error
    );
}

#[test]
fn closing_a_panel_drops_the_error_left_under_it() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Sessions;
    a.last_error = Some("sessions load failed: permission denied".into());
    a.last_error_detail = Some(crate::conn::QueryErrDetail {
        code: Some("42501".into()),
        ..Default::default()
    });
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Normal);
    assert!(a.last_error.is_none(), "{:?}", a.last_error);
    assert!(a.last_error_detail.is_none());
}

#[test]
fn a_panel_load_that_works_clears_the_failure_before_it() {
    // `r` in the panel after a failed load, or `L` straight after a
    // failed `T` with the panel already open: the load landing is the
    // footer's new subject.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.generation = 1;
    a.mode = Mode::Sessions;
    a.last_error = Some("slow queries load failed: whatever".into());
    a.on_msg(AppMsg::SessionsLoaded {
        generation: 1,
        result: Ok(Vec::new()),
    });
    assert!(a.last_error.is_none(), "{:?}", a.last_error);
    assert_eq!(a.last_status.as_deref(), Some(SESSIONS_ONLY_THIS_SESSION));

    a.mode = Mode::SlowQueries;
    a.last_error = Some("sessions load failed: whatever".into());
    a.on_msg(AppMsg::SlowQueriesLoaded {
        generation: 1,
        result: Ok(Vec::new()),
    });
    assert!(a.last_error.is_none(), "{:?}", a.last_error);
}

#[test]
fn a_query_failure_survives_leaving_the_editor_and_viewing_its_detail() {
    // The clear is scoped to panel edges. Esc from the editor to Normal
    // is not one, and neither is the error-detail overlay — it *shows*
    // the error, from any mode, including a panel.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.generation = 1;
    a.mode = Mode::Editor;
    a.on_msg(AppMsg::QueryFailed {
        generation: 1,
        error: "relation \"t\" does not exist".into(),
        position: None,
        detail: Some(crate::conn::QueryErrDetail {
            code: Some("42P01".into()),
            ..Default::default()
        }),
    });
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Normal);
    assert!(
        a.last_error.is_some(),
        "Esc from the editor is not a panel edge"
    );

    a.mode = Mode::Sessions;
    a.on_key(KeyEvent::from(KeyCode::F(2)));
    assert_eq!(a.mode, Mode::ErrorDetail);
    assert!(
        a.last_error.is_some(),
        "F2 from a panel shows the error, not drops it"
    );
    assert_eq!(
        a.last_error_detail.as_ref().and_then(|d| d.code.as_deref()),
        Some("42P01")
    );
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Sessions);
    assert!(
        a.last_error.is_some(),
        "closing the overlay returns to the panel with the error"
    );
}

#[test]
fn is_panel_separates_panels_from_base_modes_modals_and_prompts() {
    for m in [
        Mode::Sessions,
        Mode::SlowQueries,
        Mode::SchemaLint,
        Mode::SchemaBrowser,
        Mode::Notifications,
        Mode::SavedQueries,
        Mode::Help,
    ] {
        assert!(is_panel(m), "{m:?}");
    }
    for m in [
        Mode::Normal,
        Mode::Editor,
        Mode::ErrorDetail,
        Mode::Confirm,
        Mode::TxDecision,
        Mode::ConfirmTerminate,
        Mode::CommandBar,
        Mode::SaveQueryPrompt,
    ] {
        assert!(!is_panel(m), "{m:?}");
    }
}

#[test]
fn a_key_that_leaves_the_error_alone_keeps_its_sqlstate() {
    // The clear is keyed on the error *changing*: navigating around a
    // server error must not strip the detail F2 is about to show.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.generation = 1;
    a.on_msg(AppMsg::QueryFailed {
        generation: 1,
        error: "relation \"t\" does not exist".into(),
        position: None,
        detail: Some(crate::conn::QueryErrDetail {
            code: Some("42P01".into()),
            ..Default::default()
        }),
    });
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(
        a.last_error_detail.as_ref().and_then(|d| d.code.as_deref()),
        Some("42P01")
    );
    a.on_key(KeyEvent::from(KeyCode::F(2)));
    assert_eq!(a.mode, Mode::ErrorDetail);
}

#[test]
fn result_diff_first_d_pins_baseline() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = sample_grid();
    a.pin_or_diff_result();
    let p = a.result_diff.pinned.as_ref().expect("baseline pinned");
    assert_eq!(p.rows.len(), 4);
    assert_eq!(p.columns, vec!["id".to_string(), "name".to_string()]);
    // Pinning alone doesn't open the diff view.
    assert_eq!(a.mode, Mode::Normal);
    assert!(a.result_diff.active.is_none());
    assert!(a
        .last_status
        .as_deref()
        .unwrap_or("")
        .contains("pinned result A"));
}

#[test]
fn result_diff_second_d_opens_diff_with_inferred_key() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = sample_grid(); // ids 3,1,10,2
    a.pin_or_diff_result(); // pins A
                            // B: id 3 renamed, id 2 removed, id 99 added; 1 and 10 unchanged.
    a.grid = grid_of(
        &["id", "name"],
        &[
            &["3", "CAROL"],
            &["1", "alice"],
            &["10", "bob"],
            &["99", "new"],
        ],
    );
    a.pin_or_diff_result(); // diffs
    assert_eq!(a.mode, Mode::ResultDiff);
    let d = a.result_diff.active.as_ref().expect("diff computed");
    // id column (0) is unique on both sides → strong key.
    assert!(matches!(
        &d.key,
        crate::query::row_diff::RowKey::Columns(c) if c == &vec![0]
    ));
    assert_eq!(d.diff.changed.len(), 1, "id 3 name changed");
    assert_eq!(d.diff.removed.len(), 1, "id 2 gone");
    assert_eq!(d.diff.added.len(), 1, "id 99 new");
    assert_eq!(d.diff.unchanged, 2, "ids 1 and 10");
    // Baseline persists for iterative diffing.
    assert!(a.result_diff.pinned.is_some());
}

#[test]
fn result_diff_falls_back_to_full_row_when_columns_differ() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = grid_of(&["id", "name"], &[&["1", "x"]]);
    a.pin_or_diff_result();
    // B has a different column layout — cell-level keying is unsafe.
    a.grid = grid_of(&["id", "name", "extra"], &[&["1", "x", "y"]]);
    a.pin_or_diff_result();
    let d = a.result_diff.active.as_ref().expect("diff computed");
    assert!(matches!(d.key, crate::query::row_diff::RowKey::FullRow));
}

#[test]
fn result_diff_r_repins_b_as_new_baseline() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = grid_of(&["id"], &[&["1"]]);
    a.pin_or_diff_result();
    a.grid = grid_of(&["id"], &[&["1"], &["2"]]);
    a.pin_or_diff_result();
    assert_eq!(a.mode, Mode::ResultDiff);
    a.on_key(KeyEvent::from(KeyCode::Char('r')));
    assert_eq!(a.mode, Mode::Normal);
    assert!(a.result_diff.active.is_none());
    // New baseline = the B side (two rows).
    assert_eq!(a.result_diff.pinned.as_ref().unwrap().rows.len(), 2);
}

#[test]
fn result_diff_c_clears_pin() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = grid_of(&["id"], &[&["1"]]);
    a.pin_or_diff_result();
    a.grid = grid_of(&["id"], &[&["2"]]);
    a.pin_or_diff_result();
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert!(a.result_diff.pinned.is_none());
    assert!(a.result_diff.active.is_none());
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn result_diff_d_keybinding_pins_from_normal_mode() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid = sample_grid();
    a.on_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
    assert!(a.result_diff.pinned.is_some());
}

fn saved(name: &str, body: &str) -> crate::saved::SavedQuery {
    crate::saved::SavedQuery {
        name: name.into(),
        body: body.into(),
    }
}

fn type_str(a: &mut App, s: &str) {
    for c in s.chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
}

#[test]
fn param_prompt_no_params_loads_directly() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.load_saved_query(saved("plain", "SELECT 1"));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor.buffer, "SELECT 1");
    assert!(a.saved_ui.param_prompt.is_none());
}

#[test]
fn param_prompt_with_params_enters_prompt_mode() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.load_saved_query(saved("byid", "SELECT * FROM t WHERE id = :id"));
    assert_eq!(a.mode, Mode::ParamPrompt);
    assert_eq!(
        a.saved_ui.param_prompt.as_ref().unwrap().params,
        vec!["id".to_string()]
    );
}

#[test]
fn param_prompt_collects_values_and_substitutes() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.load_saved_query(saved(
        "two",
        "SELECT * FROM t WHERE id = :id AND org = :org",
    ));
    type_str(&mut a, "42");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    // First value taken; still prompting for the second.
    assert_eq!(a.mode, Mode::ParamPrompt);
    assert_eq!(a.saved_ui.param_prompt.as_ref().unwrap().idx, 1);
    type_str(&mut a, "7");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor.buffer, "SELECT * FROM t WHERE id = 42 AND org = 7");
    assert!(a.saved_ui.param_prompt.is_none());
}

#[test]
fn param_prompt_same_param_twice_fills_both() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.load_saved_query(saved("dup", "SELECT :x WHERE a = :x"));
    // Only one prompt (distinct param), substituted everywhere.
    assert_eq!(a.saved_ui.param_prompt.as_ref().unwrap().params.len(), 1);
    type_str(&mut a, "9");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.editor.buffer, "SELECT 9 WHERE a = 9");
}

#[test]
fn param_prompt_rejects_empty_value() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.load_saved_query(saved("byid", "WHERE id = :id"));
    a.on_key(KeyEvent::from(KeyCode::Enter)); // empty
    assert_eq!(a.mode, Mode::ParamPrompt);
    assert!(a
        .last_status
        .as_deref()
        .unwrap_or("")
        .contains("value required"));
}

#[test]
fn param_prompt_esc_cancels_back_to_list() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.load_saved_query(saved("byid", "WHERE id = :id"));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::SavedQueries);
    assert!(a.saved_ui.param_prompt.is_none());
}

#[test]
fn param_prompt_backspace_edits_input() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.load_saved_query(saved("byid", "WHERE id = :id"));
    type_str(&mut a, "49");
    a.on_key(KeyEvent::from(KeyCode::Backspace));
    type_str(&mut a, "2");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.editor.buffer, "WHERE id = 42");
}

fn app_with_saved(entries: &[(&str, &str)]) -> App {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    for (n, b) in entries {
        a.saved_queries.upsert(saved(n, b));
    }
    a
}

#[test]
fn filter_saved_indices_blank_returns_all() {
    let e = vec![saved("a", "x"), saved("b", "y")];
    assert_eq!(filter_saved_indices(&e, None), vec![0, 1]);
    assert_eq!(filter_saved_indices(&e, Some("   ")), vec![0, 1]);
}

#[test]
fn filter_saved_indices_matches_name_case_insensitive() {
    let e = vec![saved("ActiveUsers", "..."), saved("revenue", "...")];
    assert_eq!(filter_saved_indices(&e, Some("active")), vec![0]);
    assert_eq!(filter_saved_indices(&e, Some("REV")), vec![1]);
}

#[test]
fn filter_saved_indices_matches_body_too() {
    let e = vec![
        saved("a", "SELECT * FROM orders"),
        saved("b", "SELECT * FROM users"),
    ];
    assert_eq!(filter_saved_indices(&e, Some("orders")), vec![0]);
}

#[test]
fn filter_saved_indices_no_match_is_empty() {
    let e = vec![saved("a", "x")];
    assert!(filter_saved_indices(&e, Some("zzz")).is_empty());
}

#[test]
fn saved_filter_narrows_live_and_maps_focus_to_real_index() {
    let mut a = app_with_saved(&[("users", "a"), ("orders", "b"), ("revenue", "c")]);
    a.open_saved_queries();
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    assert_eq!(a.mode, Mode::SavedQueriesFilter);
    type_str(&mut a, "ord");
    assert_eq!(a.saved_ui.filter.as_ref().map(|t| t.text()), Some("ord"));
    assert_eq!(a.visible_saved_indices(), vec![1]);
    // Cursor 0 in the filtered view maps to real entry index 1.
    assert_eq!(a.focused_saved_index(), Some(1));
    // Enter keeps the filter applied and returns to navigation.
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::SavedQueries);
    assert_eq!(a.saved_ui.filter.as_ref().map(|t| t.text()), Some("ord"));
}

#[test]
fn saved_filter_esc_clears_filter() {
    let mut a = app_with_saved(&[("users", "a"), ("orders", "b")]);
    a.open_saved_queries();
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    type_str(&mut a, "ord");
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::SavedQueries);
    assert!(a.saved_ui.filter.is_none());
}

#[test]
fn saved_filter_backspace_widens() {
    let mut a = app_with_saved(&[("users", "a"), ("orders", "b")]);
    a.open_saved_queries();
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    type_str(&mut a, "ordz");
    assert!(a.visible_saved_indices().is_empty());
    a.on_key(KeyEvent::from(KeyCode::Backspace)); // back to "ord"
    assert_eq!(a.visible_saved_indices(), vec![1]);
}

#[test]
fn rename_prompt_prefills_current_name() {
    let mut a = app_with_saved(&[("old", "x")]);
    a.open_saved_queries();
    a.on_key(KeyEvent::from(KeyCode::Char('r')));
    assert_eq!(a.mode, Mode::RenameQueryPrompt);
    assert_eq!(a.saved_ui.rename_buf.text(), "old");
    assert_eq!(a.saved_ui.rename_from, "old");
}

#[test]
fn rename_rejects_empty_name() {
    let mut a = app_with_saved(&[("old", "x")]);
    a.open_saved_queries();
    a.on_key(KeyEvent::from(KeyCode::Char('r')));
    for _ in 0..8 {
        a.on_key(KeyEvent::from(KeyCode::Backspace));
    }
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::RenameQueryPrompt);
    assert!(a
        .last_status
        .as_deref()
        .unwrap_or("")
        .contains("name required"));
    // Original name untouched.
    assert_eq!(a.saved_queries.entries[0].name, "old");
}

#[test]
fn rename_refuses_collision_without_changing_entries() {
    let mut a = app_with_saved(&[("a", "x"), ("b", "y")]);
    a.open_saved_queries(); // cursor on "a"
    a.on_key(KeyEvent::from(KeyCode::Char('r')));
    for _ in 0..8 {
        a.on_key(KeyEvent::from(KeyCode::Backspace));
    }
    type_str(&mut a, "b"); // collide with existing "b"
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::RenameQueryPrompt); // stayed put
    assert!(a
        .last_status
        .as_deref()
        .unwrap_or("")
        .contains("already exists"));
    assert_eq!(a.saved_queries.entries[0].name, "a");
    assert_eq!(a.saved_queries.entries[1].name, "b");
}

#[test]
fn rename_esc_cancels_without_changing_entries() {
    let mut a = app_with_saved(&[("a", "x")]);
    a.open_saved_queries();
    a.on_key(KeyEvent::from(KeyCode::Char('r')));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::SavedQueries);
    assert_eq!(a.saved_queries.entries[0].name, "a");
}

#[test]
fn dispatch_fixture_writes_parseable_dataset_to_explicit_path() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = grid_of(&["id", "name"], &[&["1", "alice"], &["2", "bob"]]);
    a.grid_view.source = Some(("public".into(), "users".into()));
    let dir = std::env::temp_dir().join(format!("pgman-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("users.xml");
    a.dispatch_fixture(Some(path.to_string_lossy().to_string()));
    assert!(a.last_status.as_deref().unwrap_or("").contains("2 row(s)"));
    let xml = std::fs::read_to_string(&path).unwrap();
    let parsed = crate::dbunit::parse_flat_xml(&xml).unwrap();
    assert_eq!(parsed.rows.len(), 2);
    assert_eq!(parsed.rows[0].table, "users");
    assert_eq!(
        parsed.rows[0].columns,
        vec![("id".into(), "1".into()), ("name".into(), "alice".into())]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn dispatch_fixture_errors_without_source_table() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = grid_of(&["id"], &[&["1"]]);
    a.grid_view.source = None;
    a.dispatch_fixture(None);
    assert!(a
        .last_error
        .as_deref()
        .unwrap_or("")
        .contains("single-table"));
}

#[test]
fn dispatch_fixture_errors_on_empty_grid() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = grid_of(&["id"], &[]);
    a.grid_view.source = Some(("public".into(), "users".into()));
    a.dispatch_fixture(None);
    // A notice, not an error — same as `D` with nothing to diff.
    assert!(a.last_status.as_deref().unwrap_or("").contains("no result"));
    assert!(a.last_error.is_none(), "{:?}", a.last_error);
}

fn write_temp_fixture(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pgman-clean-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fx = dir.join("f.xml");
    std::fs::write(&fx, r#"<dataset><users id="1"/></dataset>"#).unwrap();
    fx
}

#[test]
fn load_dbunit_fixture_uses_per_db_clean_mode() {
    let fx = write_temp_fixture("delete");
    let mut cfg = SafetyConfig::default();
    cfg.databases.insert(
        "legacy".into(),
        crate::safety::SafetyProfile {
            clean_mode: crate::dbunit::CleanMode::DeleteFrom,
            ..Default::default()
        },
    );
    let dsn = crate::conn::Dsn::parse("postgres://u@h/legacy").ok();
    let mut a = App::new(Theme::default(), dsn, Vec::new(), cfg);
    a.editor.buffer = fx.to_string_lossy().to_string();
    a.load_dbunit_fixture();
    assert!(
        a.editor.buffer.contains("DELETE FROM users"),
        "expected DELETE FROM; got:\n{}",
        a.editor.buffer
    );
    assert!(!a.editor.buffer.contains("TRUNCATE"));
    let _ = std::fs::remove_file(&fx);
}

#[test]
fn load_dbunit_fixture_defaults_to_truncate() {
    let fx = write_temp_fixture("trunc");
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.editor.buffer = fx.to_string_lossy().to_string();
    a.load_dbunit_fixture();
    assert!(
        a.editor.buffer.contains("TRUNCATE TABLE users"),
        "expected TRUNCATE; got:\n{}",
        a.editor.buffer
    );
    let _ = std::fs::remove_file(&fx);
}

#[test]
fn cycle_sort_orders_numerically_asc_then_desc_then_off() {
    let mut a = app_with_grid(sample_grid());
    // Column cursor defaults to 0 (id). ASC: 1, 2, 3, 10.
    a.cycle_sort();
    let ids: Vec<&str> = a.grid.rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(ids, vec!["1", "2", "3", "10"]);
    assert_eq!(a.grid_view.sort, Some((0, true)));
    // DESC: 10, 3, 2, 1.
    a.cycle_sort();
    let ids: Vec<&str> = a.grid.rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(ids, vec!["10", "3", "2", "1"]);
    // Off: original order restored.
    a.cycle_sort();
    let ids: Vec<&str> = a.grid.rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(ids, vec!["3", "1", "10", "2"]);
    assert!(a.grid_view.sort.is_none());
}

#[test]
fn cycle_sort_on_different_column_jumps_to_asc() {
    let mut a = app_with_grid(sample_grid());
    a.cycle_sort(); // col 0 ASC
    a.move_col_cursor(1);
    a.cycle_sort(); // col 1 ASC (NOT col 0 DESC)
    assert_eq!(a.grid_view.sort, Some((1, true)));
    let names: Vec<&str> = a.grid.rows.iter().map(|r| r[1].as_str()).collect();
    assert_eq!(names, vec!["alice", "bob", "carol", "dave"]);
}

#[test]
fn filter_narrows_visible_rows_case_insensitively() {
    let mut a = app_with_grid(sample_grid());
    a.start_filter();
    // Type 'AL' — case-insensitive substring across all columns.
    a.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    a.on_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
    // Only `alice` (row idx 1) matches.
    assert_eq!(a.grid_view.visible_rows, vec![1]);
    // Enter accepts; filter persists.
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Normal);
    assert_eq!(a.grid_view.filter.as_deref(), Some("aL"));
}

#[test]
fn filter_esc_clears_pattern_and_restores_visible_rows() {
    let mut a = app_with_grid(sample_grid());
    a.start_filter();
    a.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(a.grid_view.visible_rows.is_empty());
    a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(a.grid_view.visible_rows.len(), 4);
    assert!(a.grid_view.filter.is_none());
}

#[test]
fn selected_grid_row_idx_maps_through_filter() {
    let mut a = app_with_grid(sample_grid());
    a.grid_view.filter = Some("a".into()); // matches alice, carol, dave
    a.rebuild_visible_rows();
    // visible_rows holds indices into grid.rows for matches in
    // original order: carol(0), alice(1), dave(3).
    assert_eq!(a.grid_view.visible_rows, vec![0, 1, 3]);
    a.grid_state.select(Some(1)); // second visible row → alice
    assert_eq!(a.selected_grid_row_idx(), Some(1));
}

#[test]
fn infer_single_source_table_picks_one_from_simple_select() {
    let got = infer_single_source_table("SELECT * FROM users WHERE active = true");
    assert_eq!(got, Some(("public".into(), "users".into())));
}

#[test]
fn infer_single_source_table_returns_none_for_joins() {
    assert!(infer_single_source_table("SELECT * FROM users u JOIN orders o ON true").is_none());
}

#[test]
fn infer_single_source_table_returns_none_for_no_from() {
    assert!(infer_single_source_table("SELECT 1").is_none());
}

#[test]
fn infer_single_source_table_keeps_explicit_schema() {
    assert_eq!(
        infer_single_source_table("SELECT * FROM audit.events"),
        Some(("audit".into(), "events".into()))
    );
}

#[test]
fn format_sql_literal_nulls_empty_strings() {
    assert_eq!(format_sql_literal(""), "NULL");
}

#[test]
fn format_sql_literal_passes_numerics_unquoted() {
    assert_eq!(format_sql_literal("42"), "42");
    assert_eq!(format_sql_literal("3.14"), "3.14");
    assert_eq!(format_sql_literal("-1"), "-1");
}

#[test]
fn format_sql_literal_lowercases_booleans() {
    assert_eq!(format_sql_literal("TRUE"), "true");
    assert_eq!(format_sql_literal("False"), "false");
}

#[test]
fn format_sql_literal_quotes_strings_and_doubles_internal_quotes() {
    assert_eq!(format_sql_literal("alice"), "'alice'");
    assert_eq!(format_sql_literal("it's fine"), "'it''s fine'");
}

#[test]
fn yank_row_as_insert_no_source_surfaces_actionable_error() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid_view.source = None;
    a.grid = Grid {
        columns: vec!["id".into()],
        rows: vec![vec!["1".into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.yank_row_as_insert();
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("single-table SELECTs"),
        "expected actionable error; got: {err}"
    );
}

#[test]
fn normal_mode_esc_is_a_noop_does_not_quit() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(!a.should_quit);
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn conn_pick_esc_is_a_noop_does_not_quit() {
    let dsn = Dsn::parse("postgres://test@localhost/test").unwrap();
    let picks = vec![
        DataSourcePick {
            name: "a".into(),
            origin: "test",
            dsn: Some(dsn.clone()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
        DataSourcePick {
            name: "b".into(),
            origin: "test",
            dsn: Some(dsn),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
    ];
    let mut a = App::new(Theme::default(), None, picks, SafetyConfig::default());
    a.mode = Mode::ConnPick;
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(!a.should_quit);
    assert_eq!(a.mode, Mode::ConnPick);
}

#[test]
fn conn_pick_enter_refuses_a_pick_with_an_unresolved_placeholder() {
    // Simulates a Spring pick whose username still carries `${DB_USER}`
    // because discovery couldn't resolve it from the environment.
    // Enter must refuse — not attempt a connection that would send the
    // literal `${DB_USER}` text as the login role.
    let pick = DataSourcePick {
        name: "spring.datasource (application) — unresolved ${DB_USER}".into(),
        origin: "Spring",
        dsn: Some(Dsn::parse("postgres://${DB_USER}@db.internal:5432/orders").unwrap()),
        unresolved: vec!["DB_USER".to_string()],
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    a.mode = Mode::ConnPick;
    a.conn_pick.index = 0;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(
        a.mode,
        Mode::ConnPick,
        "refused pick must stay in the picker, not fall through to Normal"
    );
    assert!(
        matches!(a.conn_state, ConnState::Disconnected),
        "must not start connecting"
    );
    assert_eq!(
        a.last_error.as_deref(),
        Some(
            "unresolved placeholder ${DB_USER} — export it, or put the connection in .pgman/pgman.toml"
        )
    );
}

#[test]
fn conn_pick_enter_refuses_a_placeholder_in_the_host_with_its_own_message() {
    // A `${…}` in the host is never resolved, whatever the environment
    // holds — so "export it" would be the wrong advice. The refusal
    // says why instead, and names the placeholder.
    let pick = DataSourcePick {
        name: "spring.datasource (application) — unresolved ${DB_HOST}".into(),
        origin: "Spring",
        dsn: Some(Dsn::parse("postgres://svc@${DB_HOST}:5432/orders").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: vec!["DB_HOST".to_string()],
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    a.mode = Mode::ConnPick;
    a.conn_pick.index = 0;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::ConnPick);
    assert!(
        matches!(a.conn_state, ConnState::Disconnected),
        "must not start connecting"
    );
    let err = a.last_error.as_deref().expect("refusal message");
    assert!(err.starts_with("${DB_HOST} sits in the host"), "got: {err}");
    assert!(
        !err.contains("export it"),
        "exporting it doesn't help — the host is never resolved: {err}"
    );
}

#[test]
fn conn_pick_enter_refuses_an_unmarked_placeholder_still_sitting_in_the_dsn() {
    // Belt and braces. Every discovery source is supposed to mark its
    // own `${…}` — this is the pick from a source that didn't. The
    // literal text is not inert: as a password it goes on the wire.
    let mut dsn = Dsn::parse("postgres://svc@db:5432/orders").unwrap();
    dsn.password = Some("${DB_PASSWORD}".to_string());
    let pick = DataSourcePick {
        name: "dev".into(),
        origin: "project",
        dsn: Some(dsn),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    a.mode = Mode::ConnPick;
    a.conn_pick.index = 0;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(
        matches!(a.conn_state, ConnState::Disconnected),
        "must not start connecting"
    );
    let err = a.last_error.as_deref().expect("refusal message");
    assert!(
        err.contains("${DB_PASSWORD}") && err.contains("password"),
        "the refusal names the placeholder and the field: {err}"
    );
}

#[test]
fn dsn_placeholder_field_finds_every_connection_critical_field_and_nothing_else() {
    let clean = Dsn::parse("postgres://svc:pw@db:5432/orders?sslmode=require").unwrap();
    assert_eq!(crate::app::dsn_placeholder_field(Some(&clean)), None);
    assert_eq!(crate::app::dsn_placeholder_field(None), None);

    let host = Dsn::parse("postgres://svc@${H}:5432/orders").unwrap();
    assert_eq!(
        crate::app::dsn_placeholder_field(Some(&host)),
        Some(("host", "H".to_string()))
    );

    let mut db = clean.clone();
    db.dbname = "${DB}".into();
    assert_eq!(
        crate::app::dsn_placeholder_field(Some(&db)),
        Some(("database name", "DB".to_string()))
    );

    let mut user = clean.clone();
    user.user = Some("${U}".into());
    assert_eq!(
        crate::app::dsn_placeholder_field(Some(&user)),
        Some(("username", "U".to_string()))
    );

    let mut tunnel = clean.clone();
    tunnel.ssh_tunnel = Some(crate::tunnel::SshTunnelSpec {
        user: None,
        host: "${BASTION}".into(),
        port: None,
    });
    assert_eq!(
        crate::app::dsn_placeholder_field(Some(&tunnel)),
        Some(("ssh tunnel host", "BASTION".to_string()))
    );

    let mut param = clean.clone();
    param.params.push(("options".into(), "${OPTS}".into()));
    assert_eq!(
        crate::app::dsn_placeholder_field(Some(&param)),
        Some(("url parameters", "OPTS".to_string()))
    );
}

#[test]
fn conn_pick_enter_refuses_a_pick_with_no_usable_dsn() {
    // Discovery keeps a pick whose URL wouldn't parse (a placeholder in
    // the port, say) so the operator can see it. Enter on it must
    // explain itself, not silently do nothing.
    let pick = DataSourcePick {
        name: "spring.datasource (application)".into(),
        origin: "Spring",
        dsn: None,
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    a.mode = Mode::ConnPick;
    a.conn_pick.index = 0;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::ConnPick);
    assert!(matches!(a.conn_state, ConnState::Disconnected));
    let err = a.last_error.as_deref().expect("refusal message");
    assert!(err.contains("no usable connection URL"), "got: {err}");
}

#[test]
fn backslash_c_by_name_refuses_a_pick_with_an_unresolved_placeholder() {
    let pick = DataSourcePick {
        name: "staging".into(),
        origin: "Spring",
        dsn: Some(Dsn::parse("postgres://${DB_USER}@db.internal:5432/orders").unwrap()),
        unresolved: vec!["DB_USER".to_string()],
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c staging".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(
        matches!(a.conn_state, ConnState::Disconnected),
        "must not start connecting"
    );
    assert_eq!(
        a.last_error.as_deref(),
        Some(
            "unresolved placeholder ${DB_USER} — export it, or put the connection in .pgman/pgman.toml"
        )
    );
}

#[test]
fn open_cell_detail_parses_json_object_and_primes_tree() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = Grid {
        columns: vec!["data".into()],
        rows: vec![vec![r#"{"id":1,"name":"alice"}"#.into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.row_detail.field = 0;
    a.open_cell_detail();
    assert_eq!(a.mode, Mode::CellDetail);
    // Root + 2 members.
    assert_eq!(a.cell_detail.json_rows.len(), 3);
    assert!(a.cell_detail.json_value.is_some());
}

#[test]
fn open_cell_detail_leaves_tree_empty_for_non_json_cells() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = Grid {
        columns: vec!["note".into()],
        rows: vec![vec!["hello world".into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.row_detail.field = 0;
    a.open_cell_detail();
    assert_eq!(a.mode, Mode::CellDetail);
    assert!(a.cell_detail.json_rows.is_empty());
    assert!(a.cell_detail.json_value.is_none());
}

#[test]
fn cell_detail_json_jk_moves_cursor_within_bounds() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = Grid {
        columns: vec!["data".into()],
        rows: vec![vec![r#"{"a":1,"b":2}"#.into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.row_detail.field = 0;
    a.open_cell_detail();
    // 3 rows: root, .a, .b. Start at 0.
    assert_eq!(a.cell_detail.json_cursor, 0);
    a.on_key(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(a.cell_detail.json_cursor, 1);
    a.on_key(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(a.cell_detail.json_cursor, 2);
    // Clamp at last row.
    a.on_key(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(a.cell_detail.json_cursor, 2);
    // k walks back.
    a.on_key(KeyEvent::from(KeyCode::Char('k')));
    assert_eq!(a.cell_detail.json_cursor, 1);
}

#[test]
fn cell_detail_json_enter_collapses_focused_container() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = Grid {
        columns: vec!["data".into()],
        rows: vec![vec![r#"{"a":{"x":1},"b":2}"#.into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.row_detail.field = 0;
    a.open_cell_detail();
    // Walk to .a (the nested object).
    a.on_key(KeyEvent::from(KeyCode::Char('j')));
    let path_at_cursor = a.cell_detail.json_rows[a.cell_detail.json_cursor]
        .path
        .clone();
    assert_eq!(path_at_cursor, ".a");
    // Expanded → collapsed reduces row count.
    let expanded_count = a.cell_detail.json_rows.len();
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(a.cell_detail.json_rows.len() < expanded_count);
    assert!(a.cell_detail.json_collapsed.contains(".a"));
    // Cursor stayed on .a (didn't drift to a sibling).
    assert_eq!(
        a.cell_detail.json_rows[a.cell_detail.json_cursor].path,
        ".a"
    );
    // Toggle back: row count restored.
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.cell_detail.json_rows.len(), expanded_count);
    assert!(!a.cell_detail.json_collapsed.contains(".a"));
}

#[test]
fn cell_detail_json_esc_returns_to_row_detail() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = Grid {
        columns: vec!["data".into()],
        rows: vec![vec![r#"{"a":1}"#.into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.row_detail.field = 0;
    a.open_cell_detail();
    assert_eq!(a.mode, Mode::CellDetail);
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::RowDetail);
}

#[test]
fn slow_queries_enter_copies_focused_sql_to_editor_and_returns() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::SlowQueries;
    a.slow_queries.rows = vec![
        crate::query::slow_queries::SlowQueryRow {
            query: "SELECT 1".into(),
            calls: 100,
            total_ms: 500.0,
            mean_ms: 5.0,
            rows: 100,
        },
        crate::query::slow_queries::SlowQueryRow {
            query: "UPDATE x SET y=1".into(),
            calls: 10,
            total_ms: 200.0,
            mean_ms: 20.0,
            rows: 10,
        },
    ];
    a.slow_queries.cursor = 1;
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor.buffer, "UPDATE x SET y=1");
}

#[test]
fn slow_queries_jk_clamps_to_row_range() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::SlowQueries;
    a.slow_queries.rows = vec![
        crate::query::slow_queries::SlowQueryRow {
            query: "a".into(),
            calls: 1,
            total_ms: 1.0,
            mean_ms: 1.0,
            rows: 1,
        },
        crate::query::slow_queries::SlowQueryRow {
            query: "b".into(),
            calls: 2,
            total_ms: 2.0,
            mean_ms: 1.0,
            rows: 2,
        },
    ];
    for _ in 0..10 {
        a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    }
    assert_eq!(a.slow_queries.cursor, 1);
}

#[test]
fn sessions_esc_returns_to_normal() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Sessions;
    a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn start_slow_queries_without_client_surfaces_not_connected() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.start_slow_queries();
    assert_eq!(a.mode, Mode::Normal);
    assert!(a
        .last_error
        .as_deref()
        .unwrap_or("")
        .contains("not connected"));
}

#[test]
fn slow_queries_loaded_failure_with_missing_extension_hints_install() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::SlowQueries;
    a.generation = 1;
    a.on_msg(AppMsg::SlowQueriesLoaded {
        generation: 1,
        result: Err(crate::conn::QueryErr::msg(
            "relation \"pg_stat_statements\" does not exist",
        )),
    });
    // Back to Normal + actionable hint in the error — one that says
    // what the extension needs, not just its name.
    assert_eq!(a.mode, Mode::Normal);
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("CREATE EXTENSION pg_stat_statements"),
        "expected install hint; got: {err}"
    );
    assert!(
        err.contains("shared_preload_libraries") && err.contains("read-write session"),
        "the hint must say what CREATE EXTENSION needs; got: {err}"
    );
}

/// What a stock Postgres actually sends for `T`: a `DbError` with
/// SQLSTATE 42P01 and the relation in the message. The footer names
/// the relation and hints the extension; F2 has the code to show.
/// (The load used to arrive as the bare `db error`, so neither did.)
#[test]
fn slow_queries_load_failure_keeps_the_server_message_and_detail_for_f2() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::SlowQueries;
    a.generation = 1;
    a.on_msg(AppMsg::SlowQueriesLoaded {
        generation: 1,
        result: Err(crate::conn::QueryErr {
            msg: "relation \"pg_stat_statements\" does not exist".into(),
            position: Some(14),
            detail: Some(crate::conn::QueryErrDetail {
                code: Some("42P01".into()),
                severity: Some("ERROR".into()),
                ..Default::default()
            }),
        }),
    });
    let err = a.last_error.as_deref().unwrap_or("");
    assert_eq!(
        err,
        "slow queries load failed: relation \"pg_stat_statements\" does not exist (try `CREATE EXTENSION pg_stat_statements` — needs shared_preload_libraries and a read-write session)"
    );
    assert_eq!(
        a.last_error_detail.as_ref().and_then(|d| d.code.as_deref()),
        Some("42P01")
    );
    // F2 now has something to expand.
    a.on_key(KeyEvent::from(KeyCode::F(2)));
    assert_eq!(a.mode, Mode::ErrorDetail);
}

#[test]
fn sessions_load_failure_keeps_the_server_message_and_detail_for_f2() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Sessions;
    a.generation = 1;
    a.on_msg(AppMsg::SessionsLoaded {
        generation: 1,
        result: Err(crate::conn::QueryErr {
            msg: "permission denied for view pg_stat_activity".into(),
            position: None,
            detail: Some(crate::conn::QueryErrDetail {
                code: Some("42501".into()),
                ..Default::default()
            }),
        }),
    });
    assert_eq!(a.mode, Mode::Normal);
    assert_eq!(
        a.last_error.as_deref(),
        Some("sessions load failed: permission denied for view pg_stat_activity")
    );
    assert_eq!(
        a.last_error_detail.as_ref().and_then(|d| d.code.as_deref()),
        Some("42501")
    );
}

fn app_with_schemas() -> App {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let mut cache = crate::query::schema::SchemaCache {
        schemas: vec!["audit".into(), "public".into()],
        tables: vec![
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "users".into(),
            },
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "orders".into(),
            },
            crate::query::schema::TableMeta {
                schema: "audit".into(),
                name: "events".into(),
            },
        ],
        ..Default::default()
    };
    cache.columns_by_table.insert(
        ("public".into(), "users".into()),
        vec!["id".into(), "email".into()],
    );
    a.schema_cache = cache;
    a
}

#[test]
fn schema_browser_flat_starts_with_schemas_collapsed() {
    let a = app_with_schemas();
    let rows = a.flattened_schema_browser();
    assert_eq!(rows.len(), 2);
    assert!(matches!(
        rows[0],
        SchemaBrowserRow::Schema {
            ref name,
            expanded: false,
            ..
        } if name == "audit"
    ));
    assert!(matches!(
        rows[1],
        SchemaBrowserRow::Schema {
            ref name,
            ..
        } if name == "public"
    ));
}

#[test]
fn schema_browser_enter_expands_focused_schema() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    // Focus row 1 (public).
    a.schema_browser.cursor = 1;
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let rows = a.flattened_schema_browser();
    // Now: audit (collapsed), public (expanded), orders, users.
    assert_eq!(rows.len(), 4);
    assert!(matches!(
        rows[1],
        SchemaBrowserRow::Schema { expanded: true, .. }
    ));
    assert!(matches!(
        rows[2],
        SchemaBrowserRow::Table { ref name, .. } if name == "orders"
    ));
    assert!(matches!(
        rows[3],
        SchemaBrowserRow::Table { ref name, .. } if name == "users"
    ));
}

#[test]
fn schema_browser_jk_nav_clamps_to_visible() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    for _ in 0..10 {
        a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    }
    // Only 2 visible rows (schemas collapsed); cursor at 1.
    assert_eq!(a.schema_browser.cursor, 1);
}

#[test]
fn schema_browser_esc_returns_to_normal() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn schema_browser_enter_on_table_expands_to_columns_and_constraints() {
    let mut a = app_with_schemas();
    // Add a constraint so the third level isn't just columns.
    a.schema_cache.constraints = vec![crate::query::schema::ConstraintMeta {
        schema: "public".into(),
        table: "users".into(),
        name: "users_pkey".into(),
    }];
    a.mode = Mode::SchemaBrowser;
    // Expand "public" first, then drill into "users".
    a.schema_browser.cursor = 1; // public
    a.on_key(KeyEvent::from(KeyCode::Enter));
    // Now rows: audit, public(expanded), orders, users.
    // Move to "users" (row 3) and toggle.
    a.schema_browser.cursor = 3;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    let rows = a.flattened_schema_browser();
    // audit, public, orders, users(expanded), id, email, users_pkey.
    assert_eq!(rows.len(), 7);
    assert!(matches!(
        rows[3],
        SchemaBrowserRow::Table {
            ref name,
            expanded: true,
            ..
        } if name == "users"
    ));
    assert!(matches!(
        rows[4],
        SchemaBrowserRow::Column { ref name, .. } if name == "id"
    ));
    assert!(matches!(
        rows[5],
        SchemaBrowserRow::Column { ref name, .. } if name == "email"
    ));
    assert!(matches!(
        rows[6],
        SchemaBrowserRow::Constraint { ref name, .. }
            if name == "users_pkey"
    ));
}

#[test]
fn schema_browser_collapsing_schema_hides_its_table_drilldown() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    // Expand "public", expand "public.users".
    a.schema_browser.expanded.insert("public".into());
    a.schema_browser
        .expanded
        .insert(schema_browser_table_key("public", "users"));
    // Now collapse "public" again.
    a.schema_browser.cursor = 1;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    let rows = a.flattened_schema_browser();
    // Only the two schema rows are visible.
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|r| matches!(r, SchemaBrowserRow::Schema { .. })));
    // The "public.users" key is still set — re-expanding the schema
    // restores the open table drilldown.
    a.on_key(KeyEvent::from(KeyCode::Enter));
    let rows = a.flattened_schema_browser();
    assert!(rows.iter().any(|r| matches!(
        r,
        SchemaBrowserRow::Column { name, .. } if name == "id"
    )));
}

#[test]
fn quote_ident_passes_simple_snake_case_unquoted() {
    assert_eq!(quote_ident("users"), "users");
    assert_eq!(quote_ident("user_id"), "user_id");
    assert_eq!(quote_ident("_internal"), "_internal");
    assert_eq!(quote_ident("a1"), "a1");
}

#[test]
fn quote_ident_wraps_anything_unusual() {
    assert_eq!(quote_ident("User"), "\"User\"");
    assert_eq!(quote_ident("1col"), "\"1col\"");
    assert_eq!(quote_ident("with space"), "\"with space\"");
    assert_eq!(quote_ident("café"), "\"café\"");
    assert_eq!(quote_ident("evil\"name"), "\"evil\"\"name\"");
}

#[test]
fn build_select_all_template_uses_quoted_idents_only_when_needed() {
    assert_eq!(
        build_select_all_template("public", "users"),
        "SELECT * FROM public.users LIMIT 100;"
    );
    assert_eq!(
        build_select_all_template("Audit", "Events"),
        "SELECT * FROM \"Audit\".\"Events\" LIMIT 100;"
    );
}

#[test]
fn build_insert_template_emits_one_null_per_column() {
    let sql = build_insert_template(
        "public",
        "users",
        &["id".into(), "email".into(), "active".into()],
    );
    assert_eq!(
        sql,
        "INSERT INTO public.users\n  (id, email, active)\nVALUES\n  (NULL, NULL, NULL);"
    );
}

#[test]
fn build_insert_template_returns_empty_when_no_columns() {
    assert!(build_insert_template("public", "t", &[]).is_empty());
}

fn log_picks_with_an_n_plus_one_cluster() -> Vec<crate::query::reconstruct::ReconstructedQuery> {
    use crate::query::reconstruct::{ReconstructedQuery, Source};
    let make = |sql: &str| ReconstructedQuery {
        raw_sql: sql.into(),
        params: Vec::new(),
        runnable_sql: sql.into(),
        source: Source::HibernateLog,
        src_line: 0,
    };
    vec![
        make("select * from item where order_id = 1"),
        make("select * from item where order_id = 2"),
        make("select * from item where order_id = 3"),
        make("select * from orders where id = 1"),
    ]
}

#[test]
fn log_pick_visible_len_reflects_view() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.log_pick.picks = log_picks_with_an_n_plus_one_cluster();
    a.log_pick.clusters = crate::query::nplus1::detect(&a.log_pick.picks);
    a.mode = Mode::LogPick;
    assert_eq!(a.log_pick.view, LogPickView::AllQueries);
    assert_eq!(a.log_pick_visible_len(), 4);
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert_eq!(a.log_pick.view, LogPickView::Clusters);
    assert_eq!(a.log_pick_visible_len(), 1); // one repeated shape
}

#[test]
fn log_pick_toggle_resets_cursor() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.log_pick.picks = log_picks_with_an_n_plus_one_cluster();
    a.log_pick.clusters = crate::query::nplus1::detect(&a.log_pick.picks);
    a.mode = Mode::LogPick;
    // Cursor at row 3 in AllQueries view.
    a.log_pick.index = 3;
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    // Clusters view has only 1 row → cursor must clamp.
    assert_eq!(a.log_pick.index, 0);
}

/// The shape a Hibernate log with bind logging gives: one raw `?`
/// template per statement, and `runnable_sql` substituted for the
/// statements whose bind lines were present.
fn log_picks_with_a_cluster_of(
    runnable: &[&str],
) -> Vec<crate::query::reconstruct::ReconstructedQuery> {
    use crate::query::reconstruct::{ReconstructedQuery, Source};
    runnable
        .iter()
        .enumerate()
        .map(|(i, r)| ReconstructedQuery {
            raw_sql: "select * from item where order_id=?".into(),
            params: Vec::new(),
            runnable_sql: (*r).into(),
            source: Source::HibernateLog,
            src_line: i,
        })
        .collect()
}

#[test]
fn log_pick_enter_in_cluster_view_loads_a_runnable_member_not_the_template() {
    // The cluster row shows the `?` template (that is what it groups by);
    // Enter used to load exactly that, and F5 then failed on the `?`.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.log_pick.picks = log_picks_with_a_cluster_of(&[
        "select * from item where order_id=?",
        "select * from item where order_id=101",
        "select * from item where order_id=102",
    ]);
    a.log_pick.clusters = crate::query::nplus1::detect(&a.log_pick.picks);
    assert_eq!(a.log_pick.clusters.len(), 1, "fixture check");
    a.mode = Mode::LogPick;
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor.buffer, "select * from item where order_id=101;");
    assert_eq!(
        a.last_status.as_deref(),
        Some("loaded query · 38 char(s)"),
        "a bound member is an ordinary load"
    );
}

#[test]
fn log_pick_enter_on_a_cluster_with_no_bound_member_loads_the_template_and_says_so() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.log_pick.picks = log_picks_with_a_cluster_of(&[
        "select * from item where order_id=?",
        "select * from item where order_id=?",
    ]);
    a.log_pick.clusters = crate::query::nplus1::detect(&a.log_pick.picks);
    a.mode = Mode::LogPick;
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor.buffer, "select * from item where order_id=?;");
    assert_eq!(a.last_status.as_deref(), Some(LOG_PICK_UNBOUND_STATUS));
    assert!(
        a.last_error.is_none(),
        "a template is a notice, not a failure"
    );
}

#[test]
fn log_pick_enter_in_all_queries_view_says_when_the_pick_is_a_template() {
    // The same honesty in the flat view: a pick whose bind lines were
    // missing lands as a template, and the status must not call it a
    // loaded, runnable query.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.log_pick.picks = log_picks_with_a_cluster_of(&[
        "select * from item where order_id=?",
        "select * from item where order_id=5",
    ]);
    a.log_pick.clusters = crate::query::nplus1::detect(&a.log_pick.picks);
    a.mode = Mode::LogPick;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.editor.buffer, "select * from item where order_id=?;");
    assert_eq!(a.last_status.as_deref(), Some(LOG_PICK_UNBOUND_STATUS));
}

#[test]
fn schema_browser_s_on_schema_row_surfaces_error_not_garbage() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    a.schema_browser.cursor = 0; // a schema row
    a.on_key(KeyEvent::from(KeyCode::Char('s')));
    assert!(a.last_error.is_some());
}

#[test]
fn schema_browser_i_with_no_cached_columns_surfaces_error() {
    let mut a = app_with_schemas();
    // public.orders has no columns_by_table entry.
    a.schema_browser.expanded.insert("public".into());
    a.mode = Mode::SchemaBrowser;
    // Walk to orders.
    let rows = a.flattened_schema_browser();
    let idx = rows
        .iter()
        .position(|r| matches!(r, SchemaBrowserRow::Table { name, .. } if name == "orders"))
        .unwrap();
    a.schema_browser.cursor = idx;
    a.on_key(KeyEvent::from(KeyCode::Char('i')));
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(err.contains("no columns known"), "got: {err}");
}

#[test]
fn is_cost_checkable_accepts_plain_selects_and_ctes() {
    assert!(is_cost_checkable("SELECT * FROM users"));
    assert!(is_cost_checkable("  select 1"));
    assert!(is_cost_checkable("WITH x AS (SELECT 1) SELECT * FROM x"));
    assert!(is_cost_checkable("TABLE users"));
    assert!(is_cost_checkable("VALUES (1, 2)"));
}

#[test]
fn is_cost_checkable_rejects_writes_and_explain() {
    assert!(!is_cost_checkable("INSERT INTO t VALUES (1)"));
    assert!(!is_cost_checkable("UPDATE t SET a = 1"));
    assert!(!is_cost_checkable("DELETE FROM t"));
    assert!(!is_cost_checkable("EXPLAIN SELECT 1"));
    assert!(!is_cost_checkable("CREATE TABLE t (id int)"));
}

#[test]
fn is_cost_checkable_skips_self_bounded_limit_queries() {
    // A LIMIT means the query already self-bounds its result —
    // pre-flight gating would be noisy.
    assert!(!is_cost_checkable("SELECT * FROM events LIMIT 100"));
    assert!(!is_cost_checkable("select * from t LIMIT 5"));
}

#[test]
fn is_cost_checkable_ignores_limit_inside_string_literal() {
    // The token `limit` only counts when it's actually a clause.
    // A literal value with the word in it must NOT skip the gate.
    assert!(is_cost_checkable(
        "SELECT 'over the limit' AS reason FROM t"
    ));
    // Same with doubled-quote escapes inside.
    assert!(is_cost_checkable("SELECT 'it''s past the limit' FROM t"));
}

#[test]
fn is_cost_checkable_rejects_cte_wrapped_writes() {
    // CTE-wrapped writes look like SELECT but are really DML.
    // Reject so the cost-preview Confirm doesn't misleadingly
    // call them "estimated N rows — proceed?".
    assert!(!is_cost_checkable(
        "WITH d AS (DELETE FROM t RETURNING id) SELECT count(*) FROM d"
    ));
    assert!(!is_cost_checkable(
        "WITH u AS (UPDATE t SET x=1 RETURNING *) SELECT * FROM u"
    ));
    assert!(!is_cost_checkable(
        "WITH i AS (INSERT INTO t VALUES (1) RETURNING id) SELECT * FROM i"
    ));
}

#[test]
fn is_cost_checkable_keeps_delete_keyword_in_string_safe() {
    // The CTE-write check would false-reject a query with
    // 'DELETE' inside a string literal if string-stripping
    // weren't applied. Verify the stripping rescues it.
    assert!(is_cost_checkable("SELECT 'DELETE me later' AS note FROM t"));
}

#[test]
fn strip_strings_replaces_literal_bodies_preserving_length() {
    let s = "SELECT 'hello' FROM t WHERE x = \"a\\b\"";
    let out = strip_strings(s);
    assert_eq!(out.len(), s.len());
    // The 'hello' body got replaced; the quoting char stays.
    assert!(out.contains("'_____'"));
    // The double-quoted body got replaced too.
    assert!(out.contains("\"___\""));
}

#[test]
fn strip_strings_handles_doubled_quote_escapes() {
    let s = "SELECT 'it''s ok'";
    let out = strip_strings(s);
    assert_eq!(out.len(), s.len());
    // The whole body including the `''` escape becomes `_`s
    // (the embedded quote was treated as part of the literal,
    // not a terminator).
    assert!(out.contains("'________'"));
}

#[test]
fn format_row_estimate_uses_commas() {
    assert_eq!(format_row_estimate(0.0), "0");
    assert_eq!(format_row_estimate(999.0), "999");
    assert_eq!(format_row_estimate(1_000.0), "1,000");
    assert_eq!(format_row_estimate(1_234_567.0), "1,234,567");
    assert_eq!(format_row_estimate(4_200_000.5), "4,200,001");
}

#[test]
fn history_encode_decode_round_trips_multiline() {
    let sample = "select 1\nfrom t\nwhere x = 'a\\b'";
    let encoded = encode_history_line(sample);
    assert!(
        !encoded.contains('\n'),
        "encoded must be one line: {encoded:?}"
    );
    let decoded = decode_history_line(&encoded);
    assert_eq!(decoded, sample);
}

#[test]
fn history_decode_tolerates_unknown_escapes() {
    // Unknown `\?` sequences emit literally so we never lose bytes.
    assert_eq!(decode_history_line("\\?"), "\\?");
    // Trailing lone `\` at end of string keeps the literal.
    assert_eq!(decode_history_line("foo\\"), "foo\\");
}

#[test]
fn history_persist_then_load_round_trips_via_temp_file() {
    let dir = std::env::temp_dir().join(format!("pgman-history-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("history.log");
    let entries: Vec<String> = vec![
        "select 1".into(),
        "select *\nfrom users".into(),
        "select now()".into(),
    ];
    persist_history_to(&path, &entries).unwrap();
    let loaded = load_history_from(&path);
    assert_eq!(loaded, entries);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn history_persist_caps_to_history_cap_entries() {
    let dir = std::env::temp_dir().join(format!("pgman-history-cap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("history.log");
    // 200 entries → file gets capped at HISTORY_CAP.
    let many: Vec<String> = (0..200).map(|i| format!("query {i}")).collect();
    persist_history_to(&path, &many).unwrap();
    let loaded = load_history_from(&path);
    assert_eq!(loaded.len(), HISTORY_CAP);
    // Persist keeps the NEWEST cap entries — symmetric with
    // load_history_from which also drops from the head.
    // For 200 entries, the kept window is [150..199].
    assert_eq!(loaded[0], format!("query {}", 200 - HISTORY_CAP));
    assert_eq!(loaded[HISTORY_CAP - 1], "query 199");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn history_load_from_missing_file_returns_empty() {
    let path = std::env::temp_dir().join("definitely-not-a-real-file-xyz");
    let _ = std::fs::remove_file(&path);
    assert!(load_history_from(&path).is_empty());
}

#[test]
fn filter_schema_browser_rows_matches_self() {
    let rows = vec![
        SchemaBrowserRow::Schema {
            name: "public".into(),
            expanded: false,
            table_count: 1,
        },
        SchemaBrowserRow::Schema {
            name: "audit".into(),
            expanded: false,
            table_count: 1,
        },
    ];
    let out = filter_schema_browser_rows(rows, "aud");
    assert_eq!(out.len(), 1);
    assert!(matches!(
        &out[0],
        SchemaBrowserRow::Schema { name, .. } if name == "audit"
    ));
}

#[test]
fn filter_schema_browser_rows_keeps_ancestor_of_match() {
    let rows = vec![
        SchemaBrowserRow::Schema {
            name: "public".into(),
            expanded: true,
            table_count: 2,
        },
        SchemaBrowserRow::Table {
            schema: "public".into(),
            name: "orders".into(),
            expanded: false,
            column_count: 0,
            constraint_count: 0,
        },
        SchemaBrowserRow::Table {
            schema: "public".into(),
            name: "users".into(),
            expanded: false,
            column_count: 0,
            constraint_count: 0,
        },
    ];
    let out = filter_schema_browser_rows(rows, "users");
    // public schema (ancestor) + users table.
    assert_eq!(out.len(), 2);
    assert!(matches!(&out[0], SchemaBrowserRow::Schema { name, .. } if name == "public"));
    assert!(matches!(&out[1], SchemaBrowserRow::Table { name, .. } if name == "users"));
}

#[test]
fn filter_schema_browser_rows_keeps_path_to_deep_match() {
    let rows = vec![
        SchemaBrowserRow::Schema {
            name: "public".into(),
            expanded: true,
            table_count: 1,
        },
        SchemaBrowserRow::Table {
            schema: "public".into(),
            name: "users".into(),
            expanded: true,
            column_count: 2,
            constraint_count: 0,
        },
        SchemaBrowserRow::Column {
            schema: "public".into(),
            table: "users".into(),
            name: "email".into(),
        },
        SchemaBrowserRow::Column {
            schema: "public".into(),
            table: "users".into(),
            name: "id".into(),
        },
    ];
    let out = filter_schema_browser_rows(rows, "email");
    // schema + table + email column.
    assert_eq!(out.len(), 3);
    assert!(matches!(&out[2], SchemaBrowserRow::Column { name, .. } if name == "email"));
}

#[test]
fn filter_schema_browser_rows_is_case_insensitive() {
    let rows = vec![SchemaBrowserRow::Schema {
        name: "PUBLIC".into(),
        expanded: false,
        table_count: 0,
    }];
    assert_eq!(filter_schema_browser_rows(rows, "pub").len(), 1);
}

#[test]
fn schema_browser_slash_starts_filter_mode_with_empty_pattern() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    assert_eq!(a.mode, Mode::SchemaBrowserFilter);
    assert_eq!(a.schema_browser.filter.as_deref(), Some(""));
}

#[test]
fn schema_browser_filter_typing_narrows_tree_live() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    a.on_key(KeyEvent::from(KeyCode::Char('a')));
    a.on_key(KeyEvent::from(KeyCode::Char('u')));
    // Filter is "au"; only the `audit` schema matches.
    let rows = a.flattened_schema_browser();
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0], SchemaBrowserRow::Schema { name, .. } if name == "audit"));
}

#[test]
fn schema_browser_filter_enter_accepts_keeps_filter_applied() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    a.on_key(KeyEvent::from(KeyCode::Char('a')));
    a.on_key(KeyEvent::from(KeyCode::Char('u')));
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::SchemaBrowser);
    assert_eq!(a.schema_browser.filter.as_deref(), Some("au"));
}

#[test]
fn schema_browser_filter_esc_clears() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    a.on_key(KeyEvent::from(KeyCode::Char('a')));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::SchemaBrowser);
    assert!(a.schema_browser.filter.is_none());
}

fn synthetic_browser_rows() -> Vec<SchemaBrowserRow> {
    // schema A (expanded) → tA1 (expanded) → col1, col2 → schema B
    vec![
        SchemaBrowserRow::Schema {
            name: "a".into(),
            expanded: true,
            table_count: 1,
        },
        SchemaBrowserRow::Table {
            schema: "a".into(),
            name: "tA1".into(),
            expanded: true,
            column_count: 2,
            constraint_count: 0,
        },
        SchemaBrowserRow::Column {
            schema: "a".into(),
            table: "tA1".into(),
            name: "col1".into(),
        },
        SchemaBrowserRow::Column {
            schema: "a".into(),
            table: "tA1".into(),
            name: "col2".into(),
        },
        SchemaBrowserRow::Schema {
            name: "b".into(),
            expanded: false,
            table_count: 0,
        },
    ]
}

#[test]
fn next_schema_row_idx_skips_past_table_internals_forward() {
    let rows = synthetic_browser_rows();
    // From schema "a" at index 0 → next schema is "b" at 4,
    // jumping over its table + columns in one move.
    assert_eq!(next_schema_row_idx(&rows, 0, Direction::Forward), Some(4));
    // From a column row (depth 2) the next schema is still "b".
    assert_eq!(next_schema_row_idx(&rows, 3, Direction::Forward), Some(4));
    // From the last schema → no next.
    assert_eq!(next_schema_row_idx(&rows, 4, Direction::Forward), None);
}

#[test]
fn next_schema_row_idx_walks_back_skipping_internals() {
    let rows = synthetic_browser_rows();
    // From schema "b" at 4 → previous schema is "a" at 0.
    assert_eq!(next_schema_row_idx(&rows, 4, Direction::Backward), Some(0));
    // From a column (index 2) → previous schema is "a".
    assert_eq!(next_schema_row_idx(&rows, 2, Direction::Backward), Some(0));
    // From the first schema → no previous.
    assert_eq!(next_schema_row_idx(&rows, 0, Direction::Backward), None);
}

#[test]
fn schema_browser_bracket_keys_jump_by_schema() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    // Expand "public" so we have schema + tables + (collapsed)
    // schema below for an interesting jump.
    a.schema_browser.expanded.insert("public".into());
    // Cursor at row 0 (audit schema, first).
    a.schema_browser.cursor = 0;
    // `]` jumps to the next schema row.
    a.on_key(KeyEvent::from(KeyCode::Char(']')));
    let rows = a.flattened_schema_browser();
    assert!(matches!(
        rows.get(a.schema_browser.cursor),
        Some(SchemaBrowserRow::Schema { name, .. }) if name == "public"
    ));
    // `[` goes back.
    a.on_key(KeyEvent::from(KeyCode::Char('[')));
    let rows = a.flattened_schema_browser();
    assert!(matches!(
        rows.get(a.schema_browser.cursor),
        Some(SchemaBrowserRow::Schema { name, .. }) if name == "audit"
    ));
}

#[test]
fn schema_browser_plus_expands_everything() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    assert_eq!(a.flattened_schema_browser().len(), 2); // only schemas
    a.on_key(KeyEvent::from(KeyCode::Char('+')));
    let rows = a.flattened_schema_browser();
    // Both schemas expanded → schemas + tables visible.
    // audit(1 table) + public(2 tables): 2 + 1 + 2 = 5 rows
    // minimum (tables aren't expanded — they have no columns
    // in the test fixture for `audit.events` / `public.orders`,
    // so toggling them doesn't add rows). public.users has 2
    // columns → +2 rows when its table-key is expanded. Total = 7.
    assert!(
        rows.len() >= 5,
        "expected expansion; got {} rows",
        rows.len()
    );
    // Every schema is marked expanded.
    for row in &rows {
        if let SchemaBrowserRow::Schema { expanded, .. } = row {
            assert!(*expanded, "schema not expanded: {row:?}");
        }
    }
}

#[test]
fn schema_browser_minus_collapses_everything() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    // First expand, then collapse, verify back to just schemas.
    a.on_key(KeyEvent::from(KeyCode::Char('+')));
    assert!(a.flattened_schema_browser().len() > 2);
    a.on_key(KeyEvent::from(KeyCode::Char('-')));
    // Back to one row per schema.
    assert_eq!(a.flattened_schema_browser().len(), 2);
    assert!(a.schema_browser.expanded.is_empty());
}

#[test]
fn schema_browser_pagedown_jumps_ten_rows() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    // Synthetic: drive enough rows by expanding everything.
    a.on_key(KeyEvent::from(KeyCode::Char('+')));
    a.schema_browser.cursor = 0;
    a.on_key(KeyEvent::from(KeyCode::PageDown));
    let rows_len = a.flattened_schema_browser().len();
    let expected = 10usize.min(rows_len.saturating_sub(1));
    assert_eq!(a.schema_browser.cursor, expected);
}

#[test]
fn start_schema_lint_with_empty_cache_surfaces_hint() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.start_schema_lint();
    assert_ne!(a.mode, Mode::SchemaLint);
    assert!(a
        .last_status
        .as_deref()
        .unwrap_or("")
        .contains("schema cache empty"));
}

#[test]
fn start_schema_lint_with_findings_opens_panel_and_summarises() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let cache = crate::query::schema::SchemaCache {
        schemas: vec!["public".into()],
        // Two LINT001s (no constraints), one LINT002 (mixed-case).
        tables: vec![
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "events".into(),
            },
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "OrderItems".into(),
            },
        ],
        ..Default::default()
    };
    a.schema_cache = cache;
    a.start_schema_lint();
    assert_eq!(a.mode, Mode::SchemaLint);
    assert!(!a.schema_lint.findings.is_empty());
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("finding(s)") && status.contains("high"),
        "status should summarise count + severity; got: {status}"
    );
}

#[test]
fn m_then_letter_sets_grid_bookmark_at_focus() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["a".into(), "b".into()],
        rows: vec![vec!["1".into(), "2".into()], vec!["3".into(), "4".into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0, 1];
    a.grid_state.select(Some(1));
    a.grid_view.col_cursor = 1;
    // m, then 'q'.
    a.on_key(KeyEvent::from(KeyCode::Char('m')));
    assert!(a.pending_mark_set);
    a.on_key(KeyEvent::from(KeyCode::Char('q')));
    assert!(!a.pending_mark_set);
    let bm = a.bookmarks.get(&'q').copied().expect("bookmark set");
    assert_eq!(bm.row, 1);
    assert_eq!(bm.col, 1);
}

#[test]
fn jump_to_bookmark_moves_selection_and_col_cursor() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["a".into(), "b".into()],
        rows: vec![vec!["1".into(), "2".into()], vec!["3".into(), "4".into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0, 1];
    a.bookmarks.insert('a', GridBookmark { row: 1, col: 1 });
    // 'a → jumps.
    a.on_key(KeyEvent::from(KeyCode::Char('\'')));
    a.on_key(KeyEvent::from(KeyCode::Char('a')));
    assert_eq!(a.grid_state.selected(), Some(1));
    assert_eq!(a.grid_view.col_cursor, 1);
}

#[test]
fn jump_to_unset_bookmark_surfaces_status_no_op() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["a".into()],
        rows: vec![vec!["1".into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.on_key(KeyEvent::from(KeyCode::Char('\'')));
    a.on_key(KeyEvent::from(KeyCode::Char('z')));
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("no bookmark"));
}

#[test]
fn m_followed_by_non_letter_clears_pending_silently() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Char('m')));
    assert!(a.pending_mark_set);
    a.on_key(KeyEvent::from(KeyCode::Char('1')));
    // Pending cleared, no bookmark set.
    assert!(!a.pending_mark_set);
    assert!(a.bookmarks.is_empty());
}

#[test]
fn fk_navigate_with_no_grid_source_surfaces_actionable_error() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid_view.source = None;
    a.grid = Grid {
        columns: vec!["id".into()],
        rows: vec![vec!["1".into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.navigate_fk_from_focused_cell();
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(err.contains("single-table SELECT"));
}

#[test]
fn fk_navigate_with_non_fk_column_surfaces_hint() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid_view.source = Some(("public".into(), "orders".into()));
    a.grid = Grid {
        columns: vec!["id".into()],
        rows: vec![vec!["1".into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.grid_view.col_cursor = 0;
    a.navigate_fk_from_focused_cell();
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(err.contains("isn't a foreign key"));
}

#[test]
fn fk_navigate_opens_new_tab_with_parent_select() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid_view.source = Some(("public".into(), "orders".into()));
    a.grid = Grid {
        columns: vec!["id".into(), "user_id".into()],
        rows: vec![vec!["1".into(), "42".into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0];
    a.grid_state.select(Some(0));
    a.grid_view.col_cursor = 1; // user_id
    a.schema_cache.fk_edges.push(crate::query::schema::FkEdge {
        child_schema: "public".into(),
        child_table: "orders".into(),
        child_column: "user_id".into(),
        parent_schema: "public".into(),
        parent_table: "users".into(),
        parent_column: "id".into(),
    });
    a.navigate_fk_from_focused_cell();
    // New tab opened.
    assert_eq!(a.tabs.len(), 2);
    assert_eq!(a.active_tab, 1);
    // Editor in the new tab holds the parent SELECT.
    assert!(
        a.editor
            .buffer
            .contains("SELECT * FROM public.users WHERE id = 42"),
        "expected parent select; got: {}",
        a.editor.buffer
    );
    // We're in the editor ready to F5.
    assert_eq!(a.mode, Mode::Editor);
}

#[test]
fn new_tab_pushes_a_fresh_state_and_switches() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "tab one".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert_eq!(a.tabs.len(), 2);
    assert_eq!(a.active_tab, 1);
    // New tab's editor is empty.
    assert_eq!(a.editor.buffer, "");
}

/// `databases` is app-level (not part of `TabSnapshot`), so a fresh
/// tab must (a) start with an empty grid — the start card shows there
/// too, same as the very first tab right after connect — and (b)
/// still see the same `databases` list as every other tab, since
/// they all share one connection.
#[test]
fn new_tab_has_empty_grid_and_shares_app_level_databases() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.databases = vec![DatabaseInfo {
        name: "main".into(),
        size: "1.2 GB".into(),
    }];
    a.grid = Grid {
        columns: vec!["id".into()],
        rows: vec![vec!["1".into()]],
        truncated: false,
    };
    a.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert_eq!(a.tabs.len(), 2);
    assert_eq!(a.active_tab, 1);
    assert!(
        a.grid.columns.is_empty(),
        "a new tab's grid should start empty: {:?}",
        a.grid
    );
    assert_eq!(
        a.databases,
        vec![DatabaseInfo {
            name: "main".into(),
            size: "1.2 GB".into(),
        }],
        "databases is app-level — a new tab must still see it"
    );
}

#[test]
fn cycle_tab_round_trips_state_per_tab() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "one".into();
    a.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    a.editor.buffer = "two".into();
    // Cycle back to first tab.
    a.cycle_tab(false);
    assert_eq!(a.editor.buffer, "one");
    assert_eq!(a.active_tab, 0);
    // Forward to second.
    a.cycle_tab(true);
    assert_eq!(a.editor.buffer, "two");
    assert_eq!(a.active_tab, 1);
}

#[test]
fn all_prompt_modes_count_as_text_input() {
    // Every text-entry mode must opt into is_text_input so the
    // global Ctrl-W (close-tab) chord stays inert while typing.
    for m in [
        Mode::Editor,
        Mode::ParamPrompt,
        Mode::SavedQueriesFilter,
        Mode::RenameQueryPrompt,
        Mode::SaveQueryPrompt,
        Mode::GridFilter,
        Mode::GridFind,
        Mode::HistorySearch,
        Mode::SchemaBrowserFilter,
    ] {
        assert!(m.is_text_input(), "{m:?} should be a text-input mode");
    }
    assert!(!Mode::Normal.is_text_input());
    assert!(!Mode::ResultDiff.is_text_input());
    assert!(!Mode::TapMonitor.is_text_input());
}

#[test]
fn ctrl_w_in_a_prompt_does_not_close_the_tab() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.new_tab(); // two tabs, so close_active_tab would otherwise fire
    assert_eq!(a.tabs.len(), 2);
    a.mode = Mode::ParamPrompt;
    a.saved_ui.param_prompt = Some(ParamPrompt {
        query_name: "q".into(),
        template: "SELECT :x".into(),
        params: vec!["x".into()],
        idx: 0,
        values: Vec::new(),
        input: TextInput::new(),
    });
    a.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(
        a.tabs.len(),
        2,
        "Ctrl-W must not close a tab while typing in a prompt"
    );
    assert_eq!(a.mode, Mode::ParamPrompt);
}

#[test]
fn result_diff_pin_is_per_tab_and_does_not_leak() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["id".into()],
        rows: vec![vec!["1".into()]],
        truncated: false,
    };
    // Pin A on tab 1.
    a.pin_or_diff_result();
    assert!(
        a.result_diff.pinned.is_some(),
        "tab 1 should have a pinned A"
    );
    // A fresh tab must NOT inherit the pin — otherwise the first D
    // there diffs against an unrelated baseline.
    a.new_tab();
    assert!(
        a.result_diff.pinned.is_none(),
        "a fresh tab must start with no pinned baseline"
    );
    // Returning to tab 1 restores its pin.
    a.cycle_tab(false);
    assert!(
        a.result_diff.pinned.is_some(),
        "returning to tab 1 should restore its pinned baseline"
    );
}

#[test]
fn tab_switch_dismisses_an_open_result_diff_overlay() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["id".into()],
        rows: vec![vec!["1".into()]],
        truncated: false,
    };
    a.pin_or_diff_result(); // pin A
    a.grid.rows = vec![vec!["2".into()]];
    a.pin_or_diff_result(); // diff → opens the overlay
    assert_eq!(a.mode, Mode::ResultDiff);
    a.new_tab();
    // The transient overlay must not survive onto the new tab.
    assert_eq!(a.mode, Mode::Normal);
    assert!(a.result_diff.active.is_none());
}

#[test]
fn close_tab_drops_current_and_loads_neighbour() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.editor.buffer = "first".into();
    a.new_tab(); // → tab 2
    a.editor.buffer = "second".into();
    // Close the active (2nd) tab → load first.
    a.close_active_tab();
    assert_eq!(a.tabs.len(), 1);
    assert_eq!(a.active_tab, 0);
    assert_eq!(a.editor.buffer, "first");
}

#[test]
fn close_tab_on_only_tab_is_a_noop_with_hint() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.editor.buffer = "lonely".into();
    a.close_active_tab();
    assert_eq!(a.tabs.len(), 1);
    assert_eq!(a.editor.buffer, "lonely");
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("only one tab"));
}

#[test]
fn new_tab_refuses_past_cap() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    // Drive up to the cap.
    for _ in 1..TAB_CAP {
        a.new_tab();
    }
    assert_eq!(a.tabs.len(), TAB_CAP);
    a.new_tab(); // refuse
    assert_eq!(a.tabs.len(), TAB_CAP);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("max tabs"));
}

#[test]
fn alt_digit_jumps_directly_to_tab() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.editor.buffer = "t1".into();
    a.new_tab();
    a.editor.buffer = "t2".into();
    a.new_tab();
    a.editor.buffer = "t3".into();
    // Alt-1 → jump to the first tab.
    a.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
    assert_eq!(a.active_tab, 0);
    assert_eq!(a.editor.buffer, "t1");
}

#[test]
fn tab_switch_blocked_during_query_running() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "one".into();
    a.new_tab();
    a.editor.buffer = "two".into();
    a.query_running = true;
    // Try to switch back — should be blocked.
    let before = a.active_tab;
    a.switch_to_tab(0);
    assert_eq!(a.active_tab, before);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("query is running"));
}

#[test]
fn default_query_name_sanitises_to_kebab() {
    assert_eq!(
        default_query_name("SELECT * FROM users"),
        "select-from-users"
    );
    // 40-char take from the line, sanitised + trimmed.
    assert_eq!(
        default_query_name("WITH active AS (SELECT 1) SELECT * FROM active"),
        "with-active-as-select-1-select-from"
    );
    // Leading whitespace skipped.
    assert_eq!(default_query_name("  \n\n select 1"), "select-1");
    // Symbols collapse to nothing; runs of space collapse.
    assert_eq!(default_query_name("a    b"), "a-b");
}

/// A throwaway `saved.toml` path for a test. Every test that can
/// reach `persist_saved_queries` MUST inject one: `App::new` points
/// at the operator's real file, and a test that presses Enter in the
/// save prompt against that has already overwritten a maintainer's
/// `saved.toml` once.
fn temp_saved_path(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pgman-saved-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("saved.toml");
    let _ = std::fs::remove_file(&path);
    path
}

/// Ctrl-S from the editor, overwrite the pre-filled name with `name`,
/// Enter.
fn save_via_prompt(a: &mut App, name: &str) {
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(a.mode, Mode::SaveQueryPrompt);
    a.saved_ui.save_name.clear();
    for c in name.chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    a.on_key(KeyEvent::from(KeyCode::Enter));
}

#[test]
fn save_query_prompt_persists_buffer_under_name() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.saved_queries_file = temp_saved_path("persists");
    a.mode = Mode::Editor;
    a.editor.buffer = "select 1".into();
    a.editor.cursor = a.editor.buffer.len();
    // Ctrl-S — open the prompt.
    a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(a.mode, Mode::SaveQueryPrompt);
    // Type a name (the default is pre-filled but we overwrite).
    a.saved_ui.save_name.clear();
    for c in "mine".chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    // Enter persists.
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::Editor);
    let q = a.saved_queries.get("mine").expect("entry saved");
    assert_eq!(q.body, "select 1");
}

#[test]
fn save_query_prompt_esc_cancels_without_persist() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "select 1".into();
    a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Editor);
    assert!(a.saved_queries.entries.is_empty());
}

#[test]
fn ctrl_s_in_a_real_session_writes_the_injected_saved_toml() {
    // The control for the demo tests below: same keystrokes, no
    // `--demo`, and the file appears with the entry in it.
    let path = temp_saved_path("real-save");
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.saved_queries_file = path.clone();
    a.editor.buffer = "select 1".into();
    a.editor.cursor = a.editor.buffer.len();
    save_via_prompt(&mut a, "mine");
    assert_eq!(a.last_error, None);
    let on_disk = crate::saved::load_from(&path);
    assert_eq!(
        on_disk.get("mine").map(|q| q.body.as_str()),
        Some("select 1")
    );
}

#[test]
fn demo_ctrl_s_keeps_the_entry_in_memory_but_never_writes_saved_toml() {
    // `--demo` ships a fixture list of saved queries. Ctrl-S used to
    // persist that whole list to the operator's real `saved.toml`,
    // replacing their queries with the samples. The save must still
    // land in memory (the demo behaves) but nothing may reach disk.
    let path = temp_saved_path("demo-save");
    let mut a = crate::demo::app(Theme::default());
    a.saved_queries_file = path.clone();
    a.editor.buffer = "select 1".into();
    a.editor.cursor = a.editor.buffer.len();
    save_via_prompt(&mut a, "mine");
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(
        a.saved_queries.get("mine").map(|q| q.body.as_str()),
        Some("select 1"),
        "the demo still saves in memory"
    );
    assert!(
        !path.exists(),
        "--demo must never write saved.toml; found {}",
        path.display()
    );
}

#[test]
fn demo_delete_and_rename_never_write_saved_toml() {
    let path = temp_saved_path("demo-delete-rename");
    let mut a = crate::demo::app(Theme::default());
    a.saved_queries_file = path.clone();
    let first = a.saved_queries.entries[0].name.clone();
    // Delete the focused entry.
    a.open_saved_queries();
    assert_eq!(a.mode, Mode::SavedQueries);
    a.saved_ui.cursor = 0;
    a.on_key(KeyEvent::from(KeyCode::Char('d')));
    assert!(a.saved_queries.get(&first).is_none(), "deleted in memory");
    assert!(!path.exists(), "delete wrote saved.toml under --demo");
    // Rename the (new) focused entry.
    a.saved_ui.cursor = 0;
    a.on_key(KeyEvent::from(KeyCode::Char('r')));
    assert_eq!(a.mode, Mode::RenameQueryPrompt);
    a.saved_ui.rename_buf.clear();
    type_str(&mut a, "renamed-in-demo");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(
        a.saved_queries.get("renamed-in-demo").is_some(),
        "renamed in memory"
    );
    assert!(!path.exists(), "rename wrote saved.toml under --demo");
}

#[test]
fn saved_queries_panel_enter_loads_into_editor() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.saved_queries.upsert(crate::saved::SavedQuery {
        name: "ru".into(),
        body: "SELECT now();".into(),
    });
    a.mode = Mode::Normal;
    a.editor.buffer = "draft".into();
    // Q opens.
    a.on_key(KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT));
    assert_eq!(a.mode, Mode::SavedQueries);
    // Enter loads into editor.
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor.buffer, "SELECT now();");
}

#[test]
fn saved_queries_panel_d_deletes_focused_entry() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.saved_queries_file = temp_saved_path("panel-delete");
    a.saved_queries.upsert(crate::saved::SavedQuery {
        name: "a".into(),
        body: "select 1".into(),
    });
    a.saved_queries.upsert(crate::saved::SavedQuery {
        name: "b".into(),
        body: "select 2".into(),
    });
    a.mode = Mode::SavedQueries;
    a.saved_ui.cursor = 0;
    a.on_key(KeyEvent::from(KeyCode::Char('d')));
    assert_eq!(a.saved_queries.entries.len(), 1);
    assert_eq!(a.saved_queries.entries[0].name, "b");
}

#[test]
fn open_saved_queries_with_empty_list_surfaces_hint() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.open_saved_queries();
    assert_ne!(a.mode, Mode::SavedQueries);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("Ctrl-S"));
}

#[test]
fn notification_message_appends_to_ring_and_caps() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // Drive past the cap.
    for i in 0..(NOTIFICATION_CAP + 50) {
        a.on_msg(AppMsg::Notification {
            generation: a.generation,
            notification: crate::conn::NotificationMsg {
                channel: "users".into(),
                pid: 1234,
                payload: format!("event-{i}"),
            },
        });
    }
    assert_eq!(a.notifications.items.len(), NOTIFICATION_CAP);
    // Newest at the end.
    let last = a.notifications.items.last().unwrap();
    assert_eq!(last.payload, format!("event-{}", NOTIFICATION_CAP + 49));
}

// --- tap-event ring tests ------------------------

fn tap_query(sql: &str, received_at: u64) -> crate::tap::TapEvent {
    crate::tap::TapEvent {
        v: 1,
        kind: crate::tap::TapKind::Query,
        ts_unix_micros: received_at,
        received_at_unix_micros: received_at,
        app: Some("billing-service".into()),
        pool: None,
        conn: Some("primary-7".into()),
        txn: None,
        sql: Some(sql.into()),
        params: None,
        params_redacted: false,
        duration_micros: Some(100),
        rows: Some(1),
        error: None,
        caller: None,
        dropped_events_total: None,
        txn_outcome: None,
    }
}

fn tap_heartbeat(dropped: u64, received_at: u64) -> crate::tap::TapEvent {
    crate::tap::TapEvent {
        v: 1,
        kind: crate::tap::TapKind::Heartbeat,
        ts_unix_micros: received_at,
        received_at_unix_micros: received_at,
        app: Some("billing-service".into()),
        pool: None,
        conn: None,
        txn: None,
        sql: None,
        params: None,
        params_redacted: false,
        duration_micros: None,
        rows: None,
        error: None,
        caller: None,
        dropped_events_total: Some(dropped),
        txn_outcome: None,
    }
}

#[test]
fn tap_query_event_lands_in_ring_and_bumps_count() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 1", 1_000_000),
    });
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 2", 2_000_000),
    });
    assert_eq!(a.tap_events.len(), 2);
    assert_eq!(a.tap_health.query_count, 2);
    // Newest at the back.
    assert_eq!(
        a.tap_events.back().and_then(|e| e.sql.as_deref()),
        Some("SELECT 2")
    );
    assert_eq!(a.tap_health.last_event_at_unix_micros, 2_000_000);
}

#[test]
fn tap_heartbeat_does_not_pollute_ring_but_updates_health() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 1", 1_000_000),
    });
    a.on_msg(AppMsg::TapEvent {
        event: tap_heartbeat(17, 1_500_000),
    });
    // Ring only carries the query — heartbeat stays out.
    assert_eq!(a.tap_events.len(), 1);
    assert_eq!(a.tap_health.heartbeat_count, 1);
    assert_eq!(a.tap_health.dropped_events_total, 17);
    // Heartbeat still counts as a "we heard from the JAR" signal.
    assert_eq!(a.tap_health.last_event_at_unix_micros, 1_500_000);
}

#[test]
fn tap_ring_evicts_oldest_past_cap() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    for i in 0..(TAP_CAP + 50) {
        a.on_msg(AppMsg::TapEvent {
            event: tap_query(&format!("q{i}"), i as u64),
        });
    }
    assert_eq!(a.tap_events.len(), TAP_CAP);
    // First event surviving the eviction is q50 (the first 50 were dropped).
    assert_eq!(
        a.tap_events.front().and_then(|e| e.sql.as_deref()),
        Some("q50")
    );
    // Newest at the back.
    assert_eq!(
        a.tap_events.back().and_then(|e| e.sql.as_deref()),
        Some(format!("q{}", TAP_CAP + 49).as_str())
    );
    assert_eq!(a.tap_health.query_count, (TAP_CAP + 50) as u64);
}

#[test]
fn hotspots_reflect_a_push_and_eviction_that_share_a_timestamp() {
    // The memo behind `current_hotspots` used to key on
    // `(ring.len(), front.received_at, back.received_at)`. At
    // `TAP_CAP` a push is also a pop, so `len` doesn't move; and
    // `received_at` comes from `SystemTime::now()`, whose ~20 µs
    // granularity means adjacent events routinely share a stamp. All
    // three components can be identical across a mutation that
    // replaced the ring's entire content — and the panel then showed
    // an evicted statement while omitting the newest one.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    for _ in 0..TAP_CAP {
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT evicted", 1_000_000),
        });
    }
    let before = a.current_hotspots();
    assert_eq!(before.len(), 1, "one fingerprint so far: {before:?}");

    // Same length before and after, same front stamp, same back stamp.
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT newest", 1_000_000),
    });
    assert_eq!(a.tap_events.len(), TAP_CAP);

    let after = a.current_hotspots();
    assert!(
        after.iter().any(|h| h.fingerprint.contains("newest")),
        "the newest statement must be aggregated, not served from a stale memo: {after:?}"
    );
}

#[test]
fn tap_ring_eviction_keeps_cursor_aligned_with_content() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // Fill exactly to cap.
    for i in 0..TAP_CAP {
        a.on_msg(AppMsg::TapEvent {
            event: tap_query(&format!("q{i}"), i as u64),
        });
    }
    // Cursor parked on the oldest row.
    a.tap_nav.events_cursor = 0;
    let oldest_sql = a.tap_events.front().and_then(|e| e.sql.clone());
    assert_eq!(oldest_sql.as_deref(), Some("q0"));
    // One more event evicts q0; cursor stays in-bounds and
    // points at "what used to be the second row".
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("new", 9_999),
    });
    assert_eq!(a.tap_events.len(), TAP_CAP);
    assert_eq!(
        a.tap_events.front().and_then(|e| e.sql.as_deref()),
        Some("q1")
    );
    // Cursor decremented to follow the eviction.
    assert_eq!(a.tap_nav.events_cursor, 0);
}

#[test]
fn f4_opens_tap_monitor_from_any_mode() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::TapMonitor);
}

#[test]
fn tap_monitor_status_distinguishes_no_traffic_from_no_jar() {
    // Empty case.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.start_tap_monitor();
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("no events yet"),
        "expected no-events hint; got {status}"
    );
    // After traffic.
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 1", 1),
    });
    // Pretend the JAR also sent a heartbeat.
    a.on_msg(AppMsg::TapEvent {
        event: tap_heartbeat(0, 2),
    });
    a.mode = Mode::Normal;
    a.start_tap_monitor();
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("1 queries") && status.contains("1 heartbeats"),
        "expected counters in status; got {status}"
    );
}

#[test]
fn tap_monitor_q_closes_to_normal_and_clears_status() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.start_tap_monitor();
    a.on_key(KeyEvent::from(KeyCode::Char('q')));
    assert_eq!(a.mode, Mode::Normal);
    assert!(a.last_status.is_none());
}

#[test]
fn tap_monitor_c_clears_the_ring_and_resets_cursor() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    for i in 0..3 {
        a.on_msg(AppMsg::TapEvent {
            event: tap_query(&format!("q{i}"), i),
        });
    }
    a.start_tap_monitor();
    a.tap_nav.events_cursor = 2;
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert!(a.tap_events.is_empty());
    assert_eq!(a.tap_nav.events_cursor, 0);
    assert_eq!(a.last_status.as_deref(), Some("cleared 3 tap event(s)"));
}

#[test]
fn tap_monitor_v_cycles_through_seven_views() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 1", 1),
    });
    a.start_tap_monitor();
    assert_eq!(a.tap_nav.view, TapView::List);
    a.on_key(KeyEvent::from(KeyCode::Char('v')));
    assert_eq!(a.tap_nav.view, TapView::Hotspots);
    a.on_key(KeyEvent::from(KeyCode::Char('v')));
    assert_eq!(a.tap_nav.view, TapView::Callers);
    a.on_key(KeyEvent::from(KeyCode::Char('v')));
    assert_eq!(a.tap_nav.view, TapView::Transactions);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("transactions"),
        "expected transactions in status: {status}"
    );
    a.on_key(KeyEvent::from(KeyCode::Char('v')));
    assert_eq!(a.tap_nav.view, TapView::Pools);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("pools"),
        "expected pools in status: {status}"
    );
    a.on_key(KeyEvent::from(KeyCode::Char('v')));
    assert_eq!(a.tap_nav.view, TapView::NplusOne);
    a.on_key(KeyEvent::from(KeyCode::Char('v')));
    assert_eq!(a.tap_nav.view, TapView::Baseline);
    a.on_key(KeyEvent::from(KeyCode::Char('v')));
    assert_eq!(a.tap_nav.view, TapView::List);
}

#[test]
fn tap_monitor_pools_view_navigates_and_clears() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // Two pools: primary (two conns) and replica (one conn).
    for (pool, conn, i) in [
        ("primary", "p-1", 0u64),
        ("primary", "p-2", 1),
        ("replica", "r-1", 2),
    ] {
        let mut e = tap_query("SELECT 1", i);
        e.pool = Some(pool.into());
        e.conn = Some(conn.into());
        e.ts_unix_micros = i;
        e.received_at_unix_micros = i;
        a.on_msg(AppMsg::TapEvent { event: e });
    }
    a.start_tap_monitor();
    a.tap_nav.view = TapView::Pools;
    let pools = a.current_pools();
    assert_eq!(pools.len(), 2);
    // Navigation clamps to the last row.
    a.on_key(KeyEvent::from(KeyCode::Char('G')));
    assert_eq!(a.tap_nav.pools_cursor, 1);
    a.on_key(KeyEvent::from(KeyCode::Char('k')));
    assert_eq!(a.tap_nav.pools_cursor, 0);
    // `c` clears the ring from the pools view too.
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert!(a.tap_events.is_empty());
    assert_eq!(a.tap_nav.pools_cursor, 0);
    assert!(a.current_pools().is_empty());
}

#[test]
fn tap_monitor_txns_view_navigates_and_clears() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // Two transactions: c-1#a (3 stmts, open), c-1#b (1 stmt, open).
    for i in 0..3u64 {
        let mut e = tap_query("SELECT a", i);
        e.txn = Some("c-1#a".into());
        e.ts_unix_micros = i;
        e.received_at_unix_micros = i;
        a.on_msg(AppMsg::TapEvent { event: e });
    }
    let mut e = tap_query("SELECT b", 100);
    e.txn = Some("c-1#b".into());
    e.ts_unix_micros = 100;
    e.received_at_unix_micros = 100;
    a.on_msg(AppMsg::TapEvent { event: e });
    a.start_tap_monitor();
    a.tap_nav.view = TapView::Transactions;
    let txns = a.current_txns();
    assert_eq!(txns.len(), 2);
    assert!(txns.iter().all(|t| t.is_open()));
    // c-1#a has the bigger span (0..2 = 2µs) so sorts first.
    assert_eq!(txns[0].txn.as_deref(), Some("c-1#a"));
    // c clears the ring → 0 transactions.
    a.tap_nav.txns_cursor = 1;
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert!(a.current_txns().is_empty());
    assert_eq!(a.tap_nav.txns_cursor, 0);
}

#[test]
fn tap_monitor_callers_view_navigates_sorts_and_clears() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // Three events from two different callers.
    for (i, caller) in [
        ("OrderService.findById:42", 100),
        ("OrderService.findById:42", 200),
        ("UserService.lookup:7", 50),
    ]
    .into_iter()
    .enumerate()
    {
        let (frame, dur) = caller;
        let mut e = tap_query(&format!("SELECT {i}"), i as u64);
        e.caller = Some(vec![frame.into()]);
        e.duration_micros = Some(dur);
        a.on_msg(AppMsg::TapEvent { event: e });
    }
    a.start_tap_monitor();
    a.tap_nav.view = TapView::Callers;
    let groups = a.current_callers();
    assert_eq!(groups.len(), 2);
    // TotalTime sort default — OrderService bucket wins (100+200=300 > 50).
    assert_eq!(groups[0].caller, "OrderService.findById:42");
    // `s` cycles to CallCount; OrderService also wins (2 > 1).
    a.on_key(KeyEvent::from(KeyCode::Char('s')));
    assert_eq!(a.tap_nav.sort, crate::tap::HotspotSort::CallCount);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("callers · sort"), "got: {status}");
    // `c` clears; cursors reset.
    a.tap_nav.callers_cursor = 1;
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert!(a.current_callers().is_empty());
    assert_eq!(a.tap_nav.callers_cursor, 0);
}

#[test]
fn shift_b_captures_baseline_from_any_tap_view() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // Seed two distinct fingerprints so the hotspots
    // bucket count is meaningful.
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT a FROM t1", 1),
    });
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT b FROM t2", 2),
    });
    a.start_tap_monitor();
    assert!(a.tap_baseline.is_none());
    // Shift-B from the default List view captures.
    a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
    let baseline = a.tap_baseline.as_ref().expect("baseline captured");
    assert_eq!(baseline.hotspots.len(), 2);
    assert_eq!(baseline.captured_event_count, 2);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("baseline captured"),
        "expected confirmation status: {status}"
    );
}

#[test]
fn baseline_diff_flags_new_fingerprint_after_capture() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT a FROM t1", 1),
    });
    a.start_tap_monitor();
    a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
    // New fingerprint arrives post-baseline.
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT b FROM t2", 2),
    });
    let diff = a.current_baseline_diff();
    // Old "select a from t?" is unchanged (filtered);
    // new "select b from t?" surfaces.
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].kind, crate::tap::DiffKind::New);
}

#[test]
fn baseline_clear_keeps_snapshot_clears_ring() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 1", 1),
    });
    a.start_tap_monitor();
    a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
    a.tap_nav.view = TapView::Baseline;
    // c clears the ring; the captured snapshot survives
    // (operator might want to re-fill the ring against
    // the same baseline post-deploy).
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert!(a.tap_events.is_empty());
    assert!(a.tap_baseline.is_some(), "baseline must persist across `c`");
}

#[test]
fn baseline_records_listener_drop_watermark_at_capture() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 1", 1),
    });
    // Snapshot the global atomic before capture so the
    // assertion is robust to whatever other tests
    // contributed (cumulative-counter semantics).
    let baseline_drops = crate::tap::dropped_at_listener();
    a.start_tap_monitor();
    a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
    let captured = a.tap_baseline.as_ref().unwrap().captured_listener_dropped;
    assert!(
        captured >= baseline_drops,
        "captured_listener_dropped must reflect a counter snapshot at-or-after baseline read"
    );
    // delta-since-capture starts at zero (or whatever
    // concurrent tests added between capture and this read).
    let delta = a.baseline_listener_drops_since_capture().unwrap();
    assert_eq!(delta, crate::tap::dropped_at_listener() - captured);
}

#[test]
fn baseline_recapture_overwrites_previous_snapshot() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 1", 1),
    });
    a.start_tap_monitor();
    a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
    let first_count = a.tap_baseline.as_ref().unwrap().captured_event_count;
    assert_eq!(first_count, 1);
    // Two more events arrive; recapture.
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 2", 2),
    });
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 3", 3),
    });
    a.on_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
    let second_count = a.tap_baseline.as_ref().unwrap().captured_event_count;
    assert_eq!(second_count, 3, "recapture must reflect the larger ring");
}

#[test]
fn tap_monitor_v_cycle_includes_baseline_as_last_view() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.start_tap_monitor();
    a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Hotspots
    a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Callers
    a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Transactions
    a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Pools
    a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → NplusOne
    a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → Baseline
    assert_eq!(a.tap_nav.view, TapView::Baseline);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("baseline diff"),
        "expected baseline-diff status: {status}"
    );
    a.on_key(KeyEvent::from(KeyCode::Char('v'))); // → back to List
    assert_eq!(a.tap_nav.view, TapView::List);
}

#[test]
fn tap_monitor_nplus1_view_navigates_and_clears() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // 6 same-shape events in one txn within 200ms → 1
    // finding fires.
    for i in 0..6 {
        let mut e = tap_query("SELECT * FROM users WHERE id = ?", i * 20_000);
        e.txn = Some("c-1#1".into());
        e.ts_unix_micros = i * 20_000;
        e.received_at_unix_micros = i * 20_000;
        a.on_msg(AppMsg::TapEvent { event: e });
    }
    a.start_tap_monitor();
    a.tap_nav.view = TapView::NplusOne;
    let findings = a.current_nplus1();
    assert_eq!(findings.len(), 1);
    // Down past the end clamps.
    for _ in 0..5 {
        a.on_key(KeyEvent::from(KeyCode::Char('j')));
    }
    assert_eq!(a.tap_nav.nplus1_cursor, 0);
    // c clears the ring → no findings.
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert!(a.current_nplus1().is_empty());
    assert_eq!(a.tap_nav.nplus1_cursor, 0);
}

#[test]
fn tap_monitor_g_capital_jumps_to_end_in_both_views() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    for i in 0..5 {
        a.on_msg(AppMsg::TapEvent {
            event: tap_query(&format!("SELECT * FROM t{i}"), i),
        });
    }
    a.start_tap_monitor();
    // List view: `G` jumps to last row.
    a.on_key(KeyEvent::from(KeyCode::Char('G')));
    assert_eq!(a.tap_nav.events_cursor, 4);
    // Toggle to hotspots; `G` jumps within the hotspot list.
    a.tap_nav.view = TapView::Hotspots;
    a.on_key(KeyEvent::from(KeyCode::Char('G')));
    let hotspots = a.current_hotspots();
    assert_eq!(a.tap_nav.hotspots_cursor, hotspots.len().saturating_sub(1));
}

#[test]
fn tap_monitor_s_cycles_sort_in_hotspots_view() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.on_msg(AppMsg::TapEvent {
        event: tap_query("SELECT 1", 1),
    });
    a.start_tap_monitor();
    a.tap_nav.view = TapView::Hotspots;
    assert_eq!(a.tap_nav.sort, crate::tap::HotspotSort::TotalTime);
    a.on_key(KeyEvent::from(KeyCode::Char('s')));
    assert_eq!(a.tap_nav.sort, crate::tap::HotspotSort::CallCount);
    a.on_key(KeyEvent::from(KeyCode::Char('s')));
    assert_eq!(a.tap_nav.sort, crate::tap::HotspotSort::P95Latency);
    a.on_key(KeyEvent::from(KeyCode::Char('s')));
    assert_eq!(a.tap_nav.sort, crate::tap::HotspotSort::TotalTime);
}

#[test]
fn tap_monitor_s_in_list_view_is_a_noop() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.start_tap_monitor();
    let sort_before = a.tap_nav.sort;
    a.on_key(KeyEvent::from(KeyCode::Char('s')));
    assert_eq!(a.tap_nav.sort, sort_before, "list view ignores `s`");
    assert_eq!(a.tap_nav.view, TapView::List);
}

#[test]
fn tap_monitor_hotspots_clear_resets_both_cursors() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    for i in 0..3 {
        a.on_msg(AppMsg::TapEvent {
            event: tap_query(&format!("q{i}"), i),
        });
    }
    a.start_tap_monitor();
    a.tap_nav.view = TapView::Hotspots;
    a.tap_nav.hotspots_cursor = 2;
    a.tap_nav.events_cursor = 2;
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert!(a.tap_events.is_empty());
    assert_eq!(a.tap_nav.hotspots_cursor, 0);
    assert_eq!(a.tap_nav.events_cursor, 0);
}

#[test]
fn current_hotspots_reflects_current_sort() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // Bucket A: many cheap calls.
    for _ in 0..50 {
        a.on_msg(AppMsg::TapEvent {
            event: tap_query("SELECT a FROM t_a", 1),
        });
    }
    // Bucket B: one expensive call.
    let mut spike = tap_query("SELECT b FROM t_b", 1_000_000);
    spike.duration_micros = Some(1_000_000);
    a.on_msg(AppMsg::TapEvent { event: spike });
    a.tap_nav.sort = crate::tap::HotspotSort::TotalTime;
    let by_total = a.current_hotspots();
    assert_eq!(by_total[0].count, 1, "expensive spike wins on total time");
    a.tap_nav.sort = crate::tap::HotspotSort::CallCount;
    let by_count = a.current_hotspots();
    assert_eq!(by_count[0].count, 50, "cheap bucket wins on call count");
}

#[test]
fn tap_monitor_jk_navigation_clamps_at_ends() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    for i in 0..3 {
        a.on_msg(AppMsg::TapEvent {
            event: tap_query(&format!("q{i}"), i),
        });
    }
    a.start_tap_monitor();
    // Down past the end clamps to last.
    for _ in 0..10 {
        a.on_key(KeyEvent::from(KeyCode::Char('j')));
    }
    assert_eq!(a.tap_nav.events_cursor, 2);
    // Up past the start clamps to 0.
    for _ in 0..10 {
        a.on_key(KeyEvent::from(KeyCode::Char('k')));
    }
    assert_eq!(a.tap_nav.events_cursor, 0);
}

#[test]
fn tap_message_is_not_generation_gated() {
    // Tap listener is independent of the DB connection; a
    // reconnect (which bumps generation) shouldn't drop tap
    // events that arrived after.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.generation = 42;
    let msg = AppMsg::TapEvent {
        event: tap_query("SELECT 1", 1_000),
    };
    // Generation accessor returns 0 (not 42) — the dispatcher
    // doesn't filter this.
    assert_eq!(msg.generation(), 0);
    a.on_msg(msg);
    assert_eq!(a.tap_events.len(), 1);
}

#[test]
fn f3_opens_notifications_from_any_mode() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Notifications);
}

#[test]
fn notifications_c_clears_the_ring() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Notifications;
    a.notifications.items = vec![crate::conn::NotificationMsg {
        channel: "x".into(),
        pid: 1,
        payload: "p".into(),
    }];
    a.on_key(KeyEvent::from(KeyCode::Char('c')));
    assert!(a.notifications.items.is_empty());
    assert_eq!(a.notifications.cursor, 0);
}

#[test]
fn capital_r_in_sessions_toggles_auto_refresh() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode_seen.insert(Mode::Sessions);
    a.mode = Mode::Sessions;
    assert!(!a.auto_refresh);
    a.on_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    assert!(a.auto_refresh);
    // Status reflects the toggle.
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("auto-refresh on"));
    // Toggle off.
    a.on_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    assert!(!a.auto_refresh);
}

#[test]
fn tick_auto_refresh_noop_when_disabled() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Sessions;
    a.auto_refresh = false;
    a.auto_refresh_last = Some(std::time::Instant::now() - std::time::Duration::from_secs(60));
    a.tick_auto_refresh();
    // No refresh fired (client is None → refresh_sessions would
    // surface an error; we just check no panic / status change).
    assert!(a.last_error.is_none());
}

#[test]
fn tick_auto_refresh_noop_when_query_running() {
    // The tick must not stack a refresh on top of an in-flight
    // query — would surface stale-generation noise.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Sessions;
    a.auto_refresh = true;
    a.query_running = true;
    a.auto_refresh_last = Some(std::time::Instant::now() - std::time::Duration::from_secs(60));
    a.tick_auto_refresh();
    // last unchanged because we bailed.
    let elapsed = a.auto_refresh_last.unwrap().elapsed();
    assert!(elapsed >= std::time::Duration::from_secs(60));
}

#[test]
fn capital_k_in_sessions_opens_confirm_terminate_with_pid() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Sessions;
    a.sessions.rows = vec![crate::query::sessions::SessionRow {
        pid: 12345,
        user: "app".into(),
        application: "service-x".into(),
        state: "active".into(),
        age_secs: 42.0,
        blocked_by: String::new(),
        query: "SELECT * FROM events".into(),
        wait_event: None,
    }];
    a.sessions.cursor = 0;
    a.on_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT));
    assert_eq!(a.mode, Mode::ConfirmTerminate);
    assert_eq!(a.pending_terminate, Some(12345));
    // Status should mention the pid.
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("12345"),
        "expected pid in status; got: {status}"
    );
}

#[test]
fn confirm_terminate_n_cancels_and_clears_pending() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    // Seed `mode_seen` for Sessions so the cancel path's
    // status isn't overwritten by the first-entry tip.
    // Production flow always enters Sessions before opening
    // ConfirmTerminate.
    a.mode_seen.insert(Mode::Sessions);
    a.mode = Mode::ConfirmTerminate;
    a.pending_terminate = Some(999);
    a.on_key(KeyEvent::from(KeyCode::Char('n')));
    assert_eq!(a.mode, Mode::Sessions);
    assert!(a.pending_terminate.is_none());
    assert_eq!(a.last_status.as_deref(), Some("terminate cancelled"));
}

#[test]
fn confirm_terminate_esc_cancels() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode_seen.insert(Mode::Sessions);
    a.mode = Mode::ConfirmTerminate;
    a.pending_terminate = Some(123);
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Sessions);
    assert!(a.pending_terminate.is_none());
}

#[test]
fn capital_k_with_empty_session_list_does_not_open_confirm() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Sessions;
    a.sessions.rows.clear();
    a.on_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT));
    // No session to terminate → stay in Sessions, no pending.
    assert_eq!(a.mode, Mode::Sessions);
    assert!(a.pending_terminate.is_none());
}

#[test]
fn compute_grid_find_matches_finds_all_hits_in_row_major_order() {
    let grid = Grid {
        columns: vec!["name".into(), "city".into()],
        rows: vec![
            vec!["alice".into(), "London".into()],
            vec!["bob".into(), "Berlin".into()],
            vec!["carol".into(), "London".into()],
        ],
        truncated: false,
    };
    let visible: Vec<usize> = (0..grid.rows.len()).collect();
    let matches = compute_grid_find_matches(&grid, &visible, "lon");
    // Cells "London" in rows 0 and 2 at col 1.
    assert_eq!(matches, vec![(0, 1), (2, 1)]);
}

#[test]
fn compute_grid_find_matches_honours_visible_subset() {
    let grid = Grid {
        columns: vec!["name".into()],
        rows: vec![
            vec!["alice".into()],
            vec!["bob".into()],
            vec!["alex".into()],
        ],
        truncated: false,
    };
    // Filter has hidden row 1 ("bob"). visible_rows is the
    // post-filter index list.
    let visible = vec![0, 2];
    let matches = compute_grid_find_matches(&grid, &visible, "al");
    // Visible-row indices: 0 → grid row 0 (alice), 1 → grid row 2 (alex).
    assert_eq!(matches, vec![(0, 0), (1, 0)]);
}

#[test]
fn compute_grid_find_matches_empty_needle_returns_empty() {
    let grid = Grid {
        columns: vec!["x".into()],
        rows: vec![vec!["any".into()]],
        truncated: false,
    };
    assert!(compute_grid_find_matches(&grid, &[0], "").is_empty());
}

#[test]
fn grid_find_f_key_opens_mode_and_jumps_on_match() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["name".into(), "city".into()],
        rows: vec![
            vec!["a".into(), "London".into()],
            vec!["b".into(), "Berlin".into()],
        ],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0, 1];
    a.grid_state.select(Some(0));
    a.on_key(KeyEvent::from(KeyCode::Char('f')));
    assert_eq!(a.mode, Mode::GridFind);
    // Type "ber" — should jump to row 1 col 1.
    a.on_key(KeyEvent::from(KeyCode::Char('b')));
    a.on_key(KeyEvent::from(KeyCode::Char('e')));
    a.on_key(KeyEvent::from(KeyCode::Char('r')));
    assert_eq!(a.grid_state.selected(), Some(1));
    assert_eq!(a.grid_view.col_cursor, 1);
    // Enter accepts.
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn grid_find_n_and_capital_n_step_through_matches() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.grid = Grid {
        columns: vec!["c".into()],
        rows: vec![vec!["aa".into()], vec!["bb".into()], vec!["aa".into()]],
        truncated: false,
    };
    a.grid_view.visible_rows = vec![0, 1, 2];
    a.grid_state.select(Some(0));
    a.on_key(KeyEvent::from(KeyCode::Char('f')));
    // Type "a" — two matches (rows 0 and 2). Cursor jumps to first.
    a.on_key(KeyEvent::from(KeyCode::Char('a')));
    assert_eq!(a.grid_state.selected(), Some(0));
    // `n` cycles to second match.
    a.on_key(KeyEvent::from(KeyCode::Char('n')));
    assert_eq!(a.grid_state.selected(), Some(2));
    // `n` again wraps to first.
    a.on_key(KeyEvent::from(KeyCode::Char('n')));
    assert_eq!(a.grid_state.selected(), Some(0));
    // `N` (capital) walks back.
    a.on_key(KeyEvent::from(KeyCode::Char('N')));
    assert_eq!(a.grid_state.selected(), Some(2));
}

#[test]
fn f2_after_failure_opens_error_detail_with_rich_fields() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.last_error = Some("duplicate key value violates unique constraint".into());
    a.last_error_detail = Some(crate::conn::QueryErrDetail {
        code: Some("23505".into()),
        severity: Some("ERROR".into()),
        constraint: Some("users_email_key".into()),
        table: Some("users".into()),
        schema: Some("public".into()),
        ..Default::default()
    });
    a.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::ErrorDetail);
    // Close → back where F2 was pressed (the editor).
    a.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Editor);
}

#[test]
fn f2_with_no_error_surfaces_status_not_overlay() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    // No last_error / last_error_detail.
    a.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.last_status.as_deref(), Some("no error to expand"));
}

#[test]
fn query_ok_clears_last_error_detail() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.last_error_detail = Some(crate::conn::QueryErrDetail {
        code: Some("23505".into()),
        ..Default::default()
    });
    a.on_msg(AppMsg::QueryOk {
        generation: a.generation,
        grid: crate::grid::Grid {
            columns: vec!["x".into()],
            rows: vec![],
            truncated: false,
        },
        kind_label: "SELECT".into(),
        tx_open_after: false,
    });
    assert!(a.last_error_detail.is_none());
}

#[test]
fn query_ok_status_flags_truncated_grids() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let rows: Vec<Vec<String>> = (0..crate::grid::MAX_ROWS)
        .map(|i| vec![i.to_string()])
        .collect();
    a.on_msg(AppMsg::QueryOk {
        generation: a.generation,
        grid: crate::grid::Grid {
            columns: vec!["id".into()],
            rows,
            truncated: true,
        },
        kind_label: "SELECT".into(),
        tx_open_after: false,
    });
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains(&format!("capped at {}", crate::grid::MAX_ROWS)),
        "expected truncation hint in status, got: {status}"
    );
    assert!(a.grid.truncated);
}

#[test]
fn query_ok_status_omits_cap_when_not_truncated() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.on_msg(AppMsg::QueryOk {
        generation: a.generation,
        grid: crate::grid::Grid {
            columns: vec!["id".into()],
            rows: vec![vec!["1".into()]],
            truncated: false,
        },
        kind_label: "SELECT".into(),
        tx_open_after: false,
    });
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(!status.contains("capped"), "unexpected cap hint: {status}");
}

#[test]
fn backslash_d_with_target_opens_browser_with_filter() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\d users".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(a.mode, Mode::SchemaBrowser);
    assert_eq!(a.schema_browser.filter.as_deref(), Some("users"));
    // Buffer cleared so a second F5 doesn't re-fire.
    assert!(a.editor.buffer.is_empty());
}

#[test]
fn backslash_d_without_target_just_opens_browser() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\d".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(a.mode, Mode::SchemaBrowser);
    assert!(a.schema_browser.filter.is_none());
}

#[test]
fn backslash_help_routes_to_help_overlay() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\?".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(a.mode, Mode::Help);
    // The Editor section is the active anchor since we came
    // from Editor.
    assert_eq!(a.help.origin, Some(Mode::Editor));
}

#[test]
fn backslash_quit_sets_should_quit() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\q".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(a.should_quit);
}

#[test]
fn backslash_timing_toggles_state() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\timing".into();
    a.editor.cursor = a.editor.buffer.len();
    assert!(!a.timing_on);
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(a.timing_on);
    // Buffer preserved (operator commonly toggles back).
    assert_eq!(a.editor.buffer, "\\timing");
    // Toggle again → off.
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(!a.timing_on);
}

#[test]
fn backslash_unknown_surfaces_actionable_error() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\xyz".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(err.contains("unknown backslash command"));
    // Stay in Editor — no useful destination to route to.
    assert_eq!(a.mode, Mode::Editor);
}

#[test]
fn backslash_report_writes_markdown_to_explicit_path() {
    let mut a = app_with_schemas();
    let tmp = std::env::temp_dir().join(format!("pgman-report-test-{}.md", std::process::id()));
    a.mode = Mode::Editor;
    a.editor.buffer = format!("\\report {}", tmp.display());
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    let contents = std::fs::read_to_string(&tmp).expect("report written");
    assert!(
        contents.starts_with("# pgman report"),
        "got: {contents:.120}"
    );
    assert!(contents.contains("## Schema lint findings"));
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("wrote report to"),
        "expected status flash; got: {status}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn backslash_report_writes_html_when_extension_matches() {
    let mut a = app_with_schemas();
    let tmp = std::env::temp_dir().join(format!("pgman-report-test-{}.html", std::process::id()));
    a.mode = Mode::Editor;
    a.editor.buffer = format!("\\report {}", tmp.display());
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    let contents = std::fs::read_to_string(&tmp).expect("html report written");
    assert!(contents.starts_with("<!doctype html>"));
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn backslash_l_renders_databases_as_result_grid() {
    let mut a = app_with_schemas();
    a.databases = vec![
        DatabaseInfo {
            name: "app_dev".into(),
            size: "42 MB".into(),
        },
        DatabaseInfo {
            name: "postgres".into(),
            size: "7580 kB".into(),
        },
    ];
    a.mode = Mode::Editor;
    a.editor.buffer = "\\l".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(
        a.grid.columns,
        vec!["database".to_string(), "size".to_string()]
    );
    assert_eq!(
        a.grid.rows,
        vec![
            vec!["app_dev".to_string(), "42 MB".to_string()],
            vec!["postgres".to_string(), "7580 kB".to_string()],
        ]
    );
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("2 database(s)"), "got: {status}");
}

#[test]
fn backslash_l_with_no_databases_surfaces_hint_not_panic() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\l".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(a.grid.is_empty());
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("connect first"), "got: {status}");
}

#[test]
fn backslash_x_toggles_expanded_state() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\x".into();
    a.editor.cursor = a.editor.buffer.len();
    assert!(!a.expanded_on);
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(a.expanded_on);
    assert_eq!(a.last_status.as_deref(), Some("expanded on"));
    // Buffer preserved — same rationale as `\timing`: operators
    // commonly toggle it back off in the same buffer.
    assert_eq!(a.editor.buffer, "\\x");
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(!a.expanded_on);
    assert_eq!(a.last_status.as_deref(), Some("expanded off"));
}

#[test]
fn expanded_on_lands_new_query_result_in_row_detail() {
    let mut a = app_with_schemas();
    a.expanded_on = true;
    a.on_msg(AppMsg::QueryOk {
        generation: a.generation,
        grid: crate::grid::Grid {
            columns: vec!["id".into()],
            rows: vec![vec!["1".into()]],
            truncated: false,
        },
        kind_label: "SELECT".into(),
        tx_open_after: false,
    });
    assert_eq!(a.mode, Mode::RowDetail);
}

#[test]
fn expanded_off_leaves_new_query_result_in_grid_mode() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    assert!(!a.expanded_on);
    a.on_msg(AppMsg::QueryOk {
        generation: a.generation,
        grid: crate::grid::Grid {
            columns: vec!["id".into()],
            rows: vec![vec!["1".into()]],
            truncated: false,
        },
        kind_label: "SELECT".into(),
        tx_open_after: false,
    });
    assert_eq!(a.mode, Mode::Editor);
}

#[test]
fn backslash_c_with_no_arg_opens_picker() {
    let pick = DataSourcePick {
        name: "primary".into(),
        origin: "test",
        dsn: Some(crate::conn::Dsn::parse("postgres://app@db/x").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(a.mode, Mode::ConnPick);
}

#[test]
fn demo_mode_never_opens_a_connection_via_backslash_c() {
    // `--demo` promises no database and no network. `\c <name>` used to
    // walk straight past that into a real TCP connect (which is also
    // why this test can run without a tokio runtime: reaching
    // `start_connect`'s spawn would panic).
    let mut a = crate::demo::launch_app(Theme::default());
    a.conn_pick.picks.push(DataSourcePick {
        name: "staging".into(),
        origin: "project",
        dsn: Some(crate::conn::Dsn::parse("postgres://app@db.example.com/x").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    });
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c staging".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(
        !matches!(a.conn_state, ConnState::Connecting),
        "--demo must not start a connection"
    );
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("--demo has no server"), "got: {status}");
}

#[tokio::test]
async fn reconnect_clears_the_previous_servers_database_list() {
    let mut a = App::new(
        Theme::default(),
        Some(crate::conn::Dsn::parse("postgres://app@old-host/app").unwrap()),
        Vec::new(),
        SafetyConfig::default(),
    );
    a.databases = vec![crate::app::DatabaseInfo {
        name: "from_old_server".into(),
        size: "1 GB".into(),
    }];
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c other_db".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(
        a.databases.is_empty(),
        "the old server's databases must not survive a reconnect: {:?}",
        a.databases
    );
}

#[test]
fn backslash_l_with_no_databases_leaves_the_start_card_alone() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    assert!(a.grid.columns.is_empty(), "start card state");
    a.mode = Mode::Editor;
    a.editor.buffer = "\\l".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(
        a.grid.columns.is_empty(),
        "a header-only grid would replace the start card permanently"
    );
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("no databases"), "got: {status}");
}

#[test]
fn backslash_l_with_databases_still_renders_the_grid() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.databases = vec![crate::app::DatabaseInfo {
        name: "app".into(),
        size: "1 GB".into(),
    }];
    a.mode = Mode::Editor;
    a.editor.buffer = "\\l".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(a.grid.columns, vec!["database".to_string(), "size".into()]);
    assert_eq!(a.grid.rows.len(), 1);
}

/// A discovered pick carrying an `ssh_tunnel`.
fn tunnel_pick() -> DataSourcePick {
    DataSourcePick {
        name: "via-bastion".into(),
        origin: "project",
        dsn: Some(
            crate::conn::Dsn::parse(
                "postgres://app@db.internal:5432/main?ssh_tunnel=tom@bastion.example.com",
            )
            .unwrap(),
        ),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    }
}

#[test]
fn conn_pick_enter_on_a_tunnel_pick_asks_before_spawning_ssh() {
    let mut a = App::new(
        Theme::default(),
        None,
        vec![tunnel_pick()],
        SafetyConfig::default(),
    );
    a.mode = Mode::ConnPick;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    let pending = a.pending_tunnel.as_ref().expect("tunnel confirmation");
    assert_eq!(
        pending.dsn.ssh_tunnel.as_ref().map(|t| t.host.as_str()),
        Some("bastion.example.com")
    );
    assert!(
        matches!(a.conn_state, ConnState::Disconnected),
        "no connect — and no ssh — before the confirmation"
    );
    assert!(a.dsn.is_none());
    assert_eq!(a.mode, Mode::ConnPick);
}

#[test]
fn tunnel_confirm_cancels_on_anything_but_y() {
    for cancel in [
        KeyEvent::from(KeyCode::Char('n')),
        KeyEvent::from(KeyCode::Esc),
        KeyEvent::from(KeyCode::Char('j')),
        KeyEvent::from(KeyCode::Enter),
    ] {
        let mut a = App::new(
            Theme::default(),
            None,
            vec![tunnel_pick()],
            SafetyConfig::default(),
        );
        a.mode = Mode::ConnPick;
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(a.pending_tunnel.is_some());
        a.on_key(cancel);
        assert!(
            a.pending_tunnel.is_none(),
            "{cancel:?} must clear the prompt"
        );
        assert!(
            matches!(a.conn_state, ConnState::Disconnected),
            "{cancel:?} must not connect"
        );
        assert!(a.dsn.is_none(), "{cancel:?} must not adopt the dsn");
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(
            status.contains("bastion.example.com"),
            "cancel should say what didn't happen: {status}"
        );
    }
}

#[tokio::test]
async fn tunnel_confirm_proceeds_on_y() {
    let mut a = App::new(
        Theme::default(),
        None,
        vec![tunnel_pick()],
        SafetyConfig::default(),
    );
    a.mode = Mode::ConnPick;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    a.on_key(KeyEvent::from(KeyCode::Char('y')));
    assert!(a.pending_tunnel.is_none());
    assert!(matches!(a.conn_state, ConnState::Connecting));
    assert_eq!(
        a.dsn
            .as_ref()
            .and_then(|d| d.ssh_tunnel.as_ref())
            .map(|t| t.host.as_str()),
        Some("bastion.example.com")
    );
}

#[test]
fn backslash_c_by_name_also_confirms_a_tunnel() {
    // Naming a discovered pick is not authorising an ssh session to the
    // bastion it happens to carry.
    let mut a = App::new(
        Theme::default(),
        None,
        vec![tunnel_pick()],
        SafetyConfig::default(),
    );
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c via-bastion".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(a.pending_tunnel.is_some(), "must ask first");
    assert!(matches!(a.conn_state, ConnState::Disconnected));
}

/// The other half of "a lone discovered pick doesn't auto-connect"
/// (`tests/journeys.rs`): it is still one keypress away, so the rule
/// costs the operator a keystroke and nothing else.
#[tokio::test]
async fn conn_pick_enter_on_a_lone_pick_connects() {
    let pick = DataSourcePick {
        name: "theirs".into(),
        origin: "project",
        dsn: Some(crate::conn::Dsn::parse("postgres://app@db.example.com/x").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    assert_eq!(a.mode, Mode::ConnPick, "lands in the picker, not connected");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(
        a.dsn.as_ref().map(|d| d.host.as_str()),
        Some("db.example.com")
    );
    assert!(matches!(a.conn_state, ConnState::Connecting));
    assert_eq!(a.mode, Mode::Normal);
}

#[tokio::test]
async fn backslash_c_with_matching_name_connects_to_that_pick() {
    // A discovered-shaped name: Spring picks are named after the
    // bean and the file they came from, so they contain spaces and
    // parentheses. Double quotes are how you address one exactly.
    let pick = DataSourcePick {
        name: "dataSource (application)".into(),
        origin: "Spring",
        dsn: Some(crate::conn::Dsn::parse("postgres://app@db/staging_db").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c \"dataSource (application)\"".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(
        a.dsn.as_ref().map(|d| d.dbname.as_str()),
        Some("staging_db"),
        "the quoted name must reach the pick, not a `dataSource` nobody has"
    );
    assert_eq!(a.mode, Mode::Normal);
    let origin = a.dsn_origin.as_deref().unwrap_or("");
    assert!(origin.contains("dataSource (application)"), "got: {origin}");
}

#[tokio::test]
async fn backslash_c_resolves_a_unique_prefix_of_a_discovered_name() {
    let picks = vec![
        DataSourcePick {
            name: "dataSource (application)".into(),
            origin: "Spring",
            dsn: Some(crate::conn::Dsn::parse("postgres://app@db/app_db").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
        DataSourcePick {
            name: "reports (application)".into(),
            origin: "Spring",
            dsn: Some(crate::conn::Dsn::parse("postgres://app@db/reports_db").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
    ];
    let mut a = App::new(Theme::default(), None, picks, SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c rep".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(
        a.dsn.as_ref().map(|d| d.dbname.as_str()),
        Some("reports_db")
    );
    assert!(a.last_error.is_none(), "{:?}", a.last_error);
}

#[test]
fn backslash_c_with_an_ambiguous_prefix_lists_the_candidates_and_connects_to_nothing() {
    // Choosing one would be choosing which database the operator
    // connects to. (No tokio runtime here on purpose: reaching
    // `start_connect`'s spawn would panic, which is the assertion.)
    let picks = vec![
        DataSourcePick {
            name: "dataSource (application)".into(),
            origin: "Spring",
            dsn: Some(crate::conn::Dsn::parse("postgres://app@db/app_db").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
        DataSourcePick {
            name: "dataSource (application-test)".into(),
            origin: "Spring",
            dsn: Some(crate::conn::Dsn::parse("postgres://app@db/test_db").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
    ];
    let mut a = App::new(Theme::default(), None, picks, SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c dataSource".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(
        matches!(a.conn_state, ConnState::Disconnected),
        "an ambiguous name must not connect to either"
    );
    let err = a.last_error.as_deref().expect("an ambiguity error");
    assert!(err.contains("ambiguous"), "got: {err}");
    assert!(err.contains("\"dataSource (application)\""), "got: {err}");
    assert!(
        err.contains("\"dataSource (application-test)\""),
        "got: {err}"
    );
    assert!(err.contains("quote the full name"), "got: {err}");
}

#[test]
fn connect_command_addresses_a_quoted_pick_the_same_way_backslash_c_does() {
    let pick = DataSourcePick {
        name: "dataSource (application)".into(),
        origin: "Spring",
        dsn: Some(crate::conn::Dsn::parse("postgres://${DB_USER}@db/app_db").unwrap()),
        unresolved: vec!["DB_USER".to_string()],
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    // The unresolved placeholder is the refusal we can observe
    // without a runtime — what matters is that the quoted name
    // reached the pick at all.
    run_command(&mut a, "connect \"dataSource (application)\"");
    assert_eq!(
        a.last_error.as_deref(),
        Some(
            "unresolved placeholder ${DB_USER} — export it, or put the connection in .pgman/pgman.toml"
        )
    );
}

#[tokio::test]
async fn backslash_c_with_unmatched_name_swaps_dbname_on_current_dsn() {
    let mut a = App::new(
        Theme::default(),
        Some(crate::conn::Dsn::parse("postgres://app@db/app_dev").unwrap()),
        Vec::new(),
        SafetyConfig::default(),
    );
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c reporting".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(a.dsn.as_ref().map(|d| d.dbname.as_str()), Some("reporting"));
}

#[test]
fn backslash_c_with_unmatched_name_and_no_active_connection_errors() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c reporting".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(err.contains("no active connection"), "got: {err}");
}

#[test]
fn backslash_i_loads_file_into_editor_buffer() {
    let mut a = app_with_schemas();
    let tmp = std::env::temp_dir().join(format!("pgman-include-test-{}.sql", std::process::id()));
    std::fs::write(&tmp, "select 1;\nselect 2;\n").unwrap();
    a.mode = Mode::Editor;
    a.editor.buffer = format!("\\i {}", tmp.display());
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert_eq!(a.editor.buffer, "select 1;\nselect 2;\n");
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.contains("loaded 2 lines from"), "got: {status}");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn backslash_i_missing_file_is_actionable_error() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\i /no/such/path-pgman-test.sql".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(err.contains("/no/such/path-pgman-test.sql"), "got: {err}");
}

#[test]
fn backslash_i_with_no_path_is_actionable_error() {
    let mut a = app_with_schemas();
    a.mode = Mode::Editor;
    a.editor.buffer = "\\i".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(err.contains("requires a file path"), "got: {err}");
}

#[test]
fn format_unix_secs_utc_pins_the_epoch_anchor() {
    // 1970-01-01T00:00:00Z
    assert_eq!(format_unix_secs_utc(0), "1970-01-01T00:00:00Z");
    // 2000-01-01T00:00:00Z = 946684800
    assert_eq!(format_unix_secs_utc(946_684_800), "2000-01-01T00:00:00Z");
    // 2023-11-14T22:13:20Z = 1700000000 (a common
    // fixture timestamp).
    assert_eq!(format_unix_secs_utc(1_700_000_000), "2023-11-14T22:13:20Z");
}

#[test]
fn format_unix_secs_utc_handles_leap_year() {
    // 2024-02-29T00:00:00Z = 1709164800
    assert_eq!(format_unix_secs_utc(1_709_164_800), "2024-02-29T00:00:00Z");
}

#[test]
fn live_lint_loaded_merges_into_findings_and_resorts() {
    // Open the lint panel with a pre-populated (Medium)
    // finding, then deliver a successful LiveLintLoaded with
    // a High LINT101. The merged list must sort with the new
    // High entry first.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::SchemaLint;
    a.schema_lint.findings = vec![crate::query::lint::Finding {
        severity: crate::query::lint::Severity::Medium,
        code: "LINT002",
        title: "mixed-case".into(),
        object: "public.Foo".into(),
        detail: "…".into(),
        suggestion: None,
    }];
    a.on_msg(AppMsg::LiveLintLoaded {
        generation: a.generation,
        result: Ok(vec![crate::query::lint::fk_without_index_finding(
            "public",
            "orders",
            "orders_user_id_fkey",
            "user_id",
        )]),
    });
    assert_eq!(a.schema_lint.findings.len(), 2);
    // High entry now first.
    assert_eq!(a.schema_lint.findings[0].code, "LINT101");
    // Status reflects the count + live delta.
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("live: +1"),
        "expected live-merge status; got: {status}"
    );
}

#[test]
fn live_lint_loaded_after_panel_closed_is_dropped_silently() {
    // The operator opens the panel, then immediately closes
    // it. The async live-fetch completes after the close —
    // we must not mutate findings or status in that case.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal; // not on the lint panel
    a.schema_lint.findings.clear();
    a.on_msg(AppMsg::LiveLintLoaded {
        generation: a.generation,
        result: Ok(vec![crate::query::lint::fk_without_index_finding(
            "public", "t", "fk", "user_id",
        )]),
    });
    // Findings untouched.
    assert!(a.schema_lint.findings.is_empty());
}

#[test]
fn live_lint_failure_surfaces_status_keeps_pure_findings() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::SchemaLint;
    let pure = crate::query::lint::Finding {
        severity: crate::query::lint::Severity::High,
        code: "LINT001",
        title: "missing PK".into(),
        object: "public.events".into(),
        detail: "…".into(),
        suggestion: None,
    };
    a.schema_lint.findings = vec![pure.clone()];
    a.on_msg(AppMsg::LiveLintLoaded {
        generation: a.generation,
        result: Err("LINT101: permission denied for pg_constraint".into()),
    });
    // Pure findings still there.
    assert_eq!(a.schema_lint.findings.len(), 1);
    assert_eq!(a.schema_lint.findings[0].code, pure.code);
    // Status surfaces the failure.
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("live check failed"),
        "expected failure status; got: {status}"
    );
}

#[test]
fn schema_lint_jk_navigation_clamps_to_findings() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let cache = crate::query::schema::SchemaCache {
        schemas: vec!["public".into()],
        tables: vec![
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "a".into(),
            },
            crate::query::schema::TableMeta {
                schema: "public".into(),
                name: "b".into(),
            },
        ],
        ..Default::default()
    };
    a.schema_cache = cache;
    a.start_schema_lint();
    let n = a.schema_lint.findings.len();
    assert!(n >= 2);
    for _ in 0..(n * 2) {
        a.on_key(KeyEvent::from(KeyCode::Char('j')));
    }
    assert_eq!(a.schema_lint.cursor, n - 1);
}

#[test]
fn schema_browser_close_with_accepted_filter_clears_for_next_open() {
    // Accept a filter via Enter, then close the browser. The
    // filter must NOT survive across opens — the next `S` should
    // show the full tree again.
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    a.on_key(KeyEvent::from(KeyCode::Char('a')));
    a.on_key(KeyEvent::from(KeyCode::Char('u')));
    a.on_key(KeyEvent::from(KeyCode::Enter)); // accept filter
    assert_eq!(a.schema_browser.filter.as_deref(), Some("au"));
    // Now close the browser via Esc from SchemaBrowser mode.
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Normal);
    assert!(
        a.schema_browser.filter.is_none(),
        "filter should be cleared on browser close"
    );
}

#[test]
fn schema_browser_collapse_clamps_cursor_inside_visible() {
    let mut a = app_with_schemas();
    a.mode = Mode::SchemaBrowser;
    // Expand public so we have 4 visible rows; focus the last one.
    a.schema_browser.expanded.insert("public".into());
    a.schema_browser.cursor = 3;
    // Collapse public (focused on "public" row at index 1 won't
    // collapse if we're focused on a Table — move focus first).
    a.schema_browser.cursor = 1;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    // After collapse, only the 2 schema rows remain. Cursor must
    // be inside [0, 1], not the stale 3.
    let rows = a.flattened_schema_browser();
    assert!(a.schema_browser.cursor < rows.len());
}

#[test]
fn schema_browser_table_row_carries_column_and_constraint_counts() {
    let mut a = app_with_schemas();
    a.schema_cache.constraints = vec![
        crate::query::schema::ConstraintMeta {
            schema: "public".into(),
            table: "users".into(),
            name: "users_pkey".into(),
        },
        crate::query::schema::ConstraintMeta {
            schema: "public".into(),
            table: "users".into(),
            name: "users_email_uk".into(),
        },
    ];
    a.schema_browser.expanded.insert("public".into());
    let rows = a.flattened_schema_browser();
    let users = rows
        .iter()
        .find(|r| matches!(r, SchemaBrowserRow::Table { name, .. } if name == "users"))
        .expect("users row");
    match users {
        SchemaBrowserRow::Table {
            column_count,
            constraint_count,
            ..
        } => {
            assert_eq!(*column_count, 2);
            assert_eq!(*constraint_count, 2);
        }
        _ => unreachable!(),
    }
}

#[test]
fn start_schema_browser_with_empty_cache_surfaces_hint() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.start_schema_browser();
    assert_eq!(a.mode, Mode::Normal);
    assert!(a
        .last_status
        .as_deref()
        .unwrap_or("")
        .contains("schema cache empty"));
}

fn explain_app_with_plan() -> App {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let json = r#"[{
          "Plan": {
            "Node Type": "Hash Join",
            "Total Cost": 200.0,
            "Actual Total Time": 50.0,
            "Plans": [
              { "Node Type": "Seq Scan", "Relation Name": "a",
                "Total Cost": 100.0, "Actual Total Time": 30.0 },
              { "Node Type": "Hash", "Total Cost": 22.5,
                "Actual Total Time": 5.0,
                "Plans": [
                  { "Node Type": "Seq Scan", "Relation Name": "b",
                    "Total Cost": 22.5, "Actual Total Time": 4.0 }
                ]
              }
            ]
          }
        }]"#;
    let plan = crate::query::explain::parse(json).unwrap();
    a.explain.plan = Some(plan);
    a.mode = Mode::ExplainTree;
    a
}

#[test]
fn flattened_explain_lists_each_node_once() {
    let a = explain_app_with_plan();
    let rows = a.flattened_explain_rows();
    assert_eq!(rows.len(), 4); // root + 3 descendants
    assert_eq!(rows[0].node_type, "Hash Join");
    assert_eq!(rows[1].node_type, "Seq Scan");
    assert_eq!(rows[2].node_type, "Hash");
    assert_eq!(rows[3].node_type, "Seq Scan");
    // Depths.
    assert_eq!(rows[0].depth, 0);
    assert_eq!(rows[1].depth, 1);
    assert_eq!(rows[2].depth, 1);
    assert_eq!(rows[3].depth, 2);
}

#[test]
fn explain_enter_collapses_focused_node_and_hides_children() {
    let mut a = explain_app_with_plan();
    // Focus row 2 (the "Hash" node, which has children).
    a.explain.cursor = 2;
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let rows = a.flattened_explain_rows();
    // Hash's child Seq Scan is hidden now.
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].node_type, "Hash");
    assert!(rows[2].collapsed);
    // Toggle back.
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let rows = a.flattened_explain_rows();
    assert_eq!(rows.len(), 4);
}

#[test]
fn explain_jk_moves_cursor_g_jumps_to_ends() {
    let mut a = explain_app_with_plan();
    // j down to last row.
    for _ in 0..10 {
        a.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    }
    assert_eq!(a.explain.cursor, 3); // clamped to last
    a.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    assert_eq!(a.explain.cursor, 0);
    a.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
    assert_eq!(a.explain.cursor, 3);
}

#[test]
fn explain_esc_returns_to_normal() {
    let mut a = explain_app_with_plan();
    a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn explain_enter_on_leaf_node_is_a_noop() {
    let mut a = explain_app_with_plan();
    a.explain.cursor = 1; // leaf Seq Scan on `a`
    let before = a.flattened_explain_rows().len();
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let after = a.flattened_explain_rows().len();
    assert_eq!(before, after);
    assert!(a.explain.collapsed.is_empty());
}

#[test]
fn start_connection_change_with_picks_opens_picker() {
    let pick = DataSourcePick {
        name: "primary".into(),
        origin: "test",
        dsn: Some(Dsn::parse("postgres://app@db/x").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
    a.mode = Mode::Normal;
    a.start_connection_change();
    assert_eq!(a.mode, Mode::ConnPick);
    assert_eq!(a.conn_pick.index, 0);
}

#[test]
fn start_connection_change_with_no_picks_surfaces_hint() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.start_connection_change();
    assert_eq!(a.mode, Mode::Normal);
    assert!(a
        .last_status
        .as_deref()
        .unwrap_or("")
        .contains("no data sources"));
}

// Draft persistence is exercised end-to-end via util::write_atomic
// (which has its own roundtrip test) + the trivial wrapper here.
// A test that touches the real `draft_path` races against parallel
// tests since they all share the same HOME-derived location;
// skipping in favour of the util-level coverage.

/// Recording fake cancel-dispatcher. The actual `dispatch`
/// closure is fire-and-forget in production; in tests we just
/// count calls.
#[derive(Debug, Default)]
struct RecordingDispatcher {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl CancelDispatcher for RecordingDispatcher {
    fn dispatch(&self) {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[test]
fn cancel_running_query_dispatches_through_injected_handler() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    a.cancel_dispatcher = Some(Box::new(RecordingDispatcher {
        calls: calls.clone(),
    }));
    a.query_running = true;
    a.mode = Mode::Editor;

    // Ctrl-C with a running query routes through the dispatcher.
    a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(a.last_status.as_deref(), Some("cancelling query…"));
}

#[test]
fn cancel_running_query_no_dispatcher_no_op() {
    // Without a dispatcher (e.g. not connected) Ctrl-C is a
    // silent no-op rather than a panic.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.cancel_dispatcher = None;
    a.query_running = true;
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    // No panic, status not flipped (function returned at the
    // `None` guard before setting it).
    assert!(a.last_status.is_none());
}

#[test]
fn cancel_running_query_idle_skips_dispatcher() {
    // Ctrl-C only fires the cancel when `query_running` — gated
    // at the keybinding level. With no running query, the
    // dispatcher should not be called.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    a.cancel_dispatcher = Some(Box::new(RecordingDispatcher {
        calls: calls.clone(),
    }));
    a.query_running = false;
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[test]
fn pg_notice_lands_in_status_and_history() {
    use crate::conn::NoticeMsg;
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let n = NoticeMsg {
        severity: "NOTICE".into(),
        message: "function returned: 42".into(),
        detail: None,
        hint: None,
    };
    // Tag with the App's current generation so on_msg accepts it.
    let _ = a.msg_tx.send(AppMsg::Notice {
        generation: a.generation,
        notice: n,
    });
    if let Some(rx) = a.msg_rx.as_mut() {
        if let Ok(msg) = rx.try_recv() {
            a.on_msg(msg);
        }
    }
    assert_eq!(a.notices.len(), 1);
    assert!(a
        .last_status
        .as_deref()
        .unwrap_or("")
        .contains("function returned: 42"));
}

#[test]
fn notice_buffer_caps_at_50() {
    use crate::conn::NoticeMsg;
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    for i in 0..60 {
        a.on_msg(AppMsg::Notice {
            generation: a.generation,
            notice: NoticeMsg {
                severity: "NOTICE".into(),
                message: format!("msg #{i}"),
                detail: None,
                hint: None,
            },
        });
    }
    assert_eq!(a.notices.len(), 50);
    // Oldest dropped — first kept is msg #10.
    assert_eq!(a.notices.first().unwrap().message, "msg #10");
    assert_eq!(a.notices.last().unwrap().message, "msg #59");
}

#[test]
fn ctrl_r_on_empty_history_no_ops_with_status() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    // No history.
    a.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_eq!(a.mode, Mode::Editor);
    assert!(a.history_search.is_none());
    assert_eq!(a.last_status.as_deref(), Some("history is empty"));
}

#[test]
fn query_failed_with_position_past_buffer_clamps_to_end() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.editor.buffer = "SELECT 1".into();
    a.editor.cursor = 0;
    a.generation = 1;
    let _ = a.msg_tx.send(AppMsg::QueryFailed {
        generation: 1,
        error: "boom".into(),
        position: Some(999),
        detail: None,
    });
    if let Some(rx) = a.msg_rx.as_mut() {
        if let Ok(msg) = rx.try_recv() {
            a.on_msg(msg);
        }
    }
    assert_eq!(a.editor.cursor, a.editor.buffer.len());
}

// -- bootstrap → start card (not the grid) --------------------------

fn bootstrap_grid(rows: &[(&str, &str)]) -> Grid {
    Grid {
        columns: vec!["database".into(), "size".into()],
        rows: rows
            .iter()
            .map(|(name, size)| vec![name.to_string(), size.to_string()])
            .collect(),
        truncated: false,
    }
}

#[test]
fn parse_bootstrap_databases_extracts_name_and_size_in_row_order() {
    let grid = bootstrap_grid(&[("main", "1.2 GB"), ("analytics", "300 MB")]);
    let got = parse_bootstrap_databases(&grid);
    assert_eq!(
        got,
        vec![
            DatabaseInfo {
                name: "main".into(),
                size: "1.2 GB".into(),
            },
            DatabaseInfo {
                name: "analytics".into(),
                size: "300 MB".into(),
            },
        ]
    );
}

#[test]
fn parse_bootstrap_databases_skips_rows_with_fewer_than_two_columns() {
    let grid = Grid {
        columns: vec!["database".into(), "size".into()],
        rows: vec![vec!["only_name".into()], vec!["main".into(), "1 GB".into()]],
        truncated: false,
    };
    let got = parse_bootstrap_databases(&grid);
    assert_eq!(
        got,
        vec![DatabaseInfo {
            name: "main".into(),
            size: "1 GB".into(),
        }]
    );
}

#[test]
fn parse_bootstrap_databases_empty_grid_yields_empty_vec() {
    let grid = Grid::default();
    assert!(parse_bootstrap_databases(&grid).is_empty());
}

/// `apply_bootstrap_grid` is what `on_msg`'s `Booted` arm calls with the
/// bootstrap query's result. It must populate `App.databases` — never
/// `App.grid` — so the start card (which only shows while
/// `grid.columns` is empty) is what the operator lands on after every
/// real connect, not a two-column grid of database names and sizes.
///
/// VERIFY-THE-CLAIM: temporarily changing `apply_bootstrap_grid` back
/// to `self.grid = grid;` (the old, pre-fix path) makes this test fail
/// both assertions — confirmed by hand, then reverted. See the task
/// report for the CAUGHT line.
#[test]
fn bootstrap_result_populates_databases_and_leaves_grid_empty() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let grid = bootstrap_grid(&[("main", "1.2 GB"), ("analytics", "300 MB")]);
    a.apply_bootstrap_grid(grid);
    assert_eq!(
        a.databases,
        vec![
            DatabaseInfo {
                name: "main".into(),
                size: "1.2 GB".into(),
            },
            DatabaseInfo {
                name: "analytics".into(),
                size: "300 MB".into(),
            },
        ]
    );
    assert!(
        a.grid.columns.is_empty(),
        "bootstrap result must not land in the grid: {:?}",
        a.grid
    );
    assert!(a.grid.rows.is_empty());
}
// --- Pasted-log detection: on_paste status hint ---

#[test]
fn paste_of_hibernate_log_sets_reconstruct_hint_status() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_paste("[main] org.hibernate.SQL : select 1".to_string());
    assert_eq!(
        a.last_status.as_deref(),
        Some("looks like a hibernate log · ctrl-l / F8 to reconstruct queries")
    );
}

#[test]
fn paste_of_plain_sql_does_not_set_reconstruct_hint() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_paste("select * from users where id = $1".to_string());
    assert_ne!(
        a.last_status.as_deref(),
        Some("looks like a pglog log · ctrl-l / F8 to reconstruct queries")
    );
}

// --- App::preload_log (backs `--log PATH`, src/main.rs) ---

#[test]
fn preload_log_with_hibernate_sample_enters_log_pick() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let log = "\
[main] org.hibernate.SQL : select c.id from customer c where c.id=?
[main] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]
[main] org.hibernate.SQL : select * from orders where customer_id=?
[main] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [7]";
    a.preload_log(log.to_string());
    assert_eq!(a.mode, Mode::LogPick);
    assert_eq!(a.log_pick.picks.len(), 2);
    assert!(a.editor.buffer.contains("org.hibernate.SQL"));
    assert_eq!(a.editor.cursor, a.editor.buffer.len());
}

#[test]
fn preload_log_with_jdbc_paste_shape_enters_log_pick_with_substituted_sql() {
    // Neither log parser matches this — it's the third way in: a
    // `?`-statement, a blank line, then typed params.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    let pasted = "select * from orders where id = ? and status = ?\n\nINTEGER:42\nVARCHAR:shipped";
    a.preload_log(pasted.to_string());
    assert_eq!(a.mode, Mode::LogPick);
    assert_eq!(a.log_pick.picks.len(), 1);
    assert_eq!(
        a.log_pick.picks[0].runnable_sql,
        "select * from orders where id = 42 and status = 'shipped'"
    );
}

#[test]
fn preload_log_with_prose_lands_in_editor_with_no_queries_error() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.preload_log("just some notes, not a log or SQL at all".to_string());
    assert_eq!(a.mode, Mode::Editor);
    assert!(a.log_pick.picks.is_empty());
    assert_eq!(
        a.last_error.as_deref(),
        Some("no queries found (paste a Hibernate or Postgres log into the editor first)")
    );
    assert!(a.editor.buffer.contains("just some notes"));
}

#[test]
fn preload_log_overrides_conn_pick_startup_mode() {
    // Multiple data sources would normally land on Mode::ConnPick — an
    // explicit --log wins over that startup picker.
    let picks = vec![
        DataSourcePick {
            name: "a".into(),
            origin: "project",
            dsn: Some(Dsn::parse("postgres://localhost/a").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
        DataSourcePick {
            name: "b".into(),
            origin: "project",
            dsn: Some(Dsn::parse("postgres://localhost/b").unwrap()),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        },
    ];
    let mut a = App::new(Theme::default(), None, picks, SafetyConfig::default());
    assert_eq!(a.mode, Mode::ConnPick);
    a.preload_log("[main] org.hibernate.SQL : select 1".to_string());
    // A pick was found → LogPick; either way the ConnPick startup picker
    // has been overridden by the explicit --log.
    assert_eq!(a.mode, Mode::LogPick);
}

// -- error surfaces say what to do next ---------------------------

#[test]
fn blocked_by_safety_message_names_the_statement_without_debug_braces() {
    let msg = blocked_by_safety_message(
        &crate::safety::Decision {
            kind: crate::safety::StatementKind::Delete { has_where: false },
            guard: crate::safety::Guard::Block,
            wrap_in_tx: false,
            blocked_by_read_only: false,
            read_only_escape: false,
        },
        "main",
    );
    assert!(
        msg.starts_with("blocked by safety: DELETE without WHERE on 'main'"),
        "{msg}"
    );
    assert!(
        !msg.contains('{') && !msg.contains('}'),
        "leaked Debug syntax: {msg}"
    );
    assert!(
        msg.contains("safety.toml"),
        "must say where the guard lives: {msg}"
    );
}

#[test]
fn not_connected_message_offers_the_next_step_for_each_state() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    assert!(a.not_connected_message().contains("--dsn"));
    a.conn_pick.picks.push(DataSourcePick {
        name: "prod".into(),
        origin: "project",
        dsn: Some(crate::conn::Dsn::parse("postgres://app@prod-db:5432/main").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    });
    assert!(a.not_connected_message().contains("c to choose"));
    a.conn_state = ConnState::Failed("boom".into());
    assert!(a.not_connected_message().contains("r to retry"));
}

/// Point an `App`'s on-exit persistence at a scratch dir so a run-loop
/// test never writes the operator's real ~/.local/share/pgman.
fn scratch_persistence(app: &mut App) {
    let dir = std::env::temp_dir().join(format!(
        "pgman-apptest-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    app.draft_file = dir.join("draft.sql");
    app.history_file = dir.join("history.log");
    app.saved_queries_file = dir.join("saved.toml");
}

/// A TuiHost that records "draw" into a shared log, so a test can
/// assert ordering against another recorder that shares the same
/// log (the injected update-check spawn hook, below) — proving
/// `run_with` fires the update check strictly AFTER the first draw,
/// not before it.
struct RecordingTui {
    log: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
}

impl crate::tui::TuiHost for RecordingTui {
    fn draw(&mut self, _app: &mut App) -> std::io::Result<()> {
        self.log.lock().unwrap().push("draw");
        Ok(())
    }
    fn suspend(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn resume(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn update_check_spawns_after_the_first_draw_never_before() {
    use std::sync::{Arc, Mutex};

    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    scratch_persistence(&mut app);
    app.mode = Mode::Normal;
    app.update_check_enabled = true;
    let spawn_log = log.clone();
    // Synchronous recorder in place of the real network spawn — the
    // point under test is WHEN this hook fires relative to the
    // first draw, not what it sends back.
    app.update_check_spawn = Some(Box::new(move |_tx| {
        spawn_log.lock().unwrap().push("spawn");
    }));

    let mut tui = RecordingTui { log: log.clone() };
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    // Quit immediately after the first iteration so the loop runs
    // exactly one draw.
    tx.send(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )))
    .unwrap();
    drop(tx);

    tokio::time::timeout(Duration::from_secs(2), app.run_with(&mut tui, rx))
        .await
        .expect("loop should terminate quickly")
        .unwrap();

    let recorded = log.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        ["draw", "spawn"],
        "update check must spawn strictly after the first draw, got {:?}",
        *recorded
    );
}

#[tokio::test]
async fn update_check_disabled_never_spawns() {
    use std::sync::{Arc, Mutex};

    let spawned = Arc::new(Mutex::new(false));
    let mut app = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    scratch_persistence(&mut app);
    app.mode = Mode::Normal;
    app.update_check_enabled = false;
    let flag = spawned.clone();
    app.update_check_spawn = Some(Box::new(move |_tx| {
        *flag.lock().unwrap() = true;
    }));

    let mut tui = crate::tui::HeadlessTui::default();
    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    tx.send(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )))
    .unwrap();
    drop(tx);

    tokio::time::timeout(Duration::from_secs(2), app.run_with(&mut tui, rx))
        .await
        .expect("loop should terminate quickly")
        .unwrap();

    assert!(
        !*spawned.lock().unwrap(),
        "update_check_enabled = false must never spawn the check"
    );
}

// --- --demo mode: request_run answers synthetically (no client) ---
//
// `crate::demo::app()` builds a fully-populated demo App with `demo =
// true` and `client = None` — exactly the state `pgman --demo` runs
// the TUI loop with. `on_key` ctrl-Enter drives `request_run` exactly
// like a live keypress would; the resulting `AppMsg::QueryOk` is
// pumped through the SAME `on_msg` handler a real connection uses.

/// Pump the single queued `AppMsg` from `a`'s channel into `on_msg`.
fn pump_one_demo_msg(a: &mut App) {
    let msg = a
        .msg_rx
        .as_mut()
        .expect("msg_rx present")
        .try_recv()
        .expect("expected a queued AppMsg after a demo run");
    a.on_msg(msg);
}

fn run_in_demo(a: &mut App, sql: &str) {
    a.mode = Mode::Editor;
    a.editor.buffer = sql.to_string();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
}

#[test]
fn demo_select_on_users_yields_the_users_grid() {
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "SELECT id, email, plan, created_at FROM users");
    pump_one_demo_msg(&mut a);
    assert_eq!(a.grid.columns, vec!["id", "email", "plan", "created_at"]);
    assert!(!a.grid.rows.is_empty());
    assert_eq!(a.last_error, None);
    assert!(
        a.history.iter().any(|h| h.contains("FROM users")),
        "a demo run should still land in history like a live one: {:?}",
        a.history
    );
}

#[test]
fn demo_select_on_orders_yields_rows_with_orders_columns() {
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "SELECT * FROM orders");
    pump_one_demo_msg(&mut a);
    assert_eq!(
        a.grid.columns,
        vec!["id", "user_id", "status", "total_cents", "created_at"]
    );
    assert!(
        a.grid.rows.len() >= 3,
        "expected several generated rows, got {}",
        a.grid.rows.len()
    );
}

#[test]
fn demo_delete_without_where_is_refused_by_the_guard() {
    // The whole point of routing --demo through safety::evaluate: an
    // unqualified DELETE gets blocked exactly like it would live —
    // it never reaches spawn_run_demo, so no AppMsg is ever queued.
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "DELETE FROM users");
    assert!(
        a.msg_rx.as_mut().unwrap().try_recv().is_err(),
        "a blocked statement must not queue a QueryOk"
    );
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("DELETE without WHERE"),
        "expected the safety-block message, got {err:?}"
    );
}

#[test]
fn demo_unknown_statement_yields_one_row_notice() {
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "SELECT 1");
    pump_one_demo_msg(&mut a);
    assert_eq!(a.grid.columns, vec!["demo".to_string()]);
    assert_eq!(a.grid.rows.len(), 1);
    assert_eq!(a.grid.rows[0][0], "this is --demo mode, no database");
}

#[test]
fn demo_confirmed_write_is_answered_synthetically() {
    // A guarded write in --demo reaches the confirm modal like a live
    // session; answering `y` must produce a demo result, not the
    // "not connected" error `spawn_run` gives without a client.
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "UPDATE users SET active = false WHERE id = 1");
    assert_eq!(a.mode, Mode::Confirm, "a guarded write must prompt first");
    a.on_key(KeyEvent::from(KeyCode::Char('y')));
    pump_one_demo_msg(&mut a);
    assert!(a.last_error.is_none(), "got error: {:?}", a.last_error);
    assert!(
        !a.grid.columns.is_empty(),
        "a confirmed demo write must yield a grid"
    );
}

#[test]
fn demo_explain_opens_the_plan_tree() {
    // `--demo` routed EXPLAIN through `demo::answer`, which projects
    // *result* columns — so the grid's first cell was a row value,
    // `explain::parse` failed on it, and the demo fell back to raw
    // text every time. EXPLAIN was the one feature the demo could
    // never show.
    let mut a = crate::demo::app(Theme::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT * FROM orders JOIN users ON users.id = orders.user_id".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(6)));
    pump_one_demo_msg(&mut a);

    assert_eq!(a.mode, Mode::ExplainTree, "status: {:?}", a.last_status);
    let plan = a.explain.plan.as_ref().expect("a parsed plan");
    assert_eq!(plan.node_type, "Hash Join");
    assert_eq!(plan.children.len(), 2);
    assert_eq!(
        plan.actual_total_time, None,
        "plain EXPLAIN has no ANALYZE timings"
    );
}

#[test]
fn demo_explain_analyze_carries_the_actual_timings() {
    let mut a = crate::demo::app(Theme::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "SELECT * FROM orders JOIN users ON users.id = orders.user_id".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(7)));
    pump_one_demo_msg(&mut a);

    assert_eq!(a.mode, Mode::ExplainTree, "status: {:?}", a.last_status);
    let plan = a.explain.plan.as_ref().expect("a parsed plan");
    assert!(
        plan.actual_total_time.is_some(),
        "EXPLAIN ANALYZE must show measured time, not just estimates"
    );
}

#[test]
fn opening_the_command_bar_over_the_confirm_modal_cancels_the_run() {
    // `:` is not one of the modal's keys, so the statement was never
    // confirmed — but `pending_run` used to survive the detour, and it
    // is a `\watch` blocker (`watch_should_fire`). F5 on a DELETE,
    // `:about`, Esc, and `\watch` then refused to fire for the rest of
    // the session with no modal on screen to explain why.
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "UPDATE users SET active = false WHERE id = 1");
    assert_eq!(a.mode, Mode::Confirm, "a guarded write must prompt first");
    assert!(a.pending_run.is_some());

    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    assert_eq!(a.mode, Mode::CommandBar);
    assert!(
        a.pending_run.is_none(),
        "the unconfirmed run must be cancelled, not left armed"
    );

    for c in "about".chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    a.on_key(KeyEvent::from(KeyCode::Enter));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(a.pending_run.is_none());
    assert!(
        crate::app::watch_should_fire(
            &crate::app::WatchState {
                sql: "SELECT 1".into(),
                interval: std::time::Duration::from_secs(0),
                last_fire: std::time::Instant::now(),
            },
            std::time::Instant::now(),
            crate::app::WatchTickInputs {
                query_running: false,
                tx_open: false,
                pending_run: a.pending_run.is_some(),
                mode_blocks: false,
            },
        ),
        "a cancelled run must stop blocking \\watch"
    );
}

#[test]
fn cancelling_a_run_by_opening_the_command_bar_says_so_after_the_bar_closes() {
    // Both close paths blank the status line (the bar owned the
    // footer), so the "cancelled" notice has to outlive the bar.
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "UPDATE users SET active = false WHERE id = 1");
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.last_status.as_deref(), Some("cancelled"));
    assert_eq!(
        a.mode,
        Mode::Editor,
        "and Esc lands in the editor, not on a modal with nothing behind it"
    );
}

// --- the multi-statement run path: split, verify, run what was checked ---

#[test]
fn demo_batch_hiding_a_drop_behind_a_dollar_identifier_is_refused() {
    // The security-review reproduction, driven through the real key path.
    // `a$b$c` is one identifier, so this is three statements and the third
    // is a DROP — blocked by default. Before the lexer fix the splitter saw
    // two SELECTs and ran the DROP along with them.
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "SELECT 1; SELECT 1 AS a$b$c; DROP TABLE users");
    assert!(
        a.msg_rx.as_mut().unwrap().try_recv().is_err(),
        "a blocked batch must not queue a run"
    );
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("batch blocked by safety")
            && err.contains("3 statements")
            && err.contains("Drop"),
        "expected the batch block to see three statements and name the Drop, \
         got {err:?}"
    );
}

#[test]
fn demo_batch_hiding_a_drop_behind_a_quoted_identifier_is_refused() {
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(
        &mut a,
        r#"SELECT 1; SELECT * FROM "a--b"; DROP TABLE users"#,
    );
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("batch blocked by safety")
            && err.contains("3 statements")
            && err.contains("Drop"),
        "expected the batch block to see three statements and name the Drop, \
         got {err:?}"
    );
}

#[test]
fn demo_batch_the_splitter_cannot_verify_is_refused() {
    // Fail closed. An unterminated literal means the statement boundaries
    // are a guess, and a guard computed from a guess is not a guard.
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "SELECT 1; SELECT 'oops");
    assert!(
        a.msg_rx.as_mut().unwrap().try_recv().is_err(),
        "an unverifiable script must not run"
    );
    assert_eq!(
        a.last_error.as_deref(),
        Some(crate::safety::SPLIT_REFUSAL),
        "the refusal must tell the operator what to do instead"
    );
    assert_ne!(a.mode, Mode::Confirm, "and it must not offer a confirm");
}

#[test]
fn demo_batch_runs_the_statements_it_checked_not_the_raw_buffer() {
    // What reaches the server is the re-join of the verified statements, so
    // there is no text in flight that the classifier never saw — and no text
    // the operator wrote that silently went missing either, which is why the
    // comments are still in it.
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(
        &mut a,
        "SELECT 1; /* note */ UPDATE users SET active = false WHERE id = 1 -- tail",
    );
    assert_eq!(a.mode, Mode::Confirm, "a guarded batch must prompt first");
    let pending = a.pending_run.as_ref().expect("a pending batch run");
    assert!(pending.is_batch);
    assert_eq!(
        pending.sql,
        "SELECT 1\n;\n/* note */ UPDATE users SET active = false WHERE id = 1 -- tail"
    );
}

// ---------------------------------------------------------------
// `:` command bar
// ---------------------------------------------------------------

/// Type `line` into the command bar and press Enter, from Normal mode.
fn run_command(a: &mut App, line: &str) {
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    assert_eq!(a.mode, Mode::CommandBar, "':' must open the bar");
    for c in line.chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    a.on_key(KeyEvent::from(KeyCode::Enter));
}

#[test]
fn colon_opens_the_command_bar_and_esc_returns_to_the_origin_mode() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::SchemaBrowser;
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    assert_eq!(a.mode, Mode::CommandBar);
    for c in "abo".chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    assert_eq!(a.command_bar.as_ref().map(|b| b.input.text()), Some("abo"));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(
        a.mode,
        Mode::SchemaBrowser,
        "esc returns where it came from"
    );
    assert!(a.command_bar.is_none(), "and the bar state is dropped");
}

#[test]
fn colon_types_a_literal_colon_while_the_editor_has_focus() {
    // Otherwise `:param` placeholders (and every other colon in SQL)
    // would be impossible to type.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor.buffer, ":");
    assert!(a.command_bar.is_none());
}

#[test]
fn command_bar_dispatches_backslash_commands_without_clearing_the_editor_draft() {
    // `\timing` from the editor clears the buffer (it IS the buffer);
    // `:timing` never touches it — the operator's draft is not the
    // command they just typed.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.editor.buffer = "SELECT 1".into();
    run_command(&mut a, "timing on");
    assert!(a.timing_on);
    assert_eq!(a.editor.buffer, "SELECT 1", "the draft must survive");
    run_command(&mut a, "x on");
    assert!(a.expanded_on);
    assert_eq!(a.editor.buffer, "SELECT 1");
}

#[test]
fn command_about_update_and_quit() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    run_command(&mut a, "about");
    assert_eq!(a.mode, Mode::About);
    run_command(&mut a, "update");
    assert_eq!(a.mode, Mode::About);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains("update check is off for this run"),
        "App::new never enables the check — say so rather than claiming up-to-date; got {status:?}"
    );
    run_command(&mut a, "q");
    assert!(a.should_quit);
}

#[test]
fn command_help_opens_the_overlay_at_the_topic_and_names_the_topics_it_knows() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    run_command(&mut a, "help editor");
    assert_eq!(a.mode, Mode::Help);
    assert_eq!(a.help.origin, Some(Mode::Editor));
    a.mode = Mode::Normal;
    run_command(&mut a, "help nonsense");
    assert_eq!(a.mode, Mode::Normal, "an unknown topic opens nothing");
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(err.contains("unknown help topic 'nonsense'"), "got {err:?}");
    assert!(err.contains("editor"), "and it lists the topics: {err:?}");
}

#[test]
fn every_help_topic_closes_back_to_where_the_bar_was_opened_from() {
    // The topic names the SECTION to scroll to, not the mode the
    // operator was in. Treating it as both meant `:help tap` closed
    // into the tap monitor and `:help commands` into
    // `Mode::CommandBar` — a mode with no command bar behind it, so
    // every key went to a prompt that wasn't on screen.
    for (topic, anchor) in crate::app::cmd::help_topics() {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        run_command(&mut a, &format!("help {topic}"));
        assert_eq!(a.mode, Mode::Help, "`:help {topic}` must open help");
        assert_eq!(
            a.help.origin,
            Some(*anchor),
            "`:help {topic}` must anchor on its own section"
        );
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(
            a.mode,
            Mode::Normal,
            "`:help {topic}` was opened from Normal, so Esc must return there"
        );
    }
}

#[test]
fn help_from_the_bar_opened_over_help_returns_to_where_help_was_opened_from() {
    // Editor → F1 → `:help` → Esc used to land in Help again: the bar
    // captured `Help` as its origin, so `:help` re-opened help with
    // `return_to = Help`, and every Esc closed help into help.
    for line in ["help", "help editor", "help tap"] {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = Mode::Editor;
        a.on_key(KeyEvent::from(KeyCode::F(1)));
        assert_eq!(a.mode, Mode::Help);
        a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
        assert_eq!(a.mode, Mode::CommandBar, "':' must open the bar over help");
        type_str(&mut a, line);
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Help, "`:{line}` must open help");
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(
            a.mode,
            Mode::Editor,
            "`:{line}` from help opened from the editor must close back to the editor"
        );
    }
    // Esc out of the bar itself also goes back to the editor, not to
    // a help overlay the bar already dismissed.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::from(KeyCode::F(1)));
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Editor);
}

#[test]
fn command_names_are_case_insensitive_all_the_way_through() {
    // `:ABOUT` matched on the lowercased name and worked; everything
    // delegated to the backslash parser was handed the RAW line, so
    // `:DT` became `\DT` and came back "unknown command".
    let mut upper = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    run_command(&mut upper, "DT");
    assert!(
        upper.last_error.is_none(),
        ":DT must be the same command as :dt, got {:?}",
        upper.last_error
    );
    let mut lower = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    run_command(&mut lower, "dt");
    assert_eq!(upper.mode, lower.mode);
    assert_eq!(upper.last_status, lower.last_status);

    // The ARGUMENT keeps its case — paths and data-source names are
    // case-sensitive even when the command name isn't.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    run_command(&mut a, "I /Tmp/Mixed-Case.sql");
    let err = a.last_error.as_deref().unwrap_or("");
    assert!(
        err.contains("/Tmp/Mixed-Case.sql"),
        "the path must survive verbatim: {err:?}"
    );
}

#[test]
fn unknown_command_names_itself_and_points_at_help() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    run_command(&mut a, "foo");
    assert_eq!(
        a.last_error.as_deref(),
        Some("unknown command :foo · :help lists them")
    );
}

#[test]
fn the_editor_blocks_every_lift_of_the_read_only_floor_without_a_prompt() {
    // `SELECT set_config('default_transaction_read_only', 'off', false)`
    // classified as a read and ran with no prompt at all; a quoted GUC
    // name was invisible to the escape check; `DO` hid its `SET` in a
    // dollar-quoted body. The demo app carries the default, read-only,
    // profile and goes through the same `request_run` a live one does.
    let mut a = crate::demo::app(Theme::default());
    for sql in [
        "SELECT set_config('default_transaction_read_only', 'off', false)",
        "SET \"transaction_read_only\" = off",
        "DO $$ BEGIN SET default_transaction_read_only = off; END $$",
        "CALL lift_the_floor()",
        "SELECT set_config('default_transaction_read_only', 'off', false); SELECT 1",
    ] {
        a.last_error = None;
        run_in_demo(&mut a, sql);
        // Which of the two refusal texts appears depends on whether the
        // machine running the test has a `safety.toml`; either is the
        // read-only-escape refusal (see `read_only_escape_refusal`).
        let err = a.last_error.clone().unwrap_or_default();
        assert!(
            err == read_only_escape_refusal(true) || err == read_only_escape_refusal(false),
            "{sql}: {err}"
        );
        assert!(a.pending_run.is_none(), "{sql}: a block never prompts");
        assert_ne!(a.mode, Mode::Confirm, "{sql}");
    }
}

#[test]
fn command_readonly_off_is_refused_when_the_profile_pins_read_only() {
    // The default safety profile is read-only. The bar is inside the
    // session, and a session cannot vote itself out of safety.toml —
    // same refusal a `SET default_transaction_read_only = off`
    // statement gets.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    assert!(
        a.read_only,
        "fixture check: the default profile is read-only"
    );
    run_command(&mut a, "readonly off");
    assert!(a.read_only, "the flag must not move");
    let err = a.last_error.clone().unwrap_or_default();
    assert!(
        err == read_only_escape_refusal(true) || err == read_only_escape_refusal(false),
        "{err}"
    );
}

#[test]
fn command_readonly_moves_the_flag_when_the_profile_does_not_pin_it() {
    let mut cfg = SafetyConfig::default();
    cfg.default.read_only = false;
    let mut a = App::new(Theme::default(), None, Vec::new(), cfg);
    assert!(!a.read_only);
    run_command(&mut a, "readonly on");
    assert!(a.read_only);
    assert!(a.last_error.is_none(), "{:?}", a.last_error);
    run_command(&mut a, "readonly off");
    assert!(!a.read_only, "an unpinned profile can be turned back off");
    run_command(&mut a, "readonly");
    assert_eq!(a.last_error.as_deref(), Some("usage: :readonly on|off"));
}

#[test]
fn connect_in_demo_changes_nothing_before_it_refuses() {
    // `start_connect` refuses `--demo` — but `connect_to_pick` had
    // already swapped the dsn, its origin, the safety profile and the
    // mode by the time it got there, so the demo session was left
    // pointing at a connection it never opened. (No tokio runtime
    // here on purpose: reaching `start_connect`'s spawn would panic,
    // which is half the assertion.)
    let mut a = crate::demo::app(Theme::default());
    a.conn_pick.picks = vec![DataSourcePick {
        name: "dataSource (application)".into(),
        origin: "Spring",
        dsn: Some(crate::conn::Dsn::parse("postgres://app@db/app_db").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    }];
    let before_dsn = a.dsn.clone();
    let before_origin = a.dsn_origin.clone();
    let before_read_only = a.read_only;

    // Driven from the editor so the mode assertion means something —
    // `connect_to_pick` forced `Mode::Normal` before refusing.
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c dataSource".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));

    assert_eq!(a.dsn, before_dsn, "the demo's dsn must not be replaced");
    assert_eq!(a.dsn_origin, before_origin);
    assert_eq!(a.read_only, before_read_only);
    assert_eq!(
        a.mode,
        Mode::Editor,
        "and the operator stays where they were"
    );
    assert_eq!(
        a.last_status.as_deref(),
        Some("--demo has no server — restart without --demo to connect")
    );
}

#[tokio::test]
async fn command_readonly_on_survives_the_next_connect() {
    // `connect_to_pick` recomputes `read_only` from the picked
    // database's safety profile. Without a sticky override that threw
    // a session `:readonly on` away silently, and the next statement
    // ran writable against a database the operator had just protected.
    let mut cfg = SafetyConfig::default();
    cfg.default.read_only = false;
    let picks = vec![DataSourcePick {
        name: "dataSource (application)".into(),
        origin: "Spring",
        dsn: Some(crate::conn::Dsn::parse("postgres://app@db/app_db").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    }];
    let mut a = App::new(Theme::default(), None, picks, cfg);
    assert!(!a.read_only, "fixture check: the profile does not pin it");

    run_command(&mut a, "readonly on");
    assert!(a.read_only);

    run_command(&mut a, "connect dataSource");
    assert_eq!(
        a.dsn.as_ref().map(|d| d.dbname.as_str()),
        Some("app_db"),
        "fixture check: the connect actually happened: {:?}",
        a.last_error
    );
    assert!(
        a.read_only,
        "`:readonly on` must survive the reconnect it was set before"
    );
}

#[tokio::test]
async fn command_readonly_off_does_not_survive_into_a_pinned_database() {
    // The override can only tighten. `:readonly off` against an
    // unpinned database must not carry into one whose profile pins
    // read-only.
    let mut cfg = SafetyConfig::default();
    cfg.default.read_only = false;
    cfg.databases.insert("locked_db".into(), {
        let mut p = cfg.default.clone();
        p.read_only = true;
        p
    });
    let picks = vec![DataSourcePick {
        name: "locked".into(),
        origin: "Spring",
        dsn: Some(crate::conn::Dsn::parse("postgres://app@db/locked_db").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    }];
    let mut a = App::new(Theme::default(), None, picks, cfg);
    run_command(&mut a, "readonly off");
    assert!(!a.read_only);

    run_command(&mut a, "connect locked");
    assert!(
        a.read_only,
        "the target database's profile is the floor the override can't dig under"
    );
}

#[test]
fn command_bar_tab_completes_a_unique_name_and_lists_ambiguous_ones() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    for c in "rea".chars() {
        a.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    a.on_key(KeyEvent::from(KeyCode::Tab));
    assert_eq!(
        a.command_bar.as_ref().map(|b| b.input.text()),
        Some("readonly "),
        "a lone candidate completes whole, ready for its argument"
    );
    // `d`, `dn`, `dt` share the `d` prefix — nothing to insert, but
    // the candidates are surfaced.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    a.on_key(KeyEvent::from(KeyCode::Char('d')));
    a.on_key(KeyEvent::from(KeyCode::Tab));
    assert_eq!(a.command_bar.as_ref().map(|b| b.input.text()), Some("d"));
    assert_eq!(a.command_bar_completions.as_deref(), Some("d · dn · dt"));
}

#[test]
fn command_candidates_and_common_prefix_are_pure_and_total() {
    use crate::app::cmd::{command_candidates, longest_common_prefix, COMMAND_NAMES};
    assert_eq!(command_candidates("rea"), vec!["readonly"]);
    assert_eq!(command_candidates("d"), vec!["d", "dn", "dt"]);
    assert!(command_candidates("zzz").is_empty());
    assert_eq!(command_candidates("").len(), COMMAND_NAMES.len());
    assert_eq!(longest_common_prefix(&[]), "");
    assert_eq!(longest_common_prefix(&["readonly"]), "readonly");
    assert_eq!(longest_common_prefix(&["d", "dn", "dt"]), "d");
    assert_eq!(longest_common_prefix(&["report", "readonly"]), "re");
    assert_eq!(longest_common_prefix(&["abc", "xyz"]), "");
}

// ---------------------------------------------------------------
// `?` opens help from any mode that isn't taking literal text
// ---------------------------------------------------------------

#[test]
fn question_mark_opens_help_from_every_panel_mode() {
    // Before this, only the grid honoured `?` — from Sessions or the
    // tap monitor it did nothing at all, and the footer's "? help"
    // pointer was wrong everywhere but Normal.
    for (mode, anchor) in [
        (Mode::Sessions, "active sessions"),
        (Mode::SlowQueries, "slow queries"),
        (Mode::TapMonitor, "jdbc tap"),
        (Mode::SchemaBrowser, "schema browser"),
        (Mode::ConnPick, "conn pick"),
        (Mode::RowDetail, "row detail"),
    ] {
        let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
        a.mode = mode;
        a.on_key(KeyEvent::from(KeyCode::Char('?')));
        assert_eq!(a.mode, Mode::Help, "? must open help from {mode:?}");
        assert_eq!(
            a.help.origin,
            Some(mode),
            "and the overlay must open at that mode's section"
        );
        assert_eq!(App::help_anchor_for(mode), Some(anchor));
        // `?` closes it again, back where it came from.
        a.on_key(KeyEvent::from(KeyCode::Char('?')));
        assert_eq!(a.mode, mode);
    }
}

#[test]
fn question_mark_types_a_literal_question_mark_while_a_text_input_has_focus() {
    // A `?` in the editor is a JDBC placeholder; in a filter it's a
    // character to match on. Neither may pop the help overlay.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.on_key(KeyEvent::from(KeyCode::Char('?')));
    assert_eq!(a.mode, Mode::Editor);
    assert_eq!(a.editor.buffer, "?");

    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.grid = Grid {
        columns: vec!["q".into()],
        rows: vec![vec!["a?b".into()]],
        truncated: false,
    };
    a.reset_grid_view();
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    assert_eq!(a.mode, Mode::GridFilter);
    a.on_key(KeyEvent::from(KeyCode::Char('?')));
    assert_eq!(a.mode, Mode::GridFilter, "still filtering");
    assert_eq!(a.grid_view.filter.as_deref(), Some("?"));

    // And not from the command bar either — `:help ?` would be typed
    // there, not dispatched by the key.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    a.on_key(KeyEvent::from(KeyCode::Char('?')));
    assert_eq!(a.mode, Mode::CommandBar);
    assert_eq!(a.command_bar.as_ref().map(|b| b.input.text()), Some("?"));
}

#[test]
fn p_on_the_failure_screen_opens_the_picker_with_a_single_candidate() {
    // Nothing discovered auto-connects any more, so a lone candidate
    // is reachable ONLY through the picker — and it need not be the
    // DSN that just failed (a --dsn can fail with one discovered
    // candidate sitting behind it). Gating `p` at two candidates left
    // that operator with `r retry` against a connection they never
    // chose.
    let dsn = crate::conn::Dsn::parse("postgres://app@flag-host/main").unwrap();
    let pick = DataSourcePick {
        name: "dataSource (application)".into(),
        origin: "Spring",
        dsn: Some(crate::conn::Dsn::parse("postgres://app@discovered-host/main").unwrap()),
        unresolved: Vec::new(),
        unresolved_host: Vec::new(),
        creds: Default::default(),
    };
    let mut a = App::new(
        Theme::default(),
        Some(dsn),
        vec![pick],
        SafetyConfig::default(),
    );
    a.splash_visible = false;
    a.mode = Mode::Normal;
    a.conn_state = ConnState::Failed("connection refused".into());
    a.on_key(KeyEvent::from(KeyCode::Char('p')));
    assert_eq!(a.mode, Mode::ConnPick, "p must open the picker");
    assert_eq!(a.conn_pick.index, 0);
}

#[test]
fn demo_timing_reports_an_elapsed_figure_like_a_live_run() {
    // `spawn_run_demo` never started the clock, so `\timing on` in
    // --demo printed no elapsed line at all — the one mode whose whole
    // job is to look exactly like a real session.
    let mut a = crate::demo::app(Theme::default());
    run_in_demo(&mut a, "\\timing on");
    assert!(a.timing_on);
    run_in_demo(&mut a, "SELECT id FROM users");
    assert!(
        a.query_started.is_some(),
        "the clock must be running when the answer is built"
    );
    pump_one_demo_msg(&mut a);
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.contains(" ms"),
        "timing on must add an elapsed figure: {status:?}"
    );
    assert!(
        a.query_started.is_none(),
        "and the QueryOk handler must consume it"
    );
}

// ---------------------------------------------------------------
// Editor log detection: no auto-completion inside a log, and no
// buffer clone per edit
// ---------------------------------------------------------------

/// A schema cache with one table, so completion has something to
/// offer — without it "no popup" would prove nothing.
fn app_with_completable_schema() -> App {
    let mut a = crate::demo::app(Theme::default());
    a.mode = Mode::Editor;
    a.editor.buffer.clear();
    a.editor.cursor = 0;
    a.completion = None;
    a
}

/// One Hibernate log line — enough for `detect_log` to say so.
const HIBERNATE_LINE: &str =
    "2024-01-15 10:00:00.123 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : \
     select u.id from users u where u.id=?";

#[test]
fn typing_a_dot_auto_triggers_completion_in_ordinary_sql() {
    // The control for the test below: with a normal buffer, `.` after
    // an identifier does pop the popup.
    let mut a = app_with_completable_schema();
    a.editor.buffer = "select * from users u where u".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::Char('.')));
    assert!(
        a.completion.is_some(),
        "a `.` after an alias should complete in SQL"
    );
}

#[test]
fn no_auto_completion_inside_a_pasted_log() {
    // Every `.` in `org.hibernate.SQL` and every `FROM ` inside a
    // logged statement would otherwise pop the popup, once per
    // keystroke, over text the operator is about to hand to ctrl-l.
    let mut a = app_with_completable_schema();
    a.editor.buffer = format!("{HIBERNATE_LINE}\nselect * from users u where u");
    a.editor.cursor = a.editor.buffer.len();
    assert!(
        a.editor_log_kind().is_some(),
        "fixture check: this buffer must read as a log"
    );
    a.on_key(KeyEvent::from(KeyCode::Char('.')));
    assert!(
        a.completion.is_none(),
        "no `.`-triggered popup inside a log"
    );

    // The keyword trigger is gated too.
    let mut a = app_with_completable_schema();
    a.editor.buffer = format!("{HIBERNATE_LINE}\nselect * from");
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::Char(' ')));
    assert!(
        a.completion.is_none(),
        "no keyword-triggered popup inside a log"
    );

    // Tab still completes on demand — the gate is on the automatic
    // trigger, not on completion itself.
    a.editor.buffer = format!("{HIBERNATE_LINE}\nselect * from us");
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::Tab));
    assert!(
        a.completion.is_some(),
        "Tab must still complete inside a log"
    );
}

#[test]
fn the_log_verdict_is_keyed_on_a_fingerprint_not_a_copy_of_the_buffer() {
    // 300 KiB of SQL. Keying the memo on a clone of the buffer meant
    // every keystroke here copied 300 KiB; the fingerprint is two
    // words wide however big the buffer gets.
    let mut a = app_with_completable_schema();
    a.editor.buffer = "select 1 from users;\n".repeat(15_000);
    assert!(a.editor.buffer.len() > 300 * 1024);
    a.editor.cursor = a.editor.buffer.len();
    assert_eq!(a.editor_log_kind(), None, "plain SQL is not a log");
    assert!(
        std::mem::size_of::<crate::app::BufferFingerprint>() <= 16,
        "the cache key must not own the text"
    );
    let before = a.editor_log_kind_cache.expect("verdict cached").0;

    // An edit at the end moves the fingerprint, so the next read
    // recomputes the verdict rather than serving a stale one.
    a.on_key(KeyEvent::from(KeyCode::Char('x')));
    a.editor_log_kind();
    let after = a.editor_log_kind_cache.expect("verdict cached").0;
    assert_ne!(before, after, "an edit must invalidate the memo");

    // Re-reading with no edit in between reuses it.
    a.editor_log_kind();
    assert_eq!(a.editor_log_kind_cache.expect("cached").0, after);
}

#[test]
fn the_log_verdict_only_scans_the_head_of_a_huge_buffer() {
    // Stated cost of the cap: a log pasted below 64 KiB of something
    // else doesn't fire the hint. ctrl-l / F8 still reconstruct from
    // the whole buffer.
    let mut a = app_with_completable_schema();
    let filler = "select 1 from users;\n".repeat(4_000);
    assert!(filler.len() > crate::query::logdetect::DETECT_HEAD_BYTES);
    a.editor.buffer = format!("{filler}{HIBERNATE_LINE}");
    assert_eq!(a.editor_log_kind(), None);
    // The same log inside the head IS seen.
    a.editor.buffer = format!("{HIBERNATE_LINE}\n{filler}");
    assert_eq!(
        a.editor_log_kind(),
        Some(crate::query::logdetect::LogKind::Hibernate)
    );
}

#[test]
fn the_tunnel_prompt_still_cancels_on_question_mark_and_colon() {
    // The prompt's footer says "y proceed · any other key cancels".
    // The global `?` / `:` handlers must not turn that into a lie by
    // opening an overlay over an armed ssh question.
    for key in [KeyCode::Char('?'), KeyCode::Char(':')] {
        let pick = DataSourcePick {
            name: "via-bastion".into(),
            origin: "project",
            dsn: Some(
                crate::conn::Dsn::parse(
                    "postgres://app@db.internal:5432/main?ssh_tunnel=tom@bastion.example.com",
                )
                .unwrap(),
            ),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        };
        let mut a = App::new(Theme::default(), None, vec![pick], SafetyConfig::default());
        a.splash_visible = false;
        a.mode = Mode::ConnPick;
        a.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            a.pending_tunnel.is_some(),
            "fixture check: enter must arm the tunnel confirmation"
        );
        a.on_key(KeyEvent::from(key));
        assert_eq!(a.mode, Mode::ConnPick, "{key:?} must not open an overlay");
        assert!(a.pending_tunnel.is_none(), "{key:?} must cancel");
        assert!(a.command_bar.is_none());
        let status = a.last_status.as_deref().unwrap_or("");
        assert!(status.contains("cancelled"), "got: {status}");
    }
}

#[test]
fn a_leading_separator_cannot_downgrade_a_block_to_a_confirm() {
    // `; DROP TABLE users` is one command to the server, which discards the
    // empty statement, but classified as `Other` (Confirm) when the raw
    // buffer was evaluated. The verified piece is what gets classified now.
    let mut a = crate::demo::app(Theme::default());
    for sql in [
        "; DROP TABLE users",
        "\n\n; DROP TABLE users",
        "/* hi */ ; DROP TABLE users",
        "-- x\n; DROP TABLE users",
    ] {
        a.last_error = None;
        a.mode = Mode::Normal;
        run_in_demo(&mut a, sql);
        let err = a.last_error.as_deref().unwrap_or("");
        assert!(
            err.contains("blocked by safety: DROP"),
            "{sql:?} → mode {:?}, error {err:?}",
            a.mode
        );
        assert_ne!(
            a.mode,
            Mode::Confirm,
            "{sql:?} must not reach the confirm prompt"
        );
    }
}

// -- decision modals own the keyboard ---------------------------------

/// A `Confirm`-mode app holding a guarded DELETE, the way `request_run`
/// leaves it. Two tabs open so the tab chords have somewhere to go.
fn confirm_modal_app() -> App {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.new_tab();
    assert_eq!(a.tabs.len(), 2);
    a.pending_run = Some(PendingRun {
        sql: "DELETE FROM orders".into(),
        kind: RunKind::Run,
        decision: crate::safety::Decision {
            kind: crate::safety::StatementKind::Delete { has_where: false },
            guard: crate::safety::Guard::Confirm,
            wrap_in_tx: true,
            blocked_by_read_only: false,
            read_only_escape: false,
        },
        is_batch: false,
        summary: None,
    });
    a.last_error = Some("relation \"orders\" does not exist".into());
    a.mode = Mode::Confirm;
    a
}

fn tx_decision_app() -> App {
    let mut a = confirm_modal_app();
    a.pending_run = None;
    a.tx_open = true;
    a.mode = Mode::TxDecision;
    a
}

fn confirm_terminate_app() -> App {
    let mut a = confirm_modal_app();
    a.pending_run = None;
    a.pending_terminate = Some(4242);
    a.mode = Mode::ConfirmTerminate;
    a
}

/// Every global chord that used to walk out of a decision modal.
fn modal_escape_keys() -> Vec<(&'static str, KeyEvent)> {
    vec![
        ("F2", KeyEvent::from(KeyCode::F(2))),
        ("F3", KeyEvent::from(KeyCode::F(3))),
        ("F4", KeyEvent::from(KeyCode::F(4))),
        (":", KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT)),
        (
            "ctrl-t",
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        ),
        (
            "ctrl-w",
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        ),
        (
            "ctrl-tab",
            KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL),
        ),
        (
            "ctrl-shift-backtab",
            KeyEvent::new(
                KeyCode::BackTab,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        ),
        (
            "alt-1",
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT),
        ),
        (
            "alt-2",
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT),
        ),
    ]
}

#[test]
fn tx_decision_modal_ignores_every_global_escape_chord() {
    for (name, key) in modal_escape_keys() {
        let mut a = tx_decision_app();
        let tabs_before = a.active_tab;
        a.on_key(key);
        assert_eq!(a.mode, Mode::TxDecision, "{name} escaped TxDecision");
        assert!(a.tx_open, "{name} dropped tx_open");
        assert_eq!(a.active_tab, tabs_before, "{name} switched tabs");
        assert!(a.command_bar.is_none(), "{name} opened the command bar");
    }
    // The modal's own keys still work.
    let mut a = tx_decision_app();
    a.on_key(KeyEvent::from(KeyCode::Char('n')));
    assert_ne!(a.mode, Mode::TxDecision, "n must still answer the modal");
}

#[test]
fn confirm_modal_ignores_every_global_escape_chord_except_colon() {
    for (name, key) in modal_escape_keys() {
        if name == ":" {
            continue;
        }
        let mut a = confirm_modal_app();
        let tabs_before = a.active_tab;
        a.on_key(key);
        assert_eq!(a.mode, Mode::Confirm, "{name} escaped Confirm");
        assert!(a.pending_run.is_some(), "{name} dropped the pending run");
        assert_eq!(a.active_tab, tabs_before, "{name} switched tabs");
    }
    // `:` is the documented exception: it cancels the guarded run
    // on the way into the bar, so nothing is left armed behind it.
    let mut a = confirm_modal_app();
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    assert_eq!(a.mode, Mode::CommandBar);
    assert!(a.pending_run.is_none(), "':' from Confirm cancels the run");
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Editor, "esc must not land on an empty modal");
    // And the modal's own keys still work.
    let mut a = confirm_modal_app();
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Editor);
    assert!(a.pending_run.is_none());
}

#[test]
fn confirm_terminate_modal_ignores_every_global_escape_chord() {
    for (name, key) in modal_escape_keys() {
        let mut a = confirm_terminate_app();
        let tabs_before = a.active_tab;
        a.on_key(key);
        assert_eq!(
            a.mode,
            Mode::ConfirmTerminate,
            "{name} escaped ConfirmTerminate"
        );
        assert_eq!(a.pending_terminate, Some(4242), "{name} dropped the pid");
        assert_eq!(a.active_tab, tabs_before, "{name} switched tabs");
        assert!(a.command_bar.is_none(), "{name} opened the command bar");
    }
    let mut a = confirm_terminate_app();
    a.on_key(KeyEvent::from(KeyCode::Char('n')));
    assert_eq!(a.mode, Mode::Sessions, "n must still answer the modal");
    assert_eq!(a.pending_terminate, None);
}

#[test]
fn f1_from_a_decision_modal_still_opens_help_and_returns_to_it() {
    let mut a = tx_decision_app();
    a.on_key(KeyEvent::from(KeyCode::F(1)));
    assert_eq!(a.mode, Mode::Help);
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::TxDecision, "help must hand the modal back");
    assert!(a.tx_open);
}

#[tokio::test]
async fn reconnect_drops_everything_a_decision_modal_was_holding() {
    // `\c` reaches `start_connect` directly. Whatever a modal held
    // belongs to the connection being dropped: the server rolls the
    // open tx back, the guarded run was classified against the old
    // database, the pid means nothing on the new one. Left set,
    // `tx_open` refused `\watch` for the rest of the session.
    let mut a = App::new(
        Theme::default(),
        Some(crate::conn::Dsn::parse("postgres://app@old-host/app").unwrap()),
        Vec::new(),
        SafetyConfig::default(),
    );
    a.tx_open = true;
    a.pending_terminate = Some(4242);
    a.pending_run = Some(PendingRun {
        sql: "DELETE FROM orders".into(),
        kind: RunKind::Run,
        decision: crate::safety::Decision {
            kind: crate::safety::StatementKind::Delete { has_where: false },
            guard: crate::safety::Guard::Confirm,
            wrap_in_tx: true,
            blocked_by_read_only: false,
            read_only_escape: false,
        },
        is_batch: false,
        summary: None,
    });
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c other_db".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(
        matches!(a.conn_state, ConnState::Connecting),
        "the reconnect must have started"
    );
    assert!(!a.tx_open, "tx_open survived a reconnect");
    assert!(a.pending_run.is_none(), "pending_run survived a reconnect");
    assert!(
        a.pending_terminate.is_none(),
        "pending_terminate survived a reconnect"
    );
}

#[tokio::test]
async fn a_guarded_run_cannot_survive_from_confirm_to_a_reconnect() {
    // The journey the review flagged: F3 / F4 walked out of `Confirm`
    // with `pending_run` still armed, and a later `\c` connected to a
    // different database with a DELETE classified against the old one
    // waiting for a `y`. Both halves are pinned here: the modal keeps
    // the keyboard, and the reconnect drops the run regardless.
    let mut a = App::new(
        Theme::default(),
        Some(crate::conn::Dsn::parse("postgres://app@old-host/app").unwrap()),
        Vec::new(),
        SafetyConfig::default(),
    );
    a.pending_run = confirm_modal_app().pending_run.take();
    a.mode = Mode::Confirm;
    for key in [KeyCode::F(3), KeyCode::F(4)] {
        a.on_key(KeyEvent::from(key));
        assert_eq!(a.mode, Mode::Confirm, "{key:?} escaped Confirm");
        assert!(a.pending_run.is_some(), "{key:?} dropped the run");
    }
    // Even with the modal somehow bypassed, `start_connect` is the
    // choke point every reconnect goes through.
    a.mode = Mode::Editor;
    a.editor.buffer = "\\c other_db".into();
    a.editor.cursor = a.editor.buffer.len();
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(matches!(a.conn_state, ConnState::Connecting));
    assert!(
        a.pending_run.is_none(),
        "a guarded run classified against the old database survived the reconnect"
    );
}

#[test]
fn a_query_ok_landing_mid_command_bar_leaves_no_stale_bar_behind() {
    // `:rea` half-typed when an auto-tx write's `QueryOk` lands: the
    // mode jumps to TxDecision underneath the bar. The bar is only
    // ever taken by its own key handler, so it used to sit in the
    // footer forever. The next key — any key — drops it.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::SHIFT));
    type_str(&mut a, "rea");
    assert_eq!(a.mode, Mode::CommandBar);
    a.on_msg(AppMsg::QueryOk {
        generation: a.generation,
        grid: Grid::default(),
        kind_label: "run".into(),
        tx_open_after: true,
    });
    assert_eq!(a.mode, Mode::TxDecision);
    assert!(a.tx_open);
    // A key the modal ignores — it must still clear the stale bar and
    // must NOT be swallowed by the bar's own handler.
    a.on_key(KeyEvent::from(KeyCode::Char('x')));
    assert_eq!(a.mode, Mode::TxDecision);
    assert!(
        a.command_bar.is_none(),
        "the stale `:rea` bar survived a key press in TxDecision"
    );
}

#[test]
fn f5_while_a_query_is_running_does_not_start_a_second_execution() {
    // A second F5 mid-flight used to pipeline a second run — an INSERT
    // pressed twice ran twice. The demo answers synchronously through
    // `msg_rx`, so "nothing was spawned" is "nothing was queued".
    let mut a = crate::demo::app(Theme::default());
    a.mode = Mode::Editor;
    a.editor.buffer = "INSERT INTO users (email) VALUES ('twice@example.com')".into();
    a.editor.cursor = a.editor.buffer.len();
    a.query_running = true;
    a.on_key(KeyEvent::from(KeyCode::F(5)));
    assert!(
        a.msg_rx.as_mut().unwrap().try_recv().is_err(),
        "a second execution was queued while one was running"
    );
    assert!(
        a.pending_run.is_none(),
        "must not even reach the confirm prompt"
    );
    assert_eq!(
        a.last_status.as_deref(),
        Some("a query is already running · ctrl-c cancels it")
    );
    assert_eq!(a.mode, Mode::Editor);
}

#[test]
fn j_and_g_stop_at_the_end_of_a_filtered_grid_not_the_whole_grid() {
    // Ten rows, one visible after `/alice`. `j` clamped to
    // `grid.row_count()`, so five presses selected row 5 of a
    // one-row view: no highlight, and Enter opened nothing.
    let rows: Vec<Vec<String>> = (0..10)
        .map(|i| {
            vec![
                i.to_string(),
                if i == 3 {
                    "alice".into()
                } else {
                    format!("user{i}")
                },
            ]
        })
        .collect();
    let mut a = app_with_grid(Grid {
        columns: vec!["id".into(), "name".into()],
        rows,
        truncated: false,
    });
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    type_str(&mut a, "alice");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::Normal);
    assert_eq!(a.grid_view.visible_rows, vec![3], "one visible row");
    for _ in 0..5 {
        a.on_key(KeyEvent::from(KeyCode::Char('j')));
    }
    assert_eq!(
        a.grid_state.selected(),
        Some(0),
        "j walked past the only visible row"
    );
    a.on_key(KeyEvent::from(KeyCode::Char('G')));
    assert_eq!(
        a.grid_state.selected(),
        Some(0),
        "G jumped past the filtered end"
    );
    a.on_key(KeyEvent::from(KeyCode::End));
    assert_eq!(a.grid_state.selected(), Some(0));
    // Unfiltered, the whole grid is still reachable.
    a.on_key(KeyEvent::from(KeyCode::Char('/')));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.grid_view.visible_rows.len(), 10);
    a.on_key(KeyEvent::from(KeyCode::Char('G')));
    assert_eq!(a.grid_state.selected(), Some(9));
}

#[test]
fn alt_digit_does_not_switch_tabs_under_a_typing_prompt() {
    // Tab 2 holds "select 2"; Ctrl-S opens the save-as prompt. Alt-1
    // used to swap tab 1's buffer in underneath the prompt, and Enter
    // saved "select 1" under the name typed for "select 2".
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.saved_queries_file = temp_saved_path("alt-digit-prompt");
    a.mode = Mode::Editor;
    a.editor.buffer = "select 1".into();
    a.new_tab();
    a.editor.buffer = "select 2".into();
    a.editor.cursor = a.editor.buffer.len();
    assert_eq!(a.active_tab, 1);
    a.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(a.mode, Mode::SaveQueryPrompt);
    a.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
    assert_eq!(a.active_tab, 1, "alt-1 switched tabs under the prompt");
    assert_eq!(
        a.editor.buffer, "select 2",
        "the prompt's buffer was swapped"
    );
    assert_eq!(a.mode, Mode::SaveQueryPrompt);
    a.saved_ui.save_name.clear();
    type_str(&mut a, "second");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(
        a.saved_queries.get("second").map(|q| q.body.as_str()),
        Some("select 2"),
        "saved the wrong tab's SQL"
    );
    // Out of the prompt, the jump works again.
    assert_eq!(a.mode, Mode::Editor);
    a.on_key(KeyEvent::from(KeyCode::Esc));
    a.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
    assert_eq!(a.active_tab, 0);
    assert_eq!(a.editor.buffer, "select 1");
}

// ----- A quiet server is "only this session", not "0 total" -----------

#[test]
fn sessions_status_says_only_this_session_when_nothing_else_is_connected() {
    assert_eq!(sessions_status(0, 0), "sessions · only this session");
    assert_eq!(sessions_status(0, 0), SESSIONS_ONLY_THIS_SESSION);
    assert_eq!(sessions_status(3, 1), "sessions · 3 total · 1 blocked");
    assert_eq!(sessions_status(1, 0), "sessions · 1 total · 0 blocked");
}

#[test]
fn sessions_loaded_with_no_other_backend_reports_only_this_session() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Sessions;
    a.generation = 1;
    a.on_msg(AppMsg::SessionsLoaded {
        generation: 1,
        result: Ok(Vec::new()),
    });
    assert_eq!(a.mode, Mode::Sessions, "the panel stays open");
    assert_eq!(a.last_status.as_deref(), Some(SESSIONS_ONLY_THIS_SESSION));
    a.on_msg(AppMsg::SessionsLoaded {
        generation: 1,
        result: Ok(vec![crate::query::sessions::SessionRow {
            pid: 4242,
            user: "app".into(),
            application: "svc".into(),
            state: "active".into(),
            wait_event: None,
            blocked_by: String::new(),
            query: "SELECT 1".into(),
            age_secs: 0.5,
        }]),
    });
    assert_eq!(
        a.last_status.as_deref(),
        Some("sessions · 1 total · 0 blocked")
    );
}

// ----- `:` then Tab lists every command where it can be seen ----------

#[test]
fn tab_on_an_empty_command_bar_lists_every_command_beside_the_bar() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Char(':')));
    a.on_key(KeyEvent::from(KeyCode::Tab));
    let list = a
        .command_bar_completions
        .clone()
        .expect("Tab on an empty bar lists the commands");
    for name in crate::app::cmd::COMMAND_NAMES {
        assert!(
            list.split(" · ").any(|c| c == *name),
            "{name} missing from {list:?}"
        );
    }
    // The next key moves on; Tab narrows again.
    a.on_key(KeyEvent::from(KeyCode::Char('r')));
    assert!(a.command_bar_completions.is_none());
    a.on_key(KeyEvent::from(KeyCode::Tab));
    assert_eq!(
        a.command_bar_completions.as_deref(),
        Some("readonly · report")
    );
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(a.command_bar_completions.is_none());
}

// ----- read-only refusal names a file only when there is one ----------

#[test]
fn read_only_escape_refusal_blames_the_file_only_when_it_exists() {
    assert_eq!(
        read_only_escape_refusal(true),
        crate::safety::READ_ONLY_ESCAPE_REFUSAL
    );
    assert_eq!(
        read_only_escape_refusal(false),
        "read-only by default · pgman --init-config writes safety.toml; set read_only = false for this database"
    );
    assert!(!read_only_escape_refusal(false).contains("by safety.toml"));
}

/// `:readonly off` under a read-only profile is refused with one of
/// the two forms — which one depends on the config dir of the machine
/// running the test, so only the pure chooser above pins the text.
#[test]
fn readonly_off_under_a_read_only_profile_is_refused_with_a_next_step() {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.on_key(KeyEvent::from(KeyCode::Char(':')));
    type_str(&mut a, "readonly off");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    let err = a.last_error.clone().unwrap_or_default();
    assert!(
        err == read_only_escape_refusal(true) || err == read_only_escape_refusal(false),
        "{err}"
    );
    assert!(a.read_only, "the setting did not change");
}

// ----- F2 error detail returns to where it was opened -----------------

/// F2 from the editor, Esc: back in the editor, and the next key typed
/// is text — not a Normal-mode hotkey.
#[test]
fn error_detail_closes_back_into_the_mode_it_was_opened_from() {
    let mut a = editor_app();
    a.editor.buffer = "select 1".into();
    a.editor.cursor = a.editor.buffer.len();
    a.last_error = Some("syntax error at or near \"1\"".into());
    a.on_key(KeyEvent::from(KeyCode::F(2)));
    assert_eq!(a.mode, Mode::ErrorDetail);
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Editor, "Esc must return to the editor");
    a.on_key(KeyEvent::from(KeyCode::Char('e')));
    assert_eq!(a.editor.buffer, "select 1e", "the key went into the buffer");

    // `q` and F2 close the same way; from Normal it stays Normal.
    for close in [KeyCode::Char('q'), KeyCode::F(2)] {
        let mut a = editor_app();
        a.last_error = Some("boom".into());
        a.on_key(KeyEvent::from(KeyCode::F(2)));
        a.on_key(KeyEvent::from(close));
        assert_eq!(a.mode, Mode::Editor);
    }
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Normal;
    a.last_error = Some("boom".into());
    a.on_key(KeyEvent::from(KeyCode::F(2)));
    a.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(a.mode, Mode::Normal);
}

// ----- Quote / bracket autoclose: hand-typed SQL comes out as typed ----

fn editor_app() -> App {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a
}

/// Typing a whole statement key by key yields exactly that text —
/// the closer autoclose inserted is skipped over, not left behind.
/// These three are the product-review inputs verbatim: they came out
/// as `…'shipped''`, `'{"a":1}''"}'`, and a mangled `select 'a;b' x`.
#[test]
fn typing_a_quoted_literal_key_by_key_yields_exactly_the_typed_text() {
    for sql in [
        "select * from orders where status = 'shipped'",
        "'{\"a\":1}'",
        "select 'a;b' x",
        "select \"col\" from t where a = (1) and b in ('x', 'y')",
        // `AND ` / `FROM ` auto-trigger the completion popup inside the
        // literal; the pending closer must survive a popup that only
        // opened (nothing was inserted).
        "select 'a and b from c' as s",
    ] {
        let mut a = editor_app();
        type_str(&mut a, sql);
        assert_eq!(a.editor.buffer, sql, "typed {sql:?}");
        assert_eq!(a.editor.cursor, sql.len());
        assert!(
            a.auto_closers.is_empty(),
            "every closer was typed over: {:?}",
            a.auto_closers
        );
    }
}

/// An apostrophe inside a word is an apostrophe, not a pair: `don't`
/// in a comment stays `don't`, and the SQL `''` escape typed inside a
/// literal builds `'don''t'` exactly as typed.
#[test]
fn an_apostrophe_inside_a_word_is_never_paired() {
    for sql in ["-- don't do this", "select 'don''t'", "select 'it''s' as s"] {
        let mut a = editor_app();
        type_str(&mut a, sql);
        assert_eq!(a.editor.buffer, sql, "typed {sql:?}");
    }
}

/// A closer that reached the buffer any other way — a bracketed
/// paste — is not ours: typing a quote in front of it inserts a
/// literal quote, and the paste itself is untouched.
#[test]
fn a_bracketed_paste_is_untouched_and_its_closers_are_not_skipped() {
    let mut a = editor_app();
    a.on_paste("select 'a;b' x".to_string());
    assert_eq!(a.editor.buffer, "select 'a;b' x");
    assert!(a.auto_closers.is_empty());
    // Move back inside the literal (before the closing quote) and
    // type a quote: the pasted closer is not skipped.
    for _ in 0..3 {
        a.on_key(KeyEvent::from(KeyCode::Left));
    }
    a.on_key(KeyEvent::from(KeyCode::Char('\'')));
    assert_eq!(a.editor.buffer, "select 'a;b'' x");
}

/// Deleting an auto-inserted closer forgets it: the next closer typed
/// is a literal. And an insert before a pending closer keeps the
/// skip pointed at the right byte.
#[test]
fn a_deleted_auto_closer_is_forgotten_and_an_insert_before_it_shifts_it() {
    let mut a = editor_app();
    a.on_key(KeyEvent::from(KeyCode::Char('(')));
    assert_eq!(a.editor.buffer, "()");
    a.on_key(KeyEvent::from(KeyCode::Delete));
    assert_eq!(a.editor.buffer, "(");
    a.on_key(KeyEvent::from(KeyCode::Char(')')));
    assert_eq!(a.editor.buffer, "()", "the typed `)` is a literal insert");
    assert_eq!(a.editor.cursor, 2);

    let mut a = editor_app();
    type_str(&mut a, "f(");
    // Enter auto-indents the new line (the `(` opened it): the two
    // spaces come from the newline, not from typing.
    a.on_key(KeyEvent::from(KeyCode::Enter));
    type_str(&mut a, "1, 'é'");
    assert_eq!(a.editor.buffer, "f(\n  1, 'é')");
    a.on_key(KeyEvent::from(KeyCode::Char(')')));
    assert_eq!(
        a.editor.buffer, "f(\n  1, 'é')",
        "skipped the shifted closer"
    );
    assert_eq!(a.editor.cursor, a.editor.buffer.len());
}

/// Undo replaces the buffer wholesale: nothing pending survives it.
#[test]
fn undo_forgets_pending_auto_closers() {
    let mut a = editor_app();
    a.on_key(KeyEvent::from(KeyCode::Char('(')));
    assert!(!a.auto_closers.is_empty());
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "");
    assert!(a.auto_closers.is_empty());
}

#[test]
fn auto_closers_stack_tracks_inserts_and_deletes() {
    let mut s = AutoClosers::default();
    s.push(1, ')');
    s.shift_insert(1, 3); // typed "abc" at 1 → closer now at 4
    assert!(!s.take_at(1, ')', "(abc)"));
    assert!(s.take_at(4, ')', "(abc)"));
    assert!(s.is_empty());

    let mut s = AutoClosers::default();
    s.push(1, '\'');
    s.push(3, '\'');
    s.shift_delete(2, 4); // deleted the bytes holding the second closer
    assert!(!s.take_at(3, '\'', "''"));
    assert!(s.take_at(1, '\'', "''"));

    // Present in the stack but no longer in the buffer: not a skip.
    let mut s = AutoClosers::default();
    s.push(0, '"');
    assert!(!s.take_at(0, '"', "x"));
    assert!(!s.take_at(0, '"', ""));

    // A length change the stack was not told about drops it.
    let mut s = AutoClosers::default();
    s.push(0, ')');
    s.note_len(2);
    s.sync(2);
    assert!(!s.is_empty());
    s.sync(7);
    assert!(s.is_empty());
}

// -- Enter runs a terminated statement; Alt-Enter runs regardless --

fn editor_with(buffer: &str) -> App {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.mode = Mode::Editor;
    a.editor.buffer = buffer.into();
    a.editor.cursor = a.editor.buffer.len();
    a
}

/// With no client connected `request_run` answers "not connected" —
/// the same signal the Ctrl-Enter / Ctrl-J tests above key on.
fn reached_request_run(a: &App) -> bool {
    a.last_error
        .as_deref()
        .map(|e| e.contains("not connected"))
        .unwrap_or(false)
}

#[test]
fn enter_runs_is_true_only_for_a_terminated_statement_or_a_backslash_command() {
    assert!(enter_runs("select 1;"));
    assert!(
        enter_runs("select 1;  \n"),
        "trailing whitespace after the ; is still terminated"
    );
    assert!(enter_runs("\\l"));
    assert!(enter_runs("  \\d users"));
    assert!(!enter_runs("select 1"));
    assert!(
        !enter_runs("select 1; -- and then"),
        "a ; mid-buffer is not a terminator"
    );
    assert!(!enter_runs(""));
    assert!(!enter_runs("   "));
}

#[test]
fn ensure_terminated_appends_a_semicolon_only_when_one_is_missing() {
    assert_eq!(ensure_terminated("select 1"), "select 1;");
    assert_eq!(ensure_terminated("select 1  "), "select 1;");
    assert_eq!(ensure_terminated("select 1;"), "select 1;");
    assert_eq!(ensure_terminated("\\l"), "\\l");
    assert_eq!(ensure_terminated(""), "", "an empty pick stays empty");
    assert_eq!(ensure_terminated("  "), "  ");
}

#[test]
fn enter_on_a_terminated_statement_runs_it() {
    let mut a = editor_with("select 1;");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(
        reached_request_run(&a),
        "Enter after ; should hit request_run; last_error = {:?}",
        a.last_error
    );
    assert_eq!(a.editor.buffer, "select 1;", "no newline was inserted");
}

#[test]
fn enter_on_an_unterminated_statement_inserts_a_newline() {
    let mut a = editor_with("select 1");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.editor.buffer, "select 1\n");
    assert_eq!(a.editor.cursor, a.editor.buffer.len());
    assert!(a.last_error.is_none(), "nothing ran: {:?}", a.last_error);
}

#[test]
fn shift_enter_inserts_a_newline_even_after_a_semicolon() {
    let mut a = editor_with("select 1;");
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert_eq!(a.editor.buffer, "select 1;\n");
    assert!(a.last_error.is_none());
}

#[test]
fn alt_enter_runs_an_unterminated_statement() {
    let mut a = editor_with("select 1");
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    assert!(
        reached_request_run(&a),
        "Alt-Enter should hit request_run; last_error = {:?}",
        a.last_error
    );
    assert_eq!(a.editor.buffer, "select 1");
}

#[test]
fn enter_on_a_backslash_command_runs_it() {
    // `\l` needs no terminator; dispatch_backslash clears the buffer
    // on the way through, which is the proof it ran rather than
    // gaining a newline.
    let mut a = editor_with("\\l");
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.editor.buffer, "", "the backslash command was dispatched");
}

fn popup_with(a: &mut App, selected: Option<usize>) {
    use crate::query::complete::CandidateKind;
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
        selected,
    });
}

#[test]
fn enter_with_the_completion_popup_open_accepts_the_first_candidate() {
    let mut a = editor_with("");
    popup_with(&mut a, None);
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.editor.buffer, "SELECT * FROM users", "no newline, no run");
    assert_eq!(a.editor.cursor, a.editor.buffer.len());
    assert!(a.completion.is_none(), "the popup closed");
    assert!(a.last_error.is_none(), "nothing ran: {:?}", a.last_error);
    assert_eq!(
        a.last_status.as_deref(),
        Some("completion · accepted · table")
    );
}

#[test]
fn enter_with_a_highlighted_candidate_keeps_that_one() {
    let mut a = editor_with("");
    popup_with(&mut a, Some(1));
    // Mirror what the second Tab did: the highlighted insert is in the buffer.
    a.editor.buffer = "SELECT * FROM user_logs".into();
    a.editor.cursor = a.editor.buffer.len();
    a.completion.as_mut().unwrap().end = a.editor.cursor;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.editor.buffer, "SELECT * FROM user_logs");
    assert!(a.completion.is_none());
}

#[test]
fn log_pick_enter_loads_a_terminated_statement() {
    // The wedge is paste → ctrl-l → pick → Enter → Enter: the second
    // Enter only runs if the first left a `;` behind.
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.log_pick.picks = log_picks_with_a_cluster_of(&["select * from item where order_id=101"]);
    a.log_pick.clusters = crate::query::nplus1::detect(&a.log_pick.picks);
    a.mode = Mode::LogPick;
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(a.mode, Mode::Editor);
    assert!(
        enter_runs(&a.editor.buffer),
        "loaded: {:?}",
        a.editor.buffer
    );
    a.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(reached_request_run(&a), "last_error = {:?}", a.last_error);
}

// ----- Ctrl-F: built-in formatter, pg_format when installed ---------------

/// A `PATH` holding only `tests/bin/` — where `fake_pg_format` lives.
fn stub_path() -> std::ffi::OsString {
    format!("{}/tests/bin", env!("CARGO_MANIFEST_DIR")).into()
}

/// A `PATH` with nothing on it.
fn empty_path() -> std::ffi::OsString {
    std::env::temp_dir().into()
}

/// A throwaway directory holding an executable actually named
/// `pg_format` (a copy of `tests/bin/fake_pg_format`), so a
/// `PATH` of just this directory is "pg_format is installed".
/// Removed on drop.
struct PgFormatDir(std::path::PathBuf);

impl PgFormatDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("pgman-pgfmt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // `fs::copy` carries the stub's executable bit across.
        std::fs::copy(
            format!("{}/tests/bin/fake_pg_format", env!("CARGO_MANIFEST_DIR")),
            dir.join("pg_format"),
        )
        .unwrap();
        Self(dir)
    }

    fn path_var(&self) -> std::ffi::OsString {
        self.0.clone().into()
    }
}

impl Drop for PgFormatDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn ctrl_f_without_pg_format_uses_the_built_in_formatter() {
    let mut a = editor_app();
    set_editor(&mut a, "select a, b from t where x = 'select'");
    a.reformat_buffer_with_path(Some(&empty_path()));
    assert_eq!(
        a.editor.buffer,
        "SELECT\n  a,\n  b\nFROM\n  t\nWHERE\n  x = 'select'"
    );
    assert_eq!(a.editor.cursor, a.editor.buffer.len());
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.starts_with("formatted (built-in)"),
        "got: {status:?}"
    );
    assert!(a.last_error.is_none(), "last_error = {:?}", a.last_error);
    assert!(a.draft_dirty);
}

#[test]
fn ctrl_f_with_pg_format_on_path_prefers_it() {
    let installed = PgFormatDir::new("prefers");
    let mut a = editor_app();
    set_editor(&mut a, "select 1");
    a.reformat_buffer_with_path(Some(&installed.path_var()));
    assert_eq!(a.editor.buffer, "-- FORMATTED BY FAKE PG_FORMAT\nselect 1");
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(
        status.starts_with("formatted via pg_format"),
        "got: {status:?}"
    );
}

#[test]
fn ctrl_f_honours_the_editor_knobs() {
    let mut a = editor_app();
    a.format_opts = crate::query::format::FormatOptions {
        indent: 4,
        keywords: crate::query::format::KeywordCase::Lower,
    };
    set_editor(&mut a, "SELECT a FROM t");
    a.reformat_buffer_with_path(Some(&empty_path()));
    assert_eq!(a.editor.buffer, "select\n    a\nfrom\n    t");
}

#[test]
fn ctrl_f_leaves_a_dollar_quoted_buffer_alone_and_says_so() {
    let mut a = editor_app();
    let sql = "create function f() returns int as $$\n  select   1;\n$$ language sql";
    set_editor(&mut a, sql);
    a.reformat_buffer_with_path(Some(&empty_path()));
    assert_eq!(a.editor.buffer, sql);
    assert_eq!(
        a.last_status.as_deref(),
        Some("formatting skipped: dollar-quoted body")
    );
    assert!(a.last_error.is_none());
}

#[test]
fn ctrl_f_on_an_empty_buffer_is_a_status_not_a_format() {
    let mut a = editor_app();
    set_editor(&mut a, "   \n");
    a.reformat_buffer_with_path(Some(&empty_path()));
    assert_eq!(a.editor.buffer, "   \n");
    assert_eq!(a.last_status.as_deref(), Some("nothing to format"));
}

/// The key path: Ctrl-F goes through `on_editor_key`, whose undo
/// snapshot wraps it, so Ctrl-Z brings the pre-format text back.
#[test]
fn ctrl_f_then_undo_restores_the_unformatted_buffer() {
    let mut a = editor_app();
    set_editor(&mut a, "select a from t");
    a.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert_ne!(
        a.editor.buffer, "select a from t",
        "Ctrl-F should have reformatted"
    );
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.starts_with("formatted"), "got: {status:?}");
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "select a from t");
    a.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_ne!(a.editor.buffer, "select a from t", "redo should re-apply");
}

#[test]
fn find_on_path_in_finds_an_executable_stub_and_nothing_else() {
    assert!(find_on_path_in("fake_pg_format", &stub_path()).is_some());
    assert!(find_on_path_in("fake_pg_format", &empty_path()).is_none());
    assert!(find_on_path_in("definitely_not_a_real_binary_xyz", &stub_path()).is_none());
    // An empty PATH entry is not "the current directory": the stub is
    // reachable relative to the crate root (where unit tests run),
    // and must still not be found through an empty entry.
    assert!(std::path::Path::new("tests/bin/fake_pg_format").exists());
    assert!(find_on_path_in("tests/bin/fake_pg_format", std::ffi::OsStr::new("")).is_none());
}

// ----- Auto-indent on Enter, de-indent on Backspace, indent on Tab ---------

fn press(a: &mut App, code: KeyCode) {
    a.on_key(KeyEvent::from(code));
}

#[test]
fn enter_after_an_opening_paren_indents_the_new_line() {
    let mut a = editor_app();
    type_str(&mut a, "insert into t (");
    // Autoclose put the `)` after the cursor; the line up to the
    // cursor ends in `(`.
    assert_eq!(a.editor.buffer, "insert into t ()");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.editor.buffer, "insert into t (\n  )");
    assert_eq!(a.editor.cursor, "insert into t (\n  ".len());
}

#[test]
fn enter_after_a_keyword_or_comma_indents_and_a_plain_line_inherits() {
    let mut a = editor_app();
    type_str(&mut a, "select");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.editor.buffer, "select\n  ");
    type_str(&mut a, "a,");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.editor.buffer, "select\n  a,\n    ");
    type_str(&mut a, "b");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.editor.buffer, "select\n  a,\n    b\n    ");
}

#[test]
fn enter_uses_the_configured_indent_width() {
    let mut a = editor_app();
    a.format_opts.indent = 4;
    type_str(&mut a, "select");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.editor.buffer, "select\n    ");
}

/// A terminated statement runs on Enter, so the column-0 rule shows
/// where Enter still inserts: a `;` line with more text after it.
#[test]
fn enter_after_a_semicolon_line_starts_at_column_0() {
    let mut a = editor_app();
    set_editor(&mut a, "  select 1;\nselect 2");
    a.editor.cursor = "  select 1;".len();
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.editor.buffer, "  select 1;\n\nselect 2");
    assert_eq!(a.editor.cursor, "  select 1;\n".len());
}

#[test]
fn enter_on_a_terminated_buffer_still_runs_rather_than_indents() {
    let mut a = editor_app();
    set_editor(&mut a, "select 1;");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.editor.buffer, "select 1;");
    assert!(reached_request_run(&a), "last_error = {:?}", a.last_error);
}

#[test]
fn backspace_inside_indentation_removes_a_level() {
    let mut a = editor_app();
    set_editor(&mut a, "select\n    x");
    a.editor.cursor = "select\n    ".len();
    press(&mut a, KeyCode::Backspace);
    assert_eq!(a.editor.buffer, "select\n  x");
    assert_eq!(a.editor.cursor, "select\n  ".len());
    press(&mut a, KeyCode::Backspace);
    assert_eq!(a.editor.buffer, "select\nx");
    // At column 0 Backspace is the ordinary join-lines.
    press(&mut a, KeyCode::Backspace);
    assert_eq!(a.editor.buffer, "selectx");
}

#[test]
fn backspace_off_a_stop_snaps_back_to_the_previous_one() {
    let mut a = editor_app();
    set_editor(&mut a, "   ");
    press(&mut a, KeyCode::Backspace);
    assert_eq!(a.editor.buffer, "  ");
}

#[test]
fn backspace_after_text_is_a_single_character() {
    let mut a = editor_app();
    set_editor(&mut a, "  ab");
    press(&mut a, KeyCode::Backspace);
    assert_eq!(a.editor.buffer, "  a");
}

#[test]
fn tab_inside_indentation_adds_a_level_and_elsewhere_completes() {
    let mut a = editor_app();
    set_editor(&mut a, "select\n  ");
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.editor.buffer, "select\n    ");
    assert!(a.completion.is_none());
    // Off a stop: up to the next one.
    set_editor(&mut a, "select\n   ");
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.editor.buffer, "select\n    ");
    // After text on the line, Tab is completion as before.
    set_editor(&mut a, "select * from ");
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.editor.buffer, "select * from ");
    let status = a.last_status.as_deref().unwrap_or("");
    assert!(status.starts_with("completion"), "got: {status:?}");
}

#[test]
fn auto_indent_is_one_undo_step_with_the_newline() {
    let mut a = editor_app();
    set_editor(&mut a, "select");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.editor.buffer, "select\n  ");
    a.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(a.editor.buffer, "select");
}
