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
    // DQL / DML
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "WITH",
    "VALUES",
    // DDL
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "COMMENT",
    // Plan / inspection
    "EXPLAIN",
    "SHOW",
    "VACUUM",
    "ANALYZE",
    "REINDEX",
    "CLUSTER",
    // Session / transaction
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "SAVEPOINT",
    "RELEASE",
    "END",
    "SET",
    "RESET",
    // Permissions
    "GRANT",
    "REVOKE",
    // Misc
    "COPY",
    "LISTEN",
    "NOTIFY",
    "UNLISTEN",
    "CHECKPOINT",
];

/// Aggregate functions surfaced in `SelectList` (and `RETURNING`).
/// Inserts as `NAME(` so the cursor lands inside the paren ready for
/// arguments — see `query::complete::candidates_functions`.
///
/// To add a new aggregate (e.g. when Postgres adds one): append the
/// uppercase name here.
pub const AGGREGATE_FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "ARRAY_AGG",
    "STRING_AGG",
    "BOOL_AND",
    "BOOL_OR",
    "JSON_AGG",
    "JSONB_AGG",
    "JSON_OBJECT_AGG",
];

/// Scalar functions commonly used in SELECT lists / expressions —
/// COALESCE, NULLIF, string / date helpers etc. Same insertion shape
/// as aggregates (`NAME(`).
pub const SCALAR_FUNCTIONS: &[&str] = &[
    // Conditional / null handling
    "COALESCE",
    "NULLIF",
    "GREATEST",
    "LEAST",
    "CAST",
    // Time
    "NOW",
    "CURRENT_TIMESTAMP",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "EXTRACT",
    "DATE_TRUNC",
    "AGE",
    "TO_CHAR",
    "TO_DATE",
    "TO_TIMESTAMP",
    // String
    "LENGTH",
    "CHAR_LENGTH",
    "LOWER",
    "UPPER",
    "INITCAP",
    "TRIM",
    "LTRIM",
    "RTRIM",
    "BTRIM",
    "SUBSTRING",
    "POSITION",
    "REPLACE",
    "REGEXP_REPLACE",
    "REGEXP_MATCH",
    "REGEXP_MATCHES",
    "REGEXP_SPLIT_TO_ARRAY",
    "CONCAT",
    "CONCAT_WS",
    "FORMAT",
    "REPEAT",
    "REVERSE",
    "SPLIT_PART",
    // Numeric / math
    "ABS",
    "CEIL",
    "CEILING",
    "FLOOR",
    "ROUND",
    "TRUNC",
    "POWER",
    "SQRT",
    "MOD",
    "DIV",
    "RANDOM",
    // Array
    "ARRAY_LENGTH",
    "ARRAY_POSITION",
    "ARRAY_AGG",
    "UNNEST",
    "CARDINALITY",
    // JSON / JSONB
    "JSONB_BUILD_OBJECT",
    "JSON_BUILD_OBJECT",
    "JSONB_BUILD_ARRAY",
    "JSON_BUILD_ARRAY",
    "JSONB_OBJECT_KEYS",
    "JSONB_PATH_QUERY",
    "JSONB_SET",
    "TO_JSONB",
    "TO_JSON",
    // Postgres catalog / introspection — high-signal for ops queries
    "VERSION",
    "CURRENT_DATABASE",
    "CURRENT_SCHEMA",
    "CURRENT_USER",
    "SESSION_USER",
    "PG_BACKEND_PID",
    "PG_TYPEOF",
    "PG_RELATION_SIZE",
    "PG_TOTAL_RELATION_SIZE",
    "PG_SIZE_PRETTY",
    "PG_TABLE_SIZE",
    "PG_INDEXES_SIZE",
    "PG_DATABASE_SIZE",
    "TXID_CURRENT",
];

