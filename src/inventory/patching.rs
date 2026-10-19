//! Applying an environment database's pending patches, from the overview
//! (`P`) or MCP (`apply_patches`).
//!
//! Two steps, so a human sees exactly what will run before it does:
//!
//! 1. [`plan`] connects read-only, reads which patches the database has
//!    recorded, and lists the pending ones in order with their text —
//!    each checked by [`query::apply::check_patch`] against safety.toml.
//!    It stops at the first patch that can't be applied here (a read-only
//!    profile, a blocked statement, its own `BEGIN`, `CONCURRENTLY`).
//! 2. After the confirmation, [`apply`] connects writable and runs the
//!    planned patches in order — the text that was shown, not a fresh
//!    read of the file — each in its own transaction, re-reading the
//!    version table before each one and stopping at the first failure,
//!    or at the first sign the database moved since the plan.
//!
//! Only a primary (or a plain server) is patched; a replica is refused.
//! An environment is *protected* when it says `protected = true`, or by
//! default when its name contains "prod"; a protected one is confirmed
//! by typing its name.
//!
//! [`query::apply::check_patch`]: crate::query::apply::check_patch

use super::{Environment, Target};
use crate::creds::aws_secrets::{SecretCreds, SecretRef};
use crate::mcp::connections::Role;
use crate::query::patches::{PatchFile, PatchesConfig};

/// Statement timeout on the writable connection: a table rewrite takes
/// time; waiting on a lock does not (`query::apply::LOCK_TIMEOUT`).
const APPLY_STATEMENT_TIMEOUT_MS: u64 = 600_000;

/// Statement timeout on the plan's read-only connection.
const PLAN_STATEMENT_TIMEOUT_MS: u64 = 10_000;

/// One patch the plan will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPatch {
    pub file: PatchFile,
    /// The text as read at plan time — what runs.
    pub text: String,
    pub statements: Vec<String>,
}

/// What [`apply`] would do, for the operator to confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchPlan {
    pub environment: String,
    pub server: String,
    pub database: String,
    pub protected: bool,
    /// The highest id recorded when planned.
    pub applied_max: Option<i64>,
    pub patches: Vec<PlannedPatch>,
    /// The first pending patch that can't be applied here, and why; the
    /// plan ends before it.
    pub stopped_at: Option<(String, String)>,
    /// Where the patch files are, for the operator to read them.
    pub dir: String,
}

impl PatchPlan {
    /// The patch's id, as the version table records it.
    fn id(p: &PatchFile) -> Option<i64> {
        p.version_id.or(p.filename_number)
    }
}

/// One planned patch's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchResult {
    pub file: String,
    pub outcome: Result<usize, String>,
}

/// What [`apply`] did: a result per patch it got to, in order. Patches
/// after a failure are not run, and not listed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyReport {
    pub results: Vec<PatchResult>,
    /// Set when it couldn't start (connect, re-check).
    pub error: Option<String>,
}

impl ApplyReport {
    pub fn applied(&self) -> usize {
        self.results.iter().filter(|r| r.outcome.is_ok()).count()
    }

    pub fn failed(&self) -> Option<&PatchResult> {
        self.results.iter().find(|r| r.outcome.is_err())
    }

    /// One line for a status bar or a tool result.
    pub fn summary(&self) -> String {
        if let Some(e) = &self.error {
            return format!("nothing applied: {e}");
        }
        match self.failed() {
            None => format!("applied {} patch(es)", self.applied()),
            Some(f) => format!(
                "applied {}, then {} failed: {}",
                self.applied(),
                f.file,
                f.outcome.as_ref().err().map(String::as_str).unwrap_or("")
            ),
        }
    }
}

/// Whether `env` is confirmed by typing its name. Pure.
pub fn is_protected(env: &Environment) -> bool {
    env.protected
        .unwrap_or_else(|| env.name.to_ascii_lowercase().contains("prod"))
}

