//! Schema-quality checks (the "schema wizard" — `W` key).
//!
//! Pure analysis over the `SchemaCache` snapshot — no live queries
//! here, so the panel opens in microseconds and is safe against any
//! database (read-only). Each `Finding` carries:
//!
//!   - `severity` — colour-coded triage signal
//!   - `code` — short stable identifier (`LINT001`) so operators
//!     can reference / suppress specific checks later
//!   - `title` / `object` / `detail` — what / where / why
//!   - `suggestion` — optional remediation hint (an SQL snippet or
//!     a rule of thumb)
//!
//! Live-query checks (FK without index, unused indexes, bloated
//! tables, missing comments, table sizes, dead-tuple ratio) are
//! follow-ups — they need `pg_index` / `pg_stat_user_indexes` /
//! `pg_stat_user_tables` fetches that the cache doesn't carry yet.
//! Adding them is a separate vertical: a new async snapshot,
//! merged into the findings list.

use crate::query::schema::SchemaCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Almost certainly a bug or a soon-to-bite footgun.
    High,
    /// Operational pain or a frequent footgun.
    Medium,
    /// Style / consistency. No correctness impact.
    Low,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::High => "HIGH",
            Severity::Medium => "MED",
            Severity::Low => "LOW",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub code: &'static str,
    pub title: String,
    /// Fully-qualified object the finding applies to
    /// (`schema.table`, `schema.table.column`, or `schema`).
    pub object: String,
    pub detail: String,
    pub suggestion: Option<String>,
}

/// Run every check against `cache` and return findings, sorted
/// high → low severity then by object name for deterministic
/// ordering across runs.
pub fn run_all(cache: &SchemaCache) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    out.extend(tables_without_pk_or_unique(cache));
    out.extend(mixed_case_identifiers(cache));
    out.extend(reserved_keyword_identifiers(cache));
    out.extend(mixed_naming_within_schema(cache));
    out.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.code.cmp(b.code))
            .then_with(|| a.object.cmp(&b.object))
    });
    out
}

/// LINT001 — tables with no PRIMARY KEY or UNIQUE constraint.
/// Tables with no uniqueness contract can't be safely updated /
/// deduped by Postgres; they're frequently a forgotten constraint
/// rather than a deliberate "event log" shape.
pub fn tables_without_pk_or_unique(cache: &SchemaCache) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in &cache.tables {
        let has_constraint = cache.constraints.iter().any(|c| {
            c.schema.eq_ignore_ascii_case(&t.schema) && c.table.eq_ignore_ascii_case(&t.name)
        });
        if !has_constraint {
            out.push(Finding {
                severity: Severity::High,
                code: "LINT001",
                title: "table without PRIMARY KEY or UNIQUE constraint".to_string(),
                object: format!("{}.{}", t.schema, t.name),
                detail: format!(
                    "no UNIQUE / PRIMARY KEY constraint found for {}.{}",
                    t.schema, t.name
                ),
                suggestion: Some(format!(
                    "ALTER TABLE {}.{} ADD PRIMARY KEY (...);",
                    t.schema, t.name
                )),
            });
        }
    }
    out
}

/// LINT002 — identifiers with any uppercase letter. Postgres
/// folds unquoted identifiers to lowercase, so a mixed-case name
/// in the catalog means the identifier was created with `"..."`
/// and EVERY reference to it must also be `"..."`-quoted — a
/// persistent operational pain.
pub fn mixed_case_identifiers(cache: &SchemaCache) -> Vec<Finding> {
    let mut out = Vec::new();
    for schema in &cache.schemas {
        if has_uppercase(schema) {
            out.push(quote_finding("schema", schema, schema));
        }
    }
    for t in &cache.tables {
        if has_uppercase(&t.name) {
            let object = format!("{}.{}", t.schema, t.name);
            out.push(quote_finding("table", &object, &t.name));
        }
    }
    for ((schema, table), cols) in &cache.columns_by_table {
        for col in cols {
            if has_uppercase(col) {
                let object = format!("{schema}.{table}.{col}");
                out.push(quote_finding("column", &object, col));
            }
        }
    }
    out
}

