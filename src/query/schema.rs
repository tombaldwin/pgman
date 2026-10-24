//! Schema cache backing identifier completion.
//!
//! Built once at connect time (best-effort — a permission error means an
//! empty cache, which just disables completion rather than killing the
//! session). Holds enough to answer:
//!
//! - all visible tables / views (for unqualified completion + `FROM …`),
//! - all columns of a given table (for `alias.col` and FROM-aware
//!   unqualified completion),
//! - all schemas (for `schema.|` completion in a later pass).
//!
//! Pure data shape here. The fetch helper lives below the struct and is
//! the only `async` bit — keep it thin.

use std::collections::HashMap;
use std::sync::Arc;

/// One qualified-by-schema table or view.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableMeta {
    pub schema: String,
    pub name: String,
}

/// One foreign-key edge: a column in `(child_schema, child_table)`
/// references a column in `(parent_schema, parent_table)`.
/// Multi-column FKs become multiple `FkEdge` rows (one per
/// column pair) — keeps the join-on-(table, col) lookup the FK-
/// nav UI does dirt-cheap. Self-referential FKs are fine; the
/// child and parent triples are simply equal in that case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FkEdge {
    pub child_schema: String,
    pub child_table: String,
    pub child_column: String,
    pub parent_schema: String,
    pub parent_table: String,
    pub parent_column: String,
}

/// Size info for one table, in bytes. `table_bytes` is just
/// the heap; `total_bytes` includes indexes + toast + free-space
/// map. Surfaced in the schema browser's Table detail pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TableSize {
    pub table_bytes: u64,
    pub total_bytes: u64,
}

/// Per-column metadata captured by the schema fetch: the
/// pretty-printed type (`format_type(atttypid, atttypmod)` —
/// includes typmod so `varchar(50)` shows length, `numeric(10,2)`
/// shows precision/scale) and the `NOT NULL` flag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
    pub not_null: bool,
    /// `pg_attribute.attnum`: what a result column's origin
    /// (`grid::ColumnOrigin`) names the column by. Gaps where columns
    /// were dropped, so not the position in the list.
    pub attnum: i16,
}

/// A unique / primary-key constraint — what `ON CONFLICT ON
/// CONSTRAINT name` can name. The owning table is captured so
/// completion can scope to the write target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstraintMeta {
    pub schema: String,
    pub table: String,
    pub name: String,
}

/// One index on a table, as the schema browser lists it under the
/// table. `definition` is `pg_get_indexdef` — the `CREATE INDEX`
/// statement that rebuilds it — so the detail pane shows the columns,
/// method and predicate without a second query.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IndexMeta {
    pub name: String,
    pub definition: String,
    pub unique: bool,
    pub primary: bool,
}

/// Snapshot of the live database catalog used by completion. Empty is a
/// valid state (means "no info; offer nothing"); built once and only
/// replaced on explicit refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaCache {
    /// Distinct schema names found, sorted.
    pub schemas: Vec<String>,
    /// All visible tables / views / matviews / partitioned tables /
    /// foreign tables, sorted by (schema, name).
    pub tables: Vec<TableMeta>,
    /// All visible sequences (`pg_class.relkind = 'S'`), sorted by
    /// (schema, name). Used for `DROP SEQUENCE` completion + future
    /// `nextval('|')` literal-context completion.
    pub sequences: Vec<TableMeta>,
    /// All visible indexes (`pg_class.relkind = 'i'`), sorted by
    /// (schema, name). Used for `DROP INDEX` / `REINDEX INDEX`.
    pub indexes: Vec<TableMeta>,
    /// Column names per (schema, table). Order = pg attnum, so it mirrors
    /// what `SELECT *` would expose.
    pub columns_by_table: HashMap<(String, String), Vec<String>>,
    /// Per-column metadata (type name + NOT NULL flag) keyed the
    /// same way as `columns_by_table`. Populated alongside it; the
    /// schema browser uses this to render `· id : integer NOT
    /// NULL`. Kept separate so the simpler `columns_by_table`
    /// still drives identifier completion without dragging the
    /// extra fields through hot paths.
    pub columns_meta_by_table: HashMap<(String, String), Vec<ColumnMeta>>,
    /// Table sizes keyed by (schema, table). `None`-equivalent
    /// (missing entry) when the catalog fetch can't read the
    /// size (rare — `pg_relation_size` is generally available).
    pub table_sizes: HashMap<(String, String), TableSize>,
    /// FK-edge index — one row per column pair. Used by FK
    /// navigation (`F` in Normal mode) to find the target of a
    /// "click-through" to the parent row.
    pub fk_edges: Vec<FkEdge>,
    /// Unique / primary-key constraints (the kinds `ON CONFLICT ON
    /// CONSTRAINT` can name). Fetched from `pg_constraint` separately
    /// from the main relation query.
    pub constraints: Vec<ConstraintMeta>,
    /// Each partition's direct parent, keyed `(schema, partition)`.
    /// The schema browser folds partitions under their table.
    pub partition_parent: HashMap<(String, String), (String, String)>,
    /// Each table's indexes, keyed `(schema, table)`, sorted by name.
    /// `indexes` above has every index's name for completion; this has
    /// which table each belongs to, for the schema browser.
    pub indexes_by_table: HashMap<(String, String), Vec<IndexMeta>>,
    /// The entries of `tables` that are views, materialized views or
    /// foreign tables, keyed `(schema, name)`: relations that can carry
    /// no primary key, so the schema wizard doesn't ask them for one.
    pub views: std::collections::HashSet<(String, String)>,
    /// Each table's primary-key columns in key order, keyed `(schema,
    /// table)`. No entry: the table has no primary key (or the fetch
    /// failed). Cell editing and the result diff key rows on these.
    pub primary_keys: HashMap<(String, String), Vec<String>>,
    /// Every relation in `tables` by its `pg_class.oid`: a result
    /// column's origin names its table by OID, and this is how that
    /// becomes a name.
    pub relations_by_oid: HashMap<u32, (String, String)>,
    /// Why the cache is missing parts, when it is: another session's
    /// `ACCESS EXCLUSIVE` lock kept a relation's size and indexes out
    /// (the fetch skips what such a lock covers rather than queue
    /// behind it), or a catalog read waited past `lock_timeout`. One
    /// line for the status bar; `None` when the fetch was whole.
    pub incomplete: Option<String>,
}

