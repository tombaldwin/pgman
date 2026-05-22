//! Connection specs, DSN parsing, and the live `tokio-postgres` connection.
//!
//! `Dsn::parse` is pure and tested. `connect_and_bootstrap` / `run_query` do
//! the async I/O — kept thin. Plain TCP for now; TLS (RDS) is a follow-up.

use crate::grid::Grid;
use std::fmt;

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

/// A successful connection's bootstrap result: server version + first query.
pub struct Booted {
    pub server_version: String,
    pub grid: Grid,
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
        .map_err(|e| e.to_string())?;
    // Drive the connection in the background; it ends when `client` drops.
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
            .map_err(|e| e.to_string())?;
    }
    if statement_timeout_ms > 0 {
        client
            .batch_execute(&format!("SET statement_timeout = {statement_timeout_ms}"))
            .await
            .map_err(|e| e.to_string())?;
    }

    let server_version = client
        .query_one("SHOW server_version", &[])
        .await
        .ok()
        .and_then(|row| row.try_get::<usize, String>(0).ok())
        .unwrap_or_else(|| "unknown".to_string());

    let grid = run_query(&client, &bootstrap_sql).await?;
    Ok(Booted {
        server_version,
        grid,
    })
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
}
