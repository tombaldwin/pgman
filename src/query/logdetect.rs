//! Sniff pasted / buffer text for the log framing that [`super::hibernate`]
//! and [`super::pglog`] key off, so the editor can point the user at
//! `ctrl-l` / `F8` (`App::start_log_import`) instead of leaving them to
//! guess that a *SQL editor* is also where a *log* goes.
//!
//! Deliberately conservative: this only looks for the actual log framing
//! (a Hibernate logger line, a `binding parameter` bind, a Postgres
//! `LOG:`/`DETAIL:` record). Ordinary SQL — including a `?` or `$1`
//! placeholder sitting on its own — must not match; those placeholders are
//! exactly what a *runnable* pasted statement looks like, and this is not
//! the parser, just a hint.

/// Which reconstruction source a pasted/buffer text looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Hibernate,
    PgServer,
    /// A pasted `?`-placeholder statement plus a typed parameter list —
    /// see [`split_jdbc_paste`] for the exact shape and
    /// `query::jdbc::parse` for the reconstruction.
    Jdbc,
}

impl LogKind {
    /// The source this kind reconstructs into.
    pub fn source(self) -> super::reconstruct::Source {
        use super::reconstruct::Source;
        match self {
            LogKind::Hibernate => Source::HibernateLog,
            LogKind::PgServer => Source::PostgresLog,
            LogKind::Jdbc => Source::JdbcPaste,
        }
    }

    /// The source's UI word (`Source::label`), so the hint and the
    /// picker's `[tag]` can't drift apart.
    pub fn label(self) -> &'static str {
        self.source().label()
    }
}

/// How much of a buffer [`head_for_detection`] hands to
/// [`detect_log`]. A log paste announces itself in its first lines —
/// a logger name, a `LOG:` record, a parameter tail — so scanning
/// past this buys nothing, and the scan runs on every buffer change.
pub const DETECT_HEAD_BYTES: usize = 64 * 1024;

