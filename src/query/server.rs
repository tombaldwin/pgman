//! Fixed reads and the few statements `pgman dump` / `restore` run
//! themselves: what server it is, who else is connected, and making or
//! dropping the database a restore goes into. pg_dump and pg_restore do
//! the rest (`crate::pgtool`).

/// What a dump or restore checks before starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub version_num: i64,
    pub version: String,
    pub in_recovery: bool,
    pub user: String,
}

pub const FACTS_SQL: &str = "SELECT pg_catalog.current_setting('server_version_num')::int8, \
     pg_catalog.current_setting('server_version'), pg_catalog.pg_is_in_recovery(), \
     current_user::text";

pub async fn facts(client: &tokio_postgres::Client) -> Result<Facts, String> {
    let row = client
        .query_one(FACTS_SQL, &[])
        .await
        .map_err(|e| format!("could not read the server's version: {}", message(&e)))?;
    Ok(Facts {
        version_num: row.get(0),
        version: row.get(1),
        in_recovery: row.get(2),
        user: row.get(3),
    })
}

/// A relation the connected user can't read, which pg_dump would stop at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub schema: String,
    pub name: String,
    /// `table`, `partition`, `sequence`, `foreign table`.
    pub kind: String,
    pub owner: String,
    /// `no SELECT`, `no USAGE on its schema`, `row-level security`.
    pub reason: String,
    /// For a sequence a table owns (`serial`, identity): that table,
    /// `schema.name`, and whether the user can read it. pg_dump reads an
    /// owned sequence with its table whatever `--exclude-table` says, so
    /// one whose table is dumped can't be left out on its own.
    pub owned_by: Option<(String, bool)>,
}

impl Unreadable {
    /// Whether leaving it out (`--exclude-table`) gets pg_dump past it:
    /// not for a sequence whose readable table is dumped.
    pub fn skippable(&self) -> bool {
        !matches!(self.owned_by, Some((_, true)))
    }
}

/// What pg_dump would need to read and the user can't, for a dump of the
/// whole database by a user that isn't a superuser: a table, partition or
/// sequence without SELECT, or in a schema without USAGE; a table under
/// row-level security the user doesn't bypass (pg_dump turns
/// `row_security` off, and the server refuses rather than filter); and,
/// with `$1` (`--include-foreign-data`), a foreign table. Not views or
/// materialized views: pg_dump reads their definitions from the catalog,
/// never their rows.
pub const UNREADABLE_SQL: &str = "WITH me AS (SELECT rolsuper, rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = current_user) \
     SELECT n.nspname::text, c.relname::text, \
       CASE c.relkind WHEN 'S' THEN 'sequence' WHEN 'f' THEN 'foreign table' \
         ELSE CASE WHEN c.relispartition THEN 'partition' ELSE 'table' END END, \
       pg_catalog.pg_get_userbyid(c.relowner)::text, \
       CASE WHEN NOT pg_catalog.has_schema_privilege(n.oid, 'USAGE') THEN 'no USAGE on its schema' \
            WHEN c.relkind = 'S' AND NOT pg_catalog.has_sequence_privilege(c.oid, 'SELECT') THEN 'no SELECT' \
            WHEN c.relkind <> 'S' AND NOT pg_catalog.has_table_privilege(c.oid, 'SELECT') THEN 'no SELECT' \
            ELSE 'row-level security' END, \
       CASE WHEN t.oid IS NOT NULL THEN tn.nspname::text || '.' || t.relname::text END, \
       CASE WHEN t.oid IS NOT NULL THEN pg_catalog.has_schema_privilege(tn.oid, 'USAGE') \
              AND pg_catalog.has_table_privilege(t.oid, 'SELECT') END \
     FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     LEFT JOIN pg_catalog.pg_depend d ON c.relkind = 'S' AND d.classid = 'pg_catalog.pg_class'::pg_catalog.regclass \
       AND d.objid = c.oid AND d.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass AND d.deptype IN ('a', 'i') \
     LEFT JOIN pg_catalog.pg_class t ON t.oid = d.refobjid \
     LEFT JOIN pg_catalog.pg_namespace tn ON tn.oid = t.relnamespace \
     WHERE NOT (SELECT rolsuper FROM me) \
       AND (c.relkind IN ('r', 'p', 'S') OR (c.relkind = 'f' AND $1)) \
       AND n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%' \
       AND (NOT pg_catalog.has_schema_privilege(n.oid, 'USAGE') \
         OR (c.relkind = 'S' AND NOT pg_catalog.has_sequence_privilege(c.oid, 'SELECT')) \
         OR (c.relkind <> 'S' AND NOT pg_catalog.has_table_privilege(c.oid, 'SELECT')) \
         OR (c.relkind IN ('r', 'p') AND c.relrowsecurity AND NOT pg_catalog.pg_has_role(c.relowner, 'USAGE') \
             AND NOT (SELECT rolbypassrls FROM me))) \
     ORDER BY 1, 2";

