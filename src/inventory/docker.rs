//! The `docker` inventory source: local Postgres containers, from
//! `docker ps --all`. A container counts when it exposes 5432/tcp or
//! runs a `postgres` image; it is reached on the host port that 5432 is
//! published to.

use super::{ConnectSpec, Environment, Source, Target};
use crate::conn::Dsn;
use crate::mcp::connections::Role;

/// One container, as far as the overview goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub name: String,
    pub image: String,
    /// `running`, `exited`, …
    pub state: String,
    /// Where 5432 is published on the host, when it is.
    pub published: Option<(String, u16)>,
    pub exposes_postgres: bool,
}

/// `docker ps --all`, one JSON object per line.
pub fn ps_args() -> Vec<String> {
    ["ps", "--all", "--no-trunc", "--format", "{{json .}}"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Where `5432/tcp` is published in a `Ports` string such as
/// `0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp`: the first IPv4 (or
/// hostname) mapping, a wildcard address read as `127.0.0.1`. Pure.
pub fn published_5432(ports: &str) -> Option<(String, u16)> {
    ports.split(',').map(str::trim).find_map(|m| {
        let (host_side, container) = m.split_once("->")?;
        if container != "5432/tcp" || host_side.starts_with('[') {
            return None;
        }
        let (addr, port) = host_side.rsplit_once(':')?;
        let port: u16 = port.parse().ok()?;
        let addr = match addr {
            "" | "0.0.0.0" => "127.0.0.1",
            a => a,
        };
        Some((addr.to_string(), port))
    })
}

/// `docker ps` lines to containers that look like Postgres. A line that
/// isn't an object is an error: the CLI's output format changed.
pub fn parse_ps(out: &str) -> Result<Vec<Container>, String> {
    let mut all = Vec::new();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("docker ps returned unreadable JSON: {e}"))?;
        let s = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let ports = s("Ports");
        let image = s("Image");
        let exposes = ports.contains("5432/tcp");
        let postgres_image = image == "postgres" || image.starts_with("postgres:");
        if !exposes && !postgres_image {
            continue;
        }
        all.push(Container {
            name: s("Names"),
            published: published_5432(&ports),
            exposes_postgres: exposes,
            state: s("State"),
            image,
        });
    }
    Ok(all)
}

/// `postgres:16.4` reads as `postgres 16.4`; any other image as itself.
fn engine(image: &str) -> String {
    match image.split_once(':') {
        Some(("postgres", tag)) => format!("postgres {tag}"),
        _ => image.to_string(),
    }
}

/// The rows for a docker environment: matching containers, one row per
/// database (`databases`, default `postgres`). Pure.
pub fn docker_targets(env: &Environment, all: &[Container]) -> Vec<Target> {
    let Source::Docker { containers } = &env.source else {
        return Vec::new();
    };
    let dbs: Vec<String> = if env.databases.is_empty() {
        vec!["postgres".to_string()]
    } else {
        env.databases.clone()
    };
    let mut out = Vec::new();
    let mut chosen: Vec<&Container> = all
        .iter()
        .filter(|c| {
            containers
                .iter()
                .any(|p| super::rds::glob_match(p, &c.name))
        })
        .collect();
    chosen.sort_by(|a, b| a.name.cmp(&b.name));
    for c in chosen {
        for db in &dbs {
            let connect = if c.state != "running" {
                Err(format!("container {} is {}", c.name, c.state))
            } else {
                match &c.published {
                    None => Err(format!("{} doesn't publish 5432 to the host", c.name)),
                    Some((host, port)) => Ok(ConnectSpec {
                        dsn: Dsn {
                            host: host.clone(),
                            port: *port,
                            user: env.user.clone(),
                            password: None,
                            dbname: db.clone(),
                            params: env
                                .sslmode
                                .iter()
                                .map(|m| ("sslmode".to_string(), m.clone()))
                                .collect(),
                            ssh_tunnel: None,
                        },
                        secret: None,
                        password_env: env.password_env.clone(),
                    }),
                }
            };
            out.push(Target {
                database: db.clone(),
                server: c.name.clone(),
                role: Role::Direct,
                replica_of: None,
                host: c.published.as_ref().map(|(h, _)| h.clone()),
                port: c.published.as_ref().map(|(_, p)| *p),
                engine: Some(engine(&c.image)),
                status: Some(c.state.clone()),
                class: None,
                multi_az: None,
                connect,
            });
        }
    }
    out
}

