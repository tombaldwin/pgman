//! A rectangular result set for display, plus pure layout helpers.
//!
//! Query results from any source land here as strings; the TUI renders this.
//! Column-width and truncation logic is pure and tested.

/// Cap on rows pulled into a single grid — keeps a runaway `SELECT` from
/// exhausting memory.
pub const MAX_ROWS: usize = 1000;

/// A result set: column headers plus string-rendered rows.
///
/// `truncated` is set when the underlying source had more rows than
/// `MAX_ROWS` — the renderer surfaces this so the user knows the view
/// is partial.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grid {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
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
/// clamped to `[1, max_width]`. Width is counted in `char`s.
pub fn column_widths(grid: &Grid, max_width: usize) -> Vec<usize> {
    let max_width = max_width.max(1);
    grid.columns
        .iter()
        .enumerate()
        .map(|(col, header)| {
            let cell_max = grid
                .rows
                .iter()
                .filter_map(|r| r.get(col))
                .map(|c| c.chars().count())
                .max()
                .unwrap_or(0);
            header.chars().count().max(cell_max).clamp(1, max_width)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                vec!["1".into(), "alice".into()],
                vec!["1000".into(), "bob".into()],
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
