//! A SQL script that runs its own transactions: what `pgman run` takes.
//!
//! Unlike a patch (`super::apply`), which pgman wraps in one transaction
//! of its own, a script here says where its transactions begin and end
//! (`BEGIN … COMMIT`), and anything outside them commits as it runs —
//! the way `psql -f` runs it. So pgman can say exactly what stayed
//! committed when a statement fails: the statements that ran outside a
//! transaction, and the transactions that reached their `COMMIT`. The
//! transaction open at the failure is rolled back.
//!
//! A script may switch database with psql's `\c DATABASE` on a line of
//! its own — a second database on the same server, such as a logging
//! one — so it splits into [`Segment`]s, one per database. No other psql
//! meta-command is read, and a transaction can't stay open across a
//! `\c` (psql would drop it with the connection).
//!
//! [`split`], [`tx_effect`] and [`plan`] are pure; [`run`] is the
//! execution on a connection the caller opened.

/// Part of a script that runs on one database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// From the `\c` that starts it; `None` for the script's start,
    /// which runs on the target's own database.
    pub database: Option<String>,
    /// The line the segment's SQL starts on (1-based).
    pub line: usize,
    pub sql: String,
}

/// A meta-command's arguments as psql reads them: separated by
/// whitespace, a double-quoted one kept whole (`""` inside for one `"`).
/// `None` for an unclosed quote. Pure.
fn meta_args(rest: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            return Some(out);
        };
        let mut arg = String::new();
        if first == '"' {
            chars.next();
            loop {
                match chars.next()? {
                    '"' if chars.next_if_eq(&'"').is_some() => arg.push('"'),
                    '"' => break,
                    c => arg.push(c),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                arg.push(c);
            }
        }
        out.push(arg);
    }
}

/// Split `text` at its `\c DATABASE` lines. Any other line starting with
/// `\` is a psql meta-command pgman doesn't run, and is refused. Pure.
pub fn split(text: &str) -> Result<Vec<Segment>, String> {
    let mut out = vec![Segment {
        database: None,
        line: 1,
        sql: String::new(),
    }];
    for (n, line) in text.lines().enumerate() {
        let lineno = n + 1;
        let trimmed = line.trim();
        let Some(meta) = trimmed.strip_prefix('\\') else {
            let seg = out.last_mut().expect("never empty");
            seg.sql.push_str(line);
            seg.sql.push('\n');
            continue;
        };
        let (cmd, rest) = meta.split_once(char::is_whitespace).unwrap_or((meta, ""));
        if cmd != "c" && cmd != "connect" {
            return Err(format!(
                "line {lineno}: \\{cmd} is a psql meta-command pgman doesn't run; only \\c DATABASE is read"
            ));
        }
        let db = match meta_args(rest).as_deref() {
            Some([one]) if !one.is_empty() => Some(one.clone()),
            _ => None,
        }
        .ok_or_else(|| {
            format!(
                "line {lineno}: \\{cmd} takes one database name here (another user, host or port isn't followed)"
            )
        })?;
        out.push(Segment {
            database: Some(db),
            line: lineno + 1,
            sql: String::new(),
        });
    }
    Ok(out)
}

/// What a statement does to the session's transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxEffect {
    /// `BEGIN`, `START TRANSACTION`.
    Begin,
    /// `COMMIT`, `END`.
    Commit,
    /// `ROLLBACK`, `ABORT` (not `ROLLBACK TO SAVEPOINT`).
    Rollback,
    /// `COMMIT AND CHAIN` / `ROLLBACK AND CHAIN`: ends one, begins the
    /// next. `true` when it commits.
    Chain(bool),
    /// Two-phase commit (`PREPARE TRANSACTION`, `COMMIT PREPARED`…),
    /// which pgman doesn't run.
    TwoPhase,
    /// Anything else.
    None,
}

