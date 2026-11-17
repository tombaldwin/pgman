//! One statement written to an environment's database, after the
//! operator confirms it: MCP's `execute`.
//!
//! Two steps, as patching takes them ([`super::patching`]):
//!
//! 1. [`plan_for`] checks the statement ([`query::execute::check`]),
//!    connects read-only, and counts the rows it changes, with the
//!    current values an `UPDATE` overwrites, or asks the planner for an
//!    estimate where it can't count them.
//! 2. After the confirmation, [`run_for`] connects writable — a standby
//!    is refused — and runs exactly the statement shown, once, in one
//!    transaction, committing only when it changed the rows confirmed.
//!
//! Only a primary (or a plain server) is written to; a replica is
//! refused. Protected as patching protects: by `protected = true`, or a
//! name containing "prod".
//!
//! [`query::execute::check`]: crate::query::execute::check

use crate::mcp::connections::Role;
use crate::query::execute::{self, Before, Checked};

/// What [`run_for`] would do, for the operator to confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPlan {
    pub environment: String,
    pub server: String,
    pub database: String,
    pub protected: bool,
    pub checked: Checked,
    /// The rows it changes now, counted with its own `WHERE`.
    pub rows: Option<u64>,
    /// Why they weren't counted, when they weren't.
    pub uncounted: Option<String>,
    /// The planner's estimate, where they weren't counted.
    pub estimate: Option<u64>,
    /// The rows it must change, or it is rolled back: the caller's
    /// `expect_rows`, else the count. A statement with neither is
    /// refused: nothing runs unchecked.
    pub expect: u64,
    /// What an `UPDATE` overwrites, as read when planned.
    pub before: Option<Before>,
    /// Why they weren't read, when an `UPDATE` sets plain columns.
    pub before_error: Option<String>,
}

impl ExecPlan {
    /// What this run lifts: the read-only database, the blocked
    /// statement. Pure.
    pub fn lifted(&self) -> Vec<String> {
        self.checked.lifted(&self.database)
    }

    /// Why confirming takes the environment's name typed, if it does.
    /// Pure.
    pub fn typed_reason(&self) -> Option<&'static str> {
        use crate::safety::StatementKind::{Delete, Update};
        if self.protected {
            Some("is protected")
        } else if !self.lifted().is_empty() {
            Some("runs what its safety settings refuse, for this run only")
        } else if matches!(
            self.checked.kind,
            Update { has_where: false } | Delete { has_where: false }
        ) {
            Some("would have every row of the table changed")
        } else {
            None
        }
    }
}

/// `checked`'s plan on `target`'s database, read on `client`
/// (read-only): the count, the current values, or the estimate.
async fn read_plan(
    client: &tokio_postgres::Client,
    checked: &Checked,
    expect_rows: Option<u64>,
) -> Result<
    (
        Option<u64>,
        Option<String>,
        Option<u64>,
        Option<Before>,
        Option<String>,
    ),
    String,
> {
    let shape = execute::shape(&checked.statement);
    let (rows, uncounted) = match &shape {
        Some(s) => match execute::count(client, s).await {
            Ok(n) => (Some(n), None),
            Err(e) => (None, Some(format!("counting its rows failed: {e}"))),
        },
        None => (
            None,
            Some(match checked.kind {
                crate::safety::StatementKind::Insert => "an INSERT".to_string(),
                _ => "not a plain UPDATE or DELETE of one table".to_string(),
            }),
        ),
    };
    let estimate = if rows.is_none() {
        execute::estimate(client, &checked.statement).await.ok()
    } else {
        None
    };
    match (expect_rows, rows) {
        (Some(e), Some(n)) if e != n => {
            return Err(format!(
                "nothing asked, nothing run: the statement changes {n} row(s) now, not the {e} \
                 expect_rows says"
            ))
        }
        (None, None) => return Err(uncounted_refusal(uncounted.as_deref(), estimate)),
        _ => {}
    }
    let (before, before_error) = match &shape {
        Some(s) => match execute::before(client, s).await {
            Ok(b) => (b, None),
            Err(e) => (None, Some(e)),
        },
        None => (None, None),
    };
    Ok((rows, uncounted, estimate, before, before_error))
}

/// The refusal for a statement pgman can't count, passed without
/// `expect_rows`: nothing would check how many rows it changed. Pure.
fn uncounted_refusal(why: Option<&str>, estimate: Option<u64>) -> String {
    format!(
        "nothing asked, nothing run: pgman can't count this statement's rows first ({}{}); pass \
         expect_rows with the number it must change, and it is rolled back if it changes any \
         other",
        why.unwrap_or("no count"),
        estimate
            .map(|e| format!("; the planner estimates about {e}"))
            .unwrap_or_default()
    )
}

