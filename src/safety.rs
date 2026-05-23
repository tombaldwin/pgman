//! Per-database safety guard rails for statements run from the editor.
//!
//! pgman connects to production databases. Every statement the user runs is
//! classified (`classify`) and checked against a per-database `SafetyProfile`
//! (`evaluate`), which decides whether to allow it, confirm it, block it, and
//! whether to wrap it in a rollback-able transaction.
//!
//! `classify` is pure and heuristic — it strips comments and inspects the
//! leading keyword. On ambiguous input (CTEs, `EXPLAIN ANALYZE`) it over-guards
//! by design: a false "this is dangerous" only costs a keypress, a false "this
//! is safe" costs production data.
//!
//! One known imprecision: `has_where` is a whole-statement keyword check, so a
//! `DELETE` whose only `WHERE` sits in a subquery is treated as a guarded
//! delete rather than an unqualified one. It is still flagged (`Confirm`), just
//! not `Block`ed. A real SQL parser would fix this — see BACKLOG.md.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// What kind of statement a piece of SQL is — the classification the guards
/// key off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Select,
    Insert,
    Update { has_where: bool },
    Delete { has_where: bool },
    Truncate,
    Drop,
    /// `ALTER` / `CREATE` / `GRANT` / `VACUUM` / other DDL & maintenance.
    AlterDdl,
    /// Anything unrecognised — treated as a write, cautiously.
    Other,
}

impl StatementKind {
    /// Anything that is not a plain `SELECT`. Unknown statements count as writes.
    pub fn is_write(self) -> bool {
        !matches!(self, StatementKind::Select)
    }

    /// Data-modifying statements that benefit from a rollback-able transaction.
    pub fn is_dml(self) -> bool {
        matches!(
            self,
            StatementKind::Insert
                | StatementKind::Update { .. }
                | StatementKind::Delete { .. }
                | StatementKind::Truncate
        )
    }
}

/// Action a guard rail takes for a statement category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Guard {
    /// Run without prompting.
    Allow,
    /// Require an explicit confirmation keypress first.
    Confirm,
    /// Refuse to run.
    Block,
}

/// Guard rails per statement category. Configurable per database in
/// `~/.config/pgman/safety.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Guards {
    pub insert: Guard,
    pub update: Guard,
    /// An `UPDATE` with no `WHERE` clause — touches every row.
    pub update_without_where: Guard,
    pub delete: Guard,
    /// A `DELETE` with no `WHERE` clause — empties the table.
    pub delete_without_where: Guard,
    pub truncate: Guard,
    pub drop: Guard,
    pub ddl: Guard,
    pub other: Guard,
}

impl Default for Guards {
    fn default() -> Self {
        use Guard::*;
        Self {
            insert: Confirm,
            update: Confirm,
            update_without_where: Block,
            delete: Confirm,
            delete_without_where: Block,
            truncate: Confirm,
            drop: Block,
            ddl: Confirm,
            other: Confirm,
        }
    }
}

/// Safety settings for one database (or the default for unlisted databases).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyProfile {
    /// Open the connection with `default_transaction_read_only = on`.
    pub read_only: bool,
    /// Session `statement_timeout`, in milliseconds. `0` disables it.
    pub statement_timeout_ms: u64,
    /// Wrap writes in an explicit transaction and prompt commit/rollback.
    pub auto_tx: bool,
    pub guards: Guards,
}

impl Default for SafetyProfile {
    fn default() -> Self {
        Self {
            read_only: true,
            statement_timeout_ms: 30_000,
            auto_tx: true,
            guards: Guards::default(),
        }
    }
}

/// Top-level safety config: a default profile plus per-database overrides,
/// keyed by database name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    pub default: SafetyProfile,
    pub databases: HashMap<String, SafetyProfile>,
}

impl SafetyConfig {
    /// The profile for `db`, falling back to `default` when the database has no
    /// explicit entry.
    pub fn profile_for(&self, db: &str) -> &SafetyProfile {
        self.databases.get(db).unwrap_or(&self.default)
    }
}

/// The outcome of checking one statement against a database's profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub kind: StatementKind,
    pub guard: Guard,
    /// Run inside an explicit transaction the user can roll back.
    pub wrap_in_tx: bool,
    /// The connection is read-only; the server will reject this write outright.
    pub blocked_by_read_only: bool,
}

