use super::*;

fn ctx(kind: ServerKind) -> Context {
    Context {
        version_num: 160_004,
        block_size: 8192,
        kind,
        in_recovery: false,
        can_read_all_stats: true,
        uptime_secs: 10.0 * 86_400.0,
        server_start: "2026-10-13 08:00:00+00".into(),
        database: "shop".into(),
        has_pg_stat_statements: true,
    }
}

fn win(age: f64) -> Window {
    Window {
        reset_at: Some("2026-10-20 09:14:03+00".into()),
        reset_age_secs: Some(age),
        uptime_secs: 10.0 * 86_400.0,
        server_start: "2026-10-13 08:00:00+00".into(),
    }
}

const DAY: f64 = 86_400.0;

fn mem(setting: &str, unit: &str, context: &str) -> Setting {
    Setting {
        setting: setting.into(),
        unit: Some(unit.into()),
        context: context.into(),
    }
}

fn db(hit: i64, read: i64, temp_bytes: i64, age: f64) -> DbStats {
    DbStats {
        blks_hit: hit,
        blks_read: read,
        temp_files: if temp_bytes > 0 { 12 } else { 0 },
        temp_bytes,
        window: win(age),
    }
}

fn advice(f: &Finding) -> &Advice {
    f.advice.as_deref().expect("advisor finding carries advice")
}

// --- values -----------------------------------------------------------

#[test]
fn setting_bytes_reads_each_unit_pg_settings_uses() {
    assert_eq!(mem("16384", "8kB", "postmaster").bytes(), Some(128 * MIB));
    assert_eq!(mem("4096", "kB", "user").bytes(), Some(4 * MIB));
    assert_eq!(mem("1024", "MB", "sighup").bytes(), Some(GIB));
    assert_eq!(mem("5000", "ms", "user").bytes(), None);
    assert_eq!(mem("x", "kB", "user").bytes(), None);
    let plain = Setting {
        setting: "0.2".into(),
        unit: None,
        context: "sighup".into(),
    };
    assert_eq!(plain.bytes(), None);
    assert_eq!(plain.display(), "0.2");
    assert_eq!(mem("5000", "ms", "user").display(), "5s");
}

/// `SHOW`'s spelling: a time in its largest exact unit, zero and -1
/// bare (Postgres prints `0`, not `0ms`, and `-1` for "off").
#[test]
fn setting_display_spells_times_and_zero_as_show_does() {
    assert_eq!(mem("0", "ms", "user").display(), "0");
    assert_eq!(mem("-1", "kB", "superuser").display(), "-1");
    assert_eq!(mem("0", "kB", "superuser").display(), "0");
    assert_eq!(mem("86400000", "ms", "user").display(), "1d");
    assert_eq!(mem("3600000", "ms", "user").display(), "1h");
    assert_eq!(mem("300000", "ms", "user").display(), "5min");
    assert_eq!(mem("1500", "ms", "user").display(), "1500ms");
    assert_eq!(mem("300", "s", "sighup").display(), "5min");
    assert_eq!(mem("90", "s", "sighup").display(), "90s");
    assert_eq!(mem("2", "min", "sighup").display(), "2min");
    assert_eq!(mem("86400000", "ms", "user").millis(), Some(86_400_000));
    assert_eq!(mem("300", "s", "sighup").millis(), Some(300_000));
    assert_eq!(mem("4096", "kB", "user").millis(), None);
}

#[test]
fn pg_size_uses_the_largest_exact_unit() {
    assert_eq!(pg_size(256 * MIB), "256MB");
    assert_eq!(pg_size(2 * GIB), "2GB");
    assert_eq!(pg_size(1536 * KIB), "1536kB");
    assert_eq!(pg_size(0), "0kB");
}

#[test]
fn format_span_keeps_the_two_largest_units() {
    assert_eq!(format_span(40.0), "40s");
    assert_eq!(format_span(12.0 * 60.0 + 5.0), "12m");
    assert_eq!(format_span(2.0 * 3600.0 + 600.0), "2h 10m");
    assert_eq!(format_span(3.0 * DAY + 4.0 * 3600.0 + 59.0), "3d 4h");
    assert_eq!(format_span(-5.0), "0s");
    // A zero second unit is left off.
    assert_eq!(format_span(3600.0), "1h");
    assert_eq!(format_span(3600.0 + 59.0), "1h");
    assert_eq!(format_span(2.0 * DAY + 59.0), "2d");
    assert_eq!(format_span(DAY + 3600.0), "1d 1h");
}

#[test]
fn window_names_the_reset_or_the_server_start() {
    assert_eq!(
        win(3.0 * DAY + 4.0 * 3600.0).describe(),
        "counters since 2026-10-20 09:14:03+00 (3d 4h)"
    );
    let never = Window {
        reset_at: None,
        reset_age_secs: None,
        uptime_secs: 7200.0,
        server_start: "2026-10-23 06:00:00+00".into(),
    };
    assert_eq!(never.secs(), 7200.0);
    assert_eq!(
        never.describe(),
        "counters never reset: at least 2h (server started 2026-10-23 06:00:00+00)"
    );
}

