//! Reconstruct runnable SQL from pasted JDBC: a parameterised statement plus a
//! typed parameter list.
//!
//! Stub — M2 (see BACKLOG.md). v1 takes two inputs: the SQL with `?`
//! placeholders, and parameters one-per-line as `TYPE:value`. Both feed
//! `query::subst::apply`. A stretch goal scrapes `ps.setXxx(n, v)` calls out of
//! pasted Java.

use crate::query::reconstruct::ReconstructedQuery;

/// Build a reconstructed query from pasted SQL and `TYPE:value` parameter lines.
pub fn parse(_sql: &str, _params: &str) -> Option<ReconstructedQuery> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_paste_yields_nothing() {
        assert!(parse("", "").is_none());
    }
}
