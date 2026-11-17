//! One-off writes: MCP's `execute`.
//!
//! An agent passes ONE `INSERT`, `UPDATE` or `DELETE`; the operator is
//! shown it and confirms it; it runs in one transaction with a short
//! `lock_timeout` and a `statement_timeout`, and is rolled back unless it
//! changed the number of rows the operator confirmed.
//!
//! [`check`] is the statement's gate: exactly one statement, one of those
//! three verbs, no parameters, no `-- pgman: allow` of its own (the agent
//! wrote it, so it can't vouch for itself), nothing that changes session
//! settings or the transaction, and the same safety.toml guards and
//! read-only profile `apply_patches` honours — a refusal of those two
//! being what the operator may lift for one run.
//!
//! Before asking, a plain `UPDATE` or `DELETE` ([`shape`]: one table, no
//! `FROM` / `USING`) is counted on a read-only session with its own
//! `WHERE` ([`count_sql`]), and an `UPDATE`'s current values of the
//! columns it sets are read ([`before_sql`]), so the operator sees what
//! it overwrites and the audit line can undo it by hand. Both are
//! derived from the statement's text: whole top-level clauses moved, never
//! cut inside a literal, and checked again as one `SELECT` before they
//! are sent. Anything else gets the planner's estimate ([`estimate`]).

use super::apply::{Guarded, Refused};
use crate::conn;
use crate::safety::{self, SpanKind, StatementKind};

/// How long the statement waits for a lock before failing.
pub const LOCK_TIMEOUT: &str = "5s";

/// How long the statement may run: a one-off change is small.
pub const STATEMENT_TIMEOUT_MS: u64 = 30_000;

/// The longest statement `execute` takes: the operator reads all of it.
pub const MAX_STATEMENT_CHARS: usize = 4000;

/// How many rows' current values are read for the confirmation and the
/// audit line.
pub const MAX_BEFORE_ROWS: usize = 20;

/// How much of one current value is kept.
pub const MAX_BEFORE_CELL: usize = 200;

/// A statement [`check`] passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The verified statement text: what is shown, and what runs.
    pub statement: String,
    pub kind: StatementKind,
    /// What safety.toml's guards would ask about it (`TRUNCATE`-like
    /// kinds; a keyed `UPDATE` and an `INSERT` are left out, as a plan
    /// leaves them out): the operator's one confirmation answers it.
    pub confirms: Vec<String>,
    /// It is one a `Block` guard stops, and this run lifts that (`lift`).
    pub lifted: Option<Guarded>,
    /// The database is read-only and this run writes anyway (`lift`).
    pub read_only_lifted: bool,
}

impl Checked {
    /// What this run lifts, for the operator: the read-only database,
    /// then the blocked statement. Pure.
    pub fn lifted(&self, db: &str) -> Vec<String> {
        self.read_only_lifted
            .then(|| format!("'{db}' is read-only"))
            .into_iter()
            .chain(self.lifted.as_ref().map(|g| g.kind.clone()))
            .collect()
    }
}