/// Window functions (the `OVER (...)` family). Same insertion as the
/// aggregates. Not yet differentiated from aggregates in completion —
/// the operator's intent (aggregate vs window) is determined by
/// whether they type `OVER` after the call, which we can't tell at
/// suggestion time.
pub const WINDOW_FUNCTIONS: &[&str] = &[
    "ROW_NUMBER",
    "RANK",
    "DENSE_RANK",
    "PERCENT_RANK",
    "CUME_DIST",
    "LAG",
    "LEAD",
    "FIRST_VALUE",
    "LAST_VALUE",
    "NTH_VALUE",
    "NTILE",
];

/// Word-shaped operators / connectives that the operator naturally
/// Tab-completes inside a `Predicate` context (WHERE / HAVING / ON).
/// Symbolic operators (`=`, `>`, `<>`, `!=`) are short enough that
/// suggesting them adds noise; they're left out deliberately.
///
/// Multi-word phrases (`IS NULL`, `IS NOT NULL`, `NOT IN`) are
/// emitted as single candidates so Tab once gets the whole shape.
pub const PREDICATE_OPERATORS: &[&str] = &[
    "AND",
    "OR",
    "NOT",
    "LIKE",
    "ILIKE",
    "IN",
    "BETWEEN",
    "EXISTS",
    "IS NULL",
    "IS NOT NULL",
    "NOT IN",
    "NOT LIKE",
    "NOT ILIKE",
    "IS DISTINCT FROM",
    "IS NOT DISTINCT FROM",
    "SIMILAR TO",
];

/// Keywords that complete a `DROP TABLE foo | …` / `DROP VIEW v | …`
/// statement. `IF EXISTS` actually goes BEFORE the name; the others
/// come after. Surfaced in `ClauseContext::DropTarget` alongside
/// table / schema names.
pub const DROP_CONTINUATIONS: &[&str] = &["IF EXISTS", "CASCADE", "RESTRICT"];

/// Postgres SQL type names — surfaced inside `CAST(expr AS |)` and
/// DDL column-type positions. Covers the common built-in types; add
/// to this list when you find yourself typing a missing one. Lowercase
/// because Postgres prints them lowercase (so cycling shows the value
/// you actually want to commit to disk).
pub const TYPE_NAMES: &[&str] = &[
    // Numeric
    "smallint",
    "integer",
    "bigint",
    "decimal",
    "numeric",
    "real",
    "double precision",
    "smallserial",
    "serial",
    "bigserial",
    "money",
    // Character
    "character varying",
    "varchar",
    "character",
    "char",
    "text",
    // Binary
    "bytea",
    // Date / time
    "timestamp",
    "timestamp with time zone",
    "timestamptz",
    "date",
    "time",
    "time with time zone",
    "timetz",
    "interval",
    // Boolean
    "boolean",
    "bool",
    // Enumerated / network / monetary / geometric (selected)
    "uuid",
    "inet",
    "cidr",
    "macaddr",
    "macaddr8",
    "point",
    "line",
    "lseg",
    "box",
    "path",
    "polygon",
    "circle",
    // JSON / XML
    "json",
    "jsonb",
    "xml",
    // Arrays — operator usually qualifies with [] manually
    // Range types
    "int4range",
    "int8range",
    "numrange",
    "tsrange",
    "tstzrange",
    "daterange",
    // Text search
    "tsvector",
    "tsquery",
    // Bit strings
    "bit",
    "bit varying",
];

/// Universal values usable on the right-hand side of `SET <param> = |`.
/// Boolean GUCs (enable_seqscan, log_duration, …) take `on` / `off` /
/// `true` / `false`. Any GUC accepts `default` to revert. String / enum
/// GUCs need per-parameter vocab not yet modeled — operators type the
/// value manually for those.
pub const GUC_VALUES: &[&str] = &["on", "off", "true", "false", "default"];

