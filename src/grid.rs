//! A rectangular result set for display, plus pure layout helpers.
//!
//! Query results from any source land here as strings; the TUI renders this.
//! Column-width and truncation logic is pure and tested.

/// Cap on rows pulled into a single grid — keeps a runaway `SELECT` from
/// exhausting memory.
pub const MAX_ROWS: usize = 1000;

/// One result cell: the server's text rendering of the value, `None`
/// for SQL NULL. An empty string is `Some("")` — a different value, and
/// every reader has to decide what a NULL means for it
/// (docs/backlog/null-cells.md).
pub type Cell = Option<String>;

/// What the grid, row detail and cell detail draw for SQL NULL — dimmed
/// and italic in `theme.null_cell`, so it never reads as the text
/// `NULL`. An empty string draws as nothing.
pub const NULL_MARKER: &str = "NULL";

/// A cell's text with NULL as `""` — for readers to which the two mean
/// the same (a catalog parser, a size column). Anything that shows,
/// writes or compares a value must look at the `Option` instead.
pub fn cell_text(cell: &Cell) -> &str {
    cell.as_deref().unwrap_or("")
}

/// A result set: column headers plus text-rendered rows.
///
/// `truncated` is set when the underlying source had more rows than
/// `MAX_ROWS` — the renderer surfaces this so the user knows the view
/// is partial.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grid {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    pub truncated: bool,
}

impl Grid {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// Display width for each column: the widest of the header and its cells,
/// clamped to `[1, max_width]`. Width is counted in **display columns**
/// (`UnicodeWidthStr`), the same unit `truncate_cell_parts` and ratatui
/// lay the table out in — a `char` count gave a CJK column half its
/// width, so a `名前` header rendered as `名`.
pub fn column_widths(grid: &Grid, max_width: usize) -> Vec<usize> {
    use unicode_width::UnicodeWidthStr;
    let max_width = max_width.max(1);
    grid.columns
        .iter()
        .enumerate()
        .map(|(col, header)| {
            let cell_max = grid
                .rows
                .iter()
                .filter_map(|r| r.get(col))
                .map(|c| match c {
                    Some(text) => UnicodeWidthStr::width(show_controls(text, true).as_ref()),
                    None => NULL_MARKER.len(),
                })
                .max()
                .unwrap_or(0);
            UnicodeWidthStr::width(header.as_str())
                .max(cell_max)
                .clamp(1, max_width)
        })
        .collect()
}

/// Compare two rendered cells for sorting. Numeric-aware: when both
/// parse as `f64`, compare as numbers (so `2` sorts before `10`).
/// Empty strings (the renderer's representation of SQL NULL) sort
/// AFTER non-empty values, matching Postgres's default `NULLS LAST`
/// for `ORDER BY … ASC`.
///
/// ```
/// use std::cmp::Ordering;
/// use pgman::grid::cmp_cells;
/// assert_eq!(cmp_cells("2", "10"), Ordering::Less);          // numeric
/// assert_eq!(cmp_cells("alice", "bob"), Ordering::Less);     // lex
/// assert_eq!(cmp_cells("", "anything"), Ordering::Greater);  // NULL last
/// ```
pub fn cmp_cells(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.is_empty(), b.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => match (a.parse::<f64>(), b.parse::<f64>()) {
            (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or_else(|| a.cmp(b)),
            _ => a.cmp(b),
        },
    }
}

/// Truncate `s` to `width` display columns, marking a cut with `…`.
pub fn truncate_cell(s: &str, width: usize) -> String {
    let (kept, suffix) = truncate_cell_parts(s, width);
    let mut out = kept;
    out.push_str(suffix);
    out
}

/// Like `truncate_cell` but returns the kept text and the suffix
/// marker (`…` or empty) separately so a renderer can style the
/// marker distinctly from the value. The kept-text part has no
/// trailing marker; concatenating the two recreates `truncate_cell`'s
/// output. Pure / testable.
///
/// `width` is **display columns**, not chars. A CJK cell paints two
/// columns per char, so a char-counting truncation hands back a string
/// twice as wide as the budget it was given — and every caller here
/// (grid columns, picker rows, the start card's `recent` list) uses the
/// result to decide where the *next* thing starts.
pub fn truncate_cell_parts(s: &str, width: usize) -> (String, &'static str) {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if width == 0 {
        return (String::new(), "");
    }
    if UnicodeWidthStr::width(s) <= width {
        return (s.to_string(), "");
    }
    // One column goes to the `…` marker. A wide glyph that would land
    // half in and half out of the budget is dropped whole: one column
    // short beats one column over.
    let budget = width - 1;
    let mut kept = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw > budget {
            break;
        }
        used += cw;
        kept.push(c);
    }
    (kept, "…")
}

