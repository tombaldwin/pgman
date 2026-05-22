//! Reconstruct runnable SQL from application-side Hibernate logs.
//!
//! Stub — M1 (see BACKLOG.md). The parser must handle:
//!   - SQL lines from logger `org.hibernate.SQL` (`?` placeholders).
//!   - Bind lines, Hibernate 5: `org.hibernate.type.descriptor.sql.BasicBinder`
//!     — `binding parameter [1] as [INTEGER] - [42]`.
//!   - Bind lines, Hibernate 6: `org.hibernate.orm.jdbc.bind`
//!     — `binding parameter (1:INTEGER) <- [42]`.
//!   - Thread interleaving: group lines by thread token, pair each SQL line
//!     with the binds that follow it on the same thread.
//!
//! Note: bind lines are logged at TRACE and are often absent in production
//! logs; the parser must still emit the `?`-form statement when binds are
//! missing. `pglog` is the more reliable reconstruction source.

use crate::query::reconstruct::ReconstructedQuery;

/// Parse a Hibernate log into reconstructed queries.
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
