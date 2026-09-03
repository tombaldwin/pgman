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
    /// Optional SSH-tunnel target. Set from the URL param
    /// `ssh_tunnel=[user@]host[:port]` or from a project-config
    /// `Connection.ssh_tunnel`. When present, `connect_and_bootstrap`
    /// opens the tunnel first and rewrites the wire connect to
    /// `127.0.0.1:<local-port>`.
    pub ssh_tunnel: Option<crate::tunnel::SshTunnelSpec>,
}

/// A server-side notice (the unified shape we surface for any
/// `RAISE NOTICE` / `RAISE WARNING` / `RAISE INFO` from a function,
/// trigger, or DO block). Plain data — App stores recent ones and
/// renders them in the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeMsg {
    pub severity: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
}

/// One LISTEN/NOTIFY arrival: the channel the operator
/// subscribed to with `LISTEN <chan>`, the publisher's backend
/// pid, and the payload (often empty). Surfaced in
/// `Mode::Notifications` (the `N` panel) — App stores a ring
/// buffer of recent ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationMsg {
    pub channel: String,
    pub pid: i32,
    pub payload: String,
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
    /// `sslmode=<value>` (trimmed, lowercased) wasn't one of the
    /// recognised modes. A typo or wrong separator here must never
    /// fall through to a weaker mode — see `apply_ssl_mode`.
    UnknownSslMode(String),
}

impl fmt::Display for DsnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DsnError::MissingScheme => write!(f, "missing 'postgres://' scheme"),
            DsnError::BadScheme(s) => write!(f, "unsupported scheme {s:?} (expected postgres)"),
            DsnError::MissingHost => write!(f, "no host in connection string"),
            DsnError::BadPort(p) => write!(f, "invalid port {p:?}"),
            DsnError::UnknownSslMode(v) => write!(
                f,
                "unknown sslmode {v:?} — use one of disable, allow, prefer, require, verify-ca, verify-full"
            ),
        }
    }
}

impl std::error::Error for DsnError {}

/// A failed query — the human-readable message plus, when Postgres
/// sent one, the 1-indexed character position of the syntax error
/// within the submitted SQL. The position lets the editor jump the
/// cursor to the offending token.
///
/// Display elides the position so existing call sites that just
/// `.to_string()` keep working unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryErr {
    pub msg: String,
    pub position: Option<u32>,
    /// Rich fields extracted from the server-side `DbError` when
    /// the error came from Postgres. All `None` for non-Postgres
    /// failures (TLS, IO, our own validation) or when the server
    /// didn't populate the field. Surfaced by the "rich error
    /// overlay" key (`Ctrl-E` after a failure).
    pub detail: Option<QueryErrDetail>,
}

/// Postgres `DbError` fields worth showing in a rich overlay.
/// Mirrors the libpq error message anatomy: a one-line summary
/// (in `msg`) plus optional `detail` / `hint` / `where` /
/// affected-object identifiers (`schema`, `table`, `column`,
/// `constraint`, `data_type`) for FK / constraint / type errors.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryErrDetail {
    /// Five-letter SQLSTATE code (`23505` = unique_violation, …).
    pub code: Option<String>,
    /// `ERROR` / `FATAL` / `PANIC` / etc.
    pub severity: Option<String>,
    /// Secondary message providing more context.
    pub detail: Option<String>,
    /// Operator-actionable hint.
    pub hint: Option<String>,
    /// `where:` context — the call site that raised it.
    pub r#where: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
    pub column: Option<String>,
    pub data_type: Option<String>,
    pub constraint: Option<String>,
}

impl QueryErr {
    /// Build a position-less error — for non-Postgres failures (TLS,
    /// IO, our own validation).
    pub fn msg(msg: impl Into<String>) -> Self {
        Self {
            msg: msg.into(),
            position: None,
            detail: None,
        }
    }
}

impl From<tokio_postgres::Error> for QueryErr {
    fn from(e: tokio_postgres::Error) -> Self {
        let db = e.as_db_error();
        let position = db.and_then(|d| d.position()).map(|p| match p {
            tokio_postgres::error::ErrorPosition::Original(n)
            | tokio_postgres::error::ErrorPosition::Internal { position: n, .. } => *n,
        });
        let detail = db.map(|d| QueryErrDetail {
            code: Some(d.code().code().to_string()),
            severity: Some(d.severity().to_string()),
            detail: d.detail().map(str::to_string),
            hint: d.hint().map(str::to_string),
            r#where: d.where_().map(str::to_string),
            schema: d.schema().map(str::to_string),
            table: d.table().map(str::to_string),
            column: d.column().map(str::to_string),
            data_type: d.datatype().map(str::to_string),
            constraint: d.constraint().map(str::to_string),
        });
        // When DbError carries a message itself, prefer it (no `db
        // error: ERROR:` wrapping) so the message line is clean.
        // Fall through to the default Display for non-server errors.
        let mut msg = db
            .map(|d| d.message().to_string())
            .unwrap_or_else(|| e.to_string());
        // A write bounced off `default_transaction_read_only = on`
        // (SQLSTATE 25006) reads as a bare server refusal with no clue
        // *why* the session is read-only — append the same hint both
        // the TUI (`last_error` is this `.msg`) and `--batch` (which
        // prints it to stderr) end up showing, so neither path has to
        // ask separately.
        let safety_toml_exists = crate::util::config_file("safety.toml").exists();
        if let Some(hint) = read_only_refusal_hint(detail.as_ref(), &msg, safety_toml_exists) {
            msg.push('\n');
            msg.push_str("hint: ");
            msg.push_str(&hint);
        }
        Self {
            msg,
            position,
            detail,
        }
    }
}

/// The read-only explanation when there is no `safety.toml` to point at:
/// read-only is the built-in default, and the fix is to write the file.
/// Shared by the server-side refusal hint (SQLSTATE `25006`) and the
/// client-side escape refusal (`:readonly off`,
/// `app::read_only_escape_refusal`), so the two paths cannot drift.
pub const READ_ONLY_DEFAULT_HINT: &str = "read-only by default · pgman --init-config writes safety.toml; set read_only = false for this database";

/// Hint for a read-only-transaction refusal (SQLSTATE `25006`):
/// `read_only = true` set `default_transaction_read_only = on` at
/// connect, and Postgres itself — not a client-side guard — rejected the
/// write. Says where the setting came from rather than leaving the
/// operator to guess: the file's path and key when a `safety.toml`
/// exists, and [`READ_ONLY_DEFAULT_HINT`] when the profile is the
/// built-in default — naming a file that is not there sent the operator
/// looking for it. Pure — pattern-matched on the SQLSTATE code, falling
/// back to the message text when the server didn't populate `detail`
/// (e.g. a `simple_query` error path that skips it); `safety_toml_exists`
/// is checked by the caller at message time because the profile does not
/// record its origin.
fn read_only_refusal_hint(
    detail: Option<&QueryErrDetail>,
    msg: &str,
    safety_toml_exists: bool,
) -> Option<String> {
    let is_read_only_refusal = detail.and_then(|d| d.code.as_deref()) == Some("25006")
        || msg.to_ascii_lowercase().contains("read-only transaction");
    if !is_read_only_refusal {
        return None;
    }
    if !safety_toml_exists {
        return Some(READ_ONLY_DEFAULT_HINT.to_string());
    }
    Some(format!(
        "this connection is read-only by safety.toml ({}, read_only) — see docs/configuration.md",
        crate::util::config_file("safety.toml").display()
    ))
}

impl std::fmt::Display for QueryErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for QueryErr {}

impl Dsn {
    /// Parse a `postgres://user:pass@host:port/dbname?k=v` connection string.
    ///
    /// Defaults: port `5432`, host `localhost`, dbname `postgres`.
    ///
    /// Userinfo (user/password) is split from the authority with
    /// [`split_authority`] — see that function for the exact rule that
    /// lets a password contain `/` or `@` unescaped. `user`/`password`
    /// are then percent-decoded leniently (a malformed `%XX` escape is
    /// passed through literally rather than erroring) — this matches
    /// libpq's URI-connection-string behaviour, and is the only way a
    /// password can contain a literal `?` or `#` (those can't appear
    /// raw in the authority; they start the query/fragment). `host`
    /// and `dbname` are **not** percent-decoded.
    ///
    /// Every query-param value is trimmed of surrounding whitespace.
    /// `sslmode=` is additionally validated against
    /// [`KNOWN_SSLMODES`] (case-insensitively) and normalised to its
    /// canonical lowercase form — an unrecognised value is a hard
    /// `Err(DsnError::UnknownSslMode)`, never a value that quietly
    /// falls through to a weaker mode at connect time.
    ///
    /// Known limitation: bracketed IPv6 hosts (`[::1]:5432`) are not
    /// yet handled — see BACKLOG.md M0.
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

