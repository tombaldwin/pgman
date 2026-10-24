//! The "start card" shown in the result panel right after connecting,
//! before the operator has run anything — replaces the bare `(no rows)`
//! placeholder that used to be the first thing every new user saw.
//!
//! [`landing_lines`] is pure (dimensions in, styled `Line`s out) so the
//! layout — column count, height degradation — is unit-testable without
//! a terminal. [`draw_landing`] just wraps it in the bordered block.

use super::*;
use crate::app::DatabaseInfo;

/// Below this inner width, key hints stack in a single column instead
/// of two side by side.
const TWO_COL_MIN_WIDTH: u16 = 64;
/// Most recent history entries shown in the `recent` section.
const RECENT_MAX_ROWS: usize = 5;
/// Gap (in columns) between the two key-hint columns.
const COL_GAP: usize = 3;
/// Every key hint's key glyph is left-justified into a field this wide
/// before the description starts, so single-char keys (`e`) and
/// two-char keys (`F8`) line up on the same description column. A
/// longer key (`ctrl-t`) widens its own field — see `key_field`.
const KEY_FIELD_WIDTH: usize = 4;
/// Leading indent for key-hint / recent-row lines (a "sub-list" look
/// under the header / `recent` label, which get a single-space indent).
const HINT_INDENT: &str = "   ";

const F8_KEY: &str = "ctrl-l";
const F8_DESC_LONG: &str = "logs → SQL (paste a Hibernate / Postgres log in the editor)";
const F8_DESC_SHORT: &str = "logs → SQL (paste a log first)";
/// The JDBC tap's start-card hint: the `:tap` command is the key, as
/// F-keys are not a primary binding on a Mac terminal; `F4` rides along.
const TAP_KEY: &str = ":tap";
const TAP_DESC: &str = "JDBC tap (F4)";

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
/// The start card's line about the footer's tap badges, for `nplus1`
/// repeated-query findings, in the longest wording that fits `width`
/// columns — whole wordings, never one cut mid-phrase, so the `:tap` at
/// the end always survives. Pure.
pub(crate) fn tap_badges_line(nplus1: usize, width: usize) -> String {
    let n = nplus1;
    let found = match n {
        0 => None,
        1 => Some("one query it runs once per row".to_string()),
        n => Some(format!("{n} queries it runs once per row")),
    };
    let wordings: Vec<String> = match &found {
        None => vec![
            "TAP below: your app's queries are arriving over the JDBC tap · :tap to watch".into(),
            "TAP below: your app's queries, over the JDBC tap · :tap".into(),
        ],
        Some(found) => vec![
            format!("TAP below: your app's queries arriving over the JDBC tap · N+1 ×{n}: {found} · :tap to look"),
            format!("TAP below: your app's queries over the JDBC tap · N+1 ×{n}: {found} · :tap"),
            format!("TAP: app queries via the JDBC tap · N+1 ×{n}: repeated per row · :tap"),
        ],
    };
    let last = wordings.last().cloned().unwrap_or_default();
    wordings
        .into_iter()
        .find(|w| display_width(w) <= width)
        .unwrap_or(last)
}

