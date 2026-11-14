//! `pgman doctor`: what pgman sees from the directory it runs in — its
//! config files and whether they load, the programs it runs, and the
//! connections it could use and why not — for a person stuck on "why
//! won't it connect, dump or discover". MCP's `doctor` tool is the
//! agent's view of the same.
//!
//! [`gather`] reads; [`render`] and [`broken`] are pure.

use std::path::{Path, PathBuf};

/// `pgman doctor --help`.
pub const USAGE: &str = "\
usage: pgman doctor

What pgman sees from this directory: each config file it reads and
whether it loads, the programs it runs (pg_dump, pg_restore, aws, ssh,
git, docker) and where they are, and every connection it knows — from
your connections.toml and found in the project — usable or why not.
Never a password: where one comes from. Connects to nothing.

Exits 1 when a config file doesn't load.";

/// One config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub label: &'static str,
    pub path: PathBuf,
    pub state: FileState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Missing,
    /// Loaded; what it holds, in a few words.
    Loaded(String),
    /// Loaded with something that makes a setting go unused.
    Warning(String),
    /// Doesn't load: why.
    Broken(String),
}

/// One program pgman runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static str,
    pub for_what: &'static str,
    /// Where it is (and its version, when pgman reads one), or None.
    pub found: Option<String>,
}

/// One connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conn {
    pub name: String,
    pub source: String,
    /// The target (password-free) and where its password comes from, or
    /// why it can't be used.
    pub usable: Result<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub version: String,
    pub cwd: PathBuf,
    pub files: Vec<File>,
    pub tools: Vec<Tool>,
    pub conns: Vec<Conn>,
    pub logs: PathBuf,
}

/// What `pgman doctor` reports from `cwd`. Reads the config files and
/// PATH, and runs `pg_dump --version` and `pg_restore --version`.
pub fn gather(cwd: &Path) -> Report {
    let file = |label: &'static str, path: PathBuf, state: FileState| File { label, path, state };
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("can't be read: {e}")),
    };
    let mut files = Vec::new();

    let path = crate::util::config_file("safety.toml");
    let state = match read(&path) {
        Err(e) => FileState::Broken(e),
        Ok(None) => FileState::Missing,
        // Its warning is a parse error or keys that match nothing.
        Ok(Some(text)) => match crate::safety::SafetyConfig::parse_personal(&text) {
            (cfg, None) => {
                FileState::Loaded(format!("{} database profile(s)", cfg.databases.len()))
            }
            (_, Some(w)) if toml::from_str::<crate::safety::SafetyConfig>(&text).is_err() => {
                FileState::Broken(w)
            }
            (_, Some(w)) => FileState::Warning(w),
        },
    };
    files.push(file("safety.toml", path, state));

    let path = crate::util::config_file("connections.toml");
    let state = match read(&path) {
        Err(e) => FileState::Broken(e),
        Ok(None) => FileState::Missing,
        Ok(Some(text)) => {
            let p = crate::mcp::connections::parse_personal(&text);
            match p.error {
                Some(e) => FileState::Broken(e),
                None => FileState::Loaded(format!("{} connection(s)", p.connections.len())),
            }
        }
    };
    files.push(file("connections.toml", path, state));

    let loaded = crate::inventory::load();
    let state = match (&loaded.environments, loaded.path.exists()) {
        (Err(e), _) => FileState::Broken(e.clone()),
        (Ok(_), false) => FileState::Missing,
        (Ok(envs), true) => FileState::Loaded(format!("{} environment(s)", envs.len())),
    };
    files.push(file("environments.toml", loaded.path, state));

    match crate::project::find_root(cwd) {
        Some(root) => {
            let path = crate::project::config_path(&root);
            let state = match read(&path) {
                Err(e) => FileState::Broken(e),
                Ok(None) => FileState::Missing,
                Ok(Some(text)) => match crate::project::parse(&text) {
                    Ok(cfg) => FileState::Loaded(format!(
                        "{} connection(s){}",
                        cfg.connections.len(),
                        if cfg.safety.is_some() {
                            ", a [safety] block"
                        } else {
                            ""
                        }
                    )),
                    Err(e) => FileState::Broken(e),
                },
            };
            files.push(file(".pgman/pgman.toml", path, state));
        }
        None => files.push(file(
            ".pgman/pgman.toml",
            cwd.join(".pgman/pgman.toml"),
            FileState::Missing,
        )),
    }

    let pg = |name: &'static str, for_what: &'static str| Tool {
        name,
        for_what,
        found: crate::pgtool::find_tool_from(name, None)
            .ok()
            .map(|(p, from)| match crate::pgtool::tool_version(&p) {
                Ok(v) => format!("{} ({v}, {from})", p.display()),
                Err(_) => format!("{} ({from})", p.display()),
            }),
    };
    let on_path = |name: &'static str, for_what: &'static str| Tool {
        name,
        for_what,
        found: std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join(name))
                .find(|p| p.is_file())
                .map(|p| p.display().to_string())
        }),
    };
    let tools = vec![
        pg("pg_dump", "pgman dump"),
        pg("pg_restore", "pgman restore"),
        on_path("aws", "secrets, RDS environments and logs, KMS"),
        on_path("ssh", "tunnels"),
        on_path("git", "a dump's ticket from the branch, masking rules"),
        on_path("docker", "docker environments"),
    ];

    let (entries, _) = crate::mcp::connections::load(cwd, true);
    let conns = entries
        .iter()
        .map(|e| Conn {
            name: e.name.clone(),
            source: e.source.label(),
            usable: usable(e),
        })
        .collect();

    Report {
        version: env!("CARGO_PKG_VERSION").to_string(),
        cwd: cwd.to_path_buf(),
        files,
        tools,
        conns,
        logs: crate::util::cache_dir(),
    }
}

