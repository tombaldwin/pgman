//! Connection specs, DSN parsing, and the live `tokio-postgres` connection.
//!
//! `Dsn::parse` is pure and tested. `connect_and_bootstrap` / `run_query` do
//! the async I/O — kept thin. Plain TCP for now; TLS (RDS) is a follow-up.

use crate::grid::Grid;
use std::fmt;
use std::sync::Arc;

/// A parsed `postgres://` connection string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dsn {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    pub dbname: String,
    pub params: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DsnError {
    /// No `scheme://` prefix.
    MissingScheme,
    /// Scheme is something other than `postgres` / `postgresql`.
    BadScheme(String),
    /// Nothing after the scheme.
    MissingHost,
    /// The `:port` component didn't parse as a `u16`.
    BadPort(String),
}

impl fmt::Display for DsnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DsnError::MissingScheme => write!(f, "missing 'postgres://' scheme"),
            DsnError::BadScheme(s) => write!(f, "unsupported scheme {s:?} (expected postgres)"),
            DsnError::MissingHost => write!(f, "no host in connection string"),
            DsnError::BadPort(p) => write!(f, "invalid port {p:?}"),
        }
    }
}

impl std::error::Error for DsnError {}

impl Dsn {
    /// Parse a `postgres://user:pass@host:port/dbname?k=v` connection string.
    ///
    /// Defaults: port `5432`, host `localhost`, dbname `postgres`.
    ///
    /// Known limitation: bracketed IPv6 hosts (`[::1]:5432`) and percent-encoded
    /// userinfo are not yet handled — see BACKLOG.md M0.
    pub fn parse(dsn: &str) -> Result<Dsn, DsnError> {
        let dsn = dsn.trim();
        let (scheme, rest) = dsn.split_once("://").ok_or(DsnError::MissingScheme)?;
        match scheme.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" => {}
            other => return Err(DsnError::BadScheme(other.to_string())),
        }
        if rest.is_empty() {
            return Err(DsnError::MissingHost);
        }

        let (authority_path, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        let (authority, path) = match authority_path.split_once('/') {
            Some((a, p)) => (a, p),
            None => (authority_path, ""),
        };
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, authority),
        };
        let (user, password) = match userinfo {
            Some(ui) => match ui.split_once(':') {
                Some((u, p)) => (opt(u), opt(p)),
                None => (opt(ui), None),
            },
            None => (None, None),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => {
                let port = p.parse::<u16>().map_err(|_| DsnError::BadPort(p.to_string()))?;
                (h, port)
            }
            None => (hostport, 5432),
        };

        let host = if host.is_empty() {
            "localhost".to_string()
        } else {
            host.to_string()
        };
        let dbname = if path.is_empty() {
            "postgres".to_string()
        } else {
            path.to_string()
        };
        let params = match query {
            Some(q) => q
                .split('&')
                .filter(|s| !s.is_empty())
                .map(|kv| match kv.split_once('=') {
                    Some((k, v)) => (k.to_string(), v.to_string()),
                    None => (kv.to_string(), String::new()),
                })
                .collect(),
            None => Vec::new(),
        };

        Ok(Dsn {
            host,
            port,
            user,
            password,
            dbname,
            params,
        })
    }

    /// A human-readable form with the password masked — safe to log or show in
    /// the UI (see CLAUDE.md "never log credentials").
    pub fn redacted(&self) -> String {
        let userinfo = match (&self.user, &self.password) {
            (Some(u), Some(_)) => format!("{u}:***@"),
            (Some(u), None) => format!("{u}@"),
            (None, _) => String::new(),
        };
        format!(
            "postgres://{userinfo}{}:{}/{}",
            self.host, self.port, self.dbname
        )
    }
}

fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// A successful connection's bootstrap result.
pub struct Booted {
    pub server_version: String,
    pub grid: Grid,
    /// The live client — handed to `App` so subsequent queries run on the
    /// same session (and the read-only / statement-timeout settings stick).
    pub client: Arc<tokio_postgres::Client>,
    /// Snapshot of the database catalog used by Tab-completion. Empty
    /// when the catalog query failed (e.g. permissions) — that just
    /// disables completion, the connection itself is still usable.
    pub schema_cache: crate::query::schema::SchemaCache,
}