/// [`TxEffect`] of `stmt`. Pure.
pub fn tx_effect(stmt: &str) -> TxEffect {
    let toks = crate::safety::code_tokens(&crate::safety::strip_sql_comments(stmt));
    let t = |i: usize| toks.get(i).map(String::as_str);
    let chained = toks.windows(2).any(|w| w[0] == "and" && w[1] == "chain")
        && !toks
            .windows(3)
            .any(|w| w[0] == "and" && w[1] == "no" && w[2] == "chain");
    match (t(0), t(1)) {
        (Some("prepare"), Some("transaction")) => TxEffect::TwoPhase,
        (Some("commit" | "rollback"), Some("prepared")) => TxEffect::TwoPhase,
        (Some("begin"), _) | (Some("start"), Some("transaction")) => TxEffect::Begin,
        (Some("rollback"), Some("to")) | (Some("rollback"), Some("work" | "transaction"))
            if t(1) == Some("to") || t(2) == Some("to") =>
        {
            TxEffect::None
        }
        (Some("commit" | "end"), _) if chained => TxEffect::Chain(true),
        (Some("rollback" | "abort"), _) if chained => TxEffect::Chain(false),
        (Some("commit" | "end"), _) => TxEffect::Commit,
        (Some("rollback" | "abort"), _) => TxEffect::Rollback,
        _ => TxEffect::None,
    }
}

/// A segment, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub database: String,
    pub statements: Vec<String>,
    /// What safety.toml's `Confirm` guards would ask about
    /// (`super::apply::guard_confirms`), answered by the one confirmation.
    pub confirms: Vec<String>,
}

/// A script, checked and ready to confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub segments: Vec<Planned>,
    /// How many transactions it begins.
    pub transactions: usize,
    /// Statements that run outside any transaction, each committing as
    /// it runs (transaction control itself not counted).
    pub autocommit: usize,
}

impl Plan {
    pub fn statements(&self) -> usize {
        self.segments.iter().map(|s| s.statements.len()).sum()
    }
}

/// Check `segments` against `safety`, `target_db` being the database a
/// segment without a `\c` runs on. Refused: a database the profile
/// keeps read-only, a statement a `Block` guard stops, a script the
/// splitter can't account for, unicode-escaped names, two-phase commit,
/// and a transaction left open at a `\c` or at the end. Pure.
pub fn plan(
    safety: &crate::safety::SafetyConfig,
    target_db: &str,
    segments: &[Segment],
) -> Result<Plan, String> {
    let mut out = Vec::new();
    let mut transactions = 0;
    let mut autocommit = 0;
    let mut open_since: Option<usize> = None;
    let mut index = 0;
    for seg in segments {
        let db = seg.database.as_deref().unwrap_or(target_db);
        if let (Some(begun), Some(next)) = (open_since, &seg.database) {
            return Err(format!(
                "the transaction begun at statement {begun} is still open at \\c {next} (line {}); \
                 end it with COMMIT first",
                seg.line - 1
            ));
        }
        if seg.sql.trim().is_empty() {
            continue;
        }
        if safety.profile_for(db).read_only {
            return Err(read_only_refusal(safety, db));
        }
        let statements =
            crate::batch::check_statements(safety, db, &seg.sql, true).map_err(|r| match r {
                crate::batch::Refusal::Block { kind, stmt, .. }
                | crate::batch::Refusal::Confirm { kind, stmt, .. } => {
                    format!("blocked by safety on {db}: {kind} (statement: {stmt})")
                }
                crate::batch::Refusal::Split => crate::safety::SPLIT_REFUSAL.to_string(),
                crate::batch::Refusal::ReadOnlyEscape => {
                    crate::safety::READ_ONLY_ESCAPE_REFUSAL.to_string()
                }
                crate::batch::Refusal::ServerControl => {
                    crate::safety::SERVER_CONTROL_REFUSAL.to_string()
                }
            })?;
        for s in &statements {
            index += 1;
            if crate::safety::has_unicode_escape(s) {
                return Err(format!(
                    "statement {index} uses unicode-escaped names (U&\"…\"), which pgman can't check"
                ));
            }
            match tx_effect(s) {
                TxEffect::TwoPhase => {
                    return Err(format!(
                        "statement {index} is two-phase commit, which pgman doesn't run"
                    ))
                }
                TxEffect::Begin => {
                    if open_since.is_none() {
                        transactions += 1;
                        open_since = Some(index);
                    }
                }
                TxEffect::Commit | TxEffect::Rollback => open_since = None,
                TxEffect::Chain(_) => {
                    if open_since.is_some() {
                        transactions += 1;
                        open_since = Some(index);
                    }
                }
                TxEffect::None if open_since.is_none() => autocommit += 1,
                TxEffect::None => {}
            }
        }
        // The script's own BEGIN / COMMIT are what it is for, not
        // something to single out.
        let asked: Vec<String> = statements
            .iter()
            .filter(|s| !crate::safety::is_transaction_control(s))
            .cloned()
            .collect();
        out.push(Planned {
            database: db.to_string(),
            confirms: super::apply::guard_confirms(safety, db, &asked),
            statements,
        });
    }
    if let Some(begun) = open_since {
        return Err(format!(
            "the script ends inside the transaction begun at statement {begun}; end it with COMMIT"
        ));
    }
    if out.is_empty() {
        return Err("the script has no statements".to_string());
    }
    Ok(Plan {
        segments: out,
        transactions,
        autocommit,
    })
}