/// A relation another backend holds, or waits for, an `ACCESS
/// EXCLUSIVE` lock on: reading its size or index definitions would
/// queue behind that lock ([`LOCKED_RELATIONS_SQL`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedRelation {
    pub schema: String,
    pub name: String,
    pub pid: i32,
    /// Held (`true`) or still waited for (`false`) — a waiting request
    /// blocks every later reader just the same.
    pub granted: bool,
}

/// [`SchemaCache::incomplete`]'s line from what the fetch skipped
/// (`locked`, held locks first) and which catalog reads gave up at
/// `lock_timeout` (`timed_out`, named by the parts they would have
/// filled). Pure.
pub fn incomplete_reason(locked: &[LockedRelation], timed_out: &[&str]) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(first) = locked.first() {
        let rel = format!("{}.{}", first.schema, first.name);
        let more = match locked.len() - 1 {
            0 => String::new(),
            n => format!(" (and {n} more)"),
        };
        let how = if first.granted { "held" } else { "awaited" };
        parts.push(format!(
            "ACCESS EXCLUSIVE lock on {rel}{more} {how} by pid {}: no size or indexes",
            first.pid
        ));
    }
    if !timed_out.is_empty() {
        parts.push(format!("{} gave up at lock_timeout", timed_out.join(", ")));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// pgman's one size formatter — database sizes on the start card and in
/// the environments overview, table sizes in the schema browser. Units
/// as `pg_size_pretty` spells them (`bytes`, `kB`, `MB`, `GB`, `TB`,
/// 1024-based), but always the largest unit the size reaches: one
/// decimal under ten (`9.0 MB`), whole numbers from ten (`212 MB`).
/// `pg_size_pretty` itself stays in kB up to 10 MB, so one database
/// read `10199 kB` beside another's `10 MB`. Pure.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    // Rounding can carry a value up to the next unit: 1023.7 kB is 1.0 MB.
    if value.round() >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 9.95 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// A size cell as the server sent it, through [`format_bytes`] when it
/// is a byte count; anything else as it came. Pure.
pub fn format_size_cell(cell: &str) -> String {
    cell.trim()
        .parse::<u64>()
        .map(format_bytes)
        .unwrap_or_else(|_| cell.to_string())
}

impl SchemaCache {
    /// The table a partition ultimately belongs to — its parent's
    /// parent, and so on — or `None` when it isn't a partition.
    pub fn partition_root(&self, schema: &str, name: &str) -> Option<(String, String)> {
        let mut at = self
            .partition_parent
            .get(&(schema.to_string(), name.to_string()))?
            .clone();
        // Bounded: a cycle can't come out of pg_inherits, but a cache
        // built by hand could hold one.
        for _ in 0..64 {
            match self.partition_parent.get(&at) {
                Some(up) => at = up.clone(),
                None => break,
            }
        }
        Some(at)
    }

    /// Every partition, at any depth, of `(schema, table)`, sorted.
    pub fn partitions_of(&self, schema: &str, table: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .partition_parent
            .keys()
            .filter(|(s, n)| {
                self.partition_root(s, n)
                    .as_ref()
                    .map(|(rs, rt)| (rs.as_str(), rt.as_str()))
                    == Some((schema, table))
            })
            .cloned()
            .collect();
        out.sort();
        out
    }

    /// True when there's nothing to complete against — used by callers
    /// that want to skip the candidate computation early.
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// Look up the FK edge a `(schema, table, column)` triple
    /// participates in as the CHILD side. `None` when the column
    /// isn't an FK. Case-insensitive (matches how the rest of
    /// the cache resolves identifiers).
    pub fn fk_edge_for_child(&self, schema: &str, table: &str, column: &str) -> Option<&FkEdge> {
        // An exact match first: the grid's source is the server's own
        // name (`"PgRevMixed"` stays mixed-case), and a lowercase twin
        // `pgrevmixed` must not win just by sorting first.
        if let Some(e) = self.fk_edges.iter().find(|e| {
            e.child_schema == schema && e.child_table == table && e.child_column == column
        }) {
            return Some(e);
        }
        self.fk_edges.iter().find(|e| {
            e.child_schema.eq_ignore_ascii_case(schema)
                && e.child_table.eq_ignore_ascii_case(table)
                && e.child_column.eq_ignore_ascii_case(column)
        })
    }

    /// The primary key's columns of `(schema, table)` in key order —
    /// the server's own names, matched exactly (`"Users"` and `users`
    /// are different tables). `None`: no primary key.
    pub fn primary_key(&self, schema: &str, table: &str) -> Option<&[String]> {
        self.primary_keys
            .get(&(schema.to_string(), table.to_string()))
            .map(Vec::as_slice)
    }

    /// The column of `(schema, table)` whose `attnum` is `attnum`.
    pub fn column_by_attnum(&self, schema: &str, table: &str, attnum: i16) -> Option<&ColumnMeta> {
        self.columns_meta_by_table
            .get(&(schema.to_string(), table.to_string()))?
            .iter()
            .find(|c| c.attnum == attnum)
    }

    /// All distinct column names across every table, sorted.
    /// Useful for the unqualified-no-FROM fallback ("offer everything").
    pub fn all_column_names(&self) -> Vec<String> {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for cols in self.columns_by_table.values() {
            for c in cols {
                seen.insert(c.clone());
            }
        }
        seen.into_iter().collect()
    }

    /// Look up the column list for a (schema, table) pair. The schema can
    /// be `None` — in that case the first table matching `name` (across
    /// any schema) is returned, which matches how Postgres resolves
    /// unqualified names via `search_path`.
    pub fn columns_for(&self, schema: Option<&str>, name: &str) -> Option<&Vec<String>> {
        if let Some(s) = schema {
            // Case-insensitive: typed identifiers are folded to match
            // unquoted-Postgres behaviour. Quoted-case-sensitive lookups
            // are a v2 concern.
            for t in &self.tables {
                if t.schema.eq_ignore_ascii_case(s) && t.name.eq_ignore_ascii_case(name) {
                    return self
                        .columns_by_table
                        .get(&(t.schema.clone(), t.name.clone()));
                }
            }
            return None;
        }
        // Schema not specified: scan tables in their sorted order and
        // return the first hit. (search_path-style behavior would prefer
        // 'public' / first-on-path; we don't know the user's path so
        // sorted order is a reasonable proxy.)
        for t in &self.tables {
            if t.name.eq_ignore_ascii_case(name) {
                return self
                    .columns_by_table
                    .get(&(t.schema.clone(), t.name.clone()));
            }
        }
        None
    }
}

/// Query that powers `fetch`. Public so a future "show me the cache" UI
/// can quote it / re-run it. Excludes system schemas; includes regular
/// tables (`r`), views (`v`), materialized views (`m`), partitioned
/// tables (`p`), foreign tables (`f`), sequences (`S`), and indexes
/// (`i`). The `relkind` column is included so the post-processor can
/// shard rows into the right bucket on `SchemaCache`.
pub const SCHEMA_SQL: &str = "\
SELECT n.nspname, c.relname, c.relkind, a.attname, \
       format_type(a.atttypid, a.atttypmod), \
       a.attnotnull, c.oid, a.attnum \
FROM pg_catalog.pg_class c \
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
LEFT JOIN pg_catalog.pg_attribute a \
       ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
WHERE c.relkind = ANY ('{r,v,m,p,f,S,i}') \
  AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
ORDER BY n.nspname, c.relname, a.attnum";

/// Query that fetches FK edges (one row per column pair). Joins
/// `pg_constraint` (type 'f') with `pg_attribute` on both sides
/// using `array_position(conkey, attnum)` so the column order is
/// preserved across multi-column FKs.
/// Every partition and its direct parent.
pub const PARTITIONS_SQL: &str = "\
SELECT cn.nspname, c.relname, pn.nspname, p.relname \
FROM pg_inherits i \
JOIN pg_class c ON c.oid = i.inhrelid \
JOIN pg_namespace cn ON cn.oid = c.relnamespace \
JOIN pg_class p ON p.oid = i.inhparent \
JOIN pg_namespace pn ON pn.oid = p.relnamespace \
WHERE c.relispartition";

pub const FK_EDGES_SQL: &str = "\
SELECT \
    nsp.nspname AS child_schema, \
    rel.relname AS child_table, \
    ca.attname AS child_column, \
    fnsp.nspname AS parent_schema, \
    frel.relname AS parent_table, \
    pa.attname AS parent_column \
FROM pg_constraint c \
JOIN pg_class rel ON rel.oid = c.conrelid \
JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace \
JOIN pg_class frel ON frel.oid = c.confrelid \
JOIN pg_namespace fnsp ON fnsp.oid = frel.relnamespace \
JOIN LATERAL unnest(c.conkey) WITH ORDINALITY AS ck(attnum, ord) ON true \
JOIN LATERAL unnest(c.confkey) WITH ORDINALITY AS pk(attnum, ord) ON ck.ord = pk.ord \
JOIN pg_attribute ca ON ca.attrelid = c.conrelid AND ca.attnum = ck.attnum \
JOIN pg_attribute pa ON pa.attrelid = c.confrelid AND pa.attnum = pk.attnum \
WHERE c.contype = 'f' \
  AND nsp.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
ORDER BY 1, 2, 3";

/// `locked`: the relations in this database another backend holds or
/// waits for an `ACCESS EXCLUSIVE` lock on (`IS DISTINCT FROM`: a
/// prepared transaction's locks have no pid). A CTE, so `pg_locks` is
/// read once per statement rather than once per row.
macro_rules! locked_cte {
    () => {
        "WITH locked AS (\
SELECT l.relation FROM pg_catalog.pg_locks l \
WHERE l.locktype = 'relation' AND l.mode = 'AccessExclusiveLock' \
  AND l.database = (SELECT oid FROM pg_catalog.pg_database \
                    WHERE datname = pg_catalog.current_database()) \
  AND l.pid IS DISTINCT FROM pg_catalog.pg_backend_pid()) "
    };
}

/// Query that fetches per-table sizes (heap + total including
/// indexes/toast). Run as a third pass so a permission gap on
/// `pg_relation_size` doesn't kill the cache build.
///
/// Sizing a relation takes an `ACCESS SHARE` lock on it, its TOAST
/// table and its indexes, and that queues behind another session's
/// `ACCESS EXCLUSIVE` (held or waited for): a table under one is
/// skipped (`locked` — see [`LOCKED_RELATIONS_SQL`]) rather than hang
/// the connect, and every backend queued behind it.
pub const TABLE_SIZES_SQL: &str = concat!(
    locked_cte!(),
    "\
SELECT n.nspname, c.relname, \
       pg_catalog.pg_relation_size(c.oid)::bigint AS table_bytes, \
       pg_catalog.pg_total_relation_size(c.oid)::bigint AS total_bytes \
FROM pg_catalog.pg_class c \
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
WHERE c.relkind = 'r' \
  AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
  AND c.oid NOT IN (SELECT relation FROM locked) \
  AND c.reltoastrelid NOT IN (SELECT relation FROM locked) \
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_index ix \
                  WHERE ix.indrelid = c.oid \
                    AND ix.indexrelid IN (SELECT relation FROM locked))"
);

