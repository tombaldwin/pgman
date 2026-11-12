//! MCP's performance tools. `slow_queries`: the statements
//! `pg_stat_statements` says cost the most, as the TUI's slow-query
//! panel (`T`) reads them, for an agent to map back to the code that
//! sends them. `explain`: one statement's plan, with what in it is
//! worth an index or a rewrite (`query::explain::findings`).
//!
//! [`parse_args`], [`render_slow_queries`], [`explain_sql`] and
//! [`render_explain`] are pure.

use crate::query::slow_queries::SlowQueryRow;
use serde_json::{json, Value};

/// Rows returned when the call doesn't say.
pub const DEFAULT_LIMIT: u32 = 20;
/// The most rows a call returns (the TUI panel's).
pub const MAX_LIMIT: u32 = 50;
/// A statement's text past this many characters comes back cut.
pub const QUERY_CHARS: usize = 2000;

/// `order_by` (`total`, the default, or `mean`) and `limit` (1 to
/// [`MAX_LIMIT`], default [`DEFAULT_LIMIT`]): whether to rank by mean,
/// and how many. Pure.
pub fn parse_args(args: &Value) -> Result<(bool, u32), String> {
    let by_mean = match args.get("order_by") {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) if s == "total" => false,
        Some(Value::String(s)) if s == "mean" => true,
        Some(other) => {
            return Err(format!(
                "order_by must be \"total\" or \"mean\", not {other}"
            ))
        }
    };
    let limit = match args.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(v) => match v.as_u64() {
            Some(n) if (1..=u64::from(MAX_LIMIT)).contains(&n) => n as u32,
            _ => return Err(format!("limit must be 1 to {MAX_LIMIT}, not {v}")),
        },
    };
    Ok((by_mean, limit))
}

/// `slow_queries`' result. Pure.
pub fn render_slow_queries(name: &str, by_mean: bool, rows: &[SlowQueryRow]) -> String {
    let mut cut = 0usize;
    let statements: Vec<Value> = rows
        .iter()
        .map(|r| {
            let query = if r.query.chars().count() > QUERY_CHARS {
                cut += 1;
                format!("{}…", r.query.chars().take(QUERY_CHARS).collect::<String>())
            } else {
                r.query.clone()
            };
            json!({
                "query": query,
                "calls": r.calls,
                "total_ms": r.total_ms,
                "mean_ms": r.mean_ms,
                "rows": r.rows,
            })
        })
        .collect();
    json!({
        "connection": name,
        "order_by": if by_mean { "mean" } else { "total" },
        "statements": statements,
        "queries_cut": cut,
        "note": "pg_stat_statements' figures since its last reset (or the server's last restart), \
                 with literals replaced by $1, $2…; a replica's are its own reads, not the primary's",
    })
    .to_string()
}

/// The most plan JSON `explain` returns; past it, only the findings.
pub const MAX_PLAN_BYTES: usize = 256 * 1024;

/// `statement` (one, already through the guards) wrapped for a JSON
/// plan, with `ANALYZE, BUFFERS` when `analyze`. A statement that is
/// itself an EXPLAIN is refused: it would be wrapped twice. Pure.
pub fn explain_sql(statement: &str, analyze: bool) -> Result<String, String> {
    let first = statement
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    if first.eq_ignore_ascii_case("explain") {
        return Err(
            "give the statement itself, without EXPLAIN: explain wraps it (analyze: true for \
             ANALYZE)"
                .to_string(),
        );
    }
    let opts = if analyze {
        "ANALYZE, BUFFERS, FORMAT JSON"
    } else {
        "FORMAT JSON"
    };
    Ok(format!("EXPLAIN ({opts}) {statement}"))
}

