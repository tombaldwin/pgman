//! Applying schema patches, one at a time.
//!
//! Each apply takes the next pending patch only (the lowest id), checks
//! it against the database's safety profile and guards — the same ones
//! `--batch` uses — and runs it in ONE transaction with a short
//! `lock_timeout`, so a busy table makes it fail fast rather than queue
//! behind live traffic. Any statement failing rolls the whole patch back.
//! Afterwards the version table is read again: a patch is applied when it
//! recorded its own version row.
//!
//! What is refused before anything runs: a database the profile keeps
//! read-only; a statement a `Block` guard stops; a patch that manages its
//! own transaction (`BEGIN`/`COMMIT`), or holds a statement that can't
//! run inside one ([`needs_no_transaction`]: `VACUUM`, `CONCURRENTLY`…),
//! since pgman's transaction is what makes it all or nothing; and a patch that
//! changes session settings (`SET`, `RESET`, `set_config`) — it could lift
//! the lock timeout, or change how the server reads the next statement
//! (`standard_conforming_strings`) so that it isn't the statement pgman
//! checked. Those are run by hand. A `DO` block or a function the patch
//! calls can still reach `set_config` where the check can't see it, so
//! the lock and statement timeouts are both set again before every
//! statement: such a change lasts no longer than the statement that made
//! it. The caller confirms each patch first;
//! that confirmation is what answers the guards' `Confirm`.
//!
//! The one way past the transaction: an operator who plans with
//! `outside` (the TUI's `o`, confirmed by typing the environment's name;
//! never MCP) lets a patch holding a statement no transaction can run go
//! through [`apply_outside`] — each statement committing as it runs, so a
//! failure part-way leaves the earlier ones applied, and says so. The
//! other refusals stand.
//!
//! A dry run holds one transaction ([`begin`]) around every patch it
//! tries ([`try_patch`]) and rolls it back ([`rollback`]). A copy is made
//! with [`create_copy`] and dropped with [`drop_copy`], which takes only
//! a name [`copy_name`] made.
//!
//! Each statement goes to the server on its own through the extended
//! protocol, which refuses a string holding more than one command: what
//! the server runs is one statement, whatever its lexer makes of it.

use super::patches::{self, PatchFile, PatchesConfig};
use crate::conn;

/// How long a patch statement waits for a lock before failing.
pub const LOCK_TIMEOUT: &str = "5s";

/// How long a patch statement in a transaction may run: a table rewrite
/// takes time; waiting on a lock does not ([`LOCK_TIMEOUT`]). The
/// patching connection opens with it too.
pub const STATEMENT_TIMEOUT_MS: u64 = 600_000;

/// What makes `stmt` unable to run inside a transaction block, if
/// anything: the server refuses these there, so a patch holding one
/// can't be all or nothing. Reads code tokens only — `concurrently` in a
/// string or comment isn't one. Pure.
pub fn needs_no_transaction(stmt: &str) -> Option<&'static str> {
    let tokens = crate::safety::code_tokens(stmt);
    let t = |i: usize| tokens.get(i).map(String::as_str);
    // REFRESH MATERIALIZED VIEW CONCURRENTLY runs in a transaction fine.
    if t(0) != Some("refresh") && tokens.iter().any(|t| t == "concurrently") {
        return Some("CONCURRENTLY");
    }
    match (t(0), t(1)) {
        (Some("vacuum"), _) => Some("VACUUM"),
        (Some("create" | "drop"), Some("database")) => Some("CREATE or DROP DATABASE"),
        (Some("create" | "drop"), Some("tablespace")) => Some("CREATE or DROP TABLESPACE"),
        (Some("alter"), Some("system")) => Some("ALTER SYSTEM"),
        (Some("reindex"), Some("system" | "database")) => Some("REINDEX SYSTEM or DATABASE"),
        _ => None,
    }
}

/// The next patch to apply: the lowest-id pending one, if any. Pure.
pub fn next_patch(files: &[PatchFile], applied: &[i64]) -> Option<PatchFile> {
    patches::reconcile(files.to_vec(), applied)
        .pending
        .into_iter()
        .next()
}

