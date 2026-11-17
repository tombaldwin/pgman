//! `pgman execute <target> --sql SQL`: one INSERT, UPDATE or DELETE,
//! from the command line — MCP's `execute`, for the operator at a
//! terminal.
//!
//! The same two steps (`inventory::execute`): a read-only plan — the
//! statement checked against safety.toml, its rows counted with its own
//! `WHERE` and the current values an `UPDATE` overwrites read — then,
//! once confirmed, one run in one transaction, rolled back unless it
//! changed exactly the rows confirmed. As with `pgman apply`, nothing
//! safety.toml refuses is lifted here: a read-only database is made
//! writable in safety.toml. A protected target, and a statement that
//! changes every row, are confirmed by typing the name, which `--yes`
//! doesn't answer.

use crate::inventory::execute::{self, ExecPlan, Named};

pub const USAGE: &str = "\
usage: pgman execute <target> --sql SQL [--expect-rows N] [--yes]

<target> is ENV, ENV/DATABASE or ENV/SERVER/DATABASE in environments.toml,
a connection's name in connections.toml, or a postgres:// URL; its
primary is written to.

Run one INSERT, UPDATE or DELETE once: planned read-only first (an
UPDATE or DELETE is counted with its own WHERE, and an UPDATE's current
values of the columns it sets are shown), then run in one transaction
with a 5s lock timeout and a 30s statement timeout once you confirm,
and rolled back unless it changed exactly the rows confirmed.

  --sql SQL          the statement, with its values written in
  --expect-rows N    the rows it must change; needed when pgman can't
                     count them first (an INSERT), refused when the count
                     says otherwise
  --yes              don't ask; not for a protected target or a statement
                     that changes every row, which are confirmed by typing
                     the name

  pgman execute staging/ledger --sql \"UPDATE settings SET value = '10' WHERE id = 1\"";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    pub target: String,
    pub sql: String,
    pub expect_rows: Option<u64>,
    pub yes: bool,
}

/// Parse the arguments after `pgman execute`. Pure.
pub fn parse_cli(args: &[String]) -> Result<Opts, String> {
    let (mut target, mut sql, mut expect_rows, mut yes) = (None, None, None, false);
    let mut args = args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sql" if sql.is_some() => return Err(format!("--sql given twice\n\n{USAGE}")),
            "--sql" => {
                sql = Some(
                    args.next()
                        .ok_or_else(|| format!("--sql needs a statement\n\n{USAGE}"))?
                        .clone(),
                )
            }
            "--expect-rows" if expect_rows.is_some() => {
                return Err(format!("--expect-rows given twice\n\n{USAGE}"))
            }
            "--expect-rows" => {
                let n = args
                    .next()
                    .ok_or_else(|| format!("--expect-rows needs a number\n\n{USAGE}"))?;
                expect_rows = Some(n.parse::<u64>().map_err(|_| {
                    format!("--expect-rows takes a whole number, not {n:?}\n\n{USAGE}")
                })?);
            }
            "--yes" | "-y" => yes = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other:?}\n\n{USAGE}"))
            }
            other if target.is_none() => target = Some(other.to_string()),
            other => return Err(format!("unexpected argument {other:?}\n\n{USAGE}")),
        }
    }
    Ok(Opts {
        target: target.ok_or_else(|| format!("which database? name a target\n\n{USAGE}"))?,
        sql: sql.ok_or_else(|| format!("what to run? pass --sql\n\n{USAGE}"))?,
        expect_rows,
        yes,
    })
}

