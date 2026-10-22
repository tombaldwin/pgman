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

impl Source {
    /// The word the UI uses for this source: the picker's `[tag]` and
    /// the editor's "looks like a … log" hint both say it, so the two
    /// name the same thing the same way.
    pub fn label(self) -> &'static str {
        match self {
            Source::HibernateLog => "hibernate",
            Source::PostgresLog => "postgres",
            Source::JdbcPaste => "jdbc",
        }
    }
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
    /// The statement failed where it was logged: the server's error
    /// message (a Postgres log's `ERROR:` + `STATEMENT:` pair). `None`
    /// for a statement that ran, or whose outcome the log doesn't say.
    pub error: Option<String>,
}