/// Common Postgres GUC (Grand Unified Configuration) parameter names.
/// Surfaced in `SHOW |` and `SET |` completion. Not exhaustive —
/// Postgres has hundreds of GUCs — but covers the ones a daily
/// operator routinely inspects / tweaks. Add a one-liner here when
/// you find yourself reaching for a missing one.
pub const GUC_PARAMETERS: &[&str] = &[
    // SHOW-only shorthand: `SHOW ALL` lists every GUC. `SET ALL` isn't
    // valid but the cost of offering ALL after SET is just one extra
    // Tab the operator skips past.
    "all",
    // Session basics
    "search_path",
    "timezone",
    "client_encoding",
    "client_min_messages",
    "datestyle",
    "intervalstyle",
    "application_name",
    "role",
    "session_user",
    // Transactions / isolation
    "default_transaction_isolation",
    "default_transaction_read_only",
    "default_transaction_deferrable",
    "transaction_isolation",
    "transaction_read_only",
    "transaction_deferrable",
    "idle_in_transaction_session_timeout",
    // Limits / safety
    "statement_timeout",
    "lock_timeout",
    "deadlock_timeout",
    "work_mem",
    "maintenance_work_mem",
    "temp_buffers",
    "max_connections",
    "max_parallel_workers",
    "max_parallel_workers_per_gather",
    "max_wal_size",
    "min_wal_size",
    // Planner
    "enable_seqscan",
    "enable_indexscan",
    "enable_bitmapscan",
    "enable_hashjoin",
    "enable_mergejoin",
    "enable_nestloop",
    "random_page_cost",
    "seq_page_cost",
    "cpu_tuple_cost",
    "effective_cache_size",
    "default_statistics_target",
    "join_collapse_limit",
    "from_collapse_limit",
    // Logging / observability
    "log_statement",
    "log_duration",
    "log_min_duration_statement",
    "log_lock_waits",
    "log_temp_files",
    // Server info (read-mostly, common in SHOW)
    "server_version",
    "server_version_num",
    "server_encoding",
    "data_directory",
    "config_file",
    "hba_file",
    "ident_file",
    "shared_buffers",
    "wal_level",
    "max_wal_senders",
    "synchronous_commit",
    "wal_compression",
];

/// Postgres `EXPLAIN (option, option, ...)` flags. Surfaced inside
/// the `EXPLAIN (...)` paren group via `ClauseContext::ExplainOptions`.
/// Each appears as a bare word; the operator follows with the value
/// (`ON` / `OFF` / `TEXT` / `JSON` etc.).
///
/// Refresh when Postgres adds a new EXPLAIN option (rare —
/// `pg_explain_options` doesn't exist, but Postgres release notes
/// flag any addition to this list).
pub const EXPLAIN_OPTIONS: &[&str] = &[
    "ANALYZE",
    "VERBOSE",
    "COSTS",
    "SETTINGS",
    "BUFFERS",
    "WAL",
    "TIMING",
    "SUMMARY",
    "FORMAT",
    "GENERIC_PLAN",
    "SERIALIZE",
    "MEMORY",
];

/// Postgres `VACUUM (option, ...) [table]` flags. Same shape as
/// `EXPLAIN (...)`. ANALYZE — the standalone statement — accepts a
/// subset of these in its own `(...)` option list, so the same
/// vocabulary is reused for both.
pub const VACUUM_OPTIONS: &[&str] = &[
    "FULL",
    "FREEZE",
    "VERBOSE",
    "ANALYZE",
    "DISABLE_PAGE_SKIPPING",
    "SKIP_LOCKED",
    "INDEX_CLEANUP",
    "PROCESS_TOAST",
    "TRUNCATE",
    "PARALLEL",
    "BUFFER_USAGE_LIMIT",
    "SKIP_DATABASE_STATS",
    "ONLY_DATABASE_STATS",
];

/// JOIN variants surfaced as multi-word completions in `TableRef`
/// continuations. Tab once gets the whole shape (`LEFT OUTER JOIN`)
/// so the operator doesn't have to type the verb-of-art.
pub const JOIN_VARIANTS: &[&str] = &[
    "JOIN",
    "INNER JOIN",
    "LEFT JOIN",
    "LEFT OUTER JOIN",
    "RIGHT JOIN",
    "RIGHT OUTER JOIN",
    "FULL JOIN",
    "FULL OUTER JOIN",
    "CROSS JOIN",
    "NATURAL JOIN",
    "LATERAL JOIN",
    "LEFT JOIN LATERAL",
    "INNER JOIN LATERAL",
];

