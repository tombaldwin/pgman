//! The shared output type for every reconstruction source. Don't fork this —
//! `hibernate`, `pglog`, and `jdbc` all produce `ReconstructedQuery`.

/// A bound parameter value. Hibernate and Postgres both log `null` distinctly
/// from the string `"null"`, so it gets its own variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamValue {
    Null,
    Literal(String),
}

/// One bound parameter, as recovered from a log or paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundParam {
    /// 1-based parameter index, as logged.
    pub index: usize,
    /// The SQL/JDBC type name (`"INTEGER"`, `"VARCHAR"`, `"TIMESTAMP"`, …).
    /// Drives quoting in `subst` — numeric types are emitted bare.
    pub sql_type: String,
    pub value: ParamValue,
}

/// Which reconstruction source produced a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    HibernateLog,
    PostgresLog,
    JdbcPaste,
}

/// A statement recovered from a log or paste, with its parameters substituted
/// back in to produce `runnable_sql`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructedQuery {
    /// The statement with `?` / `$N` placeholders, as originally logged.
    pub raw_sql: String,
    pub params: Vec<BoundParam>,
    /// `raw_sql` with parameters substituted — ready to run.
    pub runnable_sql: String,
    pub source: Source,
    /// Line number in the source log/paste, for jump-back.
    pub src_line: usize,
}
