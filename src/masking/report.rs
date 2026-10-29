//! What `pgman masking show` and `pgman masking check` print. Pure.

use super::classify::{Origin, Plan, Rule};

/// Every column's rule and why, a table at a time: NULLed and masked
/// columns first, each with the rules-file line that changes it. Pure.
pub fn show(plan: &Plan, rules_file: Option<&str>) -> String {
    let mut out = vec![match rules_file {
        Some(f) => format!("rules: the defaults and {f}"),
        None => "rules: the defaults (no .pgman/masking/<database>.toml)".to_string(),
    }];
    for t in &plan.tables {
        out.push(String::new());
        let key = t
            .table
            .strip_prefix("public.")
            .map(str::to_string)
            .unwrap_or_else(|| format!("\"{}\"", t.table));
        if t.drop_rows {
            out.push(format!(
                "{}  no rows ({})",
                t.table,
                t.drop_why.as_deref().unwrap_or("left out")
            ));
            continue;
        }
        out.push(t.table.clone());
        let mut cols: Vec<_> = t.columns.iter().collect();
        cols.sort_by_key(|d| match d.rule {
            Rule::Null => 0,
            Rule::Keep => 2,
            _ => 1,
        });
        let width = cols.iter().map(|d| d.column.len()).max().unwrap_or(0);
        for d in cols {
            let why = match &d.origin {
                Origin::Default(w) => w.clone(),
                Origin::Preset(w) => format!("the preset for {w}"),
                Origin::Rules => "the rules file".to_string(),
                Origin::Follows(c) => format!("references {c}"),
                Origin::Plain => "nothing personal about it".to_string(),
            };
            out.push(format!(
                "  {:<width$}  {:<10}  {why}",
                d.column,
                d.rule.name()
            ));
            if d.rule == Rule::Null && matches!(d.origin, Origin::Default(_)) {
                out.push(format!(
                    "  {:<width$}  keep it:   [tables.{key}] {} = {{ rule = \"keep\", reason = \"…\" }}",
                    "", d.column
                ));
            }
        }
    }
    if !plan.problems.is_empty() {
        out.push(String::new());
        out.push(format!("{} problem(s):", plan.problems.len()));
        for p in &plan.problems {
            out.push(format!("  {p}"));
        }
    }
    out.join("\n")
}

/// `pgman masking check`: the problems, and with `strict` every column
/// masked by the defaults alone — a team that wants each one decided in
/// the rules file. What to print, and whether it passed. Pure.
pub fn check(plan: &Plan, strict: bool) -> (String, bool) {
    let mut problems = plan.problems.clone();
    if strict {
        for t in plan.tables.iter().filter(|t| !t.drop_rows) {
            for d in &t.columns {
                if d.rule != Rule::Keep && matches!(d.origin, Origin::Default(_)) {
                    problems.push(format!(
                        "{}.{}: {} by default ({}); --strict wants it in the rules file",
                        t.table,
                        d.column,
                        d.rule.name(),
                        match &d.origin {
                            Origin::Default(w) => w.as_str(),
                            _ => "",
                        }
                    ));
                }
            }
        }
    }
    let columns: usize = plan.tables.iter().map(|t| t.columns.len()).sum();
    if problems.is_empty() {
        (
            format!(
                "masking plan OK: {} tables, {columns} columns",
                plan.tables.len()
            ),
            true,
        )
    } else {
        (
            format!(
                "{} problem(s) in the masking plan:\n  {}",
                problems.len(),
                problems.join("\n  ")
            ),
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::classify::{Decision, TablePlan};
    use super::*;

    fn plan() -> Plan {
        let d = |c: &str, rule: Rule, origin: Origin| Decision {
            column: c.into(),
            rule,
            origin,
            fit: None,
            not_null: false,
            unique: false,
            base: "text".into(),
            enum_first: None,
            array: false,
        };
        Plan {
            tables: vec![TablePlan {
                table: "public.customers".into(),
                drop_rows: false,
                drop_why: None,
                columns: vec![
                    d("id", Rule::Keep, Origin::Plain),
                    d("email", Rule::Email, Origin::Rules),
                    d(
                        "notes",
                        Rule::Null,
                        Origin::Default("free text, by name".into()),
                    ),
                ],
                primary_key: vec![],
            }],
            large_objects: false,
            problems: vec![],
        }
    }

    #[test]
    fn show_puts_the_nulled_first_with_the_line_that_keeps_them() {
        let s = show(&plan(), None);
        let notes = s.find("notes").unwrap();
        assert!(
            notes < s.find("email").unwrap() && notes < s.find("  id").unwrap(),
            "{s}"
        );
        assert!(
            s.contains("[tables.customers] notes = { rule = \"keep\", reason = \"…\" }"),
            "{s}"
        );
        assert!(!s.contains("email = { rule"), "the rules file's own: {s}");
    }

    #[test]
    fn check_passes_a_plan_and_strict_wants_every_default_decided() {
        assert!(check(&plan(), false).1);
        let (text, ok) = check(&plan(), true);
        assert!(!ok);
        assert!(
            text.contains("public.customers.notes: null by default"),
            "{text}"
        );
        let mut bad = plan();
        bad.problems.push("x can't".into());
        assert!(!check(&bad, false).1);
    }
}
