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
//!    A pending file numbered below the highest recorded id is left out
//!    and named: the database skipped it, and running it now would put it
//!    after patches numbered above it.
//! 2. After the confirmation, [`apply`] connects writable and runs the
//!    planned patches in order — the text that was shown, not a fresh
//!    read of the file — each in its own transaction, re-reading the
//!    version table before each one and stopping at the first failure,
//!    or at the first sign the database moved since the plan.
//!
//! Planned with `outside` (the TUI's `o`), a patch that can't run in a
//! transaction is planned too, marked, and run statement by statement
//! with no transaction; confirming such a plan takes the typed name even
//! where the environment isn't protected. MCP never plans that way.
//!
//! A confirmed plan runs as its [`RunMode`] says: for real; as a dry
//! run, every patch in one transaction rolled back at the end (the TUI's
//! Tab, MCP's `mode`); or for real on a template copy of the database
//! made on the same server and dropped afterwards.
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

/// Statement timeout on the writable connection, set again before each
/// patch statement (`query::apply::STATEMENT_TIMEOUT_MS`).
const APPLY_STATEMENT_TIMEOUT_MS: u64 = crate::query::apply::STATEMENT_TIMEOUT_MS;

/// Statement timeout on the plan's read-only connection.
const PLAN_STATEMENT_TIMEOUT_MS: u64 = 10_000;

/// One patch the plan will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPatch {
    pub file: PatchFile,
    /// The text as read at plan time — what runs.
    pub text: String,
    pub statements: Vec<String>,
    /// Set when it runs outside a transaction, naming why (`VACUUM`…):
    /// its statements commit one by one.
    pub outside: Option<&'static str>,
    /// What its statements that safety.toml would ask about one by one
    /// are (`query::apply::guard_confirms`): the plan's one confirmation
    /// answers them, so the plan names them.
    pub confirms: Vec<String>,
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
    /// Every id recorded when planned, sorted: the run checks the
    /// database is still exactly here (plus what it has applied).
    pub recorded: Vec<i64>,
    /// Pending files numbered below the highest recorded id, left out:
    /// running one now would put it after patches numbered above it.
    pub below_recorded: Vec<String>,
    /// Planned with patches that need it allowed to run outside a
    /// transaction (the TUI's `o`; never MCP).
    pub outside_transaction: bool,
    /// The plan stopped at a patch that would run outside a transaction:
    /// planning again with `outside_transaction` gets further.
    pub outside_would_help: bool,
}

impl PlannedPatch {
    /// Its [`PlannedPatch::confirms`], each kind once in first-seen order,
    /// counted when more than one: `["INSERT ×2", "TRUNCATE"]`. Pure.
    pub fn confirm_kinds(&self) -> Vec<String> {
        let mut kinds: Vec<(&str, usize)> = Vec::new();
        for k in &self.confirms {
            match kinds.iter_mut().find(|(seen, _)| *seen == k.as_str()) {
                Some((_, c)) => *c += 1,
                None => kinds.push((k, 1)),
            }
        }
        kinds
            .into_iter()
            .map(|(kind, count)| match count {
                1 => kind.to_string(),
                n => format!("{kind} ×{n}"),
            })
            .collect()
    }
}

impl PatchPlan {
    /// The patch's id, as the version table records it.
    fn id(p: &PatchFile) -> Option<i64> {
        p.version_id.or(p.filename_number)
    }

    /// Whether confirming takes typing the environment's name: a
    /// protected one, or any patch running outside a transaction.
    pub fn needs_typed(&self) -> bool {
        self.protected || self.patches.iter().any(|p| p.outside.is_some())
    }

    /// How many planned statements safety.toml's guards would ask about
    /// one by one, were they run from the editor.
    pub fn confirm_count(&self) -> usize {
        self.patches.iter().map(|p| p.confirms.len()).sum()
    }

    /// Those statements, for the operator about to answer them all at
    /// once: `2 statement(s) your guard rails would normally ask about:
    /// TRUNCATE in 321-a.sql, DELETE without WHERE in 322-b.sql`, a kind
    /// a patch holds more than once counted (`INSERT ×2 in …`). `None`
    /// when there are none. Pure.
    pub fn confirm_summary(&self) -> Option<String> {
        let n = self.confirm_count();
        if n == 0 {
            return None;
        }
        let items: Vec<String> = self
            .patches
            .iter()
            .flat_map(|p| {
                p.confirm_kinds()
                    .into_iter()
                    .map(move |k| format!("{k} in {}", p.file.file))
            })
            .collect();
        Some(format!(
            "{n} statement(s) your guard rails would normally ask about: {}",
            items.join(", ")
        ))
    }
}

