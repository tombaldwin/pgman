//! What a dump of an environment may be, and what it takes: the
//! operator's `environments.toml` fields (`dump`, `dump_ttl`,
//! `dump_reason`, `unattended`), a project's cap from
//! `.pgman/pgman.toml`, and whether the target is protected — merged
//! into one [`Policy`], then checked against what was asked for
//! ([`gate`]). See `docs/design/masked-dumps.md`, sections 1 and 10.
//!
//! A protected environment's default is `masked`; a raw dump of one
//! needs `dump = "unmasked"` in the operator's own file and the
//! environment's name typed. Pure; [`for_target`] is the thin reader.

use serde::Deserialize;

/// `dump = "…"`: what a dump of the environment may be. Ordered from
/// most to least restrictive, so a project's cap is the minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DumpMode {
    Never,
    Schema,
    Masked,
    Unmasked,
}

impl DumpMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DumpMode::Never => "never",
            DumpMode::Schema => "schema",
            DumpMode::Masked => "masked",
            DumpMode::Unmasked => "unmasked",
        }
    }
}

/// `dump_reason = "…"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasonRule {
    Required,
    Optional,
}

/// The fields an environment sets, as written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvSettings {
    pub dump: Option<DumpMode>,
    pub ttl: Option<String>,
    pub reason: Option<ReasonRule>,
    pub unattended: bool,
}

/// A dump's effective policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub protected: bool,
    pub dump: DumpMode,
    /// `dump` is the operator's setting, not the default.
    pub dump_set: bool,
    /// Which file lowered `dump`, when a project's cap did.
    pub capped_by: Option<String>,
    pub ttl_secs: u64,
    pub reason: ReasonRule,
    /// Raw dumps of this environment may run without a terminal: only
    /// ever from the operator's own file.
    pub unattended: bool,
}

/// What was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Every row, as it is.
    Raw,
    /// Every row, what looks personal replaced ([`crate::masked`]).
    Masked,
    /// The schema alone.
    Schema,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Raw => "raw",
            Kind::Masked => "masked",
            Kind::Schema => "schema",
        }
    }
}

/// What a dump is when nothing asks: what the policy allows, the most
/// of it — but a protected environment's is masked even where a raw copy
/// is allowed: that takes `--unmasked`. Pure.
pub fn default_kind(policy: &Policy) -> Kind {
    match policy.dump {
        DumpMode::Unmasked if policy.protected => Kind::Masked,
        DumpMode::Unmasked | DumpMode::Never => Kind::Raw,
        DumpMode::Masked => Kind::Masked,
        DumpMode::Schema => Kind::Schema,
    }
}

/// Who is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The command line, with a terminal to ask on.
    Terminal,
    /// The command line with no terminal: cron, CI, a script, an agent's
    /// shell.
    NoTerminal,
    /// The MCP server: the operator answers an elicitation.
    Mcp,
}

/// What a dump needs before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// The name the operator types, for a raw dump of a protected
    /// target at a terminal.
    pub typed: Option<String>,
    /// Ask for (or require) a reason.
    pub reason: bool,
    /// It runs unattended, as the operator's `unattended = true` allows.
    pub unattended: bool,
}

pub const PROTECTED_TTL: u64 = 14 * 86_400;
pub const UNPROTECTED_TTL: u64 = 30 * 86_400;

/// `14d`, `36h`, `2w` in seconds. Pure.
pub fn parse_ttl(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let bad = || format!("dump_ttl {s:?}: give a number of hours, days or weeks: 36h, 14d, 2w");
    let unit = s.chars().last().ok_or_else(bad)?;
    let per = match unit {
        'h' => 3_600,
        'd' => 86_400,
        'w' => 7 * 86_400,
        _ => return Err(bad()),
    };
    let n: u64 = s[..s.len() - 1].parse().map_err(|_| bad())?;
    if n == 0 {
        return Err(bad());
    }
    n.checked_mul(per).ok_or_else(bad)
}