#[test]
fn server_kind_prefers_aurora_over_rds() {
    assert_eq!(server_kind(false, false), ServerKind::SelfHosted);
    assert_eq!(server_kind(true, false), ServerKind::Rds);
    assert_eq!(server_kind(true, true), ServerKind::Aurora);
}

// --- SQL --------------------------------------------------------------

#[test]
fn every_statement_is_tagged_and_a_select() {
    let th = Thresholds::default();
    for sql in [
        CONTEXT_SQL.to_string(),
        settings_sql(),
        DB_STATS_SQL.to_string(),
        SPILLERS_SQL.to_string(),
        vacuum_sql(&th),
        locks_sql(160_000),
        checkpoints_sql(160_000),
        checkpoints_sql(170_000),
        WAL_SQL.to_string(),
    ] {
        assert!(sql.starts_with("/* pgman:advisor */ "), "{sql}");
        // The wizard runs these on the editor's session, which may be
        // inside a transaction: now() would be its start, and every age
        // (a lock wait, the stats window) would be short by that much.
        assert!(!sql.contains("now()"), "use clock_timestamp(): {sql}");
        let body = sql.trim_start_matches("/* pgman:advisor */ ");
        assert!(
            body.starts_with("SELECT") || body.starts_with("WITH"),
            "{sql}"
        );
        assert!(
            !crate::safety::classify(&sql).is_write(),
            "advisor statement classified as a write: {sql}"
        );
    }
}

#[test]
fn checkpoints_read_pg_stat_checkpointer_from_pg17() {
    let old = checkpoints_sql(160_004);
    assert!(old.contains("FROM pg_stat_bgwriter"), "{old}");
    assert!(old.contains("checkpoints_timed, checkpoints_req"), "{old}");
    let new = checkpoints_sql(170_000);
    assert!(new.contains("FROM pg_stat_checkpointer"), "{new}");
    assert!(new.contains("num_timed, num_requested"), "{new}");
    assert!(checkpoints_sql(169_999).contains("pg_stat_bgwriter"));
}

#[test]
fn lock_waits_read_waitstart_from_pg14() {
    assert!(locks_sql(140_000).contains("l.waitstart"));
    assert!(!locks_sql(139_999).contains("waitstart"));
    assert!(locks_sql(139_999).contains("clock_timestamp() - s.query_start"));
}

#[test]
fn settings_sql_names_every_mapped_setting() {
    let sql = settings_sql();
    for p in rds::PARAMS {
        assert!(sql.contains(&format!("'{}'", p.name)), "{}", p.name);
    }
    assert!(sql.contains("'autovacuum_vacuum_threshold'"));
    assert!(sql.contains("'checkpoint_completion_target'"));
}

#[test]
fn vacuum_sql_carries_the_thresholds_inline() {
    let sql = vacuum_sql(&Thresholds::default());
    assert!(sql.contains("n_live_tup >= 10000"), "{sql}");
    assert!(sql.contains("n_dead_tup >= 0.1 * s.n_live_tup"), "{sql}");
    assert!(sql.contains("'{}'::text[]"), "{sql}");
}

// --- ADV000 -------------------------------------------------------------

#[test]
fn a_young_window_gets_a_note_and_quiets_the_ratio_checks() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let young = db(1_000, 1_000_000, GIB, 3599.0);
    let note = young_window_note(&c, &young.window, &th).expect("note");
    assert_eq!(note.code, "ADV000");
    assert!(
        note.detail.contains("wait until 1h have"),
        "{}",
        note.detail
    );
    assert!(cache_hit_finding(&c, &young, None, &th).is_none());
    assert!(temp_spill_finding(&c, &young, None, &Spillers::Known(vec![]), &th).is_none());
    let cp = Checkpoints {
        timed: 1,
        requested: 100,
        window: win(3599.0),
        wal: Err("not read".into()),
    };
    assert!(checkpoint_finding(&c, &cp, None, None, &th).is_none());

    let grown = db(1_000, 1_000_000, GIB, 3600.0);
    assert!(young_window_note(&c, &grown.window, &th).is_none());
    assert!(cache_hit_finding(&c, &grown, None, &th).is_some());
}

// --- ADV001 -------------------------------------------------------------

#[test]
fn cache_hit_fires_below_099_and_is_med_below_095() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let sb = mem("16384", "8kB", "postmaster");
    // 99.0% exactly: quiet.
    assert!(cache_hit_finding(&c, &db(990_000, 10_000, 0, DAY), Some(&sb), &th).is_none());
    // 98.9%: LOW.
    let low = cache_hit_finding(&c, &db(989_000, 11_000, 0, DAY), Some(&sb), &th).unwrap();
    assert_eq!(low.severity, Severity::Low);
    // 95.0% exactly: still LOW; 94.9%: MED.
    let edge = cache_hit_finding(&c, &db(950_000, 50_000, 0, DAY), Some(&sb), &th).unwrap();
    assert_eq!(edge.severity, Severity::Low);
    let med = cache_hit_finding(&c, &db(949_000, 51_000, 0, DAY), Some(&sb), &th).unwrap();
    assert_eq!(med.severity, Severity::Medium);
    assert_eq!(med.code, "ADV001");
    assert_eq!(med.object, "shop");
}