/// The leading [`DETECT_HEAD_BYTES`] of `text`, cut back to a char
/// boundary. Shorter input is returned whole.
///
/// The trade-off is explicit: a log pasted BELOW 64 KiB of something
/// else won't fire the hint. `Ctrl-L` / `F8` still reconstruct from
/// the whole buffer — this bounds the hint, not the import.
pub fn head_for_detection(text: &str) -> &str {
    if text.len() <= DETECT_HEAD_BYTES {
        return text;
    }
    let mut end = DETECT_HEAD_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `text` without ANSI SGR colour escapes (`ESC [ 0-9;* m`).
///
/// A log captured from a coloured console carries them —
/// `hibernate.highlight_sql=true` wraps every SQL keyword in
/// `\x1b[34m…\x1b[0m`, and Spring Boot colours the level and logger — and
/// they hide the framing every parser keys off (a continuation line no
/// longer starts with whitespace-then-keyword, `LOG:` is no longer at a
/// token boundary). Every log parser strips them first. Borrowed when
/// there are none.
pub fn strip_sgr(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\x1b') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\x1b') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let sgr_len = after.strip_prefix('[').and_then(|params| {
            let digits = params
                .bytes()
                .take_while(|b| b.is_ascii_digit() || *b == b';')
                .count();
            (params.as_bytes().get(digits) == Some(&b'm')).then_some(1 + digits + 1)
        });
        match sgr_len {
            Some(len) => rest = &after[len..],
            None => {
                // Not a colour escape: keep the ESC as it was.
                out.push('\x1b');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// Detect whether `text` looks like a Hibernate application log or a
/// Postgres/RDS server log. `None` for plain SQL, prose, or empty input.
///
/// Line-oriented and cheap (a handful of substring scans over the text) —
/// safe to call once per paste or once per buffer change, not safe to call
/// unconditionally every render frame against a multi-MB buffer.
pub fn detect_log(text: &str) -> Option<LogKind> {
    let text = &*strip_sgr(text);
    if text.trim().is_empty() {
        return None;
    }

    // Hibernate: the `org.hibernate.SQL` / `o.h.SQL` logger line that opens
    // a statement (but not `org.hibernate.SQL_SLOW`, a different logger
    // entirely), or a `binding parameter` bind line (Hibernate 5 or 6
    // form — see `hibernate::parse_bind`).
    let looks_hibernate = text.lines().any(|line| {
        (line.contains("org.hibernate.SQL") && !line.contains("org.hibernate.SQL_SLOW"))
            || line.contains("o.h.SQL")
            || line.contains("binding parameter")
            || line.trim_start().starts_with("Hibernate:")
    });
    if looks_hibernate {
        return Some(LogKind::Hibernate);
    }

    // Postgres server log: a record `pglog::parse` reconstructs from —
    // asked of the parser itself, so a log the hint names is one the
    // parser reads, whatever its `log_line_prefix`.
    if text.lines().any(super::pglog::is_query_record) {
        return Some(LogKind::PgServer);
    }
    // A log of failures only (the default `log_min_error_statement`
    // with nothing else on): whether an `ERROR:` + `STATEMENT:` pair
    // reconstructs depends on the records around it, so ask the parser
    // — only when a `STATEMENT:` is there at all.
    if text.contains("STATEMENT:") && !super::pglog::parse(text).is_empty() {
        return Some(LogKind::PgServer);
    }

    if split_jdbc_paste(text).is_some() {
        return Some(LogKind::Jdbc);
    }

    None
}

/// Split `text` on its first blank line into `(head, tail)` when the
/// shape looks like a pasted JDBC statement plus a typed parameter
/// list: `head` (everything before the blank line) contains a `?`
/// placeholder, and every non-empty line of `tail` (everything after)
/// is a `TYPE:value` parameter — a leading run of letters, a colon,
/// then the value (`INTEGER:42`, `VARCHAR:Alice`, `VARCHAR:NULL`).
///
/// `None` for plain SQL with a bare `?` and no parameter tail at all
/// (that's exactly what a *runnable* pasted statement looks like — see
/// the module docs), for text with no blank line, or for a tail line
/// that isn't a `TYPE:value` pair.
///
/// Shared by [`detect_log`] (fires the hint) and `App::start_log_import`
/// (does the actual `query::jdbc::parse(head, tail)` reconstruction) so
/// the two can never disagree on what counts as the shape.
pub fn split_jdbc_paste(text: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let blank_idx = lines.iter().position(|l| l.trim().is_empty())?;
    let head = &lines[..blank_idx];
    let tail = &lines[blank_idx + 1..];
    if head.is_empty() || tail.is_empty() {
        return None;
    }
    if !head.iter().any(|l| l.contains('?')) {
        return None;
    }
    let mut saw_param = false;
    for line in tail {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let (ty, _) = t.split_once(':')?;
        if ty.is_empty() || !ty.chars().all(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        saw_param = true;
    }
    if !saw_param {
        return None;
    }
    Some((head.join("\n"), tail.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_for_detection_cuts_at_the_cap_on_a_char_boundary() {
        let short = "LOG:  statement: select 1";
        assert_eq!(head_for_detection(short), short);
        // A multi-byte char straddling the cap must not be split.
        let mut long = "é".repeat(DETECT_HEAD_BYTES); // 2 bytes each
        long.push_str("tail");
        let head = head_for_detection(&long);
        assert!(head.len() <= DETECT_HEAD_BYTES);
        assert!(long.starts_with(head));
        assert!(!head.contains("tail"));
    }

    #[test]
    fn strip_sgr_removes_colour_escapes_only() {
        assert_eq!(strip_sgr("\x1b[34mselect\x1b[0m 1"), "select 1");
        assert_eq!(strip_sgr("\x1b[1;31mERROR\x1b[m:  x"), "ERROR:  x");
        // Not SGR (a cursor move, a bare ESC, an unterminated one): kept.
        assert_eq!(strip_sgr("a\x1b[2Kb"), "a\x1b[2Kb");
        assert_eq!(strip_sgr("a\x1bb\x1b[3"), "a\x1bb\x1b[3");
        assert!(matches!(
            strip_sgr("plain"),
            std::borrow::Cow::Borrowed("plain")
        ));
    }

    /// A Postgres log copied out of a colouring pager: the hint and
    /// the parser both see through the escapes.
    #[test]
    fn a_coloured_postgres_log_is_detected_and_parsed() {
        let log = "2026-10-21 09:12:48 UTC [31840] \x1b[32mLOG\x1b[0m:  statement: select 2";
        assert_eq!(detect_log(log), Some(LogKind::PgServer));
        let q = super::super::pglog::parse(log);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].runnable_sql, "select 2");
    }

    #[test]
    fn detects_hibernate_log() {
        let log = "\
2024-01-15 10:00:00.123 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select c.id from customer c where c.id=?
2024-01-15 10:00:00.124 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]
2024-01-15 10:00:00.125 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select * from orders where customer_id=?";
        assert_eq!(detect_log(log), Some(LogKind::Hibernate));
    }

    #[test]
    fn detects_pg_server_log() {
        let log = "\
2024-01-15 10:00:00.001 UTC [101] LOG:  duration: 1.200 ms  execute <unnamed>: select * from orders where id = $1
2024-01-15 10:00:00.001 UTC [101] DETAIL:  parameters: $1 = '42'
2024-01-15 10:00:01.500 UTC [102] LOG:  statement: select now()";
        assert_eq!(detect_log(log), Some(LogKind::PgServer));
    }

    #[test]
    fn the_hint_fires_exactly_when_the_parser_finds_a_query() {
        // The release-panel failure: the hint named a log the parser then
        // found nothing in. Each case here must agree with `pglog::parse`.
        for (text, parses) in [
            // RDS's prefix, and Debian's `%u@%d` one.
            (
                "2026-10-21 09:12:48 UTC:10.0.1.23(51234):billing@ledger:[31840]:LOG:  statement: select 1",
                true,
            ),
            (
                "2026-10-21 09:12:48.001 UTC [31840] billing@ledger LOG:  statement: select 1",
                true,
            ),
            // SQL that merely mentions a log record, in a literal.
            ("select 'x LOG:  statement: select 1'", false),
            // A log with no statements in it at all.
            (
                "2026-10-21 09:12:48 UTC [31840] LOG:  checkpoint starting: time",
                false,
            ),
            // Failures only: an ERROR and its STATEMENT.
            (
                "2026-10-21 09:12:48 UTC [31840] ERROR:  relation \"t\" does not exist\n\
                 2026-10-21 09:12:48 UTC [31840] STATEMENT:  select * from t",
                true,
            ),
            // A WARNING's STATEMENT did not fail, and is not a query.
            (
                "2026-10-21 09:12:48 UTC [31840] WARNING:  there is no transaction in progress\n\
                 2026-10-21 09:12:48 UTC [31840] STATEMENT:  commit",
                false,
            ),
        ] {
            assert_eq!(
                detect_log(text) == Some(LogKind::PgServer),
                parses,
                "hint for {text:?}"
            );
            assert_eq!(
                !super::super::pglog::parse(text).is_empty(),
                parses,
                "parse of {text:?}"
            );
        }
    }

    #[test]
    fn plain_sql_is_not_a_log() {
        let sql = "select * from customer where id = ? and status = $1;";
        assert_eq!(detect_log(sql), None);
        let sql = "SELECT id, name FROM widgets WHERE created_at > now() - interval '1 day';";
        assert_eq!(detect_log(sql), None);
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(detect_log(""), None);
        assert_eq!(detect_log("   \n\t  "), None);
    }

    #[test]
    fn detects_jdbc_paste_shape() {
        let text = "select * from t where id = ? and name = ?\n\nINTEGER:42\nVARCHAR:Alice";
        assert_eq!(detect_log(text), Some(LogKind::Jdbc));
    }

    #[test]
    fn plain_sql_with_placeholder_and_no_tail_is_not_jdbc() {
        let sql = "SELECT * FROM foo WHERE id = ?";
        assert_eq!(detect_log(sql), None);
    }

    #[test]
    fn split_jdbc_paste_extracts_head_and_tail() {
        let text = "select * from t where id = ?\n\nINTEGER:42";
        assert_eq!(
            split_jdbc_paste(text),
            Some((
                "select * from t where id = ?".to_string(),
                "INTEGER:42".to_string()
            ))
        );
    }

    #[test]
    fn split_jdbc_paste_rejects_a_tail_line_that_is_not_typed() {
        // Blank line present, head has a `?`, but the tail has a line
        // with no `TYPE:` prefix — not the shape.
        let text = "select * from t where id = ?\n\njust some text";
        assert_eq!(split_jdbc_paste(text), None);
    }

    #[test]
    fn split_jdbc_paste_rejects_head_without_placeholder() {
        let text = "select * from t where id = 1\n\nINTEGER:42";
        assert_eq!(split_jdbc_paste(text), None);
    }

    #[test]
    fn hibernate_log_with_crlf_line_endings() {
        let log = "2024-01-15 10:00:00.123 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select c.id from customer c where c.id=?\r\n2024-01-15 10:00:00.124 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]\r\n2024-01-15 10:00:00.125 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select * from orders where customer_id=?\r\n";
        assert_eq!(detect_log(log), Some(LogKind::Hibernate));
    }
}