/// Classify a single SQL statement. Pure and heuristic — see the module docs.
pub fn classify(sql: &str) -> StatementKind {
    let stripped = strip_sql_comments(sql);
    let trimmed = stripped.trim_start();
    let first: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    let has_where = word_present(&stripped, "where");

    match first.as_str() {
        "select" | "table" | "values" | "show" => StatementKind::Select,
        "insert" => StatementKind::Insert,
        "update" => StatementKind::Update { has_where },
        "delete" => StatementKind::Delete { has_where },
        "truncate" => StatementKind::Truncate,
        "drop" => StatementKind::Drop,
        "alter" | "create" | "comment" | "grant" | "revoke" | "reindex" | "vacuum"
        | "analyze" | "analyse" | "cluster" | "refresh" => StatementKind::AlterDdl,
        // `EXPLAIN ANALYZE <dml>` *executes* the DML — classify the inner
        // statement, not the EXPLAIN wrapper.
        "explain" => classify(strip_explain_prefix(trimmed)),
        // A CTE can front any DML. Over-guard: if the body mentions a
        // destructive verb, treat the whole statement as that verb.
        "with" => classify_cte(&stripped, has_where),
        "" => StatementKind::Other,
        _ => StatementKind::Other,
    }
}

fn classify_cte(stripped: &str, has_where: bool) -> StatementKind {
    if word_present(stripped, "delete") {
        StatementKind::Delete { has_where }
    } else if word_present(stripped, "update") {
        StatementKind::Update { has_where }
    } else if word_present(stripped, "insert") {
        StatementKind::Insert
    } else {
        StatementKind::Select
    }
}

/// The guard for `kind` under `profile`.
pub fn guard_for(profile: &SafetyProfile, kind: StatementKind) -> Guard {
    let g = &profile.guards;
    match kind {
        StatementKind::Select => Guard::Allow,
        StatementKind::Insert => g.insert,
        StatementKind::Update { has_where: true } => g.update,
        StatementKind::Update { has_where: false } => g.update_without_where,
        StatementKind::Delete { has_where: true } => g.delete,
        StatementKind::Delete { has_where: false } => g.delete_without_where,
        StatementKind::Truncate => g.truncate,
        StatementKind::Drop => g.drop,
        StatementKind::AlterDdl => g.ddl,
        StatementKind::Other => g.other,
    }
}

/// Check a statement against the profile for `db`.
pub fn evaluate(config: &SafetyConfig, db: &str, sql: &str) -> Decision {
    let profile = config.profile_for(db);
    let kind = classify(sql);
    Decision {
        kind,
        guard: guard_for(profile, kind),
        wrap_in_tx: profile.auto_tx && kind.is_write(),
        blocked_by_read_only: profile.read_only && kind.is_write(),
    }
}

/// Split a SQL script on `;` outside string literals and SQL comments. Returns
/// the trimmed, non-empty statements in order. Used by the editor's
/// multi-statement run path (DBUnit scripts, hand-written batches).
pub fn split_statements(sql: &str) -> Vec<String> {
    let stripped = strip_sql_comments(sql);
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut chars = stripped.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            current.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    current.push(chars.next().unwrap()); // '' escape — still in string
                } else {
                    in_string = false;
                }
            }
            continue;
        }
        match c {
            '\'' => {
                in_string = true;
                current.push(c);
            }
            ';' => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    result.push(trimmed);
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        result.push(trimmed);
    }
    result
}

