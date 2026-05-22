//! Reconstruct runnable SQL from PostgreSQL / RDS server logs.
//!
//! Stub — M1 (see BACKLOG.md). This is the *primary* reconstruction source:
//! with `log_statement = 'all'` (or `log_min_duration_statement`), Postgres
//! logs the statement and its `DETAIL: parameters: $1 = '…'` together, and it
//! can be enabled server-side with no application redeploy.
//!
//! The parser must handle `execute`/`statement` lines with `$N` placeholders,
//! the following `DETAIL: parameters:` line, and multi-line statements.

use crate::query::reconstruct::ReconstructedQuery;

/// Parse a Postgres server log into reconstructed queries.
pub fn parse(_log: &str) -> Vec<ReconstructedQuery> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_log_yields_nothing() {
        assert!(parse("").is_empty());
    }
}