/// How a confirmed plan runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
    /// For real: each patch committed in turn.
    #[default]
    Apply,
    /// Every patch in ONE transaction, rolled back at the end: nothing is
    /// kept, but it takes the same locks and does the same work as the
    /// real run, holding them until the last patch is done. A patch
    /// that can't run in a transaction ends it.
    DryRun,
    /// For real, on a copy of the database (`CREATE DATABASE … TEMPLATE`)
    /// on the same server, dropped afterwards. The copy needs nobody
    /// else connected to the database.
    Copy,
}

impl RunMode {
    /// The next mode, for the TUI's Tab. Pure.
    pub fn next(self) -> Self {
        match self {
            RunMode::Apply => RunMode::DryRun,
            RunMode::DryRun => RunMode::Copy,
            RunMode::Copy => RunMode::Apply,
        }
    }

    /// Its name in MCP's `mode` argument. Pure.
    pub fn name(self) -> &'static str {
        match self {
            RunMode::Apply => "apply",
            RunMode::DryRun => "dry_run",
            RunMode::Copy => "copy",
        }
    }

    /// The mode MCP's `mode` argument names. Pure.
    pub fn parse(s: &str) -> Option<Self> {
        [RunMode::Apply, RunMode::DryRun, RunMode::Copy]
            .into_iter()
            .find(|m| m.name() == s)
    }
}

/// The copy a [`RunMode::Copy`] run made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyNote {
    pub name: String,
    /// Dropped afterwards, or why not: it is then still on the server.
    pub dropped: Result<(), String>,
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
    /// The highest id recorded after the run, read again at the end;
    /// `None` when it couldn't be read (or the run never connected). On
    /// a copy, the copy's.
    pub recorded_after: Option<i64>,
    pub mode: RunMode,
    /// Set once a [`RunMode::Copy`] run has made its copy.
    pub copy: Option<CopyNote>,
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
        let copy = match &self.copy {
            None => String::new(),
            Some(c) => match &c.dropped {
                // Named in the patch view; the status bar has no room.
                Ok(()) => " · copy dropped".to_string(),
                Err(e) => format!(" · copy {} NOT dropped ({e}): drop it by hand", c.name),
            },
        };
        if let Some(e) = &self.error {
            return format!("{}nothing applied: {e}{copy}", self.prefix());
        }
        let ran = self.ran();
        let recorded = match (self.recorded_after, self.mode) {
            (None, _) => String::new(),
            (Some(v), RunMode::DryRun) => format!(" · all rolled back, recorded up to {v} still"),
            (Some(v), RunMode::Copy) => format!(" · the copy recorded up to {v}"),
            (Some(v), RunMode::Apply) => format!(" · recorded up to {v} now"),
        };
        format!("{}{ran}{recorded}{copy}", self.prefix())
    }

    fn prefix(&self) -> &'static str {
        match self.mode {
            RunMode::Apply => "",
            RunMode::DryRun => "dry run: ",
            RunMode::Copy => "on a copy: ",
        }
    }

    fn ran(&self) -> String {
        let verb = match self.mode {
            RunMode::DryRun => "ran",
            _ => "applied",
        };
        match self.failed() {
            None => format!("{verb} {} patch(es)", self.applied()),
            Some(f) => format!(
                "{verb} {}, then {} failed: {}",
                self.applied(),
                f.file,
                f.outcome.as_ref().err().map(String::as_str).unwrap_or("")
            ),
        }
    }
}

/// Where a running [`apply`] leaves the means to cancel its statement:
/// shared between the run, which fills it once connected, and whoever
/// may want to stop it (the TUI's Esc).
#[derive(Clone, Default)]
pub struct CancelSlot(
    std::sync::Arc<std::sync::Mutex<Option<CancelFn>>>,
    /// Set by [`CancelSlot::cancel`]: the run starts no further statement
    /// or patch, and commits nothing more, so a cancel between
    /// statements, or before connecting, still holds.
    std::sync::Arc<std::sync::atomic::AtomicBool>,
);

