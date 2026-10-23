//! Postgres's structured log destinations — `csvlog` (all supported
//! versions) and `jsonlog` (PG 15+) — read into the same reconstruction
//! as the stderr format.
//!
//! Each record is rewritten as the stderr records Postgres would have
//! printed for it — `[pid] SEVERITY:  message`, then `DETAIL:`, `HINT:`,
//! `CONTEXT:` and `STATEMENT:` from their own fields, a newline in any of
//! them followed by the tab stderr adds — and handed to
//! [`super::pglog`]'s parser, so pairing a bind with its execute or an
//! error with its statement works one way for every format. Each
//! reconstructed query's `src_line` is mapped back to the line its record
//! starts on in the original log.
//!
//! The pid is the pairing key, as `[%p]` is in a stderr prefix.

/// The structured format a log is in, from its first non-blank line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Json,
}

/// The fields of one record the reconstruction reads.
#[derive(Debug, Default, PartialEq, Eq)]
struct Record {
    /// 1-based line the record starts on.
    line: usize,
    pid: String,
    severity: String,
    message: String,
    detail: String,
    hint: String,
    context: String,
    statement: String,
}

/// csvlog column positions, per the "Using CSV-Format Log Output"
/// table, unchanged from PG 13 to 17 for the columns read here.
mod col {
    pub const PID: usize = 3;
    pub const SEVERITY: usize = 11;
    pub const MESSAGE: usize = 13;
    pub const DETAIL: usize = 14;
    pub const HINT: usize = 15;
    pub const CONTEXT: usize = 18;
    pub const QUERY: usize = 19;
    /// PG 13's 24 columns end at `backend_type`; 14+ add `leader_pid`
    /// and `query_id`. Fewer than this and it isn't a csvlog line.
    pub const MIN: usize = 23;
}

/// Which structured format `log` is in, if any: the first non-blank
/// line is a csvlog record (enough columns, a pid, a known severity) or
/// a jsonlog object (`error_severity` and `message` keys).
pub fn sniff(log: &str) -> Option<Format> {
    let first = log.lines().find(|l| !l.trim().is_empty())?;
    if first.trim_start().starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(first.trim()).ok()?;
        let has = |k: &str| v.get(k).is_some_and(serde_json::Value::is_string);
        return (has("error_severity") && has("message")).then_some(Format::Json);
    }
    let (fields, _) = csv_record(first.as_bytes(), 0)?;
    let is_csv = fields.len() >= col::MIN
        && is_pid(&fields[col::PID])
        && super::pglog::is_severity(&fields[col::SEVERITY]);
    is_csv.then_some(Format::Csv)
}

