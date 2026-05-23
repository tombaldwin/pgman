//! SQL vocabulary — the lookup tables driving keyword / function /
//! operator completion.
//!
//! Single source of truth for "what words can pgman suggest". When
//! Postgres adds a new aggregate, a new operator, or you want to
//! support a new SQL clause, edit this file and only this file: the
//! tables here are flat `&[&str]` slices, the completion engine
//! (`query::complete`) just iterates them.
//!
//! Conventions:
//! - Names are uppercase (the convention SQL completion tools tend to
//!   use; matching is case-insensitive via `starts_with_ci`).
//! - Each table is grouped by "where in the grammar this word makes
//!   sense" so the right slice can be plugged into the right
//!   `ClauseContext` arm.
//! - Adding entries is intentional and intentional only — no clever
//!   parsing of pg_proc / pg_aggregate at compile time. The trade-off:
//!   the lists drift if Postgres adds something we don't know about,
//!   but the surface is small (an hour of work to refresh against the
//!   current pg_proc dump) and we never spuriously suggest something
//!   that doesn't exist on a stock Postgres.

/// SQL verbs / clause introducers offered at statement-start
/// (`StatementStart` context). Adding a new verb takes one line; adding
/// a new SUB-clause keyword (e.g. `FETCH` for cursor results) takes one
/// line here AND a corresponding arm in `query::clause` if it should
/// shift the cursor's classification.
pub const STATEMENT_KEYWORDS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "WITH", "EXPLAIN",
    "BEGIN", "COMMIT", "ROLLBACK", "SHOW", "VACUUM", "ANALYZE",
    "TRUNCATE", "VALUES",
];

/// Aggregate functions surfaced in `SelectList` (and `RETURNING`).
/// Inserts as `NAME(` so the cursor lands inside the paren ready for
/// arguments — see `query::complete::candidates_functions`.
///
/// To add a new aggregate (e.g. when Postgres adds one): append the
/// uppercase name here.
pub const AGGREGATE_FUNCTIONS: &[&str] = &[
    "COUNT", "SUM", "AVG", "MIN", "MAX",
    "ARRAY_AGG", "STRING_AGG", "BOOL_AND", "BOOL_OR",
    "JSON_AGG", "JSONB_AGG", "JSON_OBJECT_AGG",
];

/// Scalar functions commonly used in SELECT lists / expressions —
/// COALESCE, NULLIF, string / date helpers etc. Same insertion shape
/// as aggregates (`NAME(`).
pub const SCALAR_FUNCTIONS: &[&str] = &[
    "COALESCE", "NULLIF", "GREATEST", "LEAST",
    "NOW", "CURRENT_TIMESTAMP", "CURRENT_DATE", "CURRENT_TIME",
    "LENGTH", "LOWER", "UPPER", "TRIM", "SUBSTRING", "CONCAT",
    "EXTRACT", "DATE_TRUNC", "AGE",
    "CAST",
];

/// Window functions (the `OVER (...)` family). Same insertion as the
/// aggregates. Not yet differentiated from aggregates in completion —
/// the operator's intent (aggregate vs window) is determined by
/// whether they type `OVER` after the call, which we can't tell at
/// suggestion time.
pub const WINDOW_FUNCTIONS: &[&str] = &[
    "ROW_NUMBER", "RANK", "DENSE_RANK", "PERCENT_RANK", "CUME_DIST",
    "LAG", "LEAD", "FIRST_VALUE", "LAST_VALUE", "NTH_VALUE", "NTILE",
];

/// Word-shaped operators / connectives that the operator naturally
/// Tab-completes inside a `Predicate` context (WHERE / HAVING / ON).
/// Symbolic operators (`=`, `>`, `<>`, `!=`) are short enough that
/// suggesting them adds noise; they're left out deliberately.
///
/// Multi-word phrases (`IS NULL`, `IS NOT NULL`, `NOT IN`) are
/// emitted as single candidates so Tab once gets the whole shape.
pub const PREDICATE_OPERATORS: &[&str] = &[
    "AND", "OR", "NOT",
    "LIKE", "ILIKE", "IN", "BETWEEN", "EXISTS",
    "IS NULL", "IS NOT NULL", "NOT IN", "NOT LIKE", "NOT ILIKE",
    "IS DISTINCT FROM", "IS NOT DISTINCT FROM",
    "SIMILAR TO",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract: every entry across every list is uppercase. Matching
    /// is case-insensitive so this isn't a correctness bug, but a
    /// lowercase entry would cycle past mixed-case Tab presses oddly
    /// (popup shows "select" not "SELECT").
    #[test]
    fn all_entries_are_uppercase() {
        for table in [
            STATEMENT_KEYWORDS,
            AGGREGATE_FUNCTIONS,
            SCALAR_FUNCTIONS,
            WINDOW_FUNCTIONS,
            PREDICATE_OPERATORS,
        ] {
            for word in table {
                assert_eq!(
                    *word,
                    word.to_ascii_uppercase(),
                    "vocabulary entry {word:?} is not uppercase"
                );
                assert!(!word.is_empty(), "vocabulary entry is empty string");
            }
        }
    }

    /// Contract: no duplicates within a single list. Duplicates would
    /// cycle past the same candidate twice in the completion popup.
    #[test]
    fn no_duplicates_within_a_list() {
        for (label, table) in [
            ("STATEMENT_KEYWORDS", STATEMENT_KEYWORDS),
            ("AGGREGATE_FUNCTIONS", AGGREGATE_FUNCTIONS),
            ("SCALAR_FUNCTIONS", SCALAR_FUNCTIONS),
            ("WINDOW_FUNCTIONS", WINDOW_FUNCTIONS),
            ("PREDICATE_OPERATORS", PREDICATE_OPERATORS),
        ] {
            let mut seen = std::collections::BTreeSet::new();
            for word in table {
                assert!(
                    seen.insert(*word),
                    "{label} contains duplicate {word:?}"
                );
            }
        }
    }
}