/// Connect to `dsn`, apply the safety session settings, then run `bootstrap_sql`
/// and return its result.
///
/// Plain TCP (`NoTls`) — RDS, which requires TLS, is a follow-up (BACKLOG.md).
pub async fn connect_and_bootstrap(
    dsn: Dsn,
    read_only: bool,
    statement_timeout_ms: u64,
    bootstrap_sql: String,
) -> Result<Booted, String> {
    let mut cfg = tokio_postgres::Config::new();
    cfg.host(&dsn.host).port(dsn.port).dbname(&dsn.dbname);
    if let Some(user) = &dsn.user {
        cfg.user(user);
    }
    if let Some(password) = &dsn.password {
        cfg.password(password);
    }

    let (client, connection) = cfg
        .connect(tokio_postgres::NoTls)
        .await
        .map_err(|e| chain_message(&e))?;
    // Drive the connection in the background; it ends when the last `Arc<Client>`
    // drops (which is when App finishes).
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::warn!("postgres connection closed: {e}");
        }
    });

    // Safety session settings — applied before any query runs.
    if read_only {
        client
            .batch_execute("SET default_transaction_read_only = on")
            .await
            .map_err(|e| chain_message(&e))?;
    }
    if statement_timeout_ms > 0 {
        client
            .batch_execute(&format!("SET statement_timeout = {statement_timeout_ms}"))
            .await
            .map_err(|e| chain_message(&e))?;
    }

    let server_version = client
        .query_one("SHOW server_version", &[])
        .await
        .ok()
        .and_then(|row| row.try_get::<usize, String>(0).ok())
        .unwrap_or_else(|| "unknown".to_string());

    let client = Arc::new(client);
    let grid = run_query(&client, &bootstrap_sql).await?;
    // Schema cache for Tab-completion. Best-effort — its own helper
    // swallows query errors and returns an empty cache, so a session
    // without `pg_catalog` SELECT (rare but possible on locked-down
    // managed instances) just disables completion.
    let schema_cache = crate::query::schema::fetch(&client).await;
    Ok(Booted {
        server_version,
        grid,
        client,
        schema_cache,
    })
}

/// Run `sql` and collect the result into a `Grid`, handling both row-returning
/// statements (`SELECT`, `EXPLAIN`, `SHOW`, …) and non-row-returning ones
/// (`UPDATE`, `DELETE`, DDL). Non-row statements yield a single-cell grid with
/// the affected-row count.
pub async fn run_statement(client: &tokio_postgres::Client, sql: &str) -> Result<Grid, String> {
    let stmt = client.prepare(sql).await.map_err(|e| e.to_string())?;
    let columns = stmt.columns();
    if columns.is_empty() {
        let affected = client
            .execute(&stmt, &[])
            .await
            .map_err(|e| e.to_string())?;
        Ok(Grid {
            columns: vec!["status".to_string()],
            rows: vec![vec![format!("{affected} row(s) affected")]],
        })
    } else {
        let column_names: Vec<String> = columns.iter().map(|c| c.name().to_string()).collect();
        let rows = client.query(&stmt, &[]).await.map_err(|e| e.to_string())?;
        let out_rows: Vec<Vec<String>> = rows
            .iter()
            .take(crate::grid::MAX_ROWS)
            .map(|row| (0..row.len()).map(|i| cell_to_string(row, i)).collect())
            .collect();
        Ok(Grid {
            columns: column_names,
            rows: out_rows,
        })
    }
}

/// Open a transaction and run `sql`. On success the transaction is **left
/// open** — the caller (usually the App's commit/rollback prompt) decides
/// `COMMIT` or `ROLLBACK`. On error the transaction is rolled back immediately
/// so the session doesn't sit aborted.
pub async fn run_in_tx_open(client: &tokio_postgres::Client, sql: &str) -> Result<Grid, String> {
    client
        .batch_execute("BEGIN")
        .await
        .map_err(|e| e.to_string())?;
    match run_statement(client, sql).await {
        Ok(grid) => Ok(grid),
        Err(e) => {
            let _ = client.batch_execute("ROLLBACK").await;
            Err(e)
        }
    }
}

