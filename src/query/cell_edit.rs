//! Cell editing: which table row a grid cell belongs to, and the one-row
//! `UPDATE` that changes it. Pure — the caller hands in the schema cache,
//! the grid's inferred FROM table and its column origins; the statement
//! runs through `safety.rs` and `conn::run_single_row_update`.
//!
//! The design is in `docs/backlog/cell-editing.md`. In short: a column is
//! a table's column only if the server's RowDescription says so
//! ([`ColumnOrigin`]) — never because of its name — and a row is named by
//! its table's real primary key, every column of which must be in the
//! result. Anything else is a [`Refusal`] that says why.
//!
//! [`resolve_key`] is shared with the result diff, which keys rows on the
//! same primary key when it can.

use crate::app::{format_sql_literal, quote_ident};
use crate::grid::{Cell, ColumnOrigin};
use crate::query::schema::SchemaCache;

/// The table a result was read from and where its primary key sits in
/// the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableKey {
    pub schema: String,
    pub table: String,
    /// The table's OID, as the result's RowDescription named it.
    pub table_oid: u32,
    /// Per primary-key column, in key order: its index in the grid, its
    /// catalog name and its attnum.
    pub pk: Vec<(usize, String, i16)>,
}

/// One planned cell edit: the row (by primary key), the column (by its
/// catalog name, whatever the result called it) and its value as shown.
///
/// The names come from the schema cache, which can be older than the
/// result: a column renamed since pgman loaded it, and another added
/// under its old name, would have the UPDATE write the wrong column. So
/// the edit also carries what the server itself said the result was
/// read from — the table's OID and each column's attnum — and the write
/// checks those still carry these names ([`expected_identity`],
/// [`check_identity`]) before it commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellEdit {
    pub schema: String,
    pub table: String,
    pub column: String,
    /// `(column, value)` per primary-key column, in key order.
    pub key: Vec<(String, String)>,
    /// The table's OID (the result's RowDescription).
    pub table_oid: u32,
    /// `column`'s attnum (the result's RowDescription).
    pub attnum: i16,
    /// Each key column's attnum, in `key` order.
    pub key_attnums: Vec<i16>,
    /// The cell as the grid shows it now.
    pub old: Cell,
    /// Index of the row in `grid.rows`.
    pub row: usize,
    /// Index of the column in the grid.
    pub col: usize,
}

/// Why a cell can't be edited (or a diff can't key on the primary key).
/// `Display` is the footer line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The query wasn't a SELECT from one table: a join, a subquery, no
    /// FROM, a write.
    NotSingleTable,
    /// The grid has no column origins: a panel, EXPLAIN, a script, the
    /// start card, `--demo`.
    NoOrigins,
    /// The column is an expression, not a table column.
    Computed {
        column: String,
    },
    /// The origin's table (or column) isn't in the schema cache.
    StaleCache,
    /// The columns were read from a different relation than the FROM
    /// clause names (a CTE).
    OtherTable {
        from: String,
        actual: String,
    },
    /// A view, materialized view or foreign table.
    View {
        table: String,
    },
    NoPrimaryKey {
        table: String,
    },
    /// Primary-key columns with no plain column in the result.
    PkNotSelected {
        table: String,
        missing: Vec<String>,
    },
    /// A primary-key cell of this row is NULL (a short row padded out).
    NullKey {
        column: String,
    },
    /// The value holds a newline or another control character, which a
    /// one-line input can't show faithfully.
    MultiLine {
        column: String,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotSingleTable => write!(
                f,
                "can't edit: not a SELECT from one table (a join, subquery or no FROM)"
            ),
            Refusal::NoOrigins => write!(
                f,
                "can't edit: only a SELECT run from the editor can be edited"
            ),
            Refusal::Computed { column } => {
                write!(f, "can't edit: '{column}' is computed, not a table column")
            }
            Refusal::StaleCache => write!(
                f,
                "can't edit: the table isn't in the schema cache · S then r reloads it"
            ),
            Refusal::OtherTable { from, actual } => {
                write!(f, "can't edit: the columns come from {actual}, not {from}")
            }
            Refusal::View { table } => {
                write!(f, "can't edit: {table} is a view · edit the table under it")
            }
            Refusal::NoPrimaryKey { table } => write!(
                f,
                "can't edit: {table} has no primary key to find the row by"
            ),
            // The table is the one the operator just queried; naming it
            // pushed the line past 80 columns.
            Refusal::PkNotSelected { missing, .. } => write!(
                f,
                "can't edit: the result lacks the primary key · select {} too",
                missing.join(", ")
            ),
            Refusal::NullKey { column } => {
                write!(f, "can't edit: the row's key column '{column}' is NULL")
            }
            Refusal::MultiLine { column } => write!(
                f,
                "can't edit: '{column}' isn't one line · write the UPDATE in the editor"
            ),
        }
    }
}