fn is_pid(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `log` (in `format`) as stderr-format text, plus the original line of
/// each line of that text (`origin[i]` for line `i + 1`).
pub fn to_stderr(log: &str, format: Format) -> (String, Vec<usize>) {
    let records = match format {
        Format::Csv => csv_records(log),
        Format::Json => json_records(log),
    };
    let mut text = String::new();
    let mut origin = Vec::new();
    for r in &records {
        // A severity the stderr parser doesn't know would read as a
        // continuation of the previous record; a WARNING ends it the same
        // way any record the parser doesn't build from does.
        let severity = if super::pglog::is_severity(&r.severity) {
            r.severity.as_str()
        } else {
            "WARNING"
        };
        let mut emit = |level: &str, body: &str| {
            text.push_str(&format!("[{}] {level}:  ", r.pid));
            // stderr writes a tab after every newline of a message.
            text.push_str(&body.replace('\n', "\n\t"));
            text.push('\n');
            origin.extend(std::iter::repeat_n(r.line, body.matches('\n').count() + 1));
        };
        emit(severity, &r.message);
        for (level, body) in [
            ("DETAIL", &r.detail),
            ("HINT", &r.hint),
            ("CONTEXT", &r.context),
            ("STATEMENT", &r.statement),
        ] {
            if !body.is_empty() {
                emit(level, body);
            }
        }
    }
    (text, origin)
}

/// Every csvlog record in `log`. A record that isn't one (too few
/// columns) is skipped; an unterminated quoted field runs to the end.
fn csv_records(log: &str) -> Vec<Record> {
    let bytes = log.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0;
    let mut line = 1;
    while pos < bytes.len() {
        let Some((fields, next)) = csv_record(bytes, pos) else {
            break;
        };
        let start_line = line;
        line += bytes[pos..next].iter().filter(|&&b| b == b'\n').count();
        pos = next;
        if fields.len() < col::MIN {
            continue;
        }
        let f = |i: usize| fields.get(i).cloned().unwrap_or_default();
        out.push(Record {
            line: start_line,
            pid: f(col::PID),
            severity: f(col::SEVERITY),
            message: f(col::MESSAGE),
            detail: f(col::DETAIL),
            hint: f(col::HINT),
            context: f(col::CONTEXT),
            statement: f(col::QUERY),
        });
    }
    out
}

/// One RFC 4180 record from `bytes[pos..]`: its fields and the offset
/// just past its line ending. A quoted field may hold commas, `""` for a
/// quote, and newlines. `None` at the end of input.
fn csv_record(bytes: &[u8], mut pos: usize) -> Option<(Vec<String>, usize)> {
    if pos >= bytes.len() {
        return None;
    }
    let mut fields = Vec::new();
    let mut field = Vec::new();
    let mut quoted = false;
    while pos < bytes.len() {
        let b = bytes[pos];
        pos += 1;
        if quoted {
            if b == b'"' {
                if bytes.get(pos) == Some(&b'"') {
                    field.push(b'"');
                    pos += 1;
                } else {
                    quoted = false;
                }
            } else {
                field.push(b);
            }
            continue;
        }
        match b {
            b'"' => quoted = true,
            b',' => fields.push(String::from_utf8_lossy(&std::mem::take(&mut field)).into_owned()),
            b'\n' => break,
            b'\r' if bytes.get(pos) == Some(&b'\n') => {}
            _ => field.push(b),
        }
    }
    fields.push(String::from_utf8_lossy(&field).into_owned());
    Some((fields, pos))
}

/// Every jsonlog record in `log`: one JSON object per line. A line that
/// isn't one is skipped.
fn json_records(log: &str) -> Vec<Record> {
    log.lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let v: serde_json::Value = serde_json::from_str(l.trim()).ok()?;
            let s = |k: &str| match v.get(k) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => String::new(),
            };
            Some(Record {
                line: i + 1,
                pid: s("pid"),
                severity: s("error_severity"),
                message: s("message"),
                detail: s("detail"),
                hint: s("hint"),
                context: s("context"),
                statement: s("statement"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::query::pglog::parse;

    /// (runnable SQL, src_line, error) for each query `log` reconstructs.
    fn picks(log: &str) -> Vec<(String, usize, Option<String>)> {
        parse(log)
            .into_iter()
            .map(|q| (q.runnable_sql, q.src_line, q.error))
            .collect()
    }

    /// An execute with its parameters, a multi-line statement in a quoted
    /// field, a bind / execute pair with a multi-line value, and an ERROR
    /// whose statement is in the record's `query` column (PG 16's 26
    /// columns; the bind row is PG 13's 24).
    const CSVLOG: &str = "\
2026-10-21 09:12:48.001 UTC,\"billing\",\"ledger\",31840,\"10.0.1.23:51234\",6530a1b2.7c60,3,\"SELECT\",2026-10-21 09:12:47 UTC,5/12,0,LOG,00000,\"duration: 12.345 ms  execute S_1: select id, total from invoices where status = $1 and total > $2\",\"parameters: $1 = 'open', $2 = '100'\",,,,,,,,\"PostgreSQL JDBC Driver\",\"client backend\",,0
2026-10-21 09:12:48.010 UTC,\"billing\",\"ledger\",31840,\"10.0.1.23:51234\",6530a1b2.7c60,4,\"idle\",2026-10-21 09:12:47 UTC,5/13,0,LOG,00000,\"statement: select id
\tfrom customer
\twhere active\",,,,,,,,,\"PostgreSQL JDBC Driver\",\"client backend\",,0
2026-10-21 09:12:48.015 UTC,\"billing\",\"ledger\",31841,\"10.0.1.24:51235\",6530a1b3.7c61,1,\"BIND\",2026-10-21 09:12:47 UTC,6/1,0,LOG,00000,\"bind <unnamed>: insert into notes (id, note) values ($1, $2)\",\"parameters: $1 = '4', $2 = 'line one
line \"\"two\"\"'\",,,,,,,,\"PostgreSQL JDBC Driver\",\"client backend\"
2026-10-21 09:12:48.016 UTC,\"billing\",\"ledger\",31841,\"10.0.1.24:51235\",6530a1b3.7c61,2,\"INSERT\",2026-10-21 09:12:47 UTC,6/1,0,LOG,00000,\"execute <unnamed>: insert into notes (id, note) values ($1, $2)\",,,,,,,,,\"PostgreSQL JDBC Driver\",\"client backend\"
2026-10-21 09:12:48.020 UTC,\"billing\",\"ledger\",31840,\"10.0.1.23:51234\",6530a1b2.7c60,5,\"SELECT\",2026-10-21 09:12:47 UTC,5/14,0,ERROR,42P01,\"relation \"\"invoice_lnes\"\" does not exist\",,,,,,\"select * from invoice_lnes where invoice_id = 1\",15,,\"PostgreSQL JDBC Driver\",\"client backend\",,0
";

    #[test]
    fn a_csvlog_reconstructs_like_the_stderr_format() {
        assert_eq!(sniff(CSVLOG), Some(Format::Csv));
        assert_eq!(
            picks(CSVLOG),
            [
                (
                    "select id, total from invoices where status = 'open' and total > '100'"
                        .to_string(),
                    1,
                    None
                ),
                (
                    "select id\n\tfrom customer\n\twhere active".to_string(),
                    2,
                    None
                ),
                (
                    "insert into notes (id, note) values ('4', 'line one\nline \"two\"')"
                        .to_string(),
                    7,
                    None
                ),
                (
                    "select * from invoice_lnes where invoice_id = 1".to_string(),
                    8,
                    Some("relation \"invoice_lnes\" does not exist".to_string())
                ),
            ]
        );
        assert_eq!(
            crate::query::logdetect::detect_log(CSVLOG),
            Some(crate::query::logdetect::LogKind::PgServer)
        );
    }

    const JSONLOG: &str = r#"{"timestamp":"2026-10-21 09:12:48.001 UTC","user":"billing","dbname":"ledger","pid":31840,"remote_host":"10.0.1.23","remote_port":51234,"session_id":"6530a1b2.7c60","line_num":3,"ps":"SELECT","session_start":"2026-10-21 09:12:47 UTC","vxid":"5/12","txid":0,"error_severity":"LOG","message":"duration: 12.345 ms  execute S_1: select id, total from invoices where status = $1 and total > $2","detail":"parameters: $1 = 'open', $2 = '100'","application_name":"PostgreSQL JDBC Driver","backend_type":"client backend","query_id":0}
{"timestamp":"2026-10-21 09:12:48.010 UTC","user":"billing","dbname":"ledger","pid":31840,"remote_host":"10.0.1.23","remote_port":51234,"session_id":"6530a1b2.7c60","line_num":4,"ps":"idle","session_start":"2026-10-21 09:12:47 UTC","vxid":"5/13","txid":0,"error_severity":"LOG","message":"statement: select id\n\tfrom customer\n\twhere active","application_name":"PostgreSQL JDBC Driver","backend_type":"client backend","query_id":0}
{"timestamp":"2026-10-21 09:12:48.020 UTC","user":"billing","dbname":"ledger","pid":31840,"remote_host":"10.0.1.23","remote_port":51234,"session_id":"6530a1b2.7c60","line_num":5,"ps":"SELECT","session_start":"2026-10-21 09:12:47 UTC","vxid":"5/14","txid":0,"error_severity":"ERROR","state_code":"42P01","message":"relation \"invoice_lnes\" does not exist","statement":"select * from invoice_lnes where invoice_id = 1","cursor_position":15,"application_name":"PostgreSQL JDBC Driver","backend_type":"client backend","query_id":0}
{"timestamp":"2026-10-21 09:12:49.000 UTC","pid":31841,"error_severity":"FATAL","message":"terminating connection due to conflict with recovery","detail":"User query might have needed to see row versions that must be removed.","statement":"select 1 from invoice_lines where invoice_id = 7"}
"#;

    #[test]
    fn a_jsonlog_reconstructs_like_the_stderr_format() {
        assert_eq!(sniff(JSONLOG), Some(Format::Json));
        assert_eq!(
            picks(JSONLOG),
            [
                (
                    "select id, total from invoices where status = 'open' and total > '100'"
                        .to_string(),
                    1,
                    None
                ),
                (
                    "select id\n\tfrom customer\n\twhere active".to_string(),
                    2,
                    None
                ),
                (
                    "select * from invoice_lnes where invoice_id = 1".to_string(),
                    3,
                    Some("relation \"invoice_lnes\" does not exist".to_string())
                ),
                (
                    "select 1 from invoice_lines where invoice_id = 7".to_string(),
                    4,
                    Some("terminating connection due to conflict with recovery".to_string())
                ),
            ]
        );
        assert_eq!(
            crate::query::logdetect::detect_log(JSONLOG),
            Some(crate::query::logdetect::LogKind::PgServer)
        );
    }

    /// jsonlog's pid is a number; it still keys a bind to its backend's
    /// execute when backends interleave.
    #[test]
    fn jsonlog_binds_pair_by_pid() {
        let log = r#"{"pid":101,"error_severity":"LOG","message":"bind <unnamed>: select * from t where id = $1","detail":"parameters: $1 = 'one'"}
{"pid":202,"error_severity":"LOG","message":"bind <unnamed>: select * from t where id = $1","detail":"parameters: $1 = 'two'"}
{"pid":101,"error_severity":"LOG","message":"execute <unnamed>: select * from t where id = $1"}
{"pid":202,"error_severity":"LOG","message":"execute <unnamed>: select * from t where id = $1"}"#;
        let sql: Vec<String> = picks(log).into_iter().map(|p| p.0).collect();
        assert_eq!(
            sql,
            [
                "select * from t where id = 'one'",
                "select * from t where id = 'two'"
            ]
        );
    }

    #[test]
    fn csv_record_handles_quotes_commas_and_newlines() {
        let text = "a,\"b, \"\"c\"\"\",\"x\ny\",\r\nnext";
        let (fields, next) = csv_record(text.as_bytes(), 0).unwrap();
        assert_eq!(fields, ["a", "b, \"c\"", "x\ny", ""]);
        assert_eq!(&text[next..], "next");
        assert_eq!(csv_record(text.as_bytes(), text.len()), None);
    }

    #[test]
    fn sniff_tells_the_formats_apart() {
        let csv = "2026-10-21 09:12:48.001 UTC,\"billing\",\"ledger\",31840,\"10.0.1.23:51234\",6530a1b2.7c60,3,\"SELECT\",2026-10-21 09:12:47 UTC,5/12,0,LOG,00000,\"statement: select 1\",,,,,,,,,\"psql\",\"client backend\",,0";
        assert_eq!(sniff(csv), Some(Format::Csv));
        assert_eq!(
            sniff("\n{\"pid\":1,\"error_severity\":\"LOG\",\"message\":\"statement: select 1\"}"),
            Some(Format::Json)
        );
        // stderr, plain SQL, other JSON, other CSV: none of them.
        for text in [
            "2026-10-21 09:12:48 UTC [31840] LOG:  statement: select 1",
            "select a, b, c from t",
            "{\"level\":\"info\",\"msg\":\"hello\"}",
            "a,b,c,d,e,f,g,h,i,j,k,l,m,n,o,p,q,r,s,t,u,v,w,x",
            "1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24",
            "a,b,c,d,e,f,g,h,i,j,k,LOG,m,n,o,p,q,r,s,t,u,v,w,x",
        ] {
            assert_eq!(sniff(text), None, "{text:?}");
        }
    }
}