/// Whether a config file doesn't load: `pgman doctor` exits 1. Pure.
pub fn broken(r: &Report) -> bool {
    r.files
        .iter()
        .any(|f| matches!(f.state, FileState::Broken(_)))
}

/// The report as `pgman doctor` prints it. Pure.
pub fn render(r: &Report) -> String {
    let mut out = format!(
        "pgman {} in {}\n\nconfig files\n",
        r.version,
        r.cwd.display()
    );
    for f in &r.files {
        let (mark, what) = match &f.state {
            FileState::Missing => ("-", "not there".to_string()),
            FileState::Loaded(s) => ("ok", s.clone()),
            FileState::Warning(w) => ("!", w.clone()),
            FileState::Broken(e) => ("BROKEN", e.clone()),
        };
        // A parse error runs over lines: each under the first.
        out.push_str(&format!(
            "  {mark:<6} {:<18} {}\n         {}\n",
            f.label,
            f.path.display(),
            what.trim_end().replace('\n', "\n         ")
        ));
    }
    out.push_str("\nprograms\n");
    for t in &r.tools {
        match &t.found {
            Some(at) => out.push_str(&format!("  ok     {:<10} {at}\n", t.name)),
            None => out.push_str(&format!(
                "  -      {:<10} not found (needed for {})\n",
                t.name, t.for_what
            )),
        }
    }
    out.push_str("\nconnections\n");
    if r.conns.is_empty() {
        out.push_str(
            "  none: add [[connections]] to connections.toml or .pgman/pgman.toml, or pass --dsn\n",
        );
    }
    for c in &r.conns {
        match &c.usable {
            Ok(what) => out.push_str(&format!(
                "  ok     {} ({})\n         {what}\n",
                c.name, c.source
            )),
            Err(why) => out.push_str(&format!(
                "  no     {} ({})\n         {why}\n",
                c.name, c.source
            )),
        }
    }
    out.push_str(&format!("\nlogs: {}\n", r.logs.display()));
    out
}