/// A patch [`check_patch`] passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub statements: Vec<String>,
    /// What can't run in a transaction ([`needs_no_transaction`]), when
    /// the patch holds one: it then runs statement by statement, each
    /// committing as it goes ([`apply_outside`]). Only with `outside`.
    pub outside: Option<&'static str>,
    /// The risky statements a `Confirm` guard would ask about, in order
    /// ([`guard_confirms`]): the plan's one confirmation answers them, so
    /// the plan names them.
    pub confirms: Vec<String>,
}

/// What each of `statements` that safety.toml's guards would ask about
/// for `db` (a `Confirm`) is, in order: `TRUNCATE`, `DELETE without
/// WHERE`… Left out: an `INSERT`, a keyed `UPDATE` / `DELETE` and DDL —
/// the default guards ask about those too, and they are what a migration
/// is made of, so naming them on every plan would hide the few that
/// matter. Pure.
pub fn guard_confirms(
    safety: &crate::safety::SafetyConfig,
    db: &str,
    statements: &[String],
) -> Vec<String> {
    statements
        .iter()
        .map(|s| crate::safety::evaluate(safety, db, s))
        .filter(|d| d.guard == crate::safety::Guard::Confirm)
        .filter(|d| {
            !matches!(
                d.kind,
                crate::safety::StatementKind::Insert
                    | crate::safety::StatementKind::Update { has_where: true }
                    | crate::safety::StatementKind::Delete { has_where: true }
                    | crate::safety::StatementKind::AlterDdl
            )
        })
        .map(|d| d.kind.describe())
        .collect()
}

/// The statements of `sql`, checked for applying to `db`, or why not.
/// `outside` lets a patch through that holds a statement no transaction
/// can run (`VACUUM`, `CONCURRENTLY`…): the operator chose to run it
/// without one. Pure (reads only `safety`).
pub fn check_patch(
    safety: &crate::safety::SafetyConfig,
    db: &str,
    sql: &str,
    outside: bool,
) -> Result<Checked, String> {
    if safety.profile_for(db).read_only {
        return Err(format!(
            "safety.toml keeps '{db}' read-only; set read_only = false for it to apply patches"
        ));
    }
    let statements =
        crate::batch::check_statements(safety, db, sql, true).map_err(|r| match r {
            // `assume_yes` answers a Confirm, so only a Block lands here.
            crate::batch::Refusal::Block { kind, stmt, .. }
            | crate::batch::Refusal::Confirm { kind, stmt, .. } => {
                format!("blocked by safety: {kind} (statement: {stmt})")
            }
            crate::batch::Refusal::Split => crate::safety::SPLIT_REFUSAL.to_string(),
            crate::batch::Refusal::ReadOnlyEscape => {
                crate::safety::READ_ONLY_ESCAPE_REFUSAL.to_string()
            }
            crate::batch::Refusal::ServerControl => {
                crate::safety::SERVER_CONTROL_REFUSAL.to_string()
            }
        })?;
    if statements.is_empty() {
        return Err("the patch has no statements".to_string());
    }
    let mut needs = None;
    for s in &statements {
        if crate::safety::has_unicode_escape(s) {
            return Err(
                "the patch uses unicode-escaped names (U&\"…\"), which pgman can't check; apply it by hand"
                    .into(),
            );
        }
        let tokens = crate::safety::code_tokens(s);
        if matches!(tokens.first().map(String::as_str), Some("set" | "reset"))
            || tokens.iter().any(|t| t == "set_config")
        {
            return Err(
                "the patch changes session settings (SET, RESET or set_config); apply it by hand"
                    .into(),
            );
        }
        if crate::safety::is_transaction_control(s) {
            return Err(
                "the patch manages its own transaction (BEGIN/COMMIT); apply it by hand".into(),
            );
        }
        if let Some(what) = needs_no_transaction(s) {
            if !outside {
                return Err(format!(
                    "the patch uses {what}, which can't run in a transaction; {OUTSIDE_HINT}"
                ));
            }
            needs.get_or_insert(what);
        }
    }
    Ok(Checked {
        confirms: guard_confirms(safety, db, &statements),
        statements,
        outside: needs,
    })
}

