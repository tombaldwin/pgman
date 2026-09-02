//! Project-level configuration: `<repo>/.pgman/pgman.toml`.
//!
//! This file is intended to be committed to git, so a team shares the same
//! list of known data sources and per-database safety rules. Passwords are
//! deliberately not stored here — they come from the variable a connection's
//! `password_env` names. `$PGPASSWORD` is *not* consulted for anything
//! discovered from the working tree; see `connection_to_dsn`.
//!
//! Discovery walks up from the current directory looking for a `.pgman/`
//! folder, so `pgman` can be launched from any subdirectory of the project.
//!
//! Pure parsing + merging here; I/O is a thin wrapper at the bottom.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::conn::Dsn;
use crate::safety::{Guard, Guards, SafetyConfig, SafetyProfile};

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
/// `jdbc:` prefix — pgman is Postgres-only). The password comes from the
/// variable `password_env` names, or from the URL; there is no
/// `PGPASSWORD` fallback (see `connection_to_dsn`).
#[derive(Debug, Clone, Deserialize)]
pub struct Connection {
    pub name: String,
    pub url: String,
    /// Override the user from the URL. Useful when the URL is shared but
    /// each teammate logs in as themselves.
    #[serde(default)]
    pub user: Option<String>,
    /// Name of an environment variable holding the password. When unset
    /// (or the variable is empty) the connection uses whatever password
    /// the URL carries, and otherwise none at all.
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
/// - Password: the variable named by `password_env` overrides anything in
///   the URL. An empty env var is treated as unset so `unset FOO` doesn't
///   accidentally blank out a URL-provided password.
///
/// **`$PGPASSWORD` is deliberately not consulted here.** This URL came
/// out of a file in the working tree, so whoever wrote the checkout
/// chose the host; lending it the operator's `$PGPASSWORD` sends that
/// password to that host. Cloning a repo and running pgman in it was
/// enough to do exactly that. `PGPASSWORD` is now only applied to a
/// `--dsn` the operator typed (`main.rs::apply_pgpassword`). A project
/// connection with no `password_env` and no password in the URL simply
/// connects without one — trust/peer auth accepts that, and anything
/// else fails with the existing "server demands a password" hint.
pub fn connection_to_dsn(c: &Connection) -> Option<Dsn> {
    let mut dsn = Dsn::parse(&c.url).ok()?;
    if let Some(u) = &c.user {
        if !u.is_empty() {
            dsn.user = Some(u.clone());
        }
    }
    // `password_env` names a variable the *project file* chose, but the
    // operator has to have exported it — an explicit act naming this
    // connection — so it stays honoured.
    let env_pw = c
        .password_env
        .as_deref()
        .and_then(|var| std::env::var(var).ok())
        .filter(|s| !s.is_empty());
    if let Some(pw) = env_pw {
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

/// Fold a project's safety overrides into the operator's personal
/// `SafetyConfig`. **Overrides can only tighten.**
///
/// `.pgman/pgman.toml` is committed to the repo, so its contents are
/// chosen by whoever wrote the checkout — not by the operator running
/// pgman inside it. A project that could *replace* the personal
/// profile could ship `read_only = false` plus `drop = "allow"` and
/// quietly disarm every guard rail. So for each field the merged value
/// is the more restrictive of the two:
///
/// - `read_only` / `auto_tx`: `personal || project` (on is stricter).
/// - `statement_timeout_ms` / `cost_preview_threshold_rows`: the
///   smaller non-zero value (`0` means "no limit", the weakest).
/// - each guard: the stricter of `Allow < Confirm < Block`.
/// - `clean_mode`: not a guard rail and not orderable, so the personal
///   value always stands (a project override there is ignored).
///
/// A project that tries to relax something is not an error — the
/// looser value is ignored, with a `tracing::info!` naming the field so
/// the operator can see what the repo asked for.
///
/// Note that `ProjectSafety` deserialises through `SafetyProfile`'s
/// serde defaults, so a field the project file omits arrives as
/// pgman's *default* value, not as "unset" — and pgman's defaults are
/// strict. A project `[safety]` block therefore also re-tightens
/// anything the operator personally relaxed. That is the safe
/// direction, and it is the documented behaviour (see
/// `docs/configuration.md`).
pub fn merge_safety(global: SafetyConfig, project: Option<&ProjectSafety>) -> SafetyConfig {
    let mut out = global;
    let Some(p) = project else { return out };
    if let Some(d) = &p.default {
        out.default = tighten_profile(&out.default, d, "safety.default");
    }
    for (db, profile) in &p.databases {
        // The base for a database the personal config doesn't mention
        // is the (already-tightened) personal default — otherwise a
        // project could relax a database simply by naming it.
        let base = out
            .databases
            .get(db)
            .cloned()
            .unwrap_or_else(|| out.default.clone());
        let merged = tighten_profile(&base, profile, &format!("safety.databases.{db}"));
        out.databases.insert(db.clone(), merged);
    }
    out
}

/// How strict a guard is. `Allow < Confirm < Block`.
fn guard_rank(g: Guard) -> u8 {
    match g {
        Guard::Allow => 0,
        Guard::Confirm => 1,
        Guard::Block => 2,
    }
}

/// The stricter of two guards; logs when the project's is the looser one.
fn stricter_guard(personal: Guard, project: Guard, scope: &str, field: &str) -> Guard {
    match guard_rank(project).cmp(&guard_rank(personal)) {
        std::cmp::Ordering::Greater => project,
        std::cmp::Ordering::Less => {
            tracing::info!(
                "project {scope}.guards.{field} = {project:?} is looser than your \
                 {personal:?}; ignoring (project overrides can only tighten)"
            );
            personal
        }
        std::cmp::Ordering::Equal => personal,
    }
}

/// `true` is the strict setting for these booleans, so the merged value
/// is `personal || project`.
fn stricter_flag(personal: bool, project: bool, scope: &str, field: &str) -> bool {
    if personal && !project {
        tracing::info!(
            "project {scope}.{field} = false is looser than your true; \
             ignoring (project overrides can only tighten)"
        );
    }
    personal || project
}

/// Smaller-non-zero wins: `0` means "no limit" for both
/// `statement_timeout_ms` and `cost_preview_threshold_rows`, so it is
/// the weakest value, never the strictest.
fn stricter_limit(personal: u64, project: u64, scope: &str, field: &str) -> u64 {
    match (personal, project) {
        (0, p) => p,
        (l, 0) => {
            tracing::info!(
                "project {scope}.{field} = 0 (no limit) is looser than your {l}; \
                 ignoring (project overrides can only tighten)"
            );
            l
        }
        (l, p) => {
            if p > l {
                tracing::info!(
                    "project {scope}.{field} = {p} is looser than your {l}; \
                     ignoring (project overrides can only tighten)"
                );
            }
            l.min(p)
        }
    }
}

/// Field-by-field tighten-only merge of one profile. Pure apart from the
/// `tracing::info!` lines; `scope` only names the block in those logs.
fn tighten_profile(
    personal: &SafetyProfile,
    project: &SafetyProfile,
    scope: &str,
) -> SafetyProfile {
    let g = &personal.guards;
    let q = &project.guards;
    if personal.clean_mode != project.clean_mode {
        tracing::info!(
            "project {scope}.clean_mode = {:?} isn't a guard rail and isn't orderable; \
             keeping your {:?}",
            project.clean_mode,
            personal.clean_mode
        );
    }
    SafetyProfile {
        read_only: stricter_flag(personal.read_only, project.read_only, scope, "read_only"),
        statement_timeout_ms: stricter_limit(
            personal.statement_timeout_ms,
            project.statement_timeout_ms,
            scope,
            "statement_timeout_ms",
        ),
        auto_tx: stricter_flag(personal.auto_tx, project.auto_tx, scope, "auto_tx"),
        guards: Guards {
            insert: stricter_guard(g.insert, q.insert, scope, "insert"),
            update: stricter_guard(g.update, q.update, scope, "update"),
            update_without_where: stricter_guard(
                g.update_without_where,
                q.update_without_where,
                scope,
                "update_without_where",
            ),
            delete: stricter_guard(g.delete, q.delete, scope, "delete"),
            delete_without_where: stricter_guard(
                g.delete_without_where,
                q.delete_without_where,
                scope,
                "delete_without_where",
            ),
            truncate: stricter_guard(g.truncate, q.truncate, scope, "truncate"),
            drop: stricter_guard(g.drop, q.drop, scope, "drop"),
            ddl: stricter_guard(g.ddl, q.ddl, scope, "ddl"),
            other: stricter_guard(g.other, q.other, scope, "other"),
        },
        cost_preview_threshold_rows: stricter_limit(
            personal.cost_preview_threshold_rows,
            project.cost_preview_threshold_rows,
            scope,
            "cost_preview_threshold_rows",
        ),
        clean_mode: personal.clean_mode,
    }
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
    fn connection_to_dsn_never_borrows_pgpassword() {
        // The repro from the security review: a committed pgman.toml
        // names a host, the operator happens to have PGPASSWORD
        // exported, and pgman used to send it there.
        // SAFETY: the lib test binary reads PGPASSWORD only here and in
        // the sibling test below, and tests in one binary that touch it
        // are all in this module.
        unsafe {
            std::env::set_var("PGPASSWORD", "operator-secret");
        }
        let c = Connection {
            name: "theirs".into(),
            url: "postgres://app@db.example.com/main".into(),
            user: None,
            password_env: None,
            ssh_tunnel: None,
        };
        let dsn = connection_to_dsn(&c).unwrap();
        unsafe {
            std::env::remove_var("PGPASSWORD");
        }
        assert_eq!(
            dsn.password, None,
            "a project-file connection must never borrow $PGPASSWORD"
        );
    }

    #[test]
    fn connection_to_dsn_uses_password_env_when_set() {
        // SAFETY: unique var name, not touched by any other test.
        unsafe {
            std::env::set_var("PGMAN_TEST_PROJECT_PW", "from-password-env");
        }
        let c = Connection {
            name: "staging".into(),
            url: "postgres://app@db.example.com/main".into(),
            user: None,
            password_env: Some("PGMAN_TEST_PROJECT_PW".into()),
            ssh_tunnel: None,
        };
        let dsn = connection_to_dsn(&c).unwrap();
        unsafe {
            std::env::remove_var("PGMAN_TEST_PROJECT_PW");
        }
        assert_eq!(dsn.password.as_deref(), Some("from-password-env"));
    }

    #[test]
    fn connection_to_dsn_with_no_password_source_connects_without_one() {
        let c = Connection {
            name: "local".into(),
            url: "postgres://postgres@localhost:5432/myapp".into(),
            user: None,
            password_env: Some("PGMAN_TEST_PROJECT_PW_UNSET".into()),
            ssh_tunnel: None,
        };
        unsafe {
            std::env::remove_var("PGMAN_TEST_PROJECT_PW_UNSET");
        }
        let dsn = connection_to_dsn(&c).unwrap();
        assert_eq!(dsn.password, None);
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

    /// A `ProjectSafety` carrying just one `[safety.databases.<db>]`
    /// profile.
    fn project_db(db: &str, profile: SafetyProfile) -> ProjectSafety {
        ProjectSafety {
            default: None,
            databases: {
                let mut m = HashMap::new();
                m.insert(db.to_string(), profile);
                m
            },
        }
    }

    /// A `ProjectSafety` carrying just `[safety.default]`.
    fn project_default(profile: SafetyProfile) -> ProjectSafety {
        ProjectSafety {
            default: Some(profile),
            databases: HashMap::new(),
        }
    }

    #[test]
    fn merge_safety_project_database_tightens_the_timeout() {
        let mut global = SafetyConfig::default();
        global.databases.insert(
            "prod".into(),
            SafetyProfile {
                statement_timeout_ms: 99_999,
                ..Default::default()
            },
        );
        let project = project_db(
            "prod",
            SafetyProfile {
                statement_timeout_ms: 1_000,
                ..Default::default()
            },
        );
        let merged = merge_safety(global, Some(&project));
        assert_eq!(merged.databases["prod"].statement_timeout_ms, 1_000);
    }

    #[test]
    fn merge_safety_project_database_cannot_loosen_the_timeout() {
        let mut global = SafetyConfig::default();
        global.databases.insert(
            "prod".into(),
            SafetyProfile {
                statement_timeout_ms: 1_000,
                ..Default::default()
            },
        );
        let project = project_db(
            "prod",
            SafetyProfile {
                statement_timeout_ms: 99_999,
                ..Default::default()
            },
        );
        let merged = merge_safety(global, Some(&project));
        assert_eq!(merged.databases["prod"].statement_timeout_ms, 1_000);
    }

    #[test]
    fn merge_safety_zero_timeout_is_no_limit_not_the_strictest() {
        // 0 means "no statement_timeout at all" — the weakest value,
        // so a project can't disable the operator's timeout with it…
        let mut global = SafetyConfig::default();
        global.default.statement_timeout_ms = 5_000;
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                statement_timeout_ms: 0,
                ..Default::default()
            })),
        );
        assert_eq!(merged.default.statement_timeout_ms, 5_000);

        // …but when the operator has no limit, the project's does apply.
        let mut global = SafetyConfig::default();
        global.default.statement_timeout_ms = 0;
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                statement_timeout_ms: 7_000,
                ..Default::default()
            })),
        );
        assert_eq!(merged.default.statement_timeout_ms, 7_000);
    }

    #[test]
    fn merge_safety_project_default_tightens_read_only_but_cannot_clear_it() {
        // personal read_only=false, project true → true (tightened).
        let mut global = SafetyConfig::default();
        global.default.read_only = false;
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                read_only: true,
                ..Default::default()
            })),
        );
        assert!(merged.default.read_only, "project must be able to tighten");

        // personal read_only=true, project false → stays true.
        let global = SafetyConfig::default();
        assert!(global.default.read_only, "default profile is read-only");
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                read_only: false,
                ..Default::default()
            })),
        );
        assert!(
            merged.default.read_only,
            "a committed pgman.toml must not be able to clear read_only"
        );
    }

    #[test]
    fn merge_safety_project_cannot_clear_auto_tx() {
        let global = SafetyConfig::default();
        assert!(global.default.auto_tx);
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                auto_tx: false,
                ..Default::default()
            })),
        );
        assert!(
            merged.default.auto_tx,
            "a committed pgman.toml must not be able to clear auto_tx"
        );
    }

    #[test]
    fn merge_safety_guards_take_the_stricter_of_the_two() {
        // personal allows DROP; project blocks it → Block.
        let mut global = SafetyConfig::default();
        global.default.guards.drop = Guard::Allow;
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                guards: Guards {
                    drop: Guard::Block,
                    ..Default::default()
                },
                ..Default::default()
            })),
        );
        assert_eq!(merged.default.guards.drop, Guard::Block);
    }

    #[test]
    fn merge_safety_project_cannot_relax_a_guard() {
        // personal blocks DROP; project says allow → still Block.
        let global = SafetyConfig::default();
        assert_eq!(global.default.guards.drop, Guard::Block);
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                guards: Guards {
                    drop: Guard::Allow,
                    delete_without_where: Guard::Allow,
                    insert: Guard::Allow,
                    ..Default::default()
                },
                ..Default::default()
            })),
        );
        assert_eq!(merged.default.guards.drop, Guard::Block);
        assert_eq!(merged.default.guards.delete_without_where, Guard::Block);
        assert_eq!(
            merged.default.guards.insert,
            Guard::Confirm,
            "insert stays at the operator's Confirm, not the project's Allow"
        );
    }

    #[test]
    fn merge_safety_named_database_starts_from_the_personal_default() {
        // The personal config has no `prod` entry, so `prod` inherits
        // the personal default — a project can't relax a database
        // just by being the first to name it.
        let global = SafetyConfig::default();
        let project = project_db(
            "prod",
            SafetyProfile {
                read_only: false,
                statement_timeout_ms: 0,
                auto_tx: false,
                guards: Guards {
                    drop: Guard::Allow,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let merged = merge_safety(global, Some(&project));
        let prod = &merged.databases["prod"];
        assert!(prod.read_only);
        assert!(prod.auto_tx);
        assert_eq!(prod.statement_timeout_ms, 30_000);
        assert_eq!(prod.guards.drop, Guard::Block);
    }

    #[test]
    fn merge_safety_cost_preview_threshold_takes_the_smaller_non_zero() {
        let mut global = SafetyConfig::default();
        global.default.cost_preview_threshold_rows = 0; // disabled
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                cost_preview_threshold_rows: 1_000,
                ..Default::default()
            })),
        );
        assert_eq!(merged.default.cost_preview_threshold_rows, 1_000);

        let mut global = SafetyConfig::default();
        global.default.cost_preview_threshold_rows = 500;
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                cost_preview_threshold_rows: 0,
                ..Default::default()
            })),
        );
        assert_eq!(
            merged.default.cost_preview_threshold_rows, 500,
            "0 disables the preview — a project must not be able to turn it off"
        );
    }

    #[test]
    fn merge_safety_clean_mode_stays_the_operators() {
        let mut global = SafetyConfig::default();
        global.default.clean_mode = crate::dbunit::CleanMode::DeleteFrom;
        let merged = merge_safety(
            global,
            Some(&project_default(SafetyProfile {
                clean_mode: crate::dbunit::CleanMode::Truncate,
                ..Default::default()
            })),
        );
        assert_eq!(
            merged.default.clean_mode,
            crate::dbunit::CleanMode::DeleteFrom
        );
    }
}
