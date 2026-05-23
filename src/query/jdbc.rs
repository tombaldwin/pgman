//! Reconstruct runnable SQL from pasted JDBC: a parameterised statement plus
//! a typed parameter list.
//!
//! `parse(sql, params)` takes:
//!   - `sql`: the statement with `?` placeholders.
//!   - `params`: one parameter per line as `TYPE:value` (e.g. `INTEGER:42`,
//!     `VARCHAR:Alice`, `NULL` value for any type → `VARCHAR:NULL`).
//!
//! The params drive `query::subst::apply` with `QuestionMark` placeholders.

use crate::query::reconstruct::{BoundParam, ParamValue, ReconstructedQuery, Source};
use crate::query::subst::{self, PlaceholderStyle};

/// Build a reconstructed query from pasted SQL and `TYPE:value` param lines.
/// Returns `None` if `sql` is empty after trimming.
pub fn parse(sql: &str, params_text: &str) -> Option<ReconstructedQuery> {
    let raw_sql = sql.trim();
    if raw_sql.is_empty() {
        return None;
    }
    let params = parse_params(params_text);
    let runnable_sql = subst::apply(raw_sql, &params, PlaceholderStyle::QuestionMark)
        .unwrap_or_else(|_| raw_sql.to_string());
    Some(ReconstructedQuery {
        raw_sql: raw_sql.to_string(),
        params,
        runnable_sql,
        source: Source::JdbcPaste,
        src_line: 0,
    })
}

/// Parse one parameter per line. Blank lines are skipped; lines without a `:`
/// are ignored (so comments / accidental text don't blow up the parse).
fn parse_params(text: &str) -> Vec<BoundParam> {
    let mut idx = 0;
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (ty, val) = line.split_once(':')?;
            idx += 1;
            let val = val.trim();
            let value = if val.eq_ignore_ascii_case("null") {
                ParamValue::Null
            } else {
                ParamValue::Literal(val.to_string())
            };
            Some(BoundParam {
                index: idx,
                sql_type: ty.trim().to_string(),
                value,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sql_yields_none() {
        assert!(parse("", "").is_none());
        assert!(parse("   ", "").is_none());
    }

    #[test]
    fn substitutes_typed_params() {
        let q = parse(
            "select * from t where id = ? and name = ?",
            "INTEGER:42\nVARCHAR:Alice",
        )
        .unwrap();
        assert_eq!(q.params.len(), 2);
        assert_eq!(q.runnable_sql, "select * from t where id = 42 and name = 'Alice'");
        assert_eq!(q.source, Source::JdbcPaste);
    }

    #[test]
    fn null_value_substitutes_unquoted_null() {
        let q = parse("update t set note = ? where id = ?", "VARCHAR:NULL\nINTEGER:9")
            .unwrap();
        assert_eq!(q.runnable_sql, "update t set note = NULL where id = 9");
    }

    #[test]
    fn blank_and_unstructured_lines_are_skipped() {
        let q = parse(
            "select ?",
            "INTEGER:7\n\n# a comment line without colons\nbut this one is empty",
        )
        .unwrap();
        // Only the INTEGER:7 line was a real parameter; "# a comment …" has no
        // colon — skipped; "but this one is empty" — skipped.
        assert_eq!(q.params.len(), 1);
        assert_eq!(q.runnable_sql, "select 7");
    }

    #[test]
    fn arity_mismatch_falls_back_to_raw_sql() {
        // Two ? placeholders, only one param → substitution fails → keep raw.
        let q = parse("select ?, ?", "INTEGER:1").unwrap();
        assert_eq!(q.runnable_sql, q.raw_sql);
        assert_eq!(q.params.len(), 1);
    }
}
