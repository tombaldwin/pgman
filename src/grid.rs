//! A rectangular result set for display, plus pure layout helpers.
//!
//! Query results from any source land here as strings; the TUI renders this.
//! Column-width and truncation logic is pure and tested.

/// Cap on rows pulled into a single grid — keeps a runaway `SELECT` from
/// exhausting memory.
pub const MAX_ROWS: usize = 1000;

/// The result panel's title note for a capped grid: that it stopped,
/// and that the rest was never fetched — `capped at 1000` alone read as
/// though the table had a thousand rows. Pure.
pub fn cap_title_note() -> String {
    format!("capped at {MAX_ROWS} · more rows not fetched")
}

/// Where to get every row: `pgman query` with JSON output is not
/// capped (see `batch::truncated_warning`).
const EVERY_ROW_HINT: &str = "pgman query '…' --json returns every row";

/// How a partial result — one with more rows than the grid holds —
/// can continue (docs/backlog/paging.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum More {
    /// It did not run through the result cursor: an `EXPLAIN`, a
    /// locking `SELECT`, a `\watch` run, a panel's grid.
    Unpaged,
    /// A plain `SELECT`, but the session was inside a transaction.
    InTransaction,
    /// `M` fetches the next page from the open cursor.
    CursorOpen,
    /// The cursor has closed; `M` re-runs the statement.
    CursorClosed,
    /// It ran on a standby, where pgman never holds a cursor (a
    /// recovery conflict would end the session); `M` re-runs it.
    Standby,
    /// pgman holds no more than `paging::MAX_HELD_ROWS`.
    Ceiling,
}

/// The result panel's title note for a partial grid. `partial` says
/// that what the grid shows — and sorts, filters, finds and yanks — is
/// the rows fetched so far. Pure.
pub fn partial_title_note(more: More) -> String {
    match more {
        More::Unpaged => cap_title_note(),
        More::InTransaction => format!("{} · in a transaction, so no paging", cap_title_note()),
        More::CursorOpen => "partial · M fetches more".to_string(),
        More::CursorClosed => "partial · cursor closed · M re-runs".to_string(),
        More::Standby => "partial · on a standby · M re-runs for more".to_string(),
        More::Ceiling => format!(
            "partial · stopped at pgman's {}-row limit",
            crate::query::paging::MAX_HELD_ROWS
        ),
    }
}

/// The status line's note for a partial grid: the title's note, plus
/// where to get every row when `M` can't. Pure.
pub fn partial_status_note(more: More) -> String {
    let note = partial_title_note(more);
    match more {
        More::CursorOpen | More::CursorClosed | More::Standby => note,
        More::Unpaged | More::InTransaction | More::Ceiling => {
            format!("{note} · {EVERY_ROW_HINT}")
        }
    }
}

/// What `Y` and `:fixture` add to their status for a partial result:
/// they write the rows pgman holds, not the whole result. Pure.
pub fn fetched_only_note(partial: bool) -> &'static str {
    if partial {
        " (fetched rows only · more not fetched)"
    } else {
        ""
    }
}

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

/// Where a result column was read from, as the server's RowDescription
/// says: the table's `pg_class.oid` and the column's `attnum`. `None`
/// in its place means an expression (`id + 1`, `count(*)`, a literal),
/// which no table column backs. Cell editing and the PK-keyed result
/// diff trust this, never the column's name: `SELECT id + 1 AS id`
/// has an `id` column that is not the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColumnOrigin {
    pub table_oid: u32,
    pub attnum: i16,
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

/// How a sort compares one column's cells, decided once for the whole
/// column by [`sort_mode`]: deciding per pair (numbers when both parse,
/// bytes otherwise) is not an order at all — `2 < 10 < 1a < 2` — and
/// `sort_by` may panic on one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    /// Every non-NULL cell is a number as Postgres prints one: compare
    /// exact values ([`cmp_numeric`]).
    Numeric,
    /// Anything else: compare the text byte by byte.
    Text,
}