/// How many large objects the user can't read: pg_dump stops at the
/// first, and they can't be left out one by one (`--no-blobs` leaves out
/// all of them). None for a superuser, or with `lo_compat_privileges`.
pub const UNREADABLE_LARGE_OBJECTS_SQL: &str = "SELECT pg_catalog.count(*) FROM pg_catalog.pg_largeobject_metadata m \
     WHERE NOT (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname = current_user) \
       AND pg_catalog.current_setting('lo_compat_privileges') = 'off' \
       AND NOT pg_catalog.pg_has_role(m.lomowner, 'USAGE') \
       AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(m.lomacl) a \
         WHERE a.privilege_type = 'SELECT' AND (a.grantee = 0 OR pg_catalog.pg_has_role(a.grantee, 'USAGE')))";

/// [`UNREADABLE_SQL`]; `foreign`: the dump includes foreign tables' data.
pub async fn unreadable(
    client: &tokio_postgres::Client,
    foreign: bool,
) -> Result<Vec<Unreadable>, String> {
    client
        .query(UNREADABLE_SQL, &[&foreign])
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| Unreadable {
                    schema: r.get(0),
                    name: r.get(1),
                    kind: r.get(2),
                    owner: r.get(3),
                    reason: r.get(4),
                    owned_by: r
                        .get::<_, Option<String>>(5)
                        .map(|t| (t, r.get::<_, Option<bool>>(6).unwrap_or(false))),
                })
                .collect()
        })
        .map_err(|e| format!("could not check what the dump can read: {}", message(&e)))
}

/// [`UNREADABLE_LARGE_OBJECTS_SQL`].
pub async fn unreadable_large_objects(client: &tokio_postgres::Client) -> Result<i64, String> {
    client
        .query_one(UNREADABLE_LARGE_OBJECTS_SQL, &[])
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("could not check the large objects: {}", message(&e)))
}

/// Another session on a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub pid: i32,
    pub user: String,
    pub application: String,
}

/// Every client session on `db` but `client`'s own: not autovacuum or
/// another background worker, which come and go on their own (a freshly
/// restored database is where autovacuum runs) and which the user may not
/// be allowed to end.
pub async fn sessions_on(
    client: &tokio_postgres::Client,
    db: &str,
) -> Result<Vec<Session>, String> {
    client
        .query(
            "SELECT pid, coalesce(usename::text, ''), coalesce(application_name, '') \
             FROM pg_catalog.pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_catalog.pg_backend_pid() \
               AND backend_type = 'client backend' ORDER BY pid",
            &[&db],
        )
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| Session {
                    pid: r.get(0),
                    user: r.get(1),
                    application: r.get(2),
                })
                .collect()
        })
        .map_err(|e| format!("could not list the sessions on {db}: {}", message(&e)))
}

pub async fn database_exists(client: &tokio_postgres::Client, db: &str) -> Result<bool, String> {
    client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = $1)",
            &[&db],
        )
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("could not look for database {db}: {}", message(&e)))
}

/// `name` as a quoted identifier. Pure.
pub fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `CREATE DATABASE name` (owned by the connecting user), with no
/// statement timeout.
pub async fn create_database(client: &tokio_postgres::Client, name: &str) -> Result<(), String> {
    client
        .batch_execute("SET statement_timeout = 0")
        .await
        .map_err(|e| format!("could not lift the statement timeout: {}", message(&e)))?;
    client
        .batch_execute(&format!("CREATE DATABASE {}", quoted(name)))
        .await
        .map_err(|e| format!("could not create database {name}: {}", message(&e)))
}

/// `DROP DATABASE IF EXISTS name`: only `pgman restore --clean`, on a
/// target it has already checked is unprotected and has no other
/// sessions, after the operator confirmed replacing it.
pub async fn drop_database(client: &tokio_postgres::Client, name: &str) -> Result<(), String> {
    // A large database takes a while to drop; the restore's admin
    // session has a short statement timeout.
    client
        .batch_execute("SET statement_timeout = 0")
        .await
        .map_err(|e| format!("could not lift the statement timeout: {}", message(&e)))?;
    client
        .batch_execute(&format!("DROP DATABASE IF EXISTS {}", quoted(name)))
        .await
        .map_err(|e| format!("could not drop database {name}: {}", message(&e)))
}

fn message(e: &tokio_postgres::Error) -> String {
    e.as_db_error()
        .map(|d| d.message().to_string())
        .unwrap_or_else(|| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_name_doubles_its_quotes() {
        assert_eq!(quoted("ledger"), "\"ledger\"");
        assert_eq!(quoted("a\"; DROP"), "\"a\"\"; DROP\"");
    }
}
