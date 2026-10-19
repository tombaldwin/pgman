//! The connections an MCP client may name, and why each one it may not
//! is refused.
//!
//! Two sources, two trust levels (Tom, 2026-10-19):
//!
//! - **Personal** — `[[connections]]` in `connections.toml` under
//!   `util::config_dir()`. The operator wrote it, so it is the operator's
//!   choice of host, the same standing as a `--dsn` they typed. This is
//!   where anything that tunnels or carries a secret lives.
//! - **Discovered** — `.pgman/pgman.toml`, Spring `application*` files and
//!   IntelliJ `.idea/dataSources*.xml` in the server's working directory.
//!   They name a host the checkout chose. An MCP client has no keypress to
//!   offer, so they are connectable only when the server was started with
//!   `--project-connections` (batch's `--discovered`, for the same reason),
//!   and one that tunnels is never connectable, flag or not.
//!
//! Everything here is pure over already-loaded inputs so the trust rules
//! are unit-tested; [`load`] is the thin I/O wrapper.

use crate::app::DataSourcePick;
use crate::conn::Dsn;
use crate::project;

/// Where an entry came from. Rendered into `connections` and `doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The operator's own `connections.toml`.
    Personal,
    /// Found in the working tree; the `&str` is the pick's origin
    /// (`project`, `Spring`, `IntelliJ`).
    Discovered(&'static str),
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::Personal => "personal connections.toml".to_string(),
            Source::Discovered(origin) => format!("discovered ({origin})"),
        }
    }
}

/// One named connection. `dsn` is `Some` exactly when `refusal` is `None`
/// — a refused entry is listed so the client can see what exists and
/// why it can't be used, but there is nothing to connect with.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub source: Source,
    pub dsn: Option<Dsn>,
    pub refusal: Option<String>,
    /// Where the password comes from, never the password itself.
    pub password_source: String,
}

/// The parsed personal file plus whatever went wrong reading it.
#[derive(Debug, Default)]
pub struct Personal {
    pub connections: Vec<project::Connection>,
    pub error: Option<String>,
}

#[derive(serde::Deserialize)]
struct PersonalFile {
    #[serde(default)]
    connections: Vec<project::Connection>,
}

/// Parse `connections.toml`. A file that doesn't parse yields no
/// connections and the error — never a partial list.
pub fn parse_personal(text: &str) -> Personal {
    match toml::from_str::<PersonalFile>(text) {
        Ok(f) => Personal {
            connections: f.connections,
            error: None,
        },
        Err(e) => Personal {
            connections: Vec::new(),
            error: Some(format!("connections.toml parse error: {e}")),
        },
    }
}

/// Refusal text for a discovered entry when the server wasn't started
/// with `--project-connections`.
pub const DISCOVERED_REFUSAL: &str = "was found in this checkout, and pgman never connects to a \
     discovered data source without a deliberate act; restart the server with \
     --project-connections to allow it, or copy it into connections.toml";

/// Refusal text for a discovered entry that tunnels.
pub const DISCOVERED_TUNNEL_REFUSAL: &str = "opens an ssh tunnel to a host named by a file in \
     this checkout; tunnelled connections must be defined in connections.toml";

/// Refusal text for a personal entry carrying `${…}`.
pub const PERSONAL_PLACEHOLDER_REFUSAL: &str = "has a ${…} placeholder, which connections.toml \
     does not resolve; name the password's variable with password_env instead";

/// Build the catalogue: personal entries first, then discovered ones.
/// Names are unique — a later entry whose name is taken gets ` #2`,
/// ` #3`, … — so a client can address each one exactly.
pub fn build(
    personal: &[project::Connection],
    discovered: Vec<DataSourcePick>,
    allow_discovered: bool,
) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for c in personal {
        out.push(personal_entry(c));
    }
    for pick in discovered {
        out.push(discovered_entry(pick, allow_discovered));
    }
    let mut seen = std::collections::HashSet::new();
    for e in &mut out {
        if !seen.insert(e.name.clone()) {
            let mut n = 2;
            while !seen.insert(format!("{} #{n}", e.name)) {
                n += 1;
            }
            e.name = format!("{} #{n}", e.name);
        }
    }
    out
}

fn personal_entry(c: &project::Connection) -> Entry {
    let placeholder = c.url.contains("${") || c.user.as_deref().is_some_and(|u| u.contains("${"));
    let (dsn, refusal) = if placeholder {
        (None, Some(PERSONAL_PLACEHOLDER_REFUSAL.to_string()))
    } else {
        match project::connection_to_dsn(c) {
            Some(d) => (Some(d), None),
            None => (
                None,
                Some("has a url that does not parse as a postgres:// DSN".to_string()),
            ),
        }
    };
    let password_source = match c.password_env.as_deref().filter(|v| !v.is_empty()) {
        Some(var) if std::env::var(var).is_ok_and(|v| !v.is_empty()) => format!("env {var} (set)"),
        Some(var) => format!("env {var} (NOT set in the server's environment)"),
        None if dsn.as_ref().is_some_and(|d| d.password.is_some()) => "in the url".to_string(),
        None => "none configured".to_string(),
    };
    Entry {
        name: c.name.clone(),
        source: Source::Personal,
        dsn,
        refusal,
        password_source,
    }
}