/// How a patch [`needs_no_transaction`] gets applied.
pub const OUTSIDE_HINT: &str =
    "pgman's environments view can plan it to run outside one (o), or apply it by hand";

/// Asked before each statement of a patch, and before its commit: `true`
/// stops the patch there (the operator cancelled). A cancel sent to the
/// server stops only the statement running when it arrives; this is what
/// holds one that arrives between two.
pub type Stop<'a> = &'a (dyn Fn() -> bool + Send + Sync);

/// Why a patch's statements stopped: statement `i` failed, or a [`Stop`]
/// came before it ran.
enum Halt {
    Failed(usize, String),
    Cancelled(usize),
}

/// What an apply did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub file: String,
    pub statements: usize,
    /// Whether the version table records the patch's id afterwards.
    pub recorded: bool,
}

/// Run `statements` (from [`check_patch`]) in one transaction on a
/// writable `client`, then read the version table again. A `stop` before
/// any statement, or before the commit, rolls it all back.
pub async fn apply(
    client: &tokio_postgres::Client,
    cfg: &PatchesConfig,
    patch: &PatchFile,
    statements: &[String],
    stop: Stop<'_>,
) -> Result<Applied, String> {
    begin(client).await?;
    let n = statements.len();
    let halted = match run_in_transaction(client, statements, stop).await {
        Err(Halt::Failed(i, msg)) => Some(format!(
            "statement {} of {n} failed, nothing applied: {msg}",
            i + 1
        )),
        Err(Halt::Cancelled(i)) => Some(format!(
            "cancelled before statement {} of {n}; rolled back, nothing applied",
            i + 1
        )),
        Ok(()) if stop() => Some(format!(
            "cancelled before the commit, all {n} statement(s) run; rolled back, nothing applied"
        )),
        Ok(()) => None,
    };
    if let Some(e) = halted {
        let _ = client.batch_execute("ROLLBACK").await;
        return Err(e);
    }
    client
        .batch_execute("COMMIT")
        .await
        .map_err(|e| format!("commit failed: {e}"))?;
    let applied = applied_ids(client, cfg).await.map_err(|e| {
        format!("committed, but the version table couldn't be read afterwards: {e}")
    })?;
    let id = patch.version_id.or(patch.filename_number);
    Ok(Applied {
        file: patch.file.clone(),
        statements: statements.len(),
        recorded: id.is_some_and(|id| applied.contains(&id)),
    })
}

/// Open a transaction on `client`: [`apply`]'s for one patch, or a dry
/// run's around all of them.
pub async fn begin(client: &tokio_postgres::Client) -> Result<(), String> {
    client
        .batch_execute("BEGIN")
        .await
        .map_err(|e| format!("could not begin: {e}"))
}

/// End a dry run: everything since [`begin`] is undone.
pub async fn rollback(client: &tokio_postgres::Client) -> Result<(), String> {
    client
        .batch_execute("ROLLBACK")
        .await
        .map_err(|e| format!("could not roll back: {e}"))
}

/// Run `statements` (from [`check_patch`]) inside the transaction the
/// caller holds — a dry run's — then read the version table in it, so
/// the patch's own version row shows. Nothing is committed: on a failure
/// the transaction is aborted and the caller rolls it back, as it does
/// after the last patch anyway.
pub async fn try_patch(
    client: &tokio_postgres::Client,
    cfg: &PatchesConfig,
    patch: &PatchFile,
    statements: &[String],
    stop: Stop<'_>,
) -> Result<Applied, String> {
    match run_in_transaction(client, statements, stop).await {
        Ok(()) => {}
        Err(Halt::Failed(i, msg)) => {
            return Err(format!(
                "statement {} of {} failed: {msg}",
                i + 1,
                statements.len()
            ))
        }
        Err(Halt::Cancelled(i)) => {
            return Err(format!(
                "cancelled before statement {} of {}",
                i + 1,
                statements.len()
            ))
        }
    }
    let applied = applied_ids(client, cfg)
        .await
        .map_err(|e| format!("ran, but the version table couldn't be read: {e}"))?;
    let id = patch.version_id.or(patch.filename_number);
    Ok(Applied {
        file: patch.file.clone(),
        statements: statements.len(),
        recorded: id.is_some_and(|id| applied.contains(&id)),
    })
}

