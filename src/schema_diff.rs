//! What differs between two databases' schemas: tables and views on one
//! side only, and per table in both, columns added, dropped or changed
//! (type, NOT NULL), the primary key, indexes, foreign keys and CHECK
//! constraints; views defined differently; functions on one side only or
//! defined differently. From `query::schema::fetch` and
//! `query::definitions::fetch` reads; [`diff`] and its renderers are pure.
//!
//! A report, not a migration: it says what drifted between, say, staging
//! and production, and leaves the DDL to whoever reconciles them.
//! Partitions are compared through their parent, and extensions' own
//! tables are left out.

use crate::query::definitions::Definitions;
use crate::query::schema::SchemaCache;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// `pgman diff --help`.
pub const USAGE: &str = "\
usage: pgman diff <A> <B>

What differs between two databases' schemas: tables and views in one
only, and per table in both, columns added, dropped or changed (type,
NOT NULL), the primary key, indexes, foreign keys and CHECK constraints;
views defined differently; functions in one only or defined differently.
A report, not a migration.

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
    let (ca, fa, la, da) = read(a).await?;
    let (cb, fb, lb, db) = read(b).await?;
    let (d, notes) = diff_reads((&ca, &fa, &la), (&cb, &fb, &lb));
    let mut out = format!("{la}: {da}\n{lb}: {db}\n\n");
    for (label, c) in [(&la, &ca), (&lb, &cb)] {
        if let Some(why) = &c.incomplete {
            out.push_str(&format!(
                "note: {label}'s catalog read was incomplete ({why})\n"
            ));
        }
    }
    for n in notes {
        out.push_str(&format!("note: {n}\n"));
    }
    out.push_str(&render_text(&d, &la, &lb));
    Ok((out, d.is_empty()))
}

/// One target's catalog and definitions, its label, and which database
/// it came to.
async fn read(
    text: &str,
) -> Result<(SchemaCache, Result<Definitions, String>, String, String), String> {
    use crate::inventory::target::{resolve, Want};
    let t = text.to_string();
    let resolved = tokio::task::spawn_blocking(move || resolve(&t, Want::PreferReplica))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))??;
    let opened = crate::pgtool::open(&resolved).await?;
    let client = crate::conn::connect_via(opened.dsn.clone(), opened.via, 30_000)
        .await
        .map_err(|e| {
            format!(
                "{}: {}",
                resolved.label,
                crate::pgtool::explain_connect(e, &resolved)
            )
        })?;
    let cache = crate::query::schema::fetch(&client).await;
    let defs = crate::query::definitions::fetch(&client).await;
    Ok((cache, defs, resolved.label.clone(), resolved.describe()))
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
    /// CHECK constraints, `name CHECK (…)`, on one side only, and of one
    /// name defined differently (`name: def → def`).
    pub checks_only_a: Vec<String>,
    pub checks_only_b: Vec<String>,
    pub checks_changed: Vec<String>,
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

/// One object in both databases whose definition differs: its name and
/// each side's definition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changed {
    pub name: String,
    pub a: String,
    pub b: String,
}

/// Everything that differs between `a` and `b`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// `schema.table` (or `… (view)`), in one database only.
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
    pub tables: Vec<TableDiff>,
    /// Whether CHECKs, views' definitions and functions were compared:
    /// both sides' [`Definitions`] were read.
    pub definitions_compared: bool,
    /// Views in both whose definitions differ.
    pub views_changed: Vec<Changed>,
    /// Functions, `schema.name(argument types)`, on one side only, and
    /// in both but defined differently.
    pub functions_only_a: Vec<String>,
    pub functions_only_b: Vec<String>,
    pub functions_changed: Vec<Changed>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.only_a.is_empty()
            && self.only_b.is_empty()
            && self.tables.is_empty()
            && self.views_changed.is_empty()
            && self.functions_only_a.is_empty()
            && self.functions_only_b.is_empty()
            && self.functions_changed.is_empty()
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
    diff_with(a, b, None)
}