#[test]
fn cache_hit_needs_enough_block_requests() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    assert!(cache_hit_finding(&c, &db(50_000, 49_999, 0, DAY), None, &th).is_none());
    assert!(cache_hit_finding(&c, &db(50_000, 50_000, 0, DAY), None, &th).is_some());
    assert!(cache_hit_finding(
        &c,
        &db(0, 0, 0, DAY),
        None,
        &Thresholds {
            min_block_requests: 0,
            ..th
        }
    )
    .is_none());
}

#[test]
fn cache_hit_suggests_double_with_a_restart_self_hosted() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let sb = mem("16384", "8kB", "postmaster");
    let f = cache_hit_finding(&c, &db(900_000, 100_000, 0, DAY), Some(&sb), &th).unwrap();
    assert_eq!(
        f.suggestion.as_deref(),
        Some("ALTER SYSTEM SET shared_buffers = '256MB';  -- then restart the server")
    );
    let a = advice(&f);
    let ch = a.change.as_ref().unwrap();
    assert_eq!(
        (ch.current.as_str(), ch.suggested.as_str()),
        ("128MB", "256MB")
    );
    assert!(ch.restart);
    assert!(a.window.starts_with("counters since"), "{}", a.window);
    assert_eq!(a.evidence_sql, DB_STATS_SQL);
    assert!(
        a.evidence[0].contains("blks_hit 900000 · blks_read 100000"),
        "{:?}",
        a.evidence
    );
}

#[test]
fn cache_hit_on_rds_gives_the_parameter_group_command_in_pages() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::Rds);
    let sb = mem("16384", "8kB", "postmaster");
    let f = cache_hit_finding(&c, &db(900_000, 100_000, 0, DAY), Some(&sb), &th).unwrap();
    let s = f.suggestion.clone().unwrap();
    assert!(s.starts_with("aws rds modify-db-parameter-group"), "{s}");
    assert!(
        s.contains("ParameterValue=32768,ApplyMethod=pending-reboot"),
        "{s}"
    );
    let ch = advice(&f).change.clone().unwrap();
    assert!(
        ch.reasoning.contains("RDS's default is already 25%"),
        "{}",
        ch.reasoning
    );
}

#[test]
fn cache_hit_on_aurora_says_leave_it() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::Aurora);
    let sb = mem("16384", "8kB", "postmaster");
    let f = cache_hit_finding(&c, &db(900_000, 100_000, 0, DAY), Some(&sb), &th).unwrap();
    assert!(f.suggestion.is_none());
    let ch = advice(&f).change.clone().unwrap();
    assert!(ch.reasoning.starts_with("leave it"), "{}", ch.reasoning);
    assert!(ch.rds.starts_with("Aurora:"), "{}", ch.rds);
}

// --- ADV002 -------------------------------------------------------------

#[test]
fn temp_spill_starts_at_64_mib_and_is_med_from_256_mib_an_hour() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let none = Spillers::Known(vec![]);
    assert!(temp_spill_finding(&c, &db(1, 0, 64 * MIB - 1, DAY), None, &none, &th).is_none());
    let low = temp_spill_finding(&c, &db(1, 0, 64 * MIB, DAY), None, &none, &th).unwrap();
    assert_eq!(low.severity, Severity::Low);
    // 10 hours: 2560 MiB is exactly 256 MiB an hour.
    let at = temp_spill_finding(&c, &db(1, 0, 2560 * MIB, 36_000.0), None, &none, &th).unwrap();
    assert_eq!(at.severity, Severity::Medium);
    let under = temp_spill_finding(&c, &db(1, 0, 2559 * MIB, 36_000.0), None, &none, &th).unwrap();
    assert_eq!(under.severity, Severity::Low);
}

#[test]
fn suggest_work_mem_covers_a_spill_within_two_to_four_times() {
    // No statements known: twice.
    assert_eq!(suggest_work_mem(4 * MIB, None).0, 8 * MIB);
    // Small spill: still at least twice.
    assert_eq!(suggest_work_mem(4 * MIB, Some(MIB)).0, 8 * MIB);
    // 10 MiB a call → 16 MiB.
    assert_eq!(suggest_work_mem(4 * MIB, Some(10 * MIB)).0, 16 * MIB);
    // Just under 4×: the power of two is 4× exactly.
    assert_eq!(suggest_work_mem(4 * MIB, Some(16 * MIB - 1)).0, 16 * MIB);
    // 4× or more: twice, and the reasoning says look at the statement.
    let (v, why) = suggest_work_mem(4 * MIB, Some(16 * MIB));
    assert_eq!(v, 8 * MIB);
    assert!(why.contains("no safe work_mem covers"), "{why}");
}

