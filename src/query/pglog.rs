//! Reconstruct runnable SQL from PostgreSQL / RDS server logs.
//!
//! This is the *primary* reconstruction source: with `log_min_duration_statement`
//! or `log_statement = 'all'`, Postgres logs the statement and its
//! `DETAIL: parameters:` line, and it can be enabled server-side with no
//! application redeploy.
//!
//! Supported input:
//!   - `LOG:  statement: <sql>` — simple queries.
//!   - `LOG:  duration: N ms  execute <tag>: <sql>` then
//!     `DETAIL:  parameters: $1 = '…'` — extended protocol via
//!     `log_min_duration_statement`.
//!   - `LOG:  parse/bind/execute <tag>: <sql>` from `log_statement = 'all'`:
//!     `bind` carries the parameters, `execute` is the execution event. One
//!     `ReconstructedQuery` is emitted per `statement` / `execute`; the matching
//!     `bind`'s parameters are paired in by backend pid.
//!
//! Parameter values are logged by Postgres as quoted text regardless of type
//! (`$1 = '42'`), so they are reconstructed as quoted literals — re-running
//! relies on Postgres's implicit casts, which is safe.
//!
//! Known limitations (see BACKLOG.md): a SQL line that itself contains a log
//! level token (`LOG:` etc.) confuses line-splitting; backends with an
//! unparseable pid share one pairing bucket.

use crate::query::reconstruct::{BoundParam, ParamValue, ReconstructedQuery, Source};
use crate::query::subst::{self, PlaceholderStyle};
use std::collections::HashMap;

/// Parse a Postgres server log into reconstructed queries.
pub fn parse(log: &str) -> Vec<ReconstructedQuery> {
    let mut out: Vec<ReconstructedQuery> = Vec::new();
    // pid -> (sql, params) from the most recent `bind` for that backend.
    let mut stashed: HashMap<String, (String, Vec<BoundParam>)> = HashMap::new();
    let mut current: Option<Pending> = None;

    for (idx, line) in log.lines().enumerate() {
        let line_no = idx + 1;
        match split_record(line) {
            Some(("DETAIL", msg)) => {
                if msg.contains("parameters:") {
                    if let Some(cur) = current.as_mut() {
                        let params = parse_parameters(msg);
                        if !params.is_empty() {
                            cur.params = params;
                        }
                    }
                }
                // Non-parameter DETAIL lines belong to `current` — don't finalize.
            }
            Some(("LOG", msg)) => match parse_log_message(msg) {
                Some((kind, sql)) => {
                    finalize(current.take(), &mut stashed, &mut out);
                    current = Some(Pending {
                        kind,
                        pid: extract_pid(line).unwrap_or_default(),
                        sql: sql.to_string(),
                        params: Vec::new(),
                        line: line_no,
                    });
                }
                // A LOG line we don't recognise ends the current statement.
                None => finalize(current.take(), &mut stashed, &mut out),
            },
            // ERROR / STATEMENT / WARNING / HINT / … end the current statement.
            Some(_) => finalize(current.take(), &mut stashed, &mut out),
            // Not a record line — a continuation of a multi-line statement.
            None => {
                if let Some(cur) = current.as_mut() {
                    cur.sql.push('\n');
                    cur.sql.push_str(line);
                }
            }
        }
    }
    finalize(current.take(), &mut stashed, &mut out);
    out.sort_by_key(|q| q.src_line);
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Statement,
    Execute,
    Bind,
    Parse,
}

struct Pending {
    kind: Kind,
    pid: String,
    sql: String,
    params: Vec<BoundParam>,
    line: usize,
}

