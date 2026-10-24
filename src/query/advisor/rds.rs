//! What each setting the advisor suggests is called on RDS and Aurora,
//! whether a change needs a reboot there, and how Aurora differs. Pure
//! data plus the one function that renders it; no I/O.
//!
//! "Static" is RDS's word for a parameter that only applies at the next
//! reboot (`ApplyMethod=pending-reboot`); "dynamic" ones may apply
//! immediately. For community Postgres that is `pg_settings.context =
//! 'postmaster'`; the tests pin that the two agree for the rows here.

/// The unit a parameter group takes the value in. It is the unit
/// `pg_settings.unit` reports, so a value read from the server can be
/// carried over as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupUnit {
    /// 8 kB pages (`shared_buffers`).
    Pages8k,
    /// Kilobytes (`work_mem`, `log_temp_files`).
    Kb,
    /// Megabytes (`max_wal_size` on PG10+).
    Mb,
    /// Milliseconds (the timeouts).
    Ms,
    /// A plain number or boolean (`autovacuum_vacuum_scale_factor`).
    Plain,
}

impl GroupUnit {
    pub fn label(self) -> &'static str {
        match self {
            GroupUnit::Pages8k => "8kB pages",
            GroupUnit::Kb => "kB",
            GroupUnit::Mb => "MB",
            GroupUnit::Ms => "ms",
            GroupUnit::Plain => "",
        }
    }

    /// A byte count in this unit, rounded down; `None` for the units
    /// that are not sizes.
    pub fn from_bytes(self, bytes: i64) -> Option<i64> {
        match self {
            GroupUnit::Pages8k => Some(bytes / 8192),
            GroupUnit::Kb => Some(bytes / 1024),
            GroupUnit::Mb => Some(bytes / (1024 * 1024)),
            GroupUnit::Ms | GroupUnit::Plain => None,
        }
    }
}