/// The plan as text, for the operator to read before confirming. Pure.
pub fn render_plan(plan: &ExecPlan) -> String {
    let protected = match (plan.protected, &plan.protected_why) {
        (true, Some(why)) => format!(", protected: {why}"),
        (true, None) => ", protected".to_string(),
        _ => String::new(),
    };
    let mut out = format!(
        "{} ({}{protected})\n\n  {};\n",
        plan.place(),
        plan.endpoint,
        crate::batch::terminal_safe(&plan.checked.statement)
    );
    match (plan.rows, &plan.uncounted) {
        (Some(n), _) => out.push_str(&format!(
            "\nIt changes {n} row(s) of the table it names (counted just now, read-only)."
        )),
        (None, why) => out.push_str(&format!(
            "\npgman couldn't count its rows first ({}){}; it must change {} (--expect-rows).",
            why.as_deref().unwrap_or("no count"),
            plan.estimate
                .map(|e| format!("; the planner estimates about {e}"))
                .unwrap_or_default(),
            plan.expect
        )),
    }
    out.push_str(&format!(
        "\nOne transaction, {} lock timeout, {}s statement timeout; rolled back unless exactly \
         {} row(s) change. Triggers, cascades and functions it calls run too, uncounted.",
        crate::query::execute::LOCK_TIMEOUT,
        crate::query::execute::STATEMENT_TIMEOUT_MS / 1000,
        plan.expect
    ));
    if !plan.checked.confirms.is_empty() {
        out.push_str(&format!(
            "\nYour guards would ask about it: {}.",
            plan.checked.confirms.join(", ")
        ));
    }
    if let Some(b) = &plan.before {
        out.push_str("\nWhat it overwrites now:");
        for row in &b.rows {
            let cells: Vec<String> = b
                .columns
                .iter()
                .zip(row)
                .map(|(c, v)| {
                    format!(
                        "{c} = {}",
                        v.as_deref()
                            .map(crate::batch::terminal_safe)
                            .unwrap_or_else(|| "NULL".into())
                    )
                })
                .collect();
            out.push_str(&format!("\n  {}", cells.join(", ")));
        }
        if b.more {
            out.push_str(&format!("\n  … (the first {} rows)", b.rows.len()));
        }
    }
    if let Some(e) = &plan.before_error {
        out.push_str(&format!("\n(its current values couldn't be read: {e})"));
    }
    out
}

/// The question for `plan`, and the answer that accepts. Pure.
pub fn question(plan: &ExecPlan) -> (String, String) {
    match plan.typed_reason() {
        Some(why) => (
            format!(
                "run it? {} {why}: type {} to go on: ",
                plan.confirm_name(),
                plan.confirm_name()
            ),
            plan.confirm_name().to_string(),
        ),
        None => ("run it? [y/N] ".to_string(), "y".to_string()),
    }
}