/// Layout, top to bottom: a one-line connection summary; a `databases`
/// line (current database first, then the rest as returned — omitted
/// while `app.databases` is empty, i.e. the bootstrap hasn't answered
/// yet); a blank line; six core key hints (two columns when
/// `inner_width >= 64`, one otherwise); the F8/F4 differentiator row; a
/// `?  all keys` line, sharing its row with `ctrl-t  new tab` in two
/// columns (its own line in one); then, once the JDBC tap has heard
/// from the app, a blank line and what the footer's `TAP` / `N+1`
/// badges mean; then (if there's room and history isn't empty) a blank
/// line, a `recent` label, and up to five of the most recent history
/// entries.
///
/// Under height pressure, sections drop in this order: `recent` first,
/// then the tap line,
/// then the one-column `ctrl-t` line, then `databases`, then the F8/F4
/// row — so the connection line and the six core keys always survive
/// down to 8 rows. The `?` line outlasts the F8/F4 row by taking the
/// blank under the connection line, and once any hint is dropped it
/// says how many (`?  all keys · +3 more`).
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
    let new_tab = Hint::new("ctrl-t", "new tab");

    // F8's description is long; shorten it rather than truncate it if
    // it (plus, in two-column mode, the tap's cell and the gap) wouldn't
    // fit at the current width.
    let f8_desc = pick_f8_desc(two_col, width);
    let f8 = Hint::new(F8_KEY, f8_desc);
    let f4 = Hint::new(TAP_KEY, TAP_DESC);

    // --- height budget: decide what survives -------------------------
    let key_lines_floor = if two_col { 3 } else { 6 }; // core only
                                                       // F8 and F4 share one row in two columns but take one row EACH in
                                                       // one — budgeting a single row for both meant a 60x16 card drew F8
                                                       // with F4 clipped off, and 60x17 clipped the `?` line.
    let f8f4_lines = if two_col { 1 } else { 2 };
    let key_lines_with_f8f4 = key_lines_floor + f8f4_lines;
    // + the `?` line
    let key_lines_with_help = key_lines_with_f8f4 + 1;
    // `ctrl-t` shares the `?` row in two columns; alone it is a row.
    let new_tab_lines = if two_col { 0 } else { 1 };
    let key_lines_with_new_tab = key_lines_with_help + new_tab_lines;
    let recent_len = if app.history.is_empty() {
        0
    } else {
        2 + app.history.len().min(RECENT_MAX_ROWS) // blank + label + rows
    };
    let has_databases = !app.databases.is_empty();
    let db_len = if has_databases { 1 } else { 0 }; // the `databases` line itself
    const HEADER_LINES: usize = 2; // connection line + blank
                                   // What the footer's TAP / N+1 badges mean, once the tap has heard
                                   // from the app: on the first screen they were unexplained.
    let tap_seen = app.tap_health.query_count > 0;
    let tap_len = if tap_seen { 2 } else { 0 }; // blank + the line

    let h = inner_height as usize;
    let (include_recent, include_new_tab, include_databases, include_help, include_f8f4) =
        if HEADER_LINES + db_len + key_lines_with_new_tab + recent_len <= h {
            (true, true, true, true, true)
        } else if HEADER_LINES + db_len + key_lines_with_new_tab <= h {
            (false, true, true, true, true)
        } else if HEADER_LINES + db_len + key_lines_with_help <= h {
            (false, false, true, true, true)
        } else if HEADER_LINES + key_lines_with_help <= h {
            (false, false, false, true, true)
        } else if HEADER_LINES + key_lines_with_f8f4 <= h {
            (false, false, false, false, true)
        } else {
            (false, false, false, false, false)
        };
    // Rows dropped with nothing on the card saying so read as all
    // there is. The `?` line is the way to the rest, so it outlasts the
    // blank under the connection line, and counts what is not shown.
    let key_rows = if include_f8f4 {
        key_lines_with_f8f4
    } else {
        key_lines_floor
    };
    // The connection line, the keys, the `?` line.
    let keep_help_for_blank = !include_help && key_rows + 2 <= h;
    let include_help = include_help || keep_help_for_blank;
    let include_blank = !keep_help_for_blank;
    let hidden_hints = hidden_hint_count(include_f8f4, include_new_tab);
    let help = if hidden_hints > 0 {
        Hint::new("?", format!("all keys · +{hidden_hints} more"))
    } else {
        help
    };
    let include_recent = include_recent && !app.history.is_empty();
    let include_databases = include_databases && has_databases;
    // Dropped before `recent` goes: it rides on the room left over.
    let used = HEADER_LINES + db_len + key_lines_with_new_tab;
    let include_tap = tap_seen && include_new_tab && used + tap_len <= h;
    let include_recent = include_recent && (!include_tap || used + tap_len + recent_len <= h);

    // --- assemble ------------------------------------------------------
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(header_line(app, muted_style, width));
    if include_databases {
        if let Some(text) = databases_line(app, width) {
            lines.push(Line::from(Span::styled(text, muted_style)));
        }
    }
    if include_blank {
        lines.push(Line::from(""));
    }

    // The card is still up while the FIRST query is in flight (the
    // grid has no columns yet), and `e write a query` is not what to
    // do next when a query is already running — nor is any other key
    // hint on the card. Say what's happening instead.
    if app.query_running {
        lines.push(Line::from(vec![
            Span::raw(HINT_INDENT),
            Span::styled("running …", muted_style),
        ]));
        return lines;
    }

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
        let left_col_width = display_width(HINT_INDENT)
            + KEY_FIELD_WIDTH
            + left_descs
                .iter()
                .map(|d| display_width(d))
                .max()
                .unwrap_or(0);

        lines.push(paired_line(&e, &s, key_style, desc_style, left_col_width));
        lines.push(paired_line(&q, &t, key_style, desc_style, left_col_width));
        lines.push(paired_line(&w, &l, key_style, desc_style, left_col_width));
        if include_f8f4 {
            lines.push(paired_line(&f8, &f4, key_style, desc_style, left_col_width));
        }
        if include_help && include_new_tab {
            lines.push(paired_line(
                &help,
                &new_tab,
                key_style,
                desc_style,
                left_col_width,
            ));
        } else if include_help {
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
        if include_new_tab {
            lines.push(single_line(&new_tab, key_style, desc_style));
        }
    }

    if include_tap {
        let room = width.saturating_sub(display_width(HINT_INDENT));
        let text = tap_badges_line(app.current_nplus1().len(), room);
        let fitted = crate::grid::truncate_cell(&text, room);
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw(HINT_INDENT),
            Span::styled(fitted, muted_style),
        ]));
    }

    if include_recent {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(recent_label(width), muted_style)));
        for entry in app.history.iter().rev().take(RECENT_MAX_ROWS) {
            let one_line = entry.split_whitespace().collect::<Vec<_>>().join(" ");
            let truncated = crate::grid::truncate_cell(
                &one_line,
                width.saturating_sub(display_width(HINT_INDENT)),
            );
            lines.push(Line::from(vec![
                Span::raw(HINT_INDENT),
                Span::styled(truncated, desc_style),
            ]));
        }
    }

    lines
}