/// Run a multi-statement script (`;`-separated) via the simple query
/// protocol. Returns a single-row "status" grid since `batch_execute` does
/// not yield row sets.
pub async fn run_batch(client: &tokio_postgres::Client, sql: &str) -> Result<Grid, String> {
    client
        .batch_execute(sql)
        .await
        .map_err(|e| e.to_string())?;
    Ok(status_grid("batch executed"))
}

/// Run a multi-statement script inside an explicit transaction that is
/// **left open** on success (caller commits or rolls back). On error in the
/// batch, rolls back immediately.
pub async fn run_batch_in_tx_open(
    client: &tokio_postgres::Client,
    sql: &str,
) -> Result<Grid, String> {
    client
        .batch_execute("BEGIN")
        .await
        .map_err(|e| e.to_string())?;
    match client.batch_execute(sql).await {
        Ok(()) => Ok(status_grid("batch ran — awaiting commit/rollback")),
        Err(e) => {
            let _ = client.batch_execute("ROLLBACK").await;
            Err(e.to_string())
        }
    }
}

fn status_grid(msg: &str) -> Grid {
    Grid {
        columns: vec!["status".to_string()],
        rows: vec![vec![msg.to_string()]],
    }
}

/// Commit an open transaction.
pub async fn tx_commit(client: &tokio_postgres::Client) -> Result<(), String> {
    client
        .batch_execute("COMMIT")
        .await
        .map_err(|e| e.to_string())
}

/// Roll back an open transaction.
pub async fn tx_rollback(client: &tokio_postgres::Client) -> Result<(), String> {
    client
        .batch_execute("ROLLBACK")
        .await
        .map_err(|e| e.to_string())
}

/// Run `sql` inside an explicit transaction that is *always* rolled back —
/// used for `EXPLAIN ANALYZE` on DML so the mutation never lands.
pub async fn run_in_tx_rollback(
    client: &tokio_postgres::Client,
    sql: &str,
) -> Result<Grid, String> {
    client
        .batch_execute("BEGIN")
        .await
        .map_err(|e| e.to_string())?;
    let result = run_statement(client, sql).await;
    let _ = client.batch_execute("ROLLBACK").await;
    result
}

/// Run `sql` and collect the result into a `Grid` (capped at `grid::MAX_ROWS`).
pub async fn run_query(client: &tokio_postgres::Client, sql: &str) -> Result<Grid, String> {
    let stmt = client.prepare(sql).await.map_err(|e| e.to_string())?;
    let columns: Vec<String> = stmt
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    let rows = client.query(&stmt, &[]).await.map_err(|e| e.to_string())?;
    let out_rows: Vec<Vec<String>> = rows
        .iter()
        .take(crate::grid::MAX_ROWS)
        .map(|row| (0..row.len()).map(|i| cell_to_string(row, i)).collect())
        .collect();
    Ok(Grid {
        columns,
        rows: out_rows,
    })
}

/// Render one cell as a display string. Handles common scalar types; SQL NULL
/// renders empty, and a type we can't decode renders `?`.
fn cell_to_string(row: &tokio_postgres::Row, i: usize) -> String {
    use tokio_postgres::types::{FromSql, Type};

    fn show<T>(row: &tokio_postgres::Row, i: usize) -> Option<String>
    where
        T: std::fmt::Display + for<'a> FromSql<'a>,
    {
        match row.try_get::<usize, Option<T>>(i) {
            Ok(Some(v)) => Some(v.to_string()),
            Ok(None) => Some(String::new()), // SQL NULL
            Err(_) => None,                  // unsupported type / decode error
        }
    }

    let ty = row.columns()[i].type_().clone();
    let rendered = if ty == Type::BOOL {
        show::<bool>(row, i)
    } else if ty == Type::INT2 {
        show::<i16>(row, i)
    } else if ty == Type::INT4 {
        show::<i32>(row, i)
    } else if ty == Type::INT8 {
        show::<i64>(row, i)
    } else if ty == Type::OID {
        show::<u32>(row, i)
    } else if ty == Type::FLOAT4 {
        show::<f32>(row, i)
    } else if ty == Type::FLOAT8 {
        show::<f64>(row, i)
    } else {
        // TEXT / VARCHAR / NAME / etc. decode as String; anything else errors.
        show::<String>(row, i)
    };
    rendered.unwrap_or_else(|| "?".to_string())
}