/// "What clause keyword can follow this position." Drives the
/// continuation candidates added to each clause arm in `query::complete`.
/// Each list is sorted by typical-frequency (descending) so the
/// completion cycle prioritises the more common follow-up.
///
/// Adding support for a new clause keyword that should appear after
/// FROM (say `WINDOW`): append it here AND make sure `query::clause`
/// classifies the cursor correctly once it appears.
pub mod continuations {
    /// After SELECT-list — the natural next is FROM. Sub-selects
    /// already have their own scope so this fires only at top-level.
    pub const AFTER_SELECT_LIST: &[&str] = &["FROM"];

    /// After a FROM / JOIN clause — JOIN variants, WHERE, GROUP BY,
    /// ORDER BY, LIMIT, RETURNING (when inside an UPDATE / DELETE).
    pub const AFTER_TABLE_REF: &[&str] = &[
        "WHERE",
        "GROUP BY",
        "ORDER BY",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "FETCH FIRST",
        "RETURNING",
        "ON CONFLICT",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        // Aliasing keywords
        "AS",
    ];

    /// After WHERE / HAVING / ON — the typical next clauses.
    pub const AFTER_PREDICATE: &[&str] = &[
        "GROUP BY",
        "ORDER BY",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "RETURNING",
        "UNION",
        "INTERSECT",
        "EXCEPT",
    ];

    /// After ORDER BY / GROUP BY — pagination + RETURNING.
    pub const AFTER_ORDER_OR_GROUP: &[&str] =
        &["LIMIT", "OFFSET", "FETCH FIRST", "HAVING", "ORDER BY"];

    /// After UPDATE … SET col = … — the natural continuations.
    pub const AFTER_UPDATE_ASSIGN: &[&str] = &["WHERE", "RETURNING", "FROM"];

    /// After a `VALUES (...)` block in INSERT.
    pub const AFTER_VALUES: &[&str] = &["RETURNING", "ON CONFLICT"];
}