/// [`diff`], and with both sides' [`Definitions`], their CHECKs,
/// views' definitions and functions too. Pure.
pub fn diff_with(
    a: &SchemaCache,
    b: &SchemaCache,
    defs: Option<(&Definitions, &Definitions)>,
) -> Diff {
    let (ra, rb) = (relations(a), relations(b));
    let mut out = Diff::default();
    // A table on one side and a view of its name on the other is one of
    // each, not the same relation: `app.tv` and `app.tv (view)`.
    for (k, view) in &ra {
        if rb.get(k) != Some(view) {
            out.only_a.push(named(k, *view));
        }
    }
    for (k, view) in &rb {
        if ra.get(k) != Some(view) {
            out.only_b.push(named(k, *view));
        }
    }
    for key in ra.keys().filter(|k| rb.get(*k) == ra.get(*k)) {
        let mut t = table_diff(a, b, key);
        if let Some((da, db)) = defs {
            checks_diff(&mut t, da, db, key);
        }
        if !t.is_empty() {
            out.tables.push(t);
        }
    }
    if let Some((da, db)) = defs {
        out.definitions_compared = true;
        for (key, view) in &ra {
            if !view || !rb.contains_key(key) {
                continue;
            }
            if let (Some(va), Some(vb)) = (da.views.get(key), db.views.get(key)) {
                if va != vb {
                    out.views_changed.push(Changed {
                        name: format!("{}.{}", key.0, key.1),
                        a: va.clone(),
                        b: vb.clone(),
                    });
                }
            }
        }
        // Functions in an extension's schema are its own, as its tables
        // are.
        let mine = |c: &SchemaCache, sig: &String| {
            !c.extension_schemas
                .iter()
                .any(|s| sig.starts_with(&format!("{s}.")))
        };
        for (sig, fa) in da.functions.iter().filter(|(s, _)| mine(a, s)) {
            match db.functions.get(sig) {
                None => out.functions_only_a.push(sig.clone()),
                Some(fb) if fb != fa => out.functions_changed.push(Changed {
                    name: sig.clone(),
                    a: fa.clone(),
                    b: fb.clone(),
                }),
                Some(_) => {}
            }
        }
        for sig in db.functions.keys().filter(|s| mine(b, s)) {
            if !da.functions.contains_key(sig) {
                out.functions_only_b.push(sig.clone());
            }
        }
    }
    out
}

/// [`diff_with`] over what each side's reads gave: the definitions are
/// compared only when both were read, and each that wasn't is named in
/// a note (labelled `la` / `lb`), so no function reads as dropped for a
/// read that failed. Pure.
pub fn diff_reads(
    (a, da, la): (&SchemaCache, &Result<Definitions, String>, &str),
    (b, db, lb): (&SchemaCache, &Result<Definitions, String>, &str),
) -> (Diff, Vec<String>) {
    let mut notes = Vec::new();
    for (label, read) in [(la, da), (lb, db)] {
        if let Err(e) = read {
            notes.push(format!(
                "{label}'s checks, views and functions couldn't be read ({e}): not compared"
            ));
        }
    }
    let defs = match (da, db) {
        (Ok(da), Ok(db)) => Some((da, db)),
        _ => None,
    };
    (diff_with(a, b, defs), notes)
}

