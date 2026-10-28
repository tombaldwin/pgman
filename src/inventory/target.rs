//! Naming one database on the command line — what `pgman dump`,
//! `restore` and `run` take as `<target>` — and finding it.
//!
//! A target is, in the order tried:
//!
//! 1. `ENV`, `ENV/DATABASE` or `ENV/SERVER/DATABASE`: a database of an
//!    environment in `environments.toml`. Its protection is the
//!    environment's ([`super::patching::is_protected`]).
//! 2. A connection in the personal `connections.toml`, by name. It is
//!    protected when an environment that lists it is, or when its own
//!    name contains "prod" — the rule an environment without
//!    `protected` follows.
//! 3. A `postgres://` URL, protected by that same name rule applied to
//!    its database.
//!
//! Which of an environment's rows answers depends on what the caller
//! wants ([`Want`]): a dump prefers a replica, so a long read never
//! weighs on the primary; anything that writes takes the primary.
//! [`pick`] makes that choice and is pure; [`resolve`] reads the files
//! (and, for an `rds` source, the aws CLI).

use super::{ConnectSpec, Environment, Source, Target};
use crate::conn::Dsn;
use crate::mcp::connections::Role;

/// What the target is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// Reading: a replica when the environment has one, the primary
    /// otherwise.
    PreferReplica,
    /// The primary (or a plain server), never a replica: writing, or a
    /// read the operator asked to take from the primary.
    Primary,
}

/// One database, found and ready to connect to.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// How the operator named it, for messages: `staging/ledger`.
    pub label: String,
    /// The environment it belongs to, if any.
    pub environment: Option<String>,
    pub server: String,
    pub database: String,
    pub role: Role,
    /// For a replica, the server it replicates.
    pub replica_of: Option<String>,
    pub protected: bool,
    /// Why it is protected, for a refusal to say: `the environment
    /// "live" says protected = true`, `it is on db.internal:5432, a
    /// server the protected environment "live" reaches`.
    pub protected_why: Option<String>,
    pub connect: ConnectSpec,
}

impl Resolved {
    /// `ledger on demo-replica (replica of demo)`, for saying what was
    /// used. Pure.
    pub fn describe(&self) -> String {
        let role = match (self.role, &self.replica_of) {
            (Role::Replica, Some(p)) => format!(" (replica of {p})"),
            (Role::Replica, None) => " (replica)".to_string(),
            (Role::Primary, _) => " (primary)".to_string(),
            (Role::Direct, _) => String::new(),
        };
        format!("{} on {}{role}", self.database, self.server)
    }

    /// Whether the server is this machine: a loopback address, the
    /// `localhost` name, or a Unix socket directory — and not reached
    /// through an ssh tunnel, which makes even `localhost` someone
    /// else's. Pure.
    pub fn is_local(&self) -> bool {
        let dsn = &self.connect.dsn;
        dsn.ssh_tunnel.is_none() && is_local_host(&dsn.host)
    }
}

/// Whether `host` names this machine: `localhost`, a loopback address,
/// or a Unix socket directory. Pure.
pub fn is_local_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
        || h.starts_with('/')
}

/// `ENV`, `ENV/DATABASE`, `ENV/SERVER/DATABASE`, split. Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub first: String,
    pub server: Option<String>,
    pub database: Option<String>,
}

/// Split a target that isn't a URL. Empty parts are refused. Pure.
pub fn parse_spec(text: &str) -> Result<Spec, String> {
    let parts: Vec<&str> = text.split('/').collect();
    if parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "target {text:?} has an empty part; use ENV, ENV/DATABASE or ENV/SERVER/DATABASE"
        ));
    }
    let owned = |s: &str| s.to_string();
    match parts.as_slice() {
        [env] => Ok(Spec {
            first: owned(env),
            server: None,
            database: None,
        }),
        [env, db] => Ok(Spec {
            first: owned(env),
            server: None,
            database: Some(owned(db)),
        }),
        [env, server, db] => Ok(Spec {
            first: owned(env),
            server: Some(owned(server)),
            database: Some(owned(db)),
        }),
        _ => Err(format!(
            "target {text:?} has too many parts; use ENV, ENV/DATABASE or ENV/SERVER/DATABASE"
        )),
    }
}