/// Why a script can't run on `db`, which `safety` keeps read-only: where
/// that comes from, and the lines that change it. Pure.
pub fn read_only_refusal(safety: &crate::safety::SafetyConfig, db: &str) -> String {
    let origin = safety.read_only_origin(db);
    if let Some(block) = origin.project.filter(|_| !origin.personal) {
        return format!(
            "{db} is kept read-only by this project's .pgman/pgman.toml ({block}), which only \
             tightens; change it there to run scripts"
        );
    }
    let why = if origin.personal_own_entry {
        "safety.toml keeps it read-only"
    } else {
        "read-only is the default"
    };
    format!(
        "{db} is read-only ({why}). To run scripts on it, set in your safety.toml \
         (pgman --init-config writes one):\n  [databases.{db}]\n  read_only = false"
    )
}

/// How far a segment's run got.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Statements that ran.
    pub ran: usize,
    /// Of those, the ones whose work is committed: run outside a
    /// transaction, or in one that reached its COMMIT.
    pub committed: usize,
    /// Transactions that reached their COMMIT.
    pub transactions: usize,
    /// The statement (index in the segment) that failed, and why.
    pub failed: Option<(usize, String)>,
    /// Stopped (Ctrl-C) before the statement at this index.
    pub cancelled: Option<usize>,
    /// The open transaction rolled back after a failure or cancel: the
    /// index of its BEGIN and how many statements it had run.
    pub rolled_back: Option<(usize, usize)>,
}

/// Set up a session for [`run`]: each statement waits at most
/// [`super::apply::LOCK_TIMEOUT`] for a lock, so a busy table fails the
/// script fast rather than queueing it behind live traffic. A script's
/// own `SET lock_timeout` overrides it.
pub async fn prepare_session(client: &tokio_postgres::Client) -> Result<(), String> {
    client
        .batch_execute(&format!(
            "SET lock_timeout = '{}'",
            super::apply::LOCK_TIMEOUT
        ))
        .await
        .map_err(|e| format!("could not set lock_timeout: {}", message(&e)))
}

