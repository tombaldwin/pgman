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
    Update {
        has_where: bool,
    },
    Delete {
        has_where: bool,
    },
    Truncate,
    Drop,
    /// `ALTER` / `CREATE` / `GRANT` / `VACUUM` / other DDL & maintenance.
    AlterDdl,
    /// Anything unrecognised — treated as a write, cautiously.
    Other,
}

impl StatementKind {
    /// Anything that is not a plain `SELECT`. Unknown statements count as writes.
    /// The auto-tx wrap (see [`evaluate`]) keys off this, so every write —
    /// DML, DDL, and `Other` (e.g. MERGE) — gets a rollback-able transaction.
    pub fn is_write(self) -> bool {
        !matches!(self, StatementKind::Select)
    }

    /// An operator-facing phrase for this classification — what the confirm
    /// modal shows in place of the enum's `Debug` form. Never contains `{`
    /// or `}`; see `describe_never_leaks_debug_braces` in the tests below.
    pub fn describe(&self) -> String {
        match self {
            StatementKind::Select => "SELECT".to_string(),
            StatementKind::Insert => "INSERT".to_string(),
            StatementKind::Update { has_where: true } => "UPDATE with WHERE".to_string(),
            StatementKind::Update { has_where: false } => "UPDATE without WHERE".to_string(),
            StatementKind::Delete { has_where: true } => "DELETE with WHERE".to_string(),
            StatementKind::Delete { has_where: false } => "DELETE without WHERE".to_string(),
            StatementKind::Truncate => "TRUNCATE".to_string(),
            StatementKind::Drop => "DROP".to_string(),
            StatementKind::AlterDdl => "ALTER / DDL".to_string(),
            StatementKind::Other => "other statement".to_string(),
        }
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
    /// Row-count threshold above which a SELECT triggers a pre-flight
    /// `EXPLAIN` cost preview + Confirm prompt before running. `0`
    /// disables the check entirely. Default: disabled — the value is
    /// opt-in per profile so existing setups don't change UX.
    pub cost_preview_threshold_rows: u64,
    /// Which table-clean strategy the DBUnit apply script
    /// (`Ctrl-D`) uses for this database. `truncate` (default,
    /// fast, needs the privilege) or `delete_from` (works without
    /// TRUNCATE privilege, respects triggers).
    pub clean_mode: crate::dbunit::CleanMode,
}

impl Default for SafetyProfile {
    fn default() -> Self {
        Self {
            read_only: true,
            statement_timeout_ms: 30_000,
            auto_tx: true,
            guards: Guards::default(),
            cost_preview_threshold_rows: 0,
            clean_mode: crate::dbunit::CleanMode::Truncate,
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
        "alter" | "create" | "comment" | "grant" | "revoke" | "reindex" | "vacuum" | "analyze"
        | "analyse" | "cluster" | "refresh" => StatementKind::AlterDdl,
        // MERGE (PG15+) can INSERT / UPDATE / DELETE. We have no dedicated
        // kind for it, so it maps to `Other` — which `is_write()` reports as
        // a write and which guards as `Confirm` by default. Listed explicitly
        // (rather than falling through `_`) so a reader sees it's intentional
        // and the regression test pins it as a write.
        "merge" => StatementKind::Other,
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

/// `true` if running `sql` could change the schema cache (tables, columns,
/// indexes, sequences, FK edges) — i.e. any statement in the script is DDL
/// (`CREATE`/`ALTER`/`DROP`/`GRANT`/…). Used to trigger a background schema
/// re-fetch after an editor run so completion / browser / lint / FK-nav stay
/// current. DML and `TRUNCATE` don't change structure, so they don't count.
pub fn changes_schema(sql: &str) -> bool {
    split_statements(sql)
        .iter()
        .any(|s| matches!(classify(s), StatementKind::AlterDdl | StatementKind::Drop))
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

// ---------------------------------------------------------------------------
// The statement lexer
// ---------------------------------------------------------------------------
//
// `split_statements` and `strip_sql_comments` used to carry a hand-rolled
// scanner each. They disagreed — one tracked `'…'` and `$tag$…$tag$`, neither
// tracked `"…"` quoted identifiers, and both treated *any* `$` as a
// dollar-quote opener even where Postgres reads it as an ordinary identifier
// character. A script like `SELECT 1 AS a$b$c; DROP TABLE users` therefore
// split into two statements instead of two-plus-a-DROP, and the DROP rode
// along inside a fragment that classified as `Select` → `Allow`.
//
// Both now run off ONE scanner, [`scan`], so they cannot drift apart again.

/// What a [`Span`] of the input is, lexically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpanKind {
    /// Ordinary SQL text — this is the only kind `;` splits on and the only
    /// kind keywords are matched in.
    Code,
    /// `'…'` string literal (`''` escapes the quote).
    String,
    /// `E'…'` escape-string literal (backslash escapes, plus `''`).
    EscapeString,
    /// `"…"` quoted identifier (`""` escapes the quote).
    Ident,
    /// `$tag$ … $tag$` dollar-quoted body, tag included.
    DollarQuoted,
    /// `-- …` to end of line.
    LineComment,
    /// `/* … */`, nesting as Postgres does.
    BlockComment,
}

/// One lexical run of the input. Spans tile the input exactly: concatenating
/// `sql[start..end]` over the returned spans reproduces `sql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) kind: SpanKind,
    /// Byte offset of the first byte of the span.
    pub(crate) start: usize,
    /// Byte offset one past the last byte of the span.
    pub(crate) end: usize,
    /// `false` when the input ended before the span's closing delimiter — an
    /// unterminated `'`, `"`, `$tag$`, or `/*`. A line comment is always
    /// terminated (end-of-input closes it).
    pub(crate) terminated: bool,
}

/// Lex `sql` into quoted / comment / code spans. The single source of truth
/// for "is this `;` a statement separator?" and "is this `--` a comment?".
///
/// Postgres rules implemented here:
/// - `'…'` with `''` as the escaped quote; `E'…'` (case-insensitive `E`, and
///   only when the `E` is not itself part of a longer identifier) additionally
///   honours backslash escapes, so `E'\''` is one literal.
/// - `"…"` quoted identifiers with `""` as the escaped quote. A `;`, `--`, or
///   `$$` inside one is part of the name, not syntax.
/// - `$tag$ … $tag$` dollar-quoting, where the tag is empty or matches
///   `[A-Za-z_][A-Za-z0-9_]*`, **and** the opening `$` is not preceded by an
///   identifier character. Postgres allows `$` inside an identifier after the
///   first character, so the `$b$` in `a$b$c` opens nothing.
/// - `--` to end of line, and `/* … */` which nests.
pub(crate) fn scan(sql: &str) -> Vec<Span> {
    let b = sql.as_bytes();
    let mut spans: Vec<Span> = Vec::new();
    let mut code_start = 0usize;
    let mut i = 0usize;

    // Close the run of plain code that ends just before `at`.
    fn flush_code(spans: &mut Vec<Span>, code_start: usize, at: usize) {
        if at > code_start {
            spans.push(Span {
                kind: SpanKind::Code,
                start: code_start,
                end: at,
                terminated: true,
            });
        }
    }

    while i < b.len() {
        let c = b[i];

        // `E'…'` / `e'…'` — an escape string, but only when the `E` stands
        // alone (`table_e'x'` is not a thing, but `some_e` followed by a
        // literal would be, so check the preceding byte).
        if (c == b'E' || c == b'e')
            && b.get(i + 1) == Some(&b'\'')
            && !i.checked_sub(1).is_some_and(|p| is_ident_byte(b[p]))
        {
            flush_code(&mut spans, code_start, i);
            let (end, terminated) = scan_quoted_string(b, i + 2, true);
            spans.push(Span {
                kind: SpanKind::EscapeString,
                start: i,
                end,
                terminated,
            });
            i = end;
            code_start = i;
            continue;
        }

        match c {
            b'\'' => {
                flush_code(&mut spans, code_start, i);
                let (end, terminated) = scan_quoted_string(b, i + 1, false);
                spans.push(Span {
                    kind: SpanKind::String,
                    start: i,
                    end,
                    terminated,
                });
                i = end;
                code_start = i;
            }
            b'"' => {
                flush_code(&mut spans, code_start, i);
                let (end, terminated) = scan_quoted_ident(b, i + 1);
                spans.push(Span {
                    kind: SpanKind::Ident,
                    start: i,
                    end,
                    terminated,
                });
                i = end;
                code_start = i;
            }
            b'$' => {
                // Only a `$` that is NOT continuing an identifier can open a
                // dollar-quote. `a$b$c` is one identifier to Postgres.
                let after_ident = i
                    .checked_sub(1)
                    .is_some_and(|p| is_ident_byte(b[p]) || b[p] == b'$');
                match if after_ident {
                    None
                } else {
                    dollar_tag_at(b, i)
                } {
                    Some(tag_len) => {
                        flush_code(&mut spans, code_start, i);
                        let (end, terminated) = scan_dollar_body(b, i, tag_len);
                        spans.push(Span {
                            kind: SpanKind::DollarQuoted,
                            start: i,
                            end,
                            terminated,
                        });
                        i = end;
                        code_start = i;
                    }
                    // `$1` (a positional parameter), `a$b`, a bare `$` — code.
                    None => i += 1,
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                flush_code(&mut spans, code_start, i);
                let mut end = i + 2;
                while end < b.len() && b[end] != b'\n' {
                    end += 1;
                }
                // The newline itself stays code, so stripping a line comment
                // still leaves the line break behind.
                spans.push(Span {
                    kind: SpanKind::LineComment,
                    start: i,
                    end,
                    terminated: true,
                });
                i = end;
                code_start = i;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                flush_code(&mut spans, code_start, i);
                let (end, terminated) = scan_block_comment(b, i);
                spans.push(Span {
                    kind: SpanKind::BlockComment,
                    start: i,
                    end,
                    terminated,
                });
                i = end;
                code_start = i;
            }
            _ => i += 1,
        }
    }
    flush_code(&mut spans, code_start, b.len());
    spans
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Scan from just past the opening `'` to just past the closing one. `escapes`
/// enables backslash escapes (`E'…'`). Returns `(end, terminated)`.
fn scan_quoted_string(b: &[u8], mut i: usize, escapes: bool) -> (usize, bool) {
    while i < b.len() {
        match b[i] {
            b'\\' if escapes => i += 2, // `\'`, `\\`, `\n`, … — skip the pair
            b'\'' => {
                if b.get(i + 1) == Some(&b'\'') {
                    i += 2; // `''` — an escaped quote, still inside
                } else {
                    return (i + 1, true);
                }
            }
            _ => i += 1,
        }
    }
    (b.len(), false)
}

/// Scan from just past the opening `"` to just past the closing one.
fn scan_quoted_ident(b: &[u8], mut i: usize) -> (usize, bool) {
    while i < b.len() {
        if b[i] == b'"' {
            if b.get(i + 1) == Some(&b'"') {
                i += 2; // `""` — an escaped quote, still inside
            } else {
                return (i + 1, true);
            }
        } else {
            i += 1;
        }
    }
    (b.len(), false)
}

/// If `b[i..]` opens a dollar-quote (`$$` or `$tag$`), return the byte length
/// of the opening tag. The caller has already ruled out a `$` that continues
/// an identifier.
fn dollar_tag_at(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'$') {
        return None;
    }
    let mut j = i + 1;
    while let Some(&c) = b.get(j) {
        if c == b'$' {
            return Some(j + 1 - i);
        }
        let first = j == i + 1;
        // Tag rules: a letter or `_` first, digits allowed after.
        let ok = c == b'_' || c.is_ascii_alphabetic() || (!first && c.is_ascii_digit());
        if !ok {
            return None;
        }
        j += 1;
    }
    None
}

/// Scan a dollar-quoted body whose opening tag starts at `start` and is
/// `tag_len` bytes long, through the matching close tag.
fn scan_dollar_body(b: &[u8], start: usize, tag_len: usize) -> (usize, bool) {
    let tag = &b[start..start + tag_len];
    let mut i = start + tag_len;
    while i + tag_len <= b.len() {
        if b[i] == b'$' && &b[i..i + tag_len] == tag {
            return (i + tag_len, true);
        }
        i += 1;
    }
    (b.len(), false)
}

/// Scan `/* … */`, honouring Postgres's nesting.
fn scan_block_comment(b: &[u8], start: usize) -> (usize, bool) {
    let mut depth = 0usize;
    let mut i = start;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return (i, true);
            }
        } else {
            i += 1;
        }
    }
    (b.len(), false)
}

/// Split a SQL script on `;` outside string literals, quoted identifiers,
/// dollar-quoted bodies, and SQL comments. Returns the trimmed, non-empty
/// statements in order. Used by the editor's multi-statement run path (DBUnit
/// scripts, hand-written batches).
///
/// Runs off [`scan`], so it agrees with [`strip_sql_comments`] by construction.
/// Prefer [`split_verified`] on any path that then *runs* the statements: it
/// refuses a script this splitter cannot account for, rather than guessing.
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    // Append `text` to the statement being built, ending a statement at every
    // top-level `;` in it.
    let push_code = |current: &mut String, result: &mut Vec<String>, text: &str| {
        for part in text.split_inclusive(';') {
            match part.strip_suffix(';') {
                Some(before) => {
                    current.push_str(before);
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        result.push(trimmed);
                    }
                    current.clear();
                }
                None => current.push_str(part),
            }
        }
    };
    for span in scan(sql) {
        let text = &sql[span.start..span.end];
        match span.kind {
            SpanKind::LineComment => {}
            SpanKind::BlockComment => {
                current.push(' ');
                // An unterminated `/*` must not swallow the rest of the
                // script: swallowing could hide a destructive verb and get a
                // fragment misclassified as a harmless SELECT. Keep the text
                // as code (Postgres rejects the unterminated comment anyway).
                if !span.terminated {
                    push_code(&mut current, &mut result, &text[2..]);
                }
            }
            SpanKind::Code => push_code(&mut current, &mut result, text),
            _ => current.push_str(text),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        result.push(trimmed);
    }
    result
}

