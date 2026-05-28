//! Reconstruct runnable SQL from application-side Hibernate logs.
//!
//! Pairs SQL statements (logger `org.hibernate.SQL`) with the bind parameters
//! logged separately. Both Hibernate generations are supported:
//!   - Hibernate 5: `binding parameter [1] as [INTEGER] - [42]`
//!     (logger `org.hibernate.type.descriptor.sql.BasicBinder`).
//!   - Hibernate 6: `binding parameter (1:INTEGER) <- [42]`
//!     (logger `org.hibernate.orm.jdbc.bind`).
//!
//! Lines are grouped by thread token (the first `[…]` group, before the
//! logger) so interleaved requests pair correctly. Each SQL line opens a
//! statement for its thread; subsequent bind lines on that thread attach to it
//! until the next SQL line.
//!
//! Bind values are logged untyped-but-with-a-type-name, so substitution quotes
//! them by type (`VARCHAR` → quoted, `INTEGER` → bare).
//!
//! `hibernate.format_sql=true` produces multi-line SQL on continuation lines
//! without a log header. We reassemble these by appending any "continuation-
//! shaped" line (leading whitespace + no `[…]` thread bracket) to the most
//! recently opened thread's SQL — Hibernate prints the formatted output in
//! one atomic log call so the contiguous-chunk assumption holds even under
//! multi-threaded interleaving.
//!
//! Known limitations (see BACKLOG.md): bind lines are logged at `TRACE` and
//! are often absent in production, in which case the `?`-form statement is
//! still emitted, just unsubstituted.

use crate::query::reconstruct::{BoundParam, ParamValue, ReconstructedQuery, Source};
use crate::query::subst::{self, PlaceholderStyle};
use std::collections::HashMap;

/// Parse a Hibernate log into reconstructed queries.
pub fn parse(log: &str) -> Vec<ReconstructedQuery> {
    // thread -> (sql, src_line) for the statement currently open on that thread.
    let mut current: HashMap<String, (String, usize)> = HashMap::new();
    // thread -> binds accumulated for the open statement.
    let mut binds: HashMap<String, Vec<BoundParam>> = HashMap::new();
    let mut out: Vec<ReconstructedQuery> = Vec::new();
    // Thread of the most-recently-opened SQL. Continuation lines (no
    // log header) attach here — Hibernate's `format_sql=true` prints
    // the formatted SQL atomically so the chunk for one thread is
    // contiguous in the log file.
    let mut last_open_thread: Option<String> = None;

    for (idx, line) in log.lines().enumerate() {
        let line_no = idx + 1;

        if let Some(bind_pos) = line.find("binding parameter") {
            let thread = extract_thread(&line[..bind_pos]);
            // Orphan binds (no open statement on the thread) are ignored.
            if current.contains_key(&thread) {
                if let Some(bp) = parse_bind(&line[bind_pos..]) {
                    binds.entry(thread).or_default().push(bp);
                }
            }
            continue;
        }

        if let Some((logger_pos, msg)) = sql_message(line) {
            let thread = extract_thread(&line[..logger_pos]);
            if let Some((sql, sline)) = current.remove(&thread) {
                let params = binds.remove(&thread).unwrap_or_default();
                finalize(sql, sline, params, &mut out);
            }
            let sql = msg.trim();
            // Empty first line is normal under `format_sql=true` —
            // the SQL body follows on the next continuation lines.
            // Insert the open statement either way so continuations
            // know where to attach.
            current.insert(thread.clone(), (sql.to_string(), line_no));
            last_open_thread = Some(thread);
            continue;
        }

        if looks_like_continuation(line) {
            if let Some(t) = last_open_thread.as_ref() {
                if let Some((sql, _)) = current.get_mut(t) {
                    if !sql.is_empty() {
                        sql.push('\n');
                    }
                    sql.push_str(line);
                }
            }
        }
    }

    for (thread, (sql, sline)) in current {
        let params = binds.remove(&thread).unwrap_or_default();
        finalize(sql, sline, params, &mut out);
    }
    out.sort_by_key(|q| q.src_line);
    out
}

fn finalize(
    raw_sql: String,
    src_line: usize,
    mut params: Vec<BoundParam>,
    out: &mut Vec<ReconstructedQuery>,
) {
    // Trim trailing whitespace introduced by reassembled multi-line
    // `format_sql=true` output (final continuation usually ends with a
    // `\n`). Skip empty statements — they happen when an SQL log line
    // had no body and no continuations followed.
    let raw_sql = raw_sql.trim().to_string();
    if raw_sql.is_empty() {
        return;
    }
    params.sort_by_key(|p| p.index);
    let runnable_sql = if params.is_empty() {
        raw_sql.clone()
    } else {
        subst::apply(&raw_sql, &params, PlaceholderStyle::QuestionMark)
            .unwrap_or_else(|_| raw_sql.clone())
    };
    out.push(ReconstructedQuery {
        raw_sql,
        params,
        runnable_sql,
        source: Source::HibernateLog,
        src_line,
    });
}

