//! Parse PostgreSQL `EXPLAIN (FORMAT JSON)` output into a tree the
//! TUI can render. The parse path is pure (string → [`PlanNode`]);
//! [`run_cost_explain`] is the one I/O helper — it runs an EXPLAIN and
//! returns the top node's row estimate, kept here in the data layer so
//! the `Db` call stays out of the UI/app layer.
//!
//! The JSON shape Postgres emits is well-documented in the manual.
//! Each node carries:
//!
//! - `Node Type` — `"Seq Scan"`, `"Hash Join"`, etc.
//! - `Plans` (optional) — array of child nodes (subplans).
//! - Cost fields — `Total Cost`, `Plan Rows`, `Plan Width`.
//! - Timing fields (only with `ANALYZE`) — `Actual Total Time`,
//!   `Actual Rows`, `Actual Loops`.
//! - A pile of node-type-specific extras (Index Name, Hash Cond, …)
//!   surfaced under `extras` so the renderer can show them without
//!   us having to enumerate every field Postgres might emit.

use serde_json::Value;

/// One node in an EXPLAIN plan tree. Numeric / string fields are
/// pulled out for cheap access; everything else gets stuffed into
/// `extras` so the renderer can show node-type-specific details
/// (Index Cond, Hash Cond, etc.) without us having to enumerate
/// every Postgres node shape.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanNode {
    pub node_type: String,
    pub total_cost: Option<f64>,
    pub startup_cost: Option<f64>,
    pub plan_rows: Option<f64>,
    pub plan_width: Option<i64>,
    /// Only set when EXPLAIN was run with `ANALYZE`.
    pub actual_total_time: Option<f64>,
    pub actual_startup_time: Option<f64>,
    pub actual_rows: Option<f64>,
    pub actual_loops: Option<f64>,
    pub relation_name: Option<String>,
    pub alias: Option<String>,
    /// All other (string-valued) fields Postgres emitted for this
    /// node. Preserved in insertion order so the renderer surfaces
    /// them in the same order psql does.
    pub extras: Vec<(String, String)>,
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    /// "Hottest" subtree score for highlighting purposes:
    /// `actual_total_time` (ANALYZE) if available; falls back to
    /// `total_cost`. `None` for a node with neither.
    pub fn hot_score(&self) -> Option<f64> {
        self.actual_total_time.or(self.total_cost)
    }

    /// Recursively find the node with the highest `hot_score` in
    /// this tree. Returns `(score, node_path_indices)` where the
    /// path is the chain of child-array indices from the root.
    /// Empty path → root is the hottest.
    pub fn hottest(&self) -> (f64, Vec<usize>) {
        let mut best = (self.hot_score().unwrap_or(0.0), Vec::new());
        for (i, child) in self.children.iter().enumerate() {
            let (s, mut p) = child.hottest();
            if s > best.0 {
                p.insert(0, i);
                best = (s, p);
            }
        }
        best
    }
}

/// Parse the string Postgres emits for `EXPLAIN (FORMAT JSON) …`.
/// The outer shape is an array of objects each containing a `Plan`
/// key; we walk the first entry's `Plan` (there's only one in
/// practice — multi-statement EXPLAIN isn't reachable from our
/// run path).
pub fn parse(json: &str) -> Result<PlanNode, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|e| format!("EXPLAIN JSON parse failed: {e}"))?;
    let plans = root
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.get("Plan"))
        .ok_or_else(|| "EXPLAIN JSON missing [{ Plan: … }]".to_string())?;
    Ok(parse_node(plans))
}