/// Find `server`/`database` in `environment`, from environments.toml
/// (listing its source). Blocking: the aws CLI may run.
pub fn resolve(
    environment: &str,
    server: &str,
    database: &str,
) -> Result<(Environment, Target), String> {
    let loaded = super::load();
    let envs = loaded.environments?;
    let env = envs
        .into_iter()
        .find(|e| e.name == environment)
        .ok_or_else(|| format!("no environment {environment:?} in environments.toml"))?;
    let personal = {
        let path = crate::util::config_file("connections.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => crate::mcp::connections::parse_personal(&text),
            Err(_) => crate::mcp::connections::Personal::default(),
        }
    };
    let entries = crate::mcp::connections::build(&personal.connections, Vec::new(), false);
    let report = super::gather(
        std::slice::from_ref(&env),
        &entries,
        super::fetch_rds,
        super::docker::fetch,
    )
    .pop()
    .ok_or("no report")?;
    if let Some(e) = report.error.filter(|_| report.targets.is_empty()) {
        return Err(format!("{environment}: {e}"));
    }
    let target = report
        .targets
        .into_iter()
        .find(|t| t.server == server && t.database == database)
        .ok_or_else(|| format!("{environment} has no {server}/{database}"))?;
    Ok((env, target))
}

/// Why `target` of `env` can't be patched at all, before connecting.
/// Pure.
fn refuse(env: &Environment, target: &Target) -> Result<PatchesConfig, String> {
    if target.role == Role::Replica {
        return Err(format!(
            "{} is a replica; patches go to its primary{}",
            target.server,
            target
                .replica_of
                .as_deref()
                .map(|p| format!(" ({p})"))
                .unwrap_or_default()
        ));
    }
    let vt = env.version_table.as_ref().ok_or_else(|| {
        format!(
            "{} has no version_table in environments.toml, so pgman doesn't know its patches",
            env.name
        )
    })?;
    if !vt.applies_to(&target.database) {
        return Err(format!(
            "{}'s version_table doesn't cover {}",
            env.name, target.database
        ));
    }
    if vt.patches.is_none() {
        return Err(format!(
            "{}'s version_table names no patches directory",
            env.name
        ));
    }
    Ok(vt.config())
}

/// Reads a secret. Blocking (the aws CLI).
pub type FetchSecret =
    std::sync::Arc<dyn Fn(&SecretRef) -> Result<SecretCreds, String> + Send + Sync>;

/// `target`'s DSN with its credentials.
async fn dsn_for(target: &Target, fetch_secret: &FetchSecret) -> Result<crate::conn::Dsn, String> {
    let spec = target.connect.clone()?;
    let mut dsn = spec.dsn.clone();
    match spec.secret {
        None => spec.apply_password_env(&mut dsn)?,
        Some(secret) => {
            let fetch = fetch_secret.clone();
            let id = secret.id.clone();
            let creds = tokio::task::spawn_blocking(move || fetch(&secret))
                .await
                .unwrap_or_else(|e| Err(format!("secret task failed: {e}")))
                .map_err(|e| format!("secret {id}: {e}"))?;
            crate::creds::aws_secrets::apply(&mut dsn, creds);
        }
    }
    Ok(dsn)
}

/// The pending patches of `target`, checked, for the operator to confirm.
pub async fn plan(
    env: &Environment,
    target: &Target,
    safety: &crate::safety::SafetyConfig,
    fetch_secret: &FetchSecret,
) -> Result<PatchPlan, String> {
    let cfg = refuse(env, target)?;
    let dir = env
        .version_table
        .as_ref()
        .and_then(|v| v.patch_dir())
        .ok_or("no patches directory")??;
    let files = {
        let (dir, cfg) = (dir.clone(), cfg.clone());
        tokio::task::spawn_blocking(move || crate::query::patches::read_patch_files(&dir, &cfg))
            .await
            .map_err(|e| format!("patch files task failed: {e}"))??
    };
    let dsn = dsn_for(target, fetch_secret).await?;
    let (client, _tunnel) = connect(dsn, true, PLAN_STATEMENT_TIMEOUT_MS).await?;
    let applied = crate::query::apply::applied_ids(&client, &cfg).await?;
    let report = crate::query::patches::reconcile(files, &applied);
    let mut plan = PatchPlan {
        environment: env.name.clone(),
        server: target.server.clone(),
        database: target.database.clone(),
        protected: is_protected(env),
        applied_max: report.applied_max,
        patches: Vec::new(),
        stopped_at: None,
        dir: dir.display().to_string(),
    };
    for file in report.pending {
        let path = dir.join(&file.file);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                plan.stopped_at = Some((file.file, format!("could not read it: {e}")));
                break;
            }
        };
        match crate::query::apply::check_patch(safety, &target.database, &text) {
            Ok(statements) => plan.patches.push(PlannedPatch {
                file,
                text,
                statements,
            }),
            Err(e) => {
                plan.stopped_at = Some((file.file, e));
                break;
            }
        }
    }
    Ok(plan)
}

