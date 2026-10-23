//! Reconstruct runnable SQL from JDBC proxy logs: p6spy and
//! datasource-proxy. Both wrap the `DataSource`, so they log what the
//! driver was really sent whatever the ORM, one line per execution.
//!
//! - **p6spy**, default `SingleLineFormat`:
//!   `now|elapsed|category|connection N|url …|prepared|sql` — `prepared`
//!   is the statement with `?`s, `sql` the same with the values p6spy
//!   substituted. The last field is already runnable.
//! - **datasource-proxy**, `DefaultQueryLogEntryCreator`:
//!   `… Query:["select … ?"], Params:[(2107,open)]`. A batch logs several
//!   queries (`Query:["q1","q2"]`) or several parameter sets
//!   (`Params:[(1),(2)]`); each becomes its own query. Values are logged
//!   untyped, so each is substituted as a quoted literal (`null` as NULL),
//!   as a Postgres log's are — Postgres casts it on re-run.
//!
//! Known limitations: a `|` in the SQL is told apart from p6spy's
//! separator only by the prepared SQL being a template of the effective
//! one; a datasource-proxy value containing `,` or a query containing
//! `","` is ambiguous in that format and is split there.

use crate::query::reconstruct::{BoundParam, ParamValue, ReconstructedQuery, Source};
use crate::query::subst::{self, PlaceholderStyle};

/// The p6spy categories that carry a statement.
const P6SPY_CATEGORIES: &[&str] = &["statement", "batch"];

/// Parse a p6spy or datasource-proxy log into reconstructed queries.
pub fn parse(log: &str) -> Vec<ReconstructedQuery> {
    let log = &*crate::query::logdetect::strip_sgr(log);
    let mut out = Vec::new();
    for (idx, line) in log.lines().enumerate() {
        let line_no = idx + 1;
        if let Some((prepared, sql)) = p6spy_line(line) {
            out.push(ReconstructedQuery {
                raw_sql: prepared.to_string(),
                params: Vec::new(),
                runnable_sql: sql.to_string(),
                source: Source::P6Spy,
                src_line: line_no,
                error: None,
            });
        } else if let Some((queries, param_sets)) = dsproxy_line(line) {
            for (raw_sql, params) in pair_batch(queries, param_sets) {
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
                    source: Source::DatasourceProxy,
                    src_line: line_no,
                    error: None,
                });
            }
        }
    }
    out
}

/// True if `line` is a p6spy or datasource-proxy statement line —
/// `logdetect`'s hint asks this, so it and [`parse`] agree.
pub(crate) fn is_p6spy_line(line: &str) -> bool {
    p6spy_line(line).is_some()
}

/// True if `line` is a datasource-proxy statement line.
pub(crate) fn is_dsproxy_line(line: &str) -> bool {
    dsproxy_line(line).is_some()
}

/// `(prepared, effective)` from a p6spy `SingleLineFormat` line whose
/// category carries a statement with SQL in it.
fn p6spy_line(line: &str) -> Option<(&str, &str)> {
    let mut fields = line.splitn(6, '|');
    let now = fields.next()?;
    let elapsed = fields.next()?;
    let category = fields.next()?;
    let connection = fields.next()?;
    let url = fields.next()?;
    let rest = fields.next()?;
    // `now` may follow a logger's own prefix (`… p6spy : 1729…`).
    let now_ok = now.trim_end().ends_with(|c: char| c.is_ascii_digit());
    let elapsed_ok =
        !elapsed.trim().is_empty() && elapsed.trim().bytes().all(|b| b.is_ascii_digit());
    if !now_ok
        || !elapsed_ok
        || !P6SPY_CATEGORIES.contains(&category.trim())
        || !connection.trim_start().starts_with("connection")
        || !url.trim_start().starts_with("url")
    {
        return None;
    }
    let (prepared, sql) = split_prepared(rest)?;
    let sql = sql.trim();
    (!sql.is_empty()).then(|| (prepared.trim(), sql))
}