#[test]
fn temp_spill_names_the_statements_and_their_role() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::Rds);
    let spillers = Spillers::Known(vec![
        Spiller {
            query: "SELECT * FROM orders ORDER BY placed_at".into(),
            calls: 4,
            temp_bytes: 40 * MIB,
            role: "shop_app".into(),
        },
        Spiller {
            query: "SELECT 2".into(),
            calls: 100,
            temp_bytes: 100 * MIB,
            role: "batch".into(),
        },
    ]);
    let wm = mem("4096", "kB", "user");
    let f = temp_spill_finding(&c, &db(1, 0, GIB, DAY), Some(&wm), &spillers, &th).unwrap();
    // The worst per call is 10 MiB (shop_app): 16MB for that role.
    assert_eq!(
        f.suggestion.as_deref(),
        Some("ALTER ROLE shop_app SET work_mem = '16MB';")
    );
    let a = advice(&f);
    assert!(
        a.evidence[1].starts_with("10 MB a call × 4 calls: SELECT * FROM orders"),
        "{:?}",
        a.evidence
    );
    assert!(a.evidence_sql.contains("pg_stat_statements"));
    assert!(a.change.as_ref().unwrap().rds.starts_with("per role:"));
}

#[test]
fn temp_spill_without_pg_stat_statements_says_how_to_get_it() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let why = Spillers::Unavailable(crate::query::slow_queries::PGSS_MISSING.into());
    let f = temp_spill_finding(&c, &db(1, 0, GIB, DAY), None, &why, &th).unwrap();
    let a = advice(&f);
    assert!(
        a.evidence
            .iter()
            .any(|e| e.contains("CREATE EXTENSION pg_stat_statements")),
        "{:?}",
        a.evidence
    );
    assert!(f.detail.contains("log_temp_files"), "{}", f.detail);
    assert_eq!(
        f.suggestion.as_deref(),
        Some("ALTER ROLE <app_role> SET work_mem = '8MB';")
    );
    assert!(!a.evidence_sql.contains("pg_stat_statements"));
}

// --- ADV003 -------------------------------------------------------------

fn table(live: i64, dead: i64, opts: &[&str]) -> TableVacuum {
    TableVacuum {
        schema: "public".into(),
        table: "orders".into(),
        quoted: "public.orders".into(),
        live,
        dead,
        reloptions: opts.iter().map(|s| s.to_string()).collect(),
        since_vacuum_secs: Some(7200.0),
    }
}

const DEFAULTS: VacuumDefaults = VacuumDefaults {
    enabled: true,
    scale_factor: 0.2,
    threshold: 50,
};

#[test]
fn reloptions_parse_the_autovacuum_storage_parameters() {
    let o = parse_reloptions(&[
        "fillfactor=90".into(),
        "autovacuum_enabled=false".into(),
        "autovacuum_vacuum_scale_factor=0.5".into(),
        "autovacuum_vacuum_threshold=1000".into(),
        "junk".into(),
    ]);
    assert_eq!(o.enabled, Some(false));
    assert_eq!(o.scale_factor, Some(0.5));
    assert_eq!(o.threshold, Some(1000));
    assert_eq!(
        parse_reloptions(&["autovacuum_enabled=on".into()]).enabled,
        Some(true)
    );
    assert_eq!(parse_reloptions(&[]), TableAutovacuum::default());
}

#[test]
fn vacuum_defaults_read_pg_settings() {
    let mut s = Settings::new();
    s.insert(
        "autovacuum".into(),
        Setting {
            setting: "off".into(),
            unit: None,
            context: "sighup".into(),
        },
    );
    s.insert(
        "autovacuum_vacuum_scale_factor".into(),
        Setting {
            setting: "0.1".into(),
            unit: None,
            context: "sighup".into(),
        },
    );
    let d = VacuumDefaults::from_settings(&s);
    assert!(!d.enabled);
    assert_eq!(d.scale_factor, 0.1);
    assert_eq!(d.threshold, 50);
    assert!(VacuumDefaults::from_settings(&Settings::new()).enabled);
}

#[test]
fn dead_tuples_fire_from_ten_percent_on_ten_thousand_rows() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    assert!(dead_tuple_finding(&c, &table(9_999, 9_000, &[]), DEFAULTS, &th).is_none());
    assert!(dead_tuple_finding(&c, &table(10_000, 999, &[]), DEFAULTS, &th).is_none());
    let f = dead_tuple_finding(&c, &table(10_000, 1_000, &[]), DEFAULTS, &th).unwrap();
    assert_eq!(f.code, "ADV003");
    assert_eq!(f.severity, Severity::Low);
    assert_eq!(f.object, "public.orders");
    let med = dead_tuple_finding(&c, &table(10_000, 2_000, &[]), DEFAULTS, &th).unwrap();
    assert_eq!(med.severity, Severity::Medium);
}

