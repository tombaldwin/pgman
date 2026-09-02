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
}

impl LogKind {
    /// Short tag matching the label already used for reconstructed-query
    /// sources elsewhere in the UI (see `ui::panels`'s
    /// `Source::HibernateLog` → `"hibernate"` / `Source::PostgresLog` →
    /// `"pglog"` mapping) — kept identical so the hint and the picker use
    /// the same word for the same thing.
    pub fn label(self) -> &'static str {
        match self {
            LogKind::Hibernate => "hibernate",
            LogKind::PgServer => "pglog",
        }
    }
}

/// Detect whether `text` looks like a Hibernate application log or a
/// Postgres/RDS server log. `None` for plain SQL, prose, or empty input.
///
/// Line-oriented and cheap (a handful of substring scans over the text) —
/// safe to call once per paste or once per buffer change, not safe to call
/// unconditionally every render frame against a multi-MB buffer.
pub fn detect_log(text: &str) -> Option<LogKind> {
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
    });
    if looks_hibernate {
        return Some(LogKind::Hibernate);
    }

    // Postgres server log: a `LOG:` record whose message opens with
    // `statement:` / `duration:` / `execute` / `parse` / `bind` (the forms
    // `pglog::parse_log_message` recognises), or the paired
    // `DETAIL: … parameters:` line. Whitespace after the level colon
    // varies by `log_line_prefix`, so trim rather than requiring an exact
    // number of spaces.
    let looks_pglog = text.lines().any(|line| {
        after_marker(line, "LOG:").is_some_and(|rest| {
            rest.starts_with("statement:")
                || rest.starts_with("duration:")
                || rest.starts_with("execute ")
                || rest.starts_with("parse ")
                || rest.starts_with("bind ")
        }) || after_marker(line, "DETAIL:").is_some_and(|rest| rest.starts_with("parameters:"))
    });
    if looks_pglog {
        return Some(LogKind::PgServer);
    }

    None
}

/// If `line` contains `marker`, the trimmed text right after it.
fn after_marker<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let idx = line.find(marker)?;
    Some(line[idx + marker.len()..].trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn hibernate_log_with_crlf_line_endings() {
        let log = "2024-01-15 10:00:00.123 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select c.id from customer c where c.id=?\r\n2024-01-15 10:00:00.124 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]\r\n2024-01-15 10:00:00.125 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select * from orders where customer_id=?\r\n";
        assert_eq!(detect_log(log), Some(LogKind::Hibernate));
    }
}