async fn connect(
    dsn: crate::conn::Dsn,
    read_only: bool,
    timeout_ms: u64,
) -> Result<
    (
        std::sync::Arc<tokio_postgres::Client>,
        Option<crate::tunnel::SshTunnel>,
    ),
    String,
> {
    let (notice_tx, _n) = tokio::sync::mpsc::unbounded_channel();
    let (notification_tx, _m) = tokio::sync::mpsc::unbounded_channel();
    crate::conn::connect_only(dsn, read_only, timeout_ms, notice_tx, notification_tx).await
}

/// Whether `applied` is where `plan` left it, `done` patches in: the
/// earlier planned ones recorded, this one and the rest not. Pure.
pub fn still_as_planned(plan: &PatchPlan, done: usize, applied: &[i64]) -> Result<(), String> {
    for (i, p) in plan.patches.iter().enumerate() {
        let Some(id) = PatchPlan::id(&p.file) else {
            return Err(format!("{} has no version id", p.file.file));
        };
        let recorded = applied.contains(&id);
        if i < done && !recorded {
            return Err(format!("{} is no longer recorded", p.file.file));
        }
        if i >= done && recorded {
            return Err(format!(
                "{} was applied by someone else since the plan; plan again",
                p.file.file
            ));
        }
    }
    Ok(())
}

/// Run `plan` on `target`: each patch in its own transaction, in order,
/// stopping at the first failure.
pub async fn apply(
    env: &Environment,
    target: &Target,
    safety: &crate::safety::SafetyConfig,
    fetch_secret: &FetchSecret,
    plan: &PatchPlan,
) -> ApplyReport {
    let mut report = ApplyReport::default();
    let start = async {
        let cfg = refuse(env, target)?;
        if plan.environment != env.name
            || plan.server != target.server
            || plan.database != target.database
        {
            return Err("the plan is for another database".to_string());
        }
        let dsn = dsn_for(target, fetch_secret).await?;
        let (client, tunnel) = connect(dsn, false, APPLY_STATEMENT_TIMEOUT_MS).await?;
        // The inventory's role can be wrong (a static connection to a hot
        // standby reads as direct): ask the server.
        if crate::query::apply::in_recovery(&client).await? {
            return Err(format!(
                "{} is a standby (in recovery); patches go to its primary",
                target.server
            ));
        }
        Ok((cfg, client, tunnel))
    };
    let (cfg, client, _tunnel) = match start.await {
        Ok(x) => x,
        Err(e) => {
            report.error = Some(e);
            return report;
        }
    };
    for (i, p) in plan.patches.iter().enumerate() {
        let outcome = async {
            let applied = crate::query::apply::applied_ids(&client, &cfg).await?;
            still_as_planned(plan, i, &applied)?;
            // Checked again: safety.toml may have changed since the plan.
            let statements = crate::query::apply::check_patch(safety, &target.database, &p.text)?;
            if statements != p.statements {
                return Err("the patch's statements differ from the plan".to_string());
            }
            let done = crate::query::apply::apply(&client, &cfg, &p.file, &statements).await?;
            if !done.recorded {
                return Err(format!(
                    "it ran, but recorded no version row {}; stopping",
                    PatchPlan::id(&p.file).unwrap_or_default()
                ));
            }
            tracing::info!(
                target: "pgman::patching",
                environment = %plan.environment,
                server = %plan.server,
                database = %plan.database,
                file = %p.file.file,
                "patch applied"
            );
            Ok(done.statements)
        }
        .await;
        let failed = outcome.is_err();
        report.results.push(PatchResult {
            file: p.file.file.clone(),
            outcome,
        });
        if failed {
            break;
        }
    }
    report
}

/// [`plan`] for a database named in environments.toml, with the
/// personal safety.toml and secrets from AWS: what the overview's `P` and
/// MCP's `apply_patches` run.
pub async fn plan_for(
    environment: String,
    server: String,
    database: String,
) -> Result<PatchPlan, String> {
    let (env, target) =
        tokio::task::spawn_blocking(move || resolve(&environment, &server, &database))
            .await
            .map_err(|e| format!("resolve task failed: {e}"))??;
    let safety = crate::safety::SafetyConfig::load_personal().0;
    plan(&env, &target, &safety, &aws_secrets()).await
}