/// `s` with its control characters made visible for display: a tab as
/// `⇥`, a newline as `⏎` when the value must stay on one line (a grid
/// cell) or kept as a line break otherwise (row / cell detail, which
/// wrap on it), `\r\n` as a single newline, and any other control
/// character as its Unicode control picture (`␍`, `␀`, `␡` …).
/// Terminals drew a tab as nothing and a grid cell as its first line
/// only, so `tab<TAB>here<LF>line two` read `tabhere`. Display only:
/// yanks and exports keep the value as it is. Pure / testable.
pub fn show_controls(s: &str, single_line: bool) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(char::is_control) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // CRLF: the `\n` stands for both.
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' if single_line => out.push('⏎'),
            '\n' => out.push('\n'),
            '\t' => out.push('⇥'),
            '\u{7f}' => out.push('␡'),
            c if (c as u32) < 0x20 => {
                out.push(char::from_u32(0x2400 + c as u32).unwrap_or('�'));
            }
            c if c.is_control() => out.push('�'),
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Is every non-NULL cell of column `col` a number as Postgres prints
/// one (`42`, `-99999999.99`, `1e+30`, `NaN`)? A column of only NULLs
/// is not, and neither is one holding an empty string. Numeric columns are never cut by [`fit_column_widths`]: a
/// truncated number is a wrong number. Pure / testable.
pub fn is_numeric_column(grid: &Grid, col: usize) -> bool {
    let mut any = false;
    for cell in grid.rows.iter().filter_map(|r| r.get(col)) {
        let Some(cell) = cell else {
            continue;
        };
        if !looks_numeric(cell) {
            return false;
        }
        any = true;
    }
    any
}

/// `s` as Postgres's text output prints an integer, numeric or float:
/// an optional sign, digits with at most one `.`, an optional exponent
/// — or `NaN` / `Infinity`. Stricter than `str::parse::<f64>`, which
/// also takes `inf` and `infinity` spelled any way. Pure / testable.
pub fn looks_numeric(s: &str) -> bool {
    let body = s.strip_prefix(['-', '+']).unwrap_or(s);
    if matches!(body, "NaN" | "Infinity") {
        return true;
    }
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let digits_and_dot = !mantissa.is_empty()
        && mantissa.bytes().any(|b| b.is_ascii_digit())
        && mantissa.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && mantissa.bytes().filter(|&b| b == b'.').count() <= 1;
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit())
    });
    digits_and_dot && exponent_ok
}

/// Narrowest a text column is squeezed to (or its natural width, when
/// that is less) before columns start falling off the right edge.
pub const TEXT_MIN_WIDTH: usize = 8;