/// The [`SortMode`] for column `col` of `rows`: [`SortMode::Numeric`]
/// when every non-NULL cell [`looks_numeric`] (and there is at least
/// one), [`SortMode::Text`] otherwise. A short row's missing cell is a
/// NULL. Pure / testable.
pub fn sort_mode(rows: &[Vec<Cell>], col: usize) -> SortMode {
    let mut any = false;
    for cell in rows.iter().filter_map(|r| r.get(col)).flatten() {
        if !looks_numeric(cell) {
            return SortMode::Text;
        }
        any = true;
    }
    if any {
        SortMode::Numeric
    } else {
        SortMode::Text
    }
}

/// Compare two cells for sorting under `mode` — a total order. SQL
/// NULL (`None`) sorts AFTER every value, Postgres's default: `NULLS
/// LAST` for `ORDER BY … ASC`, and — the sort reversing this for DESC —
/// `NULLS FIRST` for `DESC`. An empty string is a value like any other:
/// the smallest string, so first ascending.
///
/// ```
/// use std::cmp::Ordering;
/// use pgman::grid::{cmp_cells, SortMode};
/// assert_eq!(cmp_cells(SortMode::Numeric, Some("2"), Some("10")), Ordering::Less);
/// assert_eq!(cmp_cells(SortMode::Text, Some("2"), Some("10")), Ordering::Greater);
/// assert_eq!(cmp_cells(SortMode::Text, None, Some("anything")), Ordering::Greater); // NULL last
/// assert_eq!(cmp_cells(SortMode::Text, Some(""), Some("a")), Ordering::Less);       // '' first
/// ```
pub fn cmp_cells(mode: SortMode, a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => match mode {
            SortMode::Numeric => cmp_numeric(a, b),
            SortMode::Text => a.cmp(b),
        },
    }
}

/// Sort `rows` on column `col`, ascending or not, in place. The mode
/// is [`sort_mode`] of the column; the sort is stable, and descending
/// is the ascending order reversed (rows that compare equal keep their
/// order either way). Pure / testable.
pub fn sort_rows(rows: &mut [Vec<Cell>], col: usize, asc: bool) {
    let mode = sort_mode(rows, col);
    rows.sort_by(|a, b| {
        let av = a.get(col).and_then(Option::as_deref);
        let bv = b.get(col).and_then(Option::as_deref);
        let ord = cmp_cells(mode, av, bv);
        if asc {
            ord
        } else {
            ord.reverse()
        }
    });
}

/// A number's text, as a key that compares by exact value.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NumKey {
    NegInfinity,
    /// A negative finite value: its magnitude, so larger is smaller.
    Negative(std::cmp::Reverse<Magnitude>),
    Zero,
    Positive(Magnitude),
    Infinity,
    /// Postgres sorts NaN above every other value, infinity included.
    NaN,
}

/// A non-zero magnitude `0.d₁d₂… × 10^exp`, `digits` without leading
/// or trailing zeros — so a larger `exp` is a larger magnitude, and at
/// equal `exp` the digit strings compare as the values do.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Magnitude {
    exp: i64,
    digits: Vec<u8>,
}

fn num_key(s: &str) -> NumKey {
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    match body {
        "NaN" => return NumKey::NaN,
        "Infinity" if neg => return NumKey::NegInfinity,
        "Infinity" => return NumKey::Infinity,
        _ => {}
    }
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, ""),
    };
    // An exponent too large for i64 saturates: no grid holds a value
    // where that difference matters, and it keeps the order total.
    let exponent: i64 = if exponent.is_empty() {
        0
    } else {
        exponent.parse().unwrap_or_else(|_| {
            if exponent.starts_with('-') {
                i64::MIN / 2
            } else {
                i64::MAX / 2
            }
        })
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let all: Vec<u8> = int_part.bytes().chain(frac_part.bytes()).collect();
    let Some(first) = all.iter().position(|&b| b != b'0') else {
        return NumKey::Zero;
    };
    let last = all.iter().rposition(|&b| b != b'0').unwrap_or(first);
    // Value = 0.<all> × 10^(int_part.len() + exponent); dropping
    // `first` leading zeros lowers the point by as many.
    let exp = (int_part.len() as i64)
        .saturating_sub(first as i64)
        .saturating_add(exponent);
    let mag = Magnitude {
        exp,
        digits: all[first..=last].to_vec(),
    };
    if neg {
        NumKey::Negative(std::cmp::Reverse(mag))
    } else {
        NumKey::Positive(mag)
    }
}

