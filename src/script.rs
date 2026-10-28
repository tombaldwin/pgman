//! `pgman run <target> <FILE>`: a SQL script that runs its own
//! transactions, through pgman's connection and guard rails.
//!
//! The script is split at its `\c DATABASE` lines and checked
//! ([`crate::query::script::plan`]) before anything connects for
//! writing: every database it touches must be writable in safety.toml,
//! and a `Block` guard refuses it. The operator then confirms the plan —
//! by typing the target's name when it is protected, which `--yes`
//! doesn't answer; `y` or `--yes` otherwise. A failure stops the script
//! and says what stayed committed.

use crate::inventory::target::{self, Resolved, Want};
use crate::query::script::{self, Outcome, Plan};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const USAGE: &str = "\
usage: pgman run <target> <FILE> [--yes]

Run a SQL script that runs its own transactions (BEGIN … COMMIT), the
way psql -f does: anything outside a transaction commits as it runs.
<target> as for pgman dump (ENV[/SERVER]/DATABASE, a connection name,
or a postgres:// URL); its primary, never a replica.

`\\c DATABASE` on a line of its own switches to another database of the
same environment (or server). No other psql meta-command is read.

Checked before it runs: every database must be writable in safety.toml,
a guard set to block refuses it, and no transaction may stay open at a
\\c or at the end. You confirm what it will run; a protected target is
confirmed by typing its name, which --yes doesn't answer. Each statement
waits at most 5s for a lock, and the profile's statement_timeout applies.

If a statement fails, the script stops there: what ran outside a
transaction, and transactions that reached COMMIT, stay committed; the
open one is rolled back. Ctrl-C does the same between or during
statements.

  pgman run dev/ledger fix-balances.sql";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    pub target: String,
    pub file: PathBuf,
    pub yes: bool,
}

/// Parse the arguments after `pgman run`. Pure.
pub fn parse_cli(args: &[String]) -> Result<Opts, String> {
    let mut positional = Vec::new();
    let mut yes = false;
    for a in args {
        match a.as_str() {
            "--yes" | "-y" => yes = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other:?}\n\n{USAGE}"))
            }
            other => positional.push(other.to_string()),
        }
    }
    match positional.as_slice() {
        [t, f] => Ok(Opts {
            target: t.clone(),
            file: PathBuf::from(f),
            yes,
        }),
        [] | [_] => Err(format!("name a target and a script\n\n{USAGE}")),
        _ => Err(format!(
            "unexpected argument {:?}\n\n{USAGE}",
            positional[2]
        )),
    }
}

/// The largest script read: a script, not a data load.
pub const MAX_SCRIPT_BYTES: u64 = 16 * 1024 * 1024;

/// What the operator is asked, and the answer that accepts. Pure.
pub fn question(
    plan: &Plan,
    file: &str,
    base: &Resolved,
    protected_name: Option<&str>,
    timeout_ms: u64,
) -> (String, String) {
    let first = &plan.segments[0];
    let mut q = format!(
        "run {file} on {}: {} statement(s) — {} transaction(s), {} statement(s) outside one, each committing as it runs",
        base.describe(),
        plan.statements(),
        plan.transactions,
        plan.autocommit
    );
    if first.database != base.database {
        q.push_str(&format!("\n  starting on {}", first.database));
    }
    for seg in &plan.segments[1..] {
        q.push_str(&format!(
            "\n  then on {}: {} statement(s)",
            seg.database,
            seg.statements.len()
        ));
    }
    q.push_str(&format!("\n  {}", limits(timeout_ms)));
    let confirms: Vec<String> = plan
        .segments
        .iter()
        .flat_map(|s| {
            s.confirms
                .iter()
                .map(move |c| format!("{c} on {}", s.database))
        })
        .collect();
    if !confirms.is_empty() {
        q.push_str(&format!("\n  including: {}", confirms.join(", ")));
    }
    match protected_name {
        Some(name) => (
            format!("{q}\n{name} is protected: type {name} to go on: "),
            name.to_string(),
        ),
        None => (format!("{q}\ngo on? [y/N] "), "y".to_string()),
    }
}

/// The target a `\c db` segment runs on: the environment's row for `db`
/// on the same server when it lists one (it may have its own secret);
/// otherwise the same server and credentials with the database swapped,
/// as psql's `\c` does. Blocking for an environment (it is listed
/// again).
fn segment_target(base: &Resolved, db: &str) -> Result<Resolved, String> {
    if db == base.database {
        return Ok(base.clone());
    }
    if let Some(env) = &base.environment {
        if let Ok(t) = target::resolve(&format!("{env}/{}/{db}", base.server), Want::Primary) {
            return Ok(t);
        }
    }
    Ok(with_database(base, db))
}