/// A continuation line under `hibernate.format_sql=true`: starts with
/// whitespace (formatted-SQL output indents every line) and contains
/// no `[…]` group (which a real log header would carry as the thread
/// or log-level brackets). Empty lines aren't treated as continuations
/// to avoid silently extending the open SQL with blank lines.
fn looks_like_continuation(line: &str) -> bool {
    if line.trim().is_empty() {
        return false;
    }
    let first = match line.chars().next() {
        Some(c) => c,
        None => return false,
    };
    if !first.is_whitespace() {
        return false;
    }
    !line.contains('[')
}

/// The thread token — the first `[…]` group in `prefix` (the text before the
/// logger / message). Empty when the log pattern carries no thread.
fn extract_thread(prefix: &str) -> String {
    if let Some(start) = prefix.find('[') {
        if let Some(rel_end) = prefix[start + 1..].find(']') {
            return prefix[start + 1..start + 1 + rel_end].trim().to_string();
        }
    }
    String::new()
}

/// If `line` is a `org.hibernate.SQL` line, return `(logger_position, message)`.
fn sql_message(line: &str) -> Option<(usize, &str)> {
    const MARKERS: &[&str] = &["org.hibernate.SQL", "o.h.SQL"];
    let (marker_pos, marker_len) = MARKERS.iter().find_map(|m| {
        let i = line.find(m)?;
        // Reject `org.hibernate.SQL_SLOW` and the like.
        match line.as_bytes().get(i + m.len()) {
            Some(&c) if c == b'_' || c.is_ascii_alphanumeric() => None,
            _ => Some((i, m.len())),
        }
    })?;
    let after = &line[marker_pos + marker_len..];
    let colon = after.find(':')?;
    Some((marker_pos, after[colon + 1..].trim()))
}

/// Parse a `binding parameter …` message (Hibernate 5 or 6 form).
fn parse_bind(msg: &str) -> Option<BoundParam> {
    let rest = msg.strip_prefix("binding parameter")?.trim_start();

    if let Some(r) = rest.strip_prefix('[') {
        // Hibernate 5: `[idx] as [TYPE] - [value]`
        let (idx, r) = r.split_once(']')?;
        let index = idx.trim().parse::<usize>().ok()?;
        let r = r.trim_start().strip_prefix("as")?.trim_start();
        let (sql_type, r) = r.strip_prefix('[')?.split_once(']')?;
        let value = bracketed_value(r)?;
        Some(BoundParam {
            index,
            sql_type: sql_type.trim().to_string(),
            value,
        })
    } else if let Some(r) = rest.strip_prefix('(') {
        // Hibernate 6: `(idx:TYPE) <- [value]`
        let (inside, r) = r.split_once(')')?;
        let (idx, sql_type) = inside.split_once(':')?;
        let index = idx.trim().parse::<usize>().ok()?;
        let value = bracketed_value(r)?;
        Some(BoundParam {
            index,
            sql_type: sql_type.trim().to_string(),
            value,
        })
    } else {
        None
    }
}

