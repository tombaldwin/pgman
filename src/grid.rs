//! A rectangular result set for display, plus pure layout helpers.
//!
//! Query results from any source land here as strings; the TUI renders this.
//! Column-width and truncation logic is pure and tested.

/// Cap on rows pulled into a single grid — keeps a runaway `SELECT` from
/// exhausting memory.
pub const MAX_ROWS: usize = 1000;

/// A result set: column headers plus string-rendered rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grid {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
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
    if width == 0 {
        return String::new();
    }
    if s.chars().count() <= width {
        return s.to_string();
    }
    let kept: String = s.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
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