/// `sql`, checked for running once on `db`, or why not. With `lift`,
/// a read-only profile and a `Block` guard — what a [`Refused::liftable`]
/// refusal names — are let through and named in the result. Pure but
/// for safety.toml's path, read for a read-only refusal to name it.
pub fn check(
    cfg: &safety::SafetyConfig,
    db: &str,
    sql: &str,
    lift: bool,
) -> Result<Checked, Refused> {
    if sql.trim().is_empty() {
        return Err("sql is empty".into());
    }
    let mut statements = safety::split_verified(sql).map_err(|_| safety::SPLIT_REFUSAL)?;
    let statement = match statements.len() {
        1 => statements.remove(0),
        0 => return Err("sql has no statement".into()),
        n => {
            return Err(format!("sql has {n} statements; execute runs exactly one per call").into())
        }
    };
    let chars = statement.chars().count();
    if chars > MAX_STATEMENT_CHARS {
        return Err(format!(
            "the statement is {chars} characters; execute takes at most {MAX_STATEMENT_CHARS}, \
             so the operator can read all of it"
        )
        .into());
    }
    let allows = super::apply::declared_allows(&statement).map_err(Refused::from)?;
    if !allows.is_empty() {
        return Err(
            "the statement allows its own guard (`-- pgman: allow`); only a committed patch \
             can, so remove that line"
                .into(),
        );
    }
    let tokens = safety::code_tokens(&statement);
    if !matches!(
        tokens.first().map(String::as_str),
        Some("insert" | "update" | "delete")
    ) {
        return Err(
            "execute runs one INSERT, UPDATE or DELETE; schema changes go through \
             apply_patches, and anything else is for the operator to run"
                .into(),
        );
    }
    if tokens.iter().any(|t| is_parameter(t)) {
        return Err(
            "the statement has a parameter ($1…); execute takes none, so write the value in".into(),
        );
    }
    if safety::has_unicode_escape(&statement) {
        return Err(
            "the statement uses unicode-escaped names (U&\"…\"), which pgman can't check".into(),
        );
    }
    if tokens.iter().any(|t| t == "set_config") {
        return Err("the statement calls set_config, which changes session settings".into());
    }
    if safety::is_transaction_control(&statement) {
        return Err("the statement manages its own transaction".into());
    }
    let read_only = cfg.profile_for(db).read_only;
    // Judged as on a writable session: read-only is refused as itself.
    let writable = if read_only {
        let mut w = cfg.clone();
        let mut profile = cfg.profile_for(db).clone();
        profile.read_only = false;
        w.databases
            .insert(cfg.entry_key(db).unwrap_or_else(|| db.to_string()), profile);
        w
    } else {
        cfg.clone()
    };
    let decision = safety::evaluate(&writable, db, &statement);
    if decision.read_only_escape {
        return Err(safety::READ_ONLY_ESCAPE_REFUSAL.into());
    }
    if safety::calls_server_control(&statement) || safety::mentions_destructive_function(&statement)
    {
        return Err(
            "the statement calls a function that acts on the server (sessions, settings, files, \
             replication, WAL or statistics); that is for the operator to run"
                .into(),
        );
    }
    let blocked = (decision.guard == safety::Guard::Block).then(|| Guarded {
        line: 1,
        kind: decision.kind.describe(),
        statement: crate::batch::terminal_safe(statement.lines().next().unwrap_or("")),
    });
    let confirms = super::apply::guard_confirms(cfg, db, std::slice::from_ref(&statement));
    if lift {
        return Ok(Checked {
            statement,
            kind: decision.kind,
            confirms,
            lifted: blocked,
            read_only_lifted: read_only,
        });
    }
    let mut why = Vec::new();
    if read_only {
        why.push(super::apply::read_only_refusal(
            cfg,
            db,
            &crate::util::config_file("safety.toml"),
        ));
    }
    if let Some(g) = &blocked {
        why.push(format!(
            "blocked by safety: {} (guards.{} in safety.toml)",
            g.kind,
            safety::guard_name(decision.kind).unwrap_or("other")
        ));
    }
    if !why.is_empty() {
        return Err(Refused {
            why: why.join(". Also "),
            blocked: blocked.into_iter().collect(),
            liftable: true,
        });
    }
    Ok(Checked {
        statement,
        kind: decision.kind,
        confirms,
        lifted: None,
        read_only_lifted: false,
    })
}

/// `$1`, `$2`…: a positional parameter. Pure.
fn is_parameter(token: &str) -> bool {
    token
        .strip_prefix('$')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The parts of a plain `UPDATE` or `DELETE` pgman can count and read
/// back: one table, its own `WHERE` (if any), and the columns an
/// `UPDATE` sets — each as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    /// `[ONLY] name [[AS] alias]`.
    pub table: String,
    /// The `WHERE` condition, without `WHERE`.
    pub filter: Option<String>,
    /// The columns an `UPDATE` sets, when each is a plain name; empty for
    /// a `DELETE`, or when one isn't.
    pub set_columns: Vec<String>,
}