fn quote_finding(kind: &'static str, object: &str, name: &str) -> Finding {
    Finding {
        severity: Severity::Medium,
        code: "LINT002",
        title: format!("mixed-case {kind} identifier needs `\"…\"` quoting"),
        object: object.to_string(),
        detail: format!(
            "`{name}` contains an uppercase letter — Postgres folds unquoted identifiers to lowercase, so every reference must use `\"{name}\"`",
        ),
        suggestion: None,
    }
}

/// LINT003 — identifier that matches a SQL reserved keyword.
/// Operators have to quote (or alias) every time, and tools that
/// regex-parse SQL frequently misclassify it.
pub fn reserved_keyword_identifiers(cache: &SchemaCache) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in &cache.tables {
        if is_reserved_keyword(&t.name) {
            out.push(reserved_finding(
                "table",
                &format!("{}.{}", t.schema, t.name),
                &t.name,
            ));
        }
    }
    for ((schema, table), cols) in &cache.columns_by_table {
        for col in cols {
            if is_reserved_keyword(col) {
                out.push(reserved_finding(
                    "column",
                    &format!("{schema}.{table}.{col}"),
                    col,
                ));
            }
        }
    }
    out
}

fn reserved_finding(kind: &'static str, object: &str, name: &str) -> Finding {
    Finding {
        severity: Severity::Medium,
        code: "LINT003",
        title: format!("{kind} named after a SQL reserved keyword"),
        object: object.to_string(),
        detail: format!(
            "`{name}` is a Postgres reserved keyword — references must be quoted (`\"{name}\"`) or aliased"
        ),
        suggestion: None,
    }
}

/// LINT004 — a single schema mixes `snake_case` and `camelCase`
/// (or `PascalCase`) table names. Pick a convention and stick
/// with it: mixing makes queries hard to autocomplete from
/// memory.
pub fn mixed_naming_within_schema(cache: &SchemaCache) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut by_schema: HashMap<&str, (Vec<&str>, Vec<&str>)> = HashMap::new();
    for t in &cache.tables {
        let entry = by_schema.entry(t.schema.as_str()).or_default();
        match name_convention(&t.name) {
            Convention::SnakeCase => entry.0.push(&t.name),
            Convention::MixedCase => entry.1.push(&t.name),
            Convention::Ambiguous => {} // single-word names fit both
        }
    }
    let mut out = Vec::new();
    for (schema, (snake, mixed)) in &by_schema {
        if !snake.is_empty() && !mixed.is_empty() {
            let mut snake_eg = snake.clone();
            snake_eg.sort();
            let mut mixed_eg = mixed.clone();
            mixed_eg.sort();
            let snake_one = snake_eg.first().copied().unwrap_or("");
            let mixed_one = mixed_eg.first().copied().unwrap_or("");
            out.push(Finding {
                severity: Severity::Low,
                code: "LINT004",
                title: "mixed naming conventions within a schema".to_string(),
                object: (*schema).to_string(),
                detail: format!(
                    "schema `{schema}` mixes snake_case (e.g. `{snake_one}`) and mixed-case (e.g. `{mixed_one}`) table names"
                ),
                suggestion: None,
            });
        }
    }
    out
}

