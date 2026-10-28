//! Environments: named groups of databases, discovered from pluggable
//! sources, for an overview in the spirit of ebman's main screen — every
//! database in an environment, replicas included, side by side.
//!
//! Step 1 shows what the sources know without connecting to anything:
//! role, server, endpoint, engine version and status. Schema version,
//! pending patches and replica lag need a connection per database and
//! come later (BACKLOG.md, "Environments overview").
//!
//! Sources:
//! - `static`: names from the personal `connections.toml` — local
//!   databases and any hand-listed group of remote ones.
//! - `rds`: `aws rds describe-db-instances`, filtered by identifier glob;
//!   replicas of a matched instance come along.
//! - `docker`: local Postgres containers from `docker ps`, reached on
//!   their published port.
//!
//! Environments live in `environments.toml` under `util::config_dir()`,
//! never a project file: like `connections.toml`, they name hosts and
//! accounts the operator chose.

pub mod discover;
pub mod docker;
pub mod patching;
pub mod probe;
pub mod rds;
pub mod rds_logs;
pub mod target;

use crate::aws_cli::AwsScope;
use crate::conn::Dsn;
use crate::creds::aws_secrets::SecretRef;
use crate::mcp::connections::{Entry, Role};
use std::collections::HashMap;

/// `environments.toml`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentsFile {
    #[serde(default)]
    pub environments: Vec<Environment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub name: String,
    pub source: Source,
    /// Databases to list on each discovered server. Empty means the one
    /// the source names (an RDS instance's `DBName`).
    #[serde(default)]
    pub databases: Vec<String>,
    /// `[user@]host[:port]` of the ssh bastion this environment's
    /// databases are reached through.
    #[serde(default)]
    pub ssh_tunnel: Option<String>,
    /// `sslmode` for every database in the environment.
    #[serde(default)]
    pub sslmode: Option<String>,
    /// User name when no secret supplies one.
    #[serde(default)]
    pub user: Option<String>,
    /// Environment variable holding the password, for databases without
    /// a secret. Read at connect, never stored.
    #[serde(default)]
    pub password_env: Option<String>,
    /// AWS Secrets Manager secret holding the credentials, keyed by
    /// server (`"demo"`) or, more specifically, server and database
    /// (`"demo/ledger"`). Read with the source's region, profile and
    /// account, and only when connecting.
    #[serde(default)]
    pub secrets: std::collections::BTreeMap<String, String>,
    /// Connect to each database on every refresh to read its version,
    /// recovery state and lag (and, with `version_table`, its patch
    /// level). Off unless set: a refresh then touches every database.
    #[serde(default)]
    pub probe: bool,
    /// Where the patch level lives, for probes. Without one, a probe
    /// recognises the common migration tools' own tables.
    #[serde(default)]
    pub version_table: Option<VersionTable>,
    /// Flyway's migration directories (`V<version>__<name>.sql`), for a
    /// pending count: one, or several comma-separated the way
    /// `flyway.locations` lists them, `filesystem:` prefix allowed.
    /// Each is read recursively; `~/` is your home.
    #[serde(default)]
    pub migrations_dir: Option<String>,
    /// Applying patches asks for the environment's name to be typed.
    /// Unset, an environment whose name contains "prod" is protected.
    #[serde(default)]
    pub protected: Option<bool>,
}

/// `version_table = { table = "public.db_version", column = "id", patches = "~/src/app/db/patches" }`
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionTable {
    pub table: String,
    pub column: String,
    /// Directory of patch files to count pending ones against; `~/` is
    /// your home. Absolute otherwise.
    #[serde(default)]
    pub patches: Option<String>,
    /// The databases that have the table; empty means every one. A
    /// database left out shows no patch level rather than an error.
    #[serde(default)]
    pub databases: Vec<String>,
}

impl VersionTable {
    /// Whether `database` keeps this version table.
    pub fn applies_to(&self, database: &str) -> bool {
        self.databases.is_empty() || self.databases.iter().any(|d| d == database)
    }

    pub fn config(&self) -> crate::query::patches::PatchesConfig {
        crate::query::patches::PatchesConfig {
            dir: self.patches.clone().unwrap_or_default(),
            table: self.table.clone(),
            column: self.column.clone(),
        }
    }

    /// Whether `patches` is shaped right: `~/…` or absolute. Pure, so
    /// parsing the file reads nothing from the environment.
    fn check_patches(&self) -> Result<(), String> {
        match &self.patches {
            Some(raw) if !raw.starts_with("~/") && !std::path::Path::new(raw).is_absolute() => Err(
                format!("version_table.patches {raw:?} must be absolute or start with ~/"),
            ),
            _ => Ok(()),
        }
    }

    /// The patch directory, `~/` expanded; it must end up absolute.
    pub fn patch_dir(&self) -> Option<Result<std::path::PathBuf, String>> {
        let raw = self.patches.as_ref()?;
        let path = match raw.strip_prefix("~/") {
            Some(rest) => match std::env::var_os("HOME") {
                Some(home) => std::path::PathBuf::from(home).join(rest),
                None => return Some(Err("version_table.patches uses ~/ but HOME is unset".into())),
            },
            None => std::path::PathBuf::from(raw),
        };
        Some(if path.is_absolute() {
            Ok(path)
        } else {
            Err(format!(
                "version_table.patches {raw:?} must be absolute or start with ~/"
            ))
        })
    }
}

/// What a probe of an environment needs beyond its rows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbeSettings {
    pub version_table: Option<VersionTable>,
    pub migrations_dir: Option<String>,
}

impl ProbeSettings {
    /// `migrations_dir`'s directories with `~/` expanded; each must end
    /// up absolute.
    pub fn migrations_dirs(&self) -> Option<Result<Vec<std::path::PathBuf>, String>> {
        let raw = self.migrations_dir.as_ref()?;
        Some(migration_locations(raw).and_then(|locations| {
            locations
                .iter()
                .map(|l| expand_home_dir(l, "migrations_dir"))
                .collect()
        }))
    }
}

/// `migrations_dir`'s entries: comma-separated as `flyway.locations`
/// lists them, `filesystem:` dropped. A `classpath:` location is
/// refused, since pgman doesn't know which project's classpath; the
/// directory it names is wanted instead. Each must be absolute or start
/// with `~/`. Pure.
pub fn migration_locations(raw: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for entry in raw.split(',').map(str::trim) {
        if entry.is_empty() {
            return Err(format!("migrations_dir {raw:?} has an empty entry"));
        }
        if let Some(rest) = entry.strip_prefix("classpath:") {
            return Err(format!(
                "migrations_dir {entry:?}: pgman can't see a classpath; name the \
                 directory instead, e.g. ~/src/app/src/main/resources/{}",
                rest.trim_start_matches('/')
            ));
        }
        let path = entry.strip_prefix("filesystem:").unwrap_or(entry);
        if !path.starts_with("~/") && !std::path::Path::new(path).is_absolute() {
            return Err(format!(
                "migrations_dir {path:?} must be absolute or start with ~/"
            ));
        }
        out.push(path.to_string());
    }
    Ok(out)
}