/// A token of a statement's top level: outside every parenthesis, in
/// code. `Name` covers quoted identifiers; `Other` literals and anything
/// else.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    Name,
    Punct(u8),
    Other,
}

/// `sql`'s top-level tokens with their byte ranges, or `None` when its
/// parentheses don't balance. Comments must be stripped first. Pure.
fn top_level(sql: &str) -> Option<Vec<(Tok, usize, usize)>> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for span in safety::scan(sql) {
        let (s, text) = (span.start, &sql[span.start..span.end]);
        match span.kind {
            SpanKind::Code => {
                let b = text.as_bytes();
                let mut i = 0;
                while i < b.len() {
                    let c = b[i];
                    let word =
                        |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80;
                    match c {
                        b'(' => {
                            if depth == 0 {
                                out.push((Tok::Punct(c), s + i, s + i + 1));
                            }
                            depth += 1;
                        }
                        b')' => {
                            depth = depth.checked_sub(1)?;
                            if depth == 0 {
                                out.push((Tok::Punct(c), s + i, s + i + 1));
                            }
                        }
                        _ if depth > 0 || c.is_ascii_whitespace() => {}
                        _ if word(c) => {
                            let start = i;
                            while i < b.len() && word(b[i]) {
                                i += 1;
                            }
                            let w = &text[start..i];
                            let tok = if w.as_bytes()[0].is_ascii_digit() {
                                Tok::Other
                            } else {
                                Tok::Word(w.to_ascii_lowercase())
                            };
                            out.push((tok, s + start, s + i));
                            continue;
                        }
                        _ => out.push((Tok::Punct(c), s + i, s + i + 1)),
                    }
                    i += 1;
                }
            }
            SpanKind::Ident if depth == 0 => out.push((Tok::Name, span.start, span.end)),
            _ if depth == 0 => out.push((Tok::Other, span.start, span.end)),
            _ => {}
        }
    }
    (depth == 0).then_some(out)
}

/// `stmt`'s [`Shape`], when it is a plain `UPDATE` or `DELETE`; `None`
/// for an `INSERT`, an `UPDATE … FROM`, a `DELETE … USING`, `WHERE
/// CURRENT OF`, or anything this can't read for certain. Pure.
pub fn shape(stmt: &str) -> Option<Shape> {
    let sql = safety::strip_sql_comments(stmt);
    let sql = sql.trim().trim_end_matches(';').trim_end();
    let toks = top_level(sql)?;
    let word = |i: usize| match toks.get(i) {
        Some((Tok::Word(w), _, _)) => Some(w.as_str()),
        _ => None,
    };
    let find = |w: &str, from: usize| (from..toks.len()).find(|&i| word(i) == Some(w));
    let (table_from, after_table) = match (word(0), word(1)) {
        (Some("update"), _) => (1, find("set", 1)?),
        (Some("delete"), Some("from")) => {
            if find("using", 2).is_some() {
                return None;
            }
            let end = find("where", 2)
                .or_else(|| find("returning", 2))
                .unwrap_or(toks.len());
            (2, end)
        }
        _ => return None,
    };
    // A name, `schema.name`, `ONLY`, an alias: nothing else.
    let table_toks = &toks[table_from..after_table];
    if table_toks.is_empty()
        || !table_toks
            .iter()
            .all(|(t, _, _)| matches!(t, Tok::Word(_) | Tok::Name | Tok::Punct(b'.')))
    {
        return None;
    }
    let table = sql[table_toks[0].1..table_toks[table_toks.len() - 1].2].to_string();
    let update = word(0) == Some("update");
    if update && find("from", after_table).is_some() {
        return None;
    }
    let returning = find("returning", after_table);
    let where_at = find("where", after_table).filter(|w| returning.is_none_or(|r| *w < r));
    let end = returning.map_or(sql.len(), |r| toks[r].1);
    let filter = match where_at {
        Some(w) => {
            if word(w + 1) == Some("current") && word(w + 2) == Some("of") {
                return None;
            }
            let f = sql[toks[w].2..end].trim();
            if f.is_empty() {
                return None;
            }
            Some(f.to_string())
        }
        None => None,
    };
    let set_columns = if update {
        let set_end = where_at.or(returning).unwrap_or(toks.len());
        set_columns(sql, &toks[after_table + 1..set_end])
    } else {
        Vec::new()
    };
    Some(Shape {
        table,
        filter,
        set_columns,
    })
}

