//! What differs between two databases' schemas: tables and views on one
//! side only, and per table in both, columns added, dropped or changed
//! (type, NOT NULL), the primary key, indexes and foreign keys. From two
//! `query::schema::fetch` reads; [`diff`] and its renderers are pure.
//!
//! A report, not a migration: it says what drifted between, say, staging
//! and production, and leaves the DDL to whoever reconciles them.
//! Partitions are compared through their parent, and extensions' own
//! tables are left out.

use crate::query::schema::SchemaCache;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// `pgman diff --help`.
pub const USAGE: &str = "\
usage: pgman diff <A> <B>

What differs between two databases' schemas: tables and views in one
only, and per table in both, columns added, dropped or changed (type,
NOT NULL), the primary key, indexes and foreign keys. A report, not a
migration.

<A> and <B> are targets as pgman dump takes them: ENV, ENV/DATABASE,
ENV/SERVER/DATABASE from environments.toml, a connections.toml name, or
a postgres:// URL. Each is read from its replica when it has one, over
a read-only connection, its secret read then and its ssh tunnel opened.

Exits 0 when they match, 1 when they differ, 2 on an error.

  pgman diff staging/ledger production/ledger";

/// `pgman diff <A> <B>`: the report, and whether they match. Connects
/// read-only to both, one after the other.
pub async fn run_cli(args: &[String]) -> Result<(String, bool), String> {
    let [a, b] = args else {
        return Err(format!("give two targets\n\n{USAGE}"));
    };
    let (ca, la, da) = read(a).await?;
    let (cb, lb, db) = read(b).await?;
    let d = diff(&ca, &cb);
    let mut out = format!("{la}: {da}\n{lb}: {db}\n\n");
    for (label, c) in [(&la, &ca), (&lb, &cb)] {
        if let Some(why) = &c.incomplete {
            out.push_str(&format!(
                "note: {label}'s catalog read was incomplete ({why})\n"
            ));
        }
    }
    out.push_str(&render_text(&d, &la, &lb));
    Ok((out, d.is_empty()))
}

/// One target's catalog, its label, and which database it came to.
async fn read(text: &str) -> Result<(SchemaCache, String, String), String> {
    use crate::inventory::target::{resolve, Want};
    let t = text.to_string();
    let resolved = tokio::task::spawn_blocking(move || resolve(&t, Want::PreferReplica))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))??;
    let opened = crate::pgtool::open(&resolved).await?;
    let client = crate::conn::connect_via(opened.dsn.clone(), opened.via, 30_000)
        .await
        .map_err(|e| format!("{}: {e}", resolved.label))?;
    let cache = crate::query::schema::fetch(&client).await;
    Ok((cache, resolved.label.clone(), resolved.describe()))
}

/// One table in both databases, and what differs in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableDiff {
    /// `schema.table`.
    pub table: String,
    /// `name type`, on one side only.
    pub columns_only_a: Vec<String>,
    pub columns_only_b: Vec<String>,
    /// `name: integer → bigint`, `name: NOT NULL → NULL`.
    pub columns_changed: Vec<String>,
    /// The primary key on each side, when they differ.
    pub primary_key: Option<(Vec<String>, Vec<String>)>,
    /// Index definitions on one side only, and indexes of one name
    /// defined differently (`name: def → def`).
    pub indexes_only_a: Vec<String>,
    pub indexes_only_b: Vec<String>,
    pub indexes_changed: Vec<String>,
    /// `column → schema.table.column`, on one side only.
    pub foreign_keys_only_a: Vec<String>,
    pub foreign_keys_only_b: Vec<String>,
}

impl TableDiff {
    fn is_empty(&self) -> bool {
        *self
            == TableDiff {
                table: self.table.clone(),
                ..TableDiff::default()
            }
    }
}

/// Everything that differs between `a` and `b`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// `schema.table` (or `… (view)`), in one database only.
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
    pub tables: Vec<TableDiff>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.only_a.is_empty() && self.only_b.is_empty() && self.tables.is_empty()
    }
}

type Key = (String, String);

/// The tables and views `c` compares: no partitions, no extensions'.
fn relations(c: &SchemaCache) -> BTreeMap<Key, bool> {
    c.tables
        .iter()
        .map(|t| (t.schema.clone(), t.name.clone()))
        .filter(|k| !c.extension_schemas.contains(&k.0) && !c.partition_parent.contains_key(k))
        .map(|k| {
            let view = c.views.contains(&k);
            (k, view)
        })
        .collect()
}

fn named((s, t): &Key, view: bool) -> String {
    if view {
        format!("{s}.{t} (view)")
    } else {
        format!("{s}.{t}")
    }
}