        let (userinfo, hostport, path_and_query) = split_authority(rest);
        // A `#fragment` isn't meaningful for a postgres DSN — drop it
        // (and anything after it) before splitting path from query.
        let path_and_query = path_and_query.split('#').next().unwrap_or("");
        let (path, query) = match path_and_query.strip_prefix('/') {
            Some(after_slash) => match after_slash.split_once('?') {
                Some((p, q)) => (p, Some(q)),
                None => (after_slash, None),
            },
            None => match path_and_query.strip_prefix('?') {
                Some(q) => ("", Some(q)),
                None => ("", None),
            },
        };
        let (user, password) = match userinfo {
            Some(ui) => match ui.split_once(':') {
                Some((u, p)) => (opt(percent_decode(u)), opt(percent_decode(p))),
                None => (opt(percent_decode(ui)), None),
            },
            None => (None, None),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => {
                let port = p
                    .parse::<u16>()
                    .map_err(|_| DsnError::BadPort(p.to_string()))?;
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
        // Trim whitespace off both key and value — a `?sslmode=require `
        // (trailing space) or `sslmode=require\r` (stray CR from a
        // Windows-authored `.env`/config file) is a real value some
        // sources hand us, and it shouldn't be treated as an unrecognised
        // one just because of incidental whitespace.
        let raw_params: Vec<(String, String)> = match query {
            Some(q) => q
                .split('&')
                .filter(|s| !s.is_empty())
                .map(|kv| match kv.split_once('=') {
                    Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
                    None => (kv.trim().to_string(), String::new()),
                })
                .collect(),
            None => Vec::new(),
        };

        // Split out `ssh_tunnel=` if present so the rest of the params
        // list stays Postgres-only. Parse failures fall back to "no
        // tunnel" with a tracing warning rather than failing the DSN —
        // a typo in the tunnel spec shouldn't lock the operator out of
        // a connection that might be reachable directly.
        //
        // Duplicate `ssh_tunnel=` keys: first-set-wins (whether valid
        // or not) so the resolution doesn't depend on later occurrences
        // — typical URL-param convention.
        //
        // `sslmode=` is different: unlike a stray/duplicate tunnel
        // param, a bad `sslmode` is a *security* setting, and the
        // whole point of this validation is that it must never be
        // silently swallowed — so every occurrence is validated
        // strictly and normalised to its canonical lowercase form
        // (`apply_ssl_mode` then matches on that verbatim, with no
        // further case/whitespace handling of its own).
        let mut params: Vec<(String, String)> = Vec::with_capacity(raw_params.len());
        let mut ssh_tunnel: Option<crate::tunnel::SshTunnelSpec> = None;
        let mut saw_tunnel_key = false;
        for (k, v) in raw_params {
            if k.eq_ignore_ascii_case("ssh_tunnel") {
                if saw_tunnel_key {
                    // Don't log the raw value — it's the one connection-string
                    // param that would otherwise escape redaction. The spec is
                    // host/user only (no password by design), but keeping it out
                    // of the log preserves the uniform "never log connection
                    // string contents" discipline.
                    tracing::warn!("ignoring duplicate ssh_tunnel param; first occurrence wins");
                    continue;
                }
                saw_tunnel_key = true;
                match crate::tunnel::SshTunnelSpec::parse(&v) {
                    Ok(spec) => ssh_tunnel = Some(spec),
                    Err(e) => {
                        tracing::warn!("ignoring malformed ssh_tunnel param: {e}");
                    }
                }
            } else if k.eq_ignore_ascii_case("sslmode") {
                let lower = v.to_ascii_lowercase();
                if !KNOWN_SSLMODES.contains(&lower.as_str()) {
                    return Err(DsnError::UnknownSslMode(v));
                }
                params.push((k, lower));
            } else {
                params.push((k, v));
            }
        }

        Ok(Dsn {
            host,
            port,
            user,
            password,
            dbname,
            params,
            ssh_tunnel,
        })
    }

    /// A human-readable form with the password masked — safe to log or show in
    /// the UI (see CLAUDE.md "never log credentials"). Appends the SSH
    /// tunnel target when one is configured so provenance lines surface
    /// the path the operator is actually taking (no creds, just the
    /// bastion host).
    pub fn redacted(&self) -> String {
        let userinfo = match (&self.user, &self.password) {
            (Some(u), Some(_)) => format!("{u}:***@"),
            (Some(u), None) => format!("{u}@"),
            (None, _) => String::new(),
        };
        let tunnel = match &self.ssh_tunnel {
            Some(s) => format!(" via ssh://{}", s.to_display()),
            None => String::new(),
        };
        format!(
            "postgres://{userinfo}{}:{}/{}{tunnel}",
            self.host, self.port, self.dbname
        )
    }
}

fn opt(s: impl Into<String>) -> Option<String> {
    let s = s.into();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Split the portion of a connection URL after `scheme://` into
/// `(userinfo, authority, path_and_query)`.
///
/// The naive "authority ends at the first `/`" rule (what both
/// `Dsn::parse` and `redact_url` used to do) breaks the moment a
/// password contains `/` or `@` — both are valid, unescaped, in a
/// postgres/JDBC password. So is `?`, and so is `#`.
///
/// The rule: the *last* `@` is the userinfo/host boundary, so an
/// unescaped `@` inside the password is absorbed into userinfo rather
/// than mistaken for the boundary; the authority then ends at the
/// first `/`, `?`, or `#` at-or-after that boundary.
///
/// What bounds the search for that `@` is the delicate part. Cutting
/// at the first `?` or `#` anywhere in the string — the previous rule
/// — mis-parses `postgres://u:p?ss@h/d`: the cut lands *before* the
/// real `@`, so the userinfo is missed entirely and `u:p` is read as
/// `host:port`. A `?` is only a query separator once the path has
/// begun, so the search runs to the first `?`/`#` that follows the
/// first `/`, and to the end of the string when there is no `/` at
/// all.
///
/// That last clause is deliberately the *unsafe-to-parse, safe-to-
/// redact* choice. In a path-less URL a `?` could equally start the
/// query (`…@host?sslmode=disable`, common) or sit inside the password
/// (`u:p?ss@host`, rare), and nothing in the string distinguishes
/// them. Scanning the whole string resolves the tie towards "it was
/// the password", because the cost of guessing wrong that way is a
/// wrongly-masked host in a log line, while guessing the other way
/// prints the password. `redact_url` closes the remaining gap outright.
///
/// Returns `authority` as `host[:port]` (userinfo already stripped)
/// and `path_and_query` as everything from the authority's end to the
/// end of the string (starting with `/`, `?`, `#`, or empty).
fn split_authority(rest: &str) -> (Option<&str>, &str, &str) {
    let at_search_end = match rest.find('/') {
        // Past the path's first `/`, a `?`/`#` really does open the
        // query — an `@` beyond it belongs to a parameter value.
        Some(p) => rest[p..]
            .find(['?', '#'])
            .map(|i| p + i)
            .unwrap_or(rest.len()),
        None => rest.len(),
    };
    let last_at = rest[..at_search_end].rfind('@');
    let search_start = last_at.map(|p| p + 1).unwrap_or(0);
    let authority_end = rest[search_start..]
        .find(['/', '?', '#'])
        .map(|i| search_start + i)
        .unwrap_or(rest.len());
    let userinfo = last_at.map(|p| &rest[..p]);
    let authority = &rest[search_start..authority_end];
    let path_and_query = &rest[authority_end..];
    (userinfo, authority, path_and_query)
}

/// Percent-decode a URI component leniently: a malformed or truncated
/// `%XX` escape (bad hex digits, or `%` with fewer than two
/// characters after it) is copied through literally rather than
/// erroring. This is credential material from a connection string,
/// not a strict URI — refusing to connect over one stray `%` would be
/// worse than leaving it un-decoded. Non-UTF8 byte sequences produced
/// by decoding (e.g. a raw `%FF`) are replaced per
/// `String::from_utf8_lossy` rather than panicking.
fn percent_decode(s: &str) -> String {
    // Byte-level throughout (no `&s[..]` string slicing) — the input
    // may be arbitrary bytes reinterpreted as UTF-8 (fuzzed / hostile
    // input), and a `%` can legitimately sit right before a multibyte
    // character. Slicing on a computed offset there would land
    // mid-codepoint and panic; comparing raw bytes never does.
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Mask inline credentials in a connection-URL-shaped string so it is
/// safe to log when it can't be parsed into a [`Dsn`] (where
/// [`Dsn::redacted`] would otherwise be used). Scrubs both forms a
/// postgres/JDBC URL can carry a secret in:
///   - userinfo: `scheme://user:pass@host` → `scheme://***@host`
///   - query param: `?password=secret` / `&pwd=…` → `password=***`
///
/// Best-effort and conservative. Used only on the logging/error path for
/// strings that failed to parse; never reconstruct a real DSN from it.
pub fn redact_url(url: &str) -> String {
    // 1. Userinfo between "://" and the authority.
    //
    // `split_authority` is the parser's rule, and it is right whenever
    // the DSN can be parsed at all — but this function's whole job is
    // the strings that could NOT be parsed, so it cannot stop there. A
    // password mixing `@`, `/` and `?` defeats any split (`u:@/?@h/d`
    // — which `@` is the delimiter?), and `split_authority` picking
    // the earlier one would leave the rest of the password in the log.
    //
    // So redaction cuts at the *last* `@` that could be a userinfo
    // boundary, falling back to the parser's answer when none does.
    // The test for "could be": a userinfo `@` is followed by the host
    // and therefore by the path's `/`, while an `@` in a query
    // parameter value sits past the path and has no `/` after it. With
    // no `/` anywhere there is no path, so the `@` cannot be inside a
    // query that follows one.
    //
    // Ties go to masking. Being wrong costs a host name in one log
    // line; being wrong the other way prints a password.
    let mut out = String::with_capacity(url.len());
    let tail = if let Some(scheme_end) = url.find("://") {
        let after = scheme_end + 3;
        out.push_str(&url[..after]);
        let rest = &url[after..];
        let (userinfo, authority, path_and_query) = split_authority(rest);
        let cut = rest
            .rfind('@')
            .filter(|at| rest[at + 1..].contains('/') || !rest[..*at].contains('/'))
            .or_else(|| userinfo.map(|u| u.len()));
        match cut {
            Some(at) => {
                out.push_str("***@");
                &rest[at + 1..]
            }
            None => {
                out.push_str(authority);
                path_and_query
            }
        }
    } else {
        url
    };
    out.push_str(tail);
    // 2. password-bearing query params.
    mask_password_params(out)
}

/// Replace the value of any `password=` / `pwd=` / `passwd=` query
/// parameter (case-insensitive key) with `***`. Indices are taken from
/// an ASCII-lowercased copy, whose byte length matches the original, so
/// they map back exactly.
fn mask_password_params(s: String) -> String {
    let lower = s.to_ascii_lowercase();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for key in ["password=", "pwd=", "passwd="] {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(key) {
            let val_start = from + rel + key.len();
            let val_end = s[val_start..]
                .find('&')
                .map(|i| val_start + i)
                .unwrap_or(s.len());
            ranges.push((val_start, val_end));
            from = val_end;
        }
    }
    if ranges.is_empty() {
        return s;
    }
    ranges.sort_unstable();
    let mut out = String::with_capacity(s.len());
    let mut cursor = 0;
    for (vs, ve) in ranges {
        if vs < cursor {
            continue; // nested/overlapping match already covered
        }
        out.push_str(&s[cursor..vs]);
        out.push_str("***");
        cursor = ve;
    }
    out.push_str(&s[cursor..]);
    out
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
    /// SSH tunnel keeping the connection alive when `dsn.ssh_tunnel`
    /// was set. App must hold this for as long as `client` lives —
    /// dropping the tunnel SIGTERMs the ssh subprocess, which closes
    /// the local forward and (typically) terminates the postgres
    /// connection. `None` when the connection is direct.
    pub tunnel: Option<crate::tunnel::SshTunnel>,
}

/// Connect to `dsn`, apply the safety session settings, then run `bootstrap_sql`
/// and return its result.
///
/// TLS is negotiated via tokio-postgres' standard SSL flow (SSLRequest →
/// server S/N → handshake or plaintext continue). The mode follows the
/// `sslmode` URL param:
///
/// - `disable`              — plaintext only
/// - `prefer` (default)     — try TLS; fall back to plaintext on N
/// - `require` / `verify-*` — TLS required; fail if server says N
///
/// Trust roots come from the OS keychain via `rustls-native-certs`,
/// falling back to the Mozilla root bundle (`webpki-roots`) so a fresh
/// container without an installed trust store still connects to RDS.
/// Drive a `tokio_postgres::Connection` and surface its async messages.
///
/// We replace the standard `tokio::spawn(connection.await)` pattern
/// with a manual `poll_message` loop so server-emitted notices —
/// `RAISE NOTICE`, `RAISE WARNING`, `RAISE INFO` from functions or
/// `DO` blocks — get a path back to the App instead of being silently
/// dropped by the standard driver. `LISTEN`/`NOTIFY` is plumbed
/// through the same path on the eventual `LISTEN/NOTIFY` feature; for
/// now we discard `Notification` messages.
///
/// Generic over the stream type so it works for both the TLS and
/// NoTls connect branches.
fn spawn_connection_driver<S, T>(
    mut connection: tokio_postgres::Connection<S, T>,
    notice_tx: tokio::sync::mpsc::UnboundedSender<NoticeMsg>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<NotificationMsg>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use std::pin::Pin;
        use std::task::Poll;
        futures::future::poll_fn(|cx| -> Poll<()> {
            loop {
                match Pin::new(&mut connection).poll_message(cx) {
                    Poll::Ready(Some(Ok(msg))) => match msg {
                        tokio_postgres::AsyncMessage::Notice(notice) => {
                            let n = NoticeMsg {
                                severity: notice.severity().to_string(),
                                message: notice.message().to_string(),
                                detail: notice.detail().map(|s| s.to_string()),
                                hint: notice.hint().map(|s| s.to_string()),
                            };
                            let _ = notice_tx.send(n);
                        }
                        tokio_postgres::AsyncMessage::Notification(notif) => {
                            // LISTEN / NOTIFY arrival — forward into
                            // the App's notification ring via the
                            // dedicated channel.
                            let n = NotificationMsg {
                                channel: notif.channel().to_string(),
                                pid: notif.process_id(),
                                payload: notif.payload().to_string(),
                            };
                            let _ = notification_tx.send(n);
                        }
                        // `AsyncMessage` is #[non_exhaustive]; ignore
                        // anything we don't recognise.
                        _ => {}
                    },
                    Poll::Ready(Some(Err(e))) => {
                        tracing::warn!("postgres connection error: {e}");
                        return Poll::Ready(());
                    }
                    Poll::Ready(None) => return Poll::Ready(()),
                    Poll::Pending => return Poll::Pending,
                }
            }
        })
        .await;
    });
}

/// Connect to `dsn` and apply the safety session settings, but skip
/// the bootstrap-query + schema-cache fetch that the TUI needs. Used
/// by batch / pipe mode (`--batch`), which exits as soon as the
/// operator's SQL finishes — paying the schema-fetch cost there
/// would just delay scripted runs for nothing.
pub async fn connect_only(
    dsn: Dsn,
    read_only: bool,
    statement_timeout_ms: u64,
    notice_tx: tokio::sync::mpsc::UnboundedSender<NoticeMsg>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<NotificationMsg>,
) -> Result<
    (
        Arc<tokio_postgres::Client>,
        Option<crate::tunnel::SshTunnel>,
    ),
    String,
> {
    let (client, tunnel) = connect_inner(
        dsn,
        read_only,
        statement_timeout_ms,
        notice_tx,
        notification_tx,
    )
    .await?;
    Ok((Arc::new(client), tunnel))
}

pub async fn connect_and_bootstrap(
    dsn: Dsn,
    read_only: bool,
    statement_timeout_ms: u64,
    bootstrap_sql: String,
    notice_tx: tokio::sync::mpsc::UnboundedSender<NoticeMsg>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<NotificationMsg>,
) -> Result<Booted, String> {
    let (client, tunnel) = connect_inner(
        dsn,
        read_only,
        statement_timeout_ms,
        notice_tx,
        notification_tx,
    )
    .await?;
    let client = Arc::new(client);
    // The version probe, the bootstrap query, and the schema-cache fetch are
    // independent reads — run them concurrently (pipelined on the one
    // connection) instead of serially, cutting time-to-interactive on a
    // high-latency / tunnelled link.
    let (version_res, grid_res, schema_cache) = tokio::join!(
        client.query_one("SHOW server_version", &[]),
        run_query(&client, &bootstrap_sql),
        crate::query::schema::fetch(&client),
    );
    let server_version = version_res
        .ok()
        .and_then(|row| row.try_get::<usize, String>(0).ok())
        .unwrap_or_else(|| "unknown".to_string());
    let grid = grid_res.map_err(|e| e.to_string())?;
    Ok(Booted {
        server_version,
        grid,
        client,
        schema_cache,
        tunnel,
    })
}

/// Shared connect path for both `connect_only` and
/// `connect_and_bootstrap`. Opens the SSH tunnel (if any), builds the
/// `tokio_postgres::Config`, runs the handshake (TLS or NoTls
/// fallback), spawns the notice-aware connection driver, and applies
/// the read-only / statement-timeout session settings. Returns a raw
/// (non-Arc) client so callers can choose how to wrap it.
async fn connect_inner(
    dsn: Dsn,
    read_only: bool,
    statement_timeout_ms: u64,
    notice_tx: tokio::sync::mpsc::UnboundedSender<NoticeMsg>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<NotificationMsg>,
) -> Result<(tokio_postgres::Client, Option<crate::tunnel::SshTunnel>), String> {
    // Open the SSH tunnel (if configured) BEFORE building the
    // postgres Config — the host/port we hand to tokio-postgres point
    // at the local forward, not the real Postgres server. The tunnel
    // handle lives in `Booted` so the App keeps it alive for the
    // session.
    //
    // `spawn_blocking` because `SshTunnel::open` polls with
    // `TcpStream::connect_timeout` and `thread::sleep`, which would
    // block the tokio reactor if called inline.
    let tunnel = match dsn.ssh_tunnel.clone() {
        Some(spec) => {
            let remote_host = dsn.host.clone();
            let remote_port = dsn.port;
            let opened = tokio::task::spawn_blocking(move || {
                crate::tunnel::SshTunnel::open(&spec, &remote_host, remote_port)
            })
            .await
            .map_err(|e| format!("ssh tunnel task panicked: {e}"))??;
            tracing::info!(
                "SSH tunnel up — 127.0.0.1:{} → {}:{}",
                opened.local_port,
                dsn.host,
                dsn.port
            );
            Some(opened)
        }
        None => None,
    };

    let mut cfg = tokio_postgres::Config::new();
    match &tunnel {
        Some(t) => {
            // `host` is the canonical name — used for TLS SNI and cert
            // verification — and stays as the real server we'd be
            // talking to without the tunnel. `hostaddr` overrides
            // where the TCP connection actually goes: 127.0.0.1 +
            // the local forward port. That keeps TLS-through-the-
            // tunnel working without disabling hostname checks.
            cfg.host(&dsn.host)
                .hostaddr(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
                .port(t.local_port)
                .dbname(&dsn.dbname);
        }
        None => {
            cfg.host(&dsn.host).port(dsn.port).dbname(&dsn.dbname);
        }
    }
    if let Some(user) = &dsn.user {
        cfg.user(user);
    }
    if let Some(password) = &dsn.password {
        cfg.password(password);
    }
    cfg.application_name(application_name(&dsn));
    let verify = apply_ssl_mode(&mut cfg, &dsn);

    // Connect with TLS when we can build a connector; fall back to
    // NoTls only when connector init itself fails (e.g. crypto provider
    // panic) — that's expected to be rare. The connection driver is
    // spawned inside each branch because its stream type depends on
    // which connector ran the handshake.
    let client = match build_tls_connector(verify) {
        Ok(connector) => {
            let (client, connection) = cfg
                .connect(connector)
                .await
                .map_err(|e| chain_message(&e))?;
            spawn_connection_driver(connection, notice_tx.clone(), notification_tx.clone());
            client
        }
        Err(tls_err) => {
            // TLS connector setup failed. Fall back to NoTls so localhost
            // / dev databases still connect; the operator will see the
            // warning in the log if they were expecting TLS.
            tracing::warn!("TLS connector init failed ({tls_err}); falling back to plaintext");
            let (client, connection) = cfg
                .connect(tokio_postgres::NoTls)
                .await
                .map_err(|e| chain_message(&e))?;
            spawn_connection_driver(connection, notice_tx.clone(), notification_tx.clone());
            client
        }
    };

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

    Ok((client, tunnel))
}

/// Run `sql` and collect the result into a `Grid`, handling both row-returning
/// statements (`SELECT`, `EXPLAIN`, `SHOW`, …) and non-row-returning ones
/// (`UPDATE`, `DELETE`, DDL). Non-row statements yield a single-cell grid with
/// the affected-row count.
///
/// Row-returning statements run over the *text* wire format
/// ([`stream_text_rows`]) so every column type renders as psql prints
/// it — a timestamptz, numeric, uuid, json, array, interval, bytea,
/// money or inet cell used to come back as `?` because the binary
/// path decoded only bool / int / float / text. `prepare` (Parse +
/// Describe, no execution) still supplies the column shape and decides
/// the branch; the statement executes exactly once either way. Rows
/// are capped at `grid::MAX_ROWS`; if the underlying result is larger,
/// the returned `Grid` carries `truncated: true` so the renderer can
/// surface that.
pub async fn run_statement(client: &tokio_postgres::Client, sql: &str) -> Result<Grid, QueryErr> {
    let stmt = client.prepare(sql).await.map_err(QueryErr::from)?;
    let columns = stmt.columns();
    if columns.is_empty() {
        let affected = client.execute(&stmt, &[]).await.map_err(QueryErr::from)?;
        Ok(Grid {
            columns: vec!["status".to_string()],
            rows: vec![vec![format!("{affected} row(s) affected")]],
            truncated: false,
        })
    } else {
        let column_names: Vec<String> = columns.iter().map(|c| c.name().to_string()).collect();
        let (rows, truncated) = stream_text_rows(client, sql, column_names.len()).await?;
        Ok(Grid {
            columns: column_names,
            rows,
            truncated,
        })
    }
}

/// One result row as the grid shows it: the text-wire cells lined up
/// with the `n_columns` Describe promised ([`project_cells`]) and SQL
/// NULL rendered as the empty string — the same convention
/// [`cell_to_string`] uses, which `grid::cmp_cells` sorts last. The
/// shared row→cells step of every result-set path (TUI grid, `--batch
/// csv` / `tsv` / `expanded`). Pure / testable.
pub fn display_cells(cells: Vec<Option<String>>, n_columns: usize) -> Vec<String> {
    project_cells(cells, n_columns)
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect()
}

/// Stream a row-returning statement over the simple-query (text)
/// protocol into display strings, stopping at `grid::MAX_ROWS`. Every
/// type arrives already rendered by the server — no client-side
/// decoder to be missing. Returns `(rows, truncated)` like
/// [`stream_rows`].
async fn stream_text_rows(
    client: &tokio_postgres::Client,
    sql: &str,
    n_columns: usize,
) -> Result<(Vec<Vec<String>>, bool), QueryErr> {
    use futures::StreamExt;
    let stream = client.simple_query_raw(sql).await.map_err(QueryErr::from)?;
    let mut stream = Box::pin(stream);
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut truncated = false;
    while let Some(msg) = stream.next().await {
        let tokio_postgres::SimpleQueryMessage::Row(row) = msg.map_err(QueryErr::from)? else {
            continue;
        };
        if out.len() >= crate::grid::MAX_ROWS {
            truncated = true;
            break;
        }
        // Read only what the row carries (see `run_statement_typed`):
        // `SimpleQueryRow::get` panics past the row's own width.
        let cells: Vec<Option<String>> = (0..row.columns().len())
            .map(|i| row.try_get(i).ok().flatten().map(str::to_string))
            .collect();
        out.push(display_cells(cells, n_columns));
    }
    Ok((out, truncated))
}

#[cfg(test)]
mod display_cells_tests {
    use super::display_cells;

    fn cells(items: &[Option<&str>]) -> Vec<Option<String>> {
        items.iter().map(|c| c.map(str::to_string)).collect()
    }

    #[test]
    fn text_wire_values_pass_through_and_null_is_the_empty_string() {
        let got = display_cells(
            cells(&[
                Some("2024-05-06 07:08:09+00"),
                None,
                Some("{1,2}"),
                Some(""),
            ]),
            4,
        );
        assert_eq!(got, vec!["2024-05-06 07:08:09+00", "", "{1,2}", ""]);
    }

    #[test]
    fn a_short_row_is_padded_and_a_long_row_cut_to_the_described_width() {
        assert_eq!(display_cells(cells(&[Some("a")]), 3), vec!["a", "", ""]);
        assert_eq!(
            display_cells(cells(&[Some("a"), Some("b"), Some("c")]), 2),
            vec!["a", "b"]
        );
        assert!(display_cells(Vec::new(), 0).is_empty());
    }
}

/// Column names + Postgres [`Type`]s, plus the text-wire value of every
/// cell (`None` for SQL NULL), for `--batch --format json`'s typed
/// path ([`batch::run_statement_typed_json`]). Unlike `run_statement`,
/// which renders every cell to a display `String` and loses the
/// NULL-vs-empty-string / numeric-vs-text distinction on the way,
/// this keeps the `Type` so the caller can decide what belongs in
/// quotes.
///
/// [`batch::run_statement_typed_json`]: crate::batch::run_statement_typed_json
pub struct TypedRows {
    pub columns: Vec<(String, tokio_postgres::types::Type)>,
    /// One entry per result row; each inner `Vec` aligned with `columns`.
    /// Empty when `affected` is `Some` (a DDL/DML statement with no
    /// `RETURNING` — nothing to type).
    pub rows: Vec<Vec<Option<String>>>,
    /// `Some(n)` for a non-row-returning statement (`client.execute`'s
    /// affected-row count); `None` for a row-returning one.
    pub affected: Option<u64>,
}

/// Line up one result row's cells with the column list Describe
/// promised: extra cells are dropped, missing ones become SQL NULL.
///
/// The two are read a round trip apart — `prepare` describes the shape,
/// `simple_query` fetches the data — so a view redefined in between (or
/// any other concurrent DDL) can return a row narrower or wider than
/// the header. The JSON writer pairs `columns[i]` with `cells[i]` and
/// must get a rectangle; a short row used to reach it as a panic
/// instead.
fn project_cells(mut cells: Vec<Option<String>>, n_columns: usize) -> Vec<Option<String>> {
    cells.resize(n_columns, None);
    cells
}

/// Run a single statement and collect it into [`TypedRows`]. `client
/// .prepare` gives the column shape via Parse+Describe (no
/// execution); a row-returning statement then runs over the *text*
/// wire format via `client.simple_query` — so int/float/numeric
/// values never need a binary decoder pgman doesn't have
/// (`tokio-postgres` has no `FromSql<String>` for `NUMERIC`). Exactly
/// one execution happens either way.
pub async fn run_statement_typed(
    client: &tokio_postgres::Client,
    sql: &str,
) -> Result<TypedRows, QueryErr> {
    let stmt = client.prepare(sql).await.map_err(QueryErr::from)?;
    let columns: Vec<(String, tokio_postgres::types::Type)> = stmt
        .columns()
        .iter()
        .map(|c| (c.name().to_string(), c.type_().clone()))
        .collect();
    if columns.is_empty() {
        let affected = client.execute(&stmt, &[]).await.map_err(QueryErr::from)?;
        return Ok(TypedRows {
            columns: Vec::new(),
            rows: Vec::new(),
            affected: Some(affected),
        });
    }
    let messages = client.simple_query(sql).await.map_err(QueryErr::from)?;
    let mut rows = Vec::new();
    for msg in &messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            // `columns` came from Parse+Describe; these cells come from
            // a second round trip. A view redefined in between narrows
            // the row, and `SimpleQueryRow::get` *panics* out of range
            // — aborting `--batch --format json` mid-document. Read
            // only what the row actually carries and let
            // `project_cells` pad the rest with SQL NULL.
            let cells: Vec<Option<String>> = (0..row.columns().len())
                .map(|i| row.try_get(i).ok().flatten().map(str::to_string))
                .collect();
            rows.push(project_cells(cells, columns.len()));
        }
    }
    Ok(TypedRows {
        columns,
        rows,
        affected: None,
    })
}

