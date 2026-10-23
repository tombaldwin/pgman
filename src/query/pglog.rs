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
//!   - `ERROR:  <message>` then `STATEMENT:  <sql>` from the same backend —
//!     what the default `log_min_error_statement = error` logs for a failed
//!     statement with nothing else enabled. It becomes a query marked failed
//!     with the error's message (`ReconstructedQuery::error`); parameters come
//!     from a `DETAIL:` / `CONTEXT:  … with parameters:` line in the report or
//!     the backend's last `bind`. When `log_statement` already logged the
//!     statement before it ran, that query is marked failed instead of a second
//!     copy being emitted — two copies would read as an N+1 loop.
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
//! bucket; a `FATAL` / `PANIC` + `STATEMENT` pair (a terminated backend)
//! is not reconstructed, only `ERROR`.

use crate::query::reconstruct::{BoundParam, ParamValue, ReconstructedQuery, Source};
use crate::query::subst::{self, PlaceholderStyle};
use std::collections::HashMap;

/// Parse a Postgres server log into reconstructed queries.
pub fn parse(log: &str) -> Vec<ReconstructedQuery> {
    let mut out: Vec<ReconstructedQuery> = Vec::new();
    let mut seen = Seen::default();
    let mut current: Option<Pending> = None;
    // A `parameters:` message whose last quoted value is still open: a
    // bound value with a newline in it runs on over continuation lines,
    // which belong to the value, not to the statement's SQL.
    let mut open_params: Option<OpenParams> = None;

    for (idx, line) in log.lines().enumerate() {
        let line_no = idx + 1;
        let Some(Record { prefix, level, msg }) = split_record(line) else {
            // Postgres writes a tab after every newline in a message;
            // that tab is the log's, not the statement's or the value's
            // — inside a string literal it would change the data.
            let line = line.strip_prefix('\t').unwrap_or(line);
            if let Some(open) = open_params.as_mut() {
                open.text.push('\n');
                open.text.push_str(line);
                let params = parse_parameters(&open.text);
                match &open.target {
                    ParamTarget::Current => {
                        if let Some(cur) = current.as_mut() {
                            cur.params = params;
                        }
                    }
                    ParamTarget::Error(pid) => {
                        if let Some(err) = seen.errors.get_mut(pid) {
                            err.params = params;
                        }
                    }
                }
                if !quote_open(&open.text) {
                    open_params = None;
                }
                continue;
            }
            // Not a record line — a continuation of a multi-line statement.
            if let Some(cur) = current.as_mut() {
                cur.sql.push('\n');
                cur.sql.push_str(line);
            }
            continue;
        };
        open_params = None;
        let pid = extract_pid(prefix).unwrap_or_default();
        // An ERROR's report runs on through DETAIL / HINT / CONTEXT / …
        // to its STATEMENT; anything else from that backend means the
        // STATEMENT is not coming (`log_min_error_statement` above
        // `error`), and a later one belongs to some other message.
        if !REPORT_LEVELS.contains(&level) {
            seen.errors.remove(&pid);
        }
        match level {
            "DETAIL" => {
                if msg.contains("parameters:") {
                    let params = parse_parameters(msg);
                    let target = if let Some(cur) = current.as_mut() {
                        if !params.is_empty() {
                            cur.params = params;
                        }
                        Some(ParamTarget::Current)
                    } else if let Some(err) = seen.errors.get_mut(&pid) {
                        err.params = params;
                        Some(ParamTarget::Error(pid.clone()))
                    } else {
                        None
                    };
                    open_params = OpenParams::new(msg, target);
                }
                // Non-parameter DETAIL lines belong to `current` — don't finalize.
            }
            "LOG" => match parse_log_message(msg) {
                Some((kind, sql)) => {
                    finalize(current.take(), &mut seen, &mut out);
                    current = Some(Pending {
                        kind,
                        pid,
                        sql: sql.to_string(),
                        params: Vec::new(),
                        line: line_no,
                        before_run: !msg.starts_with("duration:"),
                        error: None,
                    });
                }
                // A LOG line we don't recognise ends the current statement.
                None => finalize(current.take(), &mut seen, &mut out),
            },
            "ERROR" => {
                finalize(current.take(), &mut seen, &mut out);
                seen.errors.insert(
                    pid,
                    FailedRun {
                        message: msg.trim().to_string(),
                        params: Vec::new(),
                    },
                );
            }
            // PG 13+ `log_parameter_max_length_on_error`: `CONTEXT:
            // unnamed portal with parameters: $1 = '…'`.
            "CONTEXT" => {
                finalize(current.take(), &mut seen, &mut out);
                if msg.contains("parameters:") {
                    if let Some(err) = seen.errors.get_mut(&pid) {
                        err.params = parse_parameters(msg);
                        open_params = OpenParams::new(msg, Some(ParamTarget::Error(pid.clone())));
                    }
                }
            }
            "STATEMENT" => {
                finalize(current.take(), &mut seen, &mut out);
                if let Some(err) = seen.errors.remove(&pid) {
                    current = Some(Pending {
                        kind: Kind::Failed,
                        pid,
                        sql: msg.to_string(),
                        params: err.params,
                        line: line_no,
                        before_run: false,
                        error: Some(err.message),
                    });
                }
            }
            // WARNING / HINT / FATAL / … end the current statement.
            _ => finalize(current.take(), &mut seen, &mut out),
        }
    }
    finalize(current.take(), &mut seen, &mut out);
    out.sort_by_key(|q| q.src_line);
    out
}

