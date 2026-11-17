//! One statement written to a database, after the operator confirms
//! it: MCP's `execute` and `pgman execute`.
//!
//! Two steps, as patching takes them ([`super::patching`]):
//!
//! 1. [`plan_for`] finds the database ([`super::target::resolve`], the
//!    primary), checks the statement ([`query::execute::check`]),
//!    connects read-only, and counts the rows it changes, with the
//!    current values an `UPDATE` overwrites, or asks the planner for an
//!    estimate where it can't count them.
//! 2. After the confirmation, [`run_for`] finds it again — it must be
//!    where it was ([`ExecPlan::endpoint`]) — connects writable, a
//!    standby refused, and runs exactly the statement shown, once, in
//!    one transaction, committing only when it changed the rows
//!    confirmed.
//!
//! Only a primary (or a plain server) is written to; a replica is
//! refused. Protected as the target says ([`super::target`]): an
//! environment by `protected = true` or a name containing "prod"; a
//! connection when an environment listing it, or its own name, is.
//!
//! [`query::execute::check`]: crate::query::execute::check

use super::target::{self, Resolved, Want};
use crate::mcp::connections::Role;
use crate::query::execute::{self, Before, Checked};

/// How the database was named.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Named {
    /// An environment's database, all three given: MCP.
    Environment {
        environment: String,
        server: String,
        database: String,
    },
    /// A connection in the personal connections.toml, by name: MCP. Not
    /// a URL, and not an environment's name.
    Connection(String),
    /// Any target `pgman dump` takes, a URL included: the command line,
    /// where the operator typed it.
    Any(String),
}

impl Named {
    /// The text [`target::resolve`] reads, or why it can't be one.
    /// Pure.
    pub fn text(&self) -> Result<String, String> {
        match self {
            Named::Environment {
                environment,
                server,
                database,
            } => {
                if [environment, server, database]
                    .iter()
                    .any(|p| p.is_empty() || p.contains('/'))
                {
                    return Err("environment, server and database are names, without a /".into());
                }
                Ok(format!("{environment}/{server}/{database}"))
            }
            Named::Connection(name) => {
                if name.is_empty() || name.contains("://") || name.contains('/') {
                    return Err(format!(
                        "connection {name:?} isn't a connection's name; execute takes a name from \
                         connections.toml, never a URL"
                    ));
                }
                Ok(name.clone())
            }
            Named::Any(text) => Ok(text.clone()),
        }
    }

    /// Whether `r` is the database this names: an environment's the
    /// same three names, a connection one outside any environment.
    /// Pure.
    pub fn matches(&self, r: &Resolved) -> Result<(), String> {
        match self {
            Named::Environment {
                environment,
                server,
                database,
            } if r.environment.as_deref() != Some(environment.as_str())
                || &r.server != server
                || &r.database != database =>
            {
                Err(format!("{environment} has no {server}/{database}"))
            }
            Named::Connection(name) if r.environment.is_some() => Err(format!(
                "{name:?} names an environment; give environment, server and database instead"
            )),
            _ => Ok(()),
        }
    }
}

/// What [`run_for`] would do, for the operator to confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPlan {
    /// How it was named: the run finds it again from this.
    pub named: Named,
    /// The environment, for an environment's database.
    pub environment: Option<String>,
    pub server: String,
    pub database: String,
    /// How the target was named, for messages.
    pub label: String,
    pub protected: bool,
    /// Why, when it is.
    pub protected_why: Option<String>,
    /// Where it connects, password never included: what the operator is
    /// shown, and what the run must find again.
    pub endpoint: String,
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

    /// The name typed to confirm: the environment's, else the target's.
    /// Pure.
    pub fn confirm_name(&self) -> &str {
        self.environment.as_deref().unwrap_or(&self.label)
    }

    /// Where it runs, for the operator: `demo db1/ledger`, or `app-dev
    /// (ledger)` for a connection. Pure.
    pub fn place(&self) -> String {
        match &self.environment {
            Some(env) => format!("{env} {}/{}", self.server, self.database),
            None => format!("{} ({})", self.label, self.database),
        }
    }

    /// Why confirming takes the name typed, if it does. Pure.
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

