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
pub mod probe;
pub mod rds;

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
    /// Where the patch level lives, for probes.
    #[serde(default)]
    pub version_table: Option<VersionTable>,
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
}

impl VersionTable {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSettings {
    pub version_table: Option<VersionTable>,
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
    /// `Some` when the environment opted into probes.
    pub probe: Option<ProbeSettings>,
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
                engine: Some(format!("{} {}", i.engine, i.engine_version)),
                status: Some(i.status.clone()),
                class: Some(i.class.clone()),
                multi_az: Some(i.multi_az),
            });
        }
    }
    out
}

/// Static rows from the personal connection entries, by name. An unknown
/// name is an error for the environment, not a silent gap.
pub fn static_targets(names: &[String], entries: &[Entry]) -> Result<Vec<Target>, String> {
    names
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
        .collect()
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
                        Ok(all) => Ok(rds_targets(env, all)),
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
                probe: env.probe.then(|| ProbeSettings {
                    version_table: env.version_table.clone(),
                }),
            }
        })
        .collect()
}

/// List RDS instances through the aws CLI, checking the account first.
/// Blocking.
pub fn fetch_rds(scope: &AwsScope) -> Result<Vec<rds::RdsInstance>, String> {
    scope.validate()?;
    scope.check_account("RDS instances")?;
    rds::parse_describe(&crate::aws_cli::run(&rds::describe_args(scope))?)
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
                ("demo".into(), "ledgerlog".into(), Role::Primary),
                (
                    "demo-read-only-replica".into(),
                    "ledger".into(),
                    Role::Replica
                ),
                (
                    "demo-read-only-replica".into(),
                    "ledgerlog".into(),
                    Role::Replica
                ),
            ]
        );
        assert_eq!(t[2].replica_of.as_deref(), Some("demo"));
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
        assert!(reports[2].error.as_deref().unwrap().contains("ledger-local"));

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
            secret_of(1),
            "demo/database/ledgerlog/credentials",
            "server/database beats server"
        );
        assert_eq!(secret_of(2), "demo/database/ledger_replica/credentials");
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
        ] {
            assert!(parse_config(&bad).is_err(), "{why}");
        }
        assert!(parse_config(&format!("{rds}sslmode = \"verify-full\"\nuser = \"me\"\n")).is_ok());
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
}