/// Run `statements` in order on `client`, each through the extended
/// protocol (one command per string, as checked), stopping at the first
/// failure or when `stop` says so; an open transaction then rolls back.
pub async fn run(
    client: &tokio_postgres::Client,
    statements: &[String],
    stop: &(dyn Fn() -> bool + Send + Sync),
) -> Outcome {
    let mut o = Outcome::default();
    let mut open: Option<(usize, usize)> = None;
    for (i, stmt) in statements.iter().enumerate() {
        if stop() {
            o.cancelled = Some(i);
            break;
        }
        // What can't run in a transaction (VACUUM, CONCURRENTLY…) can
        // take hours, and a concurrent build cut short is left INVALID:
        // no statement timeout for it, as `apply --outside` does. The
        // lock timeout stays.
        let long = open.is_none() && super::apply::needs_no_transaction(stmt).is_some();
        let ran = if long {
            run_untimed(client, stmt).await
        } else {
            client.execute(stmt.as_str(), &[]).await.map(drop)
        };
        if let Err(e) = ran {
            o.failed = Some((i, message(&e)));
            break;
        }
        o.ran += 1;
        match tx_effect(stmt) {
            TxEffect::Begin if open.is_none() => open = Some((i, 0)),
            TxEffect::Commit => {
                if let Some((_, n)) = open.take() {
                    o.committed += n;
                    o.transactions += 1;
                }
            }
            TxEffect::Rollback => open = None,
            TxEffect::Chain(commits) => {
                if let Some((_, n)) = open.take() {
                    if commits {
                        o.committed += n;
                        o.transactions += 1;
                    }
                }
                open = Some((i, 0));
            }
            _ => match open.as_mut() {
                Some((_, n)) => *n += 1,
                None => o.committed += 1,
            },
        }
    }
    if let Some(tx) = open {
        if o.failed.is_some() || o.cancelled.is_some() {
            let _ = client.batch_execute("ROLLBACK").await;
            o.rolled_back = Some(tx);
        }
    }
    o
}

/// `stmt` with the session's statement timeout lifted, then put back.
async fn run_untimed(
    client: &tokio_postgres::Client,
    stmt: &str,
) -> Result<(), tokio_postgres::Error> {
    let saved: String = client
        .query_one("SHOW statement_timeout", &[])
        .await?
        .get(0);
    client.batch_execute("SET statement_timeout = 0").await?;
    let ran = client.execute(stmt, &[]).await.map(drop);
    let restored = client
        .execute(
            "SELECT pg_catalog.set_config('statement_timeout', $1, false)",
            &[&saved],
        )
        .await
        .map(drop);
    ran.and(restored)
}