fn finalize(
    pending: Option<Pending>,
    stashed: &mut HashMap<String, (String, Vec<BoundParam>)>,
    out: &mut Vec<ReconstructedQuery>,
) {
    let Some(mut p) = pending else {
        return;
    };
    let raw_sql = p.sql.trim().to_string();
    if raw_sql.is_empty() {
        return;
    }
    match p.kind {
        // `parse` only declares the statement — nothing to emit.
        Kind::Parse => {}
        // `bind` carries the parameters; stash them for the matching `execute`.
        Kind::Bind => {
            stashed.insert(p.pid.clone(), (raw_sql, std::mem::take(&mut p.params)));
        }
        Kind::Statement | Kind::Execute => {
            let mut params = std::mem::take(&mut p.params);
            if params.is_empty() && p.kind == Kind::Execute {
                if let Some((bind_sql, bind_params)) = stashed.get(&p.pid) {
                    if *bind_sql == raw_sql {
                        params = bind_params.clone();
                    }
                }
            }
            let runnable_sql = if params.is_empty() {
                raw_sql.clone()
            } else {
                subst::apply(&raw_sql, &params, PlaceholderStyle::Numbered)
                    .unwrap_or_else(|_| raw_sql.clone())
            };
            out.push(ReconstructedQuery {
                raw_sql,
                params,
                runnable_sql,
                source: Source::PostgresLog,
                src_line: p.line,
            });
        }
    }
}

/// Split a log line into its level and the message after it. Returns `None`
/// for continuation lines (no level token at a record-header position).
///
/// A "record-header position" is one of:
///   - directly after a `[<digits>] ` pid bracket near the start of the
///     line (Postgres's standard `log_line_prefix` shape), or
///   - the very start of the line (logs without a pid prefix).
///
/// This deliberately rejects level-token-shaped substrings that sit
/// inside SQL on a continuation line — e.g. `WHERE note = 'LOG: x'` no
/// longer mis-splits the statement.
fn split_record(line: &str) -> Option<(&'static str, &str)> {
    const LEVELS: &[&str] = &[
        "LOG",
        "DETAIL",
        "STATEMENT",
        "ERROR",
        "WARNING",
        "FATAL",
        "PANIC",
        "HINT",
        "CONTEXT",
        "NOTICE",
        "INFO",
    ];
    let header_start = find_after_pid_bracket(line).unwrap_or(0);
    for &lvl in LEVELS {
        let needle = format!("{lvl}:");
        if line[header_start..].starts_with(&needle) {
            let after = header_start + needle.len();
            return Some((lvl, line[after..].trim_start()));
        }
    }
    None
}

/// Locate the byte position right after a `[<digits>] ` group near the
/// start of `line` — the conventional pg log header terminator. Returns
/// `None` if no such group exists, the bracket sits past a reasonable
/// header length (so we don't match brackets buried inside SQL bodies),
/// or anything before the bracket looks like SQL string content (a `'`
/// is the giveaway).
fn find_after_pid_bracket(line: &str) -> Option<usize> {
    // Real pg log_line_prefix headers are short (timestamp + tz + pid
    // ≈ 50 chars). Anything past 80 is almost certainly the message
    // body — refuse to look there for `[…]`.
    const MAX_HEADER: usize = 80;
    let lb = line.find('[')?;
    if lb > MAX_HEADER {
        return None;
    }
    // Anything resembling a SQL string literal before the bracket is
    // a strong signal we're inside a continuation line whose content
    // happens to include `[…]`.
    if line[..lb].contains('\'') {
        return None;
    }
    let rb_rel = line[lb + 1..].find(']')?;
    let inside = &line[lb + 1..lb + 1 + rb_rel];
    if inside.is_empty() || !inside.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let after = lb + 1 + rb_rel + 1;
    // Standard `[pid] LEVEL:` puts exactly one space here.
    if line.as_bytes().get(after) == Some(&b' ') {
        Some(after + 1)
    } else {
        None
    }
}

/// Recognise a `LOG:` message as a statement, returning its kind and SQL.
fn parse_log_message(msg: &str) -> Option<(Kind, &str)> {
    let msg = strip_duration_prefix(msg);
    if let Some(rest) = msg.strip_prefix("statement:") {
        return Some((Kind::Statement, rest.trim_start()));
    }
    for (kw, kind) in [
        ("execute ", Kind::Execute),
        ("bind ", Kind::Bind),
        ("parse ", Kind::Parse),
    ] {
        if let Some(rest) = msg.strip_prefix(kw) {
            // `rest` is `<tag>: <sql>` — the tag (`<unnamed>`, `S_1`, …) has
            // no embedded `: `.
            if let Some((_tag, sql)) = rest.split_once(": ") {
                return Some((kind, sql));
            }
            if let Some((_tag, sql)) = rest.split_once(':') {
                return Some((kind, sql.trim_start()));
            }
        }
    }
    None
}