/// `left` and `right` minus each other, sorted.
fn only<T: Ord + Clone>(left: &BTreeSet<T>, right: &BTreeSet<T>) -> Vec<T> {
    left.difference(right).cloned().collect()
}

/// What differs between `a` and `b`. Pure.
pub fn diff(a: &SchemaCache, b: &SchemaCache) -> Diff {
    let (ra, rb) = (relations(a), relations(b));
    let mut out = Diff::default();
    for (k, view) in &ra {
        if !rb.contains_key(k) {
            out.only_a.push(named(k, *view));
        }
    }
    for (k, view) in &rb {
        if !ra.contains_key(k) {
            out.only_b.push(named(k, *view));
        }
    }
    for key in ra.keys().filter(|k| rb.contains_key(*k)) {
        let t = table_diff(a, b, key);
        if !t.is_empty() {
            out.tables.push(t);
        }
    }
    out
}

fn table_diff(a: &SchemaCache, b: &SchemaCache, key: &Key) -> TableDiff {
    let mut d = TableDiff {
        table: format!("{}.{}", key.0, key.1),
        ..TableDiff::default()
    };
    let cols = |c: &SchemaCache| -> BTreeMap<String, (String, bool)> {
        c.columns_meta_by_table
            .get(key)
            .map(|cs| {
                cs.iter()
                    .map(|c| (c.name.clone(), (c.type_name.clone(), c.not_null)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (ca, cb) = (cols(a), cols(b));
    for (name, (ty, _)) in &ca {
        if !cb.contains_key(name) {
            d.columns_only_a.push(format!("{name} {ty}"));
        }
    }
    for (name, (ty, _)) in &cb {
        if !ca.contains_key(name) {
            d.columns_only_b.push(format!("{name} {ty}"));
        }
    }
    let null = |n: bool| if n { "NOT NULL" } else { "NULL" };
    for (name, (ta, na)) in &ca {
        if let Some((tb, nb)) = cb.get(name) {
            if ta != tb {
                d.columns_changed.push(format!("{name}: {ta} → {tb}"));
            }
            if na != nb {
                d.columns_changed
                    .push(format!("{name}: {} → {}", null(*na), null(*nb)));
            }
        }
    }
    let pk = |c: &SchemaCache| c.primary_keys.get(key).cloned().unwrap_or_default();
    let (pa, pb) = (pk(a), pk(b));
    if pa != pb {
        d.primary_key = Some((pa, pb));
    }
    let indexes = |c: &SchemaCache| -> BTreeMap<String, String> {
        c.indexes_by_table
            .get(key)
            .map(|ix| {
                ix.iter()
                    .map(|i| (i.name.clone(), i.definition.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (ia, ib) = (indexes(a), indexes(b));
    for (name, def) in &ia {
        match ib.get(name) {
            None => d.indexes_only_a.push(def.clone()),
            Some(other) if other != def => {
                d.indexes_changed.push(format!("{name}: {def} → {other}"))
            }
            Some(_) => {}
        }
    }
    for (name, def) in &ib {
        if !ia.contains_key(name) {
            d.indexes_only_b.push(def.clone());
        }
    }
    let fks = |c: &SchemaCache| -> BTreeSet<String> {
        c.fk_edges
            .iter()
            .filter(|e| e.child_schema == key.0 && e.child_table == key.1)
            .map(|e| {
                format!(
                    "{} → {}.{}.{}",
                    e.child_column, e.parent_schema, e.parent_table, e.parent_column
                )
            })
            .collect()
    };
    let (fa, fb) = (fks(a), fks(b));
    d.foreign_keys_only_a = only(&fa, &fb);
    d.foreign_keys_only_b = only(&fb, &fa);
    d
}

/// The diff as `pgman diff` prints it, `a` and `b` named by `la` and
/// `lb`. Pure.
pub fn render_text(d: &Diff, la: &str, lb: &str) -> String {
    if d.is_empty() {
        return format!("{la} and {lb} have the same tables, columns, keys and indexes\n");
    }
    let mut out = String::new();
    let list = |out: &mut String, head: String, items: &[String]| {
        if !items.is_empty() {
            out.push_str(&format!("{head}\n"));
            for i in items {
                out.push_str(&format!("  {i}\n"));
            }
        }
    };
    list(&mut out, format!("only in {la}:"), &d.only_a);
    list(&mut out, format!("only in {lb}:"), &d.only_b);
    for t in &d.tables {
        out.push_str(&format!("\n{}\n", t.table));
        let sub = |out: &mut String, head: &str, items: &[String]| {
            for i in items {
                out.push_str(&format!("  {head} {i}\n"));
            }
        };
        sub(
            &mut out,
            &format!("column only in {la}:"),
            &t.columns_only_a,
        );
        sub(
            &mut out,
            &format!("column only in {lb}:"),
            &t.columns_only_b,
        );
        sub(&mut out, "column changed:", &t.columns_changed);
        if let Some((pa, pb)) = &t.primary_key {
            let show = |k: &[String]| {
                if k.is_empty() {
                    "none".to_string()
                } else {
                    format!("({})", k.join(", "))
                }
            };
            out.push_str(&format!("  primary key: {} → {}\n", show(pa), show(pb)));
        }
        sub(&mut out, &format!("index only in {la}:"), &t.indexes_only_a);
        sub(&mut out, &format!("index only in {lb}:"), &t.indexes_only_b);
        sub(&mut out, "index changed:", &t.indexes_changed);
        sub(
            &mut out,
            &format!("foreign key only in {la}:"),
            &t.foreign_keys_only_a,
        );
        sub(
            &mut out,
            &format!("foreign key only in {lb}:"),
            &t.foreign_keys_only_b,
        );
    }
    out
}

/// `d`, from before to after a change (a dry run's patches), as lines of
/// `+` added, `-` dropped, `~` changed. Empty when nothing changed. Pure.
pub fn render_changes(d: &Diff) -> String {
    let mut out = String::new();
    let mut line = |mark: char, what: String| out.push_str(&format!("  {mark} {what}\n"));
    for t in &d.only_b {
        line('+', format!("table {t}"));
    }
    for t in &d.only_a {
        line('-', format!("table {t}"));
    }
    for t in &d.tables {
        let on = &t.table;
        for c in &t.columns_only_b {
            line('+', format!("column {on}.{c}"));
        }
        for c in &t.columns_only_a {
            line('-', format!("column {on}.{c}"));
        }
        for c in &t.columns_changed {
            line('~', format!("column {on}.{c}"));
        }
        if let Some((a, b)) = &t.primary_key {
            line(
                '~',
                format!(
                    "primary key of {on}: ({}) → ({})",
                    a.join(", "),
                    b.join(", ")
                ),
            );
        }
        for i in &t.indexes_only_b {
            line('+', format!("index {i}"));
        }
        for i in &t.indexes_only_a {
            line('-', format!("index {i}"));
        }
        for i in &t.indexes_changed {
            line('~', format!("index {i}"));
        }
        for f in &t.foreign_keys_only_b {
            line('+', format!("foreign key {on}.{f}"));
        }
        for f in &t.foreign_keys_only_a {
            line('-', format!("foreign key {on}.{f}"));
        }
    }
    out
}

/// The diff as MCP's `schema_diff` returns it. Pure.
pub fn to_json(d: &Diff, la: &str, lb: &str) -> Value {
    let tables: Vec<Value> = d
        .tables
        .iter()
        .map(|t| {
            json!({
                "table": t.table,
                "columns_only_a": t.columns_only_a,
                "columns_only_b": t.columns_only_b,
                "columns_changed": t.columns_changed,
                "primary_key": t.primary_key.as_ref().map(|(a, b)| json!({"a": a, "b": b})),
                "indexes_only_a": t.indexes_only_a,
                "indexes_only_b": t.indexes_only_b,
                "indexes_changed": t.indexes_changed,
                "foreign_keys_only_a": t.foreign_keys_only_a,
                "foreign_keys_only_b": t.foreign_keys_only_b,
            })
        })
        .collect();
    json!({
        "a": la,
        "b": lb,
        "same": d.is_empty(),
        "only_a": d.only_a,
        "only_b": d.only_b,
        "tables": tables,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::schema::{ColumnMeta, FkEdge, IndexMeta, TableMeta};

    fn key(t: &str) -> Key {
        ("public".to_string(), t.to_string())
    }

    fn col(name: &str, ty: &str, not_null: bool) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            type_name: ty.into(),
            not_null,
            attnum: 0,
        }
    }

    fn base() -> SchemaCache {
        let mut c = SchemaCache::default();
        for t in ["customers", "orders", "events", "events_2026"] {
            c.tables.push(TableMeta {
                schema: "public".into(),
                name: t.into(),
            });
        }
        c.tables.push(TableMeta {
            schema: "ext".into(),
            name: "spatial_ref_sys".into(),
        });
        c.extension_schemas.insert("ext".into());
        c.partition_parent.insert(key("events_2026"), key("events"));
        c.columns_meta_by_table.insert(
            key("orders"),
            vec![
                col("id", "bigint", true),
                col("total", "numeric(10,2)", false),
                col("note", "text", false),
            ],
        );
        c.primary_keys.insert(key("orders"), vec!["id".into()]);
        c.indexes_by_table.insert(
            key("orders"),
            vec![IndexMeta {
                name: "orders_total".into(),
                definition: "CREATE INDEX orders_total ON public.orders USING btree (total)".into(),
                unique: false,
                primary: false,
            }],
        );
        c.fk_edges.push(FkEdge {
            child_schema: "public".into(),
            child_table: "orders".into(),
            child_column: "customer_id".into(),
            parent_schema: "public".into(),
            parent_table: "customers".into(),
            parent_column: "id".into(),
        });
        c
    }

    #[test]
    fn the_same_schema_has_nothing_to_say() {
        let d = diff(&base(), &base());
        assert!(d.is_empty(), "{d:?}");
        assert!(render_text(&d, "staging", "production").contains("have the same"));
        assert_eq!(to_json(&d, "a", "b")["same"], true);
    }

    #[test]
    fn what_drifted_is_named_per_table() {
        let a = base();
        let mut b = base();
        // Only in b: a table and a view; only in a: customers.
        b.tables.retain(|t| t.name != "customers");
        for t in ["refunds", "open_orders"] {
            b.tables.push(TableMeta {
                schema: "public".into(),
                name: t.into(),
            });
        }
        b.views.insert(key("open_orders"));
        // A partition that only one side has is not a difference.
        b.tables.push(TableMeta {
            schema: "public".into(),
            name: "events_2027".into(),
        });
        b.partition_parent.insert(key("events_2027"), key("events"));
        b.columns_meta_by_table.insert(
            key("orders"),
            vec![
                col("id", "integer", true),
                col("total", "numeric(10,2)", true),
                col("status", "text", false),
            ],
        );
        b.primary_keys.remove(&key("orders"));
        b.indexes_by_table.insert(
            key("orders"),
            vec![IndexMeta {
                name: "orders_total".into(),
                definition: "CREATE INDEX orders_total ON public.orders USING btree (total DESC)"
                    .into(),
                unique: false,
                primary: false,
            }],
        );
        b.fk_edges.clear();
        let d = diff(&a, &b);
        assert_eq!(d.only_a, ["public.customers"]);
        assert_eq!(d.only_b, ["public.open_orders (view)", "public.refunds"]);
        assert_eq!(d.tables.len(), 1, "{d:?}");
        let t = &d.tables[0];
        assert_eq!(t.table, "public.orders");
        assert_eq!(t.columns_only_a, ["note text"]);
        assert_eq!(t.columns_only_b, ["status text"]);
        assert_eq!(
            t.columns_changed,
            ["id: bigint → integer", "total: NULL → NOT NULL"]
        );
        assert_eq!(t.primary_key, Some((vec!["id".to_string()], vec![])));
        assert_eq!(t.indexes_changed.len(), 1);
        assert!(t.indexes_changed[0].starts_with("orders_total: "));
        assert_eq!(t.foreign_keys_only_a, ["customer_id → public.customers.id"]);
        let text = render_text(&d, "staging", "production");
        for want in [
            "only in staging:\n  public.customers",
            "only in production:\n  public.open_orders (view)",
            "\npublic.orders\n",
            "  column only in staging: note text",
            "  column changed: id: bigint → integer",
            "  primary key: (id) → none",
            "  index changed: orders_total:",
            "  foreign key only in staging: customer_id → public.customers.id",
        ] {
            assert!(text.contains(want), "{want:?} in\n{text}");
        }
        let v = to_json(&d, "staging", "production");
        assert_eq!(v["same"], false);
        assert_eq!(v["tables"][0]["primary_key"]["a"], json!(["id"]));
    }

    #[test]
    fn changes_read_as_added_dropped_and_changed() {
        let before = base();
        let mut after = base();
        after.tables.push(TableMeta {
            schema: "public".into(),
            name: "refunds".into(),
        });
        after.columns_meta_by_table.insert(
            key("orders"),
            vec![
                col("id", "bigint", true),
                col("total", "numeric(12,2)", false),
                col("note", "text", false),
                col("status", "text", false),
            ],
        );
        after.indexes_by_table.insert(key("orders"), vec![]);
        let text = render_changes(&diff(&before, &after));
        for want in [
            "  + table public.refunds\n",
            "  + column public.orders.status text\n",
            "  ~ column public.orders.total: numeric(10,2) → numeric(12,2)\n",
            "  - index CREATE INDEX orders_total",
        ] {
            assert!(text.contains(want), "{want:?} in\n{text}");
        }
        assert_eq!(render_changes(&diff(&before, &before)), "");
    }

    #[test]
    fn an_index_on_one_side_only_is_listed_by_its_definition() {
        let a = base();
        let mut b = base();
        b.indexes_by_table.insert(key("orders"), vec![]);
        let d = diff(&a, &b);
        assert_eq!(
            d.tables[0].indexes_only_a,
            ["CREATE INDEX orders_total ON public.orders USING btree (total)"]
        );
        let d = diff(&b, &a);
        assert_eq!(d.tables[0].indexes_only_b.len(), 1);
    }
}
