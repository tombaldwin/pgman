//! Type-aware placeholder substitution — shared by every reconstruction source.
//!
//! Substitutes bound parameters back into a statement to produce runnable SQL.
//! Numeric/boolean types are emitted bare; everything else is single-quoted
//! with `''` escaping. Placeholders inside string literals are left alone.

use crate::query::reconstruct::{BoundParam, ParamValue};
use std::fmt;

/// Which placeholder convention a statement uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderStyle {
    /// JDBC / Hibernate `?`, bound positionally.
    QuestionMark,
    /// PostgreSQL `$1`, `$2`, … bound by index (and possibly reused).
    Numbered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstError {
    /// The statement has a different number of `?` placeholders than params.
    ArityMismatch { placeholders: usize, params: usize },
    /// A `$N` referenced an index with no corresponding parameter.
    MissingParam(usize),
}

impl fmt::Display for SubstError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubstError::ArityMismatch {
                placeholders,
                params,
            } => write!(
                f,
                "statement has {placeholders} placeholder(s) but {params} parameter(s) were supplied"
            ),
            SubstError::MissingParam(n) => write!(f, "no parameter for placeholder ${n}"),
        }
    }
}

impl std::error::Error for SubstError {}

/// Substitute `params` into `raw_sql` according to `style`.
pub fn apply(
    raw_sql: &str,
    params: &[BoundParam],
    style: PlaceholderStyle,
) -> Result<String, SubstError> {
    match style {
        PlaceholderStyle::QuestionMark => apply_qmark(raw_sql, params),
        PlaceholderStyle::Numbered => apply_numbered(raw_sql, params),
    }
}

fn apply_qmark(sql: &str, params: &[BoundParam]) -> Result<String, SubstError> {
    let mut out = String::with_capacity(sql.len() + 32);
    let mut chars = sql.chars().peekable();
    let mut in_string = false;
    let mut seen = 0usize;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    out.push(chars.next().unwrap());
                } else {
                    in_string = false;
                }
            }
            continue;
        }
        match c {
            '\'' => {
                in_string = true;
                out.push(c);
            }
            '?' => {
                if let Some(p) = params.get(seen) {
                    out.push_str(&quote(p));
                } else {
                    out.push(c); // surplus placeholder — error reported below
                }
                seen += 1;
            }
            _ => out.push(c),
        }
    }
    if seen != params.len() {
        return Err(SubstError::ArityMismatch {
            placeholders: seen,
            params: params.len(),
        });
    }
    Ok(out)
}

fn apply_numbered(sql: &str, params: &[BoundParam]) -> Result<String, SubstError> {
    let mut out = String::with_capacity(sql.len() + 32);
    let mut chars = sql.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    out.push(chars.next().unwrap());
                } else {
                    in_string = false;
                }
            }
            continue;
        }
        if c == '\'' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == '$' && chars.peek().is_some_and(|d| d.is_ascii_digit()) {
            let mut digits = String::new();
            while let Some(d) = chars.peek() {
                if d.is_ascii_digit() {
                    digits.push(*d);
                    chars.next();
                } else {
                    break;
                }
            }
            let n = digits.parse::<usize>().unwrap_or(usize::MAX);
            match n.checked_sub(1).and_then(|i| params.get(i)) {
                Some(p) => out.push_str(&quote(p)),
                None => return Err(SubstError::MissingParam(n)),
            }
            continue;
        }
        out.push(c);
    }
    Ok(out)
}

/// Render one parameter as a SQL literal.
fn quote(p: &BoundParam) -> String {
    match &p.value {
        ParamValue::Null => "NULL".to_string(),
        ParamValue::Literal(v) => {
            if is_bare_type(&p.sql_type) {
                v.clone()
            } else {
                format!("'{}'", v.replace('\'', "''"))
            }
        }
    }
}

/// Types emitted without surrounding quotes — numerics and booleans.
fn is_bare_type(sql_type: &str) -> bool {
    let t = sql_type.to_ascii_uppercase();
    const BARE: &[&str] = &[
        "INT", "SERIAL", "DECIMAL", "NUMERIC", "FLOAT", "DOUBLE", "REAL", "BOOL",
    ];
    BARE.iter().any(|k| t.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(index: usize, sql_type: &str, value: &str) -> BoundParam {
        BoundParam {
            index,
            sql_type: sql_type.to_string(),
            value: ParamValue::Literal(value.to_string()),
        }
    }

    fn null(index: usize, sql_type: &str) -> BoundParam {
        BoundParam {
            index,
            sql_type: sql_type.to_string(),
            value: ParamValue::Null,
        }
    }

    #[test]
    fn substitutes_qmark_with_type_aware_quoting() {
        let params = [p(1, "INTEGER", "42"), p(2, "VARCHAR", "alice")];
        let out = apply(
            "SELECT * FROM t WHERE id = ? AND name = ?",
            &params,
            PlaceholderStyle::QuestionMark,
        )
        .unwrap();
        assert_eq!(out, "SELECT * FROM t WHERE id = 42 AND name = 'alice'");
    }

    #[test]
    fn null_becomes_unquoted_null() {
        let params = [null(1, "VARCHAR")];
        let out = apply("UPDATE t SET note = ?", &params, PlaceholderStyle::QuestionMark).unwrap();
        assert_eq!(out, "UPDATE t SET note = NULL");
    }

    #[test]
    fn escapes_single_quotes_in_string_values() {
        let params = [p(1, "VARCHAR", "O'Brien")];
        let out = apply("SELECT ?", &params, PlaceholderStyle::QuestionMark).unwrap();
        assert_eq!(out, "SELECT 'O''Brien'");
    }

    #[test]
    fn placeholder_inside_a_string_literal_is_left_alone() {
        let params = [p(1, "INTEGER", "7")];
        let out = apply(
            "SELECT '? literal', ?",
            &params,
            PlaceholderStyle::QuestionMark,
        )
        .unwrap();
        assert_eq!(out, "SELECT '? literal', 7");
    }

    #[test]
    fn arity_mismatch_is_reported() {
        let params = [p(1, "INTEGER", "1")];
        let err = apply("SELECT ?, ?", &params, PlaceholderStyle::QuestionMark).unwrap_err();
        assert_eq!(
            err,
            SubstError::ArityMismatch {
                placeholders: 2,
                params: 1
            }
        );
    }

    #[test]
    fn substitutes_numbered_placeholders_including_reuse() {
        let params = [p(1, "INTEGER", "5"), p(2, "TEXT", "x")];
        let out = apply(
            "SELECT $1 WHERE a = $2 OR b = $1",
            &params,
            PlaceholderStyle::Numbered,
        )
        .unwrap();
        assert_eq!(out, "SELECT 5 WHERE a = 'x' OR b = 5");
    }

    #[test]
    fn numbered_out_of_range_is_reported() {
        let params = [p(1, "INTEGER", "1")];
        let err = apply("SELECT $2", &params, PlaceholderStyle::Numbered).unwrap_err();
        assert_eq!(err, SubstError::MissingParam(2));
    }
}