/// `base` with its database swapped for `db`: protected when `base` is,
/// or when `db`'s name says prod, as a URL naming it would be. Pure.
fn with_database(base: &Resolved, db: &str) -> Resolved {
    let mut t = base.clone();
    t.connect.dsn.dbname = db.to_string();
    t.database = db.to_string();
    t.label = db.to_string();
    t.protected = base.protected || target::named_like_prod(db);
    t
}

/// What a statement may wait and take, said before the script runs.
/// Pure.
fn limits(timeout_ms: u64) -> String {
    let run = if timeout_ms == 0 {
        "no time limit".to_string()
    } else {
        format!(
            "at most {} (statement_timeout_ms in safety.toml)",
            human_ms(timeout_ms)
        )
    };
    format!(
        "each statement waits at most {} for a lock and runs for {run}; VACUUM and CONCURRENTLY have no time limit",
        crate::query::apply::LOCK_TIMEOUT
    )
}

/// `30s`, `2m`, `1500ms`. Pure.
fn human_ms(ms: u64) -> String {
    match ms {
        m if m % 60_000 == 0 => format!("{}m", m / 60_000),
        m if m % 1000 == 0 => format!("{}s", m / 1000),
        m => format!("{m}ms"),
    }
}

/// A one-line form of `stmt` for a report. Pure.
fn summary(stmt: &str) -> String {
    let one: String = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.char_indices().nth(120) {
        Some((i, _)) => format!("{}…", &one[..i]),
        None => one,
    }
}

/// The report of a run that stopped in segment `seg` (on `db`) after
/// `before` statements of earlier segments, all committed. Pure.
pub fn stopped_report(
    plan: &Plan,
    seg: usize,
    db: &str,
    before: usize,
    committed_before: usize,
    o: &Outcome,
) -> String {
    let total = plan.statements();
    let statements = &plan.segments[seg].statements;
    let mut out = match (&o.failed, o.cancelled) {
        (Some((i, msg)), _) => {
            let stmt = &statements[*i];
            let tokens = crate::safety::code_tokens(stmt);
            let invalid = tokens.first().is_some_and(|t| t == "create")
                && tokens.iter().any(|t| t == "concurrently");
            format!(
                "statement {} of {total} failed on {db}: {msg}\n  statement: {}{}",
                before + i + 1,
                summary(stmt),
                if invalid {
                    "\n  a failed CREATE INDEX CONCURRENTLY leaves an INVALID index: drop it before trying again"
                } else {
                    ""
                }
            )
        }
        (None, Some(i)) => format!("cancelled before statement {} of {total}", before + i + 1),
        (None, None) => format!("stopped on {db}"),
    };
    let committed = committed_before + o.committed;
    out.push_str(&format!(
        "\n  committed: {committed} statement(s) — those outside a transaction and in transactions that reached COMMIT"
    ));
    if let Some((begin, n)) = o.rolled_back {
        out.push_str(&format!(
            "\n  rolled back: the transaction begun at statement {} ({n} statement(s) in it undone)",
            before + begin + 1
        ));
    }
    let not_run = total - before - o.ran - usize::from(o.failed.is_some());
    if not_run > 0 {
        out.push_str(&format!("\n  not run: the last {not_run} statement(s)"));
    }
    out
}