/// Flat "muscle memory" keyword list for [`crate::query::complete`]'s
/// generic top-level completion layer — the SQL keywords, clauses, and
/// a handful of common functions an operator reaches for constantly,
/// offered as `CandidateKind::Keyword` regardless of the fine-grained
/// clause classification. This is deliberately independent of (and
/// overlaps with) the narrower per-clause lists above: those give
/// precise, grammar-aware suggestions in their own position; this one
/// is the pgcli/psql-style catch-all so `sel|` always offers `SELECT`
/// even in a spot the clause grammar doesn't specially recognise.
///
/// Applied only to unqualified prefixes of 2+ characters, and always
/// ranked after whatever schema-derived candidates already matched —
/// see `query::complete::candidates_for`.
pub const TOP_LEVEL_KEYWORDS: &[&str] = &[
    // Query verbs / clauses
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT",
    "INNER",
    "ON",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "RETURNING",
    "WITH",
    "AS",
    // Predicate connectives
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS",
    "NULL",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "DISTINCT",
    "UNION",
    // CASE expression
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    // Plan / transaction / DDL
    "EXPLAIN",
    "ANALYZE",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "CREATE",
    "TABLE",
    "INDEX",
    "ALTER",
    "DROP",
    "TRUNCATE",
    // Common functions
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "NOW",
    "COALESCE",
    "NULLIF",
    "LOWER",
    "UPPER",
    "LENGTH",
    "DATE_TRUNC",
    "EXTRACT",
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
        // Keyword / function / operator tables are uppercase by
        // convention. GUC_PARAMETERS is intentionally lowercase (it
        // mirrors Postgres' own naming — `search_path`, not
        // `SEARCH_PATH`) so it gets its own no-uppercase test below.
        let uppercase_tables: &[&[&str]] = &[
            STATEMENT_KEYWORDS,
            AGGREGATE_FUNCTIONS,
            SCALAR_FUNCTIONS,
            WINDOW_FUNCTIONS,
            PREDICATE_OPERATORS,
            JOIN_VARIANTS,
            EXPLAIN_OPTIONS,
            VACUUM_OPTIONS,
            DROP_CONTINUATIONS,
            continuations::AFTER_SELECT_LIST,
            continuations::AFTER_TABLE_REF,
            continuations::AFTER_PREDICATE,
            continuations::AFTER_ORDER_OR_GROUP,
            continuations::AFTER_UPDATE_ASSIGN,
            continuations::AFTER_VALUES,
            TOP_LEVEL_KEYWORDS,
        ];
        for table in uppercase_tables {
            for word in *table {
                assert_eq!(
                    *word,
                    word.to_ascii_uppercase(),
                    "vocabulary entry {word:?} is not uppercase"
                );
                assert!(!word.is_empty(), "vocabulary entry is empty string");
            }
        }
        // GUCs are lowercase by Postgres convention.
        for word in GUC_PARAMETERS {
            assert_eq!(
                *word,
                word.to_ascii_lowercase(),
                "GUC entry {word:?} should be lowercase"
            );
            assert!(!word.is_empty());
        }
        // Type names are lowercase (Postgres prints them lowercase).
        for word in TYPE_NAMES {
            assert_eq!(
                *word,
                word.to_ascii_lowercase(),
                "type name {word:?} should be lowercase"
            );
            assert!(!word.is_empty());
        }
        // GUC values (`on`, `off`, `default`) — lowercase by SQL
        // convention.
        for word in GUC_VALUES {
            assert_eq!(
                *word,
                word.to_ascii_lowercase(),
                "GUC value {word:?} should be lowercase"
            );
            assert!(!word.is_empty());
        }
    }

    /// Contract: no duplicates within a single list. Duplicates would
    /// cycle past the same candidate twice in the completion popup.
    #[test]
    fn no_duplicates_within_a_list() {
        let labelled: &[(&str, &[&str])] = &[
            ("STATEMENT_KEYWORDS", STATEMENT_KEYWORDS),
            ("AGGREGATE_FUNCTIONS", AGGREGATE_FUNCTIONS),
            ("SCALAR_FUNCTIONS", SCALAR_FUNCTIONS),
            ("WINDOW_FUNCTIONS", WINDOW_FUNCTIONS),
            ("PREDICATE_OPERATORS", PREDICATE_OPERATORS),
            ("JOIN_VARIANTS", JOIN_VARIANTS),
            ("EXPLAIN_OPTIONS", EXPLAIN_OPTIONS),
            ("VACUUM_OPTIONS", VACUUM_OPTIONS),
            ("DROP_CONTINUATIONS", DROP_CONTINUATIONS),
            ("GUC_PARAMETERS", GUC_PARAMETERS),
            ("GUC_VALUES", GUC_VALUES),
            ("TYPE_NAMES", TYPE_NAMES),
            ("AFTER_SELECT_LIST", continuations::AFTER_SELECT_LIST),
            ("AFTER_TABLE_REF", continuations::AFTER_TABLE_REF),
            ("AFTER_PREDICATE", continuations::AFTER_PREDICATE),
            ("AFTER_ORDER_OR_GROUP", continuations::AFTER_ORDER_OR_GROUP),
            ("AFTER_UPDATE_ASSIGN", continuations::AFTER_UPDATE_ASSIGN),
            ("AFTER_VALUES", continuations::AFTER_VALUES),
            ("TOP_LEVEL_KEYWORDS", TOP_LEVEL_KEYWORDS),
        ];
        for (label, table) in labelled {
            let mut seen = std::collections::BTreeSet::new();
            for word in *table {
                assert!(seen.insert(*word), "{label} contains duplicate {word:?}");
            }
        }
    }
}
