//! MCP's `describe`: the tables a connection can see, or one table in
//! full — its columns, keys, indexes, references both ways, size and
//! `CREATE` statement — from the catalog read the schema browser makes
//! (`query::schema::fetch`, `query::ddl`). An agent otherwise hand-writes
//! `information_schema` queries through `query`, 200 rows at a time.
//!
//! [`resolve`], [`render_tables`] and [`render_table`] are pure.

use crate::query::schema::{format_bytes, SchemaCache};
use serde_json::{json, Value};

/// The most tables the listing returns.
pub const MAX_TABLES: usize = 500;

/// The table `wanted` names in `cache` — `schema.table`, or a bare
/// `table` in whichever one schema has it — as `(schema, table)`; an
/// exact match first, else the one that matches ignoring case. Or why
/// none. Pure.
pub fn resolve(cache: &SchemaCache, wanted: &str) -> Result<(String, String), String> {
    let (schema, table) = match wanted.split_once('.') {
        Some((s, t)) => (Some(s), t),
        None => (None, wanted),
    };
    let candidates = |exact: bool| -> Vec<(String, String)> {
        let same = |a: &str, b: &str| {
            if exact {
                a == b
            } else {
                a.eq_ignore_ascii_case(b)
            }
        };
        cache
            .tables
            .iter()
            .filter(|t| same(&t.name, table) && schema.is_none_or(|s| same(&t.schema, s)))
            .map(|t| (t.schema.clone(), t.name.clone()))
            .collect()
    };
    let mut hits = candidates(true);
    if hits.is_empty() {
        hits = candidates(false);
    }
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => Err(format!(
            "no table or view {wanted:?} that this connection's role can see; call describe \
             without `table` for the list"
        )),
        _ => Err(format!(
            "{wanted:?} is in more than one schema: {} — name one as schema.table",
            hits.iter()
                .map(|(s, t)| format!("{s}.{t}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// What `cache` calls `(schema, table)`'s kind. Pure.
fn kind(cache: &SchemaCache, schema: &str, table: &str) -> &'static str {
    let key = (schema.to_string(), table.to_string());
    if cache.views.contains(&key) {
        "view"
    } else if !cache.partitions_of(schema, table).is_empty() {
        "partitioned table"
    } else if cache.partition_parent.contains_key(&key) {
        "partition"
    } else {
        "table"
    }
}

/// `describe` without a table: every table and view but partitions
/// (counted under their parent) and extensions' own, with kind, size
/// and column count, up to [`MAX_TABLES`]. Pure.
pub fn render_tables(name: &str, cache: &SchemaCache) -> String {
    let listed: Vec<_> = cache
        .tables
        .iter()
        .filter(|t| !cache.extension_schemas.contains(&t.schema))
        .filter(|t| {
            !cache
                .partition_parent
                .contains_key(&(t.schema.clone(), t.name.clone()))
        })
        .collect();
    let rows: Vec<Value> = listed
        .iter()
        .take(MAX_TABLES)
        .map(|t| {
            let key = (t.schema.clone(), t.name.clone());
            let partitions = cache.partitions_of(&t.schema, &t.name).len();
            let mut row = json!({
                "table": format!("{}.{}", t.schema, t.name),
                "kind": kind(cache, &t.schema, &t.name),
                "columns": cache.columns_meta_by_table.get(&key).map(Vec::len),
                "size": cache.table_sizes.get(&key).map(|s| format_bytes(s.total_bytes)),
            });
            if partitions > 0 {
                row["partitions"] = json!(partitions);
            }
            row
        })
        .collect();
    json!({
        "connection": name,
        "tables": rows,
        "count": listed.len(),
        "truncated": listed.len() > MAX_TABLES,
        "incomplete": cache.incomplete,
    })
    .to_string()
}

/// `describe` of `(schema, table)`: columns, primary key, foreign keys
/// out and in, indexes, size, and its `CREATE` statement (`ddl`, or why
/// it couldn't be read). Pure.
pub fn render_table(
    name: &str,
    cache: &SchemaCache,
    (schema, table): (&str, &str),
    ddl: Result<String, String>,
) -> String {
    let key = (schema.to_string(), table.to_string());
    let columns: Vec<Value> = cache
        .columns_meta_by_table
        .get(&key)
        .map(|cols| {
            cols.iter()
                .map(|c| json!({"name": c.name, "type": c.type_name, "not_null": c.not_null}))
                .collect()
        })
        .unwrap_or_default();
    let qualified = |s: &str, t: &str, c: &str| format!("{s}.{t}.{c}");
    let references: Vec<Value> = cache
        .fk_edges
        .iter()
        .filter(|e| e.child_schema == schema && e.child_table == table)
        .map(|e| {
            json!({
                "column": e.child_column,
                "references": qualified(&e.parent_schema, &e.parent_table, &e.parent_column),
            })
        })
        .collect();
    let referenced_by: Vec<Value> = cache
        .fk_edges
        .iter()
        .filter(|e| e.parent_schema == schema && e.parent_table == table)
        .map(|e| {
            json!({
                "column": e.parent_column,
                "from": qualified(&e.child_schema, &e.child_table, &e.child_column),
            })
        })
        .collect();
    let indexes: Vec<Value> = cache
        .indexes_by_table
        .get(&key)
        .map(|ix| {
            ix.iter()
                .map(|i| {
                    json!({
                        "name": i.name,
                        "definition": i.definition,
                        "unique": i.unique,
                        "primary": i.primary,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let (ddl, ddl_error) = match ddl {
        Ok(s) => (Some(s), None),
        Err(e) => (None, Some(e)),
    };
    let partitions: Vec<String> = cache
        .partitions_of(schema, table)
        .into_iter()
        .map(|(s, t)| format!("{s}.{t}"))
        .collect();
    json!({
        "connection": name,
        "table": format!("{schema}.{table}"),
        "kind": kind(cache, schema, table),
        "columns": columns,
        "primary_key": cache.primary_key(schema, table),
        "references": references,
        "referenced_by": referenced_by,
        "indexes": indexes,
        "size": cache.table_sizes.get(&key).map(|s| format_bytes(s.total_bytes)),
        "partition_of": cache
            .partition_parent
            .get(&key)
            .map(|(s, t)| format!("{s}.{t}")),
        "partitions": partitions,
        "ddl": ddl,
        "ddl_error": ddl_error,
        "incomplete": cache.incomplete,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::schema::{ColumnMeta, FkEdge, IndexMeta, TableMeta, TableSize};

    fn key(s: &str, t: &str) -> (String, String) {
        (s.to_string(), t.to_string())
    }

    fn cache() -> SchemaCache {
        let mut c = SchemaCache::default();
        for (s, t) in [
            ("public", "customers"),
            ("public", "orders"),
            ("billing", "orders"),
            ("public", "events"),
            ("public", "events_2026"),
            ("public", "recent"),
            ("ext", "spatial_ref_sys"),
        ] {
            c.tables.push(TableMeta {
                schema: s.into(),
                name: t.into(),
            });
        }
        c.extension_schemas.insert("ext".into());
        c.views.insert(key("public", "recent"));
        c.partition_parent
            .insert(key("public", "events_2026"), key("public", "events"));
        c.columns_meta_by_table.insert(
            key("public", "orders"),
            vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "bigint".into(),
                    not_null: true,
                    attnum: 1,
                },
                ColumnMeta {
                    name: "customer_id".into(),
                    type_name: "integer".into(),
                    not_null: false,
                    attnum: 2,
                },
            ],
        );
        c.primary_keys
            .insert(key("public", "orders"), vec!["id".into()]);
        c.fk_edges.push(FkEdge {
            child_schema: "public".into(),
            child_table: "orders".into(),
            child_column: "customer_id".into(),
            parent_schema: "public".into(),
            parent_table: "customers".into(),
            parent_column: "id".into(),
            constraint: String::new(),
            ord: 1,
        });
        c.indexes_by_table.insert(
            key("public", "orders"),
            vec![IndexMeta {
                name: "orders_pkey".into(),
                definition: "CREATE UNIQUE INDEX orders_pkey ON public.orders USING btree (id)"
                    .into(),
                unique: true,
                primary: true,
            }],
        );
        c.table_sizes.insert(
            key("public", "orders"),
            TableSize {
                table_bytes: 8192,
                total_bytes: 3 * 1024 * 1024,
            },
        );
        c
    }

    #[test]
    fn a_table_is_found_qualified_bare_or_by_case_and_ambiguity_is_named() {
        let c = cache();
        assert_eq!(resolve(&c, "public.orders"), Ok(key("public", "orders")));
        assert_eq!(resolve(&c, "customers"), Ok(key("public", "customers")));
        assert_eq!(resolve(&c, "Customers"), Ok(key("public", "customers")));
        let e = resolve(&c, "orders").unwrap_err();
        assert!(
            e.contains("billing.orders") && e.contains("public.orders"),
            "{e}"
        );
        assert!(resolve(&c, "nope")
            .unwrap_err()
            .contains("describe without"));
        assert!(resolve(&c, "billing.customers").is_err());
    }

    #[test]
    fn the_listing_leaves_out_partitions_and_extensions_and_says_what_each_is() {
        let v: Value = serde_json::from_str(&render_tables("app", &cache())).unwrap();
        let names: Vec<&str> = v["tables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["table"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"public.events_2026"), "{names:?}");
        assert!(!names.contains(&"ext.spatial_ref_sys"), "{names:?}");
        assert_eq!(v["count"], 5);
        assert_eq!(v["truncated"], false);
        let row = |n: &str| {
            v["tables"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["table"] == n)
                .unwrap()
                .clone()
        };
        assert_eq!(row("public.recent")["kind"], "view");
        assert_eq!(row("public.events")["kind"], "partitioned table");
        assert_eq!(row("public.events")["partitions"], 1);
        assert_eq!(row("public.orders")["columns"], 2);
        assert_eq!(row("public.orders")["size"], "3.0 MB");
    }

    #[test]
    fn the_listing_is_capped() {
        let mut c = SchemaCache::default();
        for i in 0..MAX_TABLES + 3 {
            c.tables.push(TableMeta {
                schema: "public".into(),
                name: format!("t{i}"),
            });
        }
        let v: Value = serde_json::from_str(&render_tables("app", &c)).unwrap();
        assert_eq!(v["tables"].as_array().unwrap().len(), MAX_TABLES);
        assert_eq!(v["count"], MAX_TABLES + 3);
        assert_eq!(v["truncated"], true);
    }

    #[test]
    fn one_table_has_its_keys_both_ways_its_indexes_and_its_ddl() {
        let c = cache();
        let v: Value = serde_json::from_str(&render_table(
            "app",
            &c,
            ("public", "orders"),
            Ok("CREATE TABLE public.orders (…);".into()),
        ))
        .unwrap();
        assert_eq!(v["table"], "public.orders");
        assert_eq!(v["columns"][0]["name"], "id");
        assert_eq!(v["columns"][1]["not_null"], false);
        assert_eq!(v["primary_key"], json!(["id"]));
        assert_eq!(
            v["references"],
            json!([{"column": "customer_id", "references": "public.customers.id"}])
        );
        assert_eq!(v["indexes"][0]["primary"], true);
        assert_eq!(v["ddl"], "CREATE TABLE public.orders (…);");
        assert_eq!(v["ddl_error"], Value::Null);
        let v: Value = serde_json::from_str(&render_table(
            "app",
            &c,
            ("public", "customers"),
            Err("permission denied".into()),
        ))
        .unwrap();
        assert_eq!(
            v["referenced_by"],
            json!([{"column": "id", "from": "public.orders.customer_id"}])
        );
        assert_eq!(v["ddl"], Value::Null);
        assert_eq!(v["ddl_error"], "permission denied");
        let v: Value = serde_json::from_str(&render_table(
            "app",
            &c,
            ("public", "events_2026"),
            Ok(String::new()),
        ))
        .unwrap();
        assert_eq!(v["kind"], "partition");
        assert_eq!(v["partition_of"], "public.events");
    }
}