/// How Aurora PostgreSQL differs from RDS for one parameter, as AWS's
/// Aurora PostgreSQL parameter reference lists it (the default cluster
/// and instance parameter group tables).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aurora {
    Same,
    /// Not in AWS's Aurora tables: pgman can't say how Aurora takes it.
    Unverified(&'static str),
    /// Exists on Aurora, but behaves or defaults differently.
    Differs(&'static str),
    /// Not modifiable, or meaningless, on Aurora.
    NotApplicable(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamMapping {
    /// The Postgres setting name.
    pub name: &'static str,
    /// The parameter group key. The same as `name` for every row today;
    /// kept apart because the table's job is to say what to type there.
    pub group_key: &'static str,
    /// Static: applies only at reboot.
    pub is_static: bool,
    pub unit: GroupUnit,
    pub aurora: Aurora,
    /// In both Aurora's cluster and instance parameter group tables: an
    /// instance's DB parameter group can override the cluster's value.
    pub aurora_instance_too: bool,
}

/// Every setting the advisor can suggest, sorted by name.
pub const PARAMS: &[ParamMapping] = &[
    ParamMapping {
        name: "autovacuum",
        group_key: "autovacuum",
        is_static: false,
        unit: GroupUnit::Plain,
        aurora: Aurora::Same,
        aurora_instance_too: false,
    },
    ParamMapping {
        name: "autovacuum_vacuum_scale_factor",
        group_key: "autovacuum_vacuum_scale_factor",
        is_static: false,
        unit: GroupUnit::Plain,
        aurora: Aurora::Same,
        aurora_instance_too: false,
    },
    ParamMapping {
        name: "idle_in_transaction_session_timeout",
        group_key: "idle_in_transaction_session_timeout",
        is_static: false,
        unit: GroupUnit::Ms,
        aurora: Aurora::Same,
        aurora_instance_too: true,
    },
    ParamMapping {
        name: "lock_timeout",
        group_key: "lock_timeout",
        is_static: false,
        unit: GroupUnit::Ms,
        // AWS's default Aurora PostgreSQL cluster and instance tables
        // don't list it.
        aurora: Aurora::Unverified(
            "not in AWS's Aurora parameter group tables: set it per role or session, and check your parameter group",
        ),
        aurora_instance_too: false,
    },
    ParamMapping {
        name: "log_lock_waits",
        group_key: "log_lock_waits",
        is_static: false,
        unit: GroupUnit::Plain,
        aurora: Aurora::Same,
        aurora_instance_too: true,
    },
    ParamMapping {
        name: "log_temp_files",
        group_key: "log_temp_files",
        is_static: false,
        unit: GroupUnit::Kb,
        aurora: Aurora::Same,
        aurora_instance_too: true,
    },
    ParamMapping {
        name: "max_wal_size",
        group_key: "max_wal_size",
        is_static: false,
        unit: GroupUnit::Mb,
        aurora: Aurora::NotApplicable(
            "Aurora's storage layer applies the WAL itself: there are no checkpoints to space out, and the parameter is not modifiable",
        ),
        aurora_instance_too: false,
    },
    ParamMapping {
        name: "shared_buffers",
        group_key: "shared_buffers",
        is_static: true,
        unit: GroupUnit::Pages8k,
        aurora: Aurora::Differs(
            "Aurora's default is about two-thirds of instance memory (SUM({DBInstanceClassMemory/12038},-50003) pages) against RDS's 25% ({DBInstanceClassMemory/32768}), as it has no OS page cache to share with",
        ),
        aurora_instance_too: true,
    },
    ParamMapping {
        name: "work_mem",
        group_key: "work_mem",
        is_static: false,
        unit: GroupUnit::Kb,
        aurora: Aurora::Same,
        aurora_instance_too: true,
    },
];

pub fn lookup(name: &str) -> Option<&'static ParamMapping> {
    PARAMS.iter().find(|p| p.name == name)
}

/// One line for the detail strip: the parameter group key, whether it
/// needs a reboot, and Aurora's difference when there is one. `aurora`
/// says the server is Aurora, which puts the Aurora note first.
pub fn describe(p: &ParamMapping, aurora: bool) -> String {
    let apply = if p.is_static {
        "static: applies at reboot"
    } else {
        "dynamic: no reboot"
    };
    let unit = match p.unit.label() {
        "" => String::new(),
        u => format!(", in {u}"),
    };
    let base = format!("parameter group key {} ({apply}{unit})", p.group_key);
    let line = match (p.aurora, aurora) {
        (Aurora::Same, _) if p.aurora_instance_too => {
            return format!("{base} · same on Aurora, where {AURORA_INSTANCE_OVERRIDE}");
        }
        (Aurora::Same, _) => format!("{base} · same on Aurora"),
        (Aurora::Unverified(why), true) => format!("Aurora: {why} · {base}"),
        (Aurora::Unverified(why), false) => format!("{base} · Aurora: {why}"),
        (Aurora::Differs(why), true) => format!("Aurora: {why} · {base}"),
        (Aurora::Differs(why), false) => format!("{base} · Aurora differs: {why}"),
        (Aurora::NotApplicable(why), true) => format!("Aurora: not applicable — {why}"),
        (Aurora::NotApplicable(why), false) => {
            format!("{base} · not on Aurora: {why}")
        }
    };
    if p.aurora_instance_too {
        format!("{line} · on Aurora, {AURORA_INSTANCE_OVERRIDE}")
    } else {
        line
    }
}

/// Said of a parameter both Aurora parameter groups carry.
const AURORA_INSTANCE_OVERRIDE: &str =
    "an instance's DB parameter group can override the cluster's value";

/// The `aws` command that sets `value` (already in the group's unit) in
/// a parameter group, with the apply method the parameter allows. The
/// group name is a placeholder: pgman does not know which group the
/// instance uses. Aurora takes it in the cluster parameter group.
pub fn aws_cli(p: &ParamMapping, value: &str, aurora: bool) -> String {
    let method = if p.is_static {
        "pending-reboot"
    } else {
        "immediate"
    };
    let (cmd, flag) = if aurora {
        (
            "modify-db-cluster-parameter-group",
            "--db-cluster-parameter-group-name",
        )
    } else {
        ("modify-db-parameter-group", "--db-parameter-group-name")
    };
    let note = if aurora && p.aurora_instance_too {
        "  # an instance's DB parameter group can override the cluster's value"
    } else {
        ""
    };
    format!(
        "aws rds {cmd} {flag} <your-group> --parameters \"ParameterName={},ParameterValue={value},ApplyMethod={method}\"{note}",
        p.group_key
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_has_no_duplicates() {
        let names: Vec<&str> = PARAMS.iter().map(|p| p.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted);
    }

    /// Community Postgres's `postmaster`-context settings among the rows:
    /// the ones a reboot is needed for. Every other row is `sighup`,
    /// `superuser` or `user` context, all of which apply without one.
    #[test]
    fn static_is_exactly_the_postmaster_context_settings() {
        const POSTMASTER: &[&str] = &["shared_buffers"];
        for p in PARAMS {
            assert_eq!(
                p.is_static,
                POSTMASTER.contains(&p.name),
                "{} static flag",
                p.name
            );
        }
    }

    #[test]
    fn units_match_what_pg_settings_reports() {
        assert_eq!(lookup("shared_buffers").unwrap().unit, GroupUnit::Pages8k);
        assert_eq!(lookup("work_mem").unwrap().unit, GroupUnit::Kb);
        assert_eq!(lookup("max_wal_size").unwrap().unit, GroupUnit::Mb);
        assert_eq!(lookup("lock_timeout").unwrap().unit, GroupUnit::Ms);
    }

    #[test]
    fn from_bytes_converts_sizes_and_refuses_the_rest() {
        assert_eq!(
            GroupUnit::Pages8k.from_bytes(256 * 1024 * 1024),
            Some(32768)
        );
        assert_eq!(GroupUnit::Kb.from_bytes(8 * 1024 * 1024), Some(8192));
        assert_eq!(GroupUnit::Mb.from_bytes(2 * 1024 * 1024 * 1024), Some(2048));
        assert_eq!(GroupUnit::Ms.from_bytes(1024), None);
        assert_eq!(GroupUnit::Plain.from_bytes(1024), None);
    }

    #[test]
    fn lookup_misses_an_unknown_name() {
        assert!(lookup("work_mem").is_some());
        assert!(lookup("no_such_setting").is_none());
    }

    #[test]
    fn describe_says_static_and_the_aurora_difference() {
        let sb = lookup("shared_buffers").unwrap();
        let rds = describe(sb, false);
        assert!(
            rds.starts_with(
                "parameter group key shared_buffers (static: applies at reboot, in 8kB pages)"
            ),
            "{rds}"
        );
        assert!(
            rds.contains("Aurora differs: Aurora's default is about two-thirds"),
            "{rds}"
        );
        let aurora = describe(sb, true);
        assert!(aurora.starts_with("Aurora: Aurora's default"), "{aurora}");
        assert!(
            aurora.ends_with(
                " · on Aurora, an instance's DB parameter group can override the cluster's value"
            ),
            "{aurora}"
        );
    }

    #[test]
    fn describe_says_dynamic_and_same_on_aurora() {
        let av = describe(lookup("autovacuum").unwrap(), false);
        assert_eq!(
            av,
            "parameter group key autovacuum (dynamic: no reboot) · same on Aurora"
        );
        let wm = describe(lookup("work_mem").unwrap(), false);
        assert_eq!(
            wm,
            "parameter group key work_mem (dynamic: no reboot, in kB) · same on Aurora, where an instance's DB parameter group can override the cluster's value"
        );
    }

    /// AWS's Aurora tables don't list lock_timeout: pgman says so
    /// rather than "same on Aurora".
    #[test]
    fn describe_doesnt_claim_what_the_aurora_tables_dont_list() {
        let lt = lookup("lock_timeout").unwrap();
        for aurora in [false, true] {
            let d = describe(lt, aurora);
            assert!(!d.contains("same on Aurora"), "{d}");
            assert!(
                d.contains("set it per role or session, and check your parameter group"),
                "{d}"
            );
        }
        assert!(describe(lt, true).starts_with("Aurora: not in AWS's"));
    }

    /// The parameters both of Aurora's group tables list, as AWS's
    /// Aurora PostgreSQL parameter reference has them.
    #[test]
    fn aurora_instance_too_is_the_parameters_in_both_tables() {
        const BOTH: &[&str] = &[
            "idle_in_transaction_session_timeout",
            "log_lock_waits",
            "log_temp_files",
            "shared_buffers",
            "work_mem",
        ];
        for p in PARAMS {
            assert_eq!(p.aurora_instance_too, BOTH.contains(&p.name), "{}", p.name);
        }
    }

    #[test]
    fn describe_says_not_applicable_on_aurora() {
        let mw = lookup("max_wal_size").unwrap();
        assert!(describe(mw, true).starts_with("Aurora: not applicable"));
        assert!(describe(mw, false).contains("not on Aurora:"));
    }

    #[test]
    fn aws_cli_uses_the_apply_method_the_parameter_allows() {
        let sb = aws_cli(lookup("shared_buffers").unwrap(), "32768", false);
        assert!(sb.contains("modify-db-parameter-group --db-parameter-group-name"));
        assert!(sb.contains(
            "ParameterName=shared_buffers,ParameterValue=32768,ApplyMethod=pending-reboot"
        ));
        let wm = aws_cli(lookup("work_mem").unwrap(), "8192", true);
        assert!(wm.contains("modify-db-cluster-parameter-group --db-cluster-parameter-group-name"));
        assert!(
            wm.contains("ApplyMethod=immediate\"  # an instance's DB parameter group can override"),
            "{wm}"
        );
        assert!(!aws_cli(lookup("work_mem").unwrap(), "8192", false).contains('#'));
        assert!(!aws_cli(lookup("autovacuum").unwrap(), "1", true).contains('#'));
    }
}