/// Where a `parameters:` message's values go: the statement being
/// built, or a backend's pending `ERROR:`.
enum ParamTarget {
    Current,
    Error(String),
}

/// A `parameters:` message that ended inside a quoted value — the rest
/// of the value is on the continuation lines that follow.
struct OpenParams {
    text: String,
    target: ParamTarget,
}

impl OpenParams {
    /// `Some` only when `msg`'s last quoted value is unterminated.
    fn new(msg: &str, target: Option<ParamTarget>) -> Option<Self> {
        let target = target?;
        quote_open(msg).then(|| OpenParams {
            text: msg.to_string(),
            target,
        })
    }
}

/// True if `params` (a `parameters: $1 = '…', …` message) ends inside a
/// quoted value. Postgres doubles a quote inside a value, so an odd
/// number of quotes means the last one opened a value that hasn't closed.
fn quote_open(params: &str) -> bool {
    params.bytes().filter(|&b| b == b'\'').count() % 2 == 1
}

/// The levels an `ERROR`'s report carries between it and its
/// `STATEMENT` (and the `STATEMENT` itself): a record at any of these
/// keeps the backend's pending error.
const REPORT_LEVELS: &[&str] = &[
    "DETAIL",
    "HINT",
    "CONTEXT",
    "LOCATION",
    "QUERY",
    "STATEMENT",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Statement,
    Execute,
    Bind,
    Parse,
    /// An `ERROR`'s `STATEMENT:` — the statement that failed.
    Failed,
}

struct Pending {
    kind: Kind,
    pid: String,
    sql: String,
    params: Vec<BoundParam>,
    line: usize,
    /// Logged before it ran (`log_statement`), not after
    /// (`log_min_duration_statement`'s `duration: … statement:`) — so
    /// an ERROR + STATEMENT that follows may be this same run failing.
    before_run: bool,
    /// The `ERROR:` message, for [`Kind::Failed`].
    error: Option<String>,
}

/// An `ERROR:` waiting for its `STATEMENT:`.
struct FailedRun {
    message: String,
    params: Vec<BoundParam>,
}

/// Per-backend state the records leave behind for the ones after them.
#[derive(Default)]
struct Seen {
    /// pid -> (sql, params) from the most recent `bind` for that backend.
    stashed: HashMap<String, (String, Vec<BoundParam>)>,
    /// pid -> the `ERROR:` whose report is still being read.
    errors: HashMap<String, FailedRun>,
    /// pid -> index in `out` of that backend's latest query, when it was
    /// logged before it ran — the run an ERROR + STATEMENT for the same
    /// SQL reports on.
    before_run: HashMap<String, usize>,
}