/// `~/x` against `$HOME`, else the path as written; it must be absolute.
fn expand_home_dir(raw: &str, what: &str) -> Result<std::path::PathBuf, String> {
    let path = match raw.strip_prefix("~/") {
        Some(rest) => std::path::PathBuf::from(
            std::env::var_os("HOME").ok_or(format!("{what} uses ~/ but HOME is unset"))?,
        )
        .join(rest),
        None => std::path::PathBuf::from(raw),
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("{what} {raw:?} must be absolute or start with ~/"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    /// Connections from the personal `connections.toml`, by name.
    Static { connections: Vec<String> },
    /// RDS instances matching any `instances` glob (`*` only), plus their
    /// read replicas.
    Rds {
        instances: Vec<String>,
        #[serde(default)]
        region: Option<String>,
        #[serde(default)]
        profile: Option<String>,
        /// Checked with `sts get-caller-identity` before listing.
        #[serde(default)]
        account: Option<String>,
    },
    /// Local containers running Postgres, from `docker ps`, whose names
    /// match any `containers` glob (default: all of them).
    Docker {
        #[serde(default = "every_container")]
        containers: Vec<String>,
    },
}

fn every_container() -> Vec<String> {
    vec!["*".to_string()]
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::Static { .. } => "connections.toml".to_string(),
            Source::Docker { .. } => "docker".to_string(),
            Source::Rds {
                region, account, ..
            } => {
                let mut s = "RDS".to_string();
                if let Some(a) = account {
                    s.push_str(&format!(" {a}"));
                }
                if let Some(r) = region {
                    s.push_str(&format!(" {r}"));
                }
                s
            }
        }
    }

    /// The AWS scope an `rds` source reads in.
    pub fn rds_scope(&self) -> Option<AwsScope> {
        self.scope()
    }

    fn scope(&self) -> Option<AwsScope> {
        match self {
            Source::Rds {
                region,
                profile,
                account,
                ..
            } => Some(AwsScope {
                region: region.clone(),
                profile: profile.clone(),
                account: account.clone(),
            }),
            Source::Static { .. } | Source::Docker { .. } => None,
        }
    }
}

impl Environment {
    /// The connection settings are for discovered servers; a static
    /// environment's connections carry their own in `connections.toml`.
    fn check_connection_settings(&self) -> Result<(), String> {
        if matches!(self.source, Source::Static { .. })
            && (self.ssh_tunnel.is_some()
                || self.sslmode.is_some()
                || self.user.is_some()
                || self.password_env.is_some()
                || !self.secrets.is_empty())
        {
            return Err(
                "ssh_tunnel, sslmode, user and secrets belong on each connection in \
                 connections.toml for a static environment"
                    .to_string(),
            );
        }
        if matches!(self.source, Source::Docker { .. })
            && (self.ssh_tunnel.is_some() || !self.secrets.is_empty())
        {
            return Err("a docker environment is local: ssh_tunnel and secrets don't apply".into());
        }
        if let Some(v) = &self.password_env {
            let ok = v
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok {
                return Err(format!(
                    "password_env {v:?} is not an environment variable name"
                ));
            }
        }
        if let Some(t) = &self.ssh_tunnel {
            crate::tunnel::SshTunnelSpec::parse(t).map_err(|e| format!("ssh_tunnel {t:?}: {e}"))?;
        }
        if let Some(m) = &self.sslmode {
            if !crate::conn::KNOWN_SSLMODES.contains(&m.as_str()) {
                return Err(format!(
                    "unknown sslmode {m:?}; use one of {}",
                    crate::conn::KNOWN_SSLMODES.join(", ")
                ));
            }
        }
        if let Some(d) = &self.migrations_dir {
            migration_locations(d)?;
        }
        if let Some(v) = &self.version_table {
            crate::query::patches::applied_ids_sql(&v.config())
                .map_err(|e| format!("version_table: {e}"))?;
            v.check_patches()?;
        }
        for (key, id) in &self.secrets {
            self.secret_ref(id)
                .validate()
                .map_err(|e| format!("secrets.{key}: {e}"))?;
        }
        Ok(())
    }

    fn secret_ref(&self, id: &str) -> SecretRef {
        let scope = self.source.scope().unwrap_or_default();
        SecretRef {
            id: id.to_string(),
            region: scope.region,
            profile: scope.profile,
            account: scope.account,
        }
    }

    /// The secret for `database` on `server`: the `server/database` key
    /// first, then the `server` key.
    pub fn secret_for(&self, server: &str, database: &str) -> Option<SecretRef> {
        self.secrets
            .get(&format!("{server}/{database}"))
            .or_else(|| self.secrets.get(server))
            .map(|id| self.secret_ref(id))
    }

    /// How to connect to one database on a discovered server.
    fn connect_spec(
        &self,
        server: &str,
        database: &str,
        host: Option<&str>,
        port: Option<u16>,
    ) -> Result<ConnectSpec, String> {
        let host = host.ok_or_else(|| format!("{server} has no endpoint yet"))?;
        let ssh_tunnel = match &self.ssh_tunnel {
            Some(t) => Some(
                crate::tunnel::SshTunnelSpec::parse(t)
                    .map_err(|e| format!("ssh_tunnel {t:?}: {e}"))?,
            ),
            None => None,
        };
        Ok(ConnectSpec {
            dsn: Dsn {
                host: host.to_string(),
                port: port.unwrap_or(5432),
                user: self.user.clone(),
                password: None,
                dbname: database.to_string(),
                params: self
                    .sslmode
                    .iter()
                    .map(|m| ("sslmode".to_string(), m.clone()))
                    .collect(),
                ssh_tunnel,
            },
            secret: self.secret_for(server, database),
            password_env: self.password_env.clone(),
        })
    }
}

/// Parse `environments.toml`. Unknown keys are errors — a typo in a
/// filter must not silently widen what an environment covers. Names must
/// be unique and non-empty.
pub fn parse_config(text: &str) -> Result<Vec<Environment>, String> {
    let f: EnvironmentsFile =
        toml::from_str(text).map_err(|e| format!("environments.toml parse error: {e}"))?;
    let mut seen = std::collections::HashSet::new();
    for e in &f.environments {
        if e.name.trim().is_empty() {
            return Err("environments.toml: an environment has an empty name".to_string());
        }
        if !seen.insert(e.name.as_str()) {
            return Err(format!(
                "environments.toml: environment {:?} is defined twice",
                e.name
            ));
        }
        match &e.source {
            Source::Static { connections } if connections.is_empty() => {
                return Err(format!(
                    "environments.toml: environment {:?} lists no connections",
                    e.name
                ))
            }
            Source::Rds { instances, .. } if instances.is_empty() => {
                return Err(format!(
                    "environments.toml: environment {:?} matches no instances",
                    e.name
                ))
            }
            _ => {}
        }
        if let Some(scope) = e.source.scope() {
            scope
                .validate()
                .map_err(|err| format!("environments.toml: environment {:?}: {err}", e.name))?;
        }
        e.check_connection_settings()
            .map_err(|err| format!("environments.toml: environment {:?}: {err}", e.name))?;
    }
    Ok(f.environments)
}

