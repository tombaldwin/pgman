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
    /// Column names per (schema, table). Order = pg attnum, so it mirrors
    /// what `SELECT *` would expose.
    pub columns_by_table: HashMap<(String, String), Vec<String>>,
}

impl SchemaCache {
    /// True when there's nothing to complete against — used by callers
    /// that want to skip the candidate computation early.
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// All distinct column names across every table, sorted.
    /// Useful for the unqualified-no-FROM fallback ("offer everything").
    pub fn all_column_names(&self) -> Vec<String> {
        let mut seen: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
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
/// tables (`p`), and foreign tables (`f`).
pub const SCHEMA_SQL: &str = "\
SELECT n.nspname, c.relname, a.attname \
FROM pg_catalog.pg_class c \
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
LEFT JOIN pg_catalog.pg_attribute a \
       ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
WHERE c.relkind = ANY ('{r,v,m,p,f}') \
  AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
ORDER BY n.nspname, c.relname, a.attnum";

/// Run the cache-building query on `client` and assemble a `SchemaCache`.
/// Returns an empty cache (and logs a warning) if the query fails — the
/// rest of pgman keeps working without completion.
pub async fn fetch(client: &Arc<tokio_postgres::Client>) -> SchemaCache {
    let rows = match client.query(SCHEMA_SQL, &[]).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("schema cache fetch failed: {e}; completion disabled");
            return SchemaCache::default();
        }
    };
    let mut cache = SchemaCache::default();
    let mut last_table: Option<(String, String)> = None;
    let mut seen_schemas: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for row in &rows {
        let schema: String = match row.try_get(0) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let table: String = match row.try_get(1) {
            Ok(t) => t,
            Err(_) => continue,
        };
        // attname can be NULL (table with no columns? rare but possible);
        // a try_get on Option handles that.
        let col: Option<String> = row.try_get(2).ok();
        seen_schemas.insert(schema.clone());
        let key = (schema.clone(), table.clone());
        if last_table.as_ref() != Some(&key) {
            cache.tables.push(TableMeta {
                schema: schema.clone(),
                name: table.clone(),
            });
            last_table = Some(key.clone());
            cache.columns_by_table.entry(key.clone()).or_default();
        }
        if let Some(c) = col {
            cache
                .columns_by_table
                .entry(key)
                .or_default()
                .push(c);
        }
    }
    cache.schemas = seen_schemas.into_iter().collect();
    tracing::info!(
        "schema cache: {} schema(s), {} table(s)",
        cache.schemas.len(),
        cache.tables.len()
    );
    cache
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            c.all_column_names(),
            vec!["email", "id", "name", "user_id"]
        );
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