/// Remove `-- line` and `/* block */` comments, leaving string literals intact.
fn strip_sql_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    out.push(chars.next().unwrap()); // '' escape — still in string
                } else {
                    in_string = false;
                }
            }
            continue;
        }
        match c {
            '\'' => {
                in_string = true;
                out.push(c);
            }
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for cc in chars.by_ref() {
                    if cc == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for cc in chars.by_ref() {
                    if prev == '*' && cc == '/' {
                        break;
                    }
                    prev = cc;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// `true` if `word` appears in `haystack` as a whole token (case-insensitive).
fn word_present(haystack: &str, word: &str) -> bool {
    haystack
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|tok| tok.eq_ignore_ascii_case(word))
}

/// Given a string starting with `EXPLAIN`, return the inner statement — the
/// slice from the first statement keyword onward. Falls back to the slice after
/// the `EXPLAIN` token so recursion in `classify` always makes progress.
fn strip_explain_prefix(s: &str) -> &str {
    const STMT_KW: &[&str] = &[
        "select", "insert", "update", "delete", "with", "values", "table",
    ];
    let bytes = s.as_bytes();
    let mut after_explain = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = s[start..i].to_ascii_lowercase();
            if word == "explain" && after_explain == 0 {
                after_explain = i;
            } else if STMT_KW.contains(&word.as_str()) {
                return &s[start..];
            }
        } else {
            i += 1;
        }
    }
    &s[after_explain..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_basic_statements() {
        assert_eq!(classify("SELECT * FROM users"), StatementKind::Select);
        assert_eq!(classify("INSERT INTO t VALUES (1)"), StatementKind::Insert);
        assert_eq!(classify("TRUNCATE TABLE audit"), StatementKind::Truncate);
        assert_eq!(classify("DROP TABLE legacy"), StatementKind::Drop);
        assert_eq!(classify("ALTER TABLE t ADD COLUMN c int"), StatementKind::AlterDdl);
    }

    #[test]
    fn delete_distinguishes_where_clause() {
        assert_eq!(
            classify("DELETE FROM users WHERE id = 5"),
            StatementKind::Delete { has_where: true }
        );
        assert_eq!(
            classify("delete from users"),
            StatementKind::Delete { has_where: false }
        );
    }

    #[test]
    fn update_distinguishes_where_clause() {
        assert_eq!(
            classify("UPDATE t SET x = 1"),
            StatementKind::Update { has_where: false }
        );
        assert_eq!(
            classify("UPDATE t SET x = 1 WHERE id = 2"),
            StatementKind::Update { has_where: true }
        );
    }

    #[test]
    fn ignores_leading_comments_and_whitespace() {
        assert_eq!(
            classify("  -- cleanup job\n  DELETE FROM sessions"),
            StatementKind::Delete { has_where: false }
        );
        assert_eq!(
            classify("/* nightly */ TRUNCATE staging"),
            StatementKind::Truncate
        );
    }

    #[test]
    fn explain_analyze_classifies_the_inner_statement() {
        // The EXPLAIN footgun: ANALYZE on a DML executes it.
        assert_eq!(
            classify("EXPLAIN ANALYZE DELETE FROM t WHERE id = 1"),
            StatementKind::Delete { has_where: true }
        );
        assert_eq!(
            classify("EXPLAIN (FORMAT JSON) SELECT 1"),
            StatementKind::Select
        );
    }

    #[test]
    fn cte_fronting_a_delete_is_treated_as_a_delete() {
        assert_eq!(
            classify("WITH old AS (SELECT id FROM t) DELETE FROM t USING old"),
            StatementKind::Delete { has_where: false }
        );
        assert_eq!(classify("WITH x AS (SELECT 1) SELECT * FROM x"), StatementKind::Select);
    }

    #[test]
    fn comment_hidden_keyword_does_not_fool_classifier() {
        // `where` only appears inside a comment — not a real WHERE clause.
        assert_eq!(
            classify("DELETE FROM t -- where id = 1"),
            StatementKind::Delete { has_where: false }
        );
    }

    #[test]
    fn default_guards_block_the_dangerous_things() {
        let p = SafetyProfile::default();
        assert_eq!(guard_for(&p, StatementKind::Select), Guard::Allow);
        assert_eq!(guard_for(&p, StatementKind::Drop), Guard::Block);
        assert_eq!(
            guard_for(&p, StatementKind::Delete { has_where: false }),
            Guard::Block
        );
        assert_eq!(
            guard_for(&p, StatementKind::Delete { has_where: true }),
            Guard::Confirm
        );
        assert_eq!(guard_for(&p, StatementKind::Truncate), Guard::Confirm);
    }

    #[test]
    fn evaluate_wraps_writes_and_flags_read_only() {
        let cfg = SafetyConfig::default();
        let d = evaluate(&cfg, "anydb", "DELETE FROM t WHERE id = 1");
        assert_eq!(d.kind, StatementKind::Delete { has_where: true });
        assert_eq!(d.guard, Guard::Confirm);
        assert!(d.wrap_in_tx, "DML should be wrapped in a transaction");
        assert!(d.blocked_by_read_only, "default profile is read-only");

        let s = evaluate(&cfg, "anydb", "SELECT 1");
        assert!(!s.wrap_in_tx);
        assert!(!s.blocked_by_read_only);
        assert_eq!(s.guard, Guard::Allow);
    }

    #[test]
    fn split_statements_separates_on_semicolons_outside_strings() {
        let sql = "begin; insert into t values ('a;b'); update t set x = 1; commit";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "begin");
        assert_eq!(parts[1], "insert into t values ('a;b')");
        assert_eq!(parts[2], "update t set x = 1");
        assert_eq!(parts[3], "commit");
    }

    #[test]
    fn split_statements_skips_comments_and_empty_segments() {
        let sql = "-- header\nselect 1;\n\n/* block */\nselect 2;;;";
        let parts = split_statements(sql);
        assert_eq!(parts, vec!["select 1".to_string(), "select 2".to_string()]);
    }

    #[test]
    fn per_database_overrides_with_partial_config() {
        let toml = r#"
            [default]
            statement_timeout_ms = 60000

            [databases.prod]
            read_only = true

            [databases.prod.guards]
            truncate = "block"
        "#;
        let cfg: SafetyConfig = toml::from_str(toml).expect("parse safety config");

        // Default profile: explicit timeout, everything else from defaults.
        assert_eq!(cfg.default.statement_timeout_ms, 60_000);
        assert!(cfg.default.read_only);

        // prod: the guards table is partial — truncate overridden, the rest
        // fall back to defaults.
        let prod = cfg.profile_for("prod");
        assert_eq!(prod.guards.truncate, Guard::Block);
        assert_eq!(prod.guards.delete, Guard::Confirm);
        assert_eq!(prod.guards.drop, Guard::Block);

        // Unlisted database falls back to the default profile.
        let other = cfg.profile_for("scratch");
        assert_eq!(other.statement_timeout_ms, 60_000);
    }
}