/// Sends a cancel request for the run's statement. Fire and forget.
pub type CancelFn = std::sync::Arc<dyn Fn() + Send + Sync>;

impl CancelSlot {
    pub fn set(&self, cancel: CancelFn) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(cancel);
        }
    }

    /// Stop the run: no further statement or patch starts, the patch in
    /// its transaction rolls back, and the server is asked to cancel the
    /// running statement. `false` when the run hasn't
    /// connected yet (or has finished), so no request was sent.
    pub fn cancel(&self) -> bool {
        self.1.store(true, std::sync::atomic::Ordering::SeqCst);
        let cancel = self.0.lock().ok().and_then(|s| s.clone());
        match cancel {
            Some(f) => {
                f();
                true
            }
            None => false,
        }
    }

    /// Whether [`CancelSlot::cancel`] was called.
    pub fn requested(&self) -> bool {
        self.1.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn clear(&self) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = None;
        }
    }
}

impl std::fmt::Debug for CancelSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelSlot").finish_non_exhaustive()
    }
}

impl PartialEq for CancelSlot {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
mod cancel_slot_tests {
    use super::*;

    #[test]
    fn a_cancel_is_remembered_whether_or_not_anything_was_sent() {
        let slot = CancelSlot::default();
        assert!(!slot.requested());
        assert!(!slot.cancel(), "nothing to send before connecting");
        assert!(slot.requested(), "but the run won't start another patch");
        let sent = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = sent.clone();
        let slot = CancelSlot::default();
        slot.set(std::sync::Arc::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        assert!(slot.cancel());
        assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 1);
        slot.clear();
        assert!(!slot.cancel(), "emptied: nothing more is sent");
        assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

/// A [`CancelFn`] for `client`: a Postgres cancel request, through the
/// same tunnel as the connection.
fn canceller(client: &tokio_postgres::Client) -> CancelFn {
    let token = client.cancel_token();
    std::sync::Arc::new(move || {
        let token = token.clone();
        tokio::spawn(async move {
            // The cancel packet is unauthenticated and servers take it
            // plaintext, as the editor's ctrl-c does.
            if let Err(e) = token.cancel_query(tokio_postgres::NoTls).await {
                tracing::warn!(target: "pgman::patching", "cancel request failed: {e}");
            }
        });
    })
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
        super::fetch_rds_listing,
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

/// The refusal for a replica: its primary is patched instead. Pure.
pub fn replica_refusal(target: &Target) -> String {
    format!(
        "{} is a replica; patches go to its primary{}",
        target.server,
        target
            .replica_of
            .as_deref()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default()
    )
}

/// The refusal for a database outside its environment's
/// `version_table` (`databases = [...]`) — a Flyway or Liquibase one,
/// say, whose migrations its own tool applies. Pure.
pub fn not_covered(environment: &str, database: &str) -> String {
    format!("{environment}'s version_table doesn't cover {database}")
}

/// Why `target` of `env` can't be patched at all, before connecting.
/// Pure.
fn refuse(env: &Environment, target: &Target) -> Result<PatchesConfig, String> {
    if target.role == Role::Replica {
        return Err(replica_refusal(target));
    }
    let vt = env.version_table.as_ref().ok_or_else(|| {
        format!(
            "{} has no version_table in environments.toml, so pgman doesn't know its patches",
            env.name
        )
    })?;
    if !vt.applies_to(&target.database) {
        return Err(not_covered(&env.name, &target.database));
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

/// `pending` (in id order) split at `applied_max`: those above it, which
/// the plan runs, and those below it — a gap the database skipped, which
/// the plan leaves out. Pure.
pub fn split_at_recorded(
    pending: Vec<PatchFile>,
    applied_max: Option<i64>,
) -> (Vec<PatchFile>, Vec<PatchFile>) {
    pending
        .into_iter()
        .partition(|f| applied_max.is_none_or(|max| PatchPlan::id(f).is_none_or(|id| id > max)))
}

/// The pending patches of `target`, checked, for the operator to confirm.
pub async fn plan(
    env: &Environment,
    target: &Target,
    safety: &crate::safety::SafetyConfig,
    fetch_secret: &FetchSecret,
    outside: bool,
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
        below_recorded: Vec::new(),
        outside_transaction: outside,
        outside_would_help: false,
        dir: dir.display().to_string(),
        recorded: {
            let mut r = applied.clone();
            r.sort_unstable();
            r.dedup();
            r
        },
    };
    let duplicated: std::collections::BTreeSet<i64> =
        report.duplicate_ids.iter().map(|(id, _)| *id).collect();
    let (pending, below) = split_at_recorded(report.pending, report.applied_max);
    plan.below_recorded = below.into_iter().map(|f| f.file).collect();
    for file in pending {
        let id = PatchPlan::id(&file).unwrap_or_default();
        if duplicated.contains(&id) {
            plan.stopped_at = Some((
                file.file,
                format!("another file records version {id} too; fix the files first"),
            ));
            break;
        }
        if let Some(n) = file.filename_number.filter(|n| *n != id) {
            plan.stopped_at = Some((
                file.file,
                format!("its name says {n} but it records version {id}; fix the file first"),
            ));
            break;
        }
        let path = dir.join(&file.file);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                plan.stopped_at = Some((file.file, format!("could not read it: {e}")));
                break;
            }
        };
        match crate::query::apply::check_patch(safety, &target.database, &text, outside) {
            Ok(checked) => plan.patches.push(PlannedPatch {
                file,
                text,
                statements: checked.statements,
                outside: checked.outside,
                confirms: checked.confirms,
            }),
            Err(e) => {
                plan.outside_would_help = !outside
                    && crate::query::apply::check_patch(safety, &target.database, &text, true)
                        .is_ok();
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
    let planned: Vec<i64> = plan
        .patches
        .iter()
        .filter_map(|p| PatchPlan::id(&p.file))
        .collect();
    // Anything recorded that is neither from the plan's start nor one of
    // its own patches: someone else is patching this database.
    if let Some(other) = applied
        .iter()
        .find(|id| !plan.recorded.contains(id) && !planned.contains(id))
    {
        return Err(format!(
            "version {other} was recorded since the plan, by someone else; plan again"
        ));
    }
    if let Some(gone) = plan.recorded.iter().find(|id| !applied.contains(id)) {
        return Err(format!(
            "version {gone} was recorded at the plan but isn't now; plan again"
        ));
    }
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

/// Run `plan` on `target` as `mode` says: each patch in its own
/// transaction, in order, stopping at the first failure (on the database,
/// or on a copy of it); or all in one transaction that is rolled back.
/// Once connected, `cancel` stops the running statement, or the patch
/// before its next statement or its commit; that fails the patch like any
/// other error.
pub async fn apply(
    env: &Environment,
    target: &Target,
    safety: &crate::safety::SafetyConfig,
    fetch_secret: &FetchSecret,
    plan: &PatchPlan,
    mode: RunMode,
    cancel: &CancelSlot,
) -> ApplyReport {
    let mut report = ApplyReport {
        mode,
        ..Default::default()
    };
    let start = async {
        let cfg = refuse(env, target)?;
        if plan.environment != env.name
            || plan.server != target.server
            || plan.database != target.database
        {
            return Err("the plan is for another database".to_string());
        }
        Ok((cfg, dsn_for(target, fetch_secret).await?))
    };
    let (cfg, dsn) = match start.await {
        Ok(x) => x,
        Err(e) => {
            report.error = Some(e);
            return report;
        }
    };
    let run = Run {
        cfg: &cfg,
        safety,
        database: &target.database,
        plan,
        cancel,
    };
    match mode {
        RunMode::Apply | RunMode::DryRun => match open_writable(dsn, &target.server).await {
            Ok((client, _tunnel)) => run.on(&client, mode == RunMode::DryRun, &mut report).await,
            Err(e) => report.error = Some(e),
        },
        RunMode::Copy => run.on_copy(dsn, &target.server, &mut report).await,
    }
    report
}

/// A writable connection to `dsn`, refused when its server is a standby:
/// the inventory's role can be wrong (a static connection to a hot
/// standby reads as direct), so the server is asked.
async fn open_writable(
    dsn: crate::conn::Dsn,
    server: &str,
) -> Result<
    (
        std::sync::Arc<tokio_postgres::Client>,
        Option<crate::tunnel::SshTunnel>,
    ),
    String,
> {
    let (client, tunnel) = connect(dsn, false, APPLY_STATEMENT_TIMEOUT_MS).await?;
    if crate::query::apply::in_recovery(&client).await? {
        return Err(format!(
            "{server} is a standby (in recovery); patches go to its primary"
        ));
    }
    Ok((client, tunnel))
}

/// What one [`apply`] runs, whichever database it runs on.
struct Run<'a> {
    cfg: &'a PatchesConfig,
    safety: &'a crate::safety::SafetyConfig,
    /// The planned database, whose safety profile the patches are checked
    /// against — on a copy too.
    database: &'a str,
    plan: &'a PatchPlan,
    cancel: &'a CancelSlot,
}

impl Run<'_> {
    /// The plan's patches on `client`, into `report`; with `dry`, all in
    /// one transaction that is rolled back at the end.
    async fn on(&self, client: &tokio_postgres::Client, dry: bool, report: &mut ApplyReport) {
        let (plan, cancel) = (self.plan, self.cancel);
        cancel.set(canceller(client));
        if dry {
            if let Err(e) = crate::query::apply::begin(client).await {
                cancel.clear();
                report.error = Some(e);
                return;
            }
        }
        for (i, p) in plan.patches.iter().enumerate() {
            if cancel.requested() {
                report.results.push(PatchResult {
                    file: p.file.file.clone(),
                    outcome: Err("cancelled before it started; nothing applied".to_string()),
                });
                break;
            }
            let outcome = self.patch(client, i, p, dry).await;
            let failed = outcome.is_err();
            report.results.push(PatchResult {
                file: p.file.file.clone(),
                outcome,
            });
            if failed {
                break;
            }
        }
        if dry {
            // Failing, the connection closing undoes it all the same.
            if let Err(e) = crate::query::apply::rollback(client).await {
                tracing::warn!(target: "pgman::patching", error = %e, "dry run's rollback failed");
            }
        }
        // Nothing to cancel from here on, and a late cancel mustn't hit
        // the re-read below.
        cancel.clear();
        report.recorded_after = match crate::query::apply::applied_ids(client, self.cfg).await {
            Ok(ids) => ids.into_iter().max(),
            Err(e) => {
                tracing::warn!(target: "pgman::patching", error = %e, "couldn't re-read the version table");
                None
            }
        };
    }

    /// Planned patch `i`, `p`, on `client`: its statement count, or why
    /// it failed.
    async fn patch(
        &self,
        client: &tokio_postgres::Client,
        i: usize,
        p: &PlannedPatch,
        dry: bool,
    ) -> Result<usize, String> {
        let plan = self.plan;
        let applied = crate::query::apply::applied_ids(client, self.cfg).await?;
        still_as_planned(plan, i, &applied)?;
        // Checked again: safety.toml may have changed since the plan.
        let checked = crate::query::apply::check_patch(
            self.safety,
            self.database,
            &p.text,
            plan.outside_transaction,
        )?;
        if checked.statements != p.statements || checked.outside != p.outside {
            return Err("the patch's statements differ from the plan".to_string());
        }
        // The confirmation answered the guards' questions the plan named;
        // others now would be ones nobody saw.
        if checked.confirms != p.confirms {
            return Err(
                "safety.toml's guards would ask about other statements than the plan named; plan again"
                    .to_string(),
            );
        }
        let statements = checked.statements;
        // Between statements no statement is running for the server's
        // cancel to stop: the flag stops the patch instead.
        let stop = || self.cancel.requested();
        let done = match (p.outside, dry) {
            (Some(what), true) => {
                return Err(format!(
                    "{what} can't run in a transaction, so the dry run stops here"
                ))
            }
            (None, true) => {
                crate::query::apply::try_patch(client, self.cfg, &p.file, &statements, &stop)
                    .await?
            }
            (None, false) => {
                crate::query::apply::apply(client, self.cfg, &p.file, &statements, &stop).await?
            }
            (Some(what), false) => {
                tracing::warn!(
                    target: "pgman::patching",
                    environment = %plan.environment,
                    server = %plan.server,
                    database = %plan.database,
                    file = %p.file.file,
                    what,
                    "patch running outside a transaction"
                );
                crate::query::apply::apply_outside(client, self.cfg, &p.file, &statements, &stop)
                    .await?
            }
        };
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
            dry_run = dry,
            "patch applied"
        );
        Ok(done.statements)
    }

    /// The plan applied for real to a fresh template copy of the
    /// database, on the same server, dropped afterwards.
    async fn on_copy(&self, dsn: crate::conn::Dsn, server: &str, report: &mut ApplyReport) {
        let made = async {
            let (admin, tunnel) = open_admin(&dsn, server).await?;
            let others = crate::query::apply::other_sessions(&admin, self.database).await?;
            if others > 0 {
                return Err(format!(
                    "a copy needs nobody else connected to {}, and {others} session(s) are; \
                     it suits an idle database, such as a local or dev one",
                    self.database
                ));
            }
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let name = crate::query::apply::copy_name(self.database, stamp);
            self.cancel.set(canceller(&admin));
            let created = crate::query::apply::create_copy(&admin, self.database, &name).await;
            self.cancel.clear();
            created?;
            Ok((admin, tunnel, name))
        };
        let (admin, _admin_tunnel, name) = match made.await {
            Ok(x) => x,
            Err(e) => {
                report.error = Some(e);
                return;
            }
        };
        tracing::info!(target: "pgman::patching", server, database = %self.database, copy = %name, "copy made to apply patches to");
        let mut on_copy = dsn;
        on_copy.dbname = name.clone();
        match connect(on_copy, false, APPLY_STATEMENT_TIMEOUT_MS).await {
            // Closed before the drop below: a copy can't be dropped
            // while connected to.
            Ok((client, _tunnel)) => self.on(&client, false, report).await,
            Err(e) => report.error = Some(format!("could not connect to the copy {name}: {e}")),
        }
        let dropped = crate::query::apply::drop_copy(&admin, &name).await;
        if let Err(e) = &dropped {
            tracing::warn!(target: "pgman::patching", copy = %name, error = %e, "couldn't drop the copy");
        }
        report.copy = Some(CopyNote { name, dropped });
    }
}

/// A writable connection to another database on `dsn`'s server, to copy
/// `dsn`'s from: `postgres`, or `template1` where that can't be reached.
async fn open_admin(
    dsn: &crate::conn::Dsn,
    server: &str,
) -> Result<
    (
        std::sync::Arc<tokio_postgres::Client>,
        Option<crate::tunnel::SshTunnel>,
    ),
    String,
> {
    let mut first = None;
    for db in ["postgres", "template1"] {
        let mut d = dsn.clone();
        d.dbname = db.to_string();
        match open_writable(d, server).await {
            Ok(x) => return Ok(x),
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    Err(format!(
        "could not connect to {server}'s postgres database to make the copy: {}",
        first.unwrap_or_default()
    ))
}

/// [`plan`] for a database named in environments.toml, with secrets from
/// AWS: what the overview's `P` and MCP's `apply_patches` run. `safety`
/// is the personal safety.toml as the project's `[safety]` tightened it
/// (`project::merge_safety`), the same the editor and MCP's `query` use.
pub async fn plan_for(
    environment: String,
    server: String,
    database: String,
    outside: bool,
    safety: crate::safety::SafetyConfig,
) -> Result<PatchPlan, String> {
    let (env, target) =
        tokio::task::spawn_blocking(move || resolve(&environment, &server, &database))
            .await
            .map_err(|e| format!("resolve task failed: {e}"))??;
    plan(&env, &target, &safety, &aws_secrets(), outside).await
}

/// [`apply`] for a plan from [`plan_for`], resolving its database again;
/// `safety` as [`plan_for`] takes it.
pub async fn apply_for(
    p: PatchPlan,
    mode: RunMode,
    cancel: CancelSlot,
    safety: crate::safety::SafetyConfig,
) -> ApplyReport {
    let (e, s, d) = (p.environment.clone(), p.server.clone(), p.database.clone());
    let resolved = tokio::task::spawn_blocking(move || resolve(&e, &s, &d))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))
        .and_then(|r| r);
    match resolved {
        Ok((env, target)) => apply(&env, &target, &safety, &aws_secrets(), &p, mode, &cancel).await,
        Err(e) => ApplyReport {
            error: Some(e),
            mode,
            ..Default::default()
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
                    outside: None,
                    confirms: Vec::new(),
                })
                .collect(),
            stopped_at: None,
            dir: "/p".into(),
            recorded: vec![1],
            below_recorded: Vec::new(),
            outside_transaction: false,
            outside_would_help: false,
        }
    }

    #[test]
    fn a_pending_patch_below_the_recorded_version_is_left_out() {
        let f = |id: i64| PatchFile {
            file: format!("{id}-x.sql"),
            filename_number: Some(id),
            version_id: Some(id),
        };
        let names = |v: Vec<PatchFile>| v.into_iter().map(|f| f.file).collect::<Vec<_>>();
        let (run, below) = split_at_recorded(vec![f(7), f(249), f(250)], Some(248));
        assert_eq!(names(run), ["249-x.sql", "250-x.sql"]);
        assert_eq!(names(below), ["7-x.sql"]);
        // The recorded id itself isn't pending; one equal to it would be a
        // second file claiming it, which isn't "above".
        let (run, below) = split_at_recorded(vec![f(248)], Some(248));
        assert!(run.is_empty());
        assert_eq!(names(below), ["248-x.sql"]);
        let (run, below) = split_at_recorded(vec![f(1), f(2)], None);
        assert_eq!(names(run), ["1-x.sql", "2-x.sql"], "nothing recorded yet");
        assert!(below.is_empty());
    }

    #[test]
    fn the_plan_names_what_its_one_confirmation_answers() {
        let mut p = planned(&[321, 322, 323]);
        assert_eq!(p.confirm_summary(), None, "nothing to ask about");
        p.patches[0].confirms = vec!["TRUNCATE".into()];
        p.patches[1].confirms = vec![
            "INSERT".into(),
            "DELETE without WHERE".into(),
            "INSERT".into(),
        ];
        assert_eq!(p.confirm_count(), 4);
        assert_eq!(
            p.confirm_summary().unwrap(),
            "4 statement(s) your guard rails would normally ask about: TRUNCATE in 321-x.sql, \
             INSERT ×2 in 322-x.sql, DELETE without WHERE in 322-x.sql"
        );
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
        assert!(
            still_as_planned(&p, 0, &[1, 9])
                .unwrap_err()
                .contains("version 9 was recorded since"),
            "an id outside the plan"
        );
        assert!(still_as_planned(&p, 0, &[])
            .unwrap_err()
            .contains("version 1 was recorded at the plan"));
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
            recorded_after: None,
            mode: RunMode::Apply,
            copy: None,
        };
        assert_eq!(r.summary(), "applied 1, then 3-x.sql failed: lock timeout");
        let read = ApplyReport {
            recorded_after: Some(2),
            ..r.clone()
        };
        assert_eq!(
            read.summary(),
            "applied 1, then 3-x.sql failed: lock timeout · recorded up to 2 now"
        );
        assert_eq!(
            ApplyReport {
                error: Some("no route".into()),
                ..Default::default()
            }
            .summary(),
            "nothing applied: no route"
        );
    }