/// `statements` inside an open transaction, each with the lock and
/// statement timeouts; the first that fails, or the one a `stop` came
/// before.
async fn run_in_transaction(
    client: &tokio_postgres::Client,
    statements: &[String],
    stop: Stop<'_>,
) -> Result<(), Halt> {
    for (i, stmt) in statements.iter().enumerate() {
        if stop() {
            return Err(Halt::Cancelled(i));
        }
        // Both, before each statement: a `DO` block or a function the
        // patch calls can change either with `set_config` where the check
        // can't see it (`SET` and a bare `set_config` are refused), and a
        // session-wide change outlives the statement and the commit.
        let reset = client
            .batch_execute(&format!(
                "SET LOCAL lock_timeout = '{LOCK_TIMEOUT}'; \
                 SET LOCAL statement_timeout = {STATEMENT_TIMEOUT_MS}"
            ))
            .await;
        // `execute` prepares: one command per string, or the server refuses.
        let run = match reset {
            Ok(()) => client.execute(stmt.as_str(), &[]).await.map(drop),
            Err(e) => Err(e),
        };
        if let Err(e) = run {
            return Err(Halt::Failed(i, server_message(&e)));
        }
    }
    Ok(())
}

/// The server's own message for `e`, or the client's when there is none.
fn server_message(e: &tokio_postgres::Error) -> String {
    e.as_db_error()
        .map(|d| d.message().to_string())
        .unwrap_or_else(|| e.to_string())
}

/// Run `statements` (from a [`check_patch`] with `outside`) one at a
/// time with no transaction around them, each committing as it runs, then
/// read the version table again. A failure part-way leaves the earlier
/// statements applied — and, when the version row came first, the patch
/// recorded — so the error says exactly where it stopped. A `stop` before
/// a statement ends it there the same way.
///
/// No statement timeout: a `VACUUM` or a concurrent build on a large
/// table can outlast any limit, and a concurrent build cut short is left
/// `INVALID`. The operator cancels instead (the caller holds the
/// connection's cancel token). The session's own timeout is restored
/// afterwards, for the patches after this one.
pub async fn apply_outside(
    client: &tokio_postgres::Client,
    cfg: &PatchesConfig,
    patch: &PatchFile,
    statements: &[String],
    stop: Stop<'_>,
) -> Result<Applied, String> {
    let id = patch.version_id.or(patch.filename_number);
    let saved: String = client
        .query_one("SHOW statement_timeout", &[])
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("could not read statement_timeout, nothing applied: {e}"))?;
    let ran = run_without_timeout(client, statements, stop).await;
    // Restored whatever happened: the next patch runs on this session.
    let restored = client
        .execute(
            "SELECT pg_catalog.set_config('statement_timeout', $1, false)",
            &[&saved],
        )
        .await;
    if let Err(halt) = ran {
        let (i, msg) = match &halt {
            Halt::Failed(i, msg) => (*i, Some(msg.as_str())),
            Halt::Cancelled(i) => (*i, None),
        };
        let stmt = &statements[i];
        let recorded = match (applied_ids(client, cfg).await, id) {
            (Ok(ids), Some(id)) if ids.contains(&id) => {
                format!("version {id} IS recorded, so it reads as applied")
            }
            (Ok(_), Some(id)) => format!("version {id} is not recorded"),
            _ => "the version table couldn't be read".to_string(),
        };
        return Err(outside_failure(i, statements.len(), stmt, msg, &recorded));
    }
    if let Err(e) = restored {
        return Err(format!(
            "ran, but statement_timeout couldn't be restored to {saved}; stopping: {e}"
        ));
    }
    let applied = applied_ids(client, cfg)
        .await
        .map_err(|e| format!("ran, but the version table couldn't be read afterwards: {e}"))?;
    Ok(Applied {
        file: patch.file.clone(),
        statements: statements.len(),
        recorded: id.is_some_and(|id| applied.contains(&id)),
    })
}