/// List containers through the docker CLI. Blocking.
pub fn fetch() -> Result<Vec<Container>, String> {
    let out = std::process::Command::new("docker")
        .args(ps_args())
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run docker ({e}); is it installed and on PATH?"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err: String = err.trim().chars().take(300).collect();
        return Err(format!("docker ps failed: {err}"));
    }
    parse_ps(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `docker ps` gave on 2026-10-19, trimmed.
    const PS: &str = r#"{"Names":"pgman-postgres-1","Image":"postgres:16","Ports":"127.0.0.1:55432->5432/tcp","State":"running"}
{"Names":"ledger-db","Image":"ledger-db","Ports":"0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp","State":"running"}
{"Names":"redis","Image":"redis:7.0-alpine","Ports":"0.0.0.0:6379->6379/tcp","State":"running"}
{"Names":"old-pg","Image":"postgres:15","Ports":"","State":"exited"}
{"Names":"internal-pg","Image":"custom","Ports":"5432/tcp","State":"running"}
"#;

    fn env(containers: &[&str], databases: &[&str]) -> Environment {
        Environment {
            name: "local".into(),
            source: Source::Docker {
                containers: containers.iter().map(|s| s.to_string()).collect(),
            },
            databases: databases.iter().map(|s| s.to_string()).collect(),
            ssh_tunnel: None,
            sslmode: None,
            user: Some("app".into()),
            password_env: None,
            secrets: Default::default(),
            probe: false,
            version_table: None,
        }
    }

    #[test]
    fn ports_read_the_published_5432() {
        assert_eq!(
            published_5432("0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp"),
            Some(("127.0.0.1".into(), 5432))
        );
        assert_eq!(
            published_5432("127.0.0.1:55432->5432/tcp"),
            Some(("127.0.0.1".into(), 55432))
        );
        assert_eq!(
            published_5432("[::]:5432->5432/tcp, 10.0.0.2:6000->5432/tcp"),
            Some(("10.0.0.2".into(), 6000)),
            "IPv6 skipped, a specific address kept"
        );
        assert_eq!(published_5432("0.0.0.0:6379->6379/tcp"), None);
        assert_eq!(published_5432("5432/tcp"), None, "exposed, not published");
        assert_eq!(published_5432(""), None);
    }

    #[test]
    fn postgres_containers_are_found_by_port_or_image() {
        let all = parse_ps(PS).unwrap();
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["pgman-postgres-1", "ledger-db", "old-pg", "internal-pg"],
            "no redis"
        );
        assert!(parse_ps("not json").is_err());
        assert!(parse_ps("").unwrap().is_empty());
    }

    #[test]
    fn rows_connect_through_the_published_port_or_say_why_not() {
        let all = parse_ps(PS).unwrap();
        let t = docker_targets(&env(&["*"], &["ledger"]), &all);
        let by = |s: &str| t.iter().find(|x| x.server == s).unwrap();
        let ledger = by("ledger-db");
        let dsn = &ledger.connect.as_ref().unwrap().dsn;
        assert_eq!(
            (
                dsn.host.as_str(),
                dsn.port,
                dsn.dbname.as_str(),
                dsn.user.as_deref()
            ),
            ("127.0.0.1", 5432, "ledger", Some("app"))
        );
        assert_eq!(ledger.engine.as_deref(), Some("ledger-db"));
        let mut with_pw = env(&["*"], &["ledger"]);
        with_pw.password_env = Some("PGPASSWORD_LEDGER".into());
        let t2 = docker_targets(&with_pw, &all);
        let ledger2 = t2.iter().find(|x| x.server == "ledger-db").unwrap();
        assert_eq!(
            ledger2.connect.as_ref().unwrap().password_env.as_deref(),
            Some("PGPASSWORD_LEDGER")
        );
        assert_eq!(
            by("pgman-postgres-1").engine.as_deref(),
            Some("postgres 16")
        );
        assert_eq!(
            by("old-pg").connect,
            Err("container old-pg is exited".into())
        );
        assert!(by("internal-pg")
            .connect
            .as_ref()
            .unwrap_err()
            .contains("doesn't publish"));
        // Globs pick containers; no databases means `postgres`.
        let t = docker_targets(&env(&["ledger-*"], &[]), &all);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].database, "postgres");
    }
}
