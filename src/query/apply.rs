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
//! own transaction (`BEGIN`/`COMMIT`) or builds an index `CONCURRENTLY`,
//! neither of which can run inside pgman's transaction; and a patch that
//! changes session settings (`SET`, `RESET`, `set_config`) — it could lift
//! the lock timeout, or change how the server reads the next statement
//! (`standard_conforming_strings`) so that it isn't the statement pgman
//! checked. Those are run by hand. The caller confirms each patch first;
//! that confirmation is what answers the guards' `Confirm`.
//!
//! Each statement goes to the server on its own through the extended
//! protocol, which refuses a string holding more than one command: what
//! the server runs is one statement, whatever its lexer makes of it.

use super::patches::{self, PatchFile, PatchesConfig};
use crate::conn;

/// How long a patch statement waits for a lock before failing.
pub const LOCK_TIMEOUT: &str = "5s";

/// The next patch to apply: the lowest-id pending one, if any. Pure.
pub fn next_patch(files: &[PatchFile], applied: &[i64]) -> Option<PatchFile> {
    patches::reconcile(files.to_vec(), applied)
        .pending
        .into_iter()
        .next()
}

/// The statements of `sql`, checked for applying to `db`, or why not.
/// Pure (reads only `safety`).
pub fn check_patch(
    safety: &crate::safety::SafetyConfig,
    db: &str,
    sql: &str,
) -> Result<Vec<String>, String> {
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
    for s in &statements {
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
        if tokens.iter().any(|t| t == "concurrently") {
            return Err(
                "the patch builds or drops an index CONCURRENTLY, which can't run in a \
                 transaction; apply it by hand"
                    .into(),
            );
        }
    }
    Ok(statements)
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
        assert_eq!(check_patch(&writable(), "ledger", ok).unwrap().len(), 2);
        // SET inside a statement, and CONCURRENTLY in a string, are fine.
        let fine = "ALTER TABLE t ALTER COLUMN c SET DEFAULT 0;\n\
                    UPDATE t SET note = 'built concurrently' WHERE false;";
        assert_eq!(check_patch(&writable(), "ledger", fine).unwrap().len(), 2);
        assert!(
            check_patch(&SafetyConfig::default(), "ledger", ok)
                .unwrap_err()
                .contains("read-only"),
            "the default profile is read-only"
        );
        for (bad, why) in [
            ("BEGIN; ALTER TABLE t ADD c int; COMMIT;", "own transaction"),
            ("CREATE INDEX CONCURRENTLY i ON t (c);", "CONCURRENTLY"),
            ("", "no statements"),
            ("DROP TABLE t;", "blocked"),
            ("SET lock_timeout = 0;", "session settings"),
            ("RESET lock_timeout;", "session settings"),
            ("SET LOCAL statement_timeout = 0;", "session settings"),
            ("set standard_conforming_strings = off;", "session settings"),
            (
                "SELECT set_config('lock_timeout', '0', true);",
                "session settings",
            ),
        ] {
            let e = check_patch(&writable(), "ledger", bad).unwrap_err();
            assert!(e.contains(why), "{bad:?}: {e}");
        }
    }
}