/// The columns of an `UPDATE`'s `SET` list (`toks`, after `SET`), each
/// as written, when every assignment is `name = …`; empty otherwise.
/// Pure.
fn set_columns(sql: &str, toks: &[(Tok, usize, usize)]) -> Vec<String> {
    let mut cols = Vec::new();
    for assignment in toks.split(|(t, _, _)| *t == Tok::Punct(b',')) {
        match assignment {
            [(Tok::Word(_) | Tok::Name, s, e), (Tok::Punct(b'='), _, _), _, ..] => {
                cols.push(sql[*s..*e].to_string())
            }
            _ => return Vec::new(),
        }
    }
    cols
}

/// `SELECT count(*)` of the rows `shape`'s statement changes. Pure.
pub fn count_sql(shape: &Shape) -> String {
    match &shape.filter {
        Some(f) => format!("SELECT count(*) FROM {} WHERE {f}", shape.table),
        None => format!("SELECT count(*) FROM {}", shape.table),
    }
}

/// The current values of the columns an `UPDATE` sets, in the rows it
/// changes: one row past [`MAX_BEFORE_ROWS`], so a cut shows. `None` when
/// it sets no plain column. Pure.
pub fn before_sql(shape: &Shape) -> Option<String> {
    if shape.set_columns.is_empty() {
        return None;
    }
    let cols = shape.set_columns.join(", ");
    let filter = shape
        .filter
        .as_ref()
        .map(|f| format!(" WHERE {f}"))
        .unwrap_or_default();
    Some(format!(
        "SELECT {cols} FROM {}{filter} LIMIT {}",
        shape.table,
        MAX_BEFORE_ROWS + 1
    ))
}

/// `sql`, which pgman derived, as one `SELECT`, or why not: what the
/// read-only session is sent. Pure.
fn derived(sql: &str) -> Result<String, String> {
    match safety::split_verified(sql).as_deref() {
        Ok([one]) if safety::classify(one) == StatementKind::Select => Ok(one.clone()),
        _ => Err("pgman couldn't derive a SELECT from it".to_string()),
    }
}

/// The current values [`before_sql`] read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Before {
    pub columns: Vec<String>,
    /// At most [`MAX_BEFORE_ROWS`], each value cut at
    /// [`MAX_BEFORE_CELL`] characters.
    pub rows: Vec<Vec<Option<String>>>,
    /// More rows than that matched.
    pub more: bool,
}

impl Before {
    /// `[{"col": "old", …}, …]`. Pure.
    pub fn to_json(&self) -> serde_json::Value {
        self.rows
            .iter()
            .map(|r| {
                self.columns
                    .iter()
                    .cloned()
                    .zip(r.iter().cloned().map(serde_json::Value::from))
                    .collect::<serde_json::Map<_, _>>()
            })
            .collect()
    }
}

/// `value` cut at [`MAX_BEFORE_CELL`] characters, ending in `…` when
/// cut. Pure.
fn cut(value: String) -> String {
    if value.chars().count() <= MAX_BEFORE_CELL {
        return value;
    }
    let mut s: String = value.chars().take(MAX_BEFORE_CELL).collect();
    s.push('…');
    s
}

/// How many rows `shape`'s statement changes now, counted on `client`
/// (read-only).
pub async fn count(client: &tokio_postgres::Client, shape: &Shape) -> Result<u64, String> {
    let sql = derived(&count_sql(shape))?;
    let (typed, _) = conn::run_statement_typed_capped(client, &sql, Some(1))
        .await
        .map_err(|e| e.msg)?;
    typed
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|c| c.as_deref())
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| "the count came back empty".to_string())
}

