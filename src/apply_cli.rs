//! `pgman apply <target>`: an environment database's pending patches,
//! from the command line — the environments view's `P`, for scripting.
//!
//! The same two steps (`inventory::patching`): a read-only plan of the
//! pending patches, each checked against safety.toml, then — once the
//! operator confirms it — each in its own transaction, re-reading the
//! version table before each and stopping at the first failure.
//! `--outside` is the view's `o`: a patch that can't run in a
//! transaction (`VACUUM`, `CREATE INDEX CONCURRENTLY`) is planned too
//! and runs statement by statement, each committing as it goes. Such a
//! plan, like one for a protected environment, is confirmed by typing
//! the environment's name, which `--yes` doesn't answer.

use crate::inventory::patching::{self, PatchPlan, RunMode};
use crate::inventory::target::{self, Want};

pub const USAGE: &str = "\
usage: pgman apply <target> [--outside] [--dry-run | --copy] [--yes]

<target> is ENV, ENV/DATABASE or ENV/SERVER/DATABASE in environments.toml.

Apply an environment database's pending patches (its version_table's
patches directory, in environments.toml), as the environments view's P
does: planned read-only and checked against safety.toml, then each
patch in its own transaction once you confirm, stopping at the first
failure.

  --outside   also plan a patch that can't run in a transaction (VACUUM,
              CREATE INDEX CONCURRENTLY…): its statements then run one by
              one, each committing as it runs, so a failure part-way
              leaves the earlier ones applied
  --dry-run   run them all in one transaction and roll it back
  --copy      apply them to a copy of the database on the same server
              (CREATE DATABASE … TEMPLATE), then drop the copy
  --yes       don't ask; not for a protected environment or --outside,
              which are confirmed by typing the environment's name

  pgman apply staging/ledger --dry-run";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    pub target: String,
    pub outside: bool,
    pub mode: RunMode,
    pub yes: bool,
}

/// Parse the arguments after `pgman apply`. Pure.
pub fn parse_cli(args: &[String]) -> Result<Opts, String> {
    let mut target = None;
    let (mut outside, mut yes) = (false, false);
    let mut mode = RunMode::Apply;
    for a in args {
        match a.as_str() {
            "--outside" => outside = true,
            "--yes" | "-y" => yes = true,
            "--dry-run" | "--copy" if mode != RunMode::Apply => {
                return Err(format!(
                    "--dry-run and --copy are one or the other\n\n{USAGE}"
                ))
            }
            "--dry-run" => mode = RunMode::DryRun,
            "--copy" => mode = RunMode::Copy,
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other:?}\n\n{USAGE}"))
            }
            other if target.is_none() => target = Some(other.to_string()),
            other => return Err(format!("unexpected argument {other:?}\n\n{USAGE}")),
        }
    }
    Ok(Opts {
        target: target.ok_or_else(|| format!("which database? name a target\n\n{USAGE}"))?,
        outside,
        mode,
        yes,
    })
}

/// The plan as text, for the operator to read before confirming. Pure.
pub fn render_plan(plan: &PatchPlan) -> String {
    let recorded = plan
        .applied_max
        .map(|v| format!(", recorded up to {v}"))
        .unwrap_or_default();
    let mut out = format!(
        "{} {}/{}{recorded}: {} pending patch(es) from {}",
        plan.environment,
        plan.server,
        plan.database,
        plan.patches.len(),
        plan.dir
    );
    for p in &plan.patches {
        out.push_str(&format!(
            "\n  {} — {} statement(s)",
            p.file.file,
            p.statements.len()
        ));
        if let Some(why) = p.outside {
            out.push_str(&format!(
                ", OUTSIDE a transaction ({why}): each statement commits as it runs"
            ));
        }
        let kinds = p.confirm_kinds();
        if !kinds.is_empty() {
            out.push_str(&format!("; includes {}", kinds.join(", ")));
        }
    }
    if !plan.below_recorded.is_empty() {
        out.push_str(&format!(
            "\n  left out, numbered below what is recorded: {}",
            plan.below_recorded.join(", ")
        ));
    }
    if let Some((file, why)) = &plan.stopped_at {
        out.push_str(&format!("\n  stops before {file}: {why}"));
        if plan.outside_would_help {
            out.push_str(" (--outside plans it)");
        }
    }
    out
}

/// The question for `plan` in `mode`, and the answer that accepts. Pure.
pub fn question(plan: &PatchPlan, mode: RunMode) -> (String, String) {
    let how = match mode {
        RunMode::Apply => "apply them",
        RunMode::DryRun => "run them in one transaction and roll it back",
        RunMode::Copy => "apply them to a copy of the database, then drop it",
    };
    if plan.needs_typed() {
        let why = if plan.protected {
            "is protected"
        } else {
            "runs a patch outside a transaction"
        };
        (
            format!(
                "{how}? {} {why}: type {} to go on: ",
                plan.environment, plan.environment
            ),
            plan.environment.clone(),
        )
    } else {
        (format!("{how}? [y/N] "), "y".to_string())
    }
}

/// The report as text. Pure.
pub fn render_report(report: &patching::ApplyReport) -> String {
    let mut out = report.summary();
    for r in &report.results {
        match &r.outcome {
            Ok(n) => out.push_str(&format!("\n  {} — {n} statement(s)", r.file)),
            Err(e) => out.push_str(&format!("\n  {} — FAILED: {e}", r.file)),
        }
    }
    out
}