/// The table the grid's rows come from and where its primary key is in
/// the result. `anchor` is the column being edited: the table is the one
/// its origin names, and it has to have one. Without an anchor (the
/// result diff) the first column with an origin decides.
///
/// `source` is the FROM clause's single table (`infer_single_source_table`)
/// and only gates "one table, no join"; the schema comes from the OID,
/// which is what the server actually read (an unqualified `users` may be
/// `app.users` through the search_path).
pub fn resolve_key(
    cache: &SchemaCache,
    source: Option<&(String, String)>,
    columns: &[String],
    origins: &[Option<ColumnOrigin>],
    anchor: Option<usize>,
) -> Result<TableKey, Refusal> {
    let (_, from_table) = source.ok_or(Refusal::NotSingleTable)?;
    if origins.is_empty() {
        return Err(Refusal::NoOrigins);
    }
    let table_oid = match anchor {
        Some(col) => {
            origins
                .get(col)
                .copied()
                .flatten()
                .ok_or_else(|| Refusal::Computed {
                    column: columns.get(col).cloned().unwrap_or_default(),
                })?
                .table_oid
        }
        None => {
            origins
                .iter()
                .flatten()
                .next()
                .ok_or(Refusal::NoOrigins)?
                .table_oid
        }
    };
    let (schema, table) = cache
        .relations_by_oid
        .get(&table_oid)
        .ok_or(Refusal::StaleCache)?;
    let qualified = format!("{schema}.{table}");
    if table != from_table {
        return Err(Refusal::OtherTable {
            from: from_table.clone(),
            actual: qualified,
        });
    }
    if cache.views.contains(&(schema.clone(), table.clone())) {
        return Err(Refusal::View { table: qualified });
    }
    let pk_names = cache
        .primary_key(schema, table)
        .ok_or_else(|| Refusal::NoPrimaryKey {
            table: qualified.clone(),
        })?;
    let mut pk = Vec::with_capacity(pk_names.len());
    let mut missing = Vec::new();
    for name in pk_names {
        let attnum = cache
            .columns_meta_by_table
            .get(&(schema.clone(), table.clone()))
            .and_then(|cols| cols.iter().find(|c| &c.name == name))
            .map(|c| c.attnum)
            .ok_or(Refusal::StaleCache)?;
        let want = ColumnOrigin { table_oid, attnum };
        match origins.iter().position(|o| *o == Some(want)) {
            Some(idx) => pk.push((idx, name.clone(), attnum)),
            None => missing.push(name.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(Refusal::PkNotSelected {
            table: qualified,
            missing,
        });
    }
    Ok(TableKey {
        schema: schema.clone(),
        table: table.clone(),
        table_oid,
        pk,
    })
}

/// Plan editing the cell at (`row`, `col`): the table and row it belongs
/// to, the catalog name of its column, and its value now. `row_cells` is
/// `grid.rows[row]`.
pub fn plan_cell_edit(
    cache: &SchemaCache,
    source: Option<&(String, String)>,
    columns: &[String],
    origins: &[Option<ColumnOrigin>],
    row_cells: &[Cell],
    row: usize,
    col: usize,
) -> Result<CellEdit, Refusal> {
    let key = resolve_key(cache, source, columns, origins, Some(col))?;
    let shown = columns.get(col).cloned().unwrap_or_default();
    let attnum = origins
        .get(col)
        .copied()
        .flatten()
        .map(|o| o.attnum)
        .ok_or_else(|| Refusal::Computed {
            column: shown.clone(),
        })?;
    let column = cache
        .column_by_attnum(&key.schema, &key.table, attnum)
        .ok_or(Refusal::StaleCache)?
        .name
        .clone();
    let old = row_cells.get(col).cloned().flatten();
    if old
        .as_deref()
        .is_some_and(|v| v.chars().any(char::is_control))
    {
        return Err(Refusal::MultiLine { column: shown });
    }
    let mut values = Vec::with_capacity(key.pk.len());
    let mut key_attnums = Vec::with_capacity(key.pk.len());
    for (idx, name, key_attnum) in &key.pk {
        let Some(v) = row_cells.get(*idx).cloned().flatten() else {
            return Err(Refusal::NullKey {
                column: name.clone(),
            });
        };
        values.push((name.clone(), v));
        key_attnums.push(*key_attnum);
    }
    Ok(CellEdit {
        schema: key.schema,
        table: key.table,
        column,
        key: values,
        table_oid: key.table_oid,
        attnum,
        key_attnums,
        old,
        row,
        col,
    })
}

/// `UPDATE schema.table SET col = <new> WHERE pk1 = … AND pk2 = …
/// RETURNING col`. Identifiers through [`quote_ident`], values through
/// [`format_sql_literal`] (`NULL`, `''`, `E'…'` for a backslash; every
/// value quoted, which Postgres reads as the column's type). `RETURNING`
/// hands back the value as the server stored and renders it, and lets
/// the runner count the rows the statement touched.
pub fn build_update_sql(edit: &CellEdit, new: Option<&str>) -> String {
    let column = quote_ident(&edit.column);
    let wheres = edit
        .key
        .iter()
        .map(|(name, value)| {
            format!(
                "{} = {}",
                quote_ident(name),
                format_sql_literal(Some(value))
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    format!(
        "UPDATE {}.{} SET {column} = {} WHERE {wheres} RETURNING {column}",
        quote_ident(&edit.schema),
        quote_ident(&edit.table),
        format_sql_literal(new),
    )
}

/// What the server must still call the edit's table and columns when
/// its UPDATE runs: per attnum (`0` for the table itself), the names
/// `pg_identify_object_as_address` gives it — `[schema, table]` for the
/// table, `[schema, table, column]` for a column. The edited column
/// comes first, then the key in key order.
pub fn expected_identity(edit: &CellEdit) -> Vec<(i16, Vec<String>)> {
    let names = |column: Option<&str>| {
        let mut v = vec![edit.schema.clone(), edit.table.clone()];
        v.extend(column.map(str::to_string));
        v
    };
    let mut out = vec![(0, names(None)), (edit.attnum, names(Some(&edit.column)))];
    for ((name, _), attnum) in edit.key.iter().zip(&edit.key_attnums) {
        out.push((*attnum, names(Some(name))));
    }
    out
}

/// What the server calls one attnum of the edit's table now: the
/// attnum and its `pg_identify_object_as_address` names (`None`:
/// nothing by that number).
pub type ServerName = (i16, Option<Vec<String>>);

/// The guard against a schema cache older than the table: `server` is
/// what the server calls each of [`expected_identity`]'s attnums now
/// (`None`: nothing by that number), read inside the UPDATE's own
/// transaction after the UPDATE took its lock, so no ALTER TABLE can
/// slip in between. Any difference — a renamed table or column, one
/// dropped, or a key length that doesn't match — is the message the
/// rollback carries. Pure.
pub fn check_identity(edit: &CellEdit, server: &[ServerName]) -> Result<(), String> {
    if edit.key_attnums.len() != edit.key.len() {
        return Err(table_changed("its key changed"));
    }
    for (attnum, want) in expected_identity(edit) {
        let now = server
            .iter()
            .find(|(n, _)| *n == attnum)
            .and_then(|(_, names)| names.as_ref());
        if now == Some(&want) {
            continue;
        }
        let what = want.last().cloned().unwrap_or_default();
        let since = match now.and_then(|n| n.last()) {
            _ if attnum == 0 => format!("{} was renamed or dropped", want.join(".")),
            Some(name) if name.starts_with("........pg.dropped") => format!("'{what}' was dropped"),
            Some(name) => format!("'{what}' is now '{name}'"),
            None => format!("'{what}' is gone"),
        };
        return Err(table_changed(&since));
    }
    Ok(())
}

/// [`check_identity`]'s refusal: what changed, how to catch up, and
/// that nothing was written.
fn table_changed(since: &str) -> String {
    format!(
        "the table changed since pgman read it ({since}) · S then r reloads the schema · rolled back, nothing changed"
    )
}

/// `id = 3, region = eu` — the row's key as the edit popup shows it.
pub fn key_summary(edit: &CellEdit) -> String {
    edit.key
        .iter()
        .map(|(name, value)| format!("{name} = {value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one-row guard: an edit commits only when its UPDATE touched
/// exactly one row. Anything else is the message the rollback carries.
pub fn check_one_row(rows: u64) -> Result<(), String> {
    match rows {
        1 => Ok(()),
        0 => Err("the UPDATE matched no row (deleted, or its key changed, since the query ran) · rolled back, nothing changed".into()),
        n => Err(format!(
            "the UPDATE matched {n} rows, not 1 · rolled back, nothing changed"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::schema::ColumnMeta;

    const USERS: u32 = 100;
    const VIEW: u32 = 200;
    const LINES: u32 = 300;
    const KEYLESS: u32 = 400;

    fn meta(cols: &[(&str, i16)]) -> Vec<ColumnMeta> {
        cols.iter()
            .map(|(name, attnum)| ColumnMeta {
                name: name.to_string(),
                type_name: "text".into(),
                not_null: false,
                attnum: *attnum,
            })
            .collect()
    }

    /// `public.users (id, name, "Nick")` keyed on `id`; `public.user_v`
    /// a view; `public.lines (order_id, line_no, sku)` keyed on
    /// `(order_id, line_no)`; `public.keyless (v)` with no key.
    /// `"Nick"` is attnum 4: attnum 3 was dropped.
    fn cache() -> SchemaCache {
        let mut c = SchemaCache::default();
        let key = |t: &str| ("public".to_string(), t.to_string());
        for (oid, t, cols) in [
            (USERS, "users", meta(&[("id", 1), ("name", 2), ("Nick", 4)])),
            (VIEW, "user_v", meta(&[("id", 1), ("name", 2)])),
            (
                LINES,
                "lines",
                meta(&[("order_id", 1), ("line_no", 2), ("sku", 3)]),
            ),
            (KEYLESS, "keyless", meta(&[("v", 1)])),
        ] {
            c.relations_by_oid.insert(oid, key(t));
            c.columns_meta_by_table.insert(key(t), cols);
        }
        c.views.insert(key("user_v"));
        c.primary_keys.insert(key("users"), vec!["id".into()]);
        c.primary_keys
            .insert(key("lines"), vec!["order_id".into(), "line_no".into()]);
        c
    }

    fn o(table_oid: u32, attnum: i16) -> Option<ColumnOrigin> {
        Some(ColumnOrigin { table_oid, attnum })
    }

    fn src(t: &str) -> Option<(String, String)> {
        Some(("public".into(), t.into()))
    }

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn cells(values: &[Option<&str>]) -> Vec<Cell> {
        values.iter().map(|v| v.map(str::to_string)).collect()
    }

    #[test]
    fn a_cell_of_a_keyed_table_plans_by_catalog_names() {
        // `SELECT "Nick" AS handle, id FROM users`: the alias is not the
        // column, and the key is found by origin, not position.
        let edit = plan_cell_edit(
            &cache(),
            src("users").as_ref(),
            &cols(&["handle", "id"]),
            &[o(USERS, 4), o(USERS, 1)],
            &cells(&[Some("ann"), Some("7")]),
            3,
            0,
        )
        .unwrap();
        assert_eq!(
            edit,
            CellEdit {
                schema: "public".into(),
                table: "users".into(),
                column: "Nick".into(),
                key: vec![("id".into(), "7".into())],
                old: Some("ann".into()),
                row: 3,
                col: 0,
                table_oid: USERS,
                attnum: 4,
                key_attnums: vec![1],
            }
        );
        assert_eq!(
            build_update_sql(&edit, Some("bo")),
            r#"UPDATE public.users SET "Nick" = 'bo' WHERE id = '7' RETURNING "Nick""#
        );
        assert_eq!(key_summary(&edit), "id = 7");
    }

    /// `SELECT id + 1 AS id, name FROM users`: the column called `id` is
    /// not the key — editing by name would write to the next user.
    #[test]
    fn a_computed_column_named_like_the_key_is_not_the_key() {
        let r = plan_cell_edit(
            &cache(),
            src("users").as_ref(),
            &cols(&["id", "name"]),
            &[None, o(USERS, 2)],
            &cells(&[Some("8"), Some("ann")]),
            0,
            1,
        );
        assert_eq!(
            r,
            Err(Refusal::PkNotSelected {
                table: "public.users".into(),
                missing: vec!["id".into()],
            })
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            "can't edit: the result lacks the primary key · select id too"
        );
    }

    #[test]
    fn a_computed_cell_is_refused_by_its_shown_name() {
        let r = plan_cell_edit(
            &cache(),
            src("users").as_ref(),
            &cols(&["id", "upper"]),
            &[o(USERS, 1), None],
            &cells(&[Some("1"), Some("ANN")]),
            0,
            1,
        );
        assert_eq!(
            r,
            Err(Refusal::Computed {
                column: "upper".into()
            })
        );
    }

    #[test]
    fn a_composite_key_needs_every_column_and_names_the_missing_ones() {
        let r = resolve_key(
            &cache(),
            src("lines").as_ref(),
            &cols(&["sku"]),
            &[o(LINES, 3)],
            Some(0),
        );
        assert_eq!(
            r.unwrap_err().to_string(),
            "can't edit: the result lacks the primary key · select order_id, line_no too"
        );
        let edit = plan_cell_edit(
            &cache(),
            src("lines").as_ref(),
            &cols(&["line_no", "sku", "order_id"]),
            &[o(LINES, 2), o(LINES, 3), o(LINES, 1)],
            &cells(&[Some("2"), Some("A-1"), Some("10")]),
            0,
            1,
        )
        .unwrap();
        // Key order, not result order.
        assert_eq!(
            build_update_sql(&edit, None),
            "UPDATE public.lines SET sku = NULL WHERE order_id = '10' AND line_no = '2' RETURNING sku"
        );
    }

    #[test]
    fn refusals_say_why() {
        let c = cache();
        let users_cols = cols(&["id", "name"]);
        let users_origins = [o(USERS, 1), o(USERS, 2)];
        // A join (or anything else the FROM parse can't pin to one table).
        assert_eq!(
            resolve_key(&c, None, &users_cols, &users_origins, Some(1)),
            Err(Refusal::NotSingleTable)
        );
        // A grid with no origins.
        assert_eq!(
            resolve_key(&c, src("users").as_ref(), &users_cols, &[], Some(1)),
            Err(Refusal::NoOrigins)
        );
        // A view.
        assert_eq!(
            resolve_key(
                &c,
                src("user_v").as_ref(),
                &users_cols,
                &[o(VIEW, 1), o(VIEW, 2)],
                Some(1)
            ),
            Err(Refusal::View {
                table: "public.user_v".into()
            })
        );
        // No primary key.
        assert_eq!(
            resolve_key(
                &c,
                src("keyless").as_ref(),
                &cols(&["v"]),
                &[o(KEYLESS, 1)],
                Some(0)
            ),
            Err(Refusal::NoPrimaryKey {
                table: "public.keyless".into()
            })
        );
        // A CTE named `x` over users.
        assert_eq!(
            resolve_key(&c, src("x").as_ref(), &users_cols, &users_origins, Some(1)),
            Err(Refusal::OtherTable {
                from: "x".into(),
                actual: "public.users".into()
            })
        );
        // A table the cache doesn't know.
        assert_eq!(
            resolve_key(
                &c,
                src("users").as_ref(),
                &users_cols,
                &[o(999, 1), o(999, 2)],
                Some(1)
            ),
            Err(Refusal::StaleCache)
        );
    }

    /// The FROM clause said `public.users` (its default for a bare name);
    /// the server read `app.users` through the search_path. The OID wins.
    #[test]
    fn the_schema_comes_from_the_oid_not_the_from_clause() {
        let mut c = cache();
        c.relations_by_oid
            .insert(500, ("app".into(), "users".into()));
        c.columns_meta_by_table.insert(
            ("app".into(), "users".into()),
            meta(&[("id", 1), ("name", 2)]),
        );
        c.primary_keys
            .insert(("app".into(), "users".into()), vec!["id".into()]);
        let key = resolve_key(
            &c,
            src("users").as_ref(),
            &cols(&["id", "name"]),
            &[o(500, 1), o(500, 2)],
            Some(1),
        )
        .unwrap();
        assert_eq!((key.schema.as_str(), key.table.as_str()), ("app", "users"));
    }

    #[test]
    fn a_value_with_a_control_character_is_refused() {
        for v in ["two\nlines", "tab\there", "cr\r"] {
            let r = plan_cell_edit(
                &cache(),
                src("users").as_ref(),
                &cols(&["id", "name"]),
                &[o(USERS, 1), o(USERS, 2)],
                &cells(&[Some("1"), Some(v)]),
                0,
                1,
            );
            assert_eq!(
                r,
                Err(Refusal::MultiLine {
                    column: "name".into()
                }),
                "{v:?}"
            );
        }
    }

    #[test]
    fn a_null_key_cell_is_refused() {
        let r = plan_cell_edit(
            &cache(),
            src("users").as_ref(),
            &cols(&["id", "name"]),
            &[o(USERS, 1), o(USERS, 2)],
            &cells(&[None, Some("ann")]),
            0,
            1,
        );
        assert_eq!(
            r,
            Err(Refusal::NullKey {
                column: "id".into()
            })
        );
    }

    /// NULL, the empty string, quotes and backslashes each reach the
    /// statement as themselves, and a mixed-case table and key column
    /// stay quoted.
    #[test]
    fn values_and_names_are_quoted_as_the_insert_yank_quotes_them() {
        let edit = CellEdit {
            schema: "public".into(),
            table: "Mixed Case".into(),
            column: "note".into(),
            key: vec![("Id".into(), "it's".into())],
            old: None,
            row: 0,
            col: 0,
            table_oid: 100,
            attnum: 2,
            key_attnums: vec![1],
        };
        assert_eq!(
            build_update_sql(&edit, Some("")),
            r#"UPDATE public."Mixed Case" SET note = '' WHERE "Id" = 'it''s' RETURNING note"#
        );
        assert_eq!(
            build_update_sql(&edit, None),
            r#"UPDATE public."Mixed Case" SET note = NULL WHERE "Id" = 'it''s' RETURNING note"#
        );
        assert_eq!(
            build_update_sql(&edit, Some(r"C:\tmp 'x'")),
            r#"UPDATE public."Mixed Case" SET note = E'C:\\tmp ''x''' WHERE "Id" = 'it''s' RETURNING note"#
        );
    }

    /// `public.users."Nick"` (attnum 4) where `id` (attnum 1) = 7, as
    /// the server would name each attnum when nothing changed.
    fn nick_edit() -> (CellEdit, Vec<ServerName>) {
        let edit = plan_cell_edit(
            &cache(),
            src("users").as_ref(),
            &cols(&["handle", "id"]),
            &[o(USERS, 4), o(USERS, 1)],
            &cells(&[Some("ann"), Some("7")]),
            0,
            0,
        )
        .unwrap();
        let names = |v: &[&str]| Some(v.iter().map(|s| s.to_string()).collect());
        let server = vec![
            (0, names(&["public", "users"])),
            (4, names(&["public", "users", "Nick"])),
            (1, names(&["public", "users", "id"])),
        ];
        (edit, server)
    }

    #[test]
    fn the_planned_names_are_what_the_server_is_asked_to_confirm() {
        let (edit, server) = nick_edit();
        assert_eq!(edit.table_oid, USERS);
        assert_eq!((edit.attnum, edit.key_attnums.clone()), (4, vec![1]));
        let want: Vec<_> = server
            .iter()
            .map(|(n, names)| (*n, names.clone().unwrap()))
            .collect();
        assert_eq!(expected_identity(&edit), want);
        assert_eq!(check_identity(&edit, &server), Ok(()));
    }

    /// A rename of the edited column, of a key column, or of the table,
    /// a dropped column or one the server has no name for at all: each
    /// says what changed, how to catch up, and that nothing was written.
    #[test]
    fn a_column_or_table_the_server_now_names_differently_is_refused() {
        let (edit, server) = nick_edit();
        let with = |at: usize, names: Option<&[&str]>| {
            let mut s = server.clone();
            s[at].1 = names.map(|v| v.iter().map(|x| x.to_string()).collect());
            check_identity(&edit, &s).unwrap_err()
        };
        let column = with(1, Some(&["public", "users", "nick"]));
        assert_eq!(
            column,
            "the table changed since pgman read it ('Nick' is now 'nick') · S then r reloads the schema · rolled back, nothing changed"
        );
        assert!(with(2, Some(&["public", "users", "old_id"])).contains("('id' is now 'old_id')"));
        assert!(with(
            1,
            Some(&["public", "users", "........pg.dropped.4........"])
        )
        .contains("('Nick' was dropped)"));
        assert!(with(1, None).contains("('Nick' is gone)"));
        assert!(
            with(0, Some(&["public", "people"])).contains("(public.users was renamed or dropped)")
        );
        assert!(with(0, None).contains("(public.users was renamed or dropped)"));
        // A table moved to another schema: the UPDATE's `public.users`
        // is no longer it.
        assert!(with(0, Some(&["app", "users"])).contains("(public.users was renamed or dropped)"));
        // An attnum the server didn't answer for at all.
        let mut short = server.clone();
        short.pop();
        assert!(check_identity(&edit, &short)
            .unwrap_err()
            .contains("('id' is gone)"));
        let mut broken = edit.clone();
        broken.key_attnums.clear();
        assert!(check_identity(&broken, &server)
            .unwrap_err()
            .contains("(its key changed)"));
    }

    #[test]
    fn only_exactly_one_row_passes_the_guard() {
        assert_eq!(check_one_row(1), Ok(()));
        assert!(check_one_row(0).unwrap_err().contains("matched no row"));
        assert!(check_one_row(2)
            .unwrap_err()
            .contains("matched 2 rows, not 1 · rolled back"));
    }
}