fn has_uppercase(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_uppercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Convention {
    SnakeCase,
    MixedCase,
    /// Single-word lowercase (`users`, `events`) — fits either
    /// convention; we don't count it as evidence of either.
    Ambiguous,
}

fn name_convention(name: &str) -> Convention {
    if name.chars().any(|c| c.is_ascii_uppercase()) {
        Convention::MixedCase
    } else if name.contains('_') {
        Convention::SnakeCase
    } else {
        Convention::Ambiguous
    }
}

/// Catalog query for LINT101 (FK without leading-column index).
///
/// For each FK constraint, checks whether any index on the owning
/// table has the FK's first column as its leading index column.
/// Multi-column FKs are detected on their leading column only —
/// the practically-useful definition, since a `WHERE fk_a = ?`
/// lookup needs `fk_a` to be the leading column of some index to
/// benefit. Detecting the full `(a, b)` shape is a stretch goal.
pub const FK_WITHOUT_INDEX_SQL: &str = "\
SELECT \
    nsp.nspname AS schema, \
    rel.relname AS table_name, \
    c.conname AS constraint_name, \
    (SELECT string_agg(a.attname, ',' ORDER BY array_position(c.conkey, a.attnum)) \
     FROM pg_attribute a \
     WHERE a.attrelid = c.conrelid AND a.attnum = ANY(c.conkey) \
    ) AS columns \
FROM pg_constraint c \
JOIN pg_class rel ON rel.oid = c.conrelid \
JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace \
WHERE c.contype = 'f' \
  AND nsp.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
  AND NOT EXISTS ( \
    SELECT 1 FROM pg_index i \
    WHERE i.indrelid = c.conrelid \
      AND (i.indkey::int[])[1] = c.conkey[1] \
  ) \
ORDER BY 1, 2, 3";

/// Catalog query for LINT102 (unused indexes). Stats-dependent:
/// `idx_scan` accumulates since the last stats reset, so a
/// post-restart database will false-positive everything. UI
/// flags this with a "stats may be cold" suggestion line. The
/// query excludes indexes backing UNIQUE / PRIMARY KEY
/// constraints — those exist for correctness, not lookups.
pub const UNUSED_INDEXES_SQL: &str = "\
SELECT \
    nsp.nspname AS schema, \
    rel.relname AS table_name, \
    irel.relname AS index_name, \
    s.idx_scan \
FROM pg_stat_user_indexes s \
JOIN pg_class irel ON irel.oid = s.indexrelid \
JOIN pg_class rel ON rel.oid = s.relid \
JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace \
JOIN pg_index i ON i.indexrelid = s.indexrelid \
WHERE s.idx_scan = 0 \
  AND NOT i.indisunique \
  AND NOT i.indisprimary \
  AND nsp.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
ORDER BY 1, 2, 3";

/// Catalog query for LINT103 (overlapping indexes — same
/// `(table, indkey)` tuple appearing more than once). Strict
/// equality: an index on `(a, b)` and another on `(a)` are NOT
/// flagged (the latter has a legitimate use as a smaller
/// lookup); only fully-identical indkey vectors are dupes.
pub const DUPLICATE_INDEXES_SQL: &str = "\
SELECT \
    nsp.nspname AS schema, \
    rel.relname AS table_name, \
    string_agg(irel.relname, ',' ORDER BY irel.relname) AS index_names, \
    array_to_string(i.indkey::int[], ',') AS indkey_str \
FROM pg_index i \
JOIN pg_class irel ON irel.oid = i.indexrelid \
JOIN pg_class rel ON rel.oid = i.indrelid \
JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace \
WHERE nsp.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
GROUP BY nsp.nspname, rel.relname, rel.oid, i.indkey \
HAVING count(*) > 1 \
ORDER BY 1, 2";

/// Catalog query for LINT104 (table bloat). Stats-dependent:
/// the ratio reads `pg_stat_user_tables.n_dead_tup` against
/// `n_live_tup`. We surface tables with > 20% dead and at
/// least 1000 live rows (small tables hit the threshold too
/// easily on dev databases). Suggestion: `VACUUM (VERBOSE,
/// ANALYZE) <table>;`.
pub const BLOATED_TABLES_SQL: &str = "\
SELECT \
    s.schemaname AS schema, \
    s.relname AS table_name, \
    s.n_live_tup, \
    s.n_dead_tup \
FROM pg_stat_user_tables s \
WHERE s.n_live_tup >= 1000 \
  AND s.n_dead_tup::float / GREATEST(s.n_live_tup, 1) > 0.20 \
  AND s.schemaname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
ORDER BY s.n_dead_tup::float / GREATEST(s.n_live_tup, 1) DESC";

/// Catalog query for LINT105 (tables without a COMMENT). Style
/// nudge — uncommented tables make schema discovery harder for
/// new operators / query-builder tooling.
pub const TABLES_WITHOUT_COMMENT_SQL: &str = "\
SELECT \
    n.nspname AS schema, \
    c.relname AS table_name \
FROM pg_class c \
JOIN pg_namespace n ON n.oid = c.relnamespace \
WHERE c.relkind = 'r' \
  AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
  AND obj_description(c.oid, 'pg_class') IS NULL \
ORDER BY 1, 2";

/// Catalog query for LINT106 (schemas mixing `timestamp` (without
/// tz) and `timestamptz` columns). The classic "I forgot to add
/// `tz` in the migration" bug — at write time the missing
/// timezone gets converted at the session boundary and the
/// stored value can be off by hours.
pub const MIXED_TIMESTAMP_SQL: &str = "\
SELECT \
    nsp.nspname AS schema, \
    count(*) FILTER (WHERE t.typname = 'timestamp') AS timestamp_cols, \
    count(*) FILTER (WHERE t.typname = 'timestamptz') AS timestamptz_cols \
FROM pg_attribute a \
JOIN pg_type t ON t.oid = a.atttypid \
JOIN pg_class rel ON rel.oid = a.attrelid \
JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace \
WHERE rel.relkind = 'r' \
  AND a.attnum > 0 \
  AND NOT a.attisdropped \
  AND t.typname IN ('timestamp', 'timestamptz') \
  AND nsp.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
GROUP BY nsp.nspname \
HAVING count(*) FILTER (WHERE t.typname = 'timestamp') > 0 \
   AND count(*) FILTER (WHERE t.typname = 'timestamptz') > 0 \
ORDER BY 1";

/// Run every live-query lint check against `client` and return
/// the merged findings. Each check runs independently — one
/// failing (permission denied, missing stat view) does NOT kill
/// the others. Partial failures are logged via `tracing::warn`
/// and dropped from the return value; total failure (all checks
/// errored) returns Err so the App-side handler can surface it.
pub async fn fetch_live(client: &tokio_postgres::Client) -> Result<Vec<Finding>, String> {
    let mut out: Vec<Finding> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut ran = 0usize;

    ran += 1;
    match run_lint101(client).await {
        Ok(fs) => out.extend(fs),
        Err(e) => errors.push(format!("LINT101: {e}")),
    }
    ran += 1;
    match run_lint102(client).await {
        Ok(fs) => out.extend(fs),
        Err(e) => errors.push(format!("LINT102: {e}")),
    }
    ran += 1;
    match run_lint103(client).await {
        Ok(fs) => out.extend(fs),
        Err(e) => errors.push(format!("LINT103: {e}")),
    }
    ran += 1;
    match run_lint104(client).await {
        Ok(fs) => out.extend(fs),
        Err(e) => errors.push(format!("LINT104: {e}")),
    }
    ran += 1;
    match run_lint105(client).await {
        Ok(fs) => out.extend(fs),
        Err(e) => errors.push(format!("LINT105: {e}")),
    }
    ran += 1;
    match run_lint106(client).await {
        Ok(fs) => out.extend(fs),
        Err(e) => errors.push(format!("LINT106: {e}")),
    }

    if errors.len() == ran {
        // Every check failed — likely a session-level permission
        // issue. Propagate so the operator can act.
        return Err(errors.join("; "));
    }
    for e in &errors {
        tracing::warn!("schema lint partial: {e}");
    }
    Ok(out)
}

async fn run_lint101(client: &tokio_postgres::Client) -> Result<Vec<Finding>, String> {
    let rows = client
        .query(FK_WITHOUT_INDEX_SQL, &[])
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let schema: String = row.try_get::<usize, String>(0).map_err(|e| e.to_string())?;
        let table: String = row.try_get::<usize, String>(1).map_err(|e| e.to_string())?;
        let constraint: String = row.try_get::<usize, String>(2).map_err(|e| e.to_string())?;
        let columns: String = row.try_get::<usize, String>(3).map_err(|e| e.to_string())?;
        out.push(fk_without_index_finding(
            &schema,
            &table,
            &constraint,
            &columns,
        ));
    }
    Ok(out)
}

