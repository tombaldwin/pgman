//! MCP's `slow_queries`: the statements `pg_stat_statements` says cost
//! the most, as the TUI's slow-query panel (`T`) reads them, for an
//! agent to map back to the code that sends them.
//!
//! [`parse_args`] and [`render_slow_queries`] are pure.

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

#[cfg(test)]
mod tests {
    use super::*;

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