/// Whether `e` can connect as configured, and how, or why not: a
/// `password_env` that isn't set leaves the connect with no password.
/// Pure.
fn usable(e: &crate::mcp::connections::Entry) -> Result<String, String> {
    match (&e.refusal, &e.dsn) {
        (Some(why), _) => Err(why.clone()),
        (None, Some(dsn)) => match &e.password_unset {
            Some(var) => Err(format!(
                "{} · password_env names ${var}, which is not set in pgman's environment",
                dsn.redacted()
            )),
            None => Ok(format!(
                "{} · password {}",
                dsn.redacted(),
                e.password_source
            )),
        },
        (None, None) => Err("no connection settings".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection whose password_env isn't set is not usable: the
    /// connect would carry no password.
    #[test]
    fn an_unset_password_env_is_not_usable() {
        let mut e = crate::mcp::connections::Entry {
            name: "trial".into(),
            source: crate::mcp::connections::Source::Personal,
            dsn: crate::conn::Dsn::parse("postgres://app@127.0.0.1:55432/ledger").ok(),
            refusal: None,
            password_source: "env TRIAL_PW (NOT set in pgman's environment)".into(),
            password_unset: Some("TRIAL_PW".into()),
            secret: None,
            role: Default::default(),
        };
        let why = usable(&e).unwrap_err();
        assert!(
            why.contains("password_env names $TRIAL_PW, which is not set"),
            "{why}"
        );
        e.password_unset = None;
        e.password_source = "env TRIAL_PW (set)".into();
        assert!(usable(&e).is_ok());
    }

    fn report() -> Report {
        Report {
            version: "0.5.3".into(),
            cwd: "/w/app".into(),
            files: vec![
                File {
                    label: "safety.toml",
                    path: "/c/pgman/safety.toml".into(),
                    state: FileState::Missing,
                },
                File {
                    label: "connections.toml",
                    path: "/c/pgman/connections.toml".into(),
                    state: FileState::Loaded("2 connection(s)".into()),
                },
            ],
            tools: vec![
                Tool {
                    name: "pg_dump",
                    for_what: "pgman dump",
                    found: Some("/usr/bin/pg_dump (17, PATH)".into()),
                },
                Tool {
                    name: "aws",
                    for_what: "secrets",
                    found: None,
                },
            ],
            conns: vec![
                Conn {
                    name: "local".into(),
                    source: "personal connections.toml".into(),
                    usable: Ok(
                        "postgres://me@localhost:5432/app · password env LOCAL_PW (set)".into(),
                    ),
                },
                Conn {
                    name: "spring".into(),
                    source: "discovered (Spring)".into(),
                    usable: Err("unresolved ${DB_PASSWORD}".into()),
                },
            ],
            logs: "/k/pgman".into(),
        }
    }

    #[test]
    fn the_report_says_what_loads_what_is_missing_and_why_not() {
        let r = report();
        let text = render(&r);
        for want in [
            "pgman 0.5.3 in /w/app",
            "-      safety.toml",
            "not there",
            "ok     connections.toml",
            "2 connection(s)",
            "ok     pg_dump    /usr/bin/pg_dump (17, PATH)",
            "-      aws        not found (needed for secrets)",
            "ok     local (personal connections.toml)",
            "no     spring (discovered (Spring))",
            "unresolved ${DB_PASSWORD}",
            "logs: /k/pgman",
        ] {
            assert!(text.contains(want), "{want:?} in\n{text}");
        }
        assert!(!broken(&r));
        let mut r = r;
        r.files[0].state = FileState::Broken("parse error at line 1\n  |\n1 | [default\n".into());
        assert!(broken(&r));
        let text = render(&r);
        assert!(text.contains("BROKEN safety.toml"), "{text}");
        assert!(
            text.contains("parse error at line 1\n           |\n         1 | [default\n  ok"),
            "each line of the error indented, none left over: {text}"
        );
        r.conns.clear();
        assert!(render(&r).contains("none: add [[connections]]"));
    }

    #[test]
    fn a_warning_is_not_broken() {
        let mut r = report();
        r.files[0].state = FileState::Warning("[databases.x] matches nothing".into());
        assert!(!broken(&r));
        assert!(render(&r).contains("!      safety.toml"));
    }
}