async fn run_lint102(client: &tokio_postgres::Client) -> Result<Vec<Finding>, String> {
    let rows = client
        .query(UNUSED_INDEXES_SQL, &[])
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let schema: String = row.try_get::<usize, String>(0).map_err(|e| e.to_string())?;
        let table: String = row.try_get::<usize, String>(1).map_err(|e| e.to_string())?;
        let index: String = row.try_get::<usize, String>(2).map_err(|e| e.to_string())?;
        out.push(unused_index_finding(&schema, &table, &index));
    }
    Ok(out)
}

async fn run_lint103(client: &tokio_postgres::Client) -> Result<Vec<Finding>, String> {
    let rows = client
        .query(DUPLICATE_INDEXES_SQL, &[])
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let schema: String = row.try_get::<usize, String>(0).map_err(|e| e.to_string())?;
        let table: String = row.try_get::<usize, String>(1).map_err(|e| e.to_string())?;
        let names: String = row.try_get::<usize, String>(2).map_err(|e| e.to_string())?;
        out.push(duplicate_index_finding(&schema, &table, &names));
    }
    Ok(out)
}

async fn run_lint104(client: &tokio_postgres::Client) -> Result<Vec<Finding>, String> {
    let rows = client
        .query(BLOATED_TABLES_SQL, &[])
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let schema: String = row.try_get::<usize, String>(0).map_err(|e| e.to_string())?;
        let table: String = row.try_get::<usize, String>(1).map_err(|e| e.to_string())?;
        let live: i64 = row.try_get::<usize, i64>(2).map_err(|e| e.to_string())?;
        let dead: i64 = row.try_get::<usize, i64>(3).map_err(|e| e.to_string())?;
        out.push(bloated_table_finding(&schema, &table, live, dead));
    }
    Ok(out)
}