/// [`apply_outside`]'s statements, each with the lock timeout and no
/// statement timeout; the first that fails, or the one a `stop` came
/// before.
async fn run_without_timeout(
    client: &tokio_postgres::Client,
    statements: &[String],
    stop: Stop<'_>,
) -> Result<(), Halt> {
    for (i, stmt) in statements.iter().enumerate() {
        if stop() {
            return Err(Halt::Cancelled(i));
        }
        // Session-wide, there being no transaction to scope them; again
        // before each statement, as in `apply`.
        let reset = client
            .batch_execute(&format!(
                "SET lock_timeout = '{LOCK_TIMEOUT}'; SET statement_timeout = 0"
            ))
            .await;
        let run = match reset {
            Ok(()) => client.execute(stmt.as_str(), &[]).await.map(drop),
            Err(e) => Err(e),
        };
        if let Err(e) = run {
            return Err(Halt::Failed(i, server_message(&e)));
        }
    }
    Ok(())
}

/// What [`apply_outside`] says when statement `i` (of `n`) failed with
/// `msg`, or — `msg` `None` — was cancelled before it ran. Pure.
fn outside_failure(i: usize, n: usize, stmt: &str, msg: Option<&str>, recorded: &str) -> String {
    let what = match msg {
        Some(msg) => format!(
            "statement {} of {n} failed outside a transaction: {msg}",
            i + 1
        ),
        None => format!(
            "cancelled before statement {} of {n}, outside a transaction",
            i + 1
        ),
    };
    let mut out = format!(
        "{what}. {i} earlier statement(s) stay applied and {recorded}; finish the patch by hand"
    );
    // Not run, so it left nothing behind.
    if msg.is_none() {
        return out;
    }
    let tokens = crate::safety::code_tokens(stmt);
    if tokens.first().is_some_and(|t| t == "create") && tokens.iter().any(|t| t == "concurrently") {
        out.push_str(
            " (a failed CREATE INDEX CONCURRENTLY leaves an INVALID index: drop it before trying again)",
        );
    }
    out
}

/// What marks a database as a copy pgman made to try patches on.
const COPY_MARK: &str = "_pgman_copy_";

/// The name of a copy of `db` made at `stamp` (seconds since the
/// epoch): `db` reduced to lowercase letters, digits and `_`, shortened
/// to fit Postgres's 63 bytes, then `_pgman_copy_` and the stamp. Pure.
pub fn copy_name(db: &str, stamp: u64) -> String {
    let tail = format!("{COPY_MARK}{stamp}");
    let base: String = db
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(63 - tail.len())
        .collect();
    format!("{base}{tail}")
}

