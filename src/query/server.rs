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

/// Another session on a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub pid: i32,
    pub user: String,
    pub application: String,
}

/// Every session on `db` but `client`'s own.
pub async fn sessions_on(
    client: &tokio_postgres::Client,
    db: &str,
) -> Result<Vec<Session>, String> {
    client
        .query(
            "SELECT pid, coalesce(usename::text, ''), coalesce(application_name, '') \
             FROM pg_catalog.pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_catalog.pg_backend_pid() ORDER BY pid",
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
