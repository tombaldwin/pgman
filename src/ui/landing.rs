//! The "start card" shown in the result panel right after connecting,
//! before the operator has run anything — replaces the bare `(no rows)`
//! placeholder that used to be the first thing every new user saw.
//!
//! [`landing_lines`] is pure (dimensions in, styled `Line`s out) so the
//! layout — column count, height degradation — is unit-testable without
//! a terminal. [`draw_landing`] just wraps it in the bordered block.

use super::*;

/// Below this inner width, key hints stack in a single column instead
/// of two side by side.
const TWO_COL_MIN_WIDTH: u16 = 64;
/// Most recent history entries shown in the `recent` section.
const RECENT_MAX_ROWS: usize = 5;
/// Gap (in columns) between the two key-hint columns.
const COL_GAP: usize = 3;
/// Every key hint's key glyph is left-justified into a field this wide
/// before the description starts, so single-char keys (`e`) and
/// two-char keys (`F8`) line up on the same description column.
const KEY_FIELD_WIDTH: usize = 4;
/// Leading indent for key-hint / recent-row lines (a "sub-list" look
/// under the header / `recent` label, which get a single-space indent).
const HINT_INDENT: &str = "   ";

const F8_DESC_LONG: &str = "logs → SQL (paste a Hibernate / Postgres log in the editor)";
const F8_DESC_SHORT: &str = "logs → SQL (paste a log first)";
const F4_DESC: &str = "JDBC tap";

/// Draw the landing / start card into `area`.
pub(super) fn draw_landing(f: &mut Frame, area: Rect, app: &App) {
    let block = bordered(&app.theme, "start");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = landing_lines(app, inner.width, inner.height);
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// One key hint: the key glyph plus its description.
struct Hint {
    key: &'static str,
    desc: String,
}

impl Hint {
    fn new(key: &'static str, desc: impl Into<String>) -> Self {
        Hint {
            key,
            desc: desc.into(),
        }
    }
}

/// Build the styled content of the start card for a `inner_width` ×
/// `inner_height` content area. Pure — no rendering, so it's directly
/// unit-testable.
///
/// Layout, top to bottom: a one-line connection summary; a blank line;
/// six core key hints (two columns when `inner_width >= 64`, one
/// otherwise); the F8/F4 differentiator row; a `?  all keys` line;
/// then (if there's room and history isn't empty) a blank line, a
/// `recent` label, and up to five of the most recent history entries.
///
/// Under height pressure, sections drop in this order: `recent` first,
/// then the `?` line, then the F8/F4 row — so the connection line and
/// the six core keys always survive down to 8 rows.
pub(crate) fn landing_lines(app: &App, inner_width: u16, inner_height: u16) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let key_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.text);
    let muted_style = Style::default().fg(theme.muted);

    let two_col = inner_width >= TWO_COL_MIN_WIDTH;
    let width = inner_width as usize;

    // --- core hints -------------------------------------------------
    let table_hint = match app.schema_cache.tables.len() {
        0 => "schema browser".to_string(),
        n => format!("schema browser ({n} tables)"),
    };
    let saved_hint = format!("saved queries ({})", app.saved_queries.entries.len());

    let e = Hint::new("e", "write a query");
    let s = Hint::new("S", table_hint);
    let q = Hint::new("Q", saved_hint);
    let t = Hint::new("T", "slow queries");
    let w = Hint::new("W", "schema wizard");
    let l = Hint::new("L", "sessions & locks");
    let help = Hint::new("?", "all keys");

    // F8's description is long; shorten it rather than truncate it if
    // it (plus, in two-column mode, F4's cell and the gap) wouldn't
    // fit at the current width.
    let f8_desc = pick_f8_desc(two_col, width);
    let f8 = Hint::new("F8", f8_desc);
    let f4 = Hint::new("F4", F4_DESC);

    // --- height budget: decide what survives -------------------------
    let key_lines_floor = if two_col { 3 } else { 6 }; // core only
    let key_lines_with_f8f4 = key_lines_floor + 1; // + F8/F4 row
    let key_lines_with_help = key_lines_with_f8f4 + 1; // + `?` line
    let recent_len = if app.history.is_empty() {
        0
    } else {
        2 + app.history.len().min(RECENT_MAX_ROWS) // blank + label + rows
    };
    const HEADER_LINES: usize = 2; // connection line + blank

    let h = inner_height as usize;
    let (include_recent, include_help, include_f8f4) =
        if HEADER_LINES + key_lines_with_help + recent_len <= h {
            (true, true, true)
        } else if HEADER_LINES + key_lines_with_help <= h {
            (false, true, true)
        } else if HEADER_LINES + key_lines_with_f8f4 <= h {
            (false, false, true)
        } else {
            (false, false, false)
        };
    let include_recent = include_recent && !app.history.is_empty();

    // --- assemble ------------------------------------------------------
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(header_line(app, muted_style));
    lines.push(Line::from(""));

    if two_col {
        // Shared left-column width: indent + key field + widest LEFT
        // description among the paired rows (e, Q, W, F8).
        let left_descs: Vec<&str> = if include_f8f4 {
            vec![
                e.desc.as_str(),
                q.desc.as_str(),
                w.desc.as_str(),
                f8.desc.as_str(),
            ]
        } else {
            vec![e.desc.as_str(), q.desc.as_str(), w.desc.as_str()]
        };
        let left_col_width = HINT_INDENT.chars().count()
            + KEY_FIELD_WIDTH
            + left_descs
                .iter()
                .map(|d| d.chars().count())
                .max()
                .unwrap_or(0);

        lines.push(paired_line(&e, &s, key_style, desc_style, left_col_width));
        lines.push(paired_line(&q, &t, key_style, desc_style, left_col_width));
        lines.push(paired_line(&w, &l, key_style, desc_style, left_col_width));
        if include_f8f4 {
            lines.push(paired_line(&f8, &f4, key_style, desc_style, left_col_width));
        }
        if include_help {
            lines.push(single_line(&help, key_style, desc_style));
        }
    } else {
        for hint in [&e, &s, &q, &t, &w, &l] {
            lines.push(single_line(hint, key_style, desc_style));
        }
        if include_f8f4 {
            lines.push(single_line(&f8, key_style, desc_style));
            lines.push(single_line(&f4, key_style, desc_style));
        }
        if include_help {
            lines.push(single_line(&help, key_style, desc_style));
        }
    }

    if include_recent {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(" recent", muted_style)));
        for entry in app.history.iter().rev().take(RECENT_MAX_ROWS) {
            let one_line = entry.split_whitespace().collect::<Vec<_>>().join(" ");
            let truncated = crate::grid::truncate_cell(
                &one_line,
                width.saturating_sub(HINT_INDENT.chars().count()),
            );
            lines.push(Line::from(vec![
                Span::raw(HINT_INDENT),
                Span::styled(truncated, desc_style),
            ]));
        }
    }

    lines
}