/// Extract the value from the last `[…]` group in `s` (Hibernate logs the bound
/// value last; `[null]` is the literal null).
fn bracketed_value(s: &str) -> Option<ParamValue> {
    let open = s.find('[')?;
    let inner = &s[open + 1..];
    let close = inner.rfind(']')?;
    let raw = &inner[..close];
    if raw.trim().eq_ignore_ascii_case("null") {
        Some(ParamValue::Null)
    } else {
        Some(ParamValue::Literal(raw.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_log_yields_nothing() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn hibernate5_sql_and_bind_reconstruct() {
        let log = "\
2024-01-15 10:00:00.123 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select c.id from customer c where c.id=?
2024-01-15 10:00:00.124 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].raw_sql, "select c.id from customer c where c.id=?");
        assert_eq!(
            q[0].runnable_sql,
            "select c.id from customer c where c.id=42"
        );
        assert_eq!(q[0].source, Source::HibernateLog);
    }

    #[test]
    fn hibernate6_bind_form_reconstructs_and_quotes_by_type() {
        let log = "\
2024-01-15 DEBUG --- [main] org.hibernate.SQL : select id from person where name=?
2024-01-15 TRACE --- [main] org.hibernate.orm.jdbc.bind : binding parameter (1:VARCHAR) <- [alice]";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        // VARCHAR is quoted; the value carries no quotes in the log.
        assert_eq!(
            q[0].runnable_sql,
            "select id from person where name='alice'"
        );
    }

    #[test]
    fn null_bind_becomes_unquoted_null() {
        let log = "\
[main] org.hibernate.SQL : update t set note=? where id=?
[main] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [VARCHAR] - [null]
[main] o.h.type.descriptor.sql.BasicBinder : binding parameter [2] as [INTEGER] - [7]";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].runnable_sql, "update t set note=NULL where id=7");
    }

    #[test]
    fn sql_without_binds_keeps_placeholders() {
        // TRACE bind logging off — the statement is still recovered.
        let log = "[main] org.hibernate.SQL : select * from t where id=?";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        assert!(q[0].params.is_empty());
        assert_eq!(q[0].runnable_sql, "select * from t where id=?");
    }

    #[test]
    fn interleaved_threads_pair_their_own_binds() {
        let log = "\
[exec-1] org.hibernate.SQL : select * from a where id=?
[exec-2] org.hibernate.SQL : select * from b where id=?
[exec-2] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [2]
[exec-1] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [1]";
        let q = parse(log);
        assert_eq!(q.len(), 2);
        // Sorted by src_line: the `a` query (line 1) then the `b` query.
        assert_eq!(q[0].runnable_sql, "select * from a where id=1");
        assert_eq!(q[1].runnable_sql, "select * from b where id=2");
    }

    #[test]
    fn multiple_binds_substitute_in_index_order() {
        let log = "\
[main] org.hibernate.SQL : select * from t where a=? and b=? and c=?
[main] o.h.type.descriptor.sql.BasicBinder : binding parameter [3] as [INTEGER] - [30]
[main] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [10]
[main] o.h.type.descriptor.sql.BasicBinder : binding parameter [2] as [INTEGER] - [20]";
        let q = parse(log);
        assert_eq!(q.len(), 1);
        assert_eq!(
            q[0].runnable_sql,
            "select * from t where a=10 and b=20 and c=30"
        );
    }

    #[test]
    fn orphan_bind_without_a_statement_is_ignored() {
        let log = "[main] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]";
        assert!(parse(log).is_empty());
    }

    #[test]
    fn sql_slow_logger_lines_are_not_treated_as_statements() {
        let log = "[main] org.hibernate.SQL_SLOW : SlowQuery: 2000 milliseconds. SQL: 'select 1'";
        assert!(parse(log).is_empty());
    }

    #[test]
    fn format_sql_multiline_output_is_reassembled() {
        // `hibernate.format_sql=true` emits the formatted SQL on
        // continuation lines (leading whitespace, no log header).
        let log = "\
[main] org.hibernate.SQL :
    select
        u.id,
        u.email
    from
        users u
    where
        u.id=?
[main] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]";
        let q = parse(log);
        assert_eq!(q.len(), 1, "expected 1 query, got {q:?}");
        // All four continuation tokens should be present.
        for token in &[
            "select", "u.id,", "u.email", "from", "users u", "where", "u.id=?",
        ] {
            assert!(
                q[0].raw_sql.contains(token),
                "raw_sql missing {token:?}; got {:?}",
                q[0].raw_sql
            );
        }
        assert!(q[0].runnable_sql.contains("u.id=42"));
    }

    #[test]
    fn format_sql_continuation_does_not_steal_other_thread_logs() {
        // T1 opens a one-line SQL; T2 opens a formatted (multi-line)
        // SQL. The continuations belong to T2 (the most-recently
        // opened thread), NOT T1. T1's one-liner stays clean.
        let log = "\
[T1] org.hibernate.SQL : select 1
[T2] org.hibernate.SQL :
    select
        *
    from
        users
[T1] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [9]
[T2] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [7]";
        let q = parse(log);
        assert_eq!(q.len(), 2);
        let by_thread_t1 = q
            .iter()
            .find(|r| r.raw_sql.starts_with("select 1"))
            .unwrap();
        let by_thread_t2 = q
            .iter()
            .find(|r| r.raw_sql.contains("from\n        users"))
            .unwrap();
        // T1's SQL never grew — no leaked continuation lines.
        assert_eq!(by_thread_t1.raw_sql, "select 1");
        // T2's SQL contains the continuation.
        assert!(by_thread_t2.raw_sql.contains("from"));
        assert!(by_thread_t2.raw_sql.contains("users"));
    }

    #[test]
    fn format_sql_empty_log_line_with_no_continuations_is_dropped() {
        // A SQL log line with an empty body (format_sql open) but no
        // continuations following — drop the empty record rather than
        // emit a `ReconstructedQuery` with empty `raw_sql`.
        let log = "[main] org.hibernate.SQL : ";
        assert!(parse(log).is_empty());
    }

    #[test]
    fn looks_like_continuation_accepts_indented_no_bracket_lines() {
        assert!(looks_like_continuation("    select"));
        assert!(looks_like_continuation("\tfrom users"));
    }

    #[test]
    fn looks_like_continuation_rejects_log_headers_and_blanks() {
        assert!(!looks_like_continuation(""));
        assert!(!looks_like_continuation("   "));
        // Leading whitespace but contains a `[…]` somewhere — typical
        // log line.
        assert!(!looks_like_continuation("  [main] DEBUG ..."));
        // No leading whitespace.
        assert!(!looks_like_continuation("select 1"));
    }
}