fn message(e: &tokio_postgres::Error) -> String {
    e.as_db_error()
        .map(|d| d.message().to_string())
        .unwrap_or_else(|| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writable() -> crate::safety::SafetyConfig {
        crate::safety::SafetyConfig::parse_personal("[default]\nread_only = false\n").0
    }

    #[test]
    fn a_script_splits_at_its_connect_lines() {
        let segs = split("insert into a values (1);\n\\c ledgerlog\ninsert into log values (1);\n\\connect \"Odd \"\"name\"\"\"\nselect 1;\n").unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].database, None);
        assert_eq!(segs[1].database.as_deref(), Some("ledgerlog"));
        assert_eq!(segs[1].line, 3);
        assert_eq!(segs[1].sql, "insert into log values (1);\n");
        assert_eq!(segs[2].database.as_deref(), Some("Odd \"name\""));
    }

    #[test]
    fn other_meta_commands_and_connect_options_are_refused() {
        assert!(split("\\set x 1\n").unwrap_err().contains("line 1: \\set"));
        assert!(split("select 1;\n  \\i other.sql\n")
            .unwrap_err()
            .contains("line 2: \\i"));
        assert!(split("\\c\n").unwrap_err().contains("one database name"));
        assert!(split("\\c ledger app_ro db.example\n")
            .unwrap_err()
            .contains("isn't followed"));
        assert!(split("\\c \"unclosed\n")
            .unwrap_err()
            .contains("one database name"));
        assert_eq!(
            split("\\c \"two words\"\n").unwrap()[1].database.as_deref(),
            Some("two words")
        );
    }

    #[test]
    fn transaction_statements_are_told_apart() {
        assert_eq!(tx_effect("BEGIN"), TxEffect::Begin);
        assert_eq!(
            tx_effect("begin isolation level serializable"),
            TxEffect::Begin
        );
        assert_eq!(tx_effect("START TRANSACTION READ WRITE"), TxEffect::Begin);
        assert_eq!(tx_effect("COMMIT"), TxEffect::Commit);
        assert_eq!(tx_effect("end work"), TxEffect::Commit);
        assert_eq!(tx_effect("ROLLBACK"), TxEffect::Rollback);
        assert_eq!(tx_effect("abort"), TxEffect::Rollback);
        assert_eq!(tx_effect("ROLLBACK TO SAVEPOINT s"), TxEffect::None);
        assert_eq!(tx_effect("rollback work to s"), TxEffect::None);
        assert_eq!(tx_effect("SAVEPOINT s"), TxEffect::None);
        assert_eq!(tx_effect("COMMIT AND CHAIN"), TxEffect::Chain(true));
        assert_eq!(tx_effect("COMMIT AND NO CHAIN"), TxEffect::Commit);
        assert_eq!(tx_effect("rollback and chain"), TxEffect::Chain(false));
        assert_eq!(tx_effect("PREPARE TRANSACTION 'x'"), TxEffect::TwoPhase);
        assert_eq!(tx_effect("COMMIT PREPARED 'x'"), TxEffect::TwoPhase);
        assert_eq!(tx_effect("-- begin\nselect 1"), TxEffect::None);
        assert_eq!(tx_effect("insert into t values ('begin')"), TxEffect::None);
    }

    fn segs(s: &str) -> Vec<Segment> {
        split(s).unwrap()
    }

    #[test]
    fn a_plan_counts_transactions_and_what_commits_as_it_runs() {
        let p = plan(
            &writable(),
            "ledger",
            &segs("create table t (id int);\nBEGIN;\ninsert into t values (1);\nCOMMIT;\nvacuum t;\n\\c ledgerlog\nBEGIN;\nselect 1;\nCOMMIT;\n"),
        )
        .unwrap();
        assert_eq!(p.segments.len(), 2);
        assert_eq!(p.segments[0].database, "ledger");
        assert_eq!(p.segments[1].database, "ledgerlog");
        assert_eq!(p.statements(), 8);
        assert_eq!(p.transactions, 2);
        assert_eq!(p.autocommit, 2, "the CREATE and the VACUUM");
    }

    #[test]
    fn an_open_transaction_at_a_connect_or_the_end_is_refused() {
        let e = plan(
            &writable(),
            "ledger",
            &segs("BEGIN;\nselect 1;\n\\c ledgerlog\nselect 1;\nCOMMIT;\n"),
        )
        .unwrap_err();
        assert!(
            e.contains("statement 1 is still open at \\c ledgerlog (line 3)"),
            "{e}"
        );
        let e = plan(
            &writable(),
            "ledger",
            &segs("BEGIN;\ninsert into t values (1);\n"),
        )
        .unwrap_err();
        assert!(
            e.contains("ends inside the transaction begun at statement 1"),
            "{e}"
        );
    }

    #[test]
    fn a_read_only_database_or_a_blocked_statement_is_refused() {
        let mut safety = writable();
        let e = plan(
            &crate::safety::SafetyConfig::default(),
            "ledger",
            &segs("select 1;\n"),
        )
        .unwrap_err();
        assert!(
            e.contains("ledger is read-only (read-only is the default)"),
            "{e}"
        );
        assert!(e.contains("[databases.ledger]\n  read_only = false"), "{e}");
        safety
            .databases
            .insert("ledgerlog".into(), crate::safety::SafetyProfile::default());
        let e = plan(
            &safety,
            "ledger",
            &segs("select 1;\n\\c ledgerlog\nselect 1;\n"),
        )
        .unwrap_err();
        assert!(
            e.contains("ledgerlog is read-only (safety.toml keeps it read-only)"),
            "the \\c database's own profile: {e}"
        );
        let e = plan(&writable(), "ledger", &segs("drop table t;\n")).unwrap_err();
        assert!(e.contains("blocked by safety on ledger"), "{e}");
        let e = plan(&writable(), "ledger", &segs("PREPARE TRANSACTION 'x';\n")).unwrap_err();
        assert!(e.contains("two-phase"), "{e}");
        assert!(plan(&writable(), "ledger", &segs("\n\n"))
            .unwrap_err()
            .contains("no statements"));
    }

    #[test]
    fn a_plan_names_what_the_guards_would_ask_about() {
        let p = plan(
            &writable(),
            "ledger",
            &segs("BEGIN;\ntruncate t;\nCOMMIT;\n"),
        )
        .unwrap();
        assert_eq!(p.segments[0].confirms, vec!["TRUNCATE".to_string()]);
    }
}