/// A dry run's schema changes, from its reads before and after the
/// patches, and notes on what wasn't compared. None when a catalog read
/// failed on either side (a timeout, a permission gap): its empty part
/// would read as every key or index dropped. A table another session's
/// lock kept the indexes of out, on either side, has its indexes left
/// out of the comparison. Otherwise [`diff_reads`]. Pure.
pub fn dry_run_changes(
    (a, da): (&SchemaCache, &Result<Definitions, String>),
    (b, db): (&SchemaCache, &Result<Definitions, String>),
) -> (Option<Diff>, Vec<String>) {
    let sides = [("before", a), ("after", b)];
    let cut: Vec<String> = sides
        .iter()
        .filter(|(_, c)| c.reads_failed)
        .map(|(label, c)| {
            format!(
                "the schema read {label} the patches was incomplete ({}): not compared",
                c.incomplete.as_deref().unwrap_or("a catalog read failed")
            )
        })
        .collect();
    if !cut.is_empty() {
        return (None, cut);
    }
    let (mut d, mut notes) = diff_reads((a, da, "before"), (b, db, "after"));
    for (label, c) in sides {
        if let (Some(why), false) = (&c.incomplete, c.skipped.is_empty()) {
            notes.push(format!(
                "the schema read {label} the patches skipped locked tables ({why}): their indexes not compared"
            ));
        }
    }
    let skipped: std::collections::HashSet<String> = a
        .skipped
        .iter()
        .chain(&b.skipped)
        .map(|(s, t)| format!("{s}.{t}"))
        .collect();
    for t in d.tables.iter_mut().filter(|t| skipped.contains(&t.table)) {
        t.indexes_only_a.clear();
        t.indexes_only_b.clear();
        t.indexes_changed.clear();
    }
    d.tables.retain(|t| !t.is_empty());
    (Some(d), notes)
}

/// `key`'s CHECK constraints in `da` and `db`, into `t`.
fn checks_diff(t: &mut TableDiff, da: &Definitions, db: &Definitions, key: &Key) {
    let empty = BTreeMap::new();
    let ca = da.checks.get(key).unwrap_or(&empty);
    let cb = db.checks.get(key).unwrap_or(&empty);
    for (name, def) in ca {
        match cb.get(name) {
            None => t.checks_only_a.push(format!("{name} {def}")),
            Some(other) if other != def => {
                t.checks_changed.push(format!("{name}: {def} → {other}"))
            }
            Some(_) => {}
        }
    }
    for (name, def) in cb {
        if !ca.contains_key(name) {
            t.checks_only_b.push(format!("{name} {def}"));
        }
    }
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
        let also = if d.definitions_compared {
            ", checks, views and functions"
        } else {
            ""
        };
        return format!("{la} and {lb} have the same tables, columns, keys and indexes{also}\n");
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
        sub(&mut out, &format!("check only in {la}:"), &t.checks_only_a);
        sub(&mut out, &format!("check only in {lb}:"), &t.checks_only_b);
        sub(&mut out, "check changed:", &t.checks_changed);
    }
    let names = |c: &[Changed]| c.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
    if !d.views_changed.is_empty() {
        out.push('\n');
    }
    list(
        &mut out,
        "view defined differently:".to_string(),
        &names(&d.views_changed),
    );
    if !(d.functions_only_a.is_empty()
        && d.functions_only_b.is_empty()
        && d.functions_changed.is_empty())
    {
        out.push('\n');
    }
    list(
        &mut out,
        format!("function only in {la}:"),
        &d.functions_only_a,
    );
    list(
        &mut out,
        format!("function only in {lb}:"),
        &d.functions_only_b,
    );
    list(
        &mut out,
        "function defined differently:".to_string(),
        &names(&d.functions_changed),
    );
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
        for c in &t.checks_only_b {
            line('+', format!("check on {on}: {c}"));
        }
        for c in &t.checks_only_a {
            line('-', format!("check on {on}: {c}"));
        }
        for c in &t.checks_changed {
            line('~', format!("check on {on}: {c}"));
        }
    }
    for v in &d.views_changed {
        line('~', format!("view {} redefined", v.name));
    }
    for f in &d.functions_only_b {
        line('+', format!("function {f}"));
    }
    for f in &d.functions_only_a {
        line('-', format!("function {f}"));
    }
    for f in &d.functions_changed {
        line('~', format!("function {} redefined", f.name));
    }
    out
}

