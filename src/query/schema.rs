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
}

/// Pretty-print a byte count using IEC units (KiB, MiB, GiB).
/// Uses 1024-based scaling because that's what most operators
/// expect when sizing relations next to disk-free output.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    const TIB: u64 = 1024_u64.pow(4);
    if bytes >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

impl SchemaCache {
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
        self.fk_edges.iter().find(|e| {
            e.child_schema.eq_ignore_ascii_case(schema)
                && e.child_table.eq_ignore_ascii_case(table)
                && e.child_column.eq_ignore_ascii_case(column)
        })
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
       a.attnotnull \
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

/// Query that fetches per-table sizes (heap + total including
/// indexes/toast). Run as a third pass so a permission gap on
/// `pg_relation_size` doesn't kill the cache build.
pub const TABLE_SIZES_SQL: &str = "\
SELECT n.nspname, c.relname, \
       pg_relation_size(c.oid)::bigint AS table_bytes, \
       pg_total_relation_size(c.oid)::bigint AS total_bytes \
FROM pg_class c \
JOIN pg_namespace n ON n.oid = c.relnamespace \
WHERE c.relkind = 'r' \
  AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast')";

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

/// Run the cache-building query on `client` and assemble a `SchemaCache`.
/// Returns an empty cache (and logs a warning) if the query fails — the
/// rest of pgman keeps working without completion.
pub async fn fetch(client: &Arc<tokio_postgres::Client>) -> SchemaCache {
    // Issue the four independent catalog queries concurrently — tokio_postgres
    // pipelines them on the single connection, collapsing four serial round
    // trips into roughly one. Each stays best-effort: a permission gap on one
    // (e.g. pg_constraint) degrades that feature without killing the cache.
    let (schema_res, constraint_res, size_res, fk_res) = tokio::join!(
        client.query(SCHEMA_SQL, &[]),
        client.query(CONSTRAINTS_SQL, &[]),
        client.query(TABLE_SIZES_SQL, &[]),
        client.query(FK_EDGES_SQL, &[]),
    );
    let rows = match schema_res {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("schema cache fetch failed: {e}; completion disabled");
            return SchemaCache::default();
        }
    };
    let mut cache = SchemaCache::default();
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
                _ => {
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
                },
                ColumnMeta {
                    name: "email".into(),
                    type_name: "character varying(120)".into(),
                    not_null: false,
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

    #[test]
    fn fk_edges_sql_uses_array_zipped_unnest() {
        // Spot-check: the SQL must preserve column order across
        // multi-column FKs via WITH ORDINALITY zip.
        assert!(FK_EDGES_SQL.contains("WITH ORDINALITY"));
        assert!(FK_EDGES_SQL.contains("c.contype = 'f'"));
    }

    #[test]
    fn format_bytes_picks_human_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MiB");
        assert_eq!(
            format_bytes(1024 * 1024 * 1024 + 200 * 1024 * 1024),
            "1.20 GiB"
        );
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