#[test]
fn dead_tuples_under_the_trigger_blame_the_scale_factor() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    // Trigger 50 + 0.2 × 100 000 = 20 050; 15 000 dead is under it.
    let f = dead_tuple_finding(&c, &table(100_000, 15_000, &[]), DEFAULTS, &th).unwrap();
    assert!(
        f.title.contains("its scale factor allows it"),
        "{}",
        f.title
    );
    assert_eq!(
        f.suggestion.as_deref(),
        Some("ALTER TABLE public.orders SET (autovacuum_vacuum_scale_factor = 0.05);")
    );
    let a = advice(&f);
    assert!(
        a.evidence[1].starts_with("autovacuum runs at 20050 dead"),
        "{:?}",
        a.evidence
    );
    assert!(a.window.contains("last vacuumed 2h ago"), "{}", a.window);
    let ch = a.change.as_ref().unwrap();
    assert_eq!(
        (ch.current.as_str(), ch.suggested.as_str()),
        ("0.2", "0.05")
    );
    assert!(
        ch.rds.starts_with("a table storage parameter"),
        "{}",
        ch.rds
    );
}

#[test]
fn dead_tuples_past_the_trigger_say_autovacuum_is_behind() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let f = dead_tuple_finding(&c, &table(100_000, 20_051, &[]), DEFAULTS, &th).unwrap();
    assert!(f.title.contains("autovacuum is behind"), "{}", f.title);
    assert!(f.detail.contains("long transaction"), "{}", f.detail);
    let at = dead_tuple_finding(&c, &table(100_000, 20_050, &[]), DEFAULTS, &th).unwrap();
    assert!(at.title.contains("scale factor allows"), "{}", at.title);
}

#[test]
fn dead_tuples_with_autovacuum_off_say_turn_it_back_on() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let f = dead_tuple_finding(
        &c,
        &table(10_000, 1_000, &["autovacuum_enabled=false"]),
        DEFAULTS,
        &th,
    )
    .unwrap();
    assert_eq!(f.severity, Severity::Medium);
    assert_eq!(
        f.suggestion.as_deref(),
        Some("ALTER TABLE public.orders RESET (autovacuum_enabled);")
    );
    // The setting shown is the one the suggestion changes, not the
    // scale factor (found driving the wizard by hand).
    let ch = advice(&f).change.clone().unwrap();
    assert_eq!(ch.name, "autovacuum_enabled");
    assert_eq!(ch.current, "false");
    let off = VacuumDefaults {
        enabled: false,
        ..DEFAULTS
    };
    let g =
        dead_tuple_finding(&ctx(ServerKind::Rds), &table(10_000, 1_000, &[]), off, &th).unwrap();
    assert!(g.title.contains("off server-wide"), "{}", g.title);
    let s = g.suggestion.unwrap();
    assert!(
        s.contains("ParameterName=autovacuum,ParameterValue=1,ApplyMethod=immediate"),
        "{s}"
    );
}

#[test]
fn dead_tuples_already_at_a_low_scale_factor_get_no_scale_suggestion() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let f = dead_tuple_finding(
        &c,
        &table(100_000, 20_000, &["autovacuum_vacuum_scale_factor=0.05"]),
        DEFAULTS,
        &th,
    )
    .unwrap();
    assert!(f.title.contains("behind"), "{}", f.title);
    assert!(f.suggestion.is_none());
    assert!(advice(&f).change.is_none());
}

#[test]
fn big_tables_get_the_smaller_scale_factor() {
    assert_eq!(suggested_scale_factor(BIG_TABLE_ROWS - 1), 0.05);
    assert_eq!(suggested_scale_factor(BIG_TABLE_ROWS), 0.01);
}

// --- ADV004 -------------------------------------------------------------

fn sess(pid: i32, state: &str, wait: Option<f64>, blockers: &[i32]) -> LockSession {
    LockSession {
        pid,
        user: "shop_app".into(),
        state: state.into(),
        xact_secs: Some(250.0),
        wait_secs: wait,
        blockers: blockers.to_vec(),
        query: format!("UPDATE orders SET status = 'x' -- {pid}"),
    }
}

#[test]
fn blocking_chains_find_the_head_through_a_waiter() {
    // 10 holds; 11 waits on 10; 12 waits on 11; 20 holds; 21 waits on 20.
    let s = vec![
        sess(10, "idle in transaction", None, &[]),
        sess(11, "active", Some(30.0), &[10]),
        sess(12, "active", Some(20.0), &[11]),
        sess(20, "active", None, &[]),
        sess(21, "active", Some(5.0), &[20]),
    ];
    let chains = blocking_chains(&s);
    assert_eq!(
        chains,
        vec![
            Chain {
                root: 10,
                waiters: vec![11, 12]
            },
            Chain {
                root: 20,
                waiters: vec![21]
            },
        ]
    );
}

#[test]
fn blocking_chains_survive_a_cycle_and_an_unseen_head() {
    // 1 and 2 wait on each other (a deadlock not yet broken); 3 waits on
    // 99, which isn't in the list (not a client backend).
    let s = vec![
        sess(1, "active", Some(1.0), &[2]),
        sess(2, "active", Some(1.0), &[1]),
        sess(3, "active", Some(1.0), &[99]),
    ];
    let chains = blocking_chains(&s);
    assert_eq!(
        chains,
        vec![Chain {
            root: 99,
            waiters: vec![3]
        }]
    );
}

