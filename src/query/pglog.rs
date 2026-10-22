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
//! Any `log_line_prefix` works — the record's severity marker is found
//! after whatever the prefix printed (see [`split_record`]).
//!
//! Known limitations: an *unindented* continuation line that opens with
//! a severity marker (`ERROR:  …` at column 0, e.g. the second line of a
//! multi-line string literal in a paste that lost Postgres's tabs) reads
//! as a record; backends with no `[%p]` in the prefix share one pairing
//! bucket; `ERROR` + `STATEMENT` pairs are not reconstructed.

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
        match split_record(line).map(|r| (r.level, r.msg, r.prefix)) {
            Some(("DETAIL", msg, _)) => {
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
            Some(("LOG", msg, prefix)) => match parse_log_message(msg) {
                Some((kind, sql)) => {
                    finalize(current.take(), &mut stashed, &mut out);
                    current = Some(Pending {
                        kind,
                        pid: extract_pid(prefix).unwrap_or_default(),
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

/// Severity markers a record can carry, as Postgres prints them.
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
    "DEBUG1",
    "DEBUG2",
    "DEBUG3",
    "DEBUG4",
    "DEBUG5",
    "LOCATION",
    "QUERY",
];

/// How far into a line the severity marker may sit. The longest common
/// `log_line_prefix` (pgBadger's, with an application name and client
/// address) is around 120 bytes; past this it's message text.
const MAX_PREFIX: usize = 200;

/// One log record header: the `log_line_prefix` output, the severity,
/// and the message after it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Record<'a> {
    pub prefix: &'a str,
    pub level: &'static str,
    pub msg: &'a str,
}

/// Split a log line into its prefix, severity and message. `None` for a
/// continuation line of a multi-line message.
///
/// The prefix is whatever `log_line_prefix` printed — `%m [%p] `,
/// `%m [%p] %q%u@%d `, RDS's `%t:%r:%u@%d:[%p]:`, pgBadger's
/// `%t [%p]: [%l-1] user=%u,db=%d,app=%a,client=%h ` — so rather than
/// enumerating prefixes this finds the first severity marker (`LOG:`,
/// `ERROR:`, …) that sits on a token boundary and is followed by
/// whitespace, and then checks that the text before it could be a
/// prefix rather than SQL:
///
///   - a line that opens with whitespace is a continuation: Postgres
///     indents every continuation line of a message with a tab;
///   - a prefix never contains a quote or a comment opener, so a marker
///     after `'`, `"`, `--` or `/*` is inside a SQL literal or comment
///     (`WHERE note = 'LOG: x'`);
///   - a prefix is short ([`MAX_PREFIX`]).
pub(crate) fn split_record(line: &str) -> Option<Record<'_>> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let bytes = line.as_bytes();
    let limit = line.len().min(MAX_PREFIX);
    let (at, level) = (0..=limit).find_map(|i| {
        // A marker starts a token: line start or after a non-word byte.
        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            return None;
        }
        let rest = line.get(i..)?;
        LEVELS.iter().find_map(|&lvl| {
            let after = rest.strip_prefix(lvl)?.strip_prefix(':')?;
            (after.is_empty() || after.starts_with([' ', '\t'])).then_some((i, lvl))
        })
    })?;
    let prefix = &line[..at];
    if prefix.contains(['\'', '"']) || prefix.contains("--") || prefix.contains("/*") {
        return None;
    }
    let msg = line[at + level.len() + 1..].trim_start();
    Some(Record { prefix, level, msg })
}

/// True if `line` is a record [`parse`] builds a query from: a `LOG:`
/// statement / execute / bind / parse, or a `DETAIL:  parameters:` line.
/// `logdetect` keys its hint off this, so the hint and the parser can't
/// disagree about what a Postgres log looks like.
pub(crate) fn is_query_record(line: &str) -> bool {
    match split_record(line) {
        Some(Record {
            level: "LOG", msg, ..
        }) => parse_log_message(msg).is_some(),
        Some(Record {
            level: "DETAIL",
            msg,
            ..
        }) => msg.starts_with("parameters:"),
        _ => false,
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

/// Backend pid from the first all-digit `[…]` group in a record's
/// prefix (Postgres `[%p]`). A `[%l-1]` line counter isn't all digits.
fn extract_pid(prefix: &str) -> Option<String> {
    prefix.match_indices('[').find_map(|(open, _)| {
        let rest = &prefix[open + 1..];
        let inside = &rest[..rest.find(']')?];
        (!inside.is_empty() && inside.bytes().all(|b| b.is_ascii_digit()))
            .then(|| inside.to_string())
    })
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
        // not a header: no `log_line_prefix` is that long.
        let prefix = "x".repeat(MAX_PREFIX + 10);
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
            Some(Record {
                prefix: "[7] ",
                level: "LOG",
                msg: "statement: select 1"
            })
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

    /// `log_line_prefix` outputs seen in the wild, each for backend 31840.
    /// The Debian/Ubuntu default and RDS's are the two that matter most.
    const PREFIXES: &[(&str, &str)] = &[
        ("%m [%p] ", "2026-10-21 09:12:48.001 UTC [31840] "),
        ("%t [%p] ", "2026-10-21 09:12:48 UTC [31840] "),
        // Debian/Ubuntu's default.
        (
            "%m [%p] %q%u@%d ",
            "2026-10-21 09:12:48.001 UTC [31840] billing@ledger ",
        ),
        (
            "%t [%p] %q%u@%d ",
            "2026-10-21 09:12:48 UTC [31840] billing@ledger ",
        ),
        // RDS's (fixed on RDS).
        (
            "%t:%r:%u@%d:[%p]:",
            "2026-10-21 09:12:48 UTC:10.0.1.23(51234):billing@ledger:[31840]:",
        ),
        (
            "%m [%p] %a ",
            "2026-10-21 09:12:48.001 UTC [31840] PostgreSQL JDBC Driver ",
        ),
        (
            "%m [%p] app=%a ",
            "2026-10-21 09:12:48.001 UTC [31840] app=examplecorp-api ",
        ),
        // pgBadger's recommended prefix.
        (
            "%t [%p]: [%l-1] user=%u,db=%d,app=%a,client=%h ",
            "2026-10-21 09:12:48 UTC [31840]: [3-1] user=billing,db=ledger,app=PostgreSQL JDBC Driver,client=10.0.1.23 ",
        ),
        (
            "%m [%p]: [%l-1] user=%u,db=%d,app=%a,client=%h ",
            "2026-10-21 09:12:48.001 UTC [31840]: [3-1] user=billing,db=ledger,app=[unknown],client=10.0.1.23 ",
        ),
    ];

    /// One log in a given prefix: execute + parameters, a multi-line
    /// `statement:`, `duration:` on a bind then its execute, an
    /// ERROR + STATEMENT pair (with a continuation), and a trailing
    /// statement. Continuation lines are tab-indented, as Postgres
    /// writes them.
    fn fixture(p: &str) -> String {
        [
            format!("{p}LOG:  duration: 12.345 ms  execute S_1: select id, total from invoices where status = $1 and total > $2"),
            format!("{p}DETAIL:  parameters: $1 = 'open', $2 = '100'"),
            format!("{p}LOG:  statement: select id"),
            "\tfrom customer".to_string(),
            "\twhere active".to_string(),
            format!("{p}LOG:  duration: 0.120 ms  bind <unnamed>: select * from invoices where id = $1"),
            format!("{p}DETAIL:  parameters: $1 = '48202'"),
            format!("{p}LOG:  execute <unnamed>: select * from invoices where id = $1"),
            format!("{p}ERROR:  relation \"invoice_lnes\" does not exist at character 15"),
            format!("{p}STATEMENT:  select * from invoice_lnes"),
            "\twhere invoice_id = 1".to_string(),
            format!("{p}LOG:  statement: select 1"),
        ]
        .join("\n")
    }

    #[test]
    fn every_common_log_line_prefix_reconstructs() {
        for (format, prefix) in PREFIXES {
            let q = parse(&fixture(prefix));
            let sql: Vec<&str> = q.iter().map(|q| q.runnable_sql.as_str()).collect();
            assert_eq!(
                sql,
                [
                    "select id, total from invoices where status = 'open' and total > '100'",
                    "select id\n\tfrom customer\n\twhere active",
                    "select * from invoices where id = '48202'",
                    "select 1",
                ],
                "log_line_prefix = '{format}'"
            );
            assert_eq!(
                q.iter().map(|q| q.src_line).collect::<Vec<_>>(),
                [1, 3, 8, 12],
                "log_line_prefix = '{format}'"
            );
        }
    }

    #[test]
    fn every_common_log_line_prefix_yields_its_pid() {
        for (format, prefix) in PREFIXES {
            let line = format!("{prefix}LOG:  statement: select 1");
            let rec = split_record(&line).unwrap_or_else(|| panic!("'{format}' not split"));
            assert_eq!(rec.prefix, *prefix, "'{format}'");
            assert_eq!(rec.level, "LOG", "'{format}'");
            assert_eq!(
                extract_pid(rec.prefix).as_deref(),
                Some("31840"),
                "'{format}'"
            );
        }
    }

    #[test]
    fn user_at_db_prefixes_from_the_release_panel_reconstruct() {
        // Exactly the two pastes that came back "no queries found".
        for log in [
            "2026-10-21 09:12:48.001 UTC [31840] billing@ledger LOG:  duration: 12.345 ms  execute S_1: select * from invoices where status = $1 and total > $2\n\
             2026-10-21 09:12:48.001 UTC [31840] billing@ledger DETAIL:  parameters: $1 = 'open', $2 = '100'",
            "2026-10-21 09:12:48 UTC:10.0.1.23(51234):billing@ledger:[31840]:LOG:  duration: 12.345 ms  execute S_1: select * from invoices where status = $1 and total > $2\n\
             2026-10-21 09:12:48 UTC:10.0.1.23(51234):billing@ledger:[31840]:DETAIL:  parameters: $1 = 'open', $2 = '100'",
        ] {
            let q = parse(log);
            assert_eq!(q.len(), 1, "{log}");
            assert_eq!(
                q[0].runnable_sql,
                "select * from invoices where status = 'open' and total > '100'"
            );
        }
    }

    #[test]
    fn bind_pairs_by_pid_under_a_user_at_db_prefix() {
        // The pid comes out of the prefix, so the bind's parameters
        // still find their execute when backends interleave.
        let log = "\
2026-10-21 09:12:48 UTC [101] billing@ledger LOG:  bind <unnamed>: select * from t where id = $1
2026-10-21 09:12:48 UTC [101] billing@ledger DETAIL:  parameters: $1 = 'one'
2026-10-21 09:12:48 UTC [202] billing@ledger LOG:  bind <unnamed>: select * from t where id = $1
2026-10-21 09:12:48 UTC [202] billing@ledger DETAIL:  parameters: $1 = 'two'
2026-10-21 09:12:48 UTC [101] billing@ledger LOG:  execute <unnamed>: select * from t where id = $1
2026-10-21 09:12:48 UTC [202] billing@ledger LOG:  execute <unnamed>: select * from t where id = $1";
        let q = parse(log);
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].runnable_sql, "select * from t where id = 'one'");
        assert_eq!(q[1].runnable_sql, "select * from t where id = 'two'");
    }

    #[test]
    fn severity_words_inside_sql_are_not_markers() {
        // Continuation lines whose SQL mentions a severity word: in a
        // literal, in a comment, as part of a longer word, without the
        // colon-and-space, or indented as Postgres writes them.
        for line in [
            "WHERE note = 'something LOG: bad'",
            "WHERE note = \"ERROR: col\"",
            "and x = 1 -- ERROR: fixme",
            "and x = 1 /* LOG: later */",
            "and SYSLOG: 1",
            "and ERRORS: 1",
            "select LOG::text",
            "\tLOG:  statement: select 1",
            "    ERROR:  indented",
        ] {
            assert!(split_record(line).is_none(), "{line:?} split as a record");
        }
    }
}
