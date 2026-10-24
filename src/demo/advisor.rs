//! The config advisor as the demo's server would answer it, built
//! through the real classifiers from fixed numbers: staging on RDS,
//! PG16, with the lock the sessions panel shows, the webhook table's
//! dead tuples, a reporting query that spills, and checkpoints forced
//! by the nightly load.

use crate::query::advisor::{
    self, Checkpoints, Context, DbStats, LockSession, Report, ServerKind, Setting, Spiller,
    Spillers, TableVacuum, Thresholds, VacuumDefaults, WalVolume, Window,
};

const MIB: i64 = 1024 * 1024;

fn setting(value: &str, unit: &str, context: &str) -> Setting {
    Setting {
        setting: value.into(),
        unit: (!unit.is_empty()).then(|| unit.to_string()),
        context: context.into(),
    }
}

pub fn advisor() -> Report {
    let th = Thresholds::default();
    let ctx = Context {
        version_num: 160_004,
        block_size: 8192,
        kind: ServerKind::Rds,
        in_recovery: false,
        can_read_all_stats: true,
        uptime_secs: 19.0 * 86_400.0,
        server_start: "2026-10-04 03:12:40+00".into(),
        database: "ledger".into(),
        has_pg_stat_statements: true,
    };
    let window = Window {
        reset_at: Some("2026-10-16 00:00:05+00".into()),
        reset_age_secs: Some(7.0 * 86_400.0 + 9.0 * 3600.0),
        uptime_secs: ctx.uptime_secs,
        server_start: ctx.server_start.clone(),
    };
    let db = DbStats {
        blks_hit: 1_904_221_870,
        blks_read: 61_337_412,
        temp_files: 2_318,
        temp_bytes: 187 * 1024 * MIB,
        window: window.clone(),
    };
    let spillers = Spillers::Known(vec![
        Spiller {
            query: "SELECT customer_id, sum(total) FROM invoices WHERE issued_at >= $1 GROUP BY customer_id ORDER BY 2 DESC".into(),
            calls: 1_412,
            temp_bytes: 1_412 * 18 * MIB,
            role: "ledger_report".into(),
        },
        Spiller {
            query: "SELECT * FROM webhook_deliveries WHERE status = $1 ORDER BY created_at".into(),
            calls: 96,
            temp_bytes: 96 * 41 * MIB,
            role: "billing".into(),
        },
    ]);
    let webhooks = TableVacuum {
        schema: "public".into(),
        table: "webhook_deliveries".into(),
        quoted: "public.webhook_deliveries".into(),
        live: 22_418_903,
        dead: 4_120_556,
        reloptions: Vec::new(),
        since_vacuum_secs: Some(31.0 * 3600.0),
    };
    let locks = vec![
        LockSession {
            pid: 31_877,
            user: "finance_ro".into(),
            state: "idle in transaction".into(),
            xact_secs: Some(412.0),
            idle_secs: Some(398.0),
            wait_secs: None,
            blockers: Vec::new(),
            query: "SELECT * FROM invoices WHERE customer_id = 2107 FOR UPDATE".into(),
        },
        LockSession {
            pid: 31_840,
            user: "billing".into(),
            state: "active".into(),
            xact_secs: Some(8.4),
            idle_secs: None,
            wait_secs: Some(8.2),
            blockers: vec![31_877],
            query: "UPDATE invoices SET status = 'paid', paid_at = now() WHERE id = 48207".into(),
        },
    ];
    // 260 GiB of WAL in the week: one WAL-triggered checkpoint every
    // 66 segments of 16MB (2GB / 1.9, less one) has room for ~252, so
    // the 187 requested are the load's.
    let checkpoints = Checkpoints {
        timed: 402,
        requested: 187,
        window: window.clone(),
        wal: Ok(WalVolume {
            bytes: 260 * 1024 * MIB,
            window,
            segment: 16 * MIB,
        }),
    };
    let defaults = VacuumDefaults {
        enabled: true,
        scale_factor: 0.2,
        threshold: 50,
    };
    let findings = [
        advisor::cache_hit_finding(
            &ctx,
            &db,
            Some(&setting("524288", "8kB", "postmaster")),
            &th,
        ),
        advisor::temp_spill_finding(
            &ctx,
            &db,
            Some(&setting("4096", "kB", "user")),
            &spillers,
            &th,
        ),
        advisor::dead_tuple_finding(&ctx, &webhooks, defaults, &th),
        advisor::lock_finding(
            &ctx,
            &locks,
            &advisor::Settings::from([
                // AWS's default parameter group value: 24h.
                (
                    "idle_in_transaction_session_timeout".to_string(),
                    setting("86400000", "ms", "user"),
                ),
                ("lock_timeout".to_string(), setting("0", "ms", "user")),
            ]),
            &th,
        ),
        advisor::checkpoint_finding(
            &ctx,
            &checkpoints,
            Some(&setting("2048", "MB", "sighup")),
            Some(&setting("0.9", "", "sighup")),
            &th,
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    Report {
        findings,
        errors: Vec::new(),
    }
}