#[test]
fn lock_finding_fires_from_five_seconds_and_is_high_from_sixty_or_five_waiters() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let quiet = vec![
        sess(10, "active", None, &[]),
        sess(11, "active", Some(4.9), &[10]),
    ];
    assert!(lock_finding(&c, &quiet, &Settings::new(), &th).is_none());
    let med = vec![
        sess(10, "active", None, &[]),
        sess(11, "active", Some(5.0), &[10]),
    ];
    let f = lock_finding(&c, &med, &Settings::new(), &th).unwrap();
    assert_eq!(f.severity, Severity::Medium);
    assert_eq!(f.code, "ADV004");
    assert_eq!(f.object, "pid 10");
    let long = vec![
        sess(10, "active", None, &[]),
        sess(11, "active", Some(60.0), &[10]),
    ];
    assert_eq!(
        lock_finding(&c, &long, &Settings::new(), &th)
            .unwrap()
            .severity,
        Severity::High
    );
    let mut many = vec![sess(10, "active", None, &[])];
    many.extend((11..16).map(|p| sess(p, "active", Some(6.0), &[10])));
    assert_eq!(
        lock_finding(&c, &many, &Settings::new(), &th)
            .unwrap()
            .severity,
        Severity::High
    );
    let mut four = vec![sess(10, "active", None, &[])];
    four.extend((11..15).map(|p| sess(p, "active", Some(6.0), &[10])));
    assert_eq!(
        lock_finding(&c, &four, &Settings::new(), &th)
            .unwrap()
            .severity,
        Severity::Medium
    );
    assert!(lock_finding(&c, &[], &Settings::new(), &th).is_none());
}

#[test]
fn an_idle_in_transaction_head_gets_the_idle_timeout_for_its_role() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let s = vec![
        sess(10, "idle in transaction", None, &[]),
        sess(11, "active", Some(30.0), &[10]),
        sess(12, "active", Some(20.0), &[11]),
    ];
    let off = Setting {
        setting: "0".into(),
        unit: Some("ms".into()),
        context: "user".into(),
    };
    let f = lock_finding(&c, &s, &idle_timeout(&off), &th).unwrap();
    assert_eq!(
        f.suggestion.as_deref(),
        Some("ALTER ROLE shop_app SET idle_in_transaction_session_timeout = '5min';")
    );
    let a = advice(&f);
    assert_eq!(
        a.evidence[0],
        "pid 10 (shop_app, idle in transaction, in a transaction 4m) blocks 2: 11 (30s), 12 (20s)"
    );
    assert!(
        a.evidence[1].starts_with("  pid 10 last ran: UPDATE orders"),
        "{:?}",
        a.evidence
    );
    assert_eq!(a.window, "a snapshot, now");
    // With the timeout already set, the waiters get lock_timeout instead.
    let on = Setting {
        setting: "60000".into(),
        ..off
    };
    let g = lock_finding(&c, &s, &idle_timeout(&on), &th).unwrap();
    assert_eq!(
        g.suggestion.as_deref(),
        Some("ALTER ROLE shop_app SET lock_timeout = '5s';")
    );
}

fn idle_timeout(s: &Setting) -> Settings {
    Settings::from([("idle_in_transaction_session_timeout".to_string(), s.clone())])
}

/// A per-session setting's current value is pgman's own session's: the
/// application's role can differ, so it says whose it is.
#[test]
fn session_level_values_say_they_are_this_session_s() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let s = vec![
        sess(10, "idle in transaction", None, &[]),
        sess(11, "active", Some(30.0), &[10]),
    ];
    let off = Setting {
        setting: "0".into(),
        unit: Some("ms".into()),
        context: "user".into(),
    };
    let f = lock_finding(&c, &s, &idle_timeout(&off), &th).unwrap();
    assert_eq!(
        advice(&f).change.as_ref().unwrap().current,
        "0 (this session)"
    );
    let wm = mem("4096", "kB", "user");
    let none = Spillers::Known(vec![]);
    let f = temp_spill_finding(&c, &db(1, 0, GIB, DAY), Some(&wm), &none, &th).unwrap();
    assert_eq!(
        advice(&f).change.as_ref().unwrap().current,
        "4MB (this session)"
    );
    let f = temp_spill_finding(&c, &db(1, 0, GIB, DAY), None, &none, &th).unwrap();
    assert_eq!(
        advice(&f).change.as_ref().unwrap().current,
        "4MB (the default; not read)"
    );
    assert_eq!(this_session(None), "unknown");
}