    #[test]
    fn a_dry_run_or_a_copy_says_so_in_its_summary() {
        let r = ApplyReport {
            results: vec![PatchResult {
                file: "2-x.sql".into(),
                outcome: Ok(2),
            }],
            recorded_after: Some(1),
            mode: RunMode::DryRun,
            ..Default::default()
        };
        assert_eq!(
            r.summary(),
            "dry run: ran 1 patch(es) · all rolled back, recorded up to 1 still"
        );
        let copy = ApplyReport {
            recorded_after: Some(2),
            mode: RunMode::Copy,
            copy: Some(CopyNote {
                name: "d_pgman_copy_7".into(),
                dropped: Ok(()),
            }),
            ..r.clone()
        };
        assert_eq!(
            copy.summary(),
            "on a copy: applied 1 patch(es) · the copy recorded up to 2 · copy dropped"
        );
        let stuck = ApplyReport {
            error: Some("could not connect to the copy".into()),
            copy: Some(CopyNote {
                name: "d_pgman_copy_7".into(),
                dropped: Err("in use".into()),
            }),
            ..copy
        };
        assert_eq!(
            stuck.summary(),
            "on a copy: nothing applied: could not connect to the copy · copy d_pgman_copy_7 NOT dropped (in use): drop it by hand"
        );
    }

    #[test]
    fn modes_go_round_and_parse_by_name() {
        let mut m = RunMode::default();
        for want in [RunMode::DryRun, RunMode::Copy, RunMode::Apply] {
            m = m.next();
            assert_eq!(m, want);
            assert_eq!(RunMode::parse(m.name()), Some(m));
        }
        assert_eq!(RunMode::parse("dry-run"), None);
        assert_eq!(RunMode::parse(""), None);
    }
}
