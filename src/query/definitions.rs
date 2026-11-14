//! The schema objects [`crate::query::schema::fetch`] doesn't read, for
//! a schema diff: CHECK constraints, views' definitions and functions,
//! each as the server prints it (`pg_get_constraintdef`,
//! `pg_get_viewdef`, `pg_get_functiondef`). Read only by what compares
//! schemas (`pgman diff`, MCP `schema_diff`, a patch dry run), so the
//! TUI's connect doesn't pay for them. Extensions' own objects are left
//! out, as the schema diff leaves out their tables.

use std::collections::BTreeMap;

/// A relation: `(schema, name)`.
pub type Key = (String, String);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Definitions {
    /// Per table, its CHECK constraints: name → definition.
    pub checks: BTreeMap<Key, BTreeMap<String, String>>,
    /// Views and materialized views: their definitions.
    pub views: BTreeMap<Key, String>,
    /// Functions and procedures, `schema.name(argument types)` → the
    /// `CREATE OR REPLACE` the server prints.
    pub functions: BTreeMap<String, String>,
    /// The server's major version (`server_version_num / 10000`): each
    /// major prints a definition its own way.
    pub major: i32,
}

/// An object an extension owns (`pg_depend`'s `e`), by its catalog.
macro_rules! not_extension_member {
    ($catalog:literal, $oid:literal) => {
        concat!(
            "NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d WHERE d.classid = '",
            $catalog,
            "'::pg_catalog.regclass AND d.objid = ",
            $oid,
            " AND d.deptype = 'e')"
        )
    };
}

pub const CHECKS_SQL: &str = concat!(
    "SELECT n.nspname, c.relname, con.conname, pg_catalog.pg_get_constraintdef(con.oid) \
     FROM pg_catalog.pg_constraint con \
     JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     WHERE con.contype = 'c' \
       AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
       AND n.nspname NOT LIKE 'pg_temp%' AND ",
    not_extension_member!("pg_class", "c.oid"),
    " ORDER BY 1, 2, 3"
);

pub const VIEWS_SQL: &str = concat!(
    "SELECT n.nspname, c.relname, pg_catalog.pg_get_viewdef(c.oid, true) \
     FROM pg_catalog.pg_class c \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     WHERE c.relkind IN ('v', 'm') \
       AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
       AND n.nspname NOT LIKE 'pg_temp%' AND ",
    not_extension_member!("pg_class", "c.oid"),
    " ORDER BY 1, 2"
);

/// Functions and procedures, not aggregates or window functions
/// (`pg_get_functiondef` refuses an aggregate). `prokind` is Postgres
/// 11's; [`FUNCTIONS_SQL_10`] reads 10's flags.
pub const FUNCTIONS_SQL: &str = concat!(
    "SELECT n.nspname || '.' || p.proname || '(' || \
            pg_catalog.pg_get_function_identity_arguments(p.oid) || ')', \
            pg_catalog.pg_get_functiondef(p.oid) \
     FROM pg_catalog.pg_proc p \
     JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
     WHERE p.prokind IN ('f', 'p') \
       AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
       AND n.nspname NOT LIKE 'pg_temp%' AND ",
    not_extension_member!("pg_proc", "p.oid"),
    " ORDER BY 1"
);

pub const FUNCTIONS_SQL_10: &str = concat!(
    "SELECT n.nspname || '.' || p.proname || '(' || \
            pg_catalog.pg_get_function_identity_arguments(p.oid) || ')', \
            pg_catalog.pg_get_functiondef(p.oid) \
     FROM pg_catalog.pg_proc p \
     JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
     WHERE NOT p.proisagg AND NOT p.proiswindow \
       AND n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
       AND n.nspname NOT LIKE 'pg_temp%' AND ",
    not_extension_member!("pg_proc", "p.oid"),
    " ORDER BY 1"
);

/// Read them. An error from any read fails the whole: a diff that left
/// out the functions it couldn't read would report them as dropped.
pub async fn fetch(client: &tokio_postgres::Client) -> Result<Definitions, String> {
    let version: i32 = client
        .query_one(
            "SELECT pg_catalog.current_setting('server_version_num')::int",
            &[],
        )
        .await
        .map_err(|e| format!("server version: {e}"))?
        .get(0);
    let functions_sql = if version >= 110_000 {
        FUNCTIONS_SQL
    } else {
        FUNCTIONS_SQL_10
    };
    let (checks, views, functions) = tokio::join!(
        client.query(CHECKS_SQL, &[]),
        client.query(VIEWS_SQL, &[]),
        client.query(functions_sql, &[]),
    );
    let mut out = Definitions {
        major: version / 10_000,
        ..Definitions::default()
    };
    for row in checks.map_err(|e| format!("CHECK constraints: {e}"))? {
        out.checks
            .entry((row.get(0), row.get(1)))
            .or_default()
            .insert(row.get(2), row.get(3));
    }
    for row in views.map_err(|e| format!("view definitions: {e}"))? {
        // NULL for a view the role can't see the definition of: kept as
        // empty, so it compares equal to itself and not to a real one.
        let def: Option<String> = row.get(2);
        out.views
            .insert((row.get(0), row.get(1)), def.unwrap_or_default());
    }
    for row in functions.map_err(|e| format!("functions: {e}"))? {
        let def: Option<String> = row.get(1);
        out.functions.insert(row.get(0), def.unwrap_or_default());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each read leaves out the system schemas and extensions' objects,
    /// and qualifies the server's functions (a schema on the path ahead
    /// of pg_catalog can't answer them).
    #[test]
    fn the_reads_leave_out_system_and_extension_objects() {
        for sql in [CHECKS_SQL, VIEWS_SQL, FUNCTIONS_SQL, FUNCTIONS_SQL_10] {
            assert!(
                sql.contains("NOT IN ('pg_catalog','information_schema','pg_toast')"),
                "{sql}"
            );
            assert!(sql.contains("d.deptype = 'e'"), "{sql}");
            assert!(!sql.contains(" pg_get_"), "unqualified: {sql}");
        }
        assert!(FUNCTIONS_SQL.contains("p.prokind IN ('f', 'p')"));
        assert!(FUNCTIONS_SQL_10.contains("NOT p.proisagg"));
    }
}