async fn run_lint105(client: &tokio_postgres::Client) -> Result<Vec<Finding>, String> {
    let rows = client
        .query(TABLES_WITHOUT_COMMENT_SQL, &[])
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let schema: String = row.try_get::<usize, String>(0).map_err(|e| e.to_string())?;
        let table: String = row.try_get::<usize, String>(1).map_err(|e| e.to_string())?;
        out.push(missing_comment_finding(&schema, &table));
    }
    Ok(out)
}

async fn run_lint106(client: &tokio_postgres::Client) -> Result<Vec<Finding>, String> {
    let rows = client
        .query(MIXED_TIMESTAMP_SQL, &[])
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let schema: String = row.try_get::<usize, String>(0).map_err(|e| e.to_string())?;
        let ts: i64 = row.try_get::<usize, i64>(1).map_err(|e| e.to_string())?;
        let tstz: i64 = row.try_get::<usize, i64>(2).map_err(|e| e.to_string())?;
        out.push(mixed_timestamp_finding(&schema, ts, tstz));
    }
    Ok(out)
}

/// Pure builder for LINT102 (unused index) findings. Stats-cold
/// caveat is baked into the detail line so operators understand
/// the result is "trust me bro" until they confirm stats age.
pub fn unused_index_finding(schema: &str, table: &str, index: &str) -> Finding {
    Finding {
        severity: Severity::Medium,
        code: "LINT102",
        title: "index never scanned since last stats reset".to_string(),
        object: format!("{schema}.{table}.{index}"),
        detail: format!(
            "index `{index}` on `{schema}.{table}` has idx_scan = 0 in pg_stat_user_indexes — confirm the stats are warm (post-restart counters are zero too) before dropping"
        ),
        suggestion: Some(format!("DROP INDEX {schema}.{index};")),
    }
}

/// Pure builder for LINT103 (duplicate indexes — same column
/// tuple). `names` is a comma-joined list of the index names
/// (kept stable across runs by sorting in SQL).
pub fn duplicate_index_finding(schema: &str, table: &str, names: &str) -> Finding {
    let pretty = names.replace(',', ", ");
    Finding {
        severity: Severity::Medium,
        code: "LINT103",
        title: "duplicate indexes on the same columns".to_string(),
        object: format!("{schema}.{table}"),
        detail: format!(
            "indexes [{pretty}] on `{schema}.{table}` share the same column tuple — only one is doing work; drop the rest to reclaim write throughput"
        ),
        suggestion: None,
    }
}

/// Pure builder for LINT104 (bloated table). Renders the
/// dead-tuple ratio so the operator sees the magnitude — a
/// `0.42` ratio is hot, a `0.21` ratio is borderline.
pub fn bloated_table_finding(schema: &str, table: &str, live: i64, dead: i64) -> Finding {
    let ratio = dead as f64 / live.max(1) as f64;
    Finding {
        severity: Severity::Medium,
        code: "LINT104",
        title: "high dead-tuple ratio (table bloat)".to_string(),
        object: format!("{schema}.{table}"),
        detail: format!(
            "`{schema}.{table}` has {dead} dead tuples vs {live} live ({ratio:.2}× live) — autovacuum hasn't caught up; a manual VACUUM reclaims the space"
        ),
        suggestion: Some(format!("VACUUM (VERBOSE, ANALYZE) {schema}.{table};")),
    }
}

