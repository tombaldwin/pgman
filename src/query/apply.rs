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
//! checked. Those are run by hand. The caller confirms each patch first;
//! that confirmation is what answers the guards' `Confirm`.
//!
//! The one way past the transaction: an operator who plans with
//! `outside` (the TUI's `o`, confirmed by typing the environment's name;
//! never MCP) lets a patch holding a statement no transaction can run go
//! through [`apply_outside`] — each statement committing as it runs, so a
//! failure part-way leaves the earlier ones applied, and says so. The
//! other refusals stand.
//!
//! Each statement goes to the server on its own through the extended
//! protocol, which refuses a string holding more than one command: what
//! the server runs is one statement, whatever its lexer makes of it.

use super::patches::{self, PatchFile, PatchesConfig};
use crate::conn;

/// How long a patch statement waits for a lock before failing.
pub const LOCK_TIMEOUT: &str = "5s";

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
        statements,
        outside: needs,
    })
}

/// How a patch [`needs_no_transaction`] gets applied.
pub const OUTSIDE_HINT: &str =
    "pgman's environments view can plan it to run outside one (o), or apply it by hand";

/// What an apply did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub file: String,
    pub statements: usize,
    /// Whether the version table records the patch's id afterwards.
    pub recorded: bool,
}

/// Run `statements` (from [`check_patch`]) in one transaction on a
/// writable `client`, then read the version table again.
pub async fn apply(
    client: &tokio_postgres::Client,
    cfg: &PatchesConfig,
    patch: &PatchFile,
    statements: &[String],
) -> Result<Applied, String> {
    client
        .batch_execute(&format!("BEGIN; SET LOCAL lock_timeout = '{LOCK_TIMEOUT}'"))
        .await
        .map_err(|e| format!("could not begin: {e}"))?;
    for (i, stmt) in statements.iter().enumerate() {
        // Again before each statement: a function the patch calls could
        // have changed it (`SET` and `set_config` themselves are refused).
        let reset = client
            .batch_execute(&format!("SET LOCAL lock_timeout = '{LOCK_TIMEOUT}'"))
            .await;
        // `execute` prepares: one command per string, or the server refuses.
        let run = match reset {
            Ok(()) => client.execute(stmt.as_str(), &[]).await.map(drop),
            Err(e) => Err(e),
        };
        if let Err(e) = run {
            let _ = client.batch_execute("ROLLBACK").await;
            let msg = e
                .as_db_error()
                .map(|d| d.message().to_string())
                .unwrap_or_else(|| e.to_string());
            return Err(format!(
                "statement {} of {} failed, nothing applied: {msg}",
                i + 1,
                statements.len()
            ));
        }
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

/// Run `statements` (from a [`check_patch`] with `outside`) one at a
/// time with no transaction around them, each committing as it runs, then
/// read the version table again. A failure part-way leaves the earlier
/// statements applied — and, when the version row came first, the patch
/// recorded — so the error says exactly where it stopped.
pub async fn apply_outside(
    client: &tokio_postgres::Client,
    cfg: &PatchesConfig,
    patch: &PatchFile,
    statements: &[String],
) -> Result<Applied, String> {
    let id = patch.version_id.or(patch.filename_number);
    for (i, stmt) in statements.iter().enumerate() {
        // Session-wide, there being no transaction to scope it; again
        // before each statement, as in `apply`.
        let reset = client
            .batch_execute(&format!("SET lock_timeout = '{LOCK_TIMEOUT}'"))
            .await;
        let run = match reset {
            Ok(()) => client.execute(stmt.as_str(), &[]).await.map(drop),
            Err(e) => Err(e),
        };
        if let Err(e) = run {
            let msg = e
                .as_db_error()
                .map(|d| d.message().to_string())
                .unwrap_or_else(|| e.to_string());
            let recorded = match (applied_ids(client, cfg).await, id) {
                (Ok(ids), Some(id)) if ids.contains(&id) => {
                    format!("version {id} IS recorded, so it reads as applied")
                }
                (Ok(_), Some(id)) => format!("version {id} is not recorded"),
                _ => "the version table couldn't be read".to_string(),
            };
            return Err(outside_failure(i, statements.len(), stmt, &msg, &recorded));
        }
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

/// What [`apply_outside`] says when statement `i` (of `n`) failed with
/// `msg`. Pure.
fn outside_failure(i: usize, n: usize, stmt: &str, msg: &str, recorded: &str) -> String {
    let mut out = format!(
        "statement {} of {n} failed outside a transaction: {msg}. {i} earlier statement(s) \
         stay applied and {recorded}; finish the patch by hand",
        i + 1
    );
    let tokens = crate::safety::code_tokens(stmt);
    if tokens.first().is_some_and(|t| t == "create") && tokens.iter().any(|t| t == "concurrently") {
        out.push_str(
            " (a failed CREATE INDEX CONCURRENTLY leaves an INVALID index: drop it before trying again)",
        );
    }
    out
}

/// Whether `client`'s server is a standby.
pub async fn in_recovery(client: &tokio_postgres::Client) -> Result<bool, String> {
    client
        .query_one("SELECT pg_is_in_recovery()", &[])
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
            outside_failure(1, 3, "VACUUM t", "canceled", "version 4 IS recorded"),
            "statement 2 of 3 failed outside a transaction: canceled. 1 earlier statement(s) \
             stay applied and version 4 IS recorded; finish the patch by hand"
        );
        assert!(outside_failure(
            0,
            1,
            "CREATE UNIQUE INDEX CONCURRENTLY i ON t (c)",
            "canceling statement due to statement timeout",
            "x"
        )
        .ends_with("leaves an INVALID index: drop it before trying again)"));
        assert!(!outside_failure(0, 1, "DROP INDEX CONCURRENTLY i", "m", "x").contains("INVALID"));
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
