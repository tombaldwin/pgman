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

/// A scan reading at least this many rows is worth a finding.
const BIG_SCAN_ROWS: f64 = 10_000.0;
/// A row estimate this many times off, either way, is a finding…
const ESTIMATE_FACTOR: f64 = 10.0;
/// …when the larger side is at least this many rows.
const ESTIMATE_FLOOR: f64 = 1_000.0;
/// A nested loop's inner side run this many times is a finding.
const MANY_LOOPS: f64 = 1_000.0;

impl PlanNode {
    /// One of its extras, by the key Postgres gave it.
    fn extra(&self, key: &str) -> Option<&str> {
        self.extras
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The rows it produced in all its loops, when the plan was
    /// analyzed.
    fn actual_total_rows(&self) -> Option<f64> {
        Some(self.actual_rows? * self.actual_loops.unwrap_or(1.0))
    }

    /// `Seq Scan on orders`: its type and relation.
    fn label(&self) -> String {
        match &self.relation_name {
            Some(r) => format!("{} on {r}", self.node_type),
            None => self.node_type.clone(),
        }
    }
}

/// What in `root`'s plan is worth looking at, worded for someone about
/// to add an index or rewrite the query: big sequential scans, row
/// estimates far off what an `ANALYZE` run found, sorts that spilled to
/// disk, hashes split into batches, nested loops whose inner side ran
/// many times. Top-down, each once. Pure.
pub fn findings(root: &PlanNode) -> Vec<String> {
    let mut out = Vec::new();
    walk_findings(root, &mut out);
    out
}

fn walk_findings(n: &PlanNode, out: &mut Vec<String>) {
    // What a scan read: the rows it returned and the ones its filter
    // threw away (both per loop), or, unanalyzed, the estimate.
    let removed = n
        .extra("Rows Removed by Filter")
        .and_then(|r| r.parse::<f64>().ok())
        .unwrap_or(0.0);
    let read = n
        .actual_rows
        .map(|r| (r + removed) * n.actual_loops.unwrap_or(1.0))
        .or(n.plan_rows);
    if n.node_type == "Seq Scan" && read.is_some_and(|r| r >= BIG_SCAN_ROWS) {
        let filter = n
            .extra("Filter")
            .map(|f| format!(" filtered by {f}"))
            .unwrap_or_default();
        let kept = n
            .actual_total_rows()
            .filter(|_| removed > 0.0)
            .map(|k| format!(", keeping {}", k.round()))
            .unwrap_or_default();
        out.push(format!(
            "{} reads about {} rows{filter}{kept}: an index on the columns it filters or joins by \
             may serve it",
            n.label(),
            read.unwrap_or_default().round()
        ));
    }
    if let (Some(got), Some(est)) = (n.actual_rows, n.plan_rows) {
        let (hi, lo) = (got.max(est), got.min(est).max(1.0));
        if hi >= ESTIMATE_FLOOR && hi / lo >= ESTIMATE_FACTOR {
            let table = n
                .relation_name
                .as_deref()
                .map(|r| format!(" (ANALYZE {r})"))
                .unwrap_or_default();
            out.push(format!(
                "{} was estimated at {} rows a loop and returned {}: the planner's statistics may \
                 be stale{table}, or the columns correlated",
                n.label(),
                est.round(),
                got.round()
            ));
        }
    }
    if n.extra("Sort Method")
        .is_some_and(|m| m.contains("external"))
    {
        out.push(format!(
            "{} spilled to disk ({}, {} kB): more work_mem, or fewer rows to sort",
            n.label(),
            n.extra("Sort Method").unwrap_or_default(),
            n.extra("Sort Space Used").unwrap_or("?")
        ));
    }
    if n.extra("Hash Batches")
        .and_then(|b| b.parse::<f64>().ok())
        .is_some_and(|b| b > 1.0)
    {
        out.push(format!(
            "{} split its hash table into {} batches on disk: more work_mem, or a smaller side to \
             hash",
            n.label(),
            n.extra("Hash Batches").unwrap_or_default()
        ));
    }
    if n.node_type == "Nested Loop" {
        if let Some(inner) = n
            .children
            .get(1)
            .filter(|c| c.actual_loops.is_some_and(|l| l >= MANY_LOOPS))
        {
            out.push(format!(
                "Nested Loop runs its inner {} {} times: an index on its join key, or a hash join, \
                 may serve it",
                inner.label(),
                inner.actual_loops.unwrap_or_default().round()
            ));
        }
    }
    for c in &n.children {
        walk_findings(c, out);
    }
}

/// The columns a scan's `Filter` compares to something: `status` and
/// `customer_id` in `((status = 'open'::text) AND (customer_id = 5))`.
/// Only plain `column op …` comparisons, the ones an index on the column
/// serves; a function of a column, an `OR`, or anything this can't read
/// gives nothing. Pure.
pub fn filter_columns(filter: &str) -> Vec<String> {
    if filter.to_ascii_uppercase().contains(" OR ") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for part in filter.split(" AND ") {
        let part = part.trim().trim_start_matches('(');
        let mut word = String::new();
        let mut chars = part.chars().peekable();
        if part.starts_with('"') {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                word.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    word.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
        }
        let rest: String = chars.collect();
        let op = rest.trim_start();
        let compares = ["=", "<", ">", "<=", ">=", "<>", "~~", "IS ", "= ANY"]
            .iter()
            .any(|o| op.starts_with(o));
        let column = !word.is_empty() && !word.starts_with(|c: char| c.is_ascii_digit());
        if column && compares && !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

/// A relation's schema-qualified name and its index definitions
/// (`pg_get_indexdef`), by the name a plan gives it, when the catalog
/// has it.
pub type IndexesFor<'a> = &'a dyn Fn(&str) -> Option<(String, Vec<String>)>;

/// For each big sequential scan in `root` with a filter, the index that
/// could serve it when no index on its table leads with the filtered
/// columns: `CREATE INDEX CONCURRENTLY ON public.orders (status)`.
/// `indexes_for` gives a relation's schema-qualified name and its
/// index definitions (`pg_get_indexdef`), when the catalog has it. A
/// suggestion to weigh, not a migration. Pure.
pub fn index_suggestions(root: &PlanNode, indexes_for: IndexesFor) -> Vec<String> {
    let mut out = Vec::new();
    walk_suggestions(root, indexes_for, &mut out);
    out
}

fn walk_suggestions(n: &PlanNode, indexes_for: IndexesFor, out: &mut Vec<String>) {
    let removed = n
        .extra("Rows Removed by Filter")
        .and_then(|r| r.parse::<f64>().ok())
        .unwrap_or(0.0);
    let read = n
        .actual_rows
        .map(|r| (r + removed) * n.actual_loops.unwrap_or(1.0))
        .or(n.plan_rows);
    let big = read.is_some_and(|r| r >= BIG_SCAN_ROWS);
    if let (true, "Seq Scan", Some(rel), Some(filter)) = (
        big,
        n.node_type.as_str(),
        n.relation_name.as_deref(),
        n.extra("Filter"),
    ) {
        let cols = filter_columns(filter);
        if let (false, Some((table, defs))) = (cols.is_empty(), indexes_for(rel)) {
            // An index whose leading column is one the filter compares.
            let led = defs.iter().any(|d| {
                d.rsplit_once(" (")
                    .map(|(_, cols_part)| {
                        let first = cols_part
                            .trim_end_matches(')')
                            .split(',')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .trim_matches('"')
                            .split_whitespace()
                            .next()
                            .unwrap_or("")
                            .to_string();
                        cols.contains(&first)
                    })
                    .unwrap_or(false)
            });
            let suggestion = format!("CREATE INDEX CONCURRENTLY ON {table} ({})", cols.join(", "));
            if !led && !out.contains(&suggestion) {
                out.push(suggestion);
            }
        }
    }
    for c in &n.children {
        walk_suggestions(c, indexes_for, out);
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

    fn node(kind: &str, relation: Option<&str>) -> PlanNode {
        let mut n = PlanNode::default_with_type(kind);
        n.relation_name = relation.map(str::to_string);
        n
    }

    #[test]
    fn findings_name_what_is_worth_an_index_or_a_rewrite() {
        let mut scan = node("Seq Scan", Some("orders"));
        scan.plan_rows = Some(5.0);
        scan.actual_rows = Some(25_000.0);
        scan.actual_loops = Some(1.0);
        scan.extras
            .push(("Filter".into(), "(status = 'open'::text)".into()));
        let mut sort = node("Sort", None);
        sort.extras
            .push(("Sort Method".into(), "external merge".into()));
        sort.extras.push(("Sort Space Used".into(), "20480".into()));
        let mut hash = node("Hash", None);
        hash.extras.push(("Hash Batches".into(), "8".into()));
        let mut inner = node("Index Scan", Some("customers"));
        inner.actual_loops = Some(25_000.0);
        inner.actual_rows = Some(1.0);
        inner.plan_rows = Some(1.0);
        let mut nl = node("Nested Loop", None);
        nl.children = vec![scan, inner];
        sort.children = vec![nl, hash];
        let f = findings(&sort);
        let all = f.join("\n");
        for want in [
            "Seq Scan on orders reads about 25000 rows filtered by (status = 'open'::text)",
            "Seq Scan on orders was estimated at 5 rows a loop and returned 25000",
            "(ANALYZE orders)",
            "Sort spilled to disk (external merge, 20480 kB)",
            "Hash split its hash table into 8 batches",
            "Nested Loop runs its inner Index Scan on customers 25000 times",
        ] {
            assert!(all.contains(want), "{want:?} in\n{all}");
        }
        assert_eq!(f.len(), 5, "each once: {all}");
    }

    #[test]
    fn a_filters_plain_comparisons_name_its_columns() {
        assert_eq!(
            filter_columns("((status = 'open'::text) AND (customer_id = 5))"),
            ["status", "customer_id"]
        );
        assert_eq!(filter_columns("(created_at > now())"), ["created_at"]);
        assert_eq!(filter_columns("(\"Email\" ~~ 'a%'::text)"), ["Email"]);
        assert_eq!(filter_columns("(deleted_at IS NULL)"), ["deleted_at"]);
        // An OR, a function of a column, a constant first: nothing.
        assert!(filter_columns("((a = 1) OR (b = 2))").is_empty());
        assert!(filter_columns("(lower(email) = 'x'::text)").is_empty());
        assert!(filter_columns("(5 = id)").is_empty());
    }

    #[test]
    fn a_big_filtered_scan_with_no_leading_index_gets_one_suggested() {
        let mut scan = node("Seq Scan", Some("orders"));
        scan.plan_rows = Some(200.0);
        scan.actual_rows = Some(200.0);
        scan.actual_loops = Some(1.0);
        scan.extras
            .push(("Filter".into(), "(status = 'open'::text)".into()));
        scan.extras
            .push(("Rows Removed by Filter".into(), "49800".into()));
        let none = |_: &str| Some(("public.orders".to_string(), vec![]));
        assert_eq!(
            index_suggestions(&scan, &none),
            ["CREATE INDEX CONCURRENTLY ON public.orders (status)"]
        );
        // An index leading with the column: nothing to add.
        let led = |_: &str| {
            Some((
                "public.orders".to_string(),
                vec!["CREATE INDEX orders_s ON public.orders USING btree (status, id)".into()],
            ))
        };
        assert!(index_suggestions(&scan, &led).is_empty());
        // One where it isn't the leading column still leaves it unserved.
        let second = |_: &str| {
            Some((
                "public.orders".to_string(),
                vec!["CREATE INDEX orders_s ON public.orders USING btree (id, status)".into()],
            ))
        };
        assert_eq!(index_suggestions(&scan, &second).len(), 1);
        // A small scan, or a table the catalog doesn't know: nothing.
        let mut small = scan.clone();
        small.extras.retain(|(k, _)| k != "Rows Removed by Filter");
        assert!(index_suggestions(&small, &none).is_empty());
        assert!(index_suggestions(&scan, &|_| None).is_empty());
    }

    #[test]
    fn a_small_or_well_estimated_plan_has_nothing_to_say() {
        let mut scan = node("Seq Scan", Some("settings"));
        scan.plan_rows = Some(40.0);
        // Ten times off, but too few rows to matter.
        let mut small = node("Index Scan", Some("tags"));
        small.plan_rows = Some(10.0);
        small.actual_rows = Some(200.0);
        // Returns few, reads few: nothing removed.
        let mut close = node("Index Scan", Some("orders"));
        close.plan_rows = Some(900.0);
        close.actual_rows = Some(2_000.0);
        let mut nl = node("Nested Loop", None);
        let mut few = node("Index Scan", Some("customers"));
        few.actual_loops = Some(40.0);
        nl.children = vec![scan, few];
        let mut sort = node("Sort", None);
        sort.extras.push(("Sort Method".into(), "quicksort".into()));
        sort.children = vec![nl, close, small];
        assert!(findings(&sort).is_empty(), "{:?}", findings(&sort));
        // Unanalyzed, a big table's scan is still worth naming.
        let mut big = node("Seq Scan", Some("events"));
        big.plan_rows = Some(2_000_000.0);
        assert_eq!(findings(&big).len(), 1);
        // What a filter threw away was read too: 200 kept of 20000.
        let mut filtered = node("Seq Scan", Some("orders"));
        filtered.plan_rows = Some(200.0);
        filtered.actual_rows = Some(200.0);
        filtered.actual_loops = Some(1.0);
        filtered
            .extras
            .push(("Rows Removed by Filter".into(), "19800".into()));
        let f = findings(&filtered);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(
            f[0].contains("reads about 20000 rows") && f[0].contains("keeping 200"),
            "{f:?}"
        );
    }

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