/// The row of `targets` (one environment's) that `spec` names, for
/// `want`. Pure.
///
/// - No database: the environment must have just one.
/// - A server: that server's row, whatever its role — except that a
///   replica is refused for [`Want::Primary`].
/// - Otherwise, for [`Want::PreferReplica`] the first replica of the
///   database that isn't stopped, else its primary; for
///   [`Want::Primary`] its primary (or plain server), of which there
///   must be one.
pub fn pick(env: &str, spec: &Spec, targets: &[Target], want: Want) -> Result<Target, String> {
    let mut dbs: Vec<&str> = Vec::new();
    for t in targets {
        if !dbs.contains(&t.database.as_str()) {
            dbs.push(&t.database);
        }
    }
    let database = match &spec.database {
        Some(d) => d.clone(),
        None => match dbs.as_slice() {
            [one] => one.to_string(),
            [] => return Err(format!("{env} lists no databases")),
            many => {
                return Err(format!(
                    "{env} has {} databases; name one: {}",
                    many.len(),
                    many.iter()
                        .map(|d| format!("{env}/{d}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        },
    };
    let rows: Vec<&Target> = targets.iter().filter(|t| t.database == database).collect();
    if rows.is_empty() {
        return Err(format!(
            "{env} has no database {database:?}; it has {}",
            dbs.join(", ")
        ));
    }
    if let Some(server) = &spec.server {
        let t = rows.iter().find(|t| &t.server == server).ok_or_else(|| {
            format!(
                "{env} has no server {server:?} with {database}; it has {}",
                rows.iter()
                    .map(|t| t.server.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        if want == Want::Primary && t.role == Role::Replica {
            return Err(format!(
                "{} is a replica, where nothing can be written; name its primary{}",
                t.server,
                t.replica_of
                    .as_deref()
                    .map(|p| format!(" ({env}/{p}/{database})"))
                    .unwrap_or_default()
            ));
        }
        return Ok((*t).clone());
    }
    let stopped = |t: &Target| {
        t.status
            .as_deref()
            .is_some_and(|s| s != "available" && s != "running")
    };
    if want == Want::PreferReplica {
        if let Some(r) = rows.iter().find(|t| t.role == Role::Replica && !stopped(t)) {
            return Ok((*r).clone());
        }
    }
    let primaries: Vec<&&Target> = rows.iter().filter(|t| t.role != Role::Replica).collect();
    match primaries.as_slice() {
        [one] => Ok((**one).clone()),
        [] => Err(format!(
            "{env}/{database} lists only replicas ({}); name the primary's server, or add it to the environment",
            rows.iter()
                .map(|t| t.server.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        many => Err(format!(
            "{env}/{database} is on {} servers ({}); name one: {env}/SERVER/{database}",
            many.len(),
            many.iter()
                .map(|t| t.server.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The name rule for something no environment speaks for: "prod" in
/// it. Pure.
pub fn named_like_prod(name: &str) -> bool {
    name.to_ascii_lowercase().contains("prod")
}

/// Why `env` is protected, if it is. Pure.
fn env_protection(env: &Environment) -> Option<String> {
    if !super::patching::is_protected(env) {
        return None;
    }
    Some(if env.protected == Some(true) {
        format!("the environment {:?} says protected = true", env.name)
    } else {
        format!("the environment {:?} has \"prod\" in its name", env.name)
    })
}

/// Why `name` (a database or host) is protected by the name rule. Pure.
fn name_protection(what: &str, name: &str) -> Option<String> {
    named_like_prod(name).then(|| format!("its {what} {name:?} has \"prod\" in it"))
}

/// Why connection `name` (to `host`) is protected, if it is: an
/// environment that lists it is, or its name or its host's says prod —
/// as a URL to that host would be. Pure.
pub fn connection_protection(name: &str, host: &str, envs: &[Environment]) -> Option<String> {
    envs.iter()
        .filter(|e| matches!(&e.source, Source::Static { connections } if connections.iter().any(|c| c == name)))
        .find_map(env_protection)
        .or_else(|| name_protection("name", name))
        .or_else(|| name_protection("host", host))
}

/// [`connection_protection`], as a yes or no. Pure.
pub fn connection_protected(name: &str, host: &str, envs: &[Environment]) -> bool {
    connection_protection(name, host, envs).is_some()
}

impl Resolved {
    /// Why it is protected, for a refusal. Pure.
    pub fn protection(&self) -> String {
        self.protected_why
            .clone()
            .unwrap_or_else(|| "it is protected".to_string())
    }
}

/// `target` of `env`, resolved. Pure.
fn from_target(label: String, env: &Environment, target: Target) -> Result<Resolved, String> {
    let connect = target
        .connect
        .clone()
        .map_err(|e| format!("{label}: {e}"))?;
    Ok(Resolved {
        label,
        environment: Some(env.name.clone()),
        server: target.server,
        database: target.database,
        role: target.role,
        replica_of: target.replica_of,
        protected: super::patching::is_protected(env),
        protected_why: env_protection(env),
        connect,
    })
}

/// A `postgres://` URL as a target. Pure.
pub fn from_url(url: &str) -> Result<Resolved, String> {
    // The URL as typed may carry a password: never echoed.
    let dsn =
        Dsn::parse(url).map_err(|e| format!("target {:?}: {e}", crate::conn::redact_url(url)))?;
    Ok(Resolved {
        label: dsn.dbname.clone(),
        environment: None,
        server: crate::conn::host_port(&dsn.host, dsn.port),
        database: dsn.dbname.clone(),
        role: Role::Direct,
        replica_of: None,
        protected: named_like_prod(&dsn.dbname) || named_like_prod(&dsn.host),
        protected_why: name_protection("database", &dsn.dbname)
            .or_else(|| name_protection("host", &dsn.host)),
        connect: ConnectSpec {
            dsn,
            secret: None,
            password_env: None,
        },
    })
}

/// Find `text` (see the module docs) for `want`. Blocking: an `rds`
/// environment lists its instances through the aws CLI.
pub fn resolve(text: &str, want: Want) -> Result<Resolved, String> {
    let envs = super::load().environments.unwrap_or_default();
    let personal = {
        let path = crate::util::config_file("connections.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => crate::mcp::connections::parse_personal(&text),
            Err(_) => crate::mcp::connections::Personal::default(),
        }
    };
    let entries = crate::mcp::connections::build(&personal.connections, Vec::new(), false);
    let mut resolved = resolve_in(text, want, &envs, &entries)?;
    // A connection's password_env (named directly, or a static
    // environment's row), named again so the connect reads it then — and
    // says which variable, when it is unset.
    let from_connection = match &resolved.environment {
        None => true,
        Some(env) => envs
            .iter()
            .any(|e| &e.name == env && matches!(e.source, Source::Static { .. })),
    };
    if from_connection {
        if let Some(pc) = personal
            .connections
            .iter()
            .find(|c| c.base.name == resolved.server)
        {
            resolved.connect.password_env = pc.base.password_env.clone().filter(|v| !v.is_empty());
        }
    }
    // However it was named: a URL, or a second connection, to a server
    // a protected environment reaches is that environment's.
    let dsn = &resolved.connect.dsn;
    if let Some(env) = protecting_env(&dsn.host, dsn.port, &envs, &entries) {
        if !resolved.protected {
            resolved.protected_why = Some(format!(
                "it is on {}, a server the protected environment {env:?} reaches",
                crate::conn::host_port(&dsn.host, dsn.port)
            ));
        }
        resolved.protected = true;
    }
    Ok(resolved)
}

/// `host` as a key for comparing servers: lower case, without IPv6
/// brackets, every spelling of this machine's loopback one. Pure.
fn host_key(host: &str) -> String {
    let h = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    match h.parse::<std::net::IpAddr>() {
        Ok(ip) => ip_key(ip),
        Err(_) if h == "localhost" => "localhost".to_string(),
        Err(_) => h,
    }
}

/// An address as a key: an IPv4-mapped IPv6 one as its IPv4, and every
/// address that reaches this machine (loopback, or the unspecified
/// `0.0.0.0` / `::`) as `localhost`. Pure.
fn ip_key(ip: std::net::IpAddr) -> String {
    let ip = match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map(std::net::IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    if ip.is_loopback() || ip.is_unspecified() {
        "localhost".to_string()
    } else {
        ip.to_string()
    }
}

/// Every key `host` is known by: its own ([`host_key`]) and each address
/// it resolves to — so `127.1`, `0x7f000001`, `localhost.` and a
/// hostname and its IP all compare equal, however written. Blocking (the
/// resolver); a name that doesn't resolve keeps its own key alone.
fn server_keys(host: &str, port: u16) -> Vec<String> {
    use std::net::ToSocketAddrs;
    let own = host_key(host);
    let mut keys = vec![own.clone()];
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(addrs) = (bare, port).to_socket_addrs() {
        for a in addrs {
            let k = ip_key(a.ip());
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    keys
}

/// Whether `host:port` is a server a protected environment reaches: the
/// address of a connection a protected `static` environment lists, or
/// an RDS endpoint (`<instance>.<…>.rds.amazonaws.com`) whose instance a
/// protected `rds` environment's `instances` match. Blocking: names are
/// resolved, so a server is matched however it is written.
pub fn host_protected(
    host: &str,
    port: u16,
    envs: &[Environment],
    entries: &[crate::mcp::connections::Entry],
) -> bool {
    protecting_env(host, port, envs, entries).is_some()
}

/// [`host_protected`]: the protected environment that reaches
/// `host:port`, by name. Blocking, as it is.
pub fn protecting_env(
    host: &str,
    port: u16,
    envs: &[Environment],
    entries: &[crate::mcp::connections::Entry],
) -> Option<String> {
    let key = host_key(host);
    let label = key.split('.').next().unwrap_or("");
    let rds = key.ends_with(".rds.amazonaws.com");
    let mine = server_keys(host, port);
    envs.iter()
        .filter(|e| super::patching::is_protected(e))
        .find(|e| match &e.source {
            Source::Static { connections } => connections.iter().any(|name| {
                entries
                    .iter()
                    .filter(|en| &en.name == name)
                    .filter_map(|en| en.dsn.as_ref())
                    .any(|d| {
                        d.port == port
                            && server_keys(&d.host, d.port)
                                .iter()
                                .any(|k| mine.contains(k))
                    })
            }),
            Source::Rds { instances, .. } => {
                rds && instances
                    .iter()
                    .any(|p| super::rds::glob_match(&p.to_ascii_lowercase(), label))
            }
            Source::Docker { .. } => false,
        })
        .map(|e| e.name.clone())
}

/// [`resolve`] before the server check. Blocking, as it is.
fn resolve_in(
    text: &str,
    want: Want,
    envs: &[Environment],
    entries: &[crate::mcp::connections::Entry],
) -> Result<Resolved, String> {
    if text.contains("://") {
        return from_url(text);
    }
    let spec = parse_spec(text)?;
    if let Some(env) = envs.iter().find(|e| e.name == spec.first) {
        let report = super::gather(
            std::slice::from_ref(env),
            entries,
            super::fetch_rds_listing,
            super::docker::fetch,
        )
        .pop()
        .ok_or("no report")?;
        if let Some(e) = report.error.filter(|_| report.targets.is_empty()) {
            return Err(format!("{}: {e}", env.name));
        }
        let target = pick(&env.name, &spec, &report.targets, want)?;
        return from_target(text.to_string(), env, target);
    }
    if spec.server.is_none() && spec.database.is_none() {
        if let Some(e) = entries.iter().find(|e| e.name == spec.first) {
            let dsn = e.dsn.clone().ok_or_else(|| {
                format!(
                    "connection {:?} is not usable: it {}",
                    e.name,
                    e.refusal.as_deref().unwrap_or("has no DSN")
                )
            })?;
            if want == Want::Primary && e.role == Role::Replica {
                return Err(format!(
                    "connection {:?} is a replica (role = \"replica\"); name its primary",
                    e.name
                ));
            }
            return Ok(Resolved {
                label: e.name.clone(),
                environment: None,
                server: e.name.clone(),
                database: dsn.dbname.clone(),
                role: e.role,
                replica_of: None,
                protected: connection_protected(&e.name, &dsn.host, envs),
                protected_why: connection_protection(&e.name, &dsn.host, envs),
                connect: ConnectSpec {
                    dsn,
                    secret: e.secret.clone(),
                    password_env: None,
                },
            });
        }
    }
    Err(format!(
        "no environment or connection named {:?} (environments.toml, connections.toml); \
         a postgres:// URL works too",
        spec.first
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(server: &str, db: &str, role: Role, status: Option<&str>) -> Target {
        Target {
            database: db.to_string(),
            server: server.to_string(),
            role,
            replica_of: (role == Role::Replica).then(|| "ledger-db".to_string()),
            host: Some(format!("{server}.example")),
            port: Some(5432),
            engine: None,
            status: status.map(str::to_string),
            class: None,
            multi_az: None,
            connect: Err("not needed".to_string()),
        }
    }

    fn staging() -> Vec<Target> {
        vec![
            row("ledger-db", "ledger", Role::Primary, Some("available")),
            row(
                "ledger-db-replica",
                "ledger",
                Role::Replica,
                Some("available"),
            ),
            row("ledger-db", "ledgerlog", Role::Primary, Some("available")),
            row(
                "ledger-db-replica",
                "ledgerlog",
                Role::Replica,
                Some("available"),
            ),
        ]
    }

    fn spec(s: &str) -> Spec {
        parse_spec(s).unwrap()
    }

    #[test]
    fn a_spec_has_one_to_three_parts() {
        assert_eq!(
            spec("staging"),
            Spec {
                first: "staging".into(),
                server: None,
                database: None
            }
        );
        assert_eq!(spec("staging/ledger").database.as_deref(), Some("ledger"));
        let full = spec("staging/ledger-db/ledger");
        assert_eq!(full.server.as_deref(), Some("ledger-db"));
        assert_eq!(full.database.as_deref(), Some("ledger"));
        assert!(parse_spec("a/b/c/d").unwrap_err().contains("too many"));
        assert!(parse_spec("staging/").unwrap_err().contains("empty"));
        assert!(parse_spec("").unwrap_err().contains("empty"));
    }

    #[test]
    fn a_read_prefers_the_replica_and_a_write_takes_the_primary() {
        let t = pick(
            "staging",
            &spec("staging/ledger"),
            &staging(),
            Want::PreferReplica,
        )
        .unwrap();
        assert_eq!(t.server, "ledger-db-replica");
        let t = pick(
            "staging",
            &spec("staging/ledger"),
            &staging(),
            Want::Primary,
        )
        .unwrap();
        assert_eq!(t.server, "ledger-db");
        assert_eq!(t.role, Role::Primary);
    }

    #[test]
    fn a_stopped_replica_is_passed_over_for_the_primary() {
        let mut rows = staging();
        rows[1].status = Some("stopped".into());
        let t = pick(
            "staging",
            &spec("staging/ledger"),
            &rows,
            Want::PreferReplica,
        )
        .unwrap();
        assert_eq!(t.server, "ledger-db");
    }

    #[test]
    fn several_databases_need_one_named() {
        let e = pick("staging", &spec("staging"), &staging(), Want::PreferReplica).unwrap_err();
        assert!(
            e.contains("staging/ledger") && e.contains("staging/ledgerlog"),
            "{e}"
        );
        let one = vec![row("local", "ledger", Role::Direct, None)];
        let t = pick("dev", &spec("dev"), &one, Want::Primary).unwrap();
        assert_eq!(t.database, "ledger");
    }

    #[test]
    fn an_unknown_database_or_server_says_what_there_is() {
        let e = pick(
            "staging",
            &spec("staging/nope"),
            &staging(),
            Want::PreferReplica,
        )
        .unwrap_err();
        assert!(e.contains("ledger, ledgerlog"), "{e}");
        let e = pick(
            "staging",
            &spec("staging/other/ledger"),
            &staging(),
            Want::PreferReplica,
        )
        .unwrap_err();
        assert!(e.contains("ledger-db, ledger-db-replica"), "{e}");
    }

    #[test]
    fn a_named_replica_serves_a_read_but_never_a_write() {
        let s = spec("staging/ledger-db-replica/ledger");
        assert_eq!(
            pick("staging", &s, &staging(), Want::PreferReplica)
                .unwrap()
                .server,
            "ledger-db-replica"
        );
        let e = pick("staging", &s, &staging(), Want::Primary).unwrap_err();
        assert!(e.contains("is a replica, where nothing can be written; name its primary (staging/ledger-db/ledger)"), "{e}");
    }

    #[test]
    fn only_replicas_is_no_primary() {
        let rows = vec![row("r", "ledger", Role::Replica, Some("available"))];
        let e = pick("staging", &spec("staging/ledger"), &rows, Want::Primary).unwrap_err();
        assert!(e.contains("only replicas"), "{e}");
    }

    #[test]
    fn local_means_this_machine_and_no_tunnel() {
        for h in [
            "localhost",
            "127.0.0.1",
            "::1",
            "[::1]",
            "/var/run/postgresql",
        ] {
            assert!(is_local_host(h), "{h}");
        }
        for h in ["db.example", "10.0.1.23", "localhost.example"] {
            assert!(!is_local_host(h), "{h}");
        }
        let mut r = from_url("postgres://u@127.0.0.1:55432/ledger").unwrap();
        assert!(r.is_local());
        r.connect.dsn.ssh_tunnel = Some(crate::tunnel::SshTunnelSpec::parse("jump").unwrap());
        assert!(!r.is_local(), "a tunnel's localhost is the far side");
    }

    #[test]
    fn a_url_or_connection_named_like_prod_is_protected() {
        assert!(
            from_url("postgres://u@localhost/ledger_prod")
                .unwrap()
                .protected
        );
        assert!(!from_url("postgres://u@localhost/ledger").unwrap().protected);
        let envs = crate::inventory::parse_config(
            "[[environments]]\nname = \"live\"\nprotected = true\nsource = { kind = \"static\", connections = [\"main-db\"] }\n\
             [[environments]]\nname = \"scratch\"\nsource = { kind = \"static\", connections = [\"dev-db\"] }\n",
        )
        .unwrap();
        assert!(
            connection_protected("main-db", "db.example", &envs),
            "its environment is"
        );
        assert!(!connection_protected("dev-db", "db.example", &envs));
        assert!(
            connection_protected("prod-replica", "db.example", &envs),
            "the name rule"
        );
        assert!(
            connection_protected("main", "ledger-prod.example", &envs),
            "its host's name, as for a URL"
        );
    }

    #[test]
    fn a_server_a_protected_environment_reaches_is_protected_however_named() {
        let envs = crate::inventory::parse_config(
            "[[environments]]\nname = \"live\"\nprotected = true\nsource = { kind = \"static\", connections = [\"main-db\"] }\n\
             [[environments]]\nname = \"edge\"\nprotected = true\nsource = { kind = \"rds\", instances = [\"ledger-*\"], region = \"us-west-1\" }\n\
             [[environments]]\nname = \"scratch\"\nsource = { kind = \"static\", connections = [\"dev-db\"] }\n",
        )
        .unwrap();
        let conn = |name: &str, url: &str| crate::mcp::connections::PersonalConnection {
            base: crate::project::Connection {
                name: name.to_string(),
                url: url.to_string(),
                user: None,
                password_env: None,
                ssh_tunnel: None,
            },
            secret: None,
            aws_region: None,
            aws_account: None,
            aws_profile: None,
            role: None,
        };
        let entries = crate::mcp::connections::build(
            &[
                conn("main-db", "postgres://u@db.internal:5432/ledger"),
                conn("dev-db", "postgres://u@127.0.0.1:55432/ledger"),
            ],
            Vec::new(),
            false,
        );
        assert!(host_protected("db.internal", 5432, &envs, &entries));
        assert!(host_protected("DB.INTERNAL", 5432, &envs, &entries), "case");
        assert!(
            !host_protected("db.internal", 5433, &envs, &entries),
            "another port"
        );
        assert!(
            !host_protected("127.0.0.1", 55432, &envs, &entries),
            "an unprotected one's"
        );
        assert!(host_protected(
            "ledger-main.abc123.us-west-1.rds.amazonaws.com",
            5432,
            &envs,
            &entries
        ));
        assert!(!host_protected(
            "other.abc123.us-west-1.rds.amazonaws.com",
            5432,
            &envs,
            &entries
        ));
        assert_eq!(host_key("[::1]"), "localhost");
        assert_eq!(host_key("127.0.0.1"), "localhost");
        // Spellings of this machine the resolver takes and a parser doesn't.
        let entries = crate::mcp::connections::build(
            &[conn("main-db", "postgres://u@127.0.0.1:55432/ledger")],
            Vec::new(),
            false,
        );
        let envs = crate::inventory::parse_config(
            "[[environments]]\nname = \"live\"\nprotected = true\nsource = { kind = \"static\", connections = [\"main-db\"] }\n",
        )
        .unwrap();
        for spelling in [
            "localhost.",
            "127.1",
            "0x7f000001",
            "0.0.0.0",
            "::ffff:127.0.0.1",
            "[::1]",
            "LOCALHOST",
        ] {
            assert!(
                host_protected(spelling, 55432, &envs, &entries),
                "{spelling}"
            );
        }
        assert!(
            !host_protected("127.0.0.1", 55433, &envs, &entries),
            "another port"
        );
        assert_eq!(host_key("db.internal."), "db.internal");
    }

    #[test]
    fn a_protected_target_says_why() {
        let envs = crate::inventory::parse_config(
            "[[environments]]\nname = \"live\"\nprotected = true\nsource = { kind = \"static\", connections = [\"main-db\"] }\n\
             [[environments]]\nname = \"prod-eu\"\nsource = { kind = \"static\", connections = [\"eu-db\"] }\n",
        )
        .unwrap();
        assert_eq!(
            connection_protection("main-db", "db.internal", &envs).as_deref(),
            Some("the environment \"live\" says protected = true")
        );
        assert_eq!(
            connection_protection("eu-db", "db.internal", &envs).as_deref(),
            Some("the environment \"prod-eu\" has \"prod\" in its name")
        );
        assert_eq!(
            connection_protection("other", "ledger-prod.example", &envs).as_deref(),
            Some("its host \"ledger-prod.example\" has \"prod\" in it")
        );
        assert_eq!(connection_protection("other", "db.example", &envs), None);
        let r = from_url("postgres://u@localhost/ledger_prod").unwrap();
        assert_eq!(
            r.protection(),
            "its database \"ledger_prod\" has \"prod\" in it"
        );
    }

    #[test]
    fn a_bad_urls_password_isnt_echoed() {
        let e = from_url("postgres://u:s3cretpw@db.example:notaport/ledger").unwrap_err();
        assert!(!e.contains("s3cretpw"), "{e}");
    }

    #[test]
    fn describe_says_which_server_and_its_role() {
        let mut r = from_url("postgres://u@localhost/ledger").unwrap();
        assert_eq!(r.describe(), "ledger on localhost:5432");
        r.role = Role::Replica;
        r.replica_of = Some("ledger-db".into());
        r.server = "ledger-db-replica".into();
        assert_eq!(
            r.describe(),
            "ledger on ledger-db-replica (replica of ledger-db)"
        );
    }
}