/// Pure builder for LINT105 (table without a `COMMENT ON`).
/// Style nudge — no SQL suggestion since the comment text has
/// to come from the operator.
pub fn missing_comment_finding(schema: &str, table: &str) -> Finding {
    Finding {
        severity: Severity::Low,
        code: "LINT105",
        title: "table without a COMMENT".to_string(),
        object: format!("{schema}.{table}"),
        detail: format!(
            "`{schema}.{table}` has no comment — operators discovering the schema have no in-band documentation"
        ),
        suggestion: Some(format!(
            "COMMENT ON TABLE {schema}.{table} IS '…';"
        )),
    }
}

/// Pure builder for LINT106 (schema mixes `timestamp` and
/// `timestamptz` columns). Counts surface so the operator can
/// triage which side is the minority.
pub fn mixed_timestamp_finding(
    schema: &str,
    timestamp_cols: i64,
    timestamptz_cols: i64,
) -> Finding {
    Finding {
        severity: Severity::High,
        code: "LINT106",
        title: "schema mixes `timestamp` and `timestamptz` columns".to_string(),
        object: schema.to_string(),
        detail: format!(
            "schema `{schema}` has {timestamp_cols} `timestamp` (no tz) and {timestamptz_cols} `timestamptz` column(s) — almost always a forgotten `WITH TIME ZONE` in a migration that silently corrupts stored values across sessions in different timezones"
        ),
        suggestion: None,
    }
}

/// Pure helper: build a LINT101 finding from one catalog row's
/// fields. Extracted from `fetch_live` so unit tests can pin the
/// rendering without needing a live database.
pub fn fk_without_index_finding(
    schema: &str,
    table: &str,
    constraint: &str,
    columns: &str,
) -> Finding {
    Finding {
        severity: Severity::High,
        code: "LINT101",
        title: "FK column not the leading column of any index".to_string(),
        object: format!("{schema}.{table}.{constraint}"),
        detail: format!(
            "constraint `{constraint}` on `{schema}.{table}` references column(s) ({columns}) — no index leads with the first FK column, so cascade / lookup queries will sequential-scan"
        ),
        suggestion: Some(format!(
            "CREATE INDEX ON {schema}.{table} ({columns});"
        )),
    }
}