/// [`apply`] for a plan from [`plan_for`], resolving its database again.
pub async fn apply_for(p: PatchPlan) -> ApplyReport {
    let (e, s, d) = (p.environment.clone(), p.server.clone(), p.database.clone());
    let resolved = tokio::task::spawn_blocking(move || resolve(&e, &s, &d))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))
        .and_then(|r| r);
    match resolved {
        Ok((env, target)) => {
            let safety = crate::safety::SafetyConfig::load_personal().0;
            apply(&env, &target, &safety, &aws_secrets(), &p).await
        }
        Err(e) => ApplyReport {
            results: Vec::new(),
            error: Some(e),
        },
    }
}

fn aws_secrets() -> FetchSecret {
    std::sync::Arc::new(crate::creds::aws_secrets::fetch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str, protected: Option<bool>) -> Environment {
        let mut e = super::super::parse_config(&format!(
            "[[environments]]\nname = \"{name}\"\nsource = {{ kind = \"static\", connections = [\"x\"] }}\n"
        ))
        .unwrap()
        .remove(0);
        e.protected = protected;
        e
    }

    fn planned(ids: &[i64]) -> PatchPlan {
        PatchPlan {
            environment: "e".into(),
            server: "s".into(),
            database: "d".into(),
            protected: false,
            applied_max: Some(1),
            patches: ids
                .iter()
                .map(|&id| PlannedPatch {
                    file: PatchFile {
                        file: format!("{id}-x.sql"),
                        filename_number: Some(id),
                        version_id: Some(id),
                    },
                    text: String::new(),
                    statements: Vec::new(),
                })
                .collect(),
            stopped_at: None,
            dir: "/p".into(),
        }
    }

    #[test]
    fn production_is_protected_unless_it_says_otherwise() {
        assert!(is_protected(&env("production", None)));
        assert!(is_protected(&env("uk-PROD", None)));
        assert!(!is_protected(&env("staging", None)));
        assert!(is_protected(&env("staging", Some(true))));
        assert!(!is_protected(&env("production", Some(false))));
    }

    #[test]
    fn each_patch_checks_the_database_is_where_the_plan_left_it() {
        let p = planned(&[2, 3]);
        assert_eq!(still_as_planned(&p, 0, &[1]), Ok(()));
        assert_eq!(still_as_planned(&p, 1, &[1, 2]), Ok(()));
        assert!(still_as_planned(&p, 0, &[1, 3])
            .unwrap_err()
            .contains("3-x.sql was applied by someone else"));
        assert!(still_as_planned(&p, 1, &[1])
            .unwrap_err()
            .contains("no longer recorded"));
    }

    #[test]
    fn a_replica_or_an_environment_without_patches_is_refused() {
        let mut e = env("staging", None);
        let mut t = Target {
            database: "ledger".into(),
            server: "staging-replica".into(),
            role: Role::Replica,
            replica_of: Some("staging".into()),
            host: None,
            port: None,
            engine: None,
            status: None,
            class: None,
            multi_az: None,
            connect: Err("n/a".into()),
        };
        assert_eq!(
            refuse(&e, &t).unwrap_err(),
            "staging-replica is a replica; patches go to its primary (staging)"
        );
        t.role = Role::Primary;
        assert!(refuse(&e, &t).unwrap_err().contains("no version_table"));
        e.version_table = Some(super::super::VersionTable {
            table: "t_schema_version".into(),
            column: "schema_version_id".into(),
            patches: Some("/p".into()),
            databases: vec!["ledgerlog".into()],
        });
        assert!(refuse(&e, &t).unwrap_err().contains("doesn't cover ledger"));
        e.version_table.as_mut().unwrap().databases.clear();
        assert_eq!(refuse(&e, &t).unwrap().table, "t_schema_version");
    }

    #[test]
    fn the_summary_names_the_failure() {
        let r = ApplyReport {
            results: vec![
                PatchResult {
                    file: "2-x.sql".into(),
                    outcome: Ok(2),
                },
                PatchResult {
                    file: "3-x.sql".into(),
                    outcome: Err("lock timeout".into()),
                },
            ],
            error: None,
        };
        assert_eq!(r.summary(), "applied 1, then 3-x.sql failed: lock timeout");
        assert_eq!(
            ApplyReport {
                error: Some("no route".into()),
                ..Default::default()
            }
            .summary(),
            "nothing applied: no route"
        );
    }
}
