//! Slow-query inventory from `pg_stat_statements`. Pure parsing of
//! the Grid produced by the catalog query lives here; the actual
//! query dispatch and rendering are App-side.
//!
//! `pg_stat_statements` is an extension; not every server has it
//! installed. The dispatch path surfaces the standard `relation
//! "pg_stat_statements" does not exist` error so the operator
//! knows what to install.

use crate::grid::Grid;

/// One row in the slow-query top-N panel. Mirrors the columns the
/// SQL below SELECTs in order.
#[derive(Debug, Clone, PartialEq)]
pub struct SlowQueryRow {
    pub query: String,
    pub calls: i64,
    /// Total wall-clock time across all `calls`, in milliseconds.
    pub total_ms: f64,
    /// Mean per-call time in milliseconds. Useful for catching
    /// queries that are individually fast but cumulatively expensive.
    pub mean_ms: f64,
    pub rows: i64,
}

/// The SQL we issue to populate the panel. The leading comment
/// tags the query so it can be filtered out of its own result on
/// the next refresh.
pub const PANEL_SQL: &str = "/* pgman:slow */ \
SELECT query, calls, total_exec_time, mean_exec_time, rows \
FROM pg_stat_statements \
WHERE query NOT LIKE '/* pgman:%' \
ORDER BY total_exec_time DESC NULLS LAST \
LIMIT 50";

/// Parse the result Grid into typed rows. Column order matches
/// [`PANEL_SQL`]; mis-ordering would silently mangle the panel, so
/// we look the columns up by name rather than positional index.
pub fn parse(grid: &Grid) -> Vec<SlowQueryRow> {
    // Resolve column indices by header name once.
    let idx = |name: &str| -> Option<usize> {
        grid.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
    };
    let q_idx = idx("query");
    let c_idx = idx("calls");
    let t_idx = idx("total_exec_time");
    let m_idx = idx("mean_exec_time");
    let r_idx = idx("rows");
    grid.rows
        .iter()
        .map(|r| SlowQueryRow {
            query: q_idx.and_then(|i| r.get(i).cloned()).unwrap_or_default(),
            calls: c_idx
                .and_then(|i| r.get(i))
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0),
            total_ms: t_idx
                .and_then(|i| r.get(i))
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0),
            mean_ms: m_idx
                .and_then(|i| r.get(i))
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0),
            rows: r_idx
                .and_then(|i| r.get(i))
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(rows: &[&[&str]]) -> Grid {
        Grid {
            columns: vec![
                "query".into(),
                "calls".into(),
                "total_exec_time".into(),
                "mean_exec_time".into(),
                "rows".into(),
            ],
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| (*s).to_string()).collect())
                .collect(),
            truncated: false,
        }
    }

    #[test]
    fn parse_pulls_typed_fields_in_order() {
        let g = grid(&[
            &["SELECT 1", "100", "500.25", "5.0025", "100"],
            &["UPDATE …", "10", "200.0", "20.0", "10"],
        ]);
        let parsed = parse(&g);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].query, "SELECT 1");
        assert_eq!(parsed[0].calls, 100);
        assert_eq!(parsed[0].total_ms, 500.25);
        assert!((parsed[0].mean_ms - 5.0025).abs() < 1e-9);
        assert_eq!(parsed[0].rows, 100);
    }

    #[test]
    fn parse_resolves_columns_by_name_not_position() {
        // Reorder the columns; the parser should still pick the
        // right field via the header lookup.
        let mut g = grid(&[&["x", "1", "2.0", "3.0", "4"]]);
        // Swap calls and total_exec_time order.
        g.columns.swap(1, 2);
        for row in &mut g.rows {
            row.swap(1, 2);
        }
        let parsed = parse(&g);
        assert_eq!(parsed[0].calls, 1);
        assert_eq!(parsed[0].total_ms, 2.0);
    }

    #[test]
    fn parse_handles_empty_grid() {
        let g = Grid::default();
        assert!(parse(&g).is_empty());
    }

    #[test]
    fn parse_treats_missing_columns_as_default_values() {
        let g = Grid {
            columns: vec!["query".into(), "calls".into()],
            rows: vec![vec!["SELECT 1".into(), "5".into()]],
            truncated: false,
        };
        let parsed = parse(&g);
        assert_eq!(parsed[0].query, "SELECT 1");
        assert_eq!(parsed[0].calls, 5);
        assert_eq!(parsed[0].total_ms, 0.0);
        assert_eq!(parsed[0].rows, 0);
    }
}