/// Every index with the table it's on (`pg_index.indrelid`), its
/// definition, and whether it is unique / the primary key. Like
/// [`TABLE_SIZES_SQL`]'s, its `pg_get_indexdef` queues behind an
/// `ACCESS EXCLUSIVE` lock, so an index or table under one is skipped.
pub const TABLE_INDEXES_SQL: &str = concat!(
    locked_cte!(),
    "\
SELECT n.nspname, t.relname, i.relname, \
       pg_catalog.pg_get_indexdef(x.indexrelid), x.indisunique, x.indisprimary \
FROM pg_catalog.pg_index x \
JOIN pg_catalog.pg_class i ON i.oid = x.indexrelid \
JOIN pg_catalog.pg_class t ON t.oid = x.indrelid \
JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
WHERE n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
  AND x.indrelid NOT IN (SELECT relation FROM locked) \
  AND x.indexrelid NOT IN (SELECT relation FROM locked) \
ORDER BY 1, 2, 3"
);

/// The relations [`TABLE_SIZES_SQL`] and [`TABLE_INDEXES_SQL`] skip,
/// and who holds (or waits for) the lock: what
/// [`SchemaCache::incomplete`] names. Tables, matviews and indexes in
/// the operator's schemas only — a lock on a catalog stalls every
/// read, and the connect's `lock_timeout` is what bounds that.
pub const LOCKED_RELATIONS_SQL: &str = "\
SELECT n.nspname, c.relname, l.pid, l.granted \
FROM pg_catalog.pg_locks l \
JOIN pg_catalog.pg_class c ON c.oid = l.relation \
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
WHERE l.locktype = 'relation' AND l.mode = 'AccessExclusiveLock' \
  AND l.database = (SELECT oid FROM pg_catalog.pg_database \
                    WHERE datname = pg_catalog.current_database()) \
  AND l.pid IS DISTINCT FROM pg_catalog.pg_backend_pid() \
  AND c.relkind = ANY ('{r,p,m,i}') \
  AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