/// One database on one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub database: String,
    /// RDS instance identifier, or the connection's name.
    pub server: String,
    pub role: Role,
    /// For a replica, the server it replicates.
    pub replica_of: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    /// e.g. `postgres 16.10`, when the source knows it.
    pub engine: Option<String>,
    /// e.g. `available`, `stopped`, when the source knows it.
    pub status: Option<String>,
    pub class: Option<String>,
    pub multi_az: Option<bool>,
    /// How to connect, or why this row can't be connected to.
    pub connect: Result<ConnectSpec, String>,
}

/// Everything a connect needs except the password, which is read from
/// `secret` at connect time when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectSpec {
    pub dsn: Dsn,
    pub secret: Option<SecretRef>,
    /// The environment variable the password is read from at connect,
    /// when there is no secret.
    pub password_env: Option<String>,
}

impl ConnectSpec {
    /// Put the `password_env` variable's value on `dsn` — read now, at
    /// connect, never stored. Nothing to do without one; an unset
    /// variable is an error rather than a password-less attempt.
    pub fn apply_password_env(&self, dsn: &mut Dsn) -> Result<(), String> {
        let Some(var) = &self.password_env else {
            return Ok(());
        };
        match std::env::var(var).ok().filter(|v| !v.is_empty()) {
            Some(pw) => {
                dsn.password = Some(pw);
                Ok(())
            }
            None => Err(format!("password_env names ${var}, which is not set")),
        }
    }
}

/// An environment's rows, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvReport {
    pub name: String,
    pub source: String,
    pub targets: Vec<Target>,
    pub error: Option<String>,
    /// The AWS profile an `rds` source lists with. Not drawn as such, but
    /// the aws CLI names it in its errors, so redaction needs it.
    pub profile: Option<String>,
    /// `Some` when the environment opted into probes.
    pub probe: Option<ProbeSettings>,
}

/// Why instances matched but gave no rows: none names a database
/// (Aurora keeps it on the cluster; an instance can be created without
/// one). `None` when nothing matched at all. Pure.
pub fn rds_no_rows_reason(env: &Environment, all: &[rds::RdsInstance]) -> Option<String> {
    let Source::Rds { instances, .. } = &env.source else {
        return None;
    };
    let matched: Vec<&str> = rds::select(all, instances)
        .iter()
        .map(|i| i.id.as_str())
        .collect();
    if matched.is_empty() || !env.databases.is_empty() {
        return None;
    }
    Some(format!(
        "{} matched, but no instance names a database (Aurora keeps it on the cluster) — set databases = [\"…\"] for {}",
        matched.join(", "),
        env.name
    ))
}

/// RDS rows for one environment from an already-fetched instance list.
pub fn rds_targets(env: &Environment, all: &[rds::RdsInstance]) -> Vec<Target> {
    let Source::Rds { instances, .. } = &env.source else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in rds::select(all, instances) {
        let role = if i.source.is_some() {
            Role::Replica
        } else if !i.replicas.is_empty() {
            Role::Primary
        } else {
            Role::Direct
        };
        let dbs: Vec<String> = if env.databases.is_empty() {
            i.db_name.iter().cloned().collect()
        } else {
            env.databases.clone()
        };
        for db in dbs {
            out.push(Target {
                connect: env.connect_spec(&i.id, &db, i.host.as_deref(), i.port),
                database: db,
                server: i.id.clone(),
                role,
                replica_of: i.source.clone(),
                host: i.host.clone(),
                port: i.port,
                engine: Some(match i.upgrades.as_ref().and_then(|u| u.label()) {
                    Some(up) => format!("{} {} ({up})", i.engine, i.engine_version),
                    None => format!("{} {}", i.engine, i.engine_version),
                }),
                status: Some(i.status.clone()),
                class: Some(i.class.clone()),
                multi_az: Some(i.multi_az),
            });
        }
    }
    group_replicas_under_primaries(&mut out);
    out
}

/// Order rows so each database's replicas sit directly under it on its
/// primary: `ledger` on the primary, then `ledger` on each replica, then
/// `ledgerlog`, and so on. Primaries keep the order they came in. Pure.
pub fn group_replicas_under_primaries(rows: &mut [Target]) {
    let mut roots: Vec<String> = Vec::new();
    // A replica groups under its primary's name, listed or not: replicas
    // of an unlisted primary still sit together.
    let root_of =
        |t: &Target| -> String { t.replica_of.clone().unwrap_or_else(|| t.server.clone()) };
    for t in rows.iter() {
        let r = root_of(t);
        if !roots.contains(&r) {
            roots.push(r);
        }
    }
    let mut dbs: Vec<String> = Vec::new();
    for t in rows.iter() {
        if !dbs.contains(&t.database) {
            dbs.push(t.database.clone());
        }
    }
    let pos = |v: &[String], s: &str| v.iter().position(|x| x == s).unwrap_or(usize::MAX);
    rows.sort_by_cached_key(|t| {
        (
            pos(&roots, &root_of(t)),
            pos(&dbs, &t.database),
            t.replica_of.is_some(),
            t.server.clone(),
        )
    });
}