/// The current values of the columns `shape`'s `UPDATE` sets, read on
/// `client` (read-only); `None` when it sets no plain column.
pub async fn before(
    client: &tokio_postgres::Client,
    shape: &Shape,
) -> Result<Option<Before>, String> {
    let Some(sql) = before_sql(shape) else {
        return Ok(None);
    };
    let sql = derived(&sql)?;
    let (typed, _) = conn::run_statement_typed_capped(client, &sql, Some(MAX_BEFORE_ROWS + 1))
        .await
        .map_err(|e| e.msg)?;
    let more = typed.rows.len() > MAX_BEFORE_ROWS;
    Ok(Some(Before {
        columns: typed.columns.into_iter().map(|(n, _)| n).collect(),
        rows: typed
            .rows
            .into_iter()
            .take(MAX_BEFORE_ROWS)
            .map(|r| r.into_iter().map(|c| c.map(cut)).collect())
            .collect(),
        more,
    }))
}

/// The rows the planner expects `plan` (`EXPLAIN (FORMAT JSON)`'s
/// output) to change: the node under a `ModifyTable`, whose own count is
/// 0. Pure.
pub fn planned_rows(plan: &serde_json::Value) -> Option<u64> {
    let top = plan.get(0)?.get("Plan")?;
    let node = if top.get("Node Type").and_then(|t| t.as_str()) == Some("ModifyTable") {
        top.get("Plans")?.get(0)?
    } else {
        top
    };
    node.get("Plan Rows")?.as_f64().map(|r| r.round() as u64)
}

/// The planner's estimate of the rows `statement` changes, from an
/// `EXPLAIN` on `client` (read-only), which plans it without running it.
pub async fn estimate(client: &tokio_postgres::Client, statement: &str) -> Result<u64, String> {
    let sql = format!("EXPLAIN (FORMAT JSON) {statement}");
    let (typed, _) = conn::run_statement_typed_capped(client, &sql, Some(1))
        .await
        .map_err(|e| e.msg)?;
    let text = typed
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|c| c.as_deref())
        .ok_or("EXPLAIN came back empty")?;
    let plan: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("EXPLAIN's plan: {e}"))?;
    planned_rows(&plan).ok_or_else(|| "EXPLAIN's plan has no row estimate".to_string())
}