/// Strip a `duration: N ms  ` prefix, leaving the statement that follows it.
fn strip_duration_prefix(msg: &str) -> &str {
    if let Some(rest) = msg.strip_prefix("duration:") {
        for kw in ["statement:", "execute ", "bind ", "parse "] {
            if let Some(i) = rest.find(kw) {
                return &rest[i..];
            }
        }
        return rest.trim_start();
    }
    msg
}

/// Backend pid from the first all-digit `[…]` group (Postgres `[%p]`).
fn extract_pid(line: &str) -> Option<String> {
    let start = line.find('[')?;
    let end = line[start + 1..].find(']')? + start + 1;
    let inside = &line[start + 1..end];
    if !inside.is_empty() && inside.bytes().all(|b| b.is_ascii_digit()) {
        Some(inside.to_string())
    } else {
        None
    }
}

/// Parse a `DETAIL: … parameters: $1 = '…', $2 = NULL` message.
fn parse_parameters(detail: &str) -> Vec<BoundParam> {
    let body = match detail.split_once("parameters:") {
        Some((_, b)) => b,
        None => return Vec::new(),
    };
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        if chars[i] != '$' {
            i += 1;
            continue;
        }
        i += 1;
        let mut num = String::new();
        while i < chars.len() && chars[i].is_ascii_digit() {
            num.push(chars[i]);
            i += 1;
        }
        let Ok(index) = num.parse::<usize>() else {
            continue;
        };
        // Skip up to and past the `=`.
        while i < chars.len() && chars[i] != '=' {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        i += 1;
        while i < chars.len() && chars[i] == ' ' {
            i += 1;
        }
        let value = if i < chars.len() && chars[i] == '\'' {
            // Quoted string — read to the closing quote ('' escapes).
            i += 1;
            let mut val = String::new();
            while i < chars.len() {
                if chars[i] == '\'' {
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        val.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                val.push(chars[i]);
                i += 1;
            }
            ParamValue::Literal(val)
        } else {
            // Bare token (`NULL`) up to the next separator.
            let mut tok = String::new();
            while i < chars.len() && chars[i] != ',' {
                tok.push(chars[i]);
                i += 1;
            }
            let tok = tok.trim();
            if tok.eq_ignore_ascii_case("null") {
                ParamValue::Null
            } else if tok.is_empty() {
                continue;
            } else {
                ParamValue::Literal(tok.to_string())
            }
        };
        // Postgres logs values as text; mark as a quoted type so substitution
        // quotes string values (numbers re-cast implicitly on re-run).
        out.push(BoundParam {
            index,
            sql_type: "text".to_string(),
            value,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_log_yields_nothing() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn simple_statement_without_parameters() {
        let log = "2024-01-15 10:00:00.001 UTC [101] LOG:  statement: select now()";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].raw_sql, "select now()");
        assert_eq!(q[0].runnable_sql, "select now()");
        assert!(q[0].params.is_empty());
        assert_eq!(q[0].source, Source::PostgresLog);
        assert_eq!(q[0].src_line, 1);
    }

    #[test]
    fn execute_with_parameters_from_duration_logging() {
        let log = "\
2024-01-15 10:00:00.001 UTC [101] LOG:  duration: 1.200 ms  execute <unnamed>: select * from orders where id = $1
2024-01-15 10:00:00.001 UTC [101] DETAIL:  parameters: $1 = '42'";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].raw_sql, "select * from orders where id = $1");
        assert_eq!(q[0].params.len(), 1);
        assert_eq!(q[0].runnable_sql, "select * from orders where id = '42'");
    }

    #[test]
    fn log_statement_all_pairs_bind_params_into_execute() {
        // parse / bind / execute triplet — params follow `bind`, the `execute`
        // is the execution event. One query, with the bound parameter.
        let log = "\
2024-01-15 10:00:00 UTC [101] LOG:  parse <unnamed>: select x from t where y = $1
2024-01-15 10:00:00 UTC [101] LOG:  bind <unnamed>: select x from t where y = $1
2024-01-15 10:00:00 UTC [101] DETAIL:  parameters: $1 = 'abc'
2024-01-15 10:00:00 UTC [101] LOG:  execute <unnamed>: select x from t where y = $1";
        let q = parse(log);
        assert_eq!(q.len(), 1, "parse+bind+execute should yield one query");
        assert_eq!(q[0].runnable_sql, "select x from t where y = 'abc'");
        assert_eq!(q[0].src_line, 4, "the execute line is the execution event");
    }

    #[test]
    fn null_parameter_becomes_unquoted_null() {
        let log = "\
[7] LOG:  execute <unnamed>: update t set note = $1 where id = $2
[7] DETAIL:  parameters: $1 = NULL, $2 = '9'";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].runnable_sql, "update t set note = NULL where id = '9'");
    }

    #[test]
    fn string_parameter_with_embedded_comma_is_not_split() {
        let log = "\
[7] LOG:  execute <unnamed>: insert into t values ($1, $2)
[7] DETAIL:  parameters: $1 = 'Smith, Jane', $2 = 'x'";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].params.len(), 2);
        assert_eq!(
            q[0].runnable_sql,
            "insert into t values ('Smith, Jane', 'x')"
        );
    }

    #[test]
    fn multi_line_statement_is_joined() {
        let log = "\
[5] LOG:  statement: select id
        from customer
        where active
[5] LOG:  statement: select 1";
        let q = parse(log);
        assert_eq!(q.len(), 2);
        assert!(q[0].raw_sql.contains("select id"));
        assert!(q[0].raw_sql.contains("from customer"));
        assert_eq!(q[1].raw_sql, "select 1");
    }

    #[test]
    fn interleaved_backends_keep_their_own_parameters() {
        // Two backends bind different values, then both execute.
        let log = "\
[101] LOG:  bind <unnamed>: select * from t where id = $1
[101] DETAIL:  parameters: $1 = 'one'
[202] LOG:  bind <unnamed>: select * from t where id = $1
[202] DETAIL:  parameters: $1 = 'two'
[101] LOG:  execute <unnamed>: select * from t where id = $1
[202] LOG:  execute <unnamed>: select * from t where id = $1";
        let q = parse(log);
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].runnable_sql, "select * from t where id = 'one'");
        assert_eq!(q[1].runnable_sql, "select * from t where id = 'two'");
    }

    #[test]
    fn split_record_ignores_bracket_with_non_digit_contents() {
        // `[hello]` isn't a pid bracket — even if a LEVEL: word
        // follows, it's not a real log header.
        assert!(split_record("note = '[hello] LOG: bad'").is_none());
    }

    #[test]
    fn split_record_ignores_bracket_buried_in_a_long_line() {
        // A `[<digits>]` deep into a long SQL line is the message body,
        // not a header. (Cap is 80.)
        let prefix = "x".repeat(120);
        let line = format!("{prefix}[101] LOG: bad");
        assert!(split_record(&line).is_none());
    }

    #[test]
    fn split_record_rejects_bracket_preceded_by_apostrophe() {
        // A `'` before `[…]` strongly implies we're inside a SQL string
        // literal continuation. Don't read the bracket as a pid.
        assert!(split_record("WHERE note = '... [101] LOG: bad'").is_none());
    }

    #[test]
    fn split_record_accepts_pid_only_header() {
        // `[<pid>] LEVEL: ...` — no timestamp prefix; still a valid pg
        // log header.
        assert_eq!(
            split_record("[7] LOG:  statement: select 1"),
            Some(("LOG", "statement: select 1"))
        );
    }

    #[test]
    fn continuation_line_with_log_level_in_string_literal_is_joined() {
        // A multi-line SQL whose continuation contains a literal
        // matching a log-level pattern must NOT be split. Previously
        // `split_record` found `LOG:` inside the string and started a
        // bogus second statement.
        let log = "\
[5] LOG:  statement: SELECT id
       FROM t WHERE note = 'something LOG: bad'
[5] LOG:  statement: select 1";
        let q = parse(log);
        assert_eq!(q.len(), 2, "expected 2 queries, got {q:?}");
        assert!(q[0].raw_sql.contains("FROM t WHERE note"));
        assert!(q[0].raw_sql.contains("'something LOG: bad'"));
        assert_eq!(q[1].raw_sql, "select 1");
    }

    #[test]
    fn execute_without_logged_binds_degrades_to_placeholder_sql() {
        // No bind/DETAIL — the parameters simply aren't in the log.
        let log = "[9] LOG:  execute <unnamed>: select * from t where id = $1";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        assert!(q[0].params.is_empty());
        assert_eq!(q[0].runnable_sql, "select * from t where id = $1");
    }
}