/// The policy for a target, from its protection, its environment's
/// settings (`None` for a connection or URL outside any environment) and
/// a project's cap with the file it came from. Pure.
pub fn effective(
    protected: bool,
    env: Option<&EnvSettings>,
    cap: Option<(DumpMode, &str)>,
) -> Result<Policy, String> {
    let default = EnvSettings::default();
    let env = env.unwrap_or(&default);
    // A protected environment is dumped masked unless its operator says
    // otherwise; anything else as it is.
    let mut dump = env.dump.unwrap_or(if protected {
        DumpMode::Masked
    } else {
        DumpMode::Unmasked
    });
    let mut capped_by = None;
    if let Some((c, file)) = cap {
        if c < dump {
            dump = c;
            capped_by = Some(file.to_string());
        }
    }
    let ttl_secs = match &env.ttl {
        Some(t) => parse_ttl(t)?,
        None if protected => PROTECTED_TTL,
        None => UNPROTECTED_TTL,
    };
    let reason = env.reason.unwrap_or(if protected {
        ReasonRule::Required
    } else {
        ReasonRule::Optional
    });
    Ok(Policy {
        protected,
        dump,
        dump_set: env.dump.is_some(),
        capped_by,
        ttl_secs,
        reason,
        unattended: env.unattended,
    })
}

/// Whether `kind` may be dumped under `policy` from `via`, and what it
/// needs first; or why not. `name` is what the operator types (the
/// environment's name, else the target's). Pure.
pub fn gate(policy: &Policy, kind: Kind, via: Via, name: &str) -> Result<Gate, String> {
    let set = || match &policy.capped_by {
        Some(file) => format!("{file} caps {name} at dump = \"{}\"", policy.dump.as_str()),
        None if policy.dump_set => format!(
            "environments.toml says dump = \"{}\" for {name}",
            policy.dump.as_str()
        ),
        None => format!(
            "a protected environment's default; dump = \"unmasked\" on {name} in your own \
             environments.toml allows a raw copy"
        ),
    };
    match (policy.dump, kind) {
        (DumpMode::Never, _) => return Err(format!("{name} is never dumped: {}", set())),
        (DumpMode::Schema, Kind::Raw | Kind::Masked) => {
            return Err(format!(
                "{name} is dumped schema only ({}): pass --schema-only",
                set()
            ))
        }
        (DumpMode::Masked, Kind::Raw) => {
            return Err(format!(
                "{name} is dumped masked ({}): leave out --unmasked for a masked copy, or pass \
                 --schema-only for the structure alone",
                set()
            ))
        }
        _ => {}
    }
    let raw_protected = policy.protected && kind == Kind::Raw;
    let reason = policy.reason == ReasonRule::Required;
    match (raw_protected, via) {
        (false, _) => Ok(Gate {
            typed: None,
            reason,
            unattended: false,
        }),
        (true, Via::Mcp) => Err(format!(
            "{name} is protected, and over MCP a protected database is dumped masked or schema \
             only: pass masked or schema_only, or ask the operator for a raw copy at a terminal"
        )),
        (true, Via::Terminal) => Ok(Gate {
            typed: Some(name.to_string()),
            reason,
            unattended: false,
        }),
        (true, Via::NoTerminal) if policy.unattended => Ok(Gate {
            typed: None,
            reason,
            unattended: true,
        }),
        (true, Via::NoTerminal) => Err(format!(
            "{name} is protected: a raw dump of it takes its name typed at a terminal, and there \
             is no terminal here. Pass --schema-only for the structure alone, or, for a job that \
             must take raw copies, set unattended = true on the environment in your own \
             environments.toml"
        )),
    }
}