/// Flatten an error and its `source()` chain into a single string —
/// `"top: cause1: cause2"`. tokio-postgres' Display only emits the top
/// level ("error connecting to server"); the actually-useful cause
/// ("Connection refused", "no such host", "password authentication failed")
/// is on the source chain.
pub fn chain_message(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut src = err.source();
    while let Some(e) = src {
        let next = e.to_string();
        // Some libraries already include the cause in their own Display.
        // Avoid the visual stutter ("X: X: …") by skipping a duplicate tail.
        if !out.ends_with(&next) {
            out.push_str(": ");
            out.push_str(&next);
        }
        src = e.source();
    }
    out
}

/// Best-effort actionable hint for a connection-error string. Returns a
/// short imperative sentence the operator can read and act on, or `None`
/// when nothing in the message looks familiar. Pure — pattern-match only.
pub fn connect_hint(err: &str, dsn: &Dsn) -> Option<String> {
    let lower = err.to_ascii_lowercase();
    if lower.contains("connection refused") {
        return Some(format!(
            "nothing is listening at {}:{}. is the server running, and is the port right?",
            dsn.host, dsn.port
        ));
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return Some(format!(
            "{}:{} didn't answer in time. VPN / firewall / security group blocking?",
            dsn.host, dsn.port
        ));
    }
    if lower.contains("no such host")
        || lower.contains("failed to lookup")
        || lower.contains("name or service not known")
        || lower.contains("nodename nor servname")
    {
        return Some(format!(
            "host {:?} doesn't resolve. typo, or missing /etc/hosts / DNS entry?",
            dsn.host
        ));
    }
    if lower.contains("password authentication failed") {
        return Some(
            "wrong password. set PGPASSWORD before launching pgman, or pass user:pass in --dsn"
                .to_string(),
        );
    }
    if lower.contains("no password was provided") || lower.contains("requires password") {
        return Some(
            "server demands a password. set PGPASSWORD before launching pgman, or pass it in --dsn"
                .to_string(),
        );
    }
    if lower.contains("role") && lower.contains("does not exist") {
        return Some(format!(
            "user {:?} doesn't exist on the server. check spelling / `\\du`",
            dsn.user.as_deref().unwrap_or("")
        ));
    }
    if lower.contains("database") && lower.contains("does not exist") {
        return Some(format!(
            "database {:?} doesn't exist on the server. check spelling / `\\l`",
            dsn.dbname
        ));
    }
    if lower.contains("ssl") || lower.contains("tls") {
        return Some(
            "server requires TLS but pgman currently connects with NoTls (BACKLOG: TLS support)"
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_dsn() {
        let d = Dsn::parse("postgres://alice:s3cret@db.example.com:6543/orders").unwrap();
        assert_eq!(d.host, "db.example.com");
        assert_eq!(d.port, 6543);
        assert_eq!(d.user.as_deref(), Some("alice"));
        assert_eq!(d.password.as_deref(), Some("s3cret"));
        assert_eq!(d.dbname, "orders");
    }

    #[test]
    fn applies_defaults() {
        let d = Dsn::parse("postgresql://localhost/").unwrap();
        assert_eq!(d.host, "localhost");
        assert_eq!(d.port, 5432);
        assert_eq!(d.dbname, "postgres");
        assert!(d.user.is_none());
        assert!(d.password.is_none());
    }

    #[test]
    fn parses_user_without_password() {
        let d = Dsn::parse("postgres://reader@host:5432/app").unwrap();
        assert_eq!(d.user.as_deref(), Some("reader"));
        assert!(d.password.is_none());
    }

    #[test]
    fn parses_query_params() {
        let d = Dsn::parse("postgres://h/db?sslmode=require&application_name=pgman").unwrap();
        assert_eq!(
            d.params,
            vec![
                ("sslmode".to_string(), "require".to_string()),
                ("application_name".to_string(), "pgman".to_string()),
            ]
        );
    }

    #[test]
    fn rejects_missing_and_bad_scheme() {
        assert_eq!(Dsn::parse("db.example.com/orders"), Err(DsnError::MissingScheme));
        assert_eq!(
            Dsn::parse("mysql://h/db"),
            Err(DsnError::BadScheme("mysql".to_string()))
        );
    }

    #[test]
    fn rejects_bad_port() {
        assert_eq!(
            Dsn::parse("postgres://h:not-a-port/db"),
            Err(DsnError::BadPort("not-a-port".to_string()))
        );
    }

    #[test]
    fn rejects_empty_authority() {
        assert_eq!(Dsn::parse("postgres://"), Err(DsnError::MissingHost));
    }

    #[test]
    fn redacted_masks_password() {
        let d = Dsn::parse("postgres://alice:s3cret@h:5432/orders").unwrap();
        let r = d.redacted();
        assert!(r.contains("alice"), "user shown: {r}");
        assert!(!r.contains("s3cret"), "password leaked: {r}");
        assert!(r.contains("***"));
    }

    // A tiny test-only error type with a source() — enough to exercise the
    // chain walker without depending on tokio-postgres being live.
    #[derive(Debug)]
    struct StubErr {
        msg: &'static str,
        src: Option<Box<dyn std::error::Error + 'static>>,
    }
    impl std::fmt::Display for StubErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.msg)
        }
    }
    impl std::error::Error for StubErr {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.src.as_deref()
        }
    }

    #[test]
    fn chain_message_concatenates_top_and_cause() {
        let inner = StubErr {
            msg: "Connection refused (os error 61)",
            src: None,
        };
        let outer = StubErr {
            msg: "error connecting to server",
            src: Some(Box::new(inner)),
        };
        assert_eq!(
            chain_message(&outer),
            "error connecting to server: Connection refused (os error 61)"
        );
    }

    #[test]
    fn chain_message_handles_no_source() {
        let only = StubErr { msg: "boom", src: None };
        assert_eq!(chain_message(&only), "boom");
    }

    #[test]
    fn chain_message_avoids_duplicate_tail() {
        // Some libraries bake the cause into Display already; don't end up
        // with "io error: ECONNREFUSED: ECONNREFUSED".
        let inner = StubErr { msg: "ECONNREFUSED", src: None };
        let outer = StubErr {
            msg: "io error: ECONNREFUSED",
            src: Some(Box::new(inner)),
        };
        assert_eq!(chain_message(&outer), "io error: ECONNREFUSED");
    }

    fn dsn_for(host: &str, db: &str, user: Option<&str>) -> Dsn {
        Dsn {
            host: host.to_string(),
            port: 5432,
            user: user.map(|s| s.to_string()),
            password: None,
            dbname: db.to_string(),
            params: Vec::new(),
        }
    }

    #[test]
    fn connect_hint_recognises_refused() {
        let d = dsn_for("db.local", "x", None);
        let h = connect_hint("Connection refused (os error 61)", &d).unwrap();
        assert!(h.contains("db.local"), "got: {h}");
        assert!(h.contains("5432"), "got: {h}");
    }

    #[test]
    fn connect_hint_recognises_dns_failure() {
        let d = dsn_for("missing-host", "x", None);
        let h = connect_hint("failed to lookup address info", &d).unwrap();
        assert!(h.contains("missing-host"), "got: {h}");
    }

    #[test]
    fn connect_hint_recognises_auth_failure() {
        let d = dsn_for("h", "x", Some("alice"));
        let h = connect_hint("password authentication failed for user \"alice\"", &d).unwrap();
        assert!(h.to_lowercase().contains("password"), "got: {h}");
    }

    #[test]
    fn connect_hint_recognises_missing_database() {
        let d = dsn_for("h", "ghost", None);
        let h = connect_hint("database \"ghost\" does not exist", &d).unwrap();
        assert!(h.contains("ghost"), "got: {h}");
    }

    #[test]
    fn connect_hint_returns_none_for_unknown_errors() {
        let d = dsn_for("h", "x", None);
        assert!(connect_hint("something weird happened", &d).is_none());
    }
}
