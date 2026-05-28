//! Project-level configuration: `<repo>/.pgman/pgman.toml`.
//!
//! This file is intended to be committed to git, so a team shares the same
//! list of known data sources and per-database safety rules. Passwords are
//! deliberately not stored here — they come from `PGPASSWORD`, a
//! per-connection `password_env`, or IntelliJ's keychain.
//!
//! Discovery walks up from the current directory looking for a `.pgman/`
//! folder, so `pgman` can be launched from any subdirectory of the project.
//!
//! Pure parsing + merging here; I/O is a thin wrapper at the bottom.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::conn::Dsn;
use crate::safety::{SafetyConfig, SafetyProfile};

/// Top-level shape of `.pgman/pgman.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    /// Named data sources that show up in the startup picker. Origin is
    /// "project" so the operator can tell where each came from.
    #[serde(rename = "connections", default)]
    pub connections: Vec<Connection>,
    pub safety: Option<ProjectSafety>,
}

/// One project-level connection. `url` is a `postgres://` DSN string (no
/// `jdbc:` prefix — pgman is Postgres-only). Passwords are sourced from
/// `password_env` (env var name), falling back to `PGPASSWORD` if neither
/// is set.
#[derive(Debug, Clone, Deserialize)]
pub struct Connection {
    pub name: String,
    pub url: String,
    /// Override the user from the URL. Useful when the URL is shared but
    /// each teammate logs in as themselves.
    #[serde(default)]
    pub user: Option<String>,
    /// Name of an environment variable holding the password. When unset,
    /// pgman falls back to `PGPASSWORD`.
    #[serde(default)]
    pub password_env: Option<String>,
    /// Optional bastion target — `[user@]host[:port]`. When set, pgman
    /// opens an `ssh -L` tunnel before connecting and forwards the
    /// postgres traffic through it. Honours the operator's
    /// `~/.ssh/config` (keys, ProxyCommand, etc.) since we shell out
    /// to the system `ssh` binary. Equivalent to the
    /// `?ssh_tunnel=...` URL param but more discoverable.
    #[serde(default)]
    pub ssh_tunnel: Option<String>,
}

/// The `[safety]` block. Mirrors `safety::SafetyConfig` but makes `default`
/// optional so a project can override per-database rules without restating
/// the team-wide defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectSafety {
    pub default: Option<SafetyProfile>,
    #[serde(default)]
    pub databases: HashMap<String, SafetyProfile>,
}

/// Walk from `start` toward the filesystem root looking for a `.pgman/`
/// directory. Returns the path *containing* it (i.e. the project root).
pub fn find_root(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        if ancestor.join(".pgman").is_dir() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

/// Path to the project config file given a project root.
pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".pgman/pgman.toml")
}

/// Parse a TOML string into a `ProjectConfig`. Returns `Err` on syntax /
/// schema errors so the caller can log them and fall back to defaults.
pub fn parse(toml_text: &str) -> Result<ProjectConfig, String> {
    toml::from_str(toml_text).map_err(|e| e.to_string())
}

/// Resolve a `Connection` into a connectable `Dsn`. Returns `None` when the
/// URL is unparseable.
///
/// Merge rules (most-specific wins):
/// - User: `connection.user` overrides anything in the URL.
/// - Password: `password_env` env var overrides `PGPASSWORD`, which in turn
///   overrides anything in the URL. Empty env vars are treated as unset so
///   `unset FOO` doesn't accidentally blank out a URL-provided password.
pub fn connection_to_dsn(c: &Connection) -> Option<Dsn> {
    let mut dsn = Dsn::parse(&c.url).ok()?;
    if let Some(u) = &c.user {
        if !u.is_empty() {
            dsn.user = Some(u.clone());
        }
    }
    // Precedence (most → least specific): connection.password_env env
    // var, then $PGPASSWORD, then any password embedded in the URL.
    // The env vars beat the URL so an operator can rotate credentials
    // without editing the committed `pgman.toml`.
    let env_pw = c
        .password_env
        .as_deref()
        .and_then(|var| std::env::var(var).ok())
        .filter(|s| !s.is_empty());
    let pg_pw = std::env::var("PGPASSWORD").ok().filter(|s| !s.is_empty());
    if let Some(pw) = env_pw {
        dsn.password = Some(pw);
    } else if let Some(pw) = pg_pw {
        dsn.password = Some(pw);
    }
    // SSH tunnel: project-config field wins over a URL `ssh_tunnel=`
    // param. When the field is set the operator's intent is to control
    // the tunnel from the TOML — so a malformed value clears any URL-
    // derived tunnel too rather than silently falling back to it.
    // (Otherwise a typo in `ssh_tunnel = "bastion:"` would route the
    // operator through whatever obsolete `?ssh_tunnel=…` the URL
    // happened to carry.)
    if let Some(spec) = c.ssh_tunnel.as_deref().filter(|s| !s.is_empty()) {
        match crate::tunnel::SshTunnelSpec::parse(spec) {
            Ok(s) => dsn.ssh_tunnel = Some(s),
            Err(e) => {
                tracing::warn!(
                    "connection {:?} has malformed ssh_tunnel={spec:?}: {e}; \
                     dropping any URL-embedded tunnel as well so the typo is visible",
                    c.name
                );
                dsn.ssh_tunnel = None;
            }
        }
    }
    Some(dsn)
}