#[test]
fn without_pg_read_all_stats_the_lock_finding_says_what_is_hidden() {
    let th = Thresholds::default();
    let mut c = ctx(ServerKind::SelfHosted);
    c.can_read_all_stats = false;
    let mut head = sess(10, "", None, &[]);
    head.user = String::new();
    head.query = String::new();
    let s = vec![head, sess(11, "active", Some(30.0), &[10])];
    let f = lock_finding(&c, &s, &Settings::new(), &th).unwrap();
    let a = advice(&f);
    assert!(
        a.evidence[0].starts_with("pid 10 (?, state hidden"),
        "{:?}",
        a.evidence
    );
    assert!(
        a.evidence.iter().any(|e| e.contains("GRANT pg_monitor")),
        "{:?}",
        a.evidence
    );
}

// --- ADV005 -------------------------------------------------------------

/// Checkpoint counters over a day, with a GiB of WAL per requested
/// checkpoint over the same window: plenty to have forced them all.
fn cps(timed: i64, requested: i64) -> Checkpoints {
    Checkpoints {
        timed,
        requested,
        window: win(DAY),
        wal: Ok(WalVolume {
            bytes: requested * GIB,
            window: win(DAY),
        }),
    }
}

fn with_wal(mut c: Checkpoints, bytes: i64, age: f64) -> Checkpoints {
    c.wal = Ok(WalVolume {
        bytes,
        window: win(age),
    });
    c
}

#[test]
fn checkpoints_fire_over_a_fifth_requested_with_ten_of_them() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let mw = mem("1024", "MB", "sighup");
    assert!(checkpoint_finding(&c, &cps(8, 2), Some(&mw), None, &th).is_none());
    let f = checkpoint_finding(&c, &cps(79, 21), Some(&mw), None, &th).unwrap();
    assert_eq!(f.code, "ADV005");
    assert_eq!(f.severity, Severity::Medium);
    assert!(f.title.contains("forced by WAL volume"), "{}", f.title);
    assert_eq!(
        f.suggestion.as_deref(),
        Some("ALTER SYSTEM SET max_wal_size = '2GB'; SELECT pg_reload_conf();")
    );
    assert!(checkpoint_finding(&c, &cps(80, 20), Some(&mw), None, &th).is_none());
    assert!(checkpoint_finding(&c, &cps(3, 6), Some(&mw), None, &th).is_none());
    let a = advice(&f);
    assert!(a.evidence[0].starts_with("pg_stat_bgwriter: 21 requested · 79 timed"));
    assert_eq!(
        a.evidence[1],
        "pg_stat_wal: 21 GB written over the same window"
    );
    assert!(a.evidence_sql.ends_with(WAL_SQL), "{}", a.evidence_sql);
}

#[test]
fn checkpoints_name_pg_stat_checkpointer_on_pg17_and_skip_aurora() {
    let th = Thresholds::default();
    let mut c = ctx(ServerKind::Rds);
    c.version_num = 170_002;
    let mw = mem("2048", "MB", "sighup");
    let f = checkpoint_finding(&c, &cps(10, 90), Some(&mw), None, &th).unwrap();
    let a = advice(&f);
    assert!(a.evidence[0].starts_with("pg_stat_checkpointer:"));
    assert!(a.evidence_sql.contains("pg_stat_checkpointer"));
    let s = f.suggestion.clone().unwrap();
    assert!(
        s.contains("ParameterName=max_wal_size,ParameterValue=4096,ApplyMethod=immediate"),
        "{s}"
    );
    assert!(
        checkpoint_finding(&ctx(ServerKind::Aurora), &cps(10, 90), Some(&mw), None, &th).is_none()
    );
}

/// The compose primary as the release panel found it: 587 requested
/// against 794 timed, but only 2.8 GB of WAL at max_wal_size 1GB —
/// room for about five WAL-triggered checkpoints. The rest came from
/// CREATE / DROP DATABASE and the like, which max_wal_size doesn't
/// touch, so the finding must not suggest it.
#[test]
fn requested_checkpoints_without_the_wal_for_them_dont_blame_max_wal_size() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let mw = mem("1024", "MB", "sighup");
    let cct = Setting {
        setting: "0.9".into(),
        unit: None,
        context: "sighup".into(),
    };
    let cp = with_wal(cps(794, 587), 2_944_454_948, DAY);
    let f = checkpoint_finding(&c, &cp, Some(&mw), Some(&cct), &th).unwrap();
    assert_eq!(f.severity, Severity::Low);
    assert_eq!(
        f.title,
        "43% of checkpoints requested, but not by WAL volume"
    );
    assert!(f.suggestion.is_none(), "{:?}", f.suggestion);
    let a = advice(&f);
    assert!(a.change.is_none());
    assert!(
        f.detail.contains("at most about 5 of the 587"),
        "{}",
        f.detail
    );
    assert!(f.detail.contains("CREATE DATABASE"), "{}", f.detail);
    assert!(f.detail.contains("base backup"), "{}", f.detail);
    assert!(f.detail.contains("manual CHECKPOINT"), "{}", f.detail);
    assert!(
        f.detail.contains("Raising max_wal_size won't change those"),
        "{}",
        f.detail
    );
    assert!(
        a.evidence[2].contains("room for at most about 5 of the 587 requested"),
        "{:?}",
        a.evidence
    );
}