/// Split p6spy's `prepared|effective` tail at the `|` between them. SQL
/// can hold a `|` of its own (`a || b`), so the split is the first one
/// where the effective side starts the way the prepared side does up to
/// its first `?` — the prepared statement is the effective one's template.
fn split_prepared(rest: &str) -> Option<(&str, &str)> {
    let candidates: Vec<usize> = rest.match_indices('|').map(|(i, _)| i).collect();
    candidates
        .iter()
        .find(|&&i| {
            let (prepared, sql) = (&rest[..i], &rest[i + 1..]);
            let head = prepared.split('?').next().unwrap_or_default().trim();
            sql.trim_start().starts_with(head)
        })
        .or(candidates.last())
        .map(|&i| (&rest[..i], &rest[i + 1..]))
}

/// `(queries, parameter sets)` from a datasource-proxy line.
fn dsproxy_line(line: &str) -> Option<(Vec<String>, Vec<Vec<BoundParam>>)> {
    let start = line.find("Query:[")? + "Query:[".len();
    let body = &line[start..];
    let (queries, rest) = match body.find("], Params:[") {
        Some(end) => (&body[..end], Some(&body[end + "], Params:[".len()..])),
        None => (body.strip_suffix(']').unwrap_or(body), None),
    };
    let queries: Vec<String> = queries
        .trim()
        .strip_prefix('"')?
        .strip_suffix('"')?
        .split("\",\"")
        .map(str::to_string)
        .filter(|q| !q.trim().is_empty())
        .collect();
    if queries.is_empty() {
        return None;
    }
    let params = rest
        .and_then(|r| r.trim_end().strip_suffix(']'))
        .map(parse_param_sets)
        .unwrap_or_default();
    Some((queries, params))
}

/// `(a,b),(c,d)` → one bound-parameter list per tuple.
fn parse_param_sets(s: &str) -> Vec<Vec<BoundParam>> {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
        return Vec::new();
    };
    inner
        .split("),(")
        .map(|tuple| {
            if tuple.is_empty() {
                return Vec::new();
            }
            tuple
                .split(',')
                .enumerate()
                .map(|(i, v)| BoundParam {
                    index: i + 1,
                    sql_type: "text".to_string(),
                    value: if v == "null" {
                        ParamValue::Null
                    } else {
                        ParamValue::Literal(v.to_string())
                    },
                })
                .collect()
        })
        .collect()
}