/// Whether `name` is one [`copy_name`] makes: the only databases pgman
/// drops. Pure.
pub fn is_copy_name(name: &str) -> bool {
    name.len() <= 63
        && name.rsplit_once(COPY_MARK).is_some_and(|(base, stamp)| {
            !base.is_empty()
                && !stamp.is_empty()
                && stamp.bytes().all(|b| b.is_ascii_digit())
                && base
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// `name` as a quoted identifier. Pure.
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// How many sessions other than `client`'s own are connected to `db`: a
/// template copy needs none.
pub async fn other_sessions(client: &tokio_postgres::Client, db: &str) -> Result<i64, String> {
    client
        .query_one(
            "SELECT pg_catalog.count(*) FROM pg_catalog.pg_stat_activity WHERE datname = $1 AND pid <> pg_catalog.pg_backend_pid()",
            &[&db],
        )
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("could not count the sessions on {db}: {e}"))
}

/// `CREATE DATABASE copy TEMPLATE source`, with no statement timeout: a
/// large database takes a while to copy, and the caller can cancel.
/// `client` must be connected to another database than `source`.
pub async fn create_copy(
    client: &tokio_postgres::Client,
    source: &str,
    copy: &str,
) -> Result<(), String> {
    if !is_copy_name(copy) {
        return Err(format!("{copy:?} isn't a name pgman makes for a copy"));
    }
    client
        .batch_execute("SET statement_timeout = 0")
        .await
        .map_err(|e| format!("could not lift the statement timeout: {e}"))?;
    client
        .batch_execute(&format!(
            "CREATE DATABASE {} TEMPLATE {}",
            quoted(copy),
            quoted(source)
        ))
        .await
        .map_err(|e| format!("could not copy {source}: {}", server_message(&e)))
}

/// Drop a copy [`create_copy`] made. Refuses any other name: this is the
/// one `DROP DATABASE` pgman runs. The copy's own connection may take a
/// moment to go once closed, so a busy copy is tried again for a while.
pub async fn drop_copy(client: &tokio_postgres::Client, copy: &str) -> Result<(), String> {
    if !is_copy_name(copy) {
        return Err(format!("{copy:?} isn't a copy pgman made; not dropping it"));
    }
    let sql = format!("DROP DATABASE IF EXISTS {}", quoted(copy));
    let mut tries = 0;
    loop {
        match client.batch_execute(&sql).await {
            Ok(()) => return Ok(()),
            // 55006 object_in_use: our own session still closing.
            Err(e)
                if tries < 40
                    && e.code() == Some(&tokio_postgres::error::SqlState::OBJECT_IN_USE) =>
            {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(e) => return Err(server_message(&e)),
        }
    }
}

/// Whether `client`'s server is a standby.
pub async fn in_recovery(client: &tokio_postgres::Client) -> Result<bool, String> {
    client
        .query_one("SELECT pg_catalog.pg_is_in_recovery()", &[])
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("could not ask whether it's a standby: {e}"))
}

/// The ids already recorded, read on `client`.
pub async fn applied_ids(
    client: &tokio_postgres::Client,
    cfg: &PatchesConfig,
) -> Result<Vec<i64>, String> {
    let sql = patches::applied_ids_sql(cfg)?;
    let (typed, truncated) =
        conn::run_statement_typed_capped(client, &sql, Some(patches::MAX_APPLIED_IDS))
            .await
            .map_err(|e| e.msg)?;
    patches::applied_ids(&typed, truncated, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::SafetyConfig;

    fn writable() -> SafetyConfig {
        let mut c = SafetyConfig::default();
        c.default.read_only = false;
        c
    }

    fn file(name: &str, id: i64) -> PatchFile {
        PatchFile {
            file: name.into(),
            filename_number: Some(id),
            version_id: Some(id),
        }
    }

    #[test]
    fn a_copy_is_named_so_pgman_can_tell_it_made_it() {
        assert_eq!(
            copy_name("ledger", 1_760_000_000),
            "ledger_pgman_copy_1760000000"
        );
        assert_eq!(copy_name("My-DB", 7), "my_db_pgman_copy_7");
        let long = copy_name(&"x".repeat(80), 1_760_000_000);
        assert_eq!(long.len(), 63);
        assert!(long.ends_with("_pgman_copy_1760000000"));
        for made in [copy_name("ledger", 1), copy_name("Ünïcode db", 2), long] {
            assert!(is_copy_name(&made), "{made}");
        }
        for not in [
            "ledger",
            "ledger_pgman_copy_",
            "_pgman_copy_1",
            "ledger_pgman_copy_1x",
            "Ledger_pgman_copy_1",
            "led\"ger_pgman_copy_1",
            "ledger_pgman_copy_1; DROP DATABASE ledger",
        ] {
            assert!(!is_copy_name(not), "{not}");
        }
        assert!(!is_copy_name(&format!("{}_pgman_copy_1", "x".repeat(60))));
    }

    #[test]
    fn an_identifier_is_quoted_whatever_it_holds() {
        assert_eq!(quoted("ledger"), "\"ledger\"");
        assert_eq!(quoted("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn the_next_patch_is_the_lowest_pending_one() {
        let files = [
            file("252-b.sql", 252),
            file("251-a.sql", 251),
            file("253-c.sql", 253),
        ];
        assert_eq!(next_patch(&files, &[250]).unwrap().file, "251-a.sql");
        assert_eq!(next_patch(&files, &[251, 252]).unwrap().file, "253-c.sql");
        assert_eq!(next_patch(&files, &[251, 252, 253]), None);
    }

    #[test]
    fn a_patch_is_checked_before_anything_runs() {
        let ok = "INSERT INTO t_schema_version (schema_version_id) VALUES (252);\n\
                  ALTER TABLE t ADD COLUMN c int;";
        assert_eq!(
            check_patch(&writable(), "ledger", ok, false)
                .unwrap()
                .statements
                .len(),
            2
        );
        // SET inside a statement, and CONCURRENTLY in a string, are fine.
        let fine = "ALTER TABLE t ALTER COLUMN c SET DEFAULT 0;\n\
                    UPDATE t SET note = 'built concurrently' WHERE false;";
        assert_eq!(
            check_patch(&writable(), "ledger", fine, false)
                .unwrap()
                .statements
                .len(),
            2
        );
        assert!(
            check_patch(&SafetyConfig::default(), "ledger", ok, false)
                .unwrap_err()
                .contains("read-only"),
            "the default profile is read-only"
        );
        for (bad, why) in [
            ("BEGIN; ALTER TABLE t ADD c int; COMMIT;", "own transaction"),
            ("CREATE INDEX CONCURRENTLY i ON t (c);", "CONCURRENTLY"),
            (
                "VACUUM ANALYZE t;",
                "VACUUM, which can't run in a transaction",
            ),
            ("", "no statements"),
            ("DROP TABLE t;", "blocked"),
            ("SET lock_timeout = 0;", "session settings"),
            ("RESET lock_timeout;", "session settings"),
            (
                "SELECT U&\"set_\\0063onfig\"('lock_timeout', '0', true);",
                "unicode-escaped",
            ),
            ("SET LOCAL statement_timeout = 0;", "session settings"),
            ("set standard_conforming_strings = off;", "session settings"),
            (
                "SELECT set_config('lock_timeout', '0', true);",
                "session settings",
            ),
        ] {
            let e = check_patch(&writable(), "ledger", bad, false).unwrap_err();
            assert!(e.contains(why), "{bad:?}: {e}");
        }
    }

    #[test]
    fn a_patch_names_what_the_guards_would_ask_about() {
        let mut safety = writable();
        safety.default.guards.delete_without_where = crate::safety::Guard::Confirm;
        let sql = "INSERT INTO t_schema_version (schema_version_id) VALUES (252);\n\
                   ALTER TABLE audit ADD COLUMN c int;\n\
                   UPDATE audit SET c = 0 WHERE id = 1;\n\
                   TRUNCATE audit;\n\
                   DELETE FROM sessions;\n\
                   SELECT 1;";
        let c = check_patch(&safety, "ledger", sql, false).unwrap();
        // A migration's ordinary statements (the default guards confirm
        // them too) and SELECT aren't named; the risky ones are.
        assert_eq!(c.confirms, ["TRUNCATE", "DELETE without WHERE"]);
        // A database whose profile asks about nothing: none.
        let mut quiet = safety.clone();
        quiet.default.guards = crate::safety::Guards {
            insert: crate::safety::Guard::Allow,
            update: crate::safety::Guard::Allow,
            update_without_where: crate::safety::Guard::Allow,
            delete: crate::safety::Guard::Allow,
            delete_without_where: crate::safety::Guard::Allow,
            truncate: crate::safety::Guard::Allow,
            drop: crate::safety::Guard::Allow,
            ddl: crate::safety::Guard::Allow,
            other: crate::safety::Guard::Allow,
        };
        assert!(check_patch(&quiet, "ledger", sql, false)
            .unwrap()
            .confirms
            .is_empty());
    }

    #[test]
    fn outside_lets_through_only_what_needs_no_transaction() {
        let vacuum = "INSERT INTO t_schema_version (schema_version_id) VALUES (252);\n\
                      VACUUM ANALYZE t;";
        let e = check_patch(&writable(), "ledger", vacuum, false).unwrap_err();
        assert!(e.contains("plan it to run outside one (o)"), "{e}");
        let c = check_patch(&writable(), "ledger", vacuum, true).unwrap();
        assert_eq!(c.outside, Some("VACUUM"));
        assert_eq!(c.statements.len(), 2);
        // A patch that doesn't need it still runs in its transaction.
        let plain = "ALTER TABLE t ADD COLUMN c int;";
        assert_eq!(
            check_patch(&writable(), "ledger", plain, true)
                .unwrap()
                .outside,
            None
        );
        // And outside lifts nothing else.
        for (bad, why) in [
            ("BEGIN; VACUUM t; COMMIT;", "own transaction"),
            ("SET lock_timeout = 0; VACUUM t;", "session settings"),
            ("DROP TABLE t; VACUUM t;", "blocked"),
        ] {
            let e = check_patch(&writable(), "ledger", bad, true).unwrap_err();
            assert!(e.contains(why), "{bad:?}: {e}");
        }
        assert!(
            check_patch(&SafetyConfig::default(), "ledger", vacuum, true)
                .unwrap_err()
                .contains("read-only")
        );
    }

    #[test]
    fn a_failure_outside_a_transaction_says_what_stayed_and_what_to_drop() {
        assert_eq!(
            outside_failure(1, 3, "VACUUM t", Some("canceled"), "version 4 IS recorded"),
            "statement 2 of 3 failed outside a transaction: canceled. 1 earlier statement(s) \
             stay applied and version 4 IS recorded; finish the patch by hand"
        );
        assert!(outside_failure(
            0,
            1,
            "CREATE UNIQUE INDEX CONCURRENTLY i ON t (c)",
            Some("canceling statement due to statement timeout"),
            "x"
        )
        .ends_with("leaves an INVALID index: drop it before trying again)"));
        assert!(
            !outside_failure(0, 1, "DROP INDEX CONCURRENTLY i", Some("m"), "x").contains("INVALID")
        );
        // Cancelled before it ran: it built nothing, INVALID or not.
        assert_eq!(
            outside_failure(
                1,
                2,
                "CREATE INDEX CONCURRENTLY i ON t (c)",
                None,
                "version 4 IS recorded"
            ),
            "cancelled before statement 2 of 2, outside a transaction. 1 earlier statement(s) \
             stay applied and version 4 IS recorded; finish the patch by hand"
        );
    }

    #[test]
    fn statements_the_server_refuses_in_a_transaction_are_named() {
        for (stmt, what) in [
            ("VACUUM t", "VACUUM"),
            ("vacuum (verbose, analyze) t", "VACUUM"),
            ("CREATE INDEX CONCURRENTLY i ON t (c)", "CONCURRENTLY"),
            ("DROP INDEX CONCURRENTLY IF EXISTS i", "CONCURRENTLY"),
            ("REINDEX INDEX CONCURRENTLY i", "CONCURRENTLY"),
            (
                "ALTER TABLE p DETACH PARTITION c CONCURRENTLY",
                "CONCURRENTLY",
            ),
            ("CREATE DATABASE d", "CREATE or DROP DATABASE"),
            ("DROP DATABASE d", "CREATE or DROP DATABASE"),
            (
                "CREATE TABLESPACE s LOCATION '/x'",
                "CREATE or DROP TABLESPACE",
            ),
            ("ALTER SYSTEM SET work_mem = '8MB'", "ALTER SYSTEM"),
            ("REINDEX DATABASE d", "REINDEX SYSTEM or DATABASE"),
        ] {
            assert_eq!(needs_no_transaction(stmt), Some(what), "{stmt}");
        }
        for stmt in [
            "CREATE INDEX i ON t (c)",
            "REINDEX INDEX i",
            "ALTER DATABASE d SET work_mem = '8MB'",
            "UPDATE t SET note = 'vacuum concurrently' -- concurrently",
            "SELECT 1 /* VACUUM */",
            "CREATE TABLE vacuum_log (id int)",
            "REFRESH MATERIALIZED VIEW CONCURRENTLY mv",
        ] {
            assert_eq!(needs_no_transaction(stmt), None, "{stmt}");
        }
    }
}