/// WAL for at least half the requested checkpoints blames max_wal_size;
/// a byte less, not. With a completion target of 1 a WAL-triggered
/// checkpoint starts every max_wal_size / 2.
#[test]
fn checkpoints_blame_wal_from_half_of_them() {
    let th = Thresholds::default();
    let mw = 1024 * MIB;
    assert_eq!(wal_trigger_bytes(mw, 1.0), 512.0 * MIB as f64);
    assert_eq!(wal_trigger_bytes(mw, 0.0), mw as f64);
    let half = with_wal(cps(80, 20), 5 * GIB, DAY);
    assert!(matches!(
        checkpoint_cause(&half, Some(mw), 1.0, &th),
        CheckpointCause::WalVolume { wal_forced } if wal_forced == 10.0
    ));
    let short = with_wal(cps(80, 20), 5 * GIB - 1, DAY);
    assert!(matches!(
        checkpoint_cause(&short, Some(mw), 1.0, &th),
        CheckpointCause::Other { .. }
    ));
}

/// pg_stat_wal reset apart from the checkpoint counters: the WAL rate
/// is carried over to their window, and the evidence says so.
#[test]
fn checkpoints_compare_wal_reset_apart_as_a_rate() {
    let th = Thresholds::default();
    let c = ctx(ServerKind::SelfHosted);
    let mw = mem("1024", "MB", "sighup");
    // 2.5 GiB in half a day is 5 GiB over the counters' day: room for 10
    // at a completion target of 1, against 20 requested.
    let cp = with_wal(cps(70, 20), 5 * GIB / 2, DAY / 2.0);
    let one = Setting {
        setting: "1".into(),
        unit: None,
        context: "sighup".into(),
    };
    assert!(matches!(
        checkpoint_cause(&cp, Some(GIB), 1.0, &th),
        CheckpointCause::WalVolume { wal_forced } if wal_forced == 10.0
    ));
    let f = checkpoint_finding(&c, &cp, Some(&mw), Some(&one), &th).unwrap();
    let a = advice(&f);
    assert!(
        a.evidence[1].contains("compared as rates, an estimate"),
        "{:?}",
        a.evidence
    );
    // Within a minute is the same window.
    let near = with_wal(cps(70, 20), 5 * GIB, DAY - 59.0);
    assert!(
        advice(&checkpoint_finding(&c, &near, Some(&mw), Some(&one), &th).unwrap()).evidence[1]
            .ends_with("over the same window")
    );
    // Reset too recently to say anything.
    let young = with_wal(cps(80, 20), 5 * GIB, 600.0);
    assert!(matches!(
        checkpoint_cause(&young, Some(GIB), 1.0, &th),
        CheckpointCause::Unknown(why) if why.contains("reset too recently")
    ));
}

/// Before PG14 there is no pg_stat_wal: pgman can't tell, so the finding
/// hedges — LOW, the causes listed, max_wal_size only if the log agrees.
#[test]
fn checkpoints_before_pg14_hedge() {
    let th = Thresholds::default();
    let mut c = ctx(ServerKind::SelfHosted);
    c.version_num = 130_011;
    let mw = mem("1024", "MB", "sighup");
    let mut cp = cps(79, 21);
    cp.wal = Err("pg_stat_wal, which counts the WAL written, is PG14+".into());
    let f = checkpoint_finding(&c, &cp, Some(&mw), None, &th).unwrap();
    assert_eq!(f.severity, Severity::Low);
    assert_eq!(f.title, "21% of checkpoints requested rather than timed");
    assert!(f.detail.contains("can't tell how many came from WAL volume (pg_stat_wal, which counts the WAL written, is PG14+)"), "{}", f.detail);
    assert!(
        f.detail.contains("checkpoint starting: wal"),
        "{}",
        f.detail
    );
    assert!(f.detail.contains("CREATE DATABASE"), "{}", f.detail);
    let a = advice(&f);
    assert!(
        !a.evidence_sql.contains("pg_stat_wal"),
        "{}",
        a.evidence_sql
    );
    assert_eq!(a.evidence.len(), 1, "{:?}", a.evidence);
    let ch = a.change.as_ref().unwrap();
    assert!(
        ch.reasoning.starts_with("double it only if the server log"),
        "{}",
        ch.reasoning
    );
    // max_wal_size unread: also can't tell.
    assert!(matches!(
        checkpoint_cause(&cps(79, 21), None, 0.9, &th),
        CheckpointCause::Unknown(_)
    ));
}

// --- the mapping -------------------------------------------------------

/// Every setting a finding can suggest has a row in the RDS table
/// (`mapping` would panic otherwise): drive each classifier down each
/// branch that names a setting.
#[test]
fn every_suggested_setting_has_an_rds_row() {
    for name in [
        "shared_buffers",
        "work_mem",
        "autovacuum",
        "autovacuum_vacuum_scale_factor",
        "idle_in_transaction_session_timeout",
        "lock_timeout",
        "max_wal_size",
    ] {
        assert!(rds::lookup(name).is_some(), "{name}");
    }
}
