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
//!
//! Environments live in `environments.toml` under `util::config_dir()`,
//! never a project file: like `connections.toml`, they name hosts and
//! accounts the operator chose.

pub mod rds;

use crate::aws_cli::AwsScope;
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
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::Static { .. } => "connections.toml".to_string(),
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
            Source::Static { .. } => None,
        }
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
}

/// An environment's rows, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvReport {
    pub name: String,
    pub source: String,
    pub targets: Vec<Target>,
    pub error: Option<String>,
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
) -> Vec<EnvReport> {
    let mut cache: HashMap<AwsScope, Result<Vec<rds::RdsInstance>, String>> = HashMap::new();
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
    Ok(gather(&envs, &entries, fetch_rds))
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
        };
        let t = rds_targets(&env, &all());
        assert_eq!(t[0].role, Role::Direct);
        assert_eq!(t[0].status.as_deref(), Some("stopped"));
    }

    #[test]
    fn gather_lists_once_per_scope_and_reports_errors_per_environment() {
        let envs = parse_config(CONFIG).unwrap();
        let mut calls = 0;
        let reports = gather(&envs, &[], |_| {
            calls += 1;
            Ok(all())
        });
        assert_eq!(calls, 1, "demo and prod share a scope");
        assert_eq!(reports[0].targets.len(), 4);
        assert_eq!(reports[1].targets.len(), 2);
        assert!(reports[2].error.as_deref().unwrap().contains("ledger-local"));

        let reports = gather(&envs[..1], &[], |_| {
            Err("aws sts failed: expired".to_string())
        });
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
}