ORDER BY l.granted DESC, 1, 2, 3";

/// Every primary key's columns, one row per column in key order
/// (`indkey` unnested with its ordinality). `pg_index.indisprimary`
/// rather than `pg_constraint`, so a partitioned table's key (an index
/// on the parent) counts too.
pub const PRIMARY_KEYS_SQL: &str = "\
SELECT n.nspname, t.relname, a.attname \
FROM pg_catalog.pg_index x \
JOIN pg_catalog.pg_class t ON t.oid = x.indrelid \
JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
JOIN LATERAL unnest(x.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord) ON true \
JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
WHERE x.indisprimary \
  AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
ORDER BY 1, 2, k.ord";

/// Query that fetches unique + primary-key constraint names, scoped
/// by their owning table. Run separately from `SCHEMA_SQL` so a
/// permission gap on `pg_constraint` (rare) doesn't take down the
/// whole cache build.
pub const CONSTRAINTS_SQL: &str = "\
SELECT n.nspname, t.relname, c.conname \
FROM pg_catalog.pg_constraint c \
JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
WHERE c.contype = ANY ('{u,p}') \
  AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
ORDER BY n.nspname, t.relname, c.conname";

/// SQLSTATE `55P03` (`lock_not_available`): the statement gave up at
/// `lock_timeout`.
fn is_lock_timeout(e: &tokio_postgres::Error) -> bool {
    e.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)
}