/// One query per (statement, parameter set): a prepared batch is one
/// statement with several sets, a statement batch several statements
/// with none, and a plain execution one of each.
fn pair_batch(
    queries: Vec<String>,
    param_sets: Vec<Vec<BoundParam>>,
) -> Vec<(String, Vec<BoundParam>)> {
    match (queries.len(), param_sets.len()) {
        (1, n) if n > 1 => param_sets
            .into_iter()
            .map(|p| (queries[0].clone(), p))
            .collect(),
        _ => {
            let mut sets = param_sets.into_iter();
            queries
                .into_iter()
                .map(|q| (q, sets.next().unwrap_or_default()))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runnable(log: &str) -> Vec<String> {
        parse(log).into_iter().map(|q| q.runnable_sql).collect()
    }

    #[test]
    fn p6spy_single_line_format_yields_the_effective_sql() {
        let log = "\
1729501968001|3|statement|connection 0|url jdbc:postgresql://db.example:5432/ledger|select i1_0.id,i1_0.total from invoices i1_0 where i1_0.customer_id=?|select i1_0.id,i1_0.total from invoices i1_0 where i1_0.customer_id=2107
1729501968002|0|commit|connection 0|url jdbc:postgresql://db.example:5432/ledger||
1729501968005|1|statement|connection 0|url jdbc:postgresql://db.example:5432/ledger|select l1_0.id from invoice_lines l1_0 where l1_0.invoice_id=?|select l1_0.id from invoice_lines l1_0 where l1_0.invoice_id=48202
2026-10-21 09:12:48.006 INFO 1 --- [exec-1] p6spy : 1729501968006|1|statement|connection 0|url jdbc:postgresql://db.example:5432/ledger|select l1_0.id from invoice_lines l1_0 where l1_0.invoice_id=?|select l1_0.id from invoice_lines l1_0 where l1_0.invoice_id=48204";
        let q = parse(log);
        assert_eq!(
            q.iter()
                .map(|q| q.runnable_sql.as_str())
                .collect::<Vec<_>>(),
            [
                "select i1_0.id,i1_0.total from invoices i1_0 where i1_0.customer_id=2107",
                "select l1_0.id from invoice_lines l1_0 where l1_0.invoice_id=48202",
                "select l1_0.id from invoice_lines l1_0 where l1_0.invoice_id=48204",
            ]
        );
        assert_eq!(q[1].src_line, 3);
        assert_eq!(q[0].source, Source::P6Spy);
        // The raw SQL is the template, so the loop clusters.
        let clusters = crate::query::nplus1::detect(&q);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].count, 2);
    }

    #[test]
    fn p6spy_sql_with_its_own_pipes_splits_at_the_separator() {
        let log = "1|0|statement|connection 1|url jdbc:postgresql://h/d|select a || b from t where c = ?|select a || b from t where c = 'x|y'";
        assert_eq!(runnable(log), ["select a || b from t where c = 'x|y'"]);
    }

    #[test]
    fn datasource_proxy_lines_substitute_their_params() {
        let log = "\
2026-10-21T09:12:48.001Z DEBUG 4242 --- [nio-8080-exec-1] n.t.d.l.l.SLF4JQueryLoggingListener      : Name:dataSource, Connection:1, Time:3, Success:True, Type:Prepared, Batch:False, QuerySize:1, BatchSize:0, Query:[\"select i1_0.id from invoices i1_0 where i1_0.customer_id=? and i1_0.status=? and i1_0.note=?\"], Params:[(2107,it's,null)]
2026-10-21T09:12:48.005Z DEBUG 4242 --- [nio-8080-exec-1] n.t.d.l.l.SLF4JQueryLoggingListener      : Name:dataSource, Connection:1, Time:1, Success:True, Type:Prepared, Batch:True, QuerySize:1, BatchSize:2, Query:[\"insert into audit (id) values (?)\"], Params:[(1),(2)]
2026-10-21T09:12:48.006Z DEBUG 4242 --- [nio-8080-exec-1] n.t.d.l.l.SLF4JQueryLoggingListener      : Name:dataSource, Connection:1, Time:1, Success:True, Type:Statement, Batch:True, QuerySize:2, BatchSize:2, Query:[\"delete from a\",\"delete from b\"], Params:[]
2026-10-21T09:12:48.007Z DEBUG 4242 --- [nio-8080-exec-1] n.t.d.l.l.SLF4JQueryLoggingListener      : Name:dataSource, Connection:1, Time:1, Success:True, Type:Statement, Batch:False, QuerySize:1, BatchSize:0, Query:[\"select 1 from dual\"], Params:[()]";
        let q = parse(log);
        assert_eq!(
            q.iter().map(|q| q.runnable_sql.as_str()).collect::<Vec<_>>(),
            [
                "select i1_0.id from invoices i1_0 where i1_0.customer_id='2107' and i1_0.status='it''s' and i1_0.note=NULL",
                "insert into audit (id) values ('1')",
                "insert into audit (id) values ('2')",
                "delete from a",
                "delete from b",
                "select 1 from dual",
            ]
        );
        assert!(q.iter().all(|q| q.source == Source::DatasourceProxy));
        assert_eq!(
            q.iter().map(|q| q.src_line).collect::<Vec<_>>(),
            [1, 2, 2, 3, 3, 4]
        );
    }

    #[test]
    fn other_lines_are_not_proxy_lines() {
        for line in [
            "select a | b from t",
            "2026-10-21 09:12:48 UTC [31840] LOG:  statement: select 1",
            "[main] org.hibernate.SQL : select 1",
            "a|b|c|d|e|f|g",
            "1|0|statement|connection 1|url x||",
            // A result set's values, not a statement run again.
            "1|0|resultset|connection 1|url x|select id from t|id = 42",
            "Query: select 1",
        ] {
            assert!(parse(line).is_empty(), "{line:?}");
        }
    }
}