/// Choose the F8 hint description: the long form if it (plus, in
/// two-column mode, the F4 cell and the gap between them) fits within
/// `width`; the short form otherwise. Never truncates either form.
fn pick_f8_desc(two_col: bool, width: usize) -> &'static str {
    let needed = if two_col {
        HINT_INDENT.chars().count()
            + KEY_FIELD_WIDTH
            + F8_DESC_LONG.chars().count()
            + COL_GAP
            + KEY_FIELD_WIDTH
            + F4_DESC.chars().count()
    } else {
        HINT_INDENT.chars().count() + KEY_FIELD_WIDTH + F8_DESC_LONG.chars().count()
    };
    if needed <= width {
        F8_DESC_LONG
    } else {
        F8_DESC_SHORT
    }
}

/// The connection-summary line: `connected to <db> on <host>:<port> ·
/// pg <version> · RO|RW`. Never renders the password or raw DSN — only
/// the individual `Dsn` fields. `RO` reuses the exact style
/// `footer_badges` uses for the read-only pill, so the two never drift
/// out of sync.
fn header_line(app: &App, muted_style: Style) -> Line<'static> {
    let db = app.dsn.as_ref().map(|d| d.dbname.as_str()).unwrap_or("?");
    let host_port = app
        .dsn
        .as_ref()
        .map(|d| format!("{}:{}", d.host, d.port))
        .unwrap_or_else(|| "?".to_string());
    let version = match &app.conn_state {
        ConnState::Connected { server_version } => server_version.as_str(),
        _ => "?",
    };
    let mut spans = vec![Span::styled(
        format!(" connected to {db} on {host_port} · pg {version} · "),
        muted_style,
    )];
    if app.read_only {
        spans.push(Span::styled("RO", read_only_style(app)));
    } else {
        spans.push(Span::styled("RW", muted_style));
    }
    Line::from(spans)
}