fn parse_node(v: &Value) -> PlanNode {
    let obj = match v.as_object() {
        Some(o) => o,
        None => return PlanNode::default_with_type("(unknown)"),
    };
    let mut node = PlanNode {
        node_type: obj
            .get("Node Type")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)")
            .to_string(),
        total_cost: obj.get("Total Cost").and_then(Value::as_f64),
        startup_cost: obj.get("Startup Cost").and_then(Value::as_f64),
        plan_rows: obj.get("Plan Rows").and_then(Value::as_f64),
        plan_width: obj.get("Plan Width").and_then(Value::as_i64),
        actual_total_time: obj.get("Actual Total Time").and_then(Value::as_f64),
        actual_startup_time: obj.get("Actual Startup Time").and_then(Value::as_f64),
        actual_rows: obj.get("Actual Rows").and_then(Value::as_f64),
        actual_loops: obj.get("Actual Loops").and_then(Value::as_f64),
        relation_name: obj
            .get("Relation Name")
            .and_then(Value::as_str)
            .map(str::to_string),
        alias: obj.get("Alias").and_then(Value::as_str).map(str::to_string),
        extras: Vec::new(),
        children: Vec::new(),
    };
    // Capture remaining string-valued fields as extras, preserving
    // insertion order. Skip ones we already pulled out above and
    // skip `Plans` (handled below).
    let claimed: &[&str] = &[
        "Node Type",
        "Total Cost",
        "Startup Cost",
        "Plan Rows",
        "Plan Width",
        "Actual Total Time",
        "Actual Startup Time",
        "Actual Rows",
        "Actual Loops",
        "Relation Name",
        "Alias",
        "Plans",
    ];
    for (k, val) in obj {
        if claimed.contains(&k.as_str()) {
            continue;
        }
        // Render the value: strings come through as-is, everything
        // else gets `to_string`. Skip arrays / nested objects —
        // those don't render usefully in a single tree line.
        let rendered = match val {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".into(),
            _ => continue,
        };
        node.extras.push((k.clone(), rendered));
    }
    if let Some(arr) = obj.get("Plans").and_then(Value::as_array) {
        node.children = arr.iter().map(parse_node).collect();
    }
    node
}

impl PlanNode {
    fn default_with_type(name: &str) -> Self {
        Self {
            node_type: name.into(),
            total_cost: None,
            startup_cost: None,
            plan_rows: None,
            plan_width: None,
            actual_total_time: None,
            actual_startup_time: None,
            actual_rows: None,
            actual_loops: None,
            relation_name: None,
            alias: None,
            extras: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// Run `EXPLAIN (FORMAT JSON) …` against `client` and pluck the top
/// node's `Plan Rows` estimate. The one I/O helper in this module (the
/// parse path above is pure); it lives here, in the data layer, so the
/// `Db` call stays out of the UI/app layer.
pub async fn run_cost_explain(
    client: &tokio_postgres::Client,
    explain_sql: &str,
) -> Result<f64, String> {
    let row = client
        .query_one(explain_sql, &[])
        .await
        .map_err(|e| e.to_string())?;
    let json_str: String = row.try_get::<_, String>(0).map_err(|e| e.to_string())?;
    let parsed: serde_json::Value = serde_json::from_str(&json_str).map_err(|e| e.to_string())?;
    // EXPLAIN JSON output is an array with one entry per plan; we
    // care about the first plan's top node.
    let top = parsed
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.get("Plan"))
        .ok_or_else(|| "no Plan in EXPLAIN output".to_string())?;
    top.get("Plan Rows")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| "no Plan Rows on top node".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_seq_scan() -> &'static str {
        r#"[
          {
            "Plan": {
              "Node Type": "Seq Scan",
              "Relation Name": "users",
              "Alias": "u",
              "Total Cost": 22.50,
              "Startup Cost": 0.00,
              "Plan Rows": 1000,
              "Plan Width": 36,
              "Filter": "(active = true)"
            }
          }
        ]"#
    }