/// Static rows from the personal connection entries, by name. An unknown
/// name is an error for the environment, not a silent gap. Replicas are
/// linked to their primaries ([`link_static_replicas`]) and listed under
/// them, as an `rds` source's are.
pub fn static_targets(names: &[String], entries: &[Entry]) -> Result<Vec<Target>, String> {
    let mut rows = names
        .iter()
        .map(|n| {
            let e = entries
                .iter()
                .find(|e| &e.name == n)
                .ok_or_else(|| format!("no connection named {n:?} in connections.toml"))?;
            let dsn = e.dsn.as_ref().ok_or_else(|| {
                format!(
                    "connection {n:?} is not usable: it {}",
                    e.refusal.as_deref().unwrap_or("has no DSN")
                )
            })?;
            Ok(Target {
                database: dsn.dbname.clone(),
                server: n.clone(),
                role: e.role,
                replica_of: None,
                host: Some(dsn.host.clone()),
                port: Some(dsn.port),
                engine: None,
                status: None,
                class: None,
                multi_az: None,
                connect: Ok(ConnectSpec {
                    dsn: dsn.clone(),
                    secret: e.secret.clone(),
                    // connections.toml's own password_env was applied
                    // when the entry was built.
                    password_env: None,
                }),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    link_static_replicas(&mut rows);
    group_replicas_under_primaries(&mut rows);
    Ok(rows)
}

/// `connections.toml` gives a role but not whose replica a connection
/// is, so a static environment links them by database: a replica's
/// primary is the environment's one `role = "primary"` row of the same
/// database. With none, or more than one, it isn't guessed and the
/// replica stays unlinked (listed flat, and no primary is read through
/// it). Pure.
pub fn link_static_replicas(rows: &mut [Target]) {
    let primary_of = |db: &str, rows: &[Target]| -> Option<String> {
        let mut primaries = rows
            .iter()
            .filter(|p| p.role == Role::Primary && p.database == db);
        match (primaries.next(), primaries.next()) {
            (Some(p), None) => Some(p.server.clone()),
            _ => None,
        }
    };
    for i in 0..rows.len() {
        if rows[i].role == Role::Replica && rows[i].replica_of.is_none() {
            rows[i].replica_of = primary_of(&rows[i].database, rows);
        }
    }
}

/// Every environment's report. `fetch_rds` lists instances for one scope;
/// it runs once per distinct scope however many environments share it,
/// and its error lands on each environment that needed it. Blocking.
pub fn gather(
    envs: &[Environment],
    personal: &[Entry],
    mut fetch_rds: impl FnMut(&AwsScope) -> Result<Vec<rds::RdsInstance>, String>,
    mut fetch_docker: impl FnMut() -> Result<Vec<docker::Container>, String>,
) -> Vec<EnvReport> {
    let mut cache: HashMap<AwsScope, Result<Vec<rds::RdsInstance>, String>> = HashMap::new();
    // One `docker ps` however many docker environments there are.
    let mut containers: Option<Result<Vec<docker::Container>, String>> = None;
    envs.iter()
        .map(|env| {
            let result = match &env.source {
                Source::Static { connections } => static_targets(connections, personal),
                Source::Rds { .. } => {
                    let scope = env.source.scope().unwrap_or_default();
                    let listed = cache
                        .entry(scope.clone())
                        .or_insert_with(|| fetch_rds(&scope));
                    match listed {
                        Ok(all) => {
                            let rows = rds_targets(env, all);
                            match rows
                                .is_empty()
                                .then(|| rds_no_rows_reason(env, all))
                                .flatten()
                            {
                                Some(why) => Err(why),
                                None => Ok(rows),
                            }
                        }
                        Err(e) => Err(e.clone()),
                    }
                }
                Source::Docker { .. } => match containers.get_or_insert_with(&mut fetch_docker) {
                    Ok(all) => Ok(docker::docker_targets(env, all)),
                    Err(e) => Err(e.clone()),
                },
            };
            let (targets, error) = match result {
                Ok(t) if t.is_empty() => (t, Some("matched nothing".to_string())),
                Ok(t) => (t, None),
                Err(e) => (Vec::new(), Some(e)),
            };
            EnvReport {
                name: env.name.clone(),
                source: env.source.label(),
                targets,
                error,
                profile: env.source.rds_scope().and_then(|s| s.profile),
                probe: env.probe.then(|| ProbeSettings {
                    version_table: env.version_table.clone(),
                    migrations_dir: env.migrations_dir.clone(),
                }),
            }
        })
        .collect()
}

/// `describe-db-instances`, with Aurora's writer and readers placed
/// when there is an Aurora instance to place (`describe-db-clusters`).
/// Best effort for the clusters: without them each instance stands
/// alone. Blocking.
pub(crate) fn list_rds(scope: &AwsScope) -> Result<Vec<rds::RdsInstance>, String> {
    let mut all = rds::parse_describe(&crate::aws_cli::run(&rds::describe_args(scope))?)?;
    if all.iter().any(|i| i.is_postgres() && i.cluster.is_some()) {
        match crate::aws_cli::run(&rds::describe_clusters_args(scope))
            .and_then(|j| rds::parse_clusters(&j))
        {
            Ok(clusters) => rds::apply_clusters(&mut all, &clusters),
            Err(e) => tracing::warn!("Aurora cluster members unavailable: {e}"),
        }
    }
    Ok(all)
}

/// RDS instances through the aws CLI, the account checked first, without
/// the upgrade targets [`fetch_rds`] adds: for finding a server an
/// environment lists (logs, patches), which needs the listing and its
/// replica links but not one `describe-db-engine-versions` per version.
/// Blocking.
pub fn fetch_rds_listing(scope: &AwsScope) -> Result<Vec<rds::RdsInstance>, String> {
    scope.validate()?;
    scope.check_account("RDS instances")?;
    list_rds(scope)
}

/// List RDS instances through the aws CLI, checking the account first,
/// with each engine version's upgrade targets for the overview.
/// Blocking.
pub fn fetch_rds(scope: &AwsScope) -> Result<Vec<rds::RdsInstance>, String> {
    let mut all = fetch_rds_listing(scope)?;
    // Upgrade targets, once per engine version. Best effort: without
    // them the engine column just shows the version.
    let mut seen: HashMap<(String, String), Option<rds::Upgrades>> = HashMap::new();
    for i in all.iter_mut().filter(|i| i.is_postgres()) {
        let key = (i.engine.clone(), i.engine_version.clone());
        let up = seen.entry(key.clone()).or_insert_with(|| {
            match crate::aws_cli::run(&rds::engine_versions_args(scope, &key.0, &key.1))
                .and_then(|j| rds::parse_upgrades(&j))
            {
                Ok(u) => Some(u),
                Err(e) => {
                    tracing::warn!("upgrade targets for {} {} unavailable: {e}", key.0, key.1);
                    None
                }
            }
        });
        i.upgrades = up.clone();
    }
    Ok(all)
}

/// What [`load`] read: the environments (or why there are none) and the
/// file's path, for messages.
pub struct Loaded {
    pub path: std::path::PathBuf,
    pub environments: Result<Vec<Environment>, String>,
}

/// Read `environments.toml`. A missing file is no environments, not an
/// error.
pub fn load() -> Loaded {
    let path = crate::util::config_file("environments.toml");
    let environments = match std::fs::read_to_string(&path) {
        Ok(text) => parse_config(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    };
    Loaded { path, environments }
}

/// Load everything and gather. Blocking: the aws CLI runs here.
pub fn load_and_gather() -> Result<Vec<EnvReport>, String> {
    let loaded = load();
    let envs = loaded.environments?;
    let personal = {
        let path = crate::util::config_file("connections.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => crate::mcp::connections::parse_personal(&text),
            Err(_) => crate::mcp::connections::Personal::default(),
        }
    };
    let entries = crate::mcp::connections::build(&personal.connections, Vec::new(), false);
    Ok(gather(&envs, &entries, fetch_rds, docker::fetch))
}

#[cfg(test)]
mod tests {

    /// Aurora instances carry no DBName: an environment over them used to
    /// say "matched nothing" although two instances matched.
    #[test]
    fn instances_without_a_database_say_so_and_how_to_fix_it() {
        let envs = parse_config(
            "[[environments]]\nname = \"app\"\nsource = { kind = \"rds\", instances = [\"app-*\"] }\n",
        )
        .unwrap();
        let aurora = rds::parse_describe(
            r#"{"DBInstances": [
              {"DBInstanceIdentifier": "app-writer", "Engine": "aurora-postgresql", "DBClusterIdentifier": "app"},
              {"DBInstanceIdentifier": "app-reader", "Engine": "aurora-postgresql", "DBClusterIdentifier": "app"}]}"#,
        )
        .unwrap();
        let r = gather(&envs, &[], |_| Ok(aurora.clone()), || Ok(Vec::new()));
        let e = r[0].error.clone().unwrap();
        assert!(
            e.starts_with("app-reader, app-writer matched, but no instance names a database"),
            "{e}"
        );
        assert!(e.contains("set databases"), "{e}");
        // Nothing matching at all still says so.
        let r = gather(&envs, &[], |_| Ok(Vec::new()), || Ok(Vec::new()));
        assert_eq!(r[0].error.as_deref(), Some("matched nothing"));
    }

    #[test]
    fn aurora_readers_are_rows_under_the_writer_with_the_clusters_database() {
        let mut all = rds::parse_describe(rds::tests::AURORA_INSTANCES).unwrap();
        rds::apply_clusters(
            &mut all,
            &rds::parse_clusters(rds::tests::AURORA_CLUSTERS).unwrap(),
        );
        let envs = parse_config(
            "[[environments]]\nname = \"app\"\nsource = { kind = \"rds\", instances = [\"app-1\"] }\n",
        )
        .unwrap();
        let rows: Vec<(String, String, Role, Option<String>)> = rds_targets(&envs[0], &all)
            .into_iter()
            .map(|t| (t.server, t.database, t.role, t.replica_of))
            .collect();
        let row = |s: &str, d: &str, r, of: Option<&str>| {
            (s.to_string(), d.to_string(), r, of.map(str::to_string))
        };
        assert_eq!(
            rows,
            [
                row("app-2", "ledger", Role::Primary, None),
                row("app-1", "ledger", Role::Replica, Some("app-2")),
                row("app-3", "own", Role::Replica, Some("app-2")),
            ]
        );
    }

    use super::*;

    const CONFIG: &str = r#"
[[environments]]
name = "demo"
databases = ["ledger", "ledgerlog"]
source = { kind = "rds", instances = ["demo"], region = "us-west-1", account = "210987654321" }

[[environments]]
name = "prod"
source = { kind = "rds", instances = ["production"], region = "us-west-1", account = "210987654321" }

[[environments]]
name = "local"
source = { kind = "static", connections = ["ledger-local"] }
"#;

    #[test]
    fn migration_locations_read_like_flyway_locations() {
        assert_eq!(
            migration_locations("~/app/src/main/resources/db/migration").unwrap(),
            ["~/app/src/main/resources/db/migration"]
        );
        assert_eq!(
            migration_locations("filesystem:/srv/a, ~/b").unwrap(),
            ["/srv/a", "~/b"]
        );
        let e = migration_locations("~/a,classpath:db/migration").unwrap_err();
        assert!(
            e.contains("can't see a classpath") && e.contains("src/main/resources/db/migration"),
            "{e}"
        );
        assert!(migration_locations("db/migration")
            .unwrap_err()
            .contains("must be absolute"));
        assert!(migration_locations("filesystem:db/migration")
            .unwrap_err()
            .contains("\"db/migration\" must be absolute"));
        assert!(migration_locations("/a,,/b")
            .unwrap_err()
            .contains("empty entry"));
        let settings = ProbeSettings {
            version_table: None,
            migrations_dir: Some("/srv/a,filesystem:/srv/b".into()),
        };
        assert_eq!(
            settings.migrations_dirs(),
            Some(Ok(vec!["/srv/a".into(), "/srv/b".into()]))
        );
    }

    #[test]
    fn config_parses_both_source_kinds() {
        let envs = parse_config(CONFIG).unwrap();
        assert_eq!(envs.len(), 3);
        assert!(matches!(&envs[0].source, Source::Rds { instances, .. } if instances == &["demo"]));
        assert_eq!(envs[0].databases, ["ledger", "ledgerlog"]);
        assert!(
            matches!(&envs[2].source, Source::Static { connections } if connections == &["ledger-local"])
        );
        assert_eq!(envs[0].source.label(), "RDS 210987654321 us-west-1");
    }

    #[test]
    fn config_mistakes_are_errors_not_guesses() {
        for (bad, why) in [
            ("[[environments]]\nname = \"x\"\nsource = { kind = \"rds\", instance = [\"demo\"] }\n", "typo'd key"),
            ("[[environments]]\nname = \"x\"\nsource = { kind = \"rds\", instances = [\"demo\"], regoin = \"us-west-1\" }\n", "typo'd optional key beside valid ones"),
            ("[[environments]]\nname = \"x\"\nsource = { kind = \"gcp\" }\n", "unknown kind"),
            ("[[environments]]\nname = \"x\"\nsource = { kind = \"rds\", instances = [] }\n", "matches nothing"),
            ("[[environments]]\nname = \"x\"\nsource = { kind = \"static\", connections = [] }\n", "lists nothing"),
            ("[[environments]]\nname = \"\"\nsource = { kind = \"static\", connections = [\"a\"] }\n", "empty name"),
            ("[[environments]]\nname = \"x\"\nsource = { kind = \"rds\", instances = [\"d\"], region = \"--debug\" }\n", "option-shaped region"),
            ("[[environments]]\nname = \"x\"\nsource = { kind = \"static\", connections = [\"a\"] }\n[[environments]]\nname = \"x\"\nsource = { kind = \"static\", connections = [\"b\"] }\n", "duplicate"),
            ("[[environments]]\nname = \"x\"\nsource = { kind = \"static\", connections = [\"a\"] }\nfilter = \"y\"\n", "unknown env key"),
        ] {
            assert!(parse_config(bad).is_err(), "{why}");
        }
        assert_eq!(parse_config("").unwrap(), Vec::new());
    }

    fn all() -> Vec<rds::RdsInstance> {
        rds::parse_describe(rds::tests::FIXTURE).unwrap()
    }

    #[test]
    fn rds_rows_are_one_per_database_with_roles_from_the_replica_links() {
        let envs = parse_config(CONFIG).unwrap();
        let t = rds_targets(&envs[0], &all());
        let rows: Vec<(String, String, Role)> = t
            .iter()
            .map(|t| (t.server.clone(), t.database.clone(), t.role))
            .collect();
        assert_eq!(
            rows,
            [
                ("demo".into(), "ledger".into(), Role::Primary),
                (
                    "demo-read-only-replica".into(),
                    "ledger".into(),
                    Role::Replica
                ),
                ("demo".into(), "ledgerlog".into(), Role::Primary),
                (
                    "demo-read-only-replica".into(),
                    "ledgerlog".into(),
                    Role::Replica
                ),
            ],
            "each database's replica sits directly under it"
        );
        assert_eq!(t[1].replica_of.as_deref(), Some("demo"));
        assert_eq!(t[0].engine.as_deref(), Some("postgres 16.10"));
        // No `databases`: the instance's own DBName.
        let prod = rds_targets(&envs[1], &all());
        assert_eq!(
            prod.iter().map(|t| t.database.as_str()).collect::<Vec<_>>(),
            ["prod", "prod"]
        );
    }

    #[test]
    fn an_instance_with_no_replicas_and_no_source_is_direct() {
        let env = Environment {
            name: "dev".into(),
            source: Source::Rds {
                instances: vec!["development".into()],
                region: None,
                profile: None,
                account: None,
            },
            databases: vec![],
            ssh_tunnel: None,
            sslmode: None,
            user: None,
            password_env: None,
            secrets: Default::default(),
            probe: false,
            version_table: None,
            migrations_dir: None,
            protected: None,
        };
        let t = rds_targets(&env, &all());
        assert_eq!(t[0].role, Role::Direct);
        assert_eq!(t[0].status.as_deref(), Some("stopped"));
    }

    #[test]
    fn gather_lists_once_per_scope_and_reports_errors_per_environment() {
        let envs = parse_config(CONFIG).unwrap();
        let mut calls = 0;
        let reports = gather(
            &envs,
            &[],
            |_| {
                calls += 1;
                Ok(all())
            },
            || Ok(Vec::new()),
        );
        assert_eq!(calls, 1, "demo and prod share a scope");
        assert_eq!(reports[0].targets.len(), 4);
        assert_eq!(reports[1].targets.len(), 2);
        assert!(reports[2]
            .error
            .as_deref()
            .unwrap()
            .contains("ledger-local"));

        let reports = gather(
            &envs[..1],
            &[],
            |_| Err("aws sts failed: expired".to_string()),
            || Ok(Vec::new()),
        );
        assert_eq!(reports[0].error.as_deref(), Some("aws sts failed: expired"));
        assert!(reports[0].targets.is_empty());
    }

    #[test]
    fn a_report_carries_its_rds_profile_even_when_listing_fails() {
        // The aws CLI names the profile in its errors; redaction reads
        // it off the report.
        let envs = parse_config(
            r#"
[[environments]]
name = "billing"
source = { kind = "rds", instances = ["ledger"], region = "us-west-1", profile = "examplecorp-sso" }
"#,
        )
        .unwrap();
        let reports = gather(
            &envs,
            &[],
            |_| Err("The config profile (examplecorp-sso) could not be found".into()),
            || Ok(Vec::new()),
        );
        assert_eq!(reports[0].profile.as_deref(), Some("examplecorp-sso"));
        let local = &parse_config(CONFIG).unwrap()[2..];
        let reports = gather(local, &[], |_| Ok(Vec::new()), || Ok(Vec::new()));
        assert_eq!(reports[0].profile, None);
    }

    #[test]
    fn static_rows_come_from_named_personal_connections() {
        let personal = crate::mcp::connections::parse_personal(
            "[[connections]]\nname = \"ledger-local\"\nurl = \"postgres://ledger@localhost:5432/ledger\"\n\
             [[connections]]\nname = \"broken\"\nurl = \"postgres://u:${PW}@h/d\"\n",
        );
        let entries = crate::mcp::connections::build(&personal.connections, Vec::new(), false);
        let t = static_targets(&["ledger-local".into()], &entries).unwrap();
        assert_eq!(
            (
                t[0].host.as_deref(),
                t[0].port,
                t[0].database.as_str(),
                t[0].role
            ),
            (Some("localhost"), Some(5432), "ledger", Role::Direct)
        );
        assert!(static_targets(&["broken".into()], &entries)
            .unwrap_err()
            .contains("not usable"));
        assert!(static_targets(&["nope".into()], &entries)
            .unwrap_err()
            .contains("no connection"));
    }

    /// A static environment with a primary and its replica lists and
    /// probes like an rds one: the replica under the primary, and the
    /// primary read through it.
    #[test]
    fn a_static_replica_sits_under_its_primary_and_stands_in_for_it() {
        let personal = crate::mcp::connections::parse_personal(
            "[[connections]]\nname = \"panel-replica\"\nurl = \"postgres://u@replica.example/app\"\nrole = \"replica\"\n\
             [[connections]]\nname = \"panel-primary\"\nurl = \"postgres://u@primary.example/app\"\nrole = \"primary\"\n\
             [[connections]]\nname = \"panel-other\"\nurl = \"postgres://u@other.example/other\"\n",
        );
        let entries = crate::mcp::connections::build(&personal.connections, Vec::new(), false);
        let names = ["panel-replica", "panel-primary", "panel-other"].map(String::from);
        let t = static_targets(&names, &entries).unwrap();
        let rows: Vec<_> = t
            .iter()
            .map(|t| (t.server.as_str(), t.replica_of.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                ("panel-primary", None),
                ("panel-replica", Some("panel-primary")),
                ("panel-other", None),
            ]
        );
        use probe::Step;
        assert_eq!(
            probe::plan(&t),
            [Step::ViaReplica, Step::Connect, Step::Connect]
        );
    }

    #[test]
    fn a_static_replica_is_linked_only_to_the_one_primary_of_its_database() {
        let row = |server: &str, db: &str, role: Role| Target {
            database: db.into(),
            server: server.into(),
            role,
            replica_of: None,
            host: None,
            port: None,
            engine: None,
            status: None,
            class: None,
            multi_az: None,
            connect: Err("n/a".into()),
        };
        let link = |mut rows: Vec<Target>| {
            link_static_replicas(&mut rows);
            rows.into_iter().map(|t| t.replica_of).collect::<Vec<_>>()
        };
        // Another database's primary isn't this replica's.
        assert_eq!(
            link(vec![
                row("p", "a", Role::Primary),
                row("r", "b", Role::Replica)
            ]),
            [None, None]
        );
        // Two primaries of one database: not guessed.
        assert_eq!(
            link(vec![
                row("p1", "a", Role::Primary),
                row("p2", "a", Role::Primary),
                row("r", "a", Role::Replica)
            ]),
            [None, None, None]
        );
        // A direct row is nobody's primary.
        assert_eq!(
            link(vec![
                row("d", "a", Role::Direct),
                row("r", "a", Role::Replica)
            ]),
            [None, None]
        );
        assert_eq!(
            link(vec![
                row("r", "a", Role::Replica),
                row("p", "a", Role::Primary)
            ]),
            [Some("p".to_string()), None]
        );
    }

    const CONNECTABLE: &str = r#"
[[environments]]
name = "demo"
source = { kind = "rds", instances = ["demo"], region = "us-west-1", account = "210987654321" }
ssh_tunnel = "ec2-user@bastion.example.com"
sslmode = "require"

[environments.secrets]
"demo" = "demo/database/ledger/credentials"
"demo-read-only-replica" = "demo/database/ledger_replica/credentials"
"demo/ledgerlog" = "demo/database/ledgerlog/credentials"
"#;

    #[test]
    fn rds_rows_carry_how_to_connect() {
        let mut envs = parse_config(CONNECTABLE).unwrap();
        envs[0].databases = vec!["ledger".into(), "ledgerlog".into()];
        let t = rds_targets(&envs[0], &all());
        let spec = t[0].connect.as_ref().unwrap();
        assert_eq!(
            (
                spec.dsn.host.as_str(),
                spec.dsn.port,
                spec.dsn.dbname.as_str()
            ),
            ("demo.x.us-west-1.rds.amazonaws.com", 5432, "ledger")
        );
        assert_eq!(spec.dsn.password, None, "read at connect, never stored");
        assert_eq!(
            spec.dsn.params,
            [("sslmode".to_string(), "require".to_string())]
        );
        let tunnel = spec.dsn.ssh_tunnel.as_ref().unwrap();
        assert_eq!(
            (tunnel.user.as_deref(), tunnel.host.as_str()),
            (Some("ec2-user"), "bastion.example.com")
        );
        let secret = spec.secret.as_ref().unwrap();
        assert_eq!(secret.id, "demo/database/ledger/credentials");
        assert_eq!(
            (secret.region.as_deref(), secret.account.as_deref()),
            (Some("us-west-1"), Some("210987654321")),
            "read in the source's scope"
        );
        let secret_of = |i: usize| t[i].connect.as_ref().unwrap().secret.clone().unwrap().id;
        assert_eq!(
            secret_of(2),
            "demo/database/ledgerlog/credentials",
            "server/database beats server"
        );
        assert_eq!(secret_of(1), "demo/database/ledger_replica/credentials");
        assert_eq!(
            secret_of(3),
            "demo/database/ledger_replica/credentials",
            "the server key covers every database on it"
        );
    }

    #[test]
    fn rows_without_settings_still_connect_and_without_an_endpoint_do_not() {
        let envs = parse_config(CONFIG).unwrap();
        let t = rds_targets(&envs[0], &all());
        let spec = t[0].connect.as_ref().unwrap();
        assert!(spec.secret.is_none() && spec.dsn.ssh_tunnel.is_none());
        assert!(spec.dsn.params.is_empty());

        let mut no_endpoint = all();
        no_endpoint[0].host = None;
        let t = rds_targets(&envs[0], &no_endpoint);
        assert!(t[0].connect.as_ref().unwrap_err().contains("no endpoint"));
    }

    #[test]
    fn connection_setting_mistakes_are_errors() {
        let rds =
            "[[environments]]\nname = \"x\"\nsource = { kind = \"rds\", instances = [\"d\"] }\n";
        let fixed = "[[environments]]\nname = \"x\"\nsource = { kind = \"static\", connections = [\"a\"] }\n";
        for (bad, why) in [
            (
                format!("{rds}ssh_tunnel = \"-oProxyCommand=x\"\n"),
                "option-shaped tunnel",
            ),
            (
                format!("{rds}ssh_tunnel = \"h:notaport\"\n"),
                "bad tunnel port",
            ),
            (format!("{rds}sslmode = \"requir\"\n"), "typo'd sslmode"),
            (
                format!("{rds}[environments.secrets]\nd = \"--debug\"\n"),
                "option-shaped secret",
            ),
            (format!("{fixed}sslmode = \"require\"\n"), "static sslmode"),
            (
                format!("{fixed}[environments.secrets]\na = \"s\"\n"),
                "static secrets",
            ),
            (format!("{rds}secret = \"s\"\n"), "singular typo"),
            (
                format!("{rds}migrations_dir = \"classpath:db/migration\"\n"),
                "a classpath location",
            ),
            (
                format!("{rds}migrations_dir = \"/srv/a, db/migration\"\n"),
                "a relative location after a good one",
            ),
        ] {
            assert!(parse_config(&bad).is_err(), "{why}");
        }
        assert!(parse_config(&format!("{rds}sslmode = \"verify-full\"\nuser = \"me\"\n")).is_ok());
        assert!(parse_config(&format!(
            "{rds}migrations_dir = \"filesystem:/srv/a, ~/b\"\n"
        ))
        .is_ok());
    }

    #[test]
    fn static_rows_connect_with_the_personal_entry_and_its_secret() {
        let personal = crate::mcp::connections::parse_personal(
            "[[connections]]\nname = \"r\"\nurl = \"postgres://h:5432/d\"\n\
             secret = \"env/database/d/credentials\"\naws_region = \"us-west-1\"\nrole = \"replica\"\n",
        );
        let entries = crate::mcp::connections::build(&personal.connections, Vec::new(), false);
        let t = static_targets(&["r".into()], &entries).unwrap();
        let spec = t[0].connect.as_ref().unwrap();
        assert_eq!(spec.dsn.host, "h");
        assert_eq!(
            spec.secret.as_ref().map(|s| s.id.as_str()),
            Some("env/database/d/credentials")
        );
        assert_eq!(t[0].role, Role::Replica);
    }

    #[test]
    fn probes_are_opt_in_and_their_version_table_is_checked() {
        let base =
            "[[environments]]\nname = \"x\"\nsource = { kind = \"rds\", instances = [\"d\"] }\n";
        assert!(!parse_config(base).unwrap()[0].probe, "off unless set");
        let ok = parse_config(&format!(
            "{base}probe = true\nversion_table = {{ table = \"public.db_version\", column = \"id\", patches = \"~/src/db\" }}\n"
        ))
        .unwrap();
        assert!(ok[0].probe);
        let v = ok[0].version_table.as_ref().unwrap();
        assert!(v.patch_dir().unwrap().unwrap().is_absolute());
        for (bad, why) in [
            (format!("{base}version_table = {{ table = \"t; drop\", column = \"id\" }}\n"), "not an identifier"),
            (format!("{base}version_table = {{ table = \"t\", column = \"s.id\" }}\n"), "qualified column"),
            (format!("{base}version_table = {{ table = \"t\", column = \"id\", patches = \"db/patches\" }}\n"), "relative dir"),
            (format!("{base}version_table = {{ table = \"t\", colum = \"id\" }}\n"), "typo"),
            (format!("{base}probes = true\n"), "typo'd opt-in"),
        ] {
            assert!(parse_config(&bad).is_err(), "{why}");
        }
        // The report carries the settings only when the environment opts in.
        let envs = parse_config(&format!("{base}probe = true\n")).unwrap();
        let reports = gather(&envs, &[], |_| Ok(all()), || Ok(Vec::new()));
        assert!(reports[0].probe.is_some());
        let reports = gather(
            &parse_config(base).unwrap(),
            &[],
            |_| Ok(all()),
            || Ok(Vec::new()),
        );
        assert!(reports[0].probe.is_none());
    }

    #[test]
    fn docker_environments_parse_list_once_and_stay_local() {
        let envs = parse_config(
            "[[environments]]\nname = \"a\"\nsource = { kind = \"docker\" }\ndatabases = [\"ledger\"]\n\
             [[environments]]\nname = \"b\"\nsource = { kind = \"docker\", containers = [\"pg-*\"] }\nprobe = true\n",
        )
        .unwrap();
        assert!(matches!(&envs[0].source, Source::Docker { containers } if containers == &["*"]));
        assert_eq!(envs[0].source.label(), "docker");
        let mut listings = 0;
        let reports = gather(
            &envs,
            &[],
            |_| panic!("no aws for docker"),
            || {
                listings += 1;
                docker::parse_ps(
                    "{\"Names\":\"pg-1\",\"Image\":\"postgres:16\",\"Ports\":\"0.0.0.0:5433->5432/tcp\",\"State\":\"running\"}",
                )
            },
        );
        assert_eq!(listings, 1, "one docker ps for both environments");
        assert_eq!(reports[0].targets[0].database, "ledger");
        assert_eq!(reports[1].targets[0].port, Some(5433));
        assert!(reports[1].probe.is_some(), "docker environments can probe");
        let failed = gather(
            &envs[..1],
            &[],
            |_| unreachable!(),
            || Err("docker ps failed: daemon down".into()),
        );
        assert_eq!(
            failed[0].error.as_deref(),
            Some("docker ps failed: daemon down")
        );

        for bad in [
            "[[environments]]\nname = \"a\"\nsource = { kind = \"docker\" }\nssh_tunnel = \"h\"\n",
            "[[environments]]\nname = \"a\"\nsource = { kind = \"docker\" }\n[environments.secrets]\nx = \"s\"\n",
            "[[environments]]\nname = \"a\"\nsource = { kind = \"docker\", container = [\"x\"] }\n",
        ] {
            assert!(parse_config(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn password_env_is_read_at_connect_and_never_stored() {
        let base =
            "[[environments]]\nname = \"x\"\nsource = { kind = \"rds\", instances = [\"demo\"] }\n";
        let envs =
            parse_config(&format!("{base}password_env = \"PGMAN_TEST_ENV_PW_A\"\n")).unwrap();
        let t = rds_targets(&envs[0], &all());
        let spec = t[0].connect.as_ref().unwrap();
        assert_eq!(spec.password_env.as_deref(), Some("PGMAN_TEST_ENV_PW_A"));
        assert_eq!(spec.dsn.password, None, "not read at listing");

        let mut dsn = spec.dsn.clone();
        let err = spec.apply_password_env(&mut dsn).unwrap_err();
        assert_eq!(
            err,
            "password_env names $PGMAN_TEST_ENV_PW_A, which is not set"
        );
        // SAFETY: a variable only this test touches.
        unsafe { std::env::set_var("PGMAN_TEST_ENV_PW_A", "") };
        assert!(
            spec.apply_password_env(&mut dsn).is_err(),
            "empty is unset, not a blank password"
        );
        unsafe { std::env::set_var("PGMAN_TEST_ENV_PW_A", "s3cret") };
        spec.apply_password_env(&mut dsn).unwrap();
        assert_eq!(dsn.password.as_deref(), Some("s3cret"));
        unsafe { std::env::remove_var("PGMAN_TEST_ENV_PW_A") };

        for bad in [
            format!("{base}password_env = \"1BAD\"\n"),
            format!("{base}password_env = \"A-B\"\n"),
            format!("{base}password_env = \"$PW\"\n"),
            "[[environments]]\nname = \"x\"\nsource = { kind = \"static\", connections = [\"a\"] }\npassword_env = \"PW\"\n".to_string(),
        ] {
            assert!(parse_config(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn replicas_sit_under_their_primary_per_database_and_orphans_stand_alone() {
        let t = |server: &str, db: &str, of: Option<&str>| Target {
            database: db.into(),
            server: server.into(),
            role: if of.is_some() {
                Role::Replica
            } else {
                Role::Primary
            },
            replica_of: of.map(str::to_string),
            host: None,
            port: None,
            engine: None,
            status: None,
            class: None,
            multi_az: None,
            connect: Err("x".into()),
        };
        let mut rows = vec![
            t("b", "app", None),
            t("b", "log", None),
            t("b-r2", "app", Some("b")),
            t("b-r1", "app", Some("b")),
            t("b-r1", "log", Some("b")),
            t("orphan-r", "app", Some("gone")),
            t("a", "app", None),
        ];
        group_replicas_under_primaries(&mut rows);
        let got: Vec<String> = rows
            .iter()
            .map(|r| format!("{}/{}", r.server, r.database))
            .collect();
        assert_eq!(
            got,
            [
                "b/app",
                "b-r1/app",
                "b-r2/app",
                "b/log",
                "b-r1/log",
                "orphan-r/app",
                "a/app"
            ],
            "primaries keep their order; replicas under each database, by name"
        );
    }

    #[test]
    fn a_version_table_can_name_the_databases_that_have_it() {
        let envs = parse_config(
            "[[environments]]\nname = \"x\"\nsource = { kind = \"rds\", instances = [\"d\"] }\n\
             version_table = { table = \"t_schema_version\", column = \"schema_version_id\", databases = [\"ledger\"] }\n",
        )
        .unwrap();
        let v = envs[0].version_table.as_ref().unwrap();
        assert!(v.applies_to("ledger") && !v.applies_to("ledgerlog"));
        let every = VersionTable {
            databases: Vec::new(),
            ..v.clone()
        };
        assert!(every.applies_to("anything"), "empty means every database");
    }

    #[test]
    fn the_engine_column_names_the_upgrades_available() {
        let envs = parse_config(CONFIG).unwrap();
        let mut instances = all();
        instances[0].upgrades = Some(rds::Upgrades {
            minor: Some("16.15".into()),
            major: Some("18.6".into()),
        });
        let t = rds_targets(&envs[0], &instances);
        assert_eq!(
            t[0].engine.as_deref(),
            Some("postgres 16.10 (↑16.15 · 18.6)")
        );
        assert_eq!(
            t[1].engine.as_deref(),
            Some("postgres 16.10"),
            "the replica: not looked up here"
        );
    }
}