/// The `Style` `footer_badges` uses for its `RO` pill — reused here
/// verbatim (rather than re-deriving a colour) so the header line's
/// `RO` can never diverge from the footer's.
fn read_only_style(app: &App) -> Style {
    super::footer_badges_with(app, &app.theme, 0)
        .into_iter()
        .find(|s| s.content.trim() == "RO")
        .map(|s| s.style)
        .unwrap_or_default()
}

/// Render one key hint on its own line (single-column layout).
fn single_line(hint: &Hint, key_style: Style, desc_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::raw(HINT_INDENT),
        Span::styled(format!("{:<KEY_FIELD_WIDTH$}", hint.key), key_style),
        Span::styled(hint.desc.clone(), desc_style),
    ])
}

/// Render two key hints on one line: `left` indented and padded to
/// `left_col_width`, then a gap, then `right`.
fn paired_line(
    left: &Hint,
    right: &Hint,
    key_style: Style,
    desc_style: Style,
    left_col_width: usize,
) -> Line<'static> {
    let left_text_width = HINT_INDENT.chars().count() + KEY_FIELD_WIDTH + left.desc.chars().count();
    let pad = left_col_width.saturating_sub(left_text_width);
    Line::from(vec![
        Span::raw(HINT_INDENT),
        Span::styled(format!("{:<KEY_FIELD_WIDTH$}", left.key), key_style),
        Span::styled(left.desc.clone(), desc_style),
        Span::raw(" ".repeat(pad + COL_GAP)),
        Span::styled(format!("{:<KEY_FIELD_WIDTH$}", right.key), key_style),
        Span::styled(right.desc.clone(), desc_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::Dsn;
    use crate::safety::SafetyConfig;
    use crate::theme::Theme;

    fn app() -> App {
        let dsn = Some(Dsn::parse("postgres://test@localhost/test").unwrap());
        let mut a = App::new(Theme::default(), dsn, Vec::new(), SafetyConfig::default());
        a.conn_state = ConnState::Connected {
            server_version: "16.0".into(),
        };
        a
    }

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wide_layout_uses_two_columns() {
        let a = app();
        let lines = landing_lines(&a, 80, 30);
        let text = plain(&lines);
        // e/S share a line in two-column mode.
        let row = text
            .iter()
            .find(|l| l.contains('e') && l.contains("write a query"))
            .unwrap();
        assert!(
            row.contains('S'),
            "two-col row should also carry S: {row:?}"
        );
        assert!(row.contains("schema browser"), "row: {row:?}");
    }

    #[test]
    fn narrow_layout_uses_one_column() {
        let a = app();
        let lines = landing_lines(&a, 40, 30);
        let text = plain(&lines);
        let e_row = text.iter().find(|l| l.contains("write a query")).unwrap();
        assert!(
            !e_row.contains("schema browser"),
            "single-col row must not also carry S's hint: {e_row:?}"
        );
    }

    #[test]
    fn f8_row_present_at_80_columns() {
        let a = app();
        let lines = landing_lines(&a, 80, 30);
        let text = plain(&lines);
        assert!(
            text.iter().any(|l| l.contains("F8") && l.contains("logs")),
            "expected an F8 row at 80 columns: {text:?}"
        );
        assert!(
            text.iter()
                .any(|l| l.contains("F4") && l.contains("JDBC tap")),
            "expected an F4 row at 80 columns: {text:?}"
        );
    }

    #[test]
    fn f8_desc_shortens_when_it_would_not_fit() {
        // Long form needs far more than 40 columns.
        assert_eq!(pick_f8_desc(false, 40), F8_DESC_SHORT);
        assert_eq!(pick_f8_desc(false, 200), F8_DESC_LONG);
    }

    #[test]
    fn height_degradation_drops_recent_first() {
        let mut a = app();
        a.history = vec!["select 1".into()];
        // Plenty of room: recent survives.
        let full = landing_lines(&a, 80, 30);
        assert!(plain(&full).iter().any(|l| l.contains("recent")));

        // Enough for header + keys + F8/F4 + `?`, not for `recent`.
        let no_recent = landing_lines(&a, 80, 7);
        let text = plain(&no_recent);
        assert!(!text.iter().any(|l| l.contains("recent")));
        assert!(text
            .iter()
            .any(|l| l.contains('?') && l.contains("all keys")));
        assert!(text.iter().any(|l| l.contains("F8")));
    }

    #[test]
    fn height_degradation_drops_help_second() {
        let mut a = app();
        a.history = vec!["select 1".into()];
        // Two-col floor is 5 (header+blank+3 core rows), F8/F4 adds 1 (6),
        // `?` adds 1 more (7). At height 6 we should have F8/F4 but not `?`.
        let lines = landing_lines(&a, 80, 6);
        let text = plain(&lines);
        assert!(!text.iter().any(|l| l.contains("recent")));
        assert!(
            !text
                .iter()
                .any(|l| l.contains('?') && l.contains("all keys")),
            "`?` line should be dropped before F8/F4: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("F8")),
            "F8/F4 row should still be present at height 6: {text:?}"
        );
    }

    #[test]
    fn height_degradation_drops_f8f4_third_and_six_core_keys_survive_to_8_rows() {
        let mut a = app();
        a.history = vec!["select 1".into()];
        // Single-column floor: header + blank + six core keys = 8 lines.
        let lines = landing_lines(&a, 40, 8);
        assert_eq!(
            lines.len(),
            8,
            "expected exactly the 8-row floor: {lines:?}"
        );
        let text = plain(&lines);
        assert!(!text.iter().any(|l| l.contains("recent")));
        assert!(!text
            .iter()
            .any(|l| l.contains('?') && l.contains("all keys")));
        assert!(
            !text
                .iter()
                .any(|l| l.contains("F8") || l.contains("JDBC tap")),
            "F8/F4 should be dropped at the 8-row floor: {text:?}"
        );
        for key in [
            "write a query",
            "schema browser",
            "saved queries",
            "slow queries",
            "schema wizard",
            "sessions & locks",
        ] {
            assert!(
                text.iter().any(|l| l.contains(key)),
                "missing {key}: {text:?}"
            );
        }
    }

    #[test]
    fn no_history_omits_recent_section_even_with_room() {
        let a = app();
        let lines = landing_lines(&a, 80, 30);
        let text = plain(&lines);
        assert!(!text.iter().any(|l| l.contains("recent")));
    }

    #[test]
    fn unloaded_schema_cache_omits_table_count() {
        let a = app();
        let lines = landing_lines(&a, 80, 30);
        let text = plain(&lines);
        let s_row = text
            .iter()
            .find(|l| l.contains('S') && l.contains("schema browser"))
            .unwrap();
        assert!(
            !s_row.contains('('),
            "table count parenthetical should be omitted when cache is empty: {s_row:?}"
        );
    }

    #[test]
    fn loaded_schema_cache_shows_table_count() {
        use crate::query::schema::TableMeta;
        let mut a = app();
        a.schema_cache.tables.push(TableMeta {
            schema: "public".into(),
            name: "users".into(),
        });
        let lines = landing_lines(&a, 80, 30);
        let text = plain(&lines);
        assert!(text.iter().any(|l| l.contains("(1 tables)")));
    }

    #[test]
    fn recent_rows_have_no_invented_age_column() {
        let mut a = app();
        a.history = vec!["select 1".into(), "select 2 from foo".into()];
        let lines = landing_lines(&a, 80, 30);
        let text = plain(&lines);
        let row = text
            .iter()
            .find(|l| l.contains("select 2 from foo"))
            .unwrap();
        assert!(
            !row.contains("ago"),
            "history entries carry no timestamp; must not invent an age: {row:?}"
        );
    }

    #[test]
    fn recent_shows_newest_first_capped_at_five() {
        let mut a = app();
        a.history = (1..=7).map(|i| format!("select {i}")).collect();
        let lines = landing_lines(&a, 80, 30);
        let text = plain(&lines);
        // Newest (select 7) should appear before older ones, and only
        // the 5 most recent should show.
        let idx7 = text.iter().position(|l| l.contains("select 7"));
        let idx3 = text.iter().position(|l| l.contains("select 3"));
        assert!(idx7.is_some(), "{text:?}");
        assert!(idx3.is_some(), "{text:?}");
        assert!(idx7 < idx3);
        assert!(!text.iter().any(|l| l.contains("select 2")));
        assert!(!text.iter().any(|l| l.contains("select 1")));
    }
}
