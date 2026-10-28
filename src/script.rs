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

/// `base` with its database swapped for `db`. Pure.
fn with_database(base: &Resolved, db: &str) -> Resolved {
    let mut t = base.clone();
    t.connect.dsn.dbname = db.to_string();
    t.database = db.to_string();
    t
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
        (Some((i, msg)), _) => format!(
            "statement {} of {total} failed on {db}: {msg}\n  statement: {}",
            before + i + 1,
            summary(&statements[*i])
        ),
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
    let meta =
        std::fs::metadata(&opts.file).map_err(|e| format!("{}: {e}", opts.file.display()))?;
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
    let protected_name = protected.then(|| {
        base.environment
            .clone()
            .unwrap_or_else(|| base.label.clone())
    });
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
        let (prompt, accept) = question(
            &plan,
            &opts.file.display().to_string(),
            &base,
            protected_name.as_deref(),
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
            if tokio::signal::ctrl_c().await.is_ok() {
                stop.store(true, Ordering::SeqCst);
                let token = cancel.lock().ok().and_then(|t| t.clone());
                if let Some(token) = token {
                    let _ = token.cancel_query(tokio_postgres::NoTls).await;
                }
            }
        });
    }
    let (mut before, mut committed) = (0, 0);
    let mut dbs = Vec::new();
    for (i, (seg, t)) in plan.segments.iter().zip(&targets).enumerate() {
        let timeout = safety.profile_for(&seg.database).statement_timeout_ms;
        let connected = async {
            let opened = crate::pgtool::open(t).await?;
            let client =
                crate::conn::connect_via_writable(opened.dsn.clone(), opened.via, timeout).await?;
            script::prepare_session(&client).await?;
            Ok::<_, String>((opened, client))
        }
        .await;
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
        dbs.push(format!("{} on {}", o.ran, seg.database));
    }
    tracing::info!(target: "pgman::run", statements = before, "script done");
    Ok(format!(
        "ran {before} statement(s) ({}): {} transaction(s) committed, {} statement(s) committed as they ran",
        dbs.join(", "),
        plan.transactions,
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
        let (q, accept) = question(&p, "x.sql", &base(), None);
        assert!(
            q.contains("5 statement(s) — 1 transaction(s), 2 statement(s) outside one"),
            "{q}"
        );
        assert!(q.contains("then on ledgerlog: 1 statement(s)"), "{q}");
        assert!(q.contains("including: TRUNCATE on ledger"), "{q}");
        assert!(q.ends_with("[y/N] ") && accept == "y", "{q}");
        let (q, accept) = question(&p, "x.sql", &base(), Some("live"));
        assert!(
            q.ends_with("type live to go on: ") && accept == "live",
            "{q}"
        );
    }

    #[test]
    fn a_stopped_run_says_what_stayed_committed_and_what_was_undone() {
        let p = plan_of("create table t (id int);\nBEGIN;\ninsert into t values (1);\ninsert into t values (2);\nCOMMIT;\nvacuum t;\n");
        let o = Outcome {
            ran: 3,
            committed: 1,
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
    fn a_later_segment_counts_on_from_the_earlier_ones() {
        let p = plan_of("select 1;\nselect 2;\n\\c ledgerlog\nselect 3;\nselect 4;\n");
        let o = Outcome {
            ran: 0,
            committed: 0,
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