/// Remove `-- line` and `/* block */` comments, leaving string literals,
/// quoted identifiers, and dollar-quoted bodies intact (a `--` inside a
/// `$$ … $$` function body — or inside `"a--b"` — is data, not a comment).
pub(crate) fn strip_sql_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    for span in scan(sql) {
        let text = &sql[span.start..span.end];
        match span.kind {
            // The span stops before the newline, which stays in the following
            // code span — so the line break survives the strip.
            SpanKind::LineComment => {}
            SpanKind::BlockComment => {
                out.push(' ');
                // See `split_statements`: an unterminated `/*` keeps its text
                // so a destructive verb behind it still reaches the classifier.
                if !span.terminated {
                    out.push_str(&text[2..]);
                }
            }
            _ => out.push_str(text),
        }
    }
    out
}

/// `true` if `word` appears in `haystack` as a whole token (case-insensitive).
pub(crate) fn word_present(haystack: &str, word: &str) -> bool {
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
        assert_eq!(
            classify("ALTER TABLE t ADD COLUMN c int"),
            StatementKind::AlterDdl
        );
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
        assert_eq!(
            classify("WITH x AS (SELECT 1) SELECT * FROM x"),
            StatementKind::Select
        );
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
    fn split_statements_keeps_dollar_quoted_body_intact() {
        // The `;` inside the plpgsql body must NOT split the statement —
        // otherwise the body fragments get mis-classified (e.g. a bare
        // `DELETE FROM t` → Block) and a valid CREATE FUNCTION is refused.
        let sql = "CREATE FUNCTION f() RETURNS void AS $$ BEGIN DELETE FROM t; END; $$ LANGUAGE plpgsql; SELECT 1";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0],
            "CREATE FUNCTION f() RETURNS void AS $$ BEGIN DELETE FROM t; END; $$ LANGUAGE plpgsql"
        );
        assert_eq!(parts[1], "SELECT 1");
        // And the whole thing classifies as DDL, not a delete.
        assert_eq!(classify(&parts[0]), StatementKind::AlterDdl);
    }

    #[test]
    fn split_statements_handles_tagged_dollar_quotes() {
        // `$func$` tag, and a `$1` positional param that must NOT be
        // mistaken for a dollar-quote opener.
        let sql = "CREATE FUNCTION g(int) RETURNS int AS $func$ SELECT $1; $func$ LANGUAGE sql; DROP TABLE t";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("SELECT $1;"));
        assert_eq!(parts[1], "DROP TABLE t");
    }

    #[test]
    fn strip_comments_leaves_dashes_inside_dollar_body_alone() {
        // A `--` inside a dollar-quoted body is data, not a comment.
        let out = strip_sql_comments("SELECT $$ a -- b\n c $$");
        assert!(out.contains("a -- b"), "got: {out:?}");
    }

    #[test]
    fn split_statements_skips_comments_and_empty_segments() {
        let sql = "-- header\nselect 1;\n\n/* block */\nselect 2;;;";
        let parts = split_statements(sql);
        assert_eq!(parts, vec!["select 1".to_string(), "select 2".to_string()]);
    }

    // --- lexer: the security-review reproductions ------------------------

    /// The reproduction. `$` after an identifier character is an identifier
    /// character to Postgres, not a dollar-quote opener. The old splitter read
    /// `$b$` as opening a quote, swallowed the rest of the script into a
    /// fragment starting `SELECT`, classified it `Select` -> `Allow`, and let
    /// the `DROP` through.
    #[test]
    fn dollar_inside_an_identifier_does_not_open_a_dollar_quote() {
        let sql = "SELECT 1; SELECT 1 AS a$b$c; DROP TABLE users";
        let parts = split_statements(sql);
        assert_eq!(
            parts,
            vec![
                "SELECT 1".to_string(),
                "SELECT 1 AS a$b$c".to_string(),
                "DROP TABLE users".to_string(),
            ]
        );
        let kinds: Vec<StatementKind> = parts.iter().map(|s| classify(s)).collect();
        assert_eq!(
            kinds,
            vec![
                StatementKind::Select,
                StatementKind::Select,
                StatementKind::Drop
            ]
        );
    }

    /// The second reproduction. `--` inside a quoted identifier is part of the
    /// name; the old comment stripper treated it as a line comment and ate the
    /// rest of the line, including the `"` that would have re-balanced things.
    #[test]
    fn a_comment_marker_inside_a_quoted_identifier_is_part_of_the_name() {
        let sql = r#"SELECT 1; SELECT * FROM "a--b"; DROP TABLE users"#;
        let parts = split_statements(sql);
        assert_eq!(
            parts,
            vec![
                "SELECT 1".to_string(),
                r#"SELECT * FROM "a--b""#.to_string(),
                "DROP TABLE users".to_string(),
            ]
        );
        let kinds: Vec<StatementKind> = parts.iter().map(|s| classify(s)).collect();
        assert_eq!(
            kinds,
            vec![
                StatementKind::Select,
                StatementKind::Select,
                StatementKind::Drop
            ]
        );
        // And the stripper agrees - it runs off the same scanner.
        assert_eq!(strip_sql_comments(sql), sql);
    }

    // --- lexer: the rest of the constructs -------------------------------

    #[test]
    fn a_semicolon_inside_a_quoted_identifier_is_not_a_separator() {
        let sql = r#"SELECT * FROM "a;b""#;
        assert_eq!(split_statements(sql), vec![sql.to_string()]);
    }

    #[test]
    fn doubled_quote_escapes_inside_a_quoted_identifier() {
        // `"a""b;c"` is the single identifier `a"b;c`.
        let sql = r#"SELECT * FROM "a""b;c"; DROP TABLE users"#;
        assert_eq!(
            split_statements(sql),
            vec![
                r#"SELECT * FROM "a""b;c""#.to_string(),
                "DROP TABLE users".to_string(),
            ]
        );
    }

    #[test]
    fn escape_strings_honour_backslash_escapes() {
        // In `E'...'` a `\'` does NOT close the literal, so the `;` stays
        // inside it and the whole thing is one statement.
        let sql = r"SELECT E'a\'; DROP TABLE users --' AS x";
        assert_eq!(split_statements(sql), vec![sql.to_string()]);
        assert_eq!(classify(sql), StatementKind::Select);
        // A lower-case `e` prefix behaves the same.
        assert_eq!(split_statements(r"SELECT e'a\'; b' ; SELECT 2").len(), 2);
    }

    #[test]
    fn an_e_that_ends_an_identifier_does_not_start_an_escape_string() {
        // `date'2020-01-01'` is a typed literal: the `e` belongs to `date`, so
        // this is an ordinary string and `\` is not an escape.
        let sql = r"SELECT date'2020-01-01'; SELECT 2";
        assert_eq!(split_statements(sql).len(), 2);
    }

    #[test]
    fn block_comments_nest() {
        let sql = "SELECT 1 /* outer /* inner */ still comment */, 2; SELECT 3";
        assert_eq!(
            split_statements(sql),
            vec!["SELECT 1  , 2".to_string(), "SELECT 3".to_string()]
        );
    }

    #[test]
    fn comment_markers_inside_a_string_literal_are_data() {
        let sql = "SELECT '-- not a comment', '/* nor this */'; SELECT 2";
        assert_eq!(split_statements(sql).len(), 2);
        assert_eq!(strip_sql_comments(sql), sql);
    }

    #[test]
    fn a_positional_parameter_is_not_a_dollar_quote() {
        let sql = "SELECT * FROM t WHERE id = $1; DROP TABLE users";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2);
        assert_eq!(classify(&parts[1]), StatementKind::Drop);
    }

    #[test]
    fn a_tag_that_is_not_an_identifier_does_not_open_a_dollar_quote() {
        // `$9x$` is not a legal dollar-quote tag (digit lead), so the `;`
        // after it still separates statements.
        let sql = "SELECT $9x$ ; DROP TABLE users";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2);
        assert_eq!(classify(&parts[1]), StatementKind::Drop);
    }

    #[test]
    fn the_scanner_tiles_the_input_exactly() {
        // Every byte belongs to exactly one span, in order - the property the
        // two consumers rely on to stay in agreement.
        for sql in [
            r#"SELECT 'a', "b--c", $t$ d; $t$, E'\'' /* x /* y */ z */ -- tail"#,
            "SELECT 1; DROP TABLE t",
            "",
            "'unterminated",
        ] {
            let spans = scan(sql);
            let mut at = 0usize;
            for s in &spans {
                assert_eq!(s.start, at, "gap or overlap in {sql:?}");
                at = s.end;
            }
            assert_eq!(at, sql.len(), "spans must reach the end of {sql:?}");
        }
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

    #[test]
    fn clean_mode_defaults_to_truncate() {
        let cfg = SafetyConfig::default();
        assert_eq!(cfg.default.clean_mode, crate::dbunit::CleanMode::Truncate);
        // Absent from TOML → default Truncate.
        let cfg: SafetyConfig = toml::from_str("[databases.prod]\nread_only = true\n").unwrap();
        assert_eq!(
            cfg.profile_for("prod").clean_mode,
            crate::dbunit::CleanMode::Truncate
        );
    }

    #[test]
    fn changes_schema_detects_ddl_only() {
        assert!(changes_schema("CREATE TABLE foo (id int)"));
        assert!(changes_schema("ALTER TABLE bar ADD COLUMN baz text"));
        assert!(changes_schema("DROP TABLE qux"));
        // DML and TRUNCATE don't change structure.
        assert!(!changes_schema("INSERT INTO t VALUES (1)"));
        assert!(!changes_schema("DELETE FROM t WHERE id = 1"));
        assert!(!changes_schema("TRUNCATE t"));
        assert!(!changes_schema("SELECT * FROM t"));
        // A mixed batch where ANY statement is DDL counts.
        assert!(changes_schema(
            "INSERT INTO t VALUES (1); ALTER TABLE t ADD c int"
        ));
    }

    #[test]
    fn merge_is_treated_as_a_write_not_select() {
        // MERGE (PG15+) can INSERT/UPDATE/DELETE — it must never reach
        // Guard::Allow as a SELECT does.
        let k = classify("MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE");
        assert_eq!(k, StatementKind::Other);
        assert!(k.is_write());
        let cfg = SafetyConfig::default();
        let d = evaluate(
            &cfg,
            "db",
            "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
        );
        assert_ne!(d.guard, Guard::Allow, "MERGE must be guarded, not allowed");
    }

    #[test]
    fn unterminated_block_comment_does_not_hide_a_write() {
        // An unclosed `/*` must not swallow a trailing destructive verb
        // and downgrade the statement to a harmless SELECT.
        let k = classify("WITH x AS (SELECT 1) /* note\nINSERT INTO logs SELECT * FROM x");
        assert_eq!(
            k,
            StatementKind::Insert,
            "write behind an unclosed comment must still classify as a write"
        );
        // A normal closed comment still strips cleanly.
        assert_eq!(classify("SELECT 1 /* harmless */"), StatementKind::Select);
    }

    #[test]
    fn clean_mode_parses_per_database_override() {
        let toml = r#"
            [databases.legacy]
            clean_mode = "delete_from"
        "#;
        let cfg: SafetyConfig = toml::from_str(toml).expect("parse safety config");
        assert_eq!(
            cfg.profile_for("legacy").clean_mode,
            crate::dbunit::CleanMode::DeleteFrom
        );
        // Unlisted db still defaults to Truncate.
        assert_eq!(
            cfg.profile_for("other").clean_mode,
            crate::dbunit::CleanMode::Truncate
        );
    }

    #[test]
    fn describe_reads_as_an_operator_facing_phrase() {
        assert_eq!(StatementKind::Select.describe(), "SELECT");
        assert_eq!(StatementKind::Insert.describe(), "INSERT");
        assert_eq!(
            StatementKind::Update { has_where: false }.describe(),
            "UPDATE without WHERE"
        );
        assert_eq!(
            StatementKind::Update { has_where: true }.describe(),
            "UPDATE with WHERE"
        );
        assert_eq!(
            StatementKind::Delete { has_where: false }.describe(),
            "DELETE without WHERE"
        );
        assert_eq!(
            StatementKind::Delete { has_where: true }.describe(),
            "DELETE with WHERE"
        );
        assert_eq!(StatementKind::Truncate.describe(), "TRUNCATE");
        assert_eq!(StatementKind::Drop.describe(), "DROP");
        assert_eq!(StatementKind::AlterDdl.describe(), "ALTER / DDL");
        assert_eq!(StatementKind::Other.describe(), "other statement");
    }

    #[test]
    fn describe_never_leaks_debug_braces() {
        // The bug this exists to stop: `format!("{:?}", kind)` on a
        // struct-like variant (e.g. `Delete { has_where: false }`) puts
        // Rust's `Debug` syntax straight in front of the user.
        let all = [
            StatementKind::Select,
            StatementKind::Insert,
            StatementKind::Update { has_where: false },
            StatementKind::Update { has_where: true },
            StatementKind::Delete { has_where: false },
            StatementKind::Delete { has_where: true },
            StatementKind::Truncate,
            StatementKind::Drop,
            StatementKind::AlterDdl,
            StatementKind::Other,
        ];
        for kind in all {
            let d = kind.describe();
            assert!(
                !d.contains('{') && !d.contains('}'),
                "describe() leaked Debug syntax for {kind:?}: {d:?}"
            );
        }
    }
}