/// Reserved-word set used by LINT003. Curated subset of the
/// `pg_get_keywords()` "reserved" / "reserved (can be function or
/// type name)" entries — the ones most likely to bite as a table
/// or column name. We pick what's actually pain-inducing rather
/// than enumerating the full list (which would include `analyse`
/// and friends nobody names a table after).
fn is_reserved_keyword(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        // Heavy hitters operators routinely use as column names.
        "user",
        "order",
        "group",
        "all",
        "case",
        "check",
        "column",
        "constraint",
        "default",
        "desc",
        "asc",
        "distinct",
        "do",
        "else",
        "end",
        "false",
        "for",
        "from",
        "grant",
        "having",
        "in",
        "into",
        "is",
        "join",
        "limit",
        "null",
        "on",
        "or",
        "primary",
        "references",
        "select",
        "table",
        "then",
        "to",
        "true",
        "union",
        "unique",
        "values",
        "when",
        "where",
        "window",
        "with",
        // Type names that double as keywords:
        "row",
        "current_date",
        "current_time",
        "current_user",
    ];
    let lower = name.to_ascii_lowercase();
    RESERVED.iter().any(|k| *k == lower)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::schema::{ConstraintMeta, SchemaCache, TableMeta};

    fn cache_with(tables: Vec<(&str, &str)>) -> SchemaCache {
        let mut c = SchemaCache::default();
        c.schemas = tables.iter().map(|(s, _)| s.to_string()).collect();
        c.schemas.sort();
        c.schemas.dedup();
        c.tables = tables
            .into_iter()
            .map(|(s, t)| TableMeta {
                schema: s.to_string(),
                name: t.to_string(),
            })
            .collect();
        c
    }

    #[test]
    fn tables_without_pk_or_unique_flags_constraint_free_tables() {
        let mut c = cache_with(vec![("public", "events"), ("public", "users")]);
        c.constraints.push(ConstraintMeta {
            schema: "public".into(),
            table: "users".into(),
            name: "users_pkey".into(),
        });
        let findings = tables_without_pk_or_unique(&c);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].object, "public.events");
        assert_eq!(findings[0].severity, Severity::High);
    }

    #[test]
    fn mixed_case_identifiers_flags_quoted_tables_and_columns() {
        let mut c = cache_with(vec![("public", "OrderItems")]);
        c.columns_by_table.insert(
            ("public".into(), "OrderItems".into()),
            vec!["createdAt".into(), "id".into()],
        );
        let findings = mixed_case_identifiers(&c);
        // OrderItems table + createdAt column → 2 findings.
        assert!(findings.iter().any(|f| f.object == "public.OrderItems"));
        assert!(findings
            .iter()
            .any(|f| f.object == "public.OrderItems.createdAt"));
        // `id` is all-lowercase — not flagged.
        assert!(!findings.iter().any(|f| f.object == "public.OrderItems.id"));
    }

    #[test]
    fn reserved_keyword_identifiers_flags_user_table_and_order_column() {
        let mut c = cache_with(vec![("public", "user"), ("public", "events")]);
        c.columns_by_table.insert(
            ("public".into(), "events".into()),
            vec!["order".into(), "kind".into()],
        );
        let findings = reserved_keyword_identifiers(&c);
        assert!(findings.iter().any(|f| f.object == "public.user"));
        assert!(findings.iter().any(|f| f.object == "public.events.order"));
        assert!(!findings.iter().any(|f| f.object == "public.events.kind"));
    }

    #[test]
    fn mixed_naming_within_schema_flags_camel_and_snake_mix() {
        let c = cache_with(vec![
            ("public", "user_accounts"),
            ("public", "OrderItems"),
            // Single-word names are ambiguous (could be either) —
            // they don't count as evidence of a convention.
            ("public", "users"),
        ]);
        let findings = mixed_naming_within_schema(&c);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].object, "public");
    }

    #[test]
    fn mixed_naming_does_not_fire_for_consistent_schema() {
        let c = cache_with(vec![
            ("public", "user_accounts"),
            ("public", "order_items"),
            ("public", "users"),
        ]);
        assert!(mixed_naming_within_schema(&c).is_empty());
    }

    #[test]
    fn run_all_sorts_high_severity_first() {
        let mut c = cache_with(vec![("public", "events"), ("public", "OrderItems")]);
        c.columns_by_table
            .insert(("public".into(), "OrderItems".into()), vec!["id".into()]);
        // events has no constraints (LINT001 High);
        // OrderItems is mixed-case (LINT002 Medium).
        let findings = run_all(&c);
        assert!(!findings.is_empty());
        // First finding must be the High one.
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].code, "LINT001");
    }

    #[test]
    fn fk_without_index_finding_carries_sql_suggestion() {
        let f = fk_without_index_finding("public", "orders", "orders_user_id_fkey", "user_id");
        assert_eq!(f.code, "LINT101");
        assert_eq!(f.severity, Severity::High);
        assert_eq!(f.object, "public.orders.orders_user_id_fkey");
        assert!(f.detail.contains("user_id"));
        let sql = f.suggestion.as_deref().unwrap_or("");
        assert!(sql.starts_with("CREATE INDEX ON public.orders"));
        assert!(sql.contains("user_id"));
    }

    #[test]
    fn unused_index_finding_carries_drop_suggestion_and_stats_caveat() {
        let f = unused_index_finding("public", "orders", "orders_status_idx");
        assert_eq!(f.code, "LINT102");
        assert_eq!(f.severity, Severity::Medium);
        assert_eq!(f.object, "public.orders.orders_status_idx");
        // Mentions the stats caveat so operators don't blindly drop.
        assert!(f.detail.contains("stats"));
        // Suggested DROP INDEX names the index with its schema.
        let sql = f.suggestion.as_deref().unwrap_or("");
        assert!(sql.starts_with("DROP INDEX public.orders_status_idx"));
    }

    #[test]
    fn duplicate_index_finding_pretty_prints_name_list() {
        let f = duplicate_index_finding("public", "users", "users_email_idx,users_email_idx2");
        assert_eq!(f.code, "LINT103");
        assert_eq!(f.severity, Severity::Medium);
        // Names joined with ", " (with a space) for readability.
        assert!(f.detail.contains("[users_email_idx, users_email_idx2]"));
        // No SQL suggestion — choosing which one to drop is a
        // judgement call, not a mechanical fix.
        assert!(f.suggestion.is_none());
    }

    #[test]
    fn bloated_table_finding_carries_vacuum_suggestion_with_ratio() {
        let f = bloated_table_finding("public", "events", 10_000, 4_200);
        assert_eq!(f.code, "LINT104");
        assert_eq!(f.severity, Severity::Medium);
        assert_eq!(f.object, "public.events");
        // Ratio 4200/10000 = 0.42.
        assert!(f.detail.contains("0.42"));
        assert!(f.detail.contains("4200"));
        assert!(f.detail.contains("10000"));
        let sql = f.suggestion.as_deref().unwrap_or("");
        assert!(sql.contains("VACUUM"));
        assert!(sql.contains("public.events"));
    }

    #[test]
    fn missing_comment_finding_is_low_severity_with_template_sql() {
        let f = missing_comment_finding("public", "users");
        assert_eq!(f.code, "LINT105");
        assert_eq!(f.severity, Severity::Low);
        assert_eq!(f.object, "public.users");
        let sql = f.suggestion.as_deref().unwrap_or("");
        assert!(sql.starts_with("COMMENT ON TABLE public.users"));
    }

    #[test]
    fn lint104_sql_filters_small_tables_and_low_ratios() {
        // Sanity-check the SQL has the live-row floor and the
        // ratio threshold so dev databases don't drown the panel
        // in tiny-table noise.
        assert!(BLOATED_TABLES_SQL.contains("n_live_tup >= 1000"));
        assert!(BLOATED_TABLES_SQL.contains("> 0.20"));
    }

    #[test]
    fn lint105_sql_uses_obj_description() {
        assert!(TABLES_WITHOUT_COMMENT_SQL.contains("obj_description"));
        assert!(TABLES_WITHOUT_COMMENT_SQL.contains("IS NULL"));
    }

    #[test]
    fn mixed_timestamp_finding_shows_both_counts() {
        let f = mixed_timestamp_finding("public", 3, 7);
        assert_eq!(f.code, "LINT106");
        assert_eq!(f.severity, Severity::High);
        assert_eq!(f.object, "public");
        assert!(f.detail.contains("3"));
        assert!(f.detail.contains("7"));
    }

    #[test]
    fn live_lint_sql_constants_reference_expected_catalog_objects() {
        // Spot-check each constant for the catalog objects it
        // joins on — guards against accidental edits dropping a
        // crucial JOIN. Not a substitute for an integration test
        // against a real DB; just a fence against typos.
        assert!(FK_WITHOUT_INDEX_SQL.contains("pg_constraint"));
        assert!(FK_WITHOUT_INDEX_SQL.contains("pg_index"));
        assert!(UNUSED_INDEXES_SQL.contains("pg_stat_user_indexes"));
        assert!(UNUSED_INDEXES_SQL.contains("indisunique"));
        assert!(DUPLICATE_INDEXES_SQL.contains("HAVING count(*) > 1"));
        assert!(MIXED_TIMESTAMP_SQL.contains("'timestamp'"));
        assert!(MIXED_TIMESTAMP_SQL.contains("'timestamptz'"));
    }

    #[test]
    fn fk_without_index_finding_handles_multi_column() {
        let f = fk_without_index_finding(
            "public",
            "line_items",
            "line_items_order_fkey",
            "order_id,sku",
        );
        // Multi-column FK → suggested index uses the same column
        // tuple in the same order.
        let sql = f.suggestion.as_deref().unwrap_or("");
        assert!(
            sql.contains("(order_id,sku)"),
            "expected multi-col index suggestion; got: {sql}"
        );
    }

    #[test]
    fn name_convention_classifies_three_buckets() {
        assert_eq!(name_convention("user_accounts"), Convention::SnakeCase);
        assert_eq!(name_convention("OrderItems"), Convention::MixedCase);
        assert_eq!(name_convention("users"), Convention::Ambiguous);
        assert_eq!(name_convention("user"), Convention::Ambiguous);
    }
}