/// Run a script. The question and progress go to stderr; the result is
/// what to print.
pub async fn run_cli(opts: &Opts) -> Result<String, String> {
    let meta = std::fs::metadata(&opts.file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "{}: no such file (the script comes after the target: pgman run <target> <FILE>)",
                opts.file.display()
            )
        } else {
            format!("{}: {e}", opts.file.display())
        }
    })?;
    if meta.len() > MAX_SCRIPT_BYTES {
        return Err(format!(
            "{} is over {} MiB; pgman run is for scripts",
            opts.file.display(),
            MAX_SCRIPT_BYTES / 1024 / 1024
        ));
    }
    let text =
        std::fs::read_to_string(&opts.file).map_err(|e| format!("{}: {e}", opts.file.display()))?;
    let segments = script::split(&text)?;
    let t = opts.target.clone();
    let base = tokio::task::spawn_blocking(move || target::resolve(&t, Want::Primary))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))??;
    let safety = crate::project::merge_safety(
        crate::safety::SafetyConfig::load_personal().0,
        std::env::current_dir()
            .ok()
            .and_then(|cwd| crate::project::load_from(&cwd))
            .and_then(|(_, cfg)| cfg.safety)
            .as_ref(),
    );
    let plan = script::plan(&safety, &base.database, &segments)?;
    let mut targets = Vec::new();
    for seg in &plan.segments {
        let (b, db) = (base.clone(), seg.database.clone());
        targets.push(
            tokio::task::spawn_blocking(move || segment_target(&b, &db))
                .await
                .map_err(|e| format!("resolve task failed: {e}"))??,
        );
    }
    let protected = targets.iter().any(|t| t.protected);
    let protected_name = targets
        .iter()
        .find(|t| t.protected)
        .map(|t| t.environment.clone().unwrap_or_else(|| t.label.clone()));
    if protected && opts.yes {
        return Err(format!(
            "{} is protected: it is confirmed by typing its name, which --yes doesn't answer",
            protected_name.as_deref().unwrap_or_default()
        ));
    }
    if protected && !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(format!(
            "{} is protected: it is confirmed by typing its name, and stdin isn't a terminal",
            protected_name.as_deref().unwrap_or_default()
        ));
    }
    if !opts.yes {
        let timeout = safety.profile_for(&base.database).statement_timeout_ms;
        let (prompt, accept) = question(
            &plan,
            &opts.file.display().to_string(),
            &base,
            protected_name.as_deref(),
            timeout,
        );
        let ok = tokio::task::spawn_blocking(move || crate::pgtool::confirm_cli(&prompt, &accept))
            .await
            .map_err(|e| format!("prompt task failed: {e}"))??;
        if !ok {
            return Err("not confirmed; nothing ran".to_string());
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let cancel: Arc<std::sync::Mutex<Option<tokio_postgres::CancelToken>>> = Default::default();
    {
        let (stop, cancel) = (stop.clone(), cancel.clone());
        tokio::spawn(async move {
            {
                crate::pgtool::interrupted().await;
                stop.store(true, Ordering::SeqCst);
                let token = cancel.lock().ok().and_then(|t| t.clone());
                if let Some(token) = token {
                    let _ = crate::conn::cancel_query(&token).await;
                }
            }
            // The cancel may never reach the server; a second Ctrl-C
            // doesn't wait for it. The server rolls back an open
            // transaction when the connection goes.
            crate::pgtool::interrupted().await;
            eprintln!("pgman run: stopped; an open transaction is rolled back by the server");
            std::process::exit(130);
        });
    }
    let (mut before, mut committed, mut transactions) = (0, 0, 0);
    let mut dbs = Vec::new();
    for (i, (seg, t)) in plan.segments.iter().zip(&targets).enumerate() {
        let timeout = safety.profile_for(&seg.database).statement_timeout_ms;
        let connected = async {
            let opened = crate::pgtool::open(t).await?;
            let client =
                crate::conn::connect_via_writable(opened.dsn.clone(), opened.via, timeout).await?;
            if crate::query::server::facts(&client).await?.in_recovery {
                return Err(format!(
                    "{} is a standby; a script runs on a primary",
                    t.describe()
                ));
            }
            script::prepare_session(&client).await?;
            Ok::<_, String>((opened, client))
        }
        .await;
        let connected = connected.map_err(|e| crate::pgtool::explain_connect(e, t));
        let (_opened, client) = match connected {
            Ok(c) => c,
            Err(e) if i == 0 => return Err(e),
            Err(e) => {
                return Err(format!(
                    "could not connect to {} for statement {} of {}: {e}\n  committed: the {before} statement(s) before it",
                    seg.database,
                    before + 1,
                    plan.statements()
                ))
            }
        };
        if let Ok(mut c) = cancel.lock() {
            *c = Some(client.cancel_token());
        }
        tracing::info!(target: "pgman::run", database = %seg.database, statements = seg.statements.len(), "script segment starting");
        let s = stop.clone();
        let o = script::run(&client, &seg.statements, &move || s.load(Ordering::SeqCst)).await;
        if o.failed.is_some() || o.cancelled.is_some() {
            tracing::warn!(target: "pgman::run", database = %seg.database, "script stopped");
            return Err(stopped_report(
                &plan,
                i,
                &seg.database,
                before,
                committed,
                &o,
            ));
        }
        before += o.ran;
        committed += o.committed;
        transactions += o.transactions;
        dbs.push(format!("{} on {}", o.ran, seg.database));
    }
    tracing::info!(target: "pgman::run", statements = before, "script done");
    Ok(format!(
        "ran {before} statement(s) ({}): {transactions} transaction(s) committed, {} statement(s) committed as they ran",
        dbs.join(", "),
        plan.autocommit
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn a_target_and_a_file_parse() {
        let o = parse_cli(&args(&["dev/ledger", "x.sql", "--yes"])).unwrap();
        assert_eq!(o.target, "dev/ledger");
        assert_eq!(o.file, PathBuf::from("x.sql"));
        assert!(o.yes);
        assert!(parse_cli(&args(&["dev"]))
            .unwrap_err()
            .contains("a target and a script"));
        assert!(parse_cli(&args(&["a", "b", "c"]))
            .unwrap_err()
            .contains("unexpected"));
        assert!(parse_cli(&args(&["a", "b", "--no-transaction"])).is_err());
    }

    fn plan_of(sql: &str) -> Plan {
        let safety =
            crate::safety::SafetyConfig::parse_personal("[default]\nread_only = false\n").0;
        script::plan(&safety, "ledger", &script::split(sql).unwrap()).unwrap()
    }

    fn base() -> Resolved {
        target::from_url("postgres://u@localhost/ledger").unwrap()
    }

    #[test]
    fn the_question_says_what_runs_where_and_a_protected_one_wants_the_name() {
        let p = plan_of(
            "create table t (id int);\nBEGIN;\ntruncate t;\nCOMMIT;\n\\c ledgerlog\nselect 1;\n",
        );
        let (q, accept) = question(&p, "x.sql", &base(), None, 30_000);
        assert!(
            q.contains("5 statement(s) — 1 transaction(s), 2 statement(s) outside one"),
            "{q}"
        );
        assert!(q.contains("then on ledgerlog: 1 statement(s)"), "{q}");
        assert!(q.contains("including: TRUNCATE on ledger"), "{q}");
        assert!(q.ends_with("[y/N] ") && accept == "y", "{q}");
        assert!(
            q.contains(
                "waits at most 5s for a lock and runs for at most 30s (statement_timeout_ms"
            ),
            "{q}"
        );
        assert_eq!(human_ms(120_000), "2m");
        let (q, accept) = question(&p, "x.sql", &base(), Some("live"), 30_000);
        assert!(
            q.ends_with("type live to go on: ") && accept == "live",
            "{q}"
        );
    }

    #[test]
    fn connecting_to_a_prod_named_database_makes_it_protected() {
        let t = with_database(&base(), "ledger_prod");
        assert!(t.protected && t.label == "ledger_prod" && t.connect.dsn.dbname == "ledger_prod");
        assert!(!with_database(&base(), "ledgerlog").protected);
        let mut p = base();
        p.protected = true;
        assert!(
            with_database(&p, "ledgerlog").protected,
            "a protected base stays so"
        );
    }

    #[test]
    fn a_stopped_run_says_what_stayed_committed_and_what_was_undone() {
        let p = plan_of("create table t (id int);\nBEGIN;\ninsert into t values (1);\ninsert into t values (2);\nCOMMIT;\nvacuum t;\n");
        let o = Outcome {
            ran: 3,
            committed: 1,
            transactions: 0,
            failed: Some((3, "duplicate key".into())),
            cancelled: None,
            rolled_back: Some((1, 1)),
        };
        let r = stopped_report(&p, 0, "ledger", 0, 0, &o);
        assert!(
            r.starts_with("statement 4 of 6 failed on ledger: duplicate key"),
            "{r}"
        );
        assert!(r.contains("statement: insert into t values (2)"), "{r}");
        assert!(r.contains("committed: 1 statement(s)"), "{r}");
        assert!(
            r.contains("begun at statement 2 (1 statement(s) in it undone)"),
            "{r}"
        );
        assert!(r.contains("not run: the last 2 statement(s)"), "{r}");
    }

    #[test]
    fn a_failed_concurrent_build_says_to_drop_the_invalid_index() {
        let p = plan_of("CREATE INDEX CONCURRENTLY i ON t (v);\n");
        let o = Outcome {
            failed: Some((0, "canceling statement due to lock timeout".into())),
            ..Outcome::default()
        };
        let r = stopped_report(&p, 0, "ledger", 0, 0, &o);
        assert!(r.contains("leaves an INVALID index: drop it"), "{r}");
    }

    #[test]
    fn a_later_segment_counts_on_from_the_earlier_ones() {
        let p = plan_of("select 1;\nselect 2;\n\\c ledgerlog\nselect 3;\nselect 4;\n");
        let o = Outcome {
            ran: 0,
            committed: 0,
            transactions: 0,
            failed: None,
            cancelled: Some(0),
            rolled_back: None,
        };
        let r = stopped_report(&p, 1, "ledgerlog", 2, 2, &o);
        assert!(r.starts_with("cancelled before statement 3 of 4"), "{r}");
        assert!(r.contains("committed: 2"), "{r}");
        assert!(r.contains("not run: the last 2"), "{r}");
    }
}