/// Compare two numbers as Postgres prints them ([`looks_numeric`]) by
/// exact value: `9007199254740993` above `9007199254740992`, which an
/// `f64` compare calls equal; `1.50` equal to `1.5`; `-Infinity` below
/// every number, `Infinity` above, and `NaN` above even that, as
/// Postgres orders them. Meant for text that [`looks_numeric`]; other
/// text still gets a consistent place, so the order stays total.
/// Pure / testable.
pub fn cmp_numeric(a: &str, b: &str) -> std::cmp::Ordering {
    num_key(a).cmp(&num_key(b))
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
    sort_mode(&grid.rows, col) == SortMode::Numeric
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
    fn a_partial_result_says_how_to_continue() {
        assert_eq!(
            partial_title_note(More::CursorOpen),
            "partial · M fetches more"
        );
        assert_eq!(
            partial_title_note(More::CursorClosed),
            "partial · cursor closed · M re-runs"
        );
        assert_eq!(
            partial_title_note(More::Ceiling),
            "partial · stopped at pgman's 100000-row limit"
        );
        assert_eq!(
            partial_title_note(More::Unpaged),
            "capped at 1000 · more rows not fetched"
        );
        assert_eq!(
            partial_title_note(More::InTransaction),
            "capped at 1000 · more rows not fetched · in a transaction, so no paging"
        );
        // Where `M` can go on, the status says so and no more; where it
        // can't, it names the way to every row.
        assert_eq!(
            partial_title_note(More::Standby),
            "partial · on a standby · M re-runs for more"
        );
        for more in [More::CursorOpen, More::CursorClosed, More::Standby] {
            assert_eq!(partial_status_note(more), partial_title_note(more));
        }
        for more in [More::Unpaged, More::InTransaction, More::Ceiling] {
            assert!(
                partial_status_note(more).ends_with("pgman query '…' --json returns every row"),
                "{more:?}"
            );
        }
        assert_eq!(fetched_only_note(false), "");
        assert!(fetched_only_note(true).contains("fetched rows only"));
    }

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

    use std::cmp::Ordering;

    fn col(cells: &[Option<&str>]) -> Vec<Vec<Cell>> {
        cells.iter().map(|c| vec![c.map(String::from)]).collect()
    }

    fn keys(rows: &[Vec<Cell>]) -> Vec<Option<&str>> {
        rows.iter().map(|r| r[0].as_deref()).collect()
    }

    #[test]
    fn cmp_cells_numbers_sort_numerically() {
        let n = SortMode::Numeric;
        assert_eq!(cmp_cells(n, Some("2"), Some("10")), Ordering::Less);
        assert_eq!(cmp_cells(n, Some("100"), Some("20")), Ordering::Greater);
        assert_eq!(cmp_cells(n, Some("1.5"), Some("1.05")), Ordering::Greater);
        assert_eq!(cmp_cells(n, Some("-2"), Some("-10")), Ordering::Greater);
        assert_eq!(cmp_cells(n, Some("-0.5"), Some("0")), Ordering::Less);
        assert_eq!(cmp_cells(n, Some("-0"), Some("0.000")), Ordering::Equal);
        assert_eq!(cmp_cells(n, Some("1.50"), Some("1.5")), Ordering::Equal);
        assert_eq!(cmp_cells(n, Some("1e+30"), Some("999")), Ordering::Greater);
        assert_eq!(
            cmp_cells(n, Some("1.5E-07"), Some("0.0000001")),
            Ordering::Greater
        );
        assert_eq!(cmp_cells(n, Some(".5"), Some("0.49")), Ordering::Greater);
    }

    /// int8 past 2^53: an f64 compare called these equal.
    #[test]
    fn cmp_numeric_is_exact_past_f64_precision() {
        assert_eq!(
            cmp_numeric("9007199254740993", "9007199254740992"),
            Ordering::Greater
        );
        assert_eq!(
            cmp_numeric("-9223372036854775808", "-9223372036854775807"),
            Ordering::Less
        );
        assert_eq!(
            cmp_numeric("12345678901234567890.000000001", "12345678901234567890"),
            Ordering::Greater
        );
    }

    /// Postgres orders `-Infinity` < numbers < `Infinity` < `NaN`.
    #[test]
    fn cmp_numeric_orders_infinities_and_nan_as_postgres_does() {
        let mut v = vec!["NaN", "1", "Infinity", "-Infinity", "-1e+300", "1e+300"];
        v.sort_by(|a, b| cmp_numeric(a, b));
        assert_eq!(
            v,
            ["-Infinity", "-1e+300", "1", "1e+300", "Infinity", "NaN"]
        );
        assert_eq!(cmp_numeric("NaN", "NaN"), Ordering::Equal);
    }

    #[test]
    fn text_mode_compares_bytes() {
        let t = SortMode::Text;
        assert_eq!(cmp_cells(t, Some("alice"), Some("bob")), Ordering::Less);
        assert_eq!(cmp_cells(t, Some("10"), Some("2")), Ordering::Less);
        assert_eq!(cmp_cells(t, Some("10"), Some("alice")), Ordering::Less);
    }

    /// One mode for the whole column: the `2 < 10 < 1a < 2` cycle a
    /// per-pair decision made is gone, and text such as `inf` or `nan`
    /// is not taken for a number.
    #[test]
    fn sort_mode_is_decided_once_for_the_column() {
        assert_eq!(
            sort_mode(&col(&[Some("2"), Some("10"), None]), 0),
            SortMode::Numeric
        );
        assert_eq!(
            sort_mode(&col(&[Some("2"), Some("10"), Some("1a")]), 0),
            SortMode::Text
        );
        assert_eq!(
            sort_mode(&col(&[Some("1"), Some("inf")]), 0),
            SortMode::Text
        );
        assert_eq!(
            sort_mode(&col(&[Some("1"), Some("nan")]), 0),
            SortMode::Text
        );
        assert_eq!(sort_mode(&col(&[Some("1"), Some("")]), 0), SortMode::Text);
        assert_eq!(sort_mode(&col(&[None, None]), 0), SortMode::Text);
        let mut rows = col(&[Some("2"), Some("1a"), Some("10")]);
        sort_rows(&mut rows, 0, true);
        assert_eq!(keys(&rows), [Some("10"), Some("1a"), Some("2")]);
        let mut rows = col(&[Some("2"), None, Some("10"), Some("-3")]);
        sort_rows(&mut rows, 0, true);
        assert_eq!(keys(&rows), [Some("-3"), Some("2"), Some("10"), None]);
        sort_rows(&mut rows, 0, false);
        assert_eq!(keys(&rows), [None, Some("10"), Some("2"), Some("-3")]);
    }

    /// A short row's missing cell sorts as a NULL.
    #[test]
    fn sort_rows_treats_a_missing_cell_as_null() {
        let mut rows = vec![vec![], vec![Some("1".to_string())]];
        sort_rows(&mut rows, 0, true);
        assert_eq!(rows[0], vec![Some("1".to_string())]);
    }

    #[test]
    fn cmp_cells_null_is_last_and_the_empty_string_is_a_value() {
        for m in [SortMode::Numeric, SortMode::Text] {
            assert_eq!(cmp_cells(m, None, Some("anything")), Ordering::Greater);
            assert_eq!(cmp_cells(m, Some("anything"), None), Ordering::Less);
            assert_eq!(cmp_cells(m, None, None), Ordering::Equal);
            // After the empty string too: it is a value, not a NULL.
            assert_eq!(cmp_cells(m, None, Some("")), Ordering::Greater);
        }
        let t = SortMode::Text;
        assert_eq!(cmp_cells(t, Some(""), Some("a")), Ordering::Less);
        assert_eq!(cmp_cells(t, Some(""), Some("1")), Ordering::Less);
        assert_eq!(cmp_cells(t, Some(""), Some("")), Ordering::Equal);
    }
}