/// `explain`'s result from the plan JSON Postgres returned: the plan
/// (up to [`MAX_PLAN_BYTES`]), its findings, and with `analyze` the
/// planning and execution times. Pure.
pub fn render_explain(name: &str, analyzed: bool, plan_json: &str) -> Result<String, String> {
    let root = crate::query::explain::parse(plan_json)?;
    let top: Value = serde_json::from_str(plan_json)
        .ok()
        .and_then(|v: Value| v.as_array().and_then(|a| a.first()).cloned())
        .unwrap_or(Value::Null);
    let plan = top.get("Plan").cloned().unwrap_or(Value::Null);
    let cut = plan.to_string().len() > MAX_PLAN_BYTES;
    Ok(json!({
        "connection": name,
        "analyzed": analyzed,
        "findings": crate::query::explain::findings(&root),
        "planning_ms": top.get("Planning Time"),
        "execution_ms": top.get("Execution Time"),
        "plan": if cut { Value::Null } else { plan },
        "plan_cut": cut,
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statement_is_wrapped_once_for_a_json_plan() {
        assert_eq!(
            explain_sql("SELECT 1", false).unwrap(),
            "EXPLAIN (FORMAT JSON) SELECT 1"
        );
        assert_eq!(
            explain_sql("SELECT 1", true).unwrap(),
            "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) SELECT 1"
        );
        for twice in ["EXPLAIN SELECT 1", "  explain(analyze) SELECT 1"] {
            assert!(explain_sql(twice, false).is_err(), "{twice}");
        }
        assert!(explain_sql("SELECT explained FROM t", false).is_ok());
    }

    #[test]
    fn a_plan_comes_back_with_its_findings_and_times() {
        let plan = r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "orders",
            "Plan Rows": 50000, "Actual Rows": 48000, "Actual Loops": 1,
            "Total Cost": 900.5},
            "Planning Time": 0.2, "Execution Time": 41.7}]"#;
        let v: Value = serde_json::from_str(&render_explain("app", true, plan).unwrap()).unwrap();
        assert_eq!(v["analyzed"], true);
        assert_eq!(v["plan"]["Node Type"], "Seq Scan");
        assert_eq!(v["execution_ms"], 41.7);
        assert!(
            v["findings"][0]
                .as_str()
                .unwrap()
                .starts_with("Seq Scan on orders reads about 48000 rows"),
            "{v}"
        );
        assert_eq!(v["plan_cut"], false);
        assert!(render_explain("app", false, "not json").is_err());
        let huge = format!(
            r#"[{{"Plan": {{"Node Type": "Result", "Output": ["{}"]}}}}]"#,
            "x".repeat(MAX_PLAN_BYTES)
        );
        let v: Value = serde_json::from_str(&render_explain("app", false, &huge).unwrap()).unwrap();
        assert_eq!(v["plan_cut"], true);
        assert_eq!(v["plan"], Value::Null);
    }

    fn row(query: &str, total: f64) -> SlowQueryRow {
        SlowQueryRow {
            query: query.into(),
            calls: 10,
            total_ms: total,
            mean_ms: total / 10.0,
            rows: 3,
        }
    }

    #[test]
    fn the_arguments_default_and_refuse_what_is_out_of_range() {
        assert_eq!(parse_args(&json!({})), Ok((false, DEFAULT_LIMIT)));
        assert_eq!(
            parse_args(&json!({"order_by": "mean", "limit": 5})),
            Ok((true, 5))
        );
        assert_eq!(parse_args(&json!({"limit": 50})), Ok((false, 50)));
        for bad in [
            json!({"order_by": "calls"}),
            json!({"limit": 0}),
            json!({"limit": 51}),
            json!({"limit": "5"}),
        ] {
            assert!(parse_args(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn statements_come_back_with_their_figures_and_long_text_cut() {
        let long = "SELECT ".to_string() + &"x, ".repeat(1000);
        let v: Value = serde_json::from_str(&render_slow_queries(
            "app",
            true,
            &[
                row("SELECT * FROM orders WHERE id = $1", 500.0),
                row(&long, 20.0),
            ],
        ))
        .unwrap();
        assert_eq!(v["order_by"], "mean");
        assert_eq!(
            v["statements"][0]["query"],
            "SELECT * FROM orders WHERE id = $1"
        );
        assert_eq!(v["statements"][0]["mean_ms"], 50.0);
        let cut = v["statements"][1]["query"].as_str().unwrap();
        assert_eq!(cut.chars().count(), QUERY_CHARS + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(v["queries_cut"], 1);
    }
}