/// `safety` placed where `r` is: an environment's `ENV/…` entries
/// apply to its databases, bare names alone to anything else. Pure.
fn scoped(safety: &crate::safety::SafetyConfig, r: &Resolved) -> crate::safety::SafetyConfig {
    match &r.environment {
        Some(env) => safety.scoped(env, Some(&r.server)),
        None => safety.unscoped(),
    }
}

/// `named`, found as a primary (blocking: the aws CLI may run), and
/// checked to be what it names. A replica the resolver refuses itself.
async fn locate(named: &Named) -> Result<Resolved, String> {
    let text = named.text()?;
    let r = tokio::task::spawn_blocking(move || target::resolve(&text, Want::Primary))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))??;
    named.matches(&r)?;
    Ok(r)
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
                _ => "not an UPDATE or DELETE of one table pgman can read".to_string(),
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
         expect_rows (pgman execute: --expect-rows) with the number it must change, and it is \
         rolled back if it changes any other",
        why.unwrap_or("no count"),
        estimate
            .map(|e| format!("; the planner estimates about {e}"))
            .unwrap_or_default()
    )
}

/// The plan for running `sql` once on the database `named` names.
/// `safety` is the personal safety.toml as the project's `[safety]`
/// tightened it. With `floor`, the project's rules alone
/// (`project::project_floor`), what `safety` refuses the operator may
/// lift for this run, never what `floor` does; without, it is refused.
pub async fn plan_for(
    named: Named,
    sql: String,
    expect_rows: Option<u64>,
    safety: crate::safety::SafetyConfig,
    floor: Option<crate::safety::SafetyConfig>,
) -> Result<ExecPlan, String> {
    // What no lift answers is refused before anything is looked up.
    execute::check(&safety.unscoped(), "", &sql, true).map_err(|r| r.why)?;
    let r = locate(&named).await?;
    let (safety, database) = (scoped(&safety, &r), r.database.clone());
    let checked = match (execute::check(&safety, &database, &sql, false), &floor) {
        (Ok(c), _) => c,
        (Err(e), Some(floor)) if e.liftable => {
            let floor = scoped(floor, &r);
            if let Err(f) = execute::check(&floor, &database, &sql, false) {
                return Err(super::patching::project_refusal(&floor, &database, &f));
            }
            execute::check(&safety, &database, &sql, true).map_err(|e| e.why)?
        }
        (Err(e), _) => return Err(e.why),
    };
    let endpoint = endpoint(&r);
    let dsn = super::patching::spec_dsn(r.connect.clone(), &super::patching::aws_secrets()).await?;
    let (client, _tunnel) = super::patching::connect(dsn, true, 10_000).await?;
    if crate::query::apply::in_recovery(&client).await? {
        return Err(standby_refusal(&r.server));
    }
    let (rows, uncounted, estimate, before, before_error) =
        read_plan(&client, &checked, expect_rows).await?;
    Ok(ExecPlan {
        named,
        environment: r.environment.clone(),
        server: r.server.clone(),
        database,
        label: r.label.clone(),
        protected: r.protected,
        protected_why: r.protected_why.clone(),
        endpoint,
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

/// Where `r` connects, user, host, port, database and any ssh tunnel,
/// never a password. Pure.
pub fn endpoint(r: &Resolved) -> String {
    let mut dsn = r.connect.dsn.clone();
    dsn.password = None;
    dsn.redacted()
}

/// Refused unless the run connects where the plan did (`now`): the
/// operator confirmed that database, and environments.toml,
/// connections.toml or the RDS listing may say otherwise since. Pure.
fn same_endpoint(plan: &ExecPlan, now: &str) -> Result<(), String> {
    if plan.endpoint == now {
        return Ok(());
    }
    Err(format!(
        "nothing run: {} resolves to {now} now, not the {} the operator confirmed; plan again",
        plan.place(),
        plan.endpoint
    ))
}

/// The refusal for a replica. Pure.
fn replica_refusal(r: &Resolved) -> String {
    format!(
        "{} is a replica; writes go to its primary{}",
        r.server,
        r.replica_of
            .as_deref()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default()
    )
}

/// The refusal for a server that says it is a standby. Pure.
fn standby_refusal(server: &str) -> String {
    format!("{server} is a standby (in recovery); writes go to its primary")
}

/// The audit line for `plan` confirmed: `how` (typed the name,
/// ticked) and by whom (`who`: the MCP client, the terminal).
pub fn audit_confirmed(plan: &ExecPlan, how: &str, who: &str) {
    tracing::info!(target: "pgman::execute", place = %plan.place(), endpoint = %plan.endpoint, statement = ?plan.checked.statement, expect = plan.expect, lifted = ?plan.lifted(), confirmed = how, by = %who, os_user = %std::env::var("USER").unwrap_or_default(), "execute confirmed");
}

/// The audit line for `plan` not confirmed: `why`, quoted, since it
/// can carry the client's answer.
pub fn audit_not_confirmed(plan: &ExecPlan, why: &str) {
    tracing::info!(target: "pgman::execute", place = %plan.place(), endpoint = %plan.endpoint, statement = ?plan.checked.statement, why = ?why, "execute not confirmed");
}

/// The audit line for what `plan`'s run did: the rows and the values
/// it overwrote (a secret's column withheld, [`Before::for_audit`]), or
/// why nothing was kept.
pub fn audit_outcome(plan: &ExecPlan, run: &Result<u64, String>) {
    match run {
        Ok(rows) => {
            let before = plan.before.as_ref().map(|b| b.for_audit().to_string());
            tracing::info!(target: "pgman::execute", place = %plan.place(), endpoint = %plan.endpoint, statement = ?plan.checked.statement, rows, before = ?before, "execute committed")
        }
        Err(why) => {
            tracing::warn!(target: "pgman::execute", place = %plan.place(), endpoint = %plan.endpoint, statement = ?plan.checked.statement, why = ?why, "execute not kept")
        }
    }
}

/// Run a plan from [`plan_for`], finding its database again ([`run_on`]).
/// `safety` as [`plan_for`] takes it: the rows it changed, or why
/// nothing was kept.
pub async fn run_for(plan: &ExecPlan, safety: crate::safety::SafetyConfig) -> Result<u64, String> {
    let r = locate(&plan.named).await?;
    run_on(plan, &r, &safety).await
}

/// Run `plan` on `r`, the database found again: refused when its
/// statement checks differently against `safety` now, when `r` is a
/// replica, connects elsewhere than the plan, or turns out a standby.
pub async fn run_on(
    plan: &ExecPlan,
    r: &Resolved,
    safety: &crate::safety::SafetyConfig,
) -> Result<u64, String> {
    recheck(plan, &scoped(safety, r))?;
    if r.role == Role::Replica {
        return Err(replica_refusal(r));
    }
    same_endpoint(plan, &endpoint(r))?;
    let dsn = super::patching::spec_dsn(r.connect.clone(), &super::patching::aws_secrets()).await?;
    let (client, _tunnel) =
        super::patching::connect(dsn, false, execute::STATEMENT_TIMEOUT_MS).await?;
    if crate::query::apply::in_recovery(&client).await? {
        return Err(standby_refusal(&r.server));
    }
    execute::run(&client, &plan.checked.statement, plan.expect).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(sql: &str, cfg: &crate::safety::SafetyConfig, protected: bool) -> ExecPlan {
        let checked = execute::check(cfg, "ledger", sql, true).unwrap();
        ExecPlan {
            named: Named::Environment {
                environment: "demo".into(),
                server: "db1".into(),
                database: "ledger".into(),
            },
            environment: Some("demo".into()),
            server: "db1".into(),
            database: "ledger".into(),
            label: "demo/db1/ledger".into(),
            protected,
            protected_why: None,
            endpoint: "postgres://app@db1:5432/ledger".into(),
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
    fn a_run_is_refused_where_the_database_resolves_elsewhere_now() {
        let open = crate::safety::SafetyConfig::default();
        let p = plan("UPDATE a SET x = 1 WHERE id = 1", &open, false);
        assert_eq!(same_endpoint(&p, "postgres://app@db1:5432/ledger"), Ok(()));
        let why = same_endpoint(&p, "postgres://app@db2:5432/ledger").unwrap_err();
        assert!(
            why.contains("resolves to postgres://app@db2:5432/ledger now"),
            "{why}"
        );
    }

    fn resolved(environment: Option<&str>, server: &str) -> Resolved {
        Resolved {
            label: server.into(),
            environment: environment.map(str::to_string),
            server: server.into(),
            database: "ledger".into(),
            role: Role::Direct,
            replica_of: None,
            protected: false,
            protected_why: None,
            connect: super::super::ConnectSpec {
                dsn: crate::conn::Dsn::parse("postgres://app:hunter2@db1:5432/ledger").unwrap(),
                secret: None,
                password_env: None,
            },
        }
    }

    #[test]
    fn an_endpoint_never_carries_a_password() {
        assert_eq!(
            endpoint(&resolved(None, "app-dev")),
            "postgres://app@db1:5432/ledger"
        );
    }

    #[test]
    fn a_name_is_what_it_says_an_environments_or_a_connections() {
        let env = Named::Environment {
            environment: "demo".into(),
            server: "db1".into(),
            database: "ledger".into(),
        };
        assert_eq!(env.text().unwrap(), "demo/db1/ledger");
        assert!(env.matches(&resolved(Some("demo"), "db1")).is_ok());
        assert!(env.matches(&resolved(Some("demo"), "db2")).is_err());
        assert!(env.matches(&resolved(None, "db1")).is_err());
        let slash = Named::Environment {
            environment: "demo/x".into(),
            server: "db1".into(),
            database: "ledger".into(),
        };
        assert!(slash.text().is_err());
        let conn = Named::Connection("app-dev".into());
        assert_eq!(conn.text().unwrap(), "app-dev");
        assert!(conn.matches(&resolved(None, "app-dev")).is_ok());
        let why = conn
            .matches(&resolved(Some("demo"), "app-dev"))
            .unwrap_err();
        assert!(why.contains("names an environment"), "{why}");
        for url in ["postgres://app@db1/ledger", "demo/ledger", ""] {
            assert!(Named::Connection(url.into()).text().is_err(), "{url}");
        }
        assert!(Named::Any("postgres://app@db1/ledger".into())
            .text()
            .is_ok());
    }

    #[test]
    fn an_environments_entries_apply_to_its_databases_alone() {
        let (cfg, _) = crate::safety::SafetyConfig::parse_personal(
            "[databases.\"demo/ledger\"]\nread_only = false\n",
        );
        assert!(
            !scoped(&cfg, &resolved(Some("demo"), "db1"))
                .profile_for("ledger")
                .read_only
        );
        assert!(
            scoped(&cfg, &resolved(Some("staging"), "db1"))
                .profile_for("ledger")
                .read_only
        );
        assert!(
            scoped(&cfg, &resolved(None, "app-dev"))
                .profile_for("ledger")
                .read_only
        );
    }

    #[tokio::test]
    async fn a_replica_handed_to_the_run_is_refused_before_connecting() {
        let (open, _) =
            crate::safety::SafetyConfig::parse_personal("[default]\nread_only = false\n");
        let p = plan("UPDATE a SET x = 1 WHERE id = 1", &open, false);
        let mut r = resolved(Some("demo"), "db1");
        (r.role, r.replica_of) = (Role::Replica, Some("db0".into()));
        let why = run_on(&p, &r, &open).await.unwrap_err();
        assert_eq!(why, "db1 is a replica; writes go to its primary (db0)");
    }

    #[test]
    fn a_connections_plan_is_confirmed_by_its_own_name() {
        let open = crate::safety::SafetyConfig::default();
        let mut p = plan("UPDATE a SET x = 1 WHERE id = 1", &open, true);
        assert_eq!(p.confirm_name(), "demo");
        assert_eq!(p.place(), "demo db1/ledger");
        (p.environment, p.label) = (None, "app-dev".into());
        assert_eq!(p.confirm_name(), "app-dev");
        assert_eq!(p.place(), "app-dev (ledger)");
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