/// Plan, confirm and run. Returns what it did.
pub async fn run_cli(opts: &Opts) -> Result<String, String> {
    let safety = crate::project::merge_safety(
        crate::safety::SafetyConfig::load_personal().0,
        std::env::current_dir()
            .ok()
            .and_then(|cwd| crate::project::load_from(&cwd))
            .and_then(|(_, cfg)| cfg.safety)
            .as_ref(),
    );
    let plan = execute::plan_for(
        Named::Any(opts.target.clone()),
        opts.sql.clone(),
        opts.expect_rows,
        safety.clone(),
        None,
    )
    .await?;
    eprintln!("{}", render_plan(&plan));
    let typed = plan.typed_reason();
    if let Some(why) = typed.filter(|_| opts.yes) {
        return Err(format!(
            "{} {why}: it is confirmed by typing its name, which --yes doesn't answer",
            plan.confirm_name()
        ));
    }
    if typed.is_some() && !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(format!(
            "{} is confirmed by typing its name, and stdin isn't a terminal",
            plan.confirm_name()
        ));
    }
    let how = if opts.yes {
        "--yes"
    } else {
        let (prompt, accept) = question(&plan);
        let ok = tokio::task::spawn_blocking(move || crate::pgtool::confirm_cli(&prompt, &accept))
            .await
            .map_err(|e| format!("prompt task failed: {e}"))??;
        if !ok {
            execute::audit_not_confirmed(&plan, "the operator didn't confirm at the terminal");
            return Err("not confirmed; nothing run".to_string());
        }
        if typed.is_some() {
            "typed the name"
        } else {
            "answered y"
        }
    };
    execute::audit_confirmed(&plan, how, "pgman execute");
    let run = execute::run_for(&plan, safety).await;
    execute::audit_outcome(&plan, &run);
    let rows = run?;
    Ok(format!("{rows} row(s) changed, committed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_arguments_name_a_target_a_statement_and_maybe_a_count() {
        let o = parse_cli(&args(&[
            "demo/ledger",
            "--sql",
            "DELETE FROM t WHERE id = 1",
            "--expect-rows",
            "1",
            "-y",
        ]))
        .unwrap();
        assert_eq!(o.target, "demo/ledger");
        assert_eq!(o.sql, "DELETE FROM t WHERE id = 1");
        assert_eq!(o.expect_rows, Some(1));
        assert!(o.yes);
        for (bad, why) in [
            (vec!["demo"], "pass --sql"),
            (vec!["--sql", "x"], "name a target"),
            (vec!["demo", "--sql"], "needs a statement"),
            (vec!["demo", "--sql", "x", "--sql", "y"], "twice"),
            (
                vec!["demo", "--sql", "x", "--expect-rows", "-1"],
                "whole number",
            ),
            (vec!["demo", "--sql", "x", "--force"], "unknown option"),
            (vec!["demo", "other", "--sql", "x"], "unexpected argument"),
        ] {
            let e = parse_cli(&args(&bad)).unwrap_err();
            assert!(e.contains(why), "{bad:?}: {e}");
        }
    }

    fn plan(sql: &str, protected: bool) -> ExecPlan {
        let (cfg, _) =
            crate::safety::SafetyConfig::parse_personal("[databases.ledger]\nread_only = false\n");
        ExecPlan {
            named: Named::Any("app-dev".into()),
            environment: None,
            server: "app-dev".into(),
            database: "ledger".into(),
            label: "app-dev".into(),
            protected,
            protected_why: protected.then(|| "its name contains \"prod\"".to_string()),
            endpoint: "postgres://app@db1:5432/ledger".into(),
            checked: crate::query::execute::check(&cfg, "ledger", sql, false).unwrap(),
            rows: Some(1),
            uncounted: None,
            estimate: None,
            expect: 1,
            before: Some(crate::query::execute::Before {
                columns: vec!["value".into()],
                rows: vec![vec![Some("10".into())]],
                more: false,
            }),
            before_error: None,
        }
    }

    #[test]
    fn the_plan_shows_the_statement_its_rows_and_what_it_overwrites() {
        let text = render_plan(&plan(
            "UPDATE settings SET value = '11' WHERE id = 1",
            false,
        ));
        assert!(
            text.starts_with(
                "app-dev (ledger) (postgres://app@db1:5432/ledger)\n\n  UPDATE settings SET value = '11' WHERE id = 1;\n"
            ),
            "{text}"
        );
        assert!(text.contains("It changes 1 row(s)"), "{text}");
        assert!(
            text.contains("rolled back unless exactly 1 row(s) change"),
            "{text}"
        );
        assert!(
            text.contains("What it overwrites now:\n  value = 10"),
            "{text}"
        );
    }

    #[test]
    fn a_protected_target_is_confirmed_by_its_typed_name() {
        let p = plan("UPDATE settings SET value = '11' WHERE id = 1", true);
        assert!(render_plan(&p).contains("protected: its name contains \"prod\""));
        let (prompt, accept) = question(&p);
        assert_eq!(accept, "app-dev");
        assert!(
            prompt.contains("app-dev is protected: type app-dev"),
            "{prompt}"
        );
        let (prompt, accept) = question(&plan(
            "UPDATE settings SET value = '11' WHERE id = 1",
            false,
        ));
        assert_eq!((prompt.as_str(), accept.as_str()), ("run it? [y/N] ", "y"));
    }
}