/// Stream rows from a prepared statement (no params) into string-rendered
/// vectors, stopping at `grid::MAX_ROWS`. Returns `(rows, truncated)` where
/// `truncated` is `true` iff at least one additional row existed past the cap.
async fn stream_rows(
    client: &tokio_postgres::Client,
    stmt: &tokio_postgres::Statement,
) -> Result<(Vec<Vec<String>>, bool), QueryErr> {
    use futures::StreamExt;
    use tokio_postgres::types::ToSql;
    let params: [&(dyn ToSql + Sync); 0] = [];
    let stream = client
        .query_raw(stmt, params)
        .await
        .map_err(QueryErr::from)?;
    let mut stream = Box::pin(stream);
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut truncated = false;
    while let Some(row_res) = stream.next().await {
        let row = row_res.map_err(QueryErr::from)?;
        if out.len() >= crate::grid::MAX_ROWS {
            truncated = true;
            break;
        }
        out.push((0..row.len()).map(|i| cell_to_string(&row, i)).collect());
    }
    Ok((out, truncated))
}

/// Open a transaction and run `sql`. On success the transaction is **left
/// open** — the caller (usually the App's commit/rollback prompt) decides
/// `COMMIT` or `ROLLBACK`. On error the transaction is rolled back immediately
/// so the session doesn't sit aborted.
pub async fn run_in_tx_open(client: &tokio_postgres::Client, sql: &str) -> Result<Grid, QueryErr> {
    client
        .batch_execute("BEGIN")
        .await
        .map_err(QueryErr::from)?;
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
pub async fn run_batch(client: &tokio_postgres::Client, sql: &str) -> Result<Grid, QueryErr> {
    client.batch_execute(sql).await.map_err(QueryErr::from)?;
    Ok(status_grid("batch executed"))
}

/// Run a multi-statement script inside one explicit transaction —
/// `BEGIN; <script>; COMMIT` — rolled back if any statement fails.
/// `--batch`'s executor.
///
/// The simple-query protocol already runs a multi-statement string in an
/// implicit transaction, so for a script with no transaction control of
/// its own this changes nothing on the wire. The wrapper exists for
/// `batch::check_batch_safety`'s sake, which under a read-only profile
/// refuses any top-level `COMMIT` / `END` / `ROLLBACK` / `BEGIN` / `START
/// TRANSACTION` in the script. Together they pin the whole script to one
/// transaction that began read-only: a `default_transaction_read_only`
/// lifted mid-script (however the classifier missed it) only reaches the
/// *next* transaction, and there is none.
///
/// The TUI's multi-statement path deliberately does not use this — a
/// `BEGIN; UPDATE …` typed there under `auto_tx = false` is meant to stay
/// open for the operator's own `COMMIT`.
pub async fn run_batch_in_tx(client: &tokio_postgres::Client, sql: &str) -> Result<Grid, QueryErr> {
    match client.batch_execute(&wrap_in_transaction(sql)).await {
        Ok(()) => Ok(status_grid("batch executed")),
        Err(e) => {
            // An error mid-script leaves the transaction aborted; end it
            // rather than drop the connection with one open.
            let _ = client.batch_execute("ROLLBACK").await;
            Err(QueryErr::from(e))
        }
    }
}

/// The script [`run_batch_in_tx`] sends. Pure so the shape is testable.
/// The newline before the `;` ends a `-- line comment` the last statement
/// may finish with — the same reason `safety::join_verified`'s separator
/// is `"\n;\n"` — so the `COMMIT` can never be commented out.
pub fn wrap_in_transaction(sql: &str) -> String {
    format!("BEGIN;\n{sql}\n;\nCOMMIT")
}

#[cfg(test)]
mod batch_tx_tests {
    use super::wrap_in_transaction;
    use crate::safety::split_statements;

    #[test]
    fn the_wrapper_brackets_the_script_and_survives_a_trailing_line_comment() {
        assert_eq!(
            wrap_in_transaction("SELECT 1\n;\nSELECT 2"),
            "BEGIN;\nSELECT 1\n;\nSELECT 2\n;\nCOMMIT"
        );
        // The last statement ends in a line comment: the `COMMIT` still
        // splits out as its own statement rather than vanishing into it.
        let wrapped = wrap_in_transaction("SELECT 1 -- trailing");
        assert_eq!(
            split_statements(&wrapped),
            vec!["BEGIN", "SELECT 1 -- trailing", "COMMIT"]
        );
    }
}

/// Run a multi-statement script inside an explicit transaction that is
/// **left open** on success (caller commits or rolls back). On error in the
/// batch, rolls back immediately.
pub async fn run_batch_in_tx_open(
    client: &tokio_postgres::Client,
    sql: &str,
) -> Result<Grid, QueryErr> {
    client
        .batch_execute("BEGIN")
        .await
        .map_err(QueryErr::from)?;
    match client.batch_execute(sql).await {
        Ok(()) => Ok(status_grid("batch ran — awaiting commit/rollback")),
        Err(e) => {
            let _ = client.batch_execute("ROLLBACK").await;
            Err(QueryErr::from(e))
        }
    }
}

fn status_grid(msg: &str) -> Grid {
    Grid {
        columns: vec!["status".to_string()],
        rows: vec![vec![msg.to_string()]],
        truncated: false,
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
) -> Result<Grid, QueryErr> {
    client
        .batch_execute("BEGIN")
        .await
        .map_err(QueryErr::from)?;
    let result = run_statement(client, sql).await;
    let _ = client.batch_execute("ROLLBACK").await;
    result
}

/// Run `sql` and collect the result into a `Grid` (capped at `grid::MAX_ROWS`).
///
/// Streams via `query_raw` and sets `Grid.truncated` if more rows existed
/// past the cap.
///
/// Errors come back as [`QueryErr`] — the server's message with its
/// SQLSTATE / detail / hint — not `tokio_postgres::Error`'s bare
/// `db error` Display. The panel loads (`T`, `L`) show the message in
/// the footer and the rest behind F2; a missing `pg_stat_statements`
/// is only recognisable from the message.
pub async fn run_query(client: &tokio_postgres::Client, sql: &str) -> Result<Grid, QueryErr> {
    let stmt = client.prepare(sql).await.map_err(QueryErr::from)?;
    let columns: Vec<String> = stmt
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    let (rows, truncated) = stream_rows(client, &stmt).await?;
    Ok(Grid {
        columns,
        rows,
        truncated,
    })
}

/// Terminate the backend with `pid` via `pg_terminate_backend`. A
/// data-layer primitive so the parameterised `Db` call stays out of the
/// app layer (the session-terminate confirm flow lives in the UI, but
/// the query runs here).
pub async fn terminate_backend(client: &tokio_postgres::Client, pid: i32) -> Result<(), String> {
    client
        .query_opt("SELECT pg_terminate_backend($1)", &[&pid])
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
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

/// The exact set of `sslmode` values `Dsn::parse` accepts (after
/// trimming and ASCII-lowercasing). Anything else — a typo, the wrong
/// separator (`verify_full`), wrong case surviving some other way, an
/// empty value — is a hard parse error. See `apply_ssl_mode`: the
/// previous behaviour silently downgraded an unrecognised value to
/// `prefer` (encrypt-without-verifying, falls back to plaintext), which
/// is a strictly weaker guarantee than most of the other five modes —
/// exactly the kind of security setting that must never fail open.
const KNOWN_SSLMODES: [&str; 6] = [
    "disable",
    "allow",
    "prefer",
    "require",
    "verify-ca",
    "verify-full",
];

/// The `application_name` every pgman connection identifies itself with
/// in `pg_stat_activity` (and a DBA's grep of it): `pgman/<version>`,
/// unless the DSN carries its own `application_name=` — libpq honours
/// that parameter and an operator who set it meant it. Pure / testable.
pub fn application_name(dsn: &Dsn) -> String {
    dsn.params
        .iter()
        .find(|(k, v)| k.eq_ignore_ascii_case("application_name") && !v.is_empty())
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| format!("pgman/{}", env!("CARGO_PKG_VERSION")))
}

/// Apply `sslmode` from `dsn.params` to a `tokio_postgres::Config` and
/// return whether certificate verification should be performed by the
/// TLS connector. Matches libpq's semantics:
///
/// - `disable`              → plaintext only (no verify regardless)
/// - `allow`                → encrypt if the server demands it, don't
///   verify. libpq's `allow` tries plaintext first and retries with
///   TLS only if refused; pgman makes a single connection attempt, so
///   there's no "try order" to preserve — we get the same eventual
///   encryption state as `prefer` either way, just not libpq's
///   negotiation *order* preference (which has no observable effect
///   once the server states its own requirement).
/// - `prefer` / `require`   → encrypt without verifying — works against
///   self-signed dev databases. `require` differs from `prefer` only in
///   the connector's handling when the server says no: `require` fails.
/// - `verify-ca` / `verify-full` → encrypt AND verify the chain (and,
///   for verify-full, the hostname). `tokio-postgres-rustls`'s default
///   verifier checks both; we currently collapse verify-ca onto
///   verify-full (a noted follow-up — verify-ca-without-hostname needs
///   a custom rustls verifier). This makes pgman's `verify-ca` strictly
///   *stricter* than libpq's (which does not check the hostname for
///   `verify-ca`) — a deliberate simplification, not a bug: it can
///   only ever reject a connection libpq's `verify-ca` would accept,
///   never the reverse.
///
/// `Dsn::parse` validates `sslmode` against [`KNOWN_SSLMODES`] before
/// this ever runs, so every arm below except the last is reachable
/// only with a value from that exact set — the `unreachable!` is a
/// tripwire for that invariant, not a real code path.
fn apply_ssl_mode(cfg: &mut tokio_postgres::Config, dsn: &Dsn) -> bool {
    use tokio_postgres::config::SslMode;
    let mode = dsn
        .params
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("sslmode"))
        .map(|(_, v)| v.as_str());
    match mode {
        Some("disable") => {
            cfg.ssl_mode(SslMode::Disable);
            false
        }
        Some("allow") | Some("prefer") | None => {
            cfg.ssl_mode(SslMode::Prefer);
            false
        }
        Some("require") => {
            cfg.ssl_mode(SslMode::Require);
            false
        }
        Some("verify-ca") | Some("verify-full") => {
            cfg.ssl_mode(SslMode::Require);
            true
        }
        Some(other) => {
            unreachable!("Dsn::parse should have rejected sslmode={other:?}")
        }
    }
}

/// Build a `MakeRustlsConnect` for the Postgres TLS handshake. When
/// `verify` is true (sslmode=verify-ca / verify-full) we build a
/// strict verifier from the union of the OS keychain (corporate CAs)
/// and Mozilla's `webpki-roots` bundle (covers RDS, containers without
/// a populated native store). When `verify` is false (sslmode=require
/// / prefer) we install a no-op verifier that matches libpq's "encrypt
/// without verifying" behaviour — without this, self-signed dev DBs
/// would fail the handshake.
fn build_tls_connector(verify: bool) -> Result<tokio_postgres_rustls::MakeRustlsConnect, String> {
    // Install the default crypto provider once per process — rustls 0.23
    // requires this before any ClientConfig is built.
    static CRYPTO_INIT: std::sync::Once = std::sync::Once::new();
    CRYPTO_INIT.call_once(|| {
        // Ignore the result: re-installing after another caller already
        // did is fine; first-write wins.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });

    let config = if verify {
        let mut root_store = rustls::RootCertStore::empty();
        match rustls_native_certs::load_native_certs() {
            r if !r.certs.is_empty() => {
                let (added, _ignored) = root_store.add_parsable_certificates(r.certs);
                tracing::debug!("loaded {added} native root cert(s)");
                if !r.errors.is_empty() {
                    // Surface load errors as warn-level — a denied
                    // keychain prompt on macOS otherwise silently drops
                    // corporate CAs and the operator can't figure out
                    // why the connection later fails with UnknownIssuer.
                    for e in &r.errors {
                        tracing::warn!("native-certs partial load error: {e}");
                    }
                }
            }
            r => {
                if !r.errors.is_empty() {
                    for e in &r.errors {
                        tracing::warn!("native-certs load error: {e}");
                    }
                } else {
                    tracing::debug!("no native root certs available; relying on webpki-roots");
                }
            }
        }
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    } else {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(NoVerifier))
            .with_no_client_auth()
    };
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

/// Rustls verifier that accepts any server cert. Used for
/// `sslmode=require` / `prefer` — encrypts the wire but doesn't
/// validate the peer. Matches libpq's semantics for those modes.
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme::*;
        vec![
            RSA_PKCS1_SHA256,
            RSA_PKCS1_SHA384,
            RSA_PKCS1_SHA512,
            RSA_PSS_SHA256,
            RSA_PSS_SHA384,
            RSA_PSS_SHA512,
            ECDSA_NISTP256_SHA256,
            ECDSA_NISTP384_SHA384,
            ECDSA_NISTP521_SHA512,
            ED25519,
        ]
    }
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
    // SSH-tunnel failures come back from `SshTunnel::open` before we
    // ever talk to Postgres — surface concrete next steps. The
    // patterns match the error strings we emit ourselves.
    if lower.contains("ssh exited before the tunnel was ready")
        || lower.contains("ssh tunnel didn't open")
        || lower.contains("failed to spawn ssh")
    {
        let target = dsn
            .ssh_tunnel
            .as_ref()
            .map(|s| s.to_display())
            .unwrap_or_else(|| "(unknown)".to_string());
        return Some(format!(
            "ssh tunnel via {target} didn't come up. try `ssh -v {target}` manually — pgman runs with BatchMode=yes so an unloaded key / agent will fail fast"
        ));
    }
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
        // Telling an operator whose DSN already carried a password to
        // "pass user:pass in --dsn" sends them in a circle. The DSN
        // knows whether one was supplied; the value itself is never
        // shown.
        return Some(if dsn.password.is_some() {
            format!(
                "wrong password for user {:?} — the DSN (or its credential source) supplied one; check that value, or which user / database it belongs to",
                dsn.user.as_deref().unwrap_or("")
            )
        } else {
            "wrong password. set PGPASSWORD before launching pgman, or pass user:pass in --dsn"
                .to_string()
        });
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
    fn project_cells_pads_a_row_narrower_than_its_header() {
        // The shape a view redefined between Parse/Describe and
        // simple_query produces: three columns promised, two returned.
        assert_eq!(
            project_cells(vec![Some("1".into()), None], 3),
            vec![Some("1".into()), None, None],
            "the missing cell must arrive as SQL NULL, not as a panic"
        );
    }

    #[test]
    fn project_cells_drops_a_row_wider_than_its_header() {
        assert_eq!(
            project_cells(vec![Some("1".into()), Some("2".into())], 1),
            vec![Some("1".into())]
        );
    }

    #[test]
    fn project_cells_leaves_a_matching_row_alone() {
        let cells = vec![Some("a".into()), None];
        assert_eq!(project_cells(cells.clone(), 2), cells);
        assert!(project_cells(Vec::new(), 0).is_empty());
    }

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
        assert_eq!(
            Dsn::parse("db.example.com/orders"),
            Err(DsnError::MissingScheme)
        );
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

    // --- Security-review: sslmode is validated, never silently downgraded ---

    #[test]
    fn rejects_uppercase_sslmode_separator_typo() {
        // Was: byte-exact match missed this, fell through to the
        // `Prefer` + no-op-verifier fallback with only a `tracing::warn!`
        // the operator would never see behind the alternate screen.
        // `verify_full` (underscore, not a case variant) is a genuine
        // typo — it must be rejected, not guessed at.
        assert_eq!(
            Dsn::parse("postgres://h/d?sslmode=verify_full"),
            Err(DsnError::UnknownSslMode("verify_full".to_string()))
        );
    }

    #[test]
    fn rejects_empty_sslmode() {
        assert_eq!(
            Dsn::parse("postgres://h/d?sslmode="),
            Err(DsnError::UnknownSslMode(String::new()))
        );
    }

    #[test]
    fn rejects_garbage_sslmode() {
        assert_eq!(
            Dsn::parse("postgres://h/d?sslmode=yolo"),
            Err(DsnError::UnknownSslMode("yolo".to_string()))
        );
    }

    #[test]
    fn accepts_uppercase_sslmode_case_insensitively() {
        // Wrong CASE is not a typo — `VERIFY-FULL` unambiguously means
        // `verify-full`, so it's accepted (and normalised), not
        // rejected. This is the actual security fix: before, this
        // fell through to the silent-downgrade fallback instead of
        // being treated as the strict mode it obviously is.
        let d = Dsn::parse("postgres://h/d?sslmode=VERIFY-FULL").unwrap();
        assert_eq!(
            d.params,
            vec![("sslmode".to_string(), "verify-full".to_string())]
        );
    }

    #[test]
    fn accepts_sslmode_with_trailing_whitespace_or_cr() {
        // A stray trailing space or CR (Windows-authored config /
        // .env files) is whitespace noise, not a typo — trim it and
        // accept the mode it clearly names. The whitespace sits
        // *before* a following `&param`, not at the very end of the
        // DSN string, so this exercises the per-value trim in the
        // params loop rather than `Dsn::parse`'s outer `dsn.trim()`
        // (which would mask the bug by trimming the whole string).
        let d = Dsn::parse("postgres://h/d?sslmode=verify-full &application_name=x").unwrap();
        assert_eq!(
            d.params,
            vec![
                ("sslmode".to_string(), "verify-full".to_string()),
                ("application_name".to_string(), "x".to_string()),
            ]
        );
        let d2 = Dsn::parse("postgres://h/d?sslmode=verify-full\r&application_name=x").unwrap();
        assert_eq!(
            d2.params,
            vec![
                ("sslmode".to_string(), "verify-full".to_string()),
                ("application_name".to_string(), "x".to_string()),
            ]
        );
    }

    #[test]
    fn accepts_allow_sslmode() {
        let d = Dsn::parse("postgres://h/d?sslmode=allow").unwrap();
        assert_eq!(d.params, vec![("sslmode".to_string(), "allow".to_string())]);
    }

    #[test]
    fn ssh_tunnel_url_param_extracted_into_field_and_dropped_from_params() {
        let dsn = Dsn::parse(
            "postgres://app@db.internal:5432/app?sslmode=require&ssh_tunnel=tom@bastion:2222",
        )
        .unwrap();
        let spec = dsn.ssh_tunnel.as_ref().expect("ssh_tunnel set");
        assert_eq!(spec.user.as_deref(), Some("tom"));
        assert_eq!(spec.host, "bastion");
        assert_eq!(spec.port, Some(2222));
        // The non-tunnel param survives; the tunnel param is filtered
        // out so apply_ssl_mode etc. don't see a stale key.
        assert!(dsn
            .params
            .iter()
            .any(|(k, v)| k == "sslmode" && v == "require"));
        assert!(!dsn.params.iter().any(|(k, _)| k == "ssh_tunnel"));
    }

    #[test]
    fn malformed_ssh_tunnel_param_warns_and_leaves_field_unset() {
        // Malformed tunnel doesn't fail the DSN — operator may still
        // reach the db directly. Field stays None, the param is
        // dropped, and the warning lands in tracing.
        let dsn = Dsn::parse("postgres://db/app?ssh_tunnel=:bad-port:99999").unwrap();
        assert!(dsn.ssh_tunnel.is_none());
        assert!(!dsn.params.iter().any(|(k, _)| k == "ssh_tunnel"));
    }

    #[test]
    fn redacted_appends_ssh_tunnel_when_present() {
        let dsn = Dsn::parse("postgres://app:pw@db/app?ssh_tunnel=tom@bastion").unwrap();
        let s = dsn.redacted();
        assert!(s.contains("***"), "password should be masked: {s}");
        assert!(
            s.contains("via ssh://tom@bastion"),
            "tunnel target should appear: {s}"
        );
    }

    #[test]
    fn redact_url_masks_userinfo_and_password_params() {
        // Inline userinfo.
        assert_eq!(
            super::redact_url("postgres://user:s3cret@host:5432/db"),
            "postgres://***@host:5432/db"
        );
        // JDBC scheme + password query param (no userinfo).
        assert_eq!(
            super::redact_url("jdbc:postgresql://host/db?user=app&password=s3cret"),
            "jdbc:postgresql://host/db?user=app&password=***"
        );
        // Both userinfo and a trailing password param, mid-query.
        assert_eq!(
            super::redact_url("postgres://u:p@host/db?password=abc&sslmode=require"),
            "postgres://***@host/db?password=***&sslmode=require"
        );
        // pwd= alias.
        assert_eq!(
            super::redact_url("postgresql://host/db?pwd=hunter2"),
            "postgresql://host/db?pwd=***"
        );
        // Nothing to redact — passes through unchanged.
        assert_eq!(
            super::redact_url("postgres://host:5432/db?sslmode=disable"),
            "postgres://host:5432/db?sslmode=disable"
        );
        // Garbage that failed to parse still gets userinfo scrubbed.
        assert_eq!(
            super::redact_url("postgres://admin:letmein@:notaport/x"),
            "postgres://***@:notaport/x"
        );
        // Crucially: the raw secret never survives.
        for masked in [
            super::redact_url("postgres://u:topsecret@h/d"),
            super::redact_url("jdbc:postgresql://h/d?password=topsecret"),
        ] {
            assert!(!masked.contains("topsecret"), "leak: {masked}");
        }
    }

    // --- Security-review repros: passwords containing `/` or `@` ----------

    #[test]
    fn redact_url_masks_jdbc_password_containing_slash() {
        // Was: unchanged (LEAK) — the naive "authority ends at the
        // first '/'" rule cut the authority at the '/' inside the
        // password, leaving "ss@db.host/app" past the "userinfo" scan
        // window entirely unmasked.
        let masked = super::redact_url("jdbc:postgresql://svc:pa/ss@db.host/app");
        assert_eq!(masked, "jdbc:postgresql://***@db.host/app");
        assert!(!masked.contains("pa/ss"), "leak: {masked}");
    }

    #[test]
    fn redact_url_masks_password_containing_at() {
        // Was: "postgres://***@ssw0rd@db.host/app" (LEAK) — matching
        // the FIRST '@' left the rest of the password, "ssw0rd@", in
        // the output. The fix takes the LAST '@' before the path.
        let masked = super::redact_url("postgres://svc:p@ssw0rd@db.host/app");
        assert_eq!(masked, "postgres://***@db.host/app");
        assert!(!masked.contains("ssw0rd"), "leak: {masked}");
    }

    #[test]
    fn redact_url_masks_password_containing_question_mark_or_hash() {
        // Was: unchanged (LEAK). The userinfo scan cut the string at
        // the FIRST '?' or '#' anywhere in it — which, for a password
        // holding one, landed before the real '@'. No userinfo was
        // found, so nothing was masked, and `redact_url` is exactly
        // what runs on a URL that failed to parse, on its way to the
        // log.
        for (raw, want) in [
            ("postgres://u:p?ss@h/d", "postgres://***@h/d"),
            ("postgres://u:p#ss@h/d", "postgres://***@h/d"),
            (
                "postgres://svc:pa?s#s@db.host:5432/app",
                "postgres://***@db.host:5432/app",
            ),
            // No path at all — the ambiguous shape, resolved towards
            // masking.
            ("postgres://u:p?ss@h", "postgres://***@h"),
            // A password mixing '/' with '?' cannot be parsed back
            // out, but it must still never be printed.
            ("postgres://u:p/s?s@h:5432/d", "postgres://***@h:5432/d"),
            // Found by `redact_url_never_leaks_any_raw_password`: with
            // '@', '/' and '?' all in the password, `split_authority`
            // picks the FIRST '@' as the boundary and the rest of the
            // password ("/?") survived past the mask. Redaction cuts
            // at the last plausible '@', not the parser's.
            ("postgres://u:@/?@h:5432/d", "postgres://***@h:5432/d"),
        ] {
            let masked = super::redact_url(raw);
            assert_eq!(masked, want, "for {raw}");
            assert!(!masked.contains("ss"), "leak: {masked}");
        }
    }

    #[test]
    fn redact_url_leaves_an_at_sign_in_a_query_parameter_alone() {
        // The other side of the rule: once the path has begun, a '?'
        // really does start the query, so an '@' in a parameter value
        // is not a userinfo boundary and the host stays readable.
        assert_eq!(
            super::redact_url("postgres://db.host:5432/app?application_name=svc@box"),
            "postgres://db.host:5432/app?application_name=svc@box"
        );
    }

    #[test]
    fn parses_password_containing_question_mark_or_hash() {
        // Was: user=None, host="u", port parse of "p" -> DsnError, so
        // the string fell through to `redact_url` unparsed.
        let d = Dsn::parse("postgres://u:p?ss@h/d").unwrap();
        assert_eq!(d.user.as_deref(), Some("u"));
        assert_eq!(d.password.as_deref(), Some("p?ss"));
        assert_eq!(d.host, "h");
        assert_eq!(d.dbname, "d");

        let d = Dsn::parse("postgres://svc:pa#ss@db.host:5432/app").unwrap();
        assert_eq!(d.password.as_deref(), Some("pa#ss"));
        assert_eq!(d.host, "db.host");
        assert_eq!(d.port, 5432);
        assert_eq!(d.dbname, "app");
    }

    #[test]
    fn a_query_parameter_is_not_mistaken_for_userinfo() {
        // The bound on the userinfo search has to stop at the query
        // once a path has begun, or `@` in a parameter value would be
        // read as the credential boundary.
        let d = Dsn::parse("postgres://db.host:5432/app?application_name=svc@box").unwrap();
        assert_eq!(d.user, None);
        assert_eq!(d.password, None);
        assert_eq!(d.host, "db.host");
        assert_eq!(d.dbname, "app");
    }

    #[test]
    fn parses_password_containing_slash_and_colon() {
        // Was: host="app:pa", dbname="ss@db.host:5432/orders" — the
        // authority/path split (on the first '/') ran before the
        // userinfo split, so a '/' inside the password was mistaken
        // for the path separator.
        let d = Dsn::parse("postgres://app:pa:1234/ss@db.host:5432/orders").unwrap();
        assert_eq!(d.user.as_deref(), Some("app"));
        assert_eq!(d.password.as_deref(), Some("pa:1234/ss"));
        assert_eq!(d.host, "db.host");
        assert_eq!(d.port, 5432);
        assert_eq!(d.dbname, "orders");
        let r = d.redacted();
        assert!(!r.contains("pa:1234/ss"), "leak: {r}");
        assert!(!r.contains("app:pa"), "leak: {r}");
    }

    #[test]
    fn parses_password_containing_embedded_at() {
        let d = Dsn::parse("postgres://svc:p@ssw0rd@db.host/app").unwrap();
        assert_eq!(d.user.as_deref(), Some("svc"));
        assert_eq!(d.password.as_deref(), Some("p@ssw0rd"));
        assert_eq!(d.host, "db.host");
        assert_eq!(d.dbname, "app");
    }

    #[test]
    fn belt_and_braces_redacted_host_never_carries_colon_or_at() {
        // However mangled the userinfo, `host` must come out clean —
        // if it ever picked up a stray ':' or '@' from a misparsed
        // password, `redacted()` would echo raw credential material.
        for dsn_str in [
            "postgres://svc:p@ssw0rd@db.host/app",
            "postgres://app:pa:1234/ss@db.host:5432/orders",
            "postgres://user:s3cr3t/with/slashes@db.example.com:5432/orders",
        ] {
            let d = Dsn::parse(dsn_str).unwrap();
            assert!(
                !d.host.contains(':') && !d.host.contains('@'),
                "dirty host {:?} from {dsn_str}",
                d.host
            );
            let r = d.redacted();
            if let Some(pw) = &d.password {
                assert!(!r.contains(pw.as_str()), "redacted() leaked password: {r}");
            }
        }
    }

    #[test]
    fn userinfo_is_percent_decoded() {
        // libpq decodes percent-escapes in a URI's userinfo; pgman
        // matches that so a password containing `?` or `#` (which
        // can't appear raw — they start the query/fragment) can still
        // be expressed by the caller via percent-encoding.
        let d = Dsn::parse("postgres://al%69ce:p%40ss%3Fw0rd@h:5432/d").unwrap();
        assert_eq!(d.user.as_deref(), Some("alice"));
        assert_eq!(d.password.as_deref(), Some("p@ss?w0rd"));
    }

    #[test]
    fn percent_decode_is_lenient_about_malformed_escapes() {
        // A stray '%' or a truncated/non-hex escape passes through
        // literally rather than erroring the whole DSN.
        assert_eq!(super::percent_decode("100%"), "100%");
        assert_eq!(super::percent_decode("100%2"), "100%2");
        assert_eq!(super::percent_decode("100%zz"), "100%zz");
        assert_eq!(super::percent_decode("a%20b"), "a b");
    }

    #[test]
    fn percent_decode_never_panics_on_percent_before_multibyte_char() {
        // Regression guard: slicing the hex digits by byte offset
        // instead of comparing raw bytes would panic here, because
        // the second "hex" byte lands mid-codepoint of 'é' (2 bytes).
        let _ = super::percent_decode("%aé");
        let _ = super::percent_decode("%a\u{1F600}");
    }

    #[test]
    fn apply_ssl_mode_maps_url_params_to_tokio_postgres_modes() {
        use tokio_postgres::config::SslMode;
        fn parsed(s: &str) -> (tokio_postgres::Config, bool) {
            let dsn = Dsn::parse(s).unwrap();
            let mut cfg = tokio_postgres::Config::new();
            let verify = super::apply_ssl_mode(&mut cfg, &dsn);
            (cfg, verify)
        }
        // Default — no sslmode in URL — is Prefer, no verify.
        let (cfg, v) = parsed("postgres://h/d");
        assert_eq!(cfg.get_ssl_mode(), SslMode::Prefer);
        assert!(!v);

        let (cfg, v) = parsed("postgres://h/d?sslmode=disable");
        assert_eq!(cfg.get_ssl_mode(), SslMode::Disable);
        assert!(!v);

        let (cfg, v) = parsed("postgres://h/d?sslmode=prefer");
        assert_eq!(cfg.get_ssl_mode(), SslMode::Prefer);
        assert!(!v);

        // `require`: encrypt, no verify (libpq semantics).
        let (cfg, v) = parsed("postgres://h/d?sslmode=require");
        assert_eq!(cfg.get_ssl_mode(), SslMode::Require);
        assert!(!v, "sslmode=require should not verify the server cert");

        // verify-* upgrades to Require AND turns verification on.
        let (cfg, v) = parsed("postgres://h/d?sslmode=verify-full");
        assert_eq!(cfg.get_ssl_mode(), SslMode::Require);
        assert!(v);

        let (_, v) = parsed("postgres://h/d?sslmode=verify-ca");
        assert!(v);

        // `allow`: same wire outcome as `prefer` (see apply_ssl_mode's
        // doc comment for why pgman can't preserve libpq's
        // plaintext-first negotiation order).
        let (cfg, v) = parsed("postgres://h/d?sslmode=allow");
        assert_eq!(cfg.get_ssl_mode(), SslMode::Prefer);
        assert!(!v);
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
        let only = StubErr {
            msg: "boom",
            src: None,
        };
        assert_eq!(chain_message(&only), "boom");
    }

    #[test]
    fn chain_message_avoids_duplicate_tail() {
        // Some libraries bake the cause into Display already; don't end up
        // with "io error: ECONNREFUSED: ECONNREFUSED".
        let inner = StubErr {
            msg: "ECONNREFUSED",
            src: None,
        };
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
            ssh_tunnel: None,
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
        assert!(h.contains("pass user:pass in --dsn"), "got: {h}");
    }

    #[test]
    fn connect_hint_does_not_suggest_supplying_a_password_the_dsn_already_carried() {
        let mut d = dsn_for("h", "x", Some("alice"));
        d.password = Some("hunter2".into());
        let h = connect_hint("password authentication failed for user \"alice\"", &d).unwrap();
        assert!(h.contains("wrong password for user \"alice\""), "got: {h}");
        assert!(h.contains("supplied one"), "got: {h}");
        assert!(!h.contains("pass user:pass"), "got: {h}");
        assert!(!h.contains("hunter2"), "the value must never surface: {h}");
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

    #[test]
    fn connect_hint_recognises_ssh_tunnel_failure() {
        let mut d = dsn_for("db.internal", "app", None);
        d.ssh_tunnel = Some(crate::tunnel::SshTunnelSpec {
            user: Some("tom".into()),
            host: "bastion".into(),
            port: None,
        });
        let h = connect_hint(
            "ssh exited before the tunnel was ready (status 255): permission denied",
            &d,
        )
        .expect("ssh tunnel failure should map to a hint");
        assert!(
            h.contains("tom@bastion"),
            "hint should name the target: {h}"
        );
        assert!(
            h.contains("ssh -v"),
            "hint should suggest manual verify: {h}"
        );
    }

    #[test]
    fn application_name_defaults_to_pgman_and_its_version() {
        let d = Dsn::parse("postgres://h/db").unwrap();
        assert_eq!(
            application_name(&d),
            format!("pgman/{}", env!("CARGO_PKG_VERSION"))
        );
        // An empty value is no value.
        let d = Dsn::parse("postgres://h/db?application_name=").unwrap();
        assert!(application_name(&d).starts_with("pgman/"));
    }

    #[test]
    fn application_name_honours_the_dsn_parameter() {
        let d = Dsn::parse("postgres://h/db?application_name=svc@box").unwrap();
        assert_eq!(application_name(&d), "svc@box");
        let d = Dsn::parse("postgres://h/db?Application_Name=svc").unwrap();
        assert_eq!(application_name(&d), "svc");
    }

    #[test]
    fn read_only_refusal_hint_names_the_file_when_there_is_one() {
        let detail = QueryErrDetail {
            code: Some("25006".to_string()),
            ..Default::default()
        };
        let h = read_only_refusal_hint(
            Some(&detail),
            "cannot execute UPDATE in a read-only transaction",
            true,
        )
        .expect("25006 should map to a hint");
        let path = crate::util::config_file("safety.toml")
            .display()
            .to_string();
        assert!(h.contains(&path), "should name the file's path: {h}");
        assert!(h.contains("read_only"), "got: {h}");
        assert!(h.contains("docs/configuration.md"), "got: {h}");
        assert!(!h.contains("--init-config"), "got: {h}");
    }

    #[test]
    fn read_only_refusal_hint_says_how_to_get_a_file_when_there_is_none() {
        // A default profile has no safety.toml on disk: pointing at a
        // path that does not exist sent the operator looking for it.
        let detail = QueryErrDetail {
            code: Some("25006".to_string()),
            ..Default::default()
        };
        let h = read_only_refusal_hint(
            Some(&detail),
            "cannot execute UPDATE in a read-only transaction",
            false,
        )
        .expect("25006 should map to a hint");
        assert_eq!(h, READ_ONLY_DEFAULT_HINT);
        assert!(h.contains("--init-config"), "got: {h}");
        assert!(h.contains("read_only = false"), "got: {h}");
        let path = crate::util::config_file("safety.toml")
            .display()
            .to_string();
        assert!(
            !h.contains(&path),
            "must not name a file that is not there: {h}"
        );
    }

    #[test]
    fn read_only_refusal_hint_falls_back_to_message_text_without_detail() {
        // `simple_query`'s error path doesn't always populate `detail`;
        // the message text alone must still be recognised — on both
        // branches.
        let msg = "cannot execute INSERT in a read-only transaction";
        let h = read_only_refusal_hint(None, msg, true)
            .expect("message text alone should map to a hint");
        assert!(h.contains("safety.toml ("), "got: {h}");
        let h = read_only_refusal_hint(None, msg, false)
            .expect("message text alone should map to a hint");
        assert_eq!(h, READ_ONLY_DEFAULT_HINT);
    }

    #[test]
    fn read_only_refusal_hint_none_for_unrelated_errors() {
        let detail = QueryErrDetail {
            code: Some("42601".to_string()), // syntax_error
            ..Default::default()
        };
        for exists in [true, false] {
            assert!(read_only_refusal_hint(
                Some(&detail),
                "syntax error at or near \"FROM\"",
                exists
            )
            .is_none());
            assert!(
                read_only_refusal_hint(None, "relation \"t\" does not exist", exists).is_none()
            );
        }
    }
}