/// The policy for `resolved`, read from environments.toml and the
/// project's `.pgman/pgman.toml` above `cwd`. Reads files.
pub fn for_target(
    resolved: &crate::inventory::target::Resolved,
    cwd: &std::path::Path,
) -> Result<Policy, String> {
    let envs = crate::inventory::load().environments.unwrap_or_default();
    let env = resolved
        .environment
        .as_ref()
        .and_then(|n| envs.iter().find(|e| &e.name == n))
        .map(|e| e.dump_settings());
    let project = crate::project::load_dumps(cwd)?;
    let cap = match (&project, &resolved.environment) {
        (Some((file, dumps)), Some(name)) => dumps
            .environments
            .get(name)
            .and_then(|e| e.dump)
            .map(|m| (m, file.display().to_string())),
        _ => None,
    };
    effective(
        resolved.protected,
        env.as_ref(),
        cap.as_ref().map(|(m, f)| (*m, f.as_str())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(dump: Option<DumpMode>, unattended: bool) -> EnvSettings {
        EnvSettings {
            dump,
            ttl: None,
            reason: None,
            unattended,
        }
    }

    #[test]
    fn ttl_parses_hours_days_and_weeks() {
        assert_eq!(parse_ttl("36h"), Ok(36 * 3_600));
        assert_eq!(parse_ttl("14d"), Ok(14 * 86_400));
        assert_eq!(parse_ttl(" 2w "), Ok(14 * 86_400));
        for bad in [
            "",
            "d",
            "14",
            "14m",
            "0d",
            "-1d",
            "1.5d",
            "99999999999999999999w",
        ] {
            assert!(parse_ttl(bad).is_err(), "{bad:?}");
        }
    }

    fn unmasked(unattended: bool) -> EnvSettings {
        settings(Some(DumpMode::Unmasked), unattended)
    }

    #[test]
    fn defaults_follow_protection() {
        let p = effective(true, None, None).unwrap();
        assert_eq!(p.dump, DumpMode::Masked);
        assert!(!p.dump_set);
        let e = gate(&p, Kind::Raw, Via::Terminal, "production").unwrap_err();
        assert!(
            e.contains("dump = \"unmasked\" on production in your own"),
            "{e}"
        );
        assert_eq!(
            effective(false, None, None).unwrap().dump,
            DumpMode::Unmasked
        );
        assert_eq!(p.ttl_secs, PROTECTED_TTL);
        assert_eq!(p.reason, ReasonRule::Required);
        assert!(!p.unattended);
        let u = effective(false, None, None).unwrap();
        assert_eq!(u.ttl_secs, UNPROTECTED_TTL);
        assert_eq!(u.reason, ReasonRule::Optional);
    }

    #[test]
    fn a_project_cap_lowers_and_never_raises() {
        let schema = settings(Some(DumpMode::Schema), false);
        let p = effective(true, Some(&schema), Some((DumpMode::Unmasked, "p.toml"))).unwrap();
        assert_eq!(p.dump, DumpMode::Schema);
        assert_eq!(p.capped_by, None);
        let open = unmasked(false);
        let p = effective(true, Some(&open), Some((DumpMode::Masked, "p.toml"))).unwrap();
        assert_eq!(p.dump, DumpMode::Masked);
        assert_eq!(p.capped_by.as_deref(), Some("p.toml"));
        let p = effective(true, Some(&open), Some((DumpMode::Never, "p.toml"))).unwrap();
        assert_eq!(p.dump, DumpMode::Never);
    }

    #[test]
    fn a_bad_ttl_is_refused() {
        let s = EnvSettings {
            ttl: Some("soon".into()),
            ..EnvSettings::default()
        };
        assert!(effective(false, Some(&s), None)
            .unwrap_err()
            .contains("dump_ttl"));
    }

    #[test]
    fn a_raw_protected_dump_takes_the_typed_name_at_a_terminal() {
        let p = effective(true, Some(&unmasked(false)), None).unwrap();
        let g = gate(&p, Kind::Raw, Via::Terminal, "production").unwrap();
        assert_eq!(g.typed.as_deref(), Some("production"));
        assert!(g.reason);
        assert!(!g.unattended);
    }

    #[test]
    fn a_raw_protected_dump_without_a_terminal_needs_unattended() {
        let p = effective(true, Some(&unmasked(false)), None).unwrap();
        let e = gate(&p, Kind::Raw, Via::NoTerminal, "production").unwrap_err();
        assert!(e.contains("unattended = true"), "{e}");
        let on = unmasked(true);
        let p = effective(true, Some(&on), None).unwrap();
        let g = gate(&p, Kind::Raw, Via::NoTerminal, "production").unwrap();
        assert_eq!(g.typed, None);
        assert!(g.unattended);
        assert!(g.reason);
        // At a terminal, unattended changes nothing: the name is typed.
        let g = gate(&p, Kind::Raw, Via::Terminal, "production").unwrap();
        assert_eq!(g.typed.as_deref(), Some("production"));
    }

    #[test]
    fn mcp_takes_a_protected_schema_and_never_raw() {
        let on = unmasked(true);
        let p = effective(true, Some(&on), None).unwrap();
        let e = gate(&p, Kind::Raw, Via::Mcp, "production").unwrap_err();
        assert!(e.contains("masked or schema only"), "{e}");
        let g = gate(&p, Kind::Schema, Via::Mcp, "production").unwrap();
        assert_eq!(g.typed, None);
        assert!(g.reason);
    }

    #[test]
    fn a_schema_dump_never_takes_the_typed_name() {
        let p = effective(true, None, None).unwrap();
        for via in [Via::Terminal, Via::NoTerminal, Via::Mcp] {
            let g = gate(&p, Kind::Schema, via, "production").unwrap();
            assert_eq!(g.typed, None, "{via:?}");
            assert!(g.reason);
        }
    }

    #[test]
    fn an_unprotected_dump_asks_nothing() {
        let p = effective(false, None, None).unwrap();
        for via in [Via::Terminal, Via::NoTerminal, Via::Mcp] {
            let g = gate(&p, Kind::Raw, via, "staging").unwrap();
            assert_eq!(
                g,
                Gate {
                    typed: None,
                    reason: false,
                    unattended: false
                }
            );
        }
    }

    #[test]
    fn modes_refuse_what_they_dont_allow() {
        let never = effective(false, Some(&settings(Some(DumpMode::Never), false)), None).unwrap();
        assert!(gate(&never, Kind::Schema, Via::Terminal, "x")
            .unwrap_err()
            .contains("never dumped"));
        let schema =
            effective(false, Some(&settings(Some(DumpMode::Schema), false)), None).unwrap();
        assert!(gate(&schema, Kind::Raw, Via::Terminal, "x")
            .unwrap_err()
            .contains("--schema-only"));
        assert!(gate(&schema, Kind::Schema, Via::Terminal, "x").is_ok());
        let masked = effective(true, Some(&settings(Some(DumpMode::Masked), true)), None).unwrap();
        assert!(gate(&masked, Kind::Raw, Via::NoTerminal, "x")
            .unwrap_err()
            .contains("leave out --unmasked"));
        let g = gate(&masked, Kind::Masked, Via::NoTerminal, "x").unwrap();
        assert_eq!(g.typed, None, "a masked dump never takes the typed name");
        assert!(gate(&masked, Kind::Masked, Via::Mcp, "x").is_ok());
        assert!(gate(&schema, Kind::Masked, Via::Terminal, "x").is_err());
        assert_eq!(default_kind(&masked), Kind::Masked);
        assert_eq!(default_kind(&schema), Kind::Schema);
        // Allowing a raw copy of a protected environment doesn't make it
        // the default; an unprotected one's default is raw.
        let allowed =
            effective(true, Some(&settings(Some(DumpMode::Unmasked), false)), None).unwrap();
        assert_eq!(default_kind(&allowed), Kind::Masked);
        let open = effective(false, None, None).unwrap();
        assert_eq!(default_kind(&open), Kind::Raw);
        let capped = effective(
            false,
            None,
            Some((DumpMode::Schema, "/repo/.pgman/pgman.toml")),
        )
        .unwrap();
        assert!(gate(&capped, Kind::Raw, Via::Terminal, "x")
            .unwrap_err()
            .contains("/repo/.pgman/pgman.toml caps x"));
    }
}