/// The diff as MCP's `schema_diff` returns it. Pure.
pub fn to_json(d: &Diff, la: &str, lb: &str) -> Value {
    let changed = |c: &[Changed]| -> Vec<Value> {
        c.iter()
            .map(|c| json!({"name": c.name, "a": c.a, "b": c.b}))
            .collect()
    };
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
                "checks_only_a": t.checks_only_a,
                "checks_only_b": t.checks_only_b,
                "checks_changed": t.checks_changed,
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
        "definitions_compared": d.definitions_compared,
        "views_changed": changed(&d.views_changed),
        "functions_only_a": d.functions_only_a,
        "functions_only_b": d.functions_only_b,
        "functions_changed": changed(&d.functions_changed),
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
            constraint: String::new(),
            ord: 1,
        });
        c
    }

    /// `base()`'s definitions: a CHECK on orders, a view, two functions
    /// and one in the extension's schema.
    fn defs() -> Definitions {
        let mut d = Definitions::default();
        d.checks.insert(
            key("orders"),
            [(
                "orders_total_check".to_string(),
                "CHECK ((total >= (0)::numeric))".to_string(),
            )]
            .into(),
        );
        d.views
            .insert(key("open_orders"), " SELECT id FROM orders;".to_string());
        for (sig, body) in [
            (
                "public.touch()",
                "CREATE OR REPLACE FUNCTION public.touch() … NEW.at := now(); …",
            ),
            (
                "public.total(bigint)",
                "CREATE OR REPLACE FUNCTION public.total(bigint) … sum …",
            ),
            (
                "ext.st_area(geometry)",
                "CREATE OR REPLACE FUNCTION ext.st_area(geometry) …",
            ),
        ] {
            d.functions.insert(sig.to_string(), body.to_string());
        }
        d
    }

    fn with_view(mut c: SchemaCache) -> SchemaCache {
        c.tables.push(TableMeta {
            schema: "public".into(),
            name: "open_orders".into(),
        });
        c.views.insert(key("open_orders"));
        c
    }

    /// CHECKs per table, views defined differently and functions on one
    /// side or defined differently, when both sides' definitions were
    /// read; an extension's functions left out, as its tables are.
    #[test]
    fn checks_views_and_functions_are_compared_when_read() {
        let (a, b) = (with_view(base()), with_view(base()));
        let (da, mut db) = (defs(), defs());
        let same = diff_with(&a, &b, Some((&da, &db)));
        assert!(same.is_empty(), "{same:?}");
        assert!(render_text(&same, "a", "b").contains("checks, views and functions"));
        let checks = db.checks.get_mut(&key("orders")).unwrap();
        checks.insert(
            "orders_total_check".into(),
            "CHECK ((total > (0)::numeric))".into(),
        );
        checks.insert("orders_id_check".into(), "CHECK ((id > 0))".into());
        db.views
            .insert(key("open_orders"), " SELECT id, total FROM orders;".into());
        db.functions.remove("public.touch()");
        db.functions.insert(
            "public.total(bigint)".into(),
            "CREATE OR REPLACE FUNCTION public.total(bigint) … max …".into(),
        );
        db.functions
            .insert("public.audit()".into(), "CREATE OR REPLACE …".into());
        db.functions.remove("ext.st_area(geometry)");
        let d = diff_with(&a, &b, Some((&da, &db)));
        let orders = d
            .tables
            .iter()
            .find(|t| t.table == "public.orders")
            .unwrap();
        assert_eq!(orders.checks_only_b, ["orders_id_check CHECK ((id > 0))"]);
        assert_eq!(
            orders.checks_changed,
            ["orders_total_check: CHECK ((total >= (0)::numeric)) → CHECK ((total > (0)::numeric))"]
        );
        assert!(orders.checks_only_a.is_empty());
        assert_eq!(d.views_changed.len(), 1);
        assert_eq!(d.views_changed[0].name, "public.open_orders");
        assert_eq!(d.functions_only_a, ["public.touch()"]);
        assert_eq!(d.functions_only_b, ["public.audit()"]);
        assert_eq!(d.functions_changed.len(), 1);
        assert_eq!(d.functions_changed[0].name, "public.total(bigint)");
        let text = render_text(&d, "staging", "production");
        for want in [
            "check only in production: orders_id_check",
            "check changed: orders_total_check",
            "view defined differently:\n  public.open_orders",
            "function only in staging:\n  public.touch()",
            "function only in production:\n  public.audit()",
            "function defined differently:\n  public.total(bigint)",
        ] {
            assert!(text.contains(want), "{want:?} in\n{text}");
        }
        let changes = render_changes(&d);
        for want in [
            "+ check on public.orders: orders_id_check",
            "~ view public.open_orders redefined",
            "- function public.touch()",
            "+ function public.audit()",
            "~ function public.total(bigint) redefined",
        ] {
            assert!(changes.contains(want), "{want:?} in\n{changes}");
        }
        let v = to_json(&d, "a", "b");
        assert_eq!(v["functions_only_b"][0], "public.audit()");
        assert_eq!(v["views_changed"][0]["b"], " SELECT id, total FROM orders;");
        assert_eq!(v["definitions_compared"], true);
        // Without the definitions, none of it.
        let d = diff(&a, &b);
        assert!(d.is_empty() && !d.definitions_compared, "{d:?}");
    }

    /// A side whose definitions couldn't be read: nothing of them
    /// compared, and a note says which.
    #[test]
    fn a_failed_definitions_read_is_not_a_dropped_function() {
        let c = base();
        let (ok, failed) = (Ok(defs()), Err("permission denied".to_string()));
        let (d, notes) = diff_reads((&c, &ok, "staging"), (&c, &failed, "production"));
        assert!(d.is_empty() && !d.definitions_compared, "{d:?}");
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].starts_with(
                "production's checks, views and functions couldn't be read (permission denied)"
            ),
            "{notes:?}"
        );
        let (d, notes) = diff_reads((&c, &ok, "a"), (&c, &ok, "b"));
        assert!(d.definitions_compared && notes.is_empty());
    }

    /// A dry run whose read after the patches was cut short shows no
    /// schema changes, and says why: its empty buckets aren't drops.
    #[test]
    fn a_cut_short_dry_run_read_shows_no_changes() {
        let c = base();
        let mut cut = base();
        cut.incomplete = Some("primary keys gave up at lock_timeout".into());
        cut.reads_failed = true;
        let ok: Result<Definitions, String> = Ok(defs());
        let (d, notes) = dry_run_changes((&c, &ok), (&cut, &ok));
        assert!(d.is_none(), "{d:?}");
        assert_eq!(
            notes,
            ["the schema read after the patches was incomplete (primary keys gave up at lock_timeout): not compared"]
        );
        let (d, notes) = dry_run_changes((&c, &ok), (&c, &ok));
        assert!(d.is_some_and(|d| d.is_empty()) && notes.is_empty());
        // One table locked on one side: its indexes aren't compared, the
        // rest is, and a note says so.
        let mut locked = base();
        locked.indexes_by_table.remove(&key("orders"));
        locked.skipped.insert(key("orders"));
        locked.incomplete = Some("pid 7 holds ACCESS EXCLUSIVE".into());
        let (d, notes) = dry_run_changes((&c, &ok), (&locked, &ok));
        assert!(d.as_ref().is_some_and(Diff::is_empty), "{d:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("their indexes not compared"), "{notes:?}");
    }

    /// A table on one side and a view of its name on the other are
    /// named on each side, not compared as one relation.
    #[test]
    fn a_table_become_a_view_is_named_on_each_side() {
        let a = base();
        let mut b = base();
        b.views.insert(key("orders"));
        let d = diff(&a, &b);
        assert_eq!(d.only_a, ["public.orders"]);
        assert_eq!(d.only_b, ["public.orders (view)"]);
        assert!(d.tables.is_empty(), "{:?}", d.tables);
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