/// How many key hints the start card leaves off: the F8 and tap pair,
/// and `ctrl-t`. What the `?` line counts as `+N more`. Pure.
fn hidden_hint_count(include_f8f4: bool, include_new_tab: bool) -> usize {
    let mut n = 0;
    if !include_f8f4 {
        n += 2;
    }
    if !include_new_tab {
        n += 1;
    }
    n
}

/// The `recent` section's label, with the keys that bring an entry back
/// when they fit: the list itself isn't selectable, and nothing on the
/// card said how to reuse one. Pure.
fn recent_label(width: usize) -> &'static str {
    const WITH_KEYS: &str = " recent · in the editor, ctrl-p recalls · ctrl-r searches";
    if display_width(WITH_KEYS) <= width {
        WITH_KEYS
    } else {
        " recent"
    }
}

/// Choose the F8 hint description: the long form if it (plus, in
/// two-column mode, the tap cell and the gap between them) fits within
/// `width`; the short form otherwise. Never truncates either form.
/// Measures the key fields as [`key_field`] pads them, so a key wider
/// than [`KEY_FIELD_WIDTH`] (`ctrl-l`, `:tap`) is counted at its width.
fn pick_f8_desc(two_col: bool, width: usize) -> &'static str {
    let f8_cell = display_width(&key_field(F8_KEY)) + display_width(F8_DESC_LONG);
    let needed = if two_col {
        display_width(HINT_INDENT)
            + f8_cell
            + COL_GAP
            + display_width(&key_field(TAP_KEY))
            + display_width(TAP_DESC)
    } else {
        display_width(HINT_INDENT) + f8_cell
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
/// out of sync. A long host is cut the way the header cuts it, so the
/// version and the badge stay on the card.
fn header_line(app: &App, muted_style: Style, width: usize) -> Line<'static> {
    let db = app.dsn.as_ref().map(|d| d.dbname.as_str()).unwrap_or("?");
    let host_port = app
        .dsn
        .as_ref()
        .map(|d| format!("{}:{}", d.host, d.port))
        .unwrap_or_else(|| "?".to_string());
    let version = match &app.conn_state {
        ConnState::Connected { server_version } => super::short_server_version(server_version),
        _ => "?",
    };
    let head = format!(" connected to {db} on ");
    let tail = format!(" · pg {version} · ");
    // `RO` / `RW`: two columns.
    let budget = width.saturating_sub(display_width(&head) + display_width(&tail) + 2);
    let host_port = super::header::fit_server(&host_port, budget.max(8));
    let mut spans = vec![Span::styled(
        format!("{head}{host_port}{tail}"),
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

/// The `databases` line: `databases  main 1.2 GB · analytics 300 MB ·
/// staging 40 MB` — the current database (matched against the DSN's
/// `dbname`) first, the rest in the order `app.databases` came back
/// in. `None` when `app.databases` is empty (the bootstrap query
/// hasn't answered yet) — callers must not push a blank/empty line for
/// that case.
fn databases_line(app: &App, width: usize) -> Option<String> {
    let current_db = app.dsn.as_ref().map(|d| d.dbname.as_str()).unwrap_or("");
    format_databases_line(&app.databases, current_db, width)
}

/// Pure formatter behind [`databases_line`] — no `App` dependency, so
/// ordering and width-fitting are directly unit-testable. Keeps whole
/// entries when they don't all fit in `width`, ending with `· +N more`
/// rather than truncating mid-entry.
pub(crate) fn format_databases_line(
    databases: &[DatabaseInfo],
    current_db: &str,
    width: usize,
) -> Option<String> {
    if databases.is_empty() {
        return None;
    }
    const PREFIX: &str = " databases  ";
    let ordered = ordered_databases(databases, current_db);
    let entries: Vec<String> = ordered
        .iter()
        .map(|d| format!("{} {}", d.name, d.size))
        .collect();

    let mut line = PREFIX.to_string();
    let mut shown = 0;
    for (i, entry) in entries.iter().enumerate() {
        let candidate = if shown == 0 {
            format!("{line}{entry}")
        } else {
            format!("{line} · {entry}")
        };
        let not_shown_after = entries.len() - i - 1;
        let suffix_len = if not_shown_after > 0 {
            display_width(&format!(" · +{not_shown_after} more"))
        } else {
            0
        };
        if display_width(&candidate) + suffix_len <= width {
            line = candidate;
            shown = i + 1;
        } else {
            break;
        }
    }
    if shown == 0 {
        // Not even one entry fits: `" databases   · +3 more"` is both
        // wider than the budget it was given and empty of information.
        // No line at all is the honest answer.
        return None;
    }
    let not_shown = entries.len() - shown;
    if not_shown > 0 {
        line.push_str(&format!(" · +{not_shown} more"));
    }
    Some(line)
}

/// `current_db` first (matched by name; only the first match is pulled
/// forward — a duplicate name, if the server ever returned one, stays
/// in its original spot in `rest`), the rest in their original
/// relative order.
fn ordered_databases<'a>(databases: &'a [DatabaseInfo], current_db: &str) -> Vec<&'a DatabaseInfo> {
    let mut current = None;
    let mut rest = Vec::with_capacity(databases.len());
    for d in databases {
        if current.is_none() && d.name == current_db {
            current = Some(d);
        } else {
            rest.push(d);
        }
    }
    match current {
        Some(c) => std::iter::once(c).chain(rest).collect(),
        None => databases.iter().collect(),
    }
}

/// The key glyph padded to its field: [`KEY_FIELD_WIDTH`], or wider
/// when the key itself needs it (`ctrl-t` plus a two-column gap), so a
/// long key never runs straight into its description.
fn key_field(key: &str) -> String {
    let width = KEY_FIELD_WIDTH.max(display_width(key) + 2);
    format!("{key:<width$}")
}

/// Render one key hint on its own line (single-column layout).
fn single_line(hint: &Hint, key_style: Style, desc_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::raw(HINT_INDENT),
        Span::styled(key_field(hint.key), key_style),
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
    let left_key = key_field(left.key);
    let left_text_width =
        display_width(HINT_INDENT) + display_width(&left_key) + display_width(&left.desc);
    let pad = left_col_width.saturating_sub(left_text_width);
    Line::from(vec![
        Span::raw(HINT_INDENT),
        Span::styled(left_key, key_style),
        Span::styled(left.desc.clone(), desc_style),
        Span::raw(" ".repeat(pad + COL_GAP)),
        Span::styled(key_field(right.key), key_style),
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
    fn header_line_shows_the_short_server_version() {
        // The packager's build detail (`(Debian 16.15-1.pgdg13+2)`)
        // belongs in the About overlay, not restated on the start card.
        let mut a = app();
        a.conn_state = ConnState::Connected {
            server_version: "16.15 (Debian 16.15-1.pgdg13+2)".into(),
        };
        let lines = landing_lines(&a, 80, 30);
        let first = &plain(&lines)[0];
        assert!(first.contains("pg 16.15"), "{first}");
        assert!(!first.contains("Debian"), "{first}");
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
            text.iter()
                .any(|l| l.contains("ctrl-l") && l.contains("logs")),
            "expected an F8 row at 80 columns: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains(":tap  JDBC tap (F4)")),
            "expected a `:tap` row, F4 beside it, at 80 columns: {text:?}"
        );
    }

    #[test]
    fn the_tap_is_never_hinted_by_f4_alone() {
        // F4 is not a primary binding on a Mac terminal: every start
        // card line that names F4 names `:tap` too, at every size and
        // with or without the tap's badge line.
        let mut a = app();
        a.tap_health.query_count = 1;
        for (w, h) in [(200u16, 40u16), (120, 30), (78, 20), (58, 14), (40, 30)] {
            for line in plain(&landing_lines(&a, w, h)) {
                if line.contains("F4") || line.contains("JDBC tap") {
                    assert!(line.contains(":tap"), "{w}x{h}: {line:?}");
                }
            }
        }
    }

    #[test]
    fn f8_desc_shortens_when_it_would_not_fit() {
        // Long form needs far more than 40 columns.
        assert_eq!(pick_f8_desc(false, 40), F8_DESC_SHORT);
        assert_eq!(pick_f8_desc(false, 200), F8_DESC_LONG);
    }

    #[test]
    fn recent_label_names_the_recall_keys_when_they_fit() {
        assert_eq!(
            recent_label(80),
            " recent · in the editor, ctrl-p recalls · ctrl-r searches"
        );
        assert_eq!(recent_label(40), " recent");
        let mut a = app();
        a.history = vec!["select 1".into()];
        let text = plain(&landing_lines(&a, 80, 30));
        assert!(
            text.iter().any(|l| l.contains("ctrl-p recalls")),
            "{text:?}"
        );
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
        assert!(text.iter().any(|l| l.contains("ctrl-l")));
    }

    #[test]
    fn height_degradation_trades_the_blank_for_the_help_line() {
        let mut a = app();
        a.history = vec!["select 1".into()];
        // Two-col floor is 5 (header+blank+3 core rows), F8/F4 adds 1 (6),
        // `?` adds 1 more (7). At height 6 the blank under the header
        // goes, so F8/F4 and `?` both stay, and `?` counts the hidden
        // `ctrl-t`.
        let lines = landing_lines(&a, 80, 6);
        assert_eq!(lines.len(), 6, "{lines:?}");
        let text = plain(&lines);
        assert!(!text.iter().any(|l| l.contains("recent")));
        assert!(!text[1].trim().is_empty(), "no blank row: {text:?}");
        assert!(
            text.iter().any(|l| l.contains("?   all keys · +1 more")),
            "`?` says a hint is hidden: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("ctrl-l")),
            "F8/F4 row should still be present at height 6: {text:?}"
        );
        // With room for everything, no count and the blank stays.
        let text = plain(&landing_lines(&a, 80, 30));
        assert!(text[1].trim().is_empty(), "{text:?}");
        assert!(
            text.iter().any(|l| l.contains("?   all keys   ")),
            "{text:?}"
        );
        assert!(!text.iter().any(|l| l.contains("more")), "{text:?}");
    }

    #[test]
    fn height_degradation_drops_f8f4_third_and_six_core_keys_survive_to_8_rows() {
        let mut a = app();
        a.history = vec!["select 1".into()];
        // Single-column floor: header + six core keys + `?`, the blank
        // given up for the `?` line = 8 lines. (The 60x16 card: it
        // dropped F8, the tap and `ctrl-t` with nothing saying so.)
        let lines = landing_lines(&a, 40, 8);
        assert_eq!(
            lines.len(),
            8,
            "expected exactly the 8-row floor: {lines:?}"
        );
        let text = plain(&lines);
        assert!(!text.iter().any(|l| l.contains("recent")));
        assert_eq!(text[7], "   ?   all keys · +3 more", "{text:?}");
        assert!(
            !text
                .iter()
                .any(|l| l.contains("ctrl-l") || l.contains("JDBC tap")),
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
    fn one_column_budget_counts_f8_and_f4_as_two_rows() {
        // 60 columns is one-column layout, where F8 and F4 take a row
        // each. The budget used to book one row for the pair, so a
        // 60x16 card drew F8 with F4 clipped off the bottom, and 60x17
        // clipped the `?` line the same way.
        let a = app();
        // Inner height of a 60x16 terminal's start card: header(2) +
        // six core keys(6) + F8/F4(2) = 10, `?` needs an 11th.
        for h in [10u16, 11, 12] {
            let lines = landing_lines(&a, 58, h);
            let text = plain(&lines);
            let has_f8 = text.iter().any(|l| l.contains("ctrl-l"));
            let has_f4 = text.iter().any(|l| l.contains(":tap"));
            assert_eq!(
                has_f8, has_f4,
                "F8 shown without F4 at height {h}: {text:?}"
            );
            assert!(
                lines.len() <= h as usize,
                "card is {} rows at height {h}: {text:?}",
                lines.len()
            );
        }
        // One row short of the pair, both go.
        let text = plain(&landing_lines(&a, 58, 9));
        assert!(!text.iter().any(|l| l.contains("ctrl-l")), "{text:?}");
        assert!(!text.iter().any(|l| l.contains(":tap")), "{text:?}");
    }

    #[test]
    fn ctrl_t_shares_the_help_row_in_two_columns_and_has_its_own_line_in_one() {
        let a = app();
        let wide = plain(&landing_lines(&a, 80, 30));
        let help_row = wide
            .iter()
            .find(|l| l.contains("all keys"))
            .expect("`?` row");
        assert!(
            help_row.contains("ctrl-t  new tab"),
            "two columns: ctrl-t shares the `?` row, with a gap after the key: {help_row:?}"
        );
        assert_eq!(
            wide.iter().filter(|l| l.contains("new tab")).count(),
            1,
            "{wide:?}"
        );

        let narrow = plain(&landing_lines(&a, 40, 30));
        let help_at = narrow.iter().position(|l| l.contains("all keys")).unwrap();
        let tab_at = narrow
            .iter()
            .position(|l| l.contains("ctrl-t  new tab"))
            .expect("one column: ctrl-t is its own line");
        assert_eq!(tab_at, help_at + 1, "directly under `?`: {narrow:?}");
        assert!(
            !narrow[help_at].contains("new tab"),
            "one column: `?` keeps its line to itself: {narrow:?}"
        );
    }

    #[test]
    fn one_column_ctrl_t_line_drops_first_before_databases_and_help() {
        let mut a = app();
        a.history = vec!["select 1".into()];
        a.databases = vec![DatabaseInfo {
            name: "test".into(),
            size: "1.2 GB".into(),
        }];
        // One column: header(2) + databases(1) + six core(6) + F8/F4(2)
        // + `?`(1) + ctrl-t(1) = 13 rows.
        let full = plain(&landing_lines(&a, 40, 13));
        assert!(full.iter().any(|l| l.contains("new tab")), "{full:?}");
        assert!(full.iter().any(|l| l.contains("databases")), "{full:?}");
        assert!(!full.iter().any(|l| l.contains("recent")), "{full:?}");
        // One row short: ctrl-t goes, databases and `?` stay.
        let short = plain(&landing_lines(&a, 40, 12));
        assert!(
            !short.iter().any(|l| l.contains("new tab")),
            "ctrl-t is the first key line to go: {short:?}"
        );
        assert!(short.iter().any(|l| l.contains("databases")), "{short:?}");
        assert!(short.iter().any(|l| l.contains("all keys")), "{short:?}");
        assert!(short.len() <= 12, "{short:?}");
        // Another row short: now databases goes, `?` still stays.
        let shorter = plain(&landing_lines(&a, 40, 11));
        assert!(
            !shorter.iter().any(|l| l.contains("databases")),
            "{shorter:?}"
        );
        assert!(
            shorter.iter().any(|l| l.contains("all keys")),
            "{shorter:?}"
        );
        assert!(shorter.len() <= 11, "{shorter:?}");
    }

    #[test]
    fn key_field_widens_for_a_long_key_and_keeps_short_keys_aligned() {
        assert_eq!(key_field("e"), "e   ");
        assert_eq!(key_field("F8"), "F8  ");
        assert_eq!(key_field("ctrl-t"), "ctrl-t  ");
    }

    #[test]
    fn format_databases_line_returns_none_when_nothing_fits() {
        // A budget too small for even the first entry used to yield
        // `" databases   · +3 more"` — wider than the budget it was
        // given, and with no database named in it.
        let dbs = vec![
            DatabaseInfo {
                name: "main".into(),
                size: "1.2 GB".into(),
            },
            DatabaseInfo {
                name: "analytics".into(),
                size: "300 MB".into(),
            },
        ];
        assert_eq!(format_databases_line(&dbs, "main", 20), None);
        // And whenever it does return a line, that line fits.
        for width in 0..60 {
            if let Some(line) = format_databases_line(&dbs, "main", width) {
                assert!(
                    line.chars().count() <= width,
                    "width {width}: {line:?} is {} chars",
                    line.chars().count()
                );
            }
        }
    }

    #[test]
    fn running_query_replaces_the_key_hints() {
        // The card is still up while the first query is in flight;
        // `e write a query` is not what to do next then.
        let mut a = app();
        a.query_running = true;
        let text = plain(&landing_lines(&a, 80, 20));
        assert!(
            text.iter().any(|l| l.contains("running …")),
            "expected a running line: {text:?}"
        );
        assert!(
            !text.iter().any(|l| l.contains("write a query")),
            "key hints should be replaced while a query runs: {text:?}"
        );
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

    // -- `databases` line -------------------------------------------

    #[test]
    fn format_databases_line_returns_none_for_empty() {
        assert_eq!(format_databases_line(&[], "test", 80), None);
    }

    #[test]
    fn format_databases_line_orders_current_db_first_then_original_order() {
        let dbs = vec![
            DatabaseInfo {
                name: "analytics".into(),
                size: "300 MB".into(),
            },
            DatabaseInfo {
                name: "main".into(),
                size: "1.2 GB".into(),
            },
            DatabaseInfo {
                name: "staging".into(),
                size: "40 MB".into(),
            },
        ];
        let got = format_databases_line(&dbs, "main", 200).unwrap();
        assert_eq!(
            got,
            " databases  main 1.2 GB · analytics 300 MB · staging 40 MB"
        );
    }

    #[test]
    fn format_databases_line_falls_back_to_returned_order_when_current_db_absent() {
        let dbs = vec![
            DatabaseInfo {
                name: "analytics".into(),
                size: "300 MB".into(),
            },
            DatabaseInfo {
                name: "staging".into(),
                size: "40 MB".into(),
            },
        ];
        let got = format_databases_line(&dbs, "nope", 200).unwrap();
        assert_eq!(got, " databases  analytics 300 MB · staging 40 MB");
    }

    #[test]
    fn format_databases_line_keeps_whole_entries_and_ends_with_plus_n_more() {
        let dbs = vec![
            DatabaseInfo {
                name: "main".into(),
                size: "1.2 GB".into(),
            },
            DatabaseInfo {
                name: "analytics".into(),
                size: "300 MB".into(),
            },
            DatabaseInfo {
                name: "staging".into(),
                size: "40 MB".into(),
            },
        ];
        // Full line is 58 chars; width 40 fits the label + first entry
        // + a "+2 more" suffix, but not the second entry as well.
        let got = format_databases_line(&dbs, "main", 40).unwrap();
        assert_eq!(got, " databases  main 1.2 GB · +2 more");
        assert!(got.chars().count() <= 40);
        // No entry is cut mid-way — dropped entries don't appear at all.
        assert!(!got.contains("analytics"));
        assert!(!got.contains("staging"));
    }

    /// A `lc_collate=ja_JP` shop names its databases in kana. Every
    /// glyph there is two terminal columns wide, so a line that counts
    /// `char`s measures half of what it paints — at width 44 the
    /// databases line came out 55 columns and ran through the card's
    /// right border.
    #[test]
    fn format_databases_line_measures_columns_not_chars() {
        let dbs = vec![
            DatabaseInfo {
                name: "受注管理".into(),
                size: "1.2 GB".into(),
            },
            DatabaseInfo {
                name: "分析基盤データ".into(),
                size: "300 MB".into(),
            },
        ];
        for width in 0..70 {
            if let Some(line) = format_databases_line(&dbs, "受注管理", width) {
                assert!(
                    display_width(&line) <= width,
                    "width {width}: {line:?} paints {} columns",
                    display_width(&line)
                );
            }
        }
    }

    #[test]
    fn connection_line_cuts_a_long_host_and_keeps_the_version_and_badge() {
        let mut a = app();
        a.dsn = Some(
            Dsn::parse("postgres://u@ledger-prod.c9abc1def2gh.eu-west-2.rds.amazonaws.com/test")
                .unwrap(),
        );
        a.read_only = true;
        for width in [60, 80, 100] {
            let line = plain(&landing_lines(&a, width, 30))[0].clone();
            assert!(
                display_width(&line) <= width as usize,
                "width {width}: {line:?}"
            );
            assert!(
                line.starts_with(" connected to test on ledger-prod")
                    && line.ends_with(" · pg 16.0 · RO"),
                "width {width}: {line:?}"
            );
        }
        // Room to spare: the whole endpoint.
        let line = plain(&landing_lines(&a, 120, 30))[0].clone();
        assert!(line.contains("rds.amazonaws.com:5432"), "{line:?}");
    }

    #[test]
    fn databases_line_omitted_when_empty() {
        let a = app(); // app.databases is empty by default (bootstrap hasn't answered)
        let lines = landing_lines(&a, 80, 30);
        let text = plain(&lines);
        assert!(!text.iter().any(|l| l.contains("databases")));
    }

    #[test]
    fn databases_line_appears_directly_under_connection_line() {
        let mut a = app();
        a.databases = vec![
            DatabaseInfo {
                name: "analytics".into(),
                size: "300 MB".into(),
            },
            DatabaseInfo {
                name: "test".into(), // matches app()'s dsn dbname "test"
                size: "1.2 GB".into(),
            },
        ];
        let lines = landing_lines(&a, 80, 30);
        let text = plain(&lines);
        assert!(
            text[0].contains("connected to"),
            "line 0 should be the connection line: {:?}",
            text[0]
        );
        assert_eq!(
            text[1], " databases  test 1.2 GB · analytics 300 MB",
            "line 1 should be the databases line, current db first"
        );
        assert_eq!(
            text[2], "",
            "a blank line should follow the databases line: {:?}",
            text[2]
        );
    }

    #[test]
    fn databases_line_survives_recent_drop_but_drops_before_help() {
        let mut a = app();
        a.history = vec!["select 1".into()];
        a.databases = vec![DatabaseInfo {
            name: "test".into(),
            size: "1.2 GB".into(),
        }];

        // height 8: header(2) + databases(1) + keys-with-help(5) = 8 —
        // fits; recent would need 3 more and doesn't.
        let lines8 = landing_lines(&a, 80, 8);
        let text8 = plain(&lines8);
        assert!(
            !text8.iter().any(|l| l.contains("recent")),
            "recent should already be dropped at height 8: {text8:?}"
        );
        assert!(
            text8.iter().any(|l| l.contains("databases")),
            "databases should still show at height 8: {text8:?}"
        );
        assert!(text8
            .iter()
            .any(|l| l.contains('?') && l.contains("all keys")));

        // height 7: databases must drop too now — but `?` / F8-F4
        // survive, matching the documented degradation order (recent,
        // then databases, then `?`, then F8/F4).
        let lines7 = landing_lines(&a, 80, 7);
        let text7 = plain(&lines7);
        assert!(!text7.iter().any(|l| l.contains("recent")));
        assert!(
            !text7.iter().any(|l| l.contains("databases")),
            "databases should drop before help at height 7: {text7:?}"
        );
        assert!(
            text7
                .iter()
                .any(|l| l.contains('?') && l.contains("all keys")),
            "`?` line should survive databases being dropped: {text7:?}"
        );
        assert!(text7.iter().any(|l| l.contains("ctrl-l")));
    }

    #[test]
    fn the_tap_badges_are_explained_once_the_tap_has_heard_from_the_app() {
        assert_eq!(
            tap_badges_line(0, 200),
            "TAP below: your app's queries are arriving over the JDBC tap · :tap to watch"
        );
        assert!(tap_badges_line(1, 200).contains("N+1 ×1: one query it runs once per row"));
        assert!(tap_badges_line(3, 200).contains("N+1 ×3: 3 queries it runs once per row"));
        // Narrower: a shorter whole wording, still ending on `:tap`, never
        // one cut mid-phrase.
        for width in [75, 95, 115] {
            let line = tap_badges_line(1, width);
            assert!(display_width(&line) <= width, "{width}: {line}");
            assert!(
                line.contains(" · :tap") && line.contains("N+1 ×1"),
                "{width}: {line}"
            );
        }
        // The room a 100- and a 120-column card gives the line keeps
        // the wording that says what N+1 means.
        for width in [95, 115] {
            assert!(
                tap_badges_line(1, width).contains("one query it runs once per row"),
                "{width}: {}",
                tap_badges_line(1, width)
            );
        }
        assert!(display_width(&tap_badges_line(0, 60)) <= 60);

        let mut a = app();
        let quiet = plain(&landing_lines(&a, 120, 30));
        assert!(!quiet.iter().any(|l| l.contains("TAP below")), "{quiet:?}");
        a.tap_health.query_count = 1;
        a.history = vec!["select 1;".into()];
        let heard = plain(&landing_lines(&a, 120, 30));
        let tap = heard
            .iter()
            .position(|l| l.contains("TAP below"))
            .expect("tap line");
        let recent = heard
            .iter()
            .position(|l| l.contains("recent"))
            .expect("recent");
        assert!(tap < recent, "{heard:?}");
        // Short of room, it goes with `recent`, before the keys do.
        let short = plain(&landing_lines(&a, 120, 8));
        assert!(!short.iter().any(|l| l.contains("TAP below")), "{short:?}");
        assert!(short.iter().any(|l| l.contains("all keys")), "{short:?}");
        // Room for it but not `recent` as well: the tap line stays.
        let mid = plain(&landing_lines(&a, 120, 9));
        assert!(mid.iter().any(|l| l.contains("TAP below")), "{mid:?}");
        assert!(!mid.iter().any(|l| l.contains("recent")), "{mid:?}");
        // Room for `recent` alone (it would show without a tap), not
        // for both: the card still fits its height.
        let both = plain(&landing_lines(&a, 120, 10));
        assert!(both.len() <= 10, "{both:?}");
        assert!(both.iter().any(|l| l.contains("TAP below")), "{both:?}");
    }
}