fn discovered_entry(pick: DataSourcePick, allow_discovered: bool) -> Entry {
    let mut password_source = if pick.creds.password_env.is_empty() {
        "none".to_string()
    } else {
        format!("env {}", pick.creds.password_env.join(", "))
    };
    if pick.creds.password_env.is_empty() && pick.dsn.as_ref().is_some_and(|d| d.password.is_some())
    {
        password_source = format!("in the {} config file", pick.origin);
    }
    // Order matters only for which reason is shown: a tunnel is refused
    // whatever the flag says, so it outranks the flag's refusal.
    let refusal = if pick.dsn.as_ref().is_some_and(|d| d.ssh_tunnel.is_some()) {
        Some(DISCOVERED_TUNNEL_REFUSAL.to_string())
    } else if !pick.unresolved_host.is_empty() {
        Some(format!(
            "has a ${{…}} placeholder in its host ({}), which is never resolved for a \
             discovered source",
            pick.unresolved_host.join(", ")
        ))
    } else if !pick.unresolved.is_empty() {
        Some(format!(
            "has unresolved placeholders ({}); export them in the server's environment",
            pick.unresolved.join(", ")
        ))
    } else if pick.dsn.is_none() {
        Some("has a url that does not parse as a postgres:// DSN".to_string())
    } else if !allow_discovered {
        Some(DISCOVERED_REFUSAL.to_string())
    } else {
        None
    };
    Entry {
        name: pick.name,
        source: Source::Discovered(pick.origin),
        dsn: if refusal.is_none() { pick.dsn } else { None },
        refusal,
        password_source,
    }
}

/// Names of other entries that reach the same database as `entry` — same
/// host, port, database and user. A checkout often declares one database
/// several times (`dataSource` and `replicaDataSource` both on `ledger`);
/// saying so stops a client treating them as independent.
pub fn same_target<'a>(entry: &Entry, all: &'a [Entry]) -> Vec<&'a str> {
    let key = |e: &Entry| {
        e.dsn
            .as_ref()
            .map(|d| (d.host.clone(), d.port, d.dbname.clone(), d.user.clone()))
    };
    let Some(mine) = key(entry) else {
        return Vec::new();
    };
    all.iter()
        .filter(|o| o.name != entry.name && key(o).as_ref() == Some(&mine))
        .map(|o| o.name.as_str())
        .collect()
}