fn finalize(pending: Option<Pending>, seen: &mut Seen, out: &mut Vec<ReconstructedQuery>) {
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
            seen.stashed
                .insert(p.pid.clone(), (raw_sql, std::mem::take(&mut p.params)));
        }
        Kind::Statement | Kind::Execute | Kind::Failed => {
            if p.kind == Kind::Failed {
                // `log_statement` logged this run already: mark that
                // query failed rather than emit it twice.
                if let Some(&i) = seen.before_run.get(&p.pid) {
                    if out[i].raw_sql == raw_sql && out[i].error.is_none() {
                        out[i].error = p.error.take();
                        seen.before_run.remove(&p.pid);
                        return;
                    }
                }
            }
            let mut params = std::mem::take(&mut p.params);
            if params.is_empty() && p.kind != Kind::Statement {
                if let Some((bind_sql, bind_params)) = seen.stashed.get(&p.pid) {
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
                error: p.error,
            });
            if p.before_run && p.kind != Kind::Failed {
                seen.before_run.insert(p.pid, out.len() - 1);
            } else {
                seen.before_run.remove(&p.pid);
            }
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
                    "select id\nfrom customer\nwhere active",
                    "select * from invoices where id = '48202'",
                    "select * from invoice_lnes\nwhere invoice_id = 1",
                    "select 1",
                ],
                "log_line_prefix = '{format}'"
            );
            assert_eq!(
                q.iter().map(|q| q.src_line).collect::<Vec<_>>(),
                [1, 3, 8, 10, 12],
                "log_line_prefix = '{format}'"
            );
            assert_eq!(
                q.iter().map(|q| q.error.as_deref()).collect::<Vec<_>>(),
                [
                    None,
                    None,
                    None,
                    Some("relation \"invoice_lnes\" does not exist at character 15"),
                    None
                ],
                "log_line_prefix = '{format}'"
            );
        }
    }

    /// The default `log_min_error_statement = error` with nothing else
    /// on: a failed statement is logged as its ERROR and STATEMENT only.
    #[test]
    fn an_error_and_its_statement_become_a_failed_query() {
        let log = "\
2026-10-21 09:12:48 UTC [101] LOG:  checkpoint starting: time
2026-10-21 09:12:49 UTC [101] ERROR:  column \"totl\" does not exist at character 8
2026-10-21 09:12:49 UTC [101] HINT:  Perhaps you meant to reference the column \"invoices.total\".
2026-10-21 09:12:49 UTC [101] STATEMENT:  select totl
\tfrom invoices
\twhere id = 7
2026-10-21 09:12:50 UTC [101] LOG:  statement: select 1";
        let q = parse(log);
        assert_eq!(q.len(), 2, "{q:?}");
        assert_eq!(q[0].raw_sql, "select totl\nfrom invoices\nwhere id = 7");
        assert_eq!(q[0].runnable_sql, q[0].raw_sql);
        assert_eq!(q[0].src_line, 4, "the STATEMENT line, where the SQL is");
        assert_eq!(
            q[0].error.as_deref(),
            Some("column \"totl\" does not exist at character 8")
        );
        assert_eq!(q[1].raw_sql, "select 1");
        assert_eq!(q[1].error, None);
    }

    /// `log_statement = 'all'` logs the statement before it runs, then
    /// the ERROR + STATEMENT when it fails: one run, so one query, the
    /// logged one, marked failed. Two would read as a ×2 N+1 cluster.
    #[test]
    fn a_failure_log_statement_already_logged_marks_that_query_instead_of_repeating_it() {
        let log = "\
[101] LOG:  statement: update invoices set total = 0 where id = 7
[101] ERROR:  cannot execute UPDATE in a read-only transaction
[101] STATEMENT:  update invoices set total = 0 where id = 7
[101] LOG:  execute <unnamed>: select * from invoice_lines where invoice_id = $1
[101] DETAIL:  parameters: $1 = '48202'
[101] ERROR:  canceling statement due to statement timeout
[101] STATEMENT:  select * from invoice_lines where invoice_id = $1";
        let q = parse(log);
        assert_eq!(q.len(), 2, "{q:?}");
        assert_eq!(q[0].src_line, 1);
        assert_eq!(
            q[0].error.as_deref(),
            Some("cannot execute UPDATE in a read-only transaction")
        );
        assert_eq!(q[1].src_line, 4);
        assert_eq!(
            q[1].runnable_sql, "select * from invoice_lines where invoice_id = '48202'",
            "the execute's parameters are kept"
        );
        assert_eq!(
            q[1].error.as_deref(),
            Some("canceling statement due to statement timeout")
        );
        assert!(crate::query::nplus1::detect(&q).is_empty(), "no N+1 here");
    }

    /// `log_min_duration_statement` logs a statement after it finished,
    /// so the same SQL failing next is a second run, not this one.
    #[test]
    fn a_failure_after_a_duration_logged_success_is_its_own_query() {
        let log = "\
[101] LOG:  duration: 0.300 ms  statement: delete from locks where id = 1
[101] ERROR:  could not obtain lock on row in relation \"locks\"
[101] STATEMENT:  delete from locks where id = 1";
        let q = parse(log);
        assert_eq!(q.len(), 2, "{q:?}");
        assert_eq!(q[0].error, None, "the first run succeeded");
        assert!(q[1].error.is_some());
        assert_eq!(q[1].src_line, 3);
    }

    /// Parameters for a failed extended-protocol statement: from PG 13's
    /// `CONTEXT:  … with parameters:` in the report, or else from the
    /// backend's last `bind` of the same SQL.
    #[test]
    fn a_failed_statement_takes_its_parameters_from_the_report_or_the_bind() {
        let log = "\
[101] ERROR:  invalid input syntax for type integer: \"x\"
[101] CONTEXT:  unnamed portal with parameters: $1 = 'x'
[101] STATEMENT:  select * from t where id = $1
[202] LOG:  bind <unnamed>: select * from t where id = $1
[202] DETAIL:  parameters: $1 = '9'
[202] ERROR:  deadlock detected
[202] DETAIL:  Process 202 waits for ShareLock on transaction 5; blocked by process 101.
[202] STATEMENT:  select * from t where id = $1";
        let q = parse(log);
        assert_eq!(q.len(), 2, "{q:?}");
        assert_eq!(q[0].runnable_sql, "select * from t where id = 'x'");
        assert_eq!(q[1].runnable_sql, "select * from t where id = '9'");
        assert_eq!(q[1].error.as_deref(), Some("deadlock detected"));
    }

    #[test]
    fn an_error_pairs_with_its_own_backends_statement_only() {
        let log = "\
[101] ERROR:  relation \"a\" does not exist
[202] LOG:  statement: select 2
[202] STATEMENT:  select from_202
[101] STATEMENT:  select * from a
[303] ERROR:  relation \"b\" does not exist
[303] LOG:  statement: select 3
[303] STATEMENT:  select * from b
[404] WARNING:  there is no transaction in progress
[404] STATEMENT:  commit";
        let q = parse(log);
        let got: Vec<(&str, bool)> = q
            .iter()
            .map(|q| (q.raw_sql.as_str(), q.error.is_some()))
            .collect();
        assert_eq!(
            got,
            [
                ("select 2", false),
                ("select * from a", true),
                // 303's report ended at its LOG: the later STATEMENT is
                // not the ERROR's. A WARNING's STATEMENT did not fail.
                ("select 3", false),
            ]
        );
    }

    /// The same failing shape, run with different values, is a loop
    /// like any other.
    #[test]
    fn failed_statements_cluster_like_any_other() {
        let log = (1..=3)
            .map(|i| {
                format!(
                    "[101] ERROR:  permission denied for table audit\n\
                     [101] STATEMENT:  insert into audit values ({i})"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let q = parse(&log);
        assert_eq!(q.len(), 3);
        assert!(q.iter().all(|q| q.error.is_some()));
        let clusters = crate::query::nplus1::detect(&q);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].count, 3);
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

    /// A bound value with a newline in it: Postgres 16 writes the rest of
    /// the value on a tab-indented continuation line. It belongs to the
    /// value, not to the statement.
    #[test]
    fn a_parameter_value_with_a_newline_continues_on_the_next_line() {
        let log = "\
2026-10-23 09:53:54.340 UTC [1869] LOG:  duration: 0.018 ms  parse <unnamed>: insert into probe_t (id, note) values ($1, $2)
2026-10-23 09:53:54.341 UTC [1869] LOG:  duration: 0.048 ms  bind <unnamed>: insert into probe_t (id, note) values ($1, $2)
2026-10-23 09:53:54.341 UTC [1869] DETAIL:  parameters: $1 = '4', $2 = 'line one
\tline two\ttabbed'
2026-10-23 09:53:54.341 UTC [1869] LOG:  execute <unnamed>: insert into probe_t (id, note) values ($1, $2)
2026-10-23 09:53:54.341 UTC [1869] DETAIL:  parameters: $1 = '4', $2 = 'line one
\tit''s
\tthree'
2026-10-23 09:53:54.341 UTC [1869] LOG:  duration: 0.193 ms
2026-10-23 09:53:54.342 UTC [1869] ERROR:  value too long for type character varying(3)
2026-10-23 09:53:54.342 UTC [1869] CONTEXT:  unnamed portal with parameters: $1 = 'a
\tb'
2026-10-23 09:53:54.342 UTC [1869] STATEMENT:  select $1::varchar(3)";
        let q = parse(log);
        assert_eq!(q.len(), 2, "{q:#?}");
        assert_eq!(
            q[0].raw_sql,
            "insert into probe_t (id, note) values ($1, $2)"
        );
        assert_eq!(
            q[0].params[1].value,
            ParamValue::Literal("line one\nit's\nthree".to_string())
        );
        assert_eq!(
            q[0].runnable_sql,
            "insert into probe_t (id, note) values ('4', 'line one\nit''s\nthree')"
        );
        assert_eq!(q[1].runnable_sql, "select 'a\nb'::varchar(3)");
        // The bind's own value, with the tab inside the value kept.
        let bind_only = log.lines().take(4).collect::<Vec<_>>().join("\n")
            + "\n2026-10-23 09:53:54.341 UTC [1869] LOG:  execute <unnamed>: insert into probe_t (id, note) values ($1, $2) ";
        let q = parse(&bind_only);
        assert_eq!(
            q[0].runnable_sql,
            "insert into probe_t (id, note) values ('4', 'line one\nline two\ttabbed')"
        );
    }

    /// The tab Postgres writes after each newline of a message is the
    /// log's: inside a string literal that spans lines, keeping it would
    /// change the data the statement writes.
    #[test]
    fn a_statements_continuation_lines_lose_the_tab_postgres_added() {
        let log = "\
[7] LOG:  statement: insert into notes values ('line one
\t\tindented two')";
        let q = parse(log);
        assert_eq!(
            q[0].raw_sql,
            "insert into notes values ('line one\n\tindented two')"
        );
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