/// The plan for running `sql` once on `server`/`database` of
/// `environment`. `safety` is the personal safety.toml as the project's
/// `[safety]` tightened it; `floor` the project's rules alone
/// (`project::project_floor`): what `safety` refuses the operator may
/// lift for this run, never what `floor` does.
pub async fn plan_for(
    environment: String,
    server: String,
    database: String,
    sql: String,
    expect_rows: Option<u64>,
    safety: crate::safety::SafetyConfig,
    floor: crate::safety::SafetyConfig,
) -> Result<ExecPlan, String> {
    let safety = safety.scoped(&environment, Some(&server));
    let floor = floor.scoped(&environment, Some(&server));
    let checked = match execute::check(&safety, &database, &sql, false) {
        Ok(c) => c,
        Err(r) if r.liftable => {
            if let Err(f) = execute::check(&floor, &database, &sql, false) {
                return Err(super::patching::project_refusal(&floor, &database, &f));
            }
            execute::check(&safety, &database, &sql, true).map_err(|r| r.why)?
        }
        Err(r) => return Err(r.why),
    };
    let (env, target) = {
        let (e, s, d) = (environment.clone(), server.clone(), database.clone());
        tokio::task::spawn_blocking(move || super::patching::resolve(&e, &s, &d))
            .await
            .map_err(|e| format!("resolve task failed: {e}"))??
    };
    if target.role == Role::Replica {
        return Err(replica_refusal(&target));
    }
    let dsn = super::patching::dsn_for(&target, &super::patching::aws_secrets()).await?;
    let (client, _tunnel) = super::patching::connect(dsn, true, 10_000).await?;
    if crate::query::apply::in_recovery(&client).await? {
        return Err(standby_refusal(&server));
    }
    let (rows, uncounted, estimate, before, before_error) =
        read_plan(&client, &checked, expect_rows).await?;
    Ok(ExecPlan {
        environment,
        server,
        database,
        protected: super::patching::is_protected(&env),
        expect: expect_rows.or(rows).ok_or("no row count")?,
        checked,
        rows,
        uncounted,
        estimate,
        before,
        before_error,
    })
}

/// Whether `plan`'s statement checks against `safety` now as it did
/// when planned: a safety.toml changed since then, between the question
/// and the answer, is plan-again. Pure but for safety.toml's path.
fn recheck(plan: &ExecPlan, safety: &crate::safety::SafetyConfig) -> Result<(), String> {
    let lift = !plan.lifted().is_empty();
    let again =
        execute::check(safety, &plan.database, &plan.checked.statement, lift).map_err(|r| r.why)?;
    if again != plan.checked {
        return Err("the statement checks differently now than when planned; plan again".into());
    }
    Ok(())
}

/// The refusal for a replica. Pure.
fn replica_refusal(target: &super::Target) -> String {
    format!(
        "{} is a replica; writes go to its primary{}",
        target.server,
        target
            .replica_of
            .as_deref()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default()
    )
}

/// The refusal for a server that says it is a standby. Pure.
fn standby_refusal(server: &str) -> String {
    format!("{server} is a standby (in recovery); writes go to its primary")
}

/// Run a plan from [`plan_for`], resolving its database again and
/// checking its statement again against `safety` (as [`plan_for`]
/// takes it): the rows it changed, or why nothing was kept.
pub async fn run_for(plan: &ExecPlan, safety: crate::safety::SafetyConfig) -> Result<u64, String> {
    recheck(plan, &safety.scoped(&plan.environment, Some(&plan.server)))?;
    let (e, s, d) = (
        plan.environment.clone(),
        plan.server.clone(),
        plan.database.clone(),
    );
    let (_, target) = tokio::task::spawn_blocking(move || super::patching::resolve(&e, &s, &d))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))??;
    if target.role == Role::Replica {
        return Err(replica_refusal(&target));
    }
    let dsn = super::patching::dsn_for(&target, &super::patching::aws_secrets()).await?;
    let (client, _tunnel) =
        super::patching::connect(dsn, false, execute::STATEMENT_TIMEOUT_MS).await?;
    if crate::query::apply::in_recovery(&client).await? {
        return Err(standby_refusal(&plan.server));
    }
    execute::run(&client, &plan.checked.statement, plan.expect).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(sql: &str, cfg: &crate::safety::SafetyConfig, protected: bool) -> ExecPlan {
        let checked = execute::check(cfg, "ledger", sql, true).unwrap();
        ExecPlan {
            environment: "demo".into(),
            server: "db1".into(),
            database: "ledger".into(),
            protected,
            checked,
            rows: Some(1),
            uncounted: None,
            estimate: None,
            expect: 1,
            before: None,
            before_error: None,
        }
    }

    #[test]
    fn an_uncounted_statement_says_to_pass_expect_rows() {
        let why = uncounted_refusal(Some("an INSERT"), Some(3));
        assert!(
            why.contains("can't count this statement's rows first (an INSERT; the planner estimates about 3)"),
            "{why}"
        );
        assert!(why.contains("pass expect_rows"), "{why}");
    }

    #[test]
    fn a_run_is_refused_when_its_statement_checks_differently_than_planned() {
        let (open, _) =
            crate::safety::SafetyConfig::parse_personal("[databases.ledger]\nread_only = false\n");
        let ro = crate::safety::SafetyConfig::default();
        let keyed = "UPDATE a SET x = 1 WHERE id = 1";
        assert_eq!(recheck(&plan(keyed, &open, false), &open), Ok(()));
        // Planned writable, read-only now: the lift wasn't confirmed.
        let why = recheck(&plan(keyed, &open, false), &ro).unwrap_err();
        assert!(why.contains("read-only"), "{why}");
        // Planned with a lift, writable now: it checks differently.
        let why = recheck(&plan(keyed, &ro, false), &open).unwrap_err();
        assert!(why.contains("plan again"), "{why}");
    }

    #[test]
    fn the_name_is_typed_when_protected_lifted_or_every_row_changes() {
        let (open, _) = crate::safety::SafetyConfig::parse_personal(
            "[databases.ledger]\nread_only = false\n[databases.ledger.guards]\nupdate_without_where = \"confirm\"\n",
        );
        let keyed = "UPDATE a SET x = 1 WHERE id = 1";
        assert_eq!(plan(keyed, &open, false).typed_reason(), None);
        assert_eq!(
            plan(keyed, &open, true).typed_reason(),
            Some("is protected")
        );
        let ro = crate::safety::SafetyConfig::default();
        assert_eq!(
            plan(keyed, &ro, false).typed_reason(),
            Some("runs what its safety settings refuse, for this run only")
        );
        assert_eq!(
            plan("UPDATE a SET x = 1", &open, false).typed_reason(),
            Some("would have every row of the table changed")
        );
    }
}