/// Run the cache-building query on `client` and assemble a `SchemaCache`.
/// Returns an empty cache (and logs a warning) if the query fails — the
/// rest of pgman keeps working without completion.
pub async fn fetch(client: &Arc<tokio_postgres::Client>) -> SchemaCache {
    // Issue the four independent catalog queries concurrently — tokio_postgres
    // pipelines them on the single connection, collapsing four serial round
    // trips into roughly one. Each stays best-effort: a permission gap on one
    // (e.g. pg_constraint) degrades that feature without killing the cache.
    let (schema_res, constraint_res, size_res, fk_res, part_res, index_res, pk_res, locked_res) = tokio::join!(
        client.query(SCHEMA_SQL, &[]),
        client.query(CONSTRAINTS_SQL, &[]),
        client.query(TABLE_SIZES_SQL, &[]),
        client.query(FK_EDGES_SQL, &[]),
        client.query(PARTITIONS_SQL, &[]),
        client.query(TABLE_INDEXES_SQL, &[]),
        client.query(PRIMARY_KEYS_SQL, &[]),
        client.query(LOCKED_RELATIONS_SQL, &[]),
    );
    // What a lock kept out: the relations the sizes and indexes skipped,
    // and any read that gave up at the connect's `lock_timeout`.
    let locked: Vec<LockedRelation> = match &locked_res {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| {
                Some(LockedRelation {
                    schema: row.try_get(0).ok()?,
                    name: row.try_get(1).ok()?,
                    pid: row.try_get(2).ok()?,
                    granted: row.try_get(3).ok()?,
                })
            })
            .collect(),
        Err(e) => {
            tracing::warn!("lock lookup failed: {e}");
            Vec::new()
        }
    };
    let timed_out: Vec<&str> = [
        ("columns", schema_res.as_ref().err()),
        ("constraints", constraint_res.as_ref().err()),
        ("table sizes", size_res.as_ref().err()),
        ("foreign keys", fk_res.as_ref().err()),
        ("partitions", part_res.as_ref().err()),
        ("indexes", index_res.as_ref().err()),
        ("primary keys", pk_res.as_ref().err()),
    ]
    .into_iter()
    .filter(|(_, e)| e.is_some_and(is_lock_timeout))
    .map(|(what, _)| what)
    .collect();
    let incomplete = incomplete_reason(&locked, &timed_out);
    if let Some(why) = &incomplete {
        tracing::warn!("schema cache incomplete: {why}");
    }
    let rows = match schema_res {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("schema cache fetch failed: {e}; completion disabled");
            return SchemaCache {
                incomplete,
                ..SchemaCache::default()
            };
        }
    };
    let mut cache = SchemaCache {
        incomplete,
        ..SchemaCache::default()
    };
    let mut last_relation: Option<(String, String)> = None;
    let mut seen_schemas: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for row in &rows {
        let schema: String = match row.try_get(0) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let name: String = match row.try_get(1) {
            Ok(t) => t,
            Err(_) => continue,
        };
        // pg_class.relkind is a single-char field (`r`/`v`/`m`/`p`/`f`
        // /`S`/`i`); shapes rows into the right bucket on the cache.
        let relkind: i8 = row.try_get(2).unwrap_or(b'r' as i8);
        let col: Option<String> = row.try_get(3).ok();
        let coltype: Option<String> = row.try_get(4).ok();
        let not_null: bool = row.try_get(5).unwrap_or(false);
        let oid: Option<u32> = row.try_get(6).ok();
        let attnum: Option<i16> = row.try_get(7).ok().flatten();
        seen_schemas.insert(schema.clone());
        let key = (schema.clone(), name.clone());
        if last_relation.as_ref() != Some(&key) {
            let meta = TableMeta {
                schema: schema.clone(),
                name: name.clone(),
            };
            match relkind as u8 {
                b'S' => cache.sequences.push(meta),
                b'i' => cache.indexes.push(meta),
                // r, v, m, p, f all live in `tables` — completion
                // doesn't distinguish them today, and the column
                // shapes match.
                kind => {
                    if matches!(kind, b'v' | b'm' | b'f') {
                        cache.views.insert(key.clone());
                    }
                    if let Some(oid) = oid {
                        cache.relations_by_oid.insert(oid, key.clone());
                    }
                    cache.tables.push(meta);
                    cache.columns_by_table.entry(key.clone()).or_default();
                    cache.columns_meta_by_table.entry(key.clone()).or_default();
                }
            }
            last_relation = Some(key.clone());
        }
        if let Some(c) = col {
            // Only tables/views/etc. have columns we'd want to suggest;
            // sequences and indexes have internal columns that aren't
            // useful for completion.
            if matches!(relkind as u8, b'r' | b'v' | b'm' | b'p' | b'f') {
                cache
                    .columns_by_table
                    .entry(key.clone())
                    .or_default()
                    .push(c.clone());
                cache
                    .columns_meta_by_table
                    .entry(key)
                    .or_default()
                    .push(ColumnMeta {
                        name: c,
                        type_name: coltype.unwrap_or_default(),
                        not_null,
                        attnum: attnum.unwrap_or(0),
                    });
            }
        }
    }
    cache.schemas = seen_schemas.into_iter().collect();
    // Second-pass: constraint names (separate query so a
    // pg_constraint permission gap doesn't kill the main cache).
    match constraint_res {
        Ok(rows) => {
            for row in &rows {
                let s: Result<String, _> = row.try_get(0);
                let t: Result<String, _> = row.try_get(1);
                let n: Result<String, _> = row.try_get(2);
                if let (Ok(s), Ok(t), Ok(n)) = (s, t, n) {
                    cache.constraints.push(ConstraintMeta {
                        schema: s,
                        table: t,
                        name: n,
                    });
                }
            }
        }
        Err(e) => {
            tracing::warn!("constraint fetch failed: {e}; ON CONSTRAINT completion disabled");
        }
    };
    // Third-pass: per-table size info. Same best-effort stance —
    // `pg_relation_size` returns 0 for views / matviews / foreign
    // tables; we still try `relkind='r'` only so the SUM is
    // meaningful.
    match size_res {
        Ok(rows) => {
            for row in &rows {
                let s: Result<String, _> = row.try_get(0);
                let t: Result<String, _> = row.try_get(1);
                let tb: Result<i64, _> = row.try_get(2);
                let total: Result<i64, _> = row.try_get(3);
                if let (Ok(s), Ok(t), Ok(tb), Ok(total)) = (s, t, tb, total) {
                    cache.table_sizes.insert(
                        (s, t),
                        TableSize {
                            table_bytes: tb.max(0) as u64,
                            total_bytes: total.max(0) as u64,
                        },
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!("table-size fetch failed: {e}; schema browser size column disabled");
        }
    };
    // Fourth-pass: FK edges (one row per column pair). Same
    // best-effort stance — a permission gap on pg_constraint
    // is rare but recoverable.
    match part_res {
        Ok(rows) => {
            for row in &rows {
                let get = |i: usize| row.try_get::<usize, String>(i).ok();
                if let (Some(cs), Some(c), Some(ps), Some(p)) = (get(0), get(1), get(2), get(3)) {
                    cache.partition_parent.insert((cs, c), (ps, p));
                }
            }
        }
        Err(e) => tracing::warn!("partitions fetch failed: {e}; partitions list as tables"),
    }
    match index_res {
        Ok(rows) => {
            for row in &rows {
                let get = |i: usize| row.try_get::<usize, String>(i).ok();
                let flag = |i: usize| row.try_get::<usize, bool>(i).unwrap_or(false);
                if let (Some(s), Some(t), Some(name), Some(definition)) =
                    (get(0), get(1), get(2), get(3))
                {
                    cache
                        .indexes_by_table
                        .entry((s, t))
                        .or_default()
                        .push(IndexMeta {
                            name,
                            definition,
                            unique: flag(4),
                            primary: flag(5),
                        });
                }
            }
        }
        Err(e) => tracing::warn!("index fetch failed: {e}; the schema browser lists no indexes"),
    }
    match pk_res {
        Ok(rows) => {
            for row in &rows {
                let get = |i: usize| row.try_get::<usize, String>(i).ok();
                if let (Some(s), Some(t), Some(col)) = (get(0), get(1), get(2)) {
                    cache.primary_keys.entry((s, t)).or_default().push(col);
                }
            }
        }
        Err(e) => tracing::warn!(
            "primary-key fetch failed: {e}; no cell editing, the result diff keys by inference"
        ),
    }
    match fk_res {
        Ok(rows) => {
            for row in &rows {
                let cs: Result<String, _> = row.try_get(0);
                let ct: Result<String, _> = row.try_get(1);
                let cc: Result<String, _> = row.try_get(2);
                let ps: Result<String, _> = row.try_get(3);
                let pt: Result<String, _> = row.try_get(4);
                let pc: Result<String, _> = row.try_get(5);
                if let (Ok(cs), Ok(ct), Ok(cc), Ok(ps), Ok(pt), Ok(pc)) = (cs, ct, cc, ps, pt, pc) {
                    cache.fk_edges.push(FkEdge {
                        child_schema: cs,
                        child_table: ct,
                        child_column: cc,
                        parent_schema: ps,
                        parent_table: pt,
                        parent_column: pc,
                    });
                }
            }
        }
        Err(e) => {
            tracing::warn!("FK-edges fetch failed: {e}; FK navigation disabled");
        }
    };
    tracing::info!(
        "schema cache: {} schema(s), {} table(s), {} sequence(s), {} index(es), {} constraint(s)",
        cache.schemas.len(),
        cache.tables.len(),
        cache.sequences.len(),
        cache.indexes.len(),
        cache.constraints.len(),
    );
    cache
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked(name: &str, pid: i32, granted: bool) -> LockedRelation {
        LockedRelation {
            schema: "public".into(),
            name: name.into(),
            pid,
            granted,
        }
    }

    #[test]
    fn incomplete_reason_is_none_for_a_whole_fetch() {
        assert_eq!(incomplete_reason(&[], &[]), None);
    }

    #[test]
    fn incomplete_reason_names_the_lock_its_holder_and_what_is_missing() {
        assert_eq!(
            incomplete_reason(&[locked("orders", 4242, true)], &[]).as_deref(),
            Some("ACCESS EXCLUSIVE lock on public.orders held by pid 4242: no size or indexes")
        );
        // A request still waiting blocks readers just the same; the
        // message says it is waiting, not that it holds the lock.
        assert_eq!(
            incomplete_reason(&[locked("orders", 7, false)], &[]).as_deref(),
            Some("ACCESS EXCLUSIVE lock on public.orders awaited by pid 7: no size or indexes")
        );
    }

    #[test]
    fn incomplete_reason_counts_the_other_locks_and_adds_timed_out_reads() {
        let why = incomplete_reason(
            &[
                locked("a", 1, true),
                locked("b", 2, true),
                locked("c", 3, false),
            ],
            &["partitions", "indexes"],
        )
        .unwrap();
        assert_eq!(
            why,
            "ACCESS EXCLUSIVE lock on public.a (and 2 more) held by pid 1: no size or indexes \
             · partitions, indexes gave up at lock_timeout"
        );
        assert_eq!(
            incomplete_reason(&[], &["columns"]).as_deref(),
            Some("columns gave up at lock_timeout")
        );
    }

    /// The two reads that queue behind an ACCESS EXCLUSIVE lock skip
    /// what the lock covers, and the lookup that names it reads the same
    /// lock set (held or waited for, any backend but this one, this
    /// database).
    #[test]
    fn the_lock_taking_reads_skip_locked_relations() {
        for sql in [TABLE_SIZES_SQL, TABLE_INDEXES_SQL] {
            assert!(sql.starts_with("WITH locked AS ("), "{sql}");
            assert!(
                sql.contains("NOT IN (SELECT relation FROM locked)"),
                "{sql}"
            );
        }
        for sql in [TABLE_SIZES_SQL, LOCKED_RELATIONS_SQL] {
            assert!(sql.contains("l.mode = 'AccessExclusiveLock'"), "{sql}");
            assert!(
                sql.contains("l.pid IS DISTINCT FROM pg_catalog.pg_backend_pid()"),
                "{sql}"
            );
            assert!(
                !sql.contains("l.granted AND"),
                "waiting requests count: {sql}"
            );
        }
    }

    #[test]
    fn schema_cache_carries_column_meta_alongside_columns_by_table() {
        // ColumnMeta exists, carries type + not_null, and the cache
        // has a dedicated `columns_meta_by_table` map for it.
        let mut c = SchemaCache::default();
        c.columns_meta_by_table.insert(
            ("public".into(), "users".into()),
            vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "integer".into(),
                    not_null: true,
                    attnum: 1,
                },
                ColumnMeta {
                    name: "email".into(),
                    type_name: "character varying(120)".into(),
                    not_null: false,
                    attnum: 2,
                },
            ],
        );
        let meta = c
            .columns_meta_by_table
            .get(&("public".to_string(), "users".to_string()))
            .unwrap();
        assert_eq!(meta.len(), 2);
        assert_eq!(meta[0].name, "id");
        assert_eq!(meta[0].type_name, "integer");
        assert!(meta[0].not_null);
        assert_eq!(meta[1].type_name, "character varying(120)");
        assert!(!meta[1].not_null);
    }

    #[test]
    fn fk_edge_for_child_finds_match_case_insensitive() {
        let mut c = SchemaCache::default();
        c.fk_edges.push(FkEdge {
            child_schema: "public".into(),
            child_table: "orders".into(),
            child_column: "user_id".into(),
            parent_schema: "public".into(),
            parent_table: "users".into(),
            parent_column: "id".into(),
        });
        // Case-insensitive on all three coords.
        let hit = c
            .fk_edge_for_child("PUBLIC", "Orders", "USER_ID")
            .expect("found");
        assert_eq!(hit.parent_table, "users");
        assert!(c.fk_edge_for_child("public", "orders", "other").is_none());
    }

    /// A mixed-case table and its lowercase twin each have their own FK;
    /// following from `"PgRevMixed"` must take its edge, not the twin's
    /// that happens to come first.
    #[test]
    fn fk_edge_for_child_prefers_the_exact_name() {
        let mut c = SchemaCache::default();
        for (child, parent) in [
            ("pgrevmixed", "lower_parent"),
            ("PgRevMixed", "mixed_parent"),
        ] {
            c.fk_edges.push(FkEdge {
                child_schema: "public".into(),
                child_table: child.into(),
                child_column: "pid".into(),
                parent_schema: "public".into(),
                parent_table: parent.into(),
                parent_column: "id".into(),
            });
        }
        let hit = c.fk_edge_for_child("public", "PgRevMixed", "pid").unwrap();
        assert_eq!(hit.parent_table, "mixed_parent");
        let hit = c.fk_edge_for_child("public", "pgrevmixed", "pid").unwrap();
        assert_eq!(hit.parent_table, "lower_parent");
    }

    #[test]
    fn fk_edges_sql_uses_array_zipped_unnest() {
        // Spot-check: the SQL must preserve column order across
        // multi-column FKs via WITH ORDINALITY zip.
        assert!(FK_EDGES_SQL.contains("WITH ORDINALITY"));
        assert!(FK_EDGES_SQL.contains("c.contype = 'f'"));
    }

    #[test]
    fn format_bytes_picks_human_units() {
        const MB: u64 = 1024 * 1024;
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1024), "1.0 kB");
        assert_eq!(format_bytes(MB), "1.0 MB");
        assert_eq!(format_bytes(1024 * MB + 200 * MB), "1.2 GB");
        // The reported pair: the same unit either side of 10 MB.
        assert_eq!(format_bytes(9191 * 1024), "9.0 MB");
        assert_eq!(format_bytes(10199 * 1024), "10 MB");
        assert_eq!(format_bytes(212 * MB), "212 MB");
        // Rounding carries to the next unit rather than `1024 kB`.
        assert_eq!(format_bytes(MB - 100), "1.0 MB");
        assert_eq!(format_bytes(10 * MB - 1), "10 MB");
        // TB is the largest unit.
        assert_eq!(format_bytes(2048 * 1024 * 1024 * MB), "2048 TB");
    }

    #[test]
    fn format_size_cell_formats_byte_counts_and_passes_the_rest() {
        assert_eq!(format_size_cell("9411584"), "9.0 MB");
        assert_eq!(format_size_cell("1203 MB"), "1203 MB");
        assert_eq!(format_size_cell(""), "");
    }

    #[test]
    fn table_sizes_sql_uses_relation_size_helpers() {
        // Spot check — schema browser detail pane depends on
        // both columns being present and bigint-typed.
        assert!(TABLE_SIZES_SQL.contains("pg_relation_size"));
        assert!(TABLE_SIZES_SQL.contains("pg_total_relation_size"));
        assert!(TABLE_SIZES_SQL.contains("bigint"));
    }

    #[test]
    fn schema_sql_pulls_format_type_and_attnotnull() {
        // Guard against accidental edits dropping the type / NN
        // fetch — we depend on these for the schema browser
        // detail pane.
        assert!(SCHEMA_SQL.contains("format_type(a.atttypid, a.atttypmod)"));
        assert!(SCHEMA_SQL.contains("attnotnull"));
    }

    #[test]
    fn all_column_names_is_distinct_and_sorted() {
        let mut c = SchemaCache::default();
        c.columns_by_table.insert(
            ("public".into(), "users".into()),
            vec!["id".into(), "email".into(), "name".into()],
        );
        c.columns_by_table.insert(
            ("public".into(), "orders".into()),
            vec!["id".into(), "user_id".into()],
        );
        assert_eq!(c.all_column_names(), vec!["email", "id", "name", "user_id"]);
    }

    #[test]
    fn columns_for_with_schema_is_exact_match() {
        let mut c = SchemaCache::default();
        c.tables.push(TableMeta {
            schema: "public".into(),
            name: "users".into(),
        });
        c.tables.push(TableMeta {
            schema: "audit".into(),
            name: "users".into(),
        });
        c.columns_by_table.insert(
            ("public".into(), "users".into()),
            vec!["id".into(), "email".into()],
        );
        c.columns_by_table.insert(
            ("audit".into(), "users".into()),
            vec!["id".into(), "actor".into()],
        );

        assert_eq!(
            c.columns_for(Some("audit"), "users").unwrap(),
            &vec!["id".to_string(), "actor".to_string()]
        );
        assert_eq!(
            c.columns_for(Some("public"), "users").unwrap(),
            &vec!["id".to_string(), "email".to_string()]
        );
        assert!(c.columns_for(Some("nope"), "users").is_none());
    }

    #[test]
    fn columns_for_is_case_insensitive_on_both_args() {
        let mut c = SchemaCache::default();
        c.tables.push(TableMeta {
            schema: "public".into(),
            name: "users".into(),
        });
        c.columns_by_table.insert(
            ("public".into(), "users".into()),
            vec!["id".into(), "email".into()],
        );
        // Mixed case in either schema or name should still hit.
        assert!(c.columns_for(None, "USERS").is_some());
        assert!(c.columns_for(None, "Users").is_some());
        assert!(c.columns_for(Some("PUBLIC"), "users").is_some());
        assert!(c.columns_for(Some("public"), "USERS").is_some());
        assert!(c.columns_for(Some("nope"), "users").is_none());
    }

    #[test]
    fn columns_for_without_schema_takes_first_match_in_table_order() {
        let mut c = SchemaCache::default();
        // Tables are kept in sorted order by the fetch; mimic that.
        c.tables.push(TableMeta {
            schema: "audit".into(),
            name: "users".into(),
        });
        c.tables.push(TableMeta {
            schema: "public".into(),
            name: "users".into(),
        });
        c.columns_by_table.insert(
            ("audit".into(), "users".into()),
            vec!["id".into(), "actor".into()],
        );
        c.columns_by_table.insert(
            ("public".into(), "users".into()),
            vec!["id".into(), "email".into()],
        );
        // First entry in `tables` wins.
        let got = c.columns_for(None, "users").unwrap();
        assert_eq!(got, &vec!["id".to_string(), "actor".to_string()]);
    }
}