    fn sample_hash_join_with_analyze() -> &'static str {
        r#"[
          {
            "Plan": {
              "Node Type": "Hash Join",
              "Total Cost": 200.0,
              "Actual Total Time": 50.0,
              "Plan Rows": 5000,
              "Actual Rows": 4500,
              "Plans": [
                {
                  "Node Type": "Seq Scan",
                  "Relation Name": "orders",
                  "Total Cost": 100.0,
                  "Actual Total Time": 30.0,
                  "Plan Rows": 10000
                },
                {
                  "Node Type": "Hash",
                  "Total Cost": 22.5,
                  "Actual Total Time": 5.0,
                  "Plans": [
                    {
                      "Node Type": "Seq Scan",
                      "Relation Name": "users",
                      "Total Cost": 22.5,
                      "Actual Total Time": 4.0
                    }
                  ]
                }
              ]
            }
          }
        ]"#
    }

    #[test]
    fn parses_a_simple_seq_scan_plan() {
        let plan = parse(sample_seq_scan()).unwrap();
        assert_eq!(plan.node_type, "Seq Scan");
        assert_eq!(plan.relation_name.as_deref(), Some("users"));
        assert_eq!(plan.alias.as_deref(), Some("u"));
        assert_eq!(plan.total_cost, Some(22.50));
        assert_eq!(plan.plan_rows, Some(1000.0));
        assert!(plan.children.is_empty());
        // `Filter` lands in extras.
        assert!(plan
            .extras
            .iter()
            .any(|(k, v)| k == "Filter" && v == "(active = true)"));
    }

    #[test]
    fn parses_hash_join_with_subplans() {
        let plan = parse(sample_hash_join_with_analyze()).unwrap();
        assert_eq!(plan.node_type, "Hash Join");
        assert_eq!(plan.children.len(), 2);
        assert_eq!(plan.children[0].node_type, "Seq Scan");
        assert_eq!(plan.children[0].relation_name.as_deref(), Some("orders"));
        assert_eq!(plan.children[1].node_type, "Hash");
        // Nested grandchild.
        assert_eq!(plan.children[1].children.len(), 1);
        assert_eq!(plan.children[1].children[0].node_type, "Seq Scan");
    }

    #[test]
    fn hot_score_prefers_actual_time_over_cost() {
        let plan = parse(sample_hash_join_with_analyze()).unwrap();
        // ANALYZE plan → uses Actual Total Time.
        assert_eq!(plan.hot_score(), Some(50.0));
    }

    #[test]
    fn hot_score_falls_back_to_total_cost() {
        let plan = parse(sample_seq_scan()).unwrap();
        // No ANALYZE → cost.
        assert_eq!(plan.hot_score(), Some(22.50));
    }

    #[test]
    fn hottest_walks_subtrees_for_highest_node() {
        let plan = parse(sample_hash_join_with_analyze()).unwrap();
        // Root: 50.0; child[0] (Seq Scan): 30.0; child[1] (Hash):
        // 5.0; grandchild: 4.0. Root wins.
        let (score, path) = plan.hottest();
        assert_eq!(score, 50.0);
        assert!(path.is_empty());
    }

    #[test]
    fn hottest_picks_a_deep_child_when_it_dominates() {
        // Construct a plan where the hottest is a grandchild.
        let json = r#"[{
          "Plan": {
            "Node Type": "Hash Join",
            "Actual Total Time": 10.0,
            "Plans": [
              {
                "Node Type": "Seq Scan",
                "Actual Total Time": 5.0,
                "Plans": [
                  { "Node Type": "Index Scan", "Actual Total Time": 99.0 }
                ]
              }
            ]
          }
        }]"#;
        let plan = parse(json).unwrap();
        let (score, path) = plan.hottest();
        assert_eq!(score, 99.0);
        assert_eq!(path, vec![0, 0]);
    }

    #[test]
    fn parse_returns_useful_error_on_bad_json() {
        let err = parse("not json at all").unwrap_err();
        assert!(err.contains("parse failed"), "got: {err}");
    }

    #[test]
    fn parse_returns_useful_error_on_missing_plan_key() {
        let err = parse("[]").unwrap_err();
        assert!(err.contains("missing"), "got: {err}");
    }
}