/// Fold a project's safety overrides into a global `SafetyConfig`. Project
/// values win on collision; absent project entries leave the global value
/// untouched. So you can commit just `[safety.databases.production]` and
/// keep your personal defaults for everything else.
pub fn merge_safety(global: SafetyConfig, project: Option<&ProjectSafety>) -> SafetyConfig {
    let mut out = global;
    let Some(p) = project else { return out };
    if let Some(d) = &p.default {
        out.default = d.clone();
    }
    for (db, profile) in &p.databases {
        out.databases.insert(db.clone(), profile.clone());
    }
    out
}

/// Locate and load the project config, with structured logging at each step.
/// Returns the parsed config plus the project root so the caller can show
/// the operator where it came from.
pub fn load_from(start: &Path) -> Option<(PathBuf, ProjectConfig)> {
    let root = find_root(start)?;
    let path = config_path(&root);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                "found .pgman/ at {} but couldn't read pgman.toml: {e}",
                root.display()
            );
            return None;
        }
    };
    match parse(&text) {
        Ok(cfg) => {
            tracing::info!(
                "loaded project config from {} ({} connection(s))",
                path.display(),
                cfg.connections.len()
            );
            Some((root, cfg))
        }
        Err(e) => {
            tracing::warn!(
                "project config at {} parse error ({e}); ignoring",
                path.display()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn find_root_walks_up_to_dot_pgman() {
        let tmp = std::env::temp_dir().join(format!("pgman-proj-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let root = tmp.join("project");
        let sub = root.join("a/b/c");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(root.join(".pgman")).unwrap();

        assert_eq!(find_root(&sub).as_deref(), Some(root.as_path()));
        assert_eq!(find_root(&root).as_deref(), Some(root.as_path()));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_root_returns_none_when_no_dot_pgman_above() {
        let tmp = std::env::temp_dir().join(format!("pgman-proj-none-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("plain/sub");
        fs::create_dir_all(&dir).unwrap();
        assert!(find_root(&dir).is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_extracts_connections() {
        let toml = r#"
[[connections]]
name = "local"
url = "postgres://postgres@localhost:5432/myapp"

[[connections]]
name = "staging"
url = "postgres://stg-db.internal:5432/myapp"
user = "app"
password_env = "STAGING_DB_PASSWORD"
"#;
        let cfg = parse(toml).unwrap();
        assert_eq!(cfg.connections.len(), 2);
        assert_eq!(cfg.connections[0].name, "local");
        assert_eq!(cfg.connections[1].user.as_deref(), Some("app"));
        assert_eq!(
            cfg.connections[1].password_env.as_deref(),
            Some("STAGING_DB_PASSWORD")
        );
    }

    #[test]
    fn parse_extracts_safety_overrides() {
        let toml = r#"
[safety.default]
read_only = true
statement_timeout_ms = 30000

[safety.databases.production]
read_only = true
statement_timeout_ms = 5000
"#;
        let cfg = parse(toml).unwrap();
        let safety = cfg.safety.expect("safety section");
        assert!(safety.default.is_some());
        assert!(safety.databases.contains_key("production"));
    }

    #[test]
    fn parse_rejects_malformed_toml() {
        assert!(parse("[[connections\nname = oops").is_err());
    }

    #[test]
    fn connection_to_dsn_applies_user_override() {
        let c = Connection {
            name: "x".into(),
            url: "postgres://localhost/db".into(),
            user: Some("alice".into()),
            password_env: None,
            ssh_tunnel: None,
        };
        let dsn = connection_to_dsn(&c).unwrap();
        assert_eq!(dsn.user.as_deref(), Some("alice"));
    }

    #[test]
    fn connection_to_dsn_rejects_bad_url() {
        let c = Connection {
            name: "x".into(),
            url: "not-a-url".into(),
            user: None,
            password_env: None,
            ssh_tunnel: None,
        };
        assert!(connection_to_dsn(&c).is_none());
    }

    #[test]
    fn connection_to_dsn_applies_ssh_tunnel() {
        let c = Connection {
            name: "via-bastion".into(),
            url: "postgres://db.internal:5432/app".into(),
            user: Some("alice".into()),
            password_env: None,
            ssh_tunnel: Some("tom@bastion.example.com".into()),
        };
        let dsn = connection_to_dsn(&c).unwrap();
        let spec = dsn.ssh_tunnel.expect("ssh_tunnel should be set");
        assert_eq!(spec.user.as_deref(), Some("tom"));
        assert_eq!(spec.host, "bastion.example.com");
    }

    #[test]
    fn connection_to_dsn_drops_malformed_ssh_tunnel_warns_keeps_dsn() {
        let c = Connection {
            name: "x".into(),
            url: "postgres://db/app".into(),
            user: None,
            password_env: None,
            ssh_tunnel: Some("not:a:valid:spec".into()),
        };
        // Malformed tunnel shouldn't fail the DSN — operator may
        // still reach the db directly. We just log a warning.
        let dsn = connection_to_dsn(&c).unwrap();
        assert!(dsn.ssh_tunnel.is_none());
    }

    #[test]
    fn malformed_toml_ssh_tunnel_clears_url_embedded_one() {
        // The contract: setting the TOML field is an explicit
        // override intent; a typo there must NOT silently fall back
        // to a (possibly stale) URL-embedded tunnel.
        let c = Connection {
            name: "x".into(),
            url: "postgres://db/app?ssh_tunnel=old-bastion".into(),
            user: None,
            password_env: None,
            ssh_tunnel: Some("bastion:".into()), // trailing colon → BadPort
        };
        let dsn = connection_to_dsn(&c).unwrap();
        assert!(
            dsn.ssh_tunnel.is_none(),
            "malformed TOML override should clear URL tunnel; got {:?}",
            dsn.ssh_tunnel
        );
    }

    #[test]
    fn merge_safety_with_no_project_returns_global_unchanged() {
        let mut global = SafetyConfig::default();
        global
            .databases
            .insert("prod".into(), SafetyProfile::default());
        let merged = merge_safety(global.clone(), None);
        assert_eq!(merged.databases.len(), global.databases.len());
    }

    #[test]
    fn merge_safety_project_database_overrides_global() {
        let mut global = SafetyConfig::default();
        let mut g_prod = SafetyProfile::default();
        g_prod.statement_timeout_ms = 99_999;
        global.databases.insert("prod".into(), g_prod);

        let mut p_prod = SafetyProfile::default();
        p_prod.statement_timeout_ms = 1_000;
        let project = ProjectSafety {
            default: None,
            databases: {
                let mut m = HashMap::new();
                m.insert("prod".into(), p_prod);
                m
            },
        };
        let merged = merge_safety(global, Some(&project));
        assert_eq!(merged.databases["prod"].statement_timeout_ms, 1_000);
    }

    #[test]
    fn merge_safety_project_default_overrides_global_default() {
        let mut global = SafetyConfig::default();
        global.default.statement_timeout_ms = 11_111;
        let mut p_default = SafetyProfile::default();
        p_default.statement_timeout_ms = 22_222;
        let project = ProjectSafety {
            default: Some(p_default),
            databases: HashMap::new(),
        };
        let merged = merge_safety(global, Some(&project));
        assert_eq!(merged.default.statement_timeout_ms, 22_222);
    }
}