/// Plan, confirm and apply. Returns the report and whether it all went
/// through.
pub async fn run_cli(opts: &Opts) -> Result<(String, bool), String> {
    let t = opts.target.clone();
    let resolved = tokio::task::spawn_blocking(move || target::resolve(&t, Want::Primary))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))??;
    let environment = resolved.environment.clone().ok_or_else(|| {
        format!(
            "{} isn't an environment's database: pgman apply works on environments.toml's, \
             with a version_table naming the patches",
            resolved.label
        )
    })?;
    let safety = crate::project::merge_safety(
        crate::safety::SafetyConfig::load_personal().0,
        std::env::current_dir()
            .ok()
            .and_then(|cwd| crate::project::load_from(&cwd))
            .and_then(|(_, cfg)| cfg.safety)
            .as_ref(),
    );
    let plan = patching::plan_for(
        environment,
        resolved.server.clone(),
        resolved.database.clone(),
        opts.outside,
        safety.clone(),
    )
    .await?;
    eprintln!("{}", render_plan(&plan));
    if plan.patches.is_empty() {
        return Ok(("nothing to apply".to_string(), plan.stopped_at.is_none()));
    }
    if plan.needs_typed() && opts.yes {
        return Err(format!(
            "{} {}: it is confirmed by typing its name, which --yes doesn't answer",
            plan.environment,
            if plan.protected {
                "is protected"
            } else {
                "runs a patch outside a transaction"
            }
        ));
    }
    if plan.needs_typed() && !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(format!(
            "{} is confirmed by typing its name, and stdin isn't a terminal",
            plan.environment
        ));
    }
    if !opts.yes {
        let (prompt, accept) = question(&plan, opts.mode);
        let ok = tokio::task::spawn_blocking(move || crate::pgtool::confirm_cli(&prompt, &accept))
            .await
            .map_err(|e| format!("prompt task failed: {e}"))??;
        if !ok {
            return Err("not confirmed; nothing applied".to_string());
        }
    }
    let cancel = patching::CancelSlot::default();
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            crate::pgtool::interrupted().await;
            cancel.cancel();
            // A second one doesn't wait for the cancel to land.
            crate::pgtool::interrupted().await;
            eprintln!(
                "pgman apply: stopped; a patch in its transaction is rolled back by the server, \
                 and statements already run outside one stay"
            );
            std::process::exit(130);
        });
    }
    tracing::info!(target: "pgman::apply", environment = %plan.environment, server = %plan.server, database = %plan.database, patches = plan.patches.len(), mode = opts.mode.name(), "pgman apply confirmed");
    let report = patching::apply_for(plan.clone(), opts.mode, cancel, safety).await;
    let ok = report.error.is_none() && report.failed().is_none() && plan.stopped_at.is_none();
    Ok((render_report(&report), ok))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn a_target_and_its_mode_parse() {
        let o = parse_cli(&args(&["staging/ledger", "--outside", "--dry-run"])).unwrap();
        assert_eq!(o.target, "staging/ledger");
        assert!(o.outside && !o.yes);
        assert_eq!(o.mode, RunMode::DryRun);
        assert_eq!(
            parse_cli(&args(&["x", "--copy"])).unwrap().mode,
            RunMode::Copy
        );
        assert!(parse_cli(&args(&["x", "--copy", "--dry-run"]))
            .unwrap_err()
            .contains("one or the other"));
        assert!(parse_cli(&[]).unwrap_err().contains("name a target"));
        assert!(parse_cli(&args(&["x", "y"]))
            .unwrap_err()
            .contains("unexpected"));
    }

    fn plan(protected: bool, outside: Option<&'static str>) -> PatchPlan {
        PatchPlan {
            environment: "staging".into(),
            server: "ledger-db".into(),
            database: "ledger".into(),
            protected,
            applied_max: Some(41),
            patches: vec![patching::PlannedPatch {
                file: crate::query::patches::PatchFile {
                    file: "042-index.sql".into(),
                    filename_number: Some(42),
                    version_id: Some(42),
                },
                text: String::new(),
                statements: vec!["x".into(), "y".into()],
                outside,
                confirms: vec!["TRUNCATE".into()],
            }],
            stopped_at: Some(("043-x.sql".into(), "uses VACUUM".into())),
            dir: "/patches".into(),
            recorded: vec![41],
            below_recorded: vec!["007-old.sql".into()],
            outside_transaction: outside.is_some(),
            outside_would_help: true,
        }
    }

    #[test]
    fn the_plan_reads_as_what_will_run() {
        let r = render_plan(&plan(false, Some("CONCURRENTLY")));
        assert!(
            r.starts_with(
                "staging ledger-db/ledger, recorded up to 41: 1 pending patch(es) from /patches"
            ),
            "{r}"
        );
        assert!(
            r.contains("042-index.sql — 2 statement(s), OUTSIDE a transaction (CONCURRENTLY)"),
            "{r}"
        );
        assert!(r.contains("includes TRUNCATE"), "{r}");
        assert!(
            r.contains("left out, numbered below what is recorded: 007-old.sql"),
            "{r}"
        );
        assert!(
            r.contains("stops before 043-x.sql: uses VACUUM (--outside plans it)"),
            "{r}"
        );
    }

    #[test]
    fn protected_or_outside_wants_the_name_and_the_rest_wants_yes() {
        let (q, a) = question(&plan(true, None), RunMode::Apply);
        assert!(
            q.contains("staging is protected: type staging") && a == "staging",
            "{q}"
        );
        let (q, a) = question(&plan(false, Some("VACUUM")), RunMode::Apply);
        assert!(
            q.contains("runs a patch outside a transaction") && a == "staging",
            "{q}"
        );
        let (q, a) = question(&plan(false, None), RunMode::DryRun);
        assert!(
            q.starts_with("run them in one transaction and roll it back? [y/N]") && a == "y",
            "{q}"
        );
    }
}