/// Load both sources from disk: the personal file under
/// `util::config_dir()` and discovery rooted at `cwd`. Read fresh on
/// every call, so an edit to either takes effect without a restart.
pub fn load(cwd: &std::path::Path, allow_discovered: bool) -> (Vec<Entry>, Personal) {
    let path = crate::util::config_file("connections.toml");
    let personal = match std::fs::read_to_string(&path) {
        Ok(text) => parse_personal(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Personal::default(),
        Err(e) => Personal {
            connections: Vec::new(),
            error: Some(format!("could not read {}: {e}", path.display())),
        },
    };
    let mut picks: Vec<DataSourcePick> = Vec::new();
    if let Some((_, cfg)) = project::load_from(cwd) {
        for c in &cfg.connections {
            if let Some(pick) = crate::discover::project_connection_pick(c) {
                picks.push(pick);
            }
        }
    }
    if crate::creds::spring::detect_java_project(cwd) {
        crate::discover::discover_spring_datasources(cwd, &mut picks);
    }
    if crate::creds::intellij::detect_intellij_project(cwd) {
        crate::discover::discover_intellij_datasources(cwd, &mut picks);
    }
    let entries = build(&personal.connections, picks, allow_discovered);
    (entries, personal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CredsProvenance;

    fn conn(name: &str, url: &str) -> project::Connection {
        project::Connection {
            name: name.to_string(),
            url: url.to_string(),
            user: None,
            password_env: None,
            ssh_tunnel: None,
        }
    }

    fn pick(name: &str, url: &str) -> DataSourcePick {
        DataSourcePick {
            name: name.to_string(),
            origin: "Spring",
            dsn: Some(Dsn::parse(url).expect("fixture url parses")),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: CredsProvenance::default(),
        }
    }

    #[test]
    fn a_personal_connection_is_connectable_tunnel_and_all() {
        let mut c = conn("demo", "postgres://u@db.internal:5432/ledger");
        c.ssh_tunnel = Some("ec2-user@jump".to_string());
        let entries = build(&[c], Vec::new(), false);
        assert_eq!(entries[0].refusal, None);
        let dsn = entries[0].dsn.as_ref().expect("connectable");
        assert!(
            dsn.ssh_tunnel.is_some(),
            "the operator's own tunnel is kept"
        );
        assert_eq!(entries[0].source, Source::Personal);
    }

    #[test]
    fn a_personal_placeholder_is_refused_rather_than_sent_literally() {
        let entries = build(
            &[conn("p", "postgres://u:${PW}@localhost/db")],
            Vec::new(),
            true,
        );
        assert_eq!(
            entries[0].refusal.as_deref(),
            Some(PERSONAL_PLACEHOLDER_REFUSAL)
        );
        assert!(entries[0].dsn.is_none());
    }

    #[test]
    fn a_discovered_source_needs_the_flag() {
        let refused = build(
            &[],
            vec![pick("dataSource (app)", "postgres://ledger@localhost/ledger")],
            false,
        );
        assert_eq!(refused[0].refusal.as_deref(), Some(DISCOVERED_REFUSAL));
        assert!(refused[0].dsn.is_none(), "nothing to connect with");

        let allowed = build(
            &[],
            vec![pick("dataSource (app)", "postgres://ledger@localhost/ledger")],
            true,
        );
        assert_eq!(allowed[0].refusal, None);
        assert!(allowed[0].dsn.is_some());
    }

    #[test]
    fn a_discovered_tunnel_is_refused_even_with_the_flag() {
        let p = pick(
            "proj",
            "postgres://u@rds.example/db?ssh_tunnel=ec2-user@jump.example",
        );
        assert!(
            p.dsn.as_ref().unwrap().ssh_tunnel.is_some(),
            "fixture tunnels"
        );
        let entries = build(&[], vec![p], true);
        assert_eq!(
            entries[0].refusal.as_deref(),
            Some(DISCOVERED_TUNNEL_REFUSAL)
        );
        assert!(entries[0].dsn.is_none());
    }

    #[test]
    fn a_discovered_unresolved_placeholder_is_refused_even_with_the_flag() {
        let mut p = pick("s", "postgres://u@localhost/db");
        p.unresolved = vec!["DB_PASSWORD".to_string()];
        let entries = build(&[], vec![p], true);
        assert!(entries[0]
            .refusal
            .as_deref()
            .unwrap()
            .contains("DB_PASSWORD"));

        let mut p = pick("h", "postgres://u@localhost/db");
        p.unresolved_host = vec!["DB_HOST".to_string()];
        let entries = build(&[], vec![p], true);
        assert!(entries[0].refusal.as_deref().unwrap().contains("DB_HOST"));
    }

    #[test]
    fn names_are_made_unique_with_personal_entries_keeping_theirs() {
        let entries = build(
            &[conn("ledger", "postgres://a@localhost/ledger")],
            vec![
                pick("ledger", "postgres://b@localhost/ledger"),
                pick("ledger", "postgres://c@localhost/ledger"),
            ],
            true,
        );
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["ledger", "ledger #2", "ledger #3"]);
        assert_eq!(entries[0].source, Source::Personal);
    }

    #[test]
    fn same_target_links_entries_that_reach_one_database() {
        let entries = build(
            &[],
            vec![
                pick("dataSource", "postgres://ledger@localhost:5432/ledger"),
                pick("replicaDataSource", "postgres://ledger@localhost:5432/ledger"),
                pick("logDataSource", "postgres://ledger@localhost:5432/ledgerlog"),
            ],
            true,
        );
        assert_eq!(same_target(&entries[0], &entries), ["replicaDataSource"]);
        assert!(same_target(&entries[2], &entries).is_empty());
    }

    #[test]
    fn the_password_is_described_never_carried_into_the_source_text() {
        let entries = build(
            &[conn("u", "postgres://u:hunter2@localhost/db")],
            vec![pick("s", "postgres://u:sekrit@localhost/db")],
            true,
        );
        assert_eq!(entries[0].password_source, "in the url");
        assert_eq!(entries[1].password_source, "in the Spring config file");
        for e in &entries {
            assert!(!e.password_source.contains("hunter2"));
            assert!(!e.password_source.contains("sekrit"));
        }
    }

    #[test]
    fn a_personal_file_that_does_not_parse_yields_nothing_and_says_so() {
        let p = parse_personal("[[connections]]\nname = 1\n");
        assert!(p.connections.is_empty());
        assert!(p.error.as_deref().unwrap().contains("connections.toml"));

        let p = parse_personal(
            "[[connections]]\nname = \"local\"\nurl = \"postgres://ledger@localhost/ledger\"\n",
        );
        assert_eq!(p.error, None);
        assert_eq!(p.connections[0].name, "local");
        assert!(parse_personal("").connections.is_empty());
    }
}