/// Fit columns of `natural` widths, `spacing` columns apart, into
/// `available` columns. Everything fits → the natural widths. Too
/// wide → the text columns give way, widest first (one shared cap,
/// the most that fits, never under [`TEXT_MIN_WIDTH`]); `numeric`
/// columns keep their full width. Ratatui squeezed every column of an
/// over-wide table itself — cutting numbers (`-9999999` for
/// `-99999999.99`) and leaving no `…` on what it cut. When even the
/// squeezed widths do not fit, the result is wider than `available`
/// and [`visible_column_range`] picks which columns show. Pure /
/// testable.
pub fn fit_column_widths(
    natural: &[usize],
    numeric: &[bool],
    available: usize,
    spacing: usize,
) -> Vec<usize> {
    let gaps = spacing * natural.len().saturating_sub(1);
    let total = |ws: &[usize]| ws.iter().sum::<usize>() + gaps;
    if total(natural) <= available {
        return natural.to_vec();
    }
    let is_numeric = |i: usize| numeric.get(i).copied().unwrap_or(false);
    let capped = |cap: usize| -> Vec<usize> {
        natural
            .iter()
            .enumerate()
            .map(|(i, &n)| {
                if is_numeric(i) {
                    n
                } else {
                    n.min(cap.max(TEXT_MIN_WIDTH))
                }
            })
            .collect()
    };
    // The largest shared cap on text columns that still fits.
    let widest = natural.iter().copied().max().unwrap_or(0);
    let (mut lo, mut hi) = (TEXT_MIN_WIDTH, widest);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if total(&capped(mid)) <= available {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut widths = capped(lo);
    // Hand what the cap's rounding left over to the cut text columns,
    // one column each, left to right.
    let mut spare = available.saturating_sub(total(&widths));
    for (i, w) in widths.iter_mut().enumerate() {
        if spare == 0 {
            break;
        }
        if !is_numeric(i) && *w < natural[i] {
            *w += 1;
            spare -= 1;
        }
    }
    widths
}

/// The run of columns that fits in `available` columns, `spacing`
/// apart, and includes `focus` — from the first column when that
/// reaches it, otherwise ending at `focus`. Always at least the focused
/// column. Pure / testable.
pub fn visible_column_range(
    widths: &[usize],
    available: usize,
    spacing: usize,
    focus: usize,
) -> std::ops::Range<usize> {
    if widths.is_empty() {
        return 0..0;
    }
    let focus = focus.min(widths.len() - 1);
    let fits = |range: std::ops::Range<usize>| {
        widths[range.clone()].iter().sum::<usize>() + spacing * range.len().saturating_sub(1)
            <= available
    };
    let mut start = 0;
    while start < focus && !fits(start..focus + 1) {
        start += 1;
    }
    let mut end = focus + 1;
    while end < widths.len() && fits(start..end + 1) {
        end += 1;
    }
    start..end
}

/// The grid's column layout in one call: [`fit_column_widths`], then
/// [`visible_column_range`] around `focus`, then — when columns fell
/// off an edge — the shown ones fitted again on their own, so the
/// room a hidden column gave back goes to the ones on screen rather
/// than leaving them squeezed for it. `natural` and `numeric` are one
/// entry per column; the widths outside the range are meaningless.
/// Pure / testable.
pub fn layout_columns(
    natural: &[usize],
    numeric: &[bool],
    available: usize,
    spacing: usize,
    focus: usize,
) -> (Vec<usize>, std::ops::Range<usize>) {
    let mut widths = fit_column_widths(natural, numeric, available, spacing);
    let shown = visible_column_range(&widths, available, spacing, focus);
    if shown.len() < natural.len() {
        let numeric_shown: Vec<bool> = shown
            .clone()
            .map(|i| numeric.get(i).copied().unwrap_or(false))
            .collect();
        let refit = fit_column_widths(&natural[shown.clone()], &numeric_shown, available, spacing);
        widths[shown.clone()].copy_from_slice(&refit);
    }
    (widths, shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_columns_gives_a_hidden_columns_room_back() {
        // 9 + 11 + 6 + 8 + 8 + 22 and five gaps of 2 is 74; squeezed,
        // 59 — one over 58, so `due_at` goes, and `number` gets its
        // ninth column back.
        let natural = [9, 11, 6, 8, 8, 22];
        let numeric = [false, true, false, false, true, false];
        let (widths, shown) = layout_columns(&natural, &numeric, 58, 2, 0);
        assert_eq!(shown, 0..5);
        assert_eq!(&widths[shown], &[9, 11, 6, 8, 8]);
        // Everything fits: the plain fit, all columns.
        let (widths, shown) = layout_columns(&natural, &numeric, 200, 2, 0);
        assert_eq!((widths, shown), (natural.to_vec(), 0..6));
    }

    #[test]
    fn show_controls_makes_tabs_newlines_and_the_rest_visible() {
        let v = "tab\there\nline two\r\nthree";
        assert_eq!(show_controls(v, true), "tab⇥here⏎line two⏎three");
        assert_eq!(show_controls(v, false), "tab⇥here\nline two\nthree");
        assert_eq!(show_controls("a\rb\u{0}c\u{7f}\u{1b}", true), "a␍b␀c␡␛");
        assert_eq!(show_controls("x\u{85}y", true), "x�y", "a C1 control");
        // Nothing to do: borrowed, unchanged.
        assert!(matches!(
            show_controls("plain é 東京", true),
            std::borrow::Cow::Borrowed("plain é 東京")
        ));
    }

    #[test]
    fn a_null_is_as_wide_as_its_marker_and_an_empty_string_is_nothing() {
        let g = Grid {
            columns: vec!["n".into(), "e".into()],
            rows: vec![vec![None, Some(String::new())]],
            truncated: false,
        };
        assert_eq!(column_widths(&g, 48), vec![NULL_MARKER.len(), 1]);
    }

    #[test]
    fn an_empty_string_is_not_a_number_but_a_null_is_no_evidence() {
        let g = Grid {
            columns: vec!["with_null".into(), "with_empty".into()],
            rows: vec![
                vec![Some("1".into()), Some("1".into())],
                vec![None, Some(String::new())],
            ],
            truncated: false,
        };
        assert!(is_numeric_column(&g, 0));
        assert!(!is_numeric_column(&g, 1));
    }

    #[test]
    fn column_widths_measure_a_cell_as_it_is_drawn() {
        let g = Grid {
            columns: vec!["c".into()],
            rows: vec![vec![Some("a\tb\nc".into())]],
            truncated: false,
        };
        // `a⇥b⏎c`: five columns, where the raw tab and newline counted
        // for nothing.
        assert_eq!(column_widths(&g, 48), vec![5]);
    }

    #[test]
    fn looks_numeric_takes_what_postgres_prints_for_numbers() {
        for s in [
            "0",
            "42",
            "-99999999.99",
            "+1.5",
            ".5",
            "1e+30",
            "-1.5E-07",
            "NaN",
            "-Infinity",
        ] {
            assert!(looks_numeric(s), "{s}");
        }
        for s in [
            "",
            "-",
            ".",
            "1.2.3",
            "1e",
            "inf",
            "1b4e28ba-2fa1-11d2-883f-0016d3cca427",
            "2024-05-06",
            "$1.00",
            "12 34",
        ] {
            assert!(!looks_numeric(s), "{s}");
        }
    }

    #[test]
    fn a_numeric_column_is_all_numbers_and_nulls_but_not_only_nulls() {
        let g = Grid {
            columns: vec!["n".into(), "t".into(), "nulls".into()],
            rows: vec![
                vec![Some("1".into()), Some("a".into()), None],
                vec![None, Some("2".into()), None],
                vec![Some("-3.5".into()), Some("b".into()), None],
            ],
            truncated: false,
        };
        assert!(is_numeric_column(&g, 0));
        assert!(!is_numeric_column(&g, 1));
        assert!(!is_numeric_column(&g, 2));
        assert!(!is_numeric_column(&g, 9), "no such column");
    }

    #[test]
    fn fit_column_widths_leaves_a_table_that_fits_alone() {
        assert_eq!(
            fit_column_widths(&[2, 36, 12], &[true, false, true], 100, 2),
            vec![2, 36, 12]
        );
    }

    #[test]
    fn fit_column_widths_squeezes_text_and_never_numbers() {
        // id, uuid, total, three long text columns, a tag: 200 wide
        // natural, 120 available.
        let natural = [2, 36, 12, 48, 48, 48, 3];
        let numeric = [true, false, true, false, false, false, false];
        let got = fit_column_widths(&natural, &numeric, 120, 2);
        assert_eq!(got[0], 2, "{got:?}");
        assert_eq!(got[2], 12, "the number keeps its width: {got:?}");
        assert_eq!(got[6], 3, "a column under the cap keeps its width");
        assert_eq!(got.iter().sum::<usize>() + 2 * 6, 120, "{got:?}");
        // The cut is shared: the widest text columns give way first.
        assert!(got[1] >= got[3] - 1, "{got:?}");
        assert!(got[3].abs_diff(got[5]) <= 1, "{got:?}");
    }

    #[test]
    fn fit_column_widths_stops_text_at_its_floor() {
        let got = fit_column_widths(&[20, 30, 20], &[true, false, false], 30, 2);
        assert_eq!(got, vec![20, TEXT_MIN_WIDTH, TEXT_MIN_WIDTH]);
    }

    #[test]
    fn visible_column_range_keeps_the_focused_column_on_screen() {
        let widths = [10, 10, 10, 10];
        assert_eq!(visible_column_range(&widths, 100, 2, 0), 0..4);
        // 32 columns hold two (10 + 2 + 10) — not three (34).
        assert_eq!(visible_column_range(&widths, 32, 2, 0), 0..2);
        assert_eq!(visible_column_range(&widths, 32, 2, 1), 0..2);
        assert_eq!(visible_column_range(&widths, 32, 2, 3), 2..4);
        // Wider than the screen on its own: still shown.
        assert_eq!(visible_column_range(&[50, 10], 20, 2, 0), 0..1);
        assert_eq!(visible_column_range(&[], 20, 2, 0), 0..0);
        // A focus past the end clamps.
        assert_eq!(visible_column_range(&widths, 32, 2, 9), 2..4);
    }

    fn grid() -> Grid {
        Grid {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                vec![Some("1".into()), Some("alice".into())],
                vec![Some("1000".into()), Some("bob".into())],
            ],
            truncated: false,
        }
    }

    #[test]
    fn column_widths_take_the_widest_of_header_and_cells() {
        // col 0: header "id" (2) vs cell "1000" (4) -> 4
        // col 1: header "name" (4) vs cell "alice" (5) -> 5
        assert_eq!(column_widths(&grid(), 80), vec![4, 5]);
    }

    #[test]
    fn column_widths_clamp_to_max() {
        assert_eq!(column_widths(&grid(), 3), vec![3, 3]);
    }

    /// A CJK glyph paints two columns. Counting chars gave the column
    /// half the width its cells need, and `truncate_cell_parts` (which
    /// counts columns) then cut every value in half.
    #[test]
    fn column_widths_count_display_columns_not_chars() {
        let g = Grid {
            columns: vec!["id".into(), "名前".into()],
            rows: vec![
                vec![Some("1".into()), Some("東京都".into())],
                vec![Some("2".into()), Some("bob".into())],
            ],
            truncated: false,
        };
        // "名前" is 2 chars / 4 columns; "東京都" is 3 chars / 6 columns.
        assert_eq!(column_widths(&g, 80), vec![2, 6]);
        // A width-2 header alone must still get its 4 columns.
        let g = Grid {
            columns: vec!["名前".into()],
            rows: vec![vec![Some("x".into())]],
            truncated: false,
        };
        assert_eq!(column_widths(&g, 80), vec![4]);
    }

    #[test]
    fn truncate_cell_leaves_short_values_untouched() {
        assert_eq!(truncate_cell("abc", 5), "abc");
        assert_eq!(truncate_cell("abc", 3), "abc");
    }

    #[test]
    fn truncate_cell_marks_a_cut_with_ellipsis() {
        assert_eq!(truncate_cell("abcdef", 4), "abc…");
        assert_eq!(truncate_cell("abcdef", 1), "…");
    }

    #[test]
    fn truncate_cell_zero_width_is_empty() {
        assert_eq!(truncate_cell("abc", 0), "");
    }

    #[test]
    fn truncate_cell_counts_chars_not_bytes() {
        // Multi-byte chars count as one column each.
        assert_eq!(truncate_cell("héllo", 5), "héllo");
        assert_eq!(truncate_cell("héllo", 3), "hé…");
    }

    /// A CJK cell paints two columns per char, so a `char`-counting
    /// truncation returns a string twice as wide as the budget it was
    /// given — and the grid's own column layout then paints over the
    /// next column's cells.
    #[test]
    fn truncate_cell_counts_display_columns_not_chars() {
        use unicode_width::UnicodeWidthStr;
        // 4 chars, 8 columns.
        let cjk = "受注管理";
        assert_eq!(UnicodeWidthStr::width(cjk), 8);
        assert_eq!(truncate_cell(cjk, 8), cjk, "an exact fit is untouched");
        for w in 0..=10 {
            let got = truncate_cell(cjk, w);
            assert!(
                UnicodeWidthStr::width(got.as_str()) <= w,
                "width {w}: {got:?} paints {} columns",
                UnicodeWidthStr::width(got.as_str())
            );
        }
        // Room for one glyph plus the marker, not two.
        assert_eq!(truncate_cell(cjk, 4), "受…");
        assert_eq!(truncate_cell(cjk, 5), "受注…");
        // A glyph that would land half in and half out of the budget is
        // dropped whole — one column short beats one column over.
        assert_eq!(truncate_cell(cjk, 2), "…");
    }

    #[test]
    fn truncate_cell_parts_returns_empty_suffix_when_not_truncated() {
        assert_eq!(truncate_cell_parts("abc", 5), ("abc".into(), ""));
        assert_eq!(truncate_cell_parts("abc", 3), ("abc".into(), ""));
        assert_eq!(truncate_cell_parts("", 5), (String::new(), ""));
    }

    #[test]
    fn truncate_cell_parts_returns_ellipsis_when_truncated() {
        assert_eq!(truncate_cell_parts("abcdef", 4), ("abc".into(), "…"));
        assert_eq!(truncate_cell_parts("abcdef", 1), (String::new(), "…"));
        // Empty kept-text is intentional: width=1 leaves room only
        // for the marker.
    }

    #[test]
    fn truncate_cell_parts_zero_width_is_all_empty() {
        assert_eq!(truncate_cell_parts("abc", 0), (String::new(), ""));
    }

    #[test]
    fn truncate_cell_matches_truncate_cell_parts_concat() {
        for (s, w) in [
            ("abc", 0),
            ("abc", 3),
            ("abc", 5),
            ("abcdef", 4),
            ("héllo", 3),
        ] {
            let (kept, marker) = truncate_cell_parts(s, w);
            assert_eq!(truncate_cell(s, w), format!("{kept}{marker}"));
        }
    }

    #[test]
    fn cmp_cells_numbers_sort_numerically() {
        use std::cmp::Ordering;
        assert_eq!(cmp_cells("2", "10"), Ordering::Less);
        assert_eq!(cmp_cells("100", "20"), Ordering::Greater);
        assert_eq!(cmp_cells("1.5", "1.05"), Ordering::Greater);
    }

    #[test]
    fn cmp_cells_falls_back_to_string_when_either_non_numeric() {
        use std::cmp::Ordering;
        assert_eq!(cmp_cells("alice", "bob"), Ordering::Less);
        // One number, one text: lexicographic.
        assert_eq!(cmp_cells("10", "alice"), Ordering::Less);
    }

    #[test]
    fn cmp_cells_empty_string_is_null_last() {
        use std::cmp::Ordering;
        assert_eq!(cmp_cells("", "anything"), Ordering::Greater);
        assert_eq!(cmp_cells("anything", ""), Ordering::Less);
        assert_eq!(cmp_cells("", ""), Ordering::Equal);
    }
}