/// Run `statement` (from [`check`]) once on a writable `client`, in one
/// transaction with [`LOCK_TIMEOUT`] and [`STATEMENT_TIMEOUT_MS`], and
/// commit it only when it changed `expect` rows: the rows it changed,
/// or why nothing was kept.
pub async fn run(
    client: &tokio_postgres::Client,
    statement: &str,
    expect: u64,
) -> Result<u64, String> {
    super::apply::begin(client).await?;
    let ran = match client
        .batch_execute(&format!(
            "SET LOCAL lock_timeout = '{LOCK_TIMEOUT}'; \
             SET LOCAL statement_timeout = {STATEMENT_TIMEOUT_MS}"
        ))
        .await
    {
        // `execute` prepares: one command per string, or the server refuses.
        Ok(()) => client.execute(statement, &[]).await,
        Err(e) => Err(e),
    };
    let halted = match &ran {
        Err(e) => Err(format!(
            "it failed, nothing kept: {}",
            super::apply::server_message(e)
        )),
        Ok(n) if *n != expect => Err(format!(
            "it changed {n} row(s), not the {expect} confirmed, so it was rolled back: nothing kept"
        )),
        Ok(n) => Ok(*n),
    };
    let n = match halted {
        Ok(n) => n,
        Err(why) => {
            let _ = client.batch_execute("ROLLBACK").await;
            return Err(why);
        }
    };
    client
        .batch_execute("COMMIT")
        .await
        .map_err(|e| format!("the commit failed, so it may not have been kept: {e}"))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writable() -> safety::SafetyConfig {
        let (cfg, err) =
            safety::SafetyConfig::parse_personal("[databases.ledger]\nread_only = false\n");
        assert!(err.is_none(), "{err:?}");
        cfg
    }

    #[test]
    fn a_keyed_update_passes_and_is_shown_as_written() {
        let c = check(
            &writable(),
            "ledger",
            "UPDATE settings SET value = '10' WHERE id = 1;",
            false,
        )
        .unwrap();
        assert_eq!(c.statement, "UPDATE settings SET value = '10' WHERE id = 1");
        assert_eq!(c.kind, StatementKind::Update { has_where: true });
        assert!(c.lifted.is_none() && !c.read_only_lifted);
    }

    #[test]
    fn only_one_insert_update_or_delete_is_taken() {
        let w = writable();
        let refused = |sql: &str| check(&w, "ledger", sql, true).unwrap_err().why;
        assert!(refused("").contains("empty"));
        assert!(
            refused("UPDATE a SET x = 1 WHERE id = 1; UPDATE b SET x = 1 WHERE id = 1")
                .contains("2 statements")
        );
        for sql in [
            "SELECT 1",
            "ALTER TABLE a ADD COLUMN b int",
            "DROP TABLE a",
            "TRUNCATE a",
            "WITH x AS (DELETE FROM a RETURNING *) SELECT 1",
            "MERGE INTO a USING b ON a.id = b.id WHEN MATCHED THEN DELETE",
            "DO $$ BEGIN DELETE FROM a; END $$",
            "CALL purge()",
        ] {
            assert!(
                refused(sql).contains("one INSERT, UPDATE or DELETE"),
                "{sql}"
            );
        }
    }

    #[test]
    fn what_could_change_the_session_or_hide_a_name_is_refused() {
        let w = writable();
        let refused = |sql: &str| check(&w, "ledger", sql, true).unwrap_err().why;
        assert!(refused("UPDATE a SET x = $1 WHERE id = 1").contains("parameter"));
        assert!(
            refused("UPDATE a SET x = set_config('lock_timeout', '0', false) WHERE id = 1")
                .contains("set_config")
        );
        assert!(refused("UPDATE U&\"a\" SET x = 1 WHERE id = 1").contains("unicode"));
        assert!(
            refused("UPDATE a SET x = pg_terminate_backend(1) WHERE id = 1")
                .contains("acts on the server")
        );
        // `$$`-quoted text holds no parameter.
        assert!(check(&w, "ledger", "UPDATE a SET x = $$1$$ WHERE id = 1", false).is_ok());
    }

    #[test]
    fn a_statement_cannot_allow_its_own_guard() {
        let why = check(
            &writable(),
            "ledger",
            "-- pgman: allow delete_without_where\nDELETE FROM a",
            false,
        )
        .unwrap_err()
        .why;
        assert!(why.contains("allows its own guard"), "{why}");
    }

    #[test]
    fn a_long_statement_is_refused_unread() {
        let sql = format!(
            "UPDATE a SET x = '{}' WHERE id = 1",
            "y".repeat(MAX_STATEMENT_CHARS)
        );
        let why = check(&writable(), "ledger", &sql, false).unwrap_err().why;
        assert!(why.contains("at most"), "{why}");
    }

    #[test]
    fn read_only_and_a_blocked_guard_are_liftable_and_named_when_lifted() {
        let ro = safety::SafetyConfig::default();
        let r = check(&ro, "ledger", "DELETE FROM a", false).unwrap_err();
        assert!(r.liftable, "{}", r.why);
        assert!(r.why.contains("'ledger' is read-only"), "{}", r.why);
        assert!(r.why.contains("guards.delete_without_where"), "{}", r.why);
        let lifted = check(&ro, "ledger", "DELETE FROM a", true).unwrap();
        assert_eq!(
            lifted.lifted("ledger"),
            vec!["'ledger' is read-only", "DELETE without WHERE"]
        );
        // A keyed one on a writable database lifts nothing.
        let plain = check(&writable(), "ledger", "DELETE FROM a WHERE id = 1", true).unwrap();
        assert!(plain.lifted("ledger").is_empty());
    }

    #[test]
    fn a_plain_update_is_shaped_into_its_table_filter_and_columns() {
        let s = shape(
            "UPDATE ONLY app.settings AS s SET value = '10', \"Note\" = 'a, b' \
             WHERE s.id = 1 AND s.note <> 'where' RETURNING *;",
        )
        .unwrap();
        assert_eq!(s.table, "ONLY app.settings AS s");
        assert_eq!(s.filter.as_deref(), Some("s.id = 1 AND s.note <> 'where'"));
        assert_eq!(s.set_columns, vec!["value", "\"Note\""]);
        assert_eq!(
            count_sql(&s),
            "SELECT count(*) FROM ONLY app.settings AS s WHERE s.id = 1 AND s.note <> 'where'"
        );
        assert_eq!(
            before_sql(&s).unwrap(),
            "SELECT value, \"Note\" FROM ONLY app.settings AS s WHERE s.id = 1 AND s.note <> 'where' LIMIT 21"
        );
    }

    #[test]
    fn a_subquery_or_comment_does_not_move_the_clauses() {
        let s = shape(
            "UPDATE t SET a = (SELECT max(b) FROM u WHERE u.id = t.id) -- from\n WHERE t.id IN (SELECT id FROM v)",
        )
        .unwrap();
        assert_eq!(s.table, "t");
        assert_eq!(s.filter.as_deref(), Some("t.id IN (SELECT id FROM v)"));
        assert_eq!(s.set_columns, vec!["a"]);
        let d = shape("DELETE FROM t WHERE id = 3").unwrap();
        assert_eq!(count_sql(&d), "SELECT count(*) FROM t WHERE id = 3");
        assert_eq!(before_sql(&d), None);
        let all = shape("DELETE FROM t").unwrap();
        assert_eq!(count_sql(&all), "SELECT count(*) FROM t");
    }

    #[test]
    fn what_cannot_be_counted_for_certain_is_not_shaped() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = u.a FROM u WHERE u.id = t.id",
            "UPDATE t SET a = b IS DISTINCT FROM c WHERE id = 1",
            "DELETE FROM t USING u WHERE u.id = t.id",
            "UPDATE t SET a = 1 WHERE CURRENT OF c",
            "UPDATE t SET a = 1 WHERE",
            "UPDATE f(1) SET a = 1 WHERE id = 1",
            "UPDATE t SET a = (1 WHERE id = 1",
        ] {
            assert_eq!(shape(sql), None, "{sql}");
        }
        // Counted, but a set target that isn't a plain name reads nothing back.
        let s = shape("UPDATE t SET (a, b) = (1, 2) WHERE id = 1").unwrap();
        assert!(s.set_columns.is_empty() && before_sql(&s).is_none());
        let s = shape("UPDATE t SET a[1] = 2 WHERE id = 1").unwrap();
        assert!(s.set_columns.is_empty());
    }

    #[test]
    fn only_one_select_is_derived() {
        assert!(derived("SELECT count(*) FROM t WHERE id = 1").is_ok());
        assert!(derived("SELECT 1; DELETE FROM t").is_err());
        assert!(derived("DELETE FROM t").is_err());
    }

    #[test]
    fn the_planner_rows_come_from_under_the_modify_node() {
        let plan = serde_json::json!([{"Plan": {"Node Type": "ModifyTable", "Plan Rows": 0,
            "Plans": [{"Node Type": "Index Scan", "Plan Rows": 3}]}}]);
        assert_eq!(planned_rows(&plan), Some(3));
        let plain = serde_json::json!([{"Plan": {"Node Type": "Result", "Plan Rows": 1}}]);
        assert_eq!(planned_rows(&plain), Some(1));
        assert_eq!(planned_rows(&serde_json::json!([])), None);
    }

    #[test]
    fn before_values_are_cut_and_keyed_by_column() {
        let long = "x".repeat(MAX_BEFORE_CELL + 5);
        assert!(cut(long).ends_with('…'));
        let b = Before {
            columns: vec!["value".into()],
            rows: vec![vec![Some("10".into())], vec![None]],
            more: false,
        };
        assert_eq!(
            b.to_json(),
            serde_json::json!([{"value": "10"}, {"value": null}])
        );
    }
}
