//! The config advisor: findings from the server's statistics, each with
//! the numbers that triggered it, a conservative setting to change, and
//! what that setting is called on RDS / Aurora. Shown in the schema
//! wizard (`W`) beside the lint findings. Design and thresholds:
//! `docs/backlog/config-advisor.md`.
//!
//! Read-only: every statement is a fixed `SELECT` over a statistics
//! view or `pg_settings`. Suggestions are text for the operator to yank;
//! pgman never runs them.
//!
//! The classifiers (`*_finding`) are pure over plain structs of numbers;
//! [`fetch`] runs each check on its own, so one the role can't read
//! doesn't stop the others.

pub mod rds;

use crate::query::lint::{Finding, Severity};
use std::collections::HashMap;

/// Tags the advisor's statements so the slow-queries panel (which skips
/// `/* pgman:%`) doesn't list them.
macro_rules! tag {
    () => {
        "/* pgman:advisor */ "
    };
}

/// Where the server runs, which decides how a suggestion is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerKind {
    SelfHosted,
    Rds,
    Aurora,
}

/// The thresholds a finding fires at. The defaults are the documented
/// ones; integration tests lower the window so a young cluster is seen.
#[derive(Debug, Clone, PartialEq)]
pub struct Thresholds {
    /// Below this much accumulation, the ratio checks stay quiet.
    pub min_window_secs: f64,
    /// Fewest `blks_hit + blks_read` for a hit ratio to mean anything.
    pub min_block_requests: i64,
    /// Fewest `temp_bytes` for a spill finding.
    pub min_temp_bytes: i64,
    /// Smallest table (live rows) the dead-tuple check looks at.
    pub min_live_rows: i64,
    /// Dead tuples as a fraction of live, from which a table is listed.
    pub min_dead_ratio: f64,
    /// Shortest lock wait that counts as a pile-up.
    pub min_lock_wait_secs: f64,
    /// Fewest checkpoints for the requested share to mean anything.
    pub min_checkpoints: i64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            min_window_secs: 3600.0,
            min_block_requests: 100_000,
            min_temp_bytes: 64 * MIB,
            min_live_rows: 10_000,
            min_dead_ratio: 0.10,
            min_lock_wait_secs: 5.0,
            min_checkpoints: 10,
        }
    }
}

const KIB: i64 = 1024;
const MIB: i64 = 1024 * 1024;
const GIB: i64 = 1024 * 1024 * 1024;

/// Hit ratio under which ADV001 fires (LOW), and under which it is MED.
pub const CACHE_HIT_LOW: f64 = 0.99;
pub const CACHE_HIT_MED: f64 = 0.95;
/// Spill rate from which ADV002 is MED rather than LOW.
pub const TEMP_MED_BYTES_PER_HOUR: i64 = 256 * MIB;
/// Requested share of checkpoints over which ADV005 fires.
pub const CHECKPOINT_REQ_SHARE: f64 = 0.20;
/// Waiters, or seconds waited, from which ADV004 is HIGH.
pub const LOCK_HIGH_WAITERS: usize = 5;
pub const LOCK_HIGH_WAIT_SECS: f64 = 60.0;
/// Table size from which ADV003 suggests the smaller scale factor.
pub const BIG_TABLE_ROWS: i64 = 10_000_000;

/// What a finding carries beyond a lint finding: the evidence and the
/// setting. Rendered by the wizard's detail strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advice {
    /// How long the numbers have accumulated, or "now" for a snapshot.
    pub window: String,
    /// The numbers, one line each.
    pub evidence: Vec<String>,
    /// The statement that produced them, runnable as it is (`e` yanks it).
    pub evidence_sql: String,
    pub change: Option<SettingChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChange {
    pub name: &'static str,
    /// The value now, as Postgres spells it (`128MB`), or `unknown`.
    pub current: String,
    pub suggested: String,
    pub reasoning: String,
    /// From the server's `pg_settings.context`, not the table.
    pub restart: bool,
    /// [`rds::describe`]'s line.
    pub rds: String,
}

/// One row of `pg_settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub setting: String,
    pub unit: Option<String>,
    pub context: String,
}

impl Setting {
    /// The value in bytes, for the memory settings.
    pub fn bytes(&self) -> Option<i64> {
        let n: i64 = self.setting.parse().ok()?;
        let mult = match self.unit.as_deref()? {
            "B" => 1,
            "kB" => KIB,
            "8kB" => 8 * KIB,
            "MB" => MIB,
            "16MB" => 16 * MIB,
            "GB" => GIB,
            _ => return None,
        };
        n.checked_mul(mult)
    }

    pub fn needs_restart(&self) -> bool {
        self.context == "postmaster"
    }

    /// The value in milliseconds, for the time settings.
    pub fn millis(&self) -> Option<i64> {
        let n: i64 = self.setting.parse().ok()?;
        let mult = match self.unit.as_deref()? {
            "ms" => 1,
            "s" => 1000,
            "min" => 60_000,
            _ => return None,
        };
        n.checked_mul(mult)
    }

    /// As `SHOW` would show it: sizes and times in their largest exact
    /// unit (`128MB`, `1d`, `5s`), zero and the negative "off" values
    /// bare (`0`, `-1`), anything else as `setting` plus its unit.
    pub fn display(&self) -> String {
        if self.setting.parse::<i64>().is_ok_and(|n| n <= 0) {
            return self.setting.clone();
        }
        if let Some(b) = self.bytes() {
            return pg_size(b);
        }
        if let Some(ms) = self.millis() {
            return pg_time(ms);
        }
        match &self.unit {
            Some(u) => format!("{}{u}", self.setting),
            None => self.setting.clone(),
        }
    }
}

pub type Settings = HashMap<String, Setting>;

/// A byte count in Postgres's size syntax, in the largest unit that
/// divides it exactly (`256MB`, `1GB`, `1536kB`).
pub fn pg_size(bytes: i64) -> String {
    if bytes != 0 && bytes % GIB == 0 {
        format!("{}GB", bytes / GIB)
    } else if bytes != 0 && bytes % MIB == 0 {
        format!("{}MB", bytes / MIB)
    } else {
        format!("{}kB", bytes / KIB)
    }
}

/// A positive millisecond count in Postgres's time syntax, in the
/// largest unit that divides it exactly (`1d`, `5min`, `1500ms`), as
/// `SHOW` spells it.
pub fn pg_time(ms: i64) -> String {
    for (unit, size) in [
        ("d", 86_400_000),
        ("h", 3_600_000),
        ("min", 60_000),
        ("s", 1000),
    ] {
        if ms != 0 && ms % size == 0 {
            return format!("{}{unit}", ms / size);
        }
    }
    format!("{ms}ms")
}

/// A byte count for a person: one decimal, binary units.
pub fn human_size(bytes: i64) -> String {
    crate::query::schema::format_bytes(bytes.max(0) as u64)
}

/// A span of seconds as the two largest units: `3d 4h`, `2h 10m`,
/// `12m`, `40s`; a zero second unit is left off (`1h`, not `1h 0m`).
pub fn format_span(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    if d > 0 && h > 0 {
        format!("{d}d {h}h")
    } else if d > 0 {
        format!("{d}d")
    } else if h > 0 && m > 0 {
        format!("{h}h {m}m")
    } else if h > 0 {
        format!("{h}h")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// How long a set of cumulative counters has accumulated.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// `stats_reset`, to the second; `None` when never reset.
    pub reset_at: Option<String>,
    pub reset_age_secs: Option<f64>,
    pub uptime_secs: f64,
    pub server_start: String,
}

impl Window {
    /// The accumulation span: since the reset, or (never reset) at least
    /// since the server started. PG15+ keeps statistics over a clean
    /// restart, so the second is a lower bound.
    pub fn secs(&self) -> f64 {
        self.reset_age_secs.unwrap_or(self.uptime_secs)
    }

    pub fn describe(&self) -> String {
        match (&self.reset_at, self.reset_age_secs) {
            (Some(at), Some(age)) => format!("counters since {at} ({})", format_span(age)),
            _ => format!(
                "counters never reset: at least {} (server started {})",
                format_span(self.uptime_secs),
                self.server_start
            ),
        }
    }

    fn young(&self, th: &Thresholds) -> bool {
        self.secs() < th.min_window_secs
    }
}

/// What the advisor needs to know about the server before it starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub version_num: i32,
    pub block_size: i64,
    pub kind: ServerKind,
    pub in_recovery: bool,
    pub can_read_all_stats: bool,
    pub uptime_secs: f64,
    pub server_start: String,
    pub database: String,
    pub has_pg_stat_statements: bool,
}

pub const CONTEXT_SQL: &str = concat!(
    tag!(),
    "SELECT current_setting('server_version_num')::int, \
     current_setting('block_size')::int, \
     current_setting('rds.extensions', true) IS NOT NULL, \
     to_regproc('aurora_version') IS NOT NULL, \
     pg_is_in_recovery(), \
     pg_has_role(current_user, 'pg_read_all_stats', 'MEMBER'), \
     EXTRACT(EPOCH FROM clock_timestamp() - pg_postmaster_start_time())::float8, \
     date_trunc('second', pg_postmaster_start_time())::text, \
     current_database()::text, \
     EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')"
);

/// Aurora also has `rds.extensions`, so Aurora wins.
pub fn server_kind(has_rds_extensions: bool, has_aurora_version: bool) -> ServerKind {
    if has_aurora_version {
        ServerKind::Aurora
    } else if has_rds_extensions {
        ServerKind::Rds
    } else {
        ServerKind::SelfHosted
    }
}

/// The settings the findings read: every row of the RDS table plus the
/// autovacuum threshold the dead-tuple check needs and the completion
/// target the checkpoint check does.
pub fn settings_sql() -> String {
    let mut names: Vec<&str> = rds::PARAMS.iter().map(|p| p.name).collect();
    names.push("autovacuum_vacuum_threshold");
    names.push("checkpoint_completion_target");
    let list = names
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}SELECT name, setting, unit, context FROM pg_settings WHERE name IN ({list}) ORDER BY name",
        tag!()
    )
}

pub const DB_STATS_SQL: &str = concat!(
    tag!(),
    "SELECT d.blks_hit, d.blks_read, d.temp_files, d.temp_bytes, \
     date_trunc('second', d.stats_reset)::text, \
     EXTRACT(EPOCH FROM clock_timestamp() - d.stats_reset)::float8 \
     FROM pg_stat_database d WHERE d.datname = current_database()"
);

pub const SPILLERS_SQL: &str = concat!(
    tag!(),
    "SELECT left(regexp_replace(s.query, '\\s+', ' ', 'g'), 200), s.calls, s.temp_blks_written, \
     COALESCE(quote_ident(pg_get_userbyid(s.userid)::text), '') \
     FROM pg_stat_statements s \
     WHERE s.temp_blks_written > 0 \
       AND s.dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
       AND s.query NOT LIKE '/* pgman:%' \
     ORDER BY s.temp_blks_written DESC LIMIT 3"
);

/// Tables at or over the dead-tuple floor, the worst first.
pub fn vacuum_sql(th: &Thresholds) -> String {
    format!(
        "{}SELECT s.schemaname::text, s.relname::text, \
         quote_ident(s.schemaname) || '.' || quote_ident(s.relname), \
         s.n_live_tup, s.n_dead_tup, COALESCE(c.reloptions, '{{}}'::text[]), \
         EXTRACT(EPOCH FROM clock_timestamp() - GREATEST(s.last_vacuum, s.last_autovacuum))::float8 \
         FROM pg_stat_user_tables s JOIN pg_class c ON c.oid = s.relid \
         WHERE s.n_live_tup >= {} AND s.n_dead_tup >= {} * s.n_live_tup \
         ORDER BY s.n_dead_tup DESC LIMIT 20",
        tag!(),
        th.min_live_rows,
        th.min_dead_ratio
    )
}

/// Every session that waits on a lock or holds one another waits on.
/// The wait's start is `pg_locks.waitstart` from PG14; before that the
/// best there is, the statement's start.
pub fn locks_sql(version_num: i32) -> String {
    let wait_start = if version_num >= 140_000 {
        "(SELECT min(l.waitstart) FROM pg_locks l WHERE l.pid = s.pid AND NOT l.granted)"
    } else {
        "s.query_start"
    };
    format!(
        "{}WITH s AS (SELECT a.pid, a.usename, a.state, a.xact_start, a.query_start, a.query, \
         pg_blocking_pids(a.pid) AS blockers FROM pg_stat_activity a WHERE a.pid <> pg_backend_pid()) \
         SELECT s.pid, COALESCE(quote_ident(s.usename::text), ''), COALESCE(s.state, ''), \
         EXTRACT(EPOCH FROM clock_timestamp() - s.xact_start)::float8, \
         CASE WHEN cardinality(s.blockers) > 0 THEN EXTRACT(EPOCH FROM clock_timestamp() - {wait_start})::float8 END, \
         s.blockers, COALESCE(left(regexp_replace(s.query, '\\s+', ' ', 'g'), 200), '') \
         FROM s WHERE cardinality(s.blockers) > 0 OR s.pid IN (SELECT unnest(blockers) FROM s) \
         ORDER BY s.pid",
        tag!()
    )
}

/// PG17 moved the checkpoint counters out of `pg_stat_bgwriter` into
/// `pg_stat_checkpointer`, with new names.
pub fn checkpoints_sql(version_num: i32) -> String {
    let (timed, req, view) = if version_num >= 170_000 {
        ("num_timed", "num_requested", "pg_stat_checkpointer")
    } else {
        ("checkpoints_timed", "checkpoints_req", "pg_stat_bgwriter")
    };
    format!(
        "{}SELECT {timed}, {req}, date_trunc('second', stats_reset)::text, \
         EXTRACT(EPOCH FROM clock_timestamp() - stats_reset)::float8 FROM {view}",
        tag!()
    )
}

fn note(title: &str, object: &str, detail: String, suggestion: Option<String>) -> Finding {
    Finding {
        severity: Severity::Low,
        code: "ADV000",
        title: title.to_string(),
        object: object.to_string(),
        detail,
        suggestion,
        advice: None,
    }
}

/// ADV000: the counters are too young for the ratio checks.
pub fn young_window_note(ctx: &Context, w: &Window, th: &Thresholds) -> Option<Finding> {
    if !w.young(th) {
        return None;
    }
    Some(note(
        "statistics too young for the ratio checks",
        &ctx.database,
        format!(
            "{} — the cache-hit, temp-file and checkpoint checks (ADV001, ADV002, ADV005) wait until {} have accumulated, so a fresh reset doesn't read as a problem",
            w.describe(),
            format_span(th.min_window_secs)
        ),
        None,
    ))
}

/// ADV000: a standby's statistics are its own.
pub fn standby_note(ctx: &Context) -> Finding {
    note(
        "this is a standby: its statistics are its own",
        &ctx.database,
        "a standby counts its own reads and has no vacuum or checkpoints of its own — the dead-tuple and checkpoint checks (ADV003, ADV005) are skipped; run the advisor on the primary for those".to_string(),
        None,
    )
}

/// The suggestion line for a server setting, in the form the server
/// takes it: `ALTER SYSTEM` self-hosted, the `aws` command on RDS /
/// Aurora (which refuse `ALTER SYSTEM`).
fn server_suggestion(
    kind: ServerKind,
    p: &rds::ParamMapping,
    pg_value: &str,
    group_value: &str,
    restart: bool,
) -> String {
    match kind {
        ServerKind::SelfHosted if restart => format!(
            "ALTER SYSTEM SET {} = '{pg_value}';  -- then restart the server",
            p.name
        ),
        ServerKind::SelfHosted => format!(
            "ALTER SYSTEM SET {} = '{pg_value}'; SELECT pg_reload_conf();",
            p.name
        ),
        ServerKind::Rds => rds::aws_cli(p, group_value, false),
        ServerKind::Aurora => rds::aws_cli(p, group_value, true),
    }
}

/// The parameter-table row for a name the advisor suggests. Every such
/// name has one (pinned by a test), so a miss is a programming error.
fn mapping(name: &str) -> &'static rds::ParamMapping {
    rds::lookup(name).unwrap_or_else(|| panic!("advisor: no RDS mapping for {name}"))
}

/// `restart` for a setting: the server's word when it gave one, else
/// the table's.
fn restart_of(setting: Option<&Setting>, p: &rds::ParamMapping) -> bool {
    setting.map(Setting::needs_restart).unwrap_or(p.is_static)
}

/// `pg_stat_database` for the current database.
#[derive(Debug, Clone, PartialEq)]
pub struct DbStats {
    pub blks_hit: i64,
    pub blks_read: i64,
    pub temp_files: i64,
    pub temp_bytes: i64,
    pub window: Window,
}

/// ADV001: buffer cache hit ratio below [`CACHE_HIT_LOW`].
pub fn cache_hit_finding(
    ctx: &Context,
    s: &DbStats,
    shared_buffers: Option<&Setting>,
    th: &Thresholds,
) -> Option<Finding> {
    if s.window.young(th) {
        return None;
    }
    let total = s.blks_hit.checked_add(s.blks_read)?;
    if total < th.min_block_requests || total <= 0 {
        return None;
    }
    let ratio = s.blks_hit as f64 / total as f64;
    if ratio >= CACHE_HIT_LOW {
        return None;
    }
    let severity = if ratio < CACHE_HIT_MED {
        Severity::Medium
    } else {
        Severity::Low
    };
    let p = mapping("shared_buffers");
    let current = shared_buffers.and_then(Setting::bytes);
    let restart = restart_of(shared_buffers, p);
    let (suggested, reasoning, suggestion) = match (ctx.kind, current) {
        (ServerKind::Aurora, _) => (
            shared_buffers.map(Setting::display).unwrap_or_else(|| "unchanged".into()),
            "leave it: Aurora's default already gives it about two-thirds of instance memory, so the working set is bigger than the instance — a larger instance class, or fewer pages per query (the slow-queries panel's top statements)".to_string(),
            None,
        ),
        // AWS's guidance is to keep RDS's ~25% default: the rest of the
        // instance's memory is the OS page cache Postgres reads through.
        (ServerKind::Rds, _) => (
            shared_buffers.map(Setting::display).unwrap_or_else(|| "unchanged".into()),
            "keep it near RDS's default, {DBInstanceClassMemory/32768} (about 25% of instance memory): AWS advises against raising it much, as Postgres also reads through the OS page cache, which needs the rest. If the parameter group lowered it, putting the default back is the change to make; otherwise the working set is bigger than the instance — a larger instance class, or fewer pages per query (the slow-queries panel's top statements)".to_string(),
            None,
        ),
        (kind, Some(cur)) => {
            let next = cur.saturating_mul(2);
            let group = p
                .unit
                .from_bytes(next)
                .map(|v| v.to_string())
                .unwrap_or_default();
            (
                pg_size(next),
                "double it, staying at or under about 25% of the machine's memory (pgman can't see the RAM); the rest is the OS page cache, which Postgres also reads through".to_string(),
                Some(server_suggestion(kind, p, &pg_size(next), &group, restart)),
            )
        }
        (_, None) => (
            "unknown".into(),
            "the current value couldn't be read".to_string(),
            None,
        ),
    };
    Some(Finding {
        severity,
        code: "ADV001",
        title: format!("buffer cache hit ratio {ratio:.3} (below {CACHE_HIT_LOW})"),
        object: ctx.database.clone(),
        detail: format!(
            "{:.1}% of block requests in `{}` found the page in shared_buffers; the rest went to the OS page cache or the disk. A read from the OS cache is still fast, so this is a hint to look, not proof of a problem",
            ratio * 100.0,
            ctx.database
        ),
        suggestion,
        advice: Some(Box::new(Advice {
            window: s.window.describe(),
            evidence: vec![format!(
                "blks_hit {} · blks_read {} · ratio {ratio:.4}",
                s.blks_hit, s.blks_read
            )],
            evidence_sql: DB_STATS_SQL.to_string(),
            change: Some(SettingChange {
                name: p.name,
                current: shared_buffers
                    .map(Setting::display)
                    .unwrap_or_else(|| "unknown".into()),
                suggested,
                reasoning,
                restart,
                rds: rds::describe(p, ctx.kind == ServerKind::Aurora),
            }),
        })),
    })
}

/// The query text `pg_stat_statements` and `pg_stat_activity` show for
/// another role's statement without `pg_read_all_stats`.
pub const INSUFFICIENT_PRIVILEGE: &str = "<insufficient privilege>";

/// A statement `pg_stat_statements` says spilled to temp files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spiller {
    pub query: String,
    pub calls: i64,
    pub temp_bytes: i64,
    /// Quoted for SQL; empty when unknown.
    pub role: String,
}

impl Spiller {
    pub fn per_call(&self) -> i64 {
        self.temp_bytes / self.calls.max(1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spillers {
    Known(Vec<Spiller>),
    /// Why the statements can't be named.
    Unavailable(String),
}

/// The `work_mem` to suggest and why. Twice the current; when the
/// worst per-call spill fits under 4× the current, the next power of
/// two above it, within 2×–4×.
pub fn suggest_work_mem(current: i64, worst_per_call: Option<i64>) -> (i64, String) {
    let lo = current.saturating_mul(2);
    let hi = current.saturating_mul(4);
    match worst_per_call {
        Some(p) if p < hi => {
            let fit = (p.max(1) as u64).next_power_of_two() as i64;
            (
                fit.clamp(lo, hi),
                format!(
                    "the worst statement spills {} a call; this is a first step, not a fit — sorting it in memory takes more than the bytes it spilled, and temp_blks_written per call sums every sort and hash node and parallel worker; watch the spills after",
                    human_size(p)
                ),
            )
        }
        Some(p) => (
            lo,
            format!(
                "the worst statement spills {} a call, more than 4× work_mem: twice helps the smaller spills, but no safe work_mem covers that one — look at the statement (an index, or less data per sort)",
                human_size(p)
            ),
        ),
        None => (
            lo,
            "twice the current, as a first step; pg_stat_statements would show how much each statement spills".to_string(),
        ),
    }
}

/// ADV002: queries spill sorts and hashes to temp files.
pub fn temp_spill_finding(
    ctx: &Context,
    s: &DbStats,
    work_mem: Option<&Setting>,
    spillers: &Spillers,
    th: &Thresholds,
) -> Option<Finding> {
    if s.window.young(th) || s.temp_bytes < th.min_temp_bytes {
        return None;
    }
    let hours = (s.window.secs() / 3600.0).max(1.0 / 60.0);
    let per_hour = (s.temp_bytes as f64 / hours) as i64;
    let severity = if per_hour >= TEMP_MED_BYTES_PER_HOUR {
        Severity::Medium
    } else {
        Severity::Low
    };
    let p = mapping("work_mem");
    let current = work_mem.and_then(Setting::bytes).unwrap_or(4 * MIB);
    let mut evidence = vec![format!(
        "temp_files {} · temp_bytes {} ({} written, {} an hour)",
        s.temp_files,
        s.temp_bytes,
        human_size(s.temp_bytes),
        human_size(per_hour)
    )];
    let (worst, role) = match spillers {
        Spillers::Known(list) => {
            for sp in list {
                evidence.push(format!(
                    "{} a call × {} calls: {}",
                    human_size(sp.per_call()),
                    sp.calls,
                    crate::grid::truncate_cell(&sp.query, 120)
                ));
            }
            if list.iter().any(|sp| sp.query == INSUFFICIENT_PRIVILEGE) {
                evidence.push(format!(
                    "{INSUFFICIENT_PRIVILEGE}: pg_stat_statements shows other roles' statements only to pg_read_all_stats: GRANT pg_monitor TO <your role>"
                ));
            }
            let worst = list.iter().max_by_key(|sp| sp.per_call());
            (
                worst.map(Spiller::per_call),
                worst.map(|sp| sp.role.clone()).filter(|r| !r.is_empty()),
            )
        }
        Spillers::Unavailable(why) => {
            evidence.push(format!("which statements: {why}"));
            (None, None)
        }
    };
    let (next, reasoning) = suggest_work_mem(current, worst);
    let role = role.unwrap_or_else(|| "<app_role>".to_string());
    let suggestion = format!("ALTER ROLE {role} SET work_mem = '{}';", pg_size(next));
    Some(Finding {
        severity,
        code: "ADV002",
        title: format!("queries spill to temp files ({} so far)", human_size(s.temp_bytes)),
        object: ctx.database.clone(),
        detail: format!(
            "sorts and hashes bigger than work_mem ({}) write temp files. work_mem is per sort node per connection, so raise it for the role that spills rather than server-wide. ALTER ROLE … SET reaches new sessions only: a connection pool keeps its connections until they are recycled (HikariCP's maxLifetime, 30 min by default) or the application restarts{}",
            pg_size(current),
            if matches!(spillers, Spillers::Unavailable(_)) {
                "; log_temp_files = 0 logs each spilling statement"
            } else {
                ""
            }
        ),
        suggestion: Some(suggestion),
        advice: Some(Box::new(Advice {
            window: s.window.describe(),
            evidence,
            evidence_sql: match spillers {
                Spillers::Known(_) => format!("{DB_STATS_SQL};\n{SPILLERS_SQL}"),
                Spillers::Unavailable(_) => DB_STATS_SQL.to_string(),
            },
            change: Some(SettingChange {
                name: p.name,
                current: match work_mem {
                    Some(_) => this_session(work_mem),
                    None => format!("{} (the default; not read)", pg_size(current)),
                },
                suggested: pg_size(next),
                reasoning,
                restart: restart_of(work_mem, p),
                rds: format!(
                    "per role: ALTER ROLE works the same on RDS and Aurora · server-wide: {}",
                    rds::describe(p, ctx.kind == ServerKind::Aurora)
                ),
            }),
        })),
    })
}

/// One `pg_stat_user_tables` row over the dead-tuple floor.
#[derive(Debug, Clone, PartialEq)]
pub struct TableVacuum {
    pub schema: String,
    pub table: String,
    /// `schema.table`, quoted for SQL.
    pub quoted: String,
    pub live: i64,
    pub dead: i64,
    /// `pg_class.reloptions`, `name=value` each.
    pub reloptions: Vec<String>,
    /// Since the last vacuum of either kind; `None` when never.
    pub since_vacuum_secs: Option<f64>,
}

/// The server-wide autovacuum settings a table falls back to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VacuumDefaults {
    pub enabled: bool,
    pub scale_factor: f64,
    pub threshold: i64,
}

impl VacuumDefaults {
    pub fn from_settings(settings: &Settings) -> Self {
        let get = |n: &str| settings.get(n).map(|s| s.setting.as_str());
        VacuumDefaults {
            enabled: get("autovacuum") != Some("off"),
            scale_factor: get("autovacuum_vacuum_scale_factor")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.2),
            threshold: get("autovacuum_vacuum_threshold")
                .and_then(|v| v.parse().ok())
                .unwrap_or(50),
        }
    }
}

/// The autovacuum storage parameters a table sets for itself.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TableAutovacuum {
    pub enabled: Option<bool>,
    pub scale_factor: Option<f64>,
    pub threshold: Option<i64>,
}

pub fn parse_reloptions(opts: &[String]) -> TableAutovacuum {
    let mut out = TableAutovacuum::default();
    for o in opts {
        let Some((k, v)) = o.split_once('=') else {
            continue;
        };
        match k {
            "autovacuum_enabled" => {
                out.enabled = match v.to_ascii_lowercase().as_str() {
                    "true" | "on" | "1" | "yes" => Some(true),
                    "false" | "off" | "0" | "no" => Some(false),
                    _ => None,
                }
            }
            "autovacuum_vacuum_scale_factor" => out.scale_factor = v.parse().ok(),
            "autovacuum_vacuum_threshold" => out.threshold = v.parse().ok(),
            _ => {}
        }
    }
    out
}

/// Why a table has the dead tuples it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VacuumCause {
    /// Autovacuum is off for this table.
    TableDisabled,
    /// Autovacuum is off server-wide.
    ServerDisabled,
    /// Past its own trigger: autovacuum should have run and hasn't.
    Behind,
    /// Under its trigger: the scale factor allows this many.
    ScaleAllows,
}

/// The scale factor ADV003 suggests for a table of `live` rows.
pub fn suggested_scale_factor(live: i64) -> f64 {
    if live >= BIG_TABLE_ROWS {
        0.01
    } else {
        0.05
    }
}

/// ADV003: dead tuples at or over the floor, with the autovacuum
/// setting that let them get there.
pub fn dead_tuple_finding(
    ctx: &Context,
    t: &TableVacuum,
    defaults: VacuumDefaults,
    th: &Thresholds,
) -> Option<Finding> {
    if t.live < th.min_live_rows || (t.dead as f64) < th.min_dead_ratio * t.live as f64 {
        return None;
    }
    let own = parse_reloptions(&t.reloptions);
    let scale = own.scale_factor.unwrap_or(defaults.scale_factor);
    let threshold = own.threshold.unwrap_or(defaults.threshold);
    let trigger = threshold as f64 + scale * t.live as f64;
    let cause = if own.enabled == Some(false) {
        VacuumCause::TableDisabled
    } else if !defaults.enabled {
        VacuumCause::ServerDisabled
    } else if t.dead as f64 > trigger {
        VacuumCause::Behind
    } else {
        VacuumCause::ScaleAllows
    };
    let ratio = t.dead as f64 / t.live.max(1) as f64;
    let severity = if matches!(
        cause,
        VacuumCause::TableDisabled | VacuumCause::ServerDisabled
    ) || ratio >= 0.20
    {
        Severity::Medium
    } else {
        Severity::Low
    };
    let target = suggested_scale_factor(t.live);
    let lower_scale = scale > target;
    let set_scale = format!(
        "ALTER TABLE {} SET (autovacuum_vacuum_scale_factor = {target});",
        t.quoted
    );
    let auto = mapping("autovacuum");
    let sf = mapping("autovacuum_vacuum_scale_factor");
    let (why, suggestion, change) = match cause {
        VacuumCause::TableDisabled => (
            "autovacuum is switched off for this table (autovacuum_enabled = false), so nothing reclaims them".to_string(),
            Some(format!("ALTER TABLE {} RESET (autovacuum_enabled);", t.quoted)),
            Some(SettingChange {
                name: "autovacuum_enabled",
                current: "false".into(),
                suggested: "the default (true)".into(),
                reasoning: "nothing vacuums this table while it is off; a bulk load that turned it off for speed should turn it back on".into(),
                restart: false,
                rds: "a table storage parameter: no parameter group, applies at once, the same on RDS and Aurora".into(),
            }),
        ),
        VacuumCause::ServerDisabled => (
            "autovacuum is off server-wide, so nothing reclaims them (and nothing protects against transaction-id wraparound short of the emergency vacuum)".to_string(),
            Some(server_suggestion(ctx.kind, auto, "on", "1", false)),
            Some(SettingChange {
                name: auto.name,
                current: "off".into(),
                suggested: "on".into(),
                reasoning: "autovacuum keeps dead tuples and wraparound in check; turn tables off one by one if you must".into(),
                restart: false,
                rds: rds::describe(auto, ctx.kind == ServerKind::Aurora),
            }),
        ),
        VacuumCause::Behind => (
            format!(
                "that is past the point autovacuum should have run ({} dead), so it is behind: a long transaction holding back the xmin horizon (the sessions panel, `L`) or workers busy elsewhere",
                trigger as i64
            ),
            lower_scale.then(|| set_scale.clone()),
            None,
        ),
        VacuumCause::ScaleAllows => (
            format!(
                "autovacuum is doing what it is set to: it waits for {} dead ({threshold} + {scale} × live) before it runs",
                trigger as i64
            ),
            lower_scale.then(|| set_scale.clone()),
            None,
        ),
    };
    let change = change.or_else(|| {
        lower_scale.then(|| SettingChange {
            name: sf.name,
            current: scale.to_string(),
            suggested: target.to_string(),
            reasoning: format!(
                "as a storage parameter on this table only: vacuum after {:.0}% churn instead of {:.0}%",
                target * 100.0,
                scale * 100.0
            ),
            restart: false,
            rds: format!(
                "a table storage parameter: no parameter group, applies at once, the same on RDS and Aurora · the server-wide default: {}",
                rds::describe(sf, ctx.kind == ServerKind::Aurora)
            ),
        })
    });
    let last = match t.since_vacuum_secs {
        Some(s) => format!("last vacuumed {} ago", format_span(s)),
        None => "never vacuumed since the stats reset".to_string(),
    };
    Some(Finding {
        severity,
        code: "ADV003",
        title: format!("dead tuples {:.0}% of live; {}", ratio * 100.0, cause_label(cause)),
        object: format!("{}.{}", t.schema, t.table),
        detail: format!(
            "`{}.{}` has {} dead tuples against {} live — {why}. LINT104's VACUUM reclaims them now; this keeps them from coming back",
            t.schema, t.table, t.dead, t.live
        ),
        suggestion,
        advice: Some(Box::new(Advice {
            window: format!("current counts · {last}"),
            evidence: vec![
                format!("n_dead_tup {} · n_live_tup {} · ratio {ratio:.2}", t.dead, t.live),
                format!(
                    "autovacuum runs at {} dead (threshold {threshold} + scale factor {scale} × live){}",
                    trigger as i64,
                    if t.reloptions.is_empty() {
                        String::new()
                    } else {
                        format!(" · table options: {}", t.reloptions.join(", "))
                    }
                ),
            ],
            evidence_sql: vacuum_sql(th),
            change,
        })),
    })
}

fn cause_label(c: VacuumCause) -> &'static str {
    match c {
        VacuumCause::TableDisabled => "autovacuum off for the table",
        VacuumCause::ServerDisabled => "autovacuum off server-wide",
        VacuumCause::Behind => "autovacuum is behind",
        VacuumCause::ScaleAllows => "its scale factor allows it",
    }
}

/// A session in a lock chain: waiting, holding, or both.
#[derive(Debug, Clone, PartialEq)]
pub struct LockSession {
    pub pid: i32,
    /// Quoted for SQL; empty when the role can't see it.
    pub user: String,
    pub state: String,
    pub xact_secs: Option<f64>,
    /// How long it has waited; `None` when it isn't waiting.
    pub wait_secs: Option<f64>,
    pub blockers: Vec<i32>,
    pub query: String,
}

/// A session at the head of a chain and every session waiting on it,
/// directly or through another waiter.
#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub root: i32,
    pub waiters: Vec<i32>,
}

/// The heads of the lock chains, most waiters first. A head blocks
/// someone and waits on no one (or on a session not in the list); a
/// cycle (a deadlock about to be broken) is walked once and has none.
pub fn blocking_chains(sessions: &[LockSession]) -> Vec<Chain> {
    let by_pid: HashMap<i32, &LockSession> = sessions.iter().map(|s| (s.pid, s)).collect();
    let mut chains: HashMap<i32, Vec<i32>> = HashMap::new();
    for w in sessions.iter().filter(|s| !s.blockers.is_empty()) {
        let mut seen = std::collections::HashSet::new();
        let mut stack: Vec<i32> = w.blockers.clone();
        let mut roots = std::collections::BTreeSet::new();
        while let Some(pid) = stack.pop() {
            if !seen.insert(pid) {
                continue;
            }
            match by_pid.get(&pid) {
                Some(s) if !s.blockers.is_empty() => stack.extend(&s.blockers),
                _ => {
                    roots.insert(pid);
                }
            }
        }
        for r in roots {
            if r != w.pid {
                chains.entry(r).or_default().push(w.pid);
            }
        }
    }
    let mut out: Vec<Chain> = chains
        .into_iter()
        .map(|(root, mut waiters)| {
            waiters.sort_unstable();
            Chain { root, waiters }
        })
        .collect();
    out.sort_by(|a, b| {
        b.waiters
            .len()
            .cmp(&a.waiters.len())
            .then(a.root.cmp(&b.root))
    });
    out
}

/// A per-session setting's value as pgman's own session sees it. A
/// role or database can set it differently for the application, and
/// `pg_settings` can't show another session's, so the value says
/// whose it is.
pub fn this_session(setting: Option<&Setting>) -> String {
    match setting {
        Some(s) => format!("{} (this session)", s.display()),
        None => "unknown".to_string(),
    }
}

/// An idle-in-transaction timeout longer than this ends nothing a
/// lock pile-up is waiting on. AWS's default parameter groups set 24h.
pub const IDLE_TX_TIMEOUT_MAX_MS: i64 = 3_600_000;

/// Whether `idle_in_transaction_session_timeout` is, for a pile-up
/// behind a session idle in a transaction, as good as off: zero or
/// unread, over [`IDLE_TX_TIMEOUT_MAX_MS`], or longer than the head's
/// transaction has run (so it hasn't ended it, and the waiters wait).
pub fn idle_timeout_effectively_off(
    timeout: Option<&Setting>,
    head_xact_secs: Option<f64>,
) -> bool {
    let Some(s) = timeout else {
        return true;
    };
    match s.millis() {
        Some(ms) => {
            ms <= 0
                || ms > IDLE_TX_TIMEOUT_MAX_MS
                || head_xact_secs.is_some_and(|x| ms as f64 / 1000.0 > x)
        }
        None => s.setting == "0",
    }
}

/// The reasoning's word on a timeout that is set but too long to help:
/// its value, in hours when it is a day or more.
fn idle_timeout_note(timeout: Option<&Setting>) -> String {
    let Some(ms) = timeout.and_then(Setting::millis).filter(|ms| *ms > 0) else {
        return String::new();
    };
    let hours = if ms >= 86_400_000 && ms % 3_600_000 == 0 {
        format!(" = {}h", ms / 3_600_000)
    } else {
        String::new()
    };
    let why = if ms > IDLE_TX_TIMEOUT_MAX_MS {
        "far longer than a pile-up can wait (AWS's default parameter groups set 24h)"
    } else {
        "longer than the head's transaction has run, so it hasn't ended it"
    };
    format!(
        ". It is set, to {}{hours}, but that is {why}",
        timeout.map(Setting::display).unwrap_or_default()
    )
}

/// What a `lock_timeout` does to the statements it covers, said
/// wherever ADV004 brings it up.
const LOCK_TIMEOUT_CAVEAT: &str = "For a migration or other DDL, SET lock_timeout = '5s' in its session: the ALTER fails after 5s instead of queueing behind the head with everyone else queueing behind it, and the migration can be re-run. Not per role for the application: every statement of that role waiting over 5s would fail with an error — Flyway / Liquibase migrations run as it included — and Java applications generally don't retry. deadlock_timeout doesn't help a pile-up: it only decides when the deadlock check runs";

/// ADV004: sessions waiting on locks for at least the threshold.
pub fn lock_finding(
    ctx: &Context,
    sessions: &[LockSession],
    settings: &Settings,
    th: &Thresholds,
) -> Option<Finding> {
    let waiters: Vec<&LockSession> = sessions.iter().filter(|s| !s.blockers.is_empty()).collect();
    let longest = waiters
        .iter()
        .filter_map(|s| s.wait_secs)
        .fold(0.0_f64, f64::max);
    if waiters.is_empty() || longest < th.min_lock_wait_secs {
        return None;
    }
    let severity = if waiters.len() >= LOCK_HIGH_WAITERS || longest >= LOCK_HIGH_WAIT_SECS {
        Severity::High
    } else {
        Severity::Medium
    };
    let chains = blocking_chains(sessions);
    let by_pid: HashMap<i32, &LockSession> = sessions.iter().map(|s| (s.pid, s)).collect();
    let mut evidence = Vec::new();
    for c in chains.iter().take(3) {
        let head = by_pid.get(&c.root);
        let who = match head {
            Some(h) => format!(
                "{}, {}{}",
                if h.user.is_empty() { "?" } else { &h.user },
                if h.state.is_empty() {
                    "state hidden"
                } else {
                    &h.state
                },
                h.xact_secs
                    .map(|x| format!(", in a transaction {}", format_span(x)))
                    .unwrap_or_default()
            ),
            None => "not a client session".to_string(),
        };
        let waits = c
            .waiters
            .iter()
            .map(|p| match by_pid.get(p).and_then(|s| s.wait_secs) {
                Some(w) => format!("{p} ({})", format_span(w)),
                None => p.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        evidence.push(format!(
            "pid {} ({who}) blocks {}: {waits}",
            c.root,
            c.waiters.len()
        ));
        if let Some(h) = head.filter(|h| !h.query.is_empty()) {
            evidence.push(format!(
                "  pid {} last ran: {}",
                c.root,
                crate::grid::truncate_cell(&h.query, 120)
            ));
        }
    }
    if !ctx.can_read_all_stats {
        evidence.push(
            "other roles' sessions show no state or query without pg_read_all_stats: GRANT pg_monitor TO <your role>".to_string(),
        );
    }
    let head = chains.first().and_then(|c| by_pid.get(&c.root)).copied();
    let idle_in_tx = head.is_some_and(|h| h.state.starts_with("idle in transaction"));
    let idle_tx_timeout = settings.get("idle_in_transaction_session_timeout");
    let timeout_off = idle_timeout_effectively_off(idle_tx_timeout, head.and_then(|h| h.xact_secs));
    let (suggestion, change) = if idle_in_tx && timeout_off {
        let p = mapping("idle_in_transaction_session_timeout");
        let role = head.map(|h| h.user.as_str()).filter(|u| !u.is_empty());
        let suggestion = match role {
            Some(r) => format!("ALTER ROLE {r} SET idle_in_transaction_session_timeout = '5min';"),
            None => server_suggestion(ctx.kind, p, "5min", "300000", false),
        };
        (
            suggestion,
            SettingChange {
                name: p.name,
                current: this_session(idle_tx_timeout),
                suggested: "5min".into(),
                reasoning: format!(
                    "the head of the chain sits idle in a transaction, holding its locks: the timeout ends such a session (its transaction rolls back) — for the role the application connects as{}",
                    idle_timeout_note(idle_tx_timeout)
                ),
                restart: false,
                rds: format!(
                    "per role: the same on RDS and Aurora · server-wide: {}",
                    rds::describe(p, ctx.kind == ServerKind::Aurora)
                ),
            },
        )
    } else if settings
        .get("log_lock_waits")
        .is_none_or(|s| s.setting != "on")
    {
        // Logging the waits first: it changes no session's behaviour,
        // where a lock_timeout makes statements fail.
        let p = mapping("log_lock_waits");
        let current = settings.get("log_lock_waits");
        (
            server_suggestion(ctx.kind, p, "on", "1", restart_of(current, p)),
            SettingChange {
                name: p.name,
                current: current
                    .map(Setting::display)
                    .unwrap_or_else(|| "unknown".into()),
                suggested: "on".into(),
                reasoning: format!(
                    "first see who waits on whom: it logs every lock wait longer than deadlock_timeout with the holder and the statement, and changes no session's behaviour. {LOCK_TIMEOUT_CAVEAT}"
                ),
                restart: restart_of(current, p),
                rds: rds::describe(p, ctx.kind == ServerKind::Aurora),
            },
        )
    } else {
        let p = mapping("lock_timeout");
        (
            "SET lock_timeout = '5s';  -- in the migration / DDL session only: its statement then fails after 5s instead of queueing everyone behind it. Not ALTER ROLE for the application's role: every statement of that role waiting over 5s would fail, its Flyway / Liquibase migrations included".to_string(),
            SettingChange {
                name: p.name,
                current: this_session(settings.get("lock_timeout")),
                suggested: "5s, in the migration session".into(),
                reasoning: format!(
                    "log_lock_waits is already on, so the server log has the waits. {LOCK_TIMEOUT_CAVEAT}"
                ),
                restart: false,
                rds: format!(
                    "per session: SET works the same on RDS and Aurora · server-wide: {}",
                    rds::describe(p, ctx.kind == ServerKind::Aurora)
                ),
            },
        )
    };
    let root = chains.first().map(|c| c.root);
    Some(Finding {
        severity,
        code: "ADV004",
        title: format!(
            "{} session(s) waiting on locks, longest {}",
            waiters.len(),
            format_span(longest)
        ),
        object: root
            .map(|r| format!("pid {r}"))
            .unwrap_or_else(|| ctx.database.clone()),
        detail: format!(
            "{} — the sessions panel (`L`) shows them and can terminate the head",
            match head {
                Some(h) if idle_in_tx => format!(
                    "pid {} holds the locks while idle in a transaction: something opened a transaction and stopped talking",
                    h.pid
                ),
                _ => "the head of the chain is running or not visible to this role; the waiters queue behind it".to_string(),
            }
        ),
        suggestion: Some(suggestion),
        advice: Some(Box::new(Advice {
            window: "a snapshot, now".to_string(),
            evidence,
            evidence_sql: locks_sql(ctx.version_num),
            change: Some(change),
        })),
    })
}

/// The checkpoint counters, and the WAL written over (about) the same
/// window.
#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoints {
    pub timed: i64,
    pub requested: i64,
    pub window: Window,
    /// `pg_stat_wal` (PG14+); `Err` says why there is none.
    pub wal: Result<WalVolume, String>,
}

/// `pg_stat_wal.wal_bytes` and its own reset, which
/// `pg_stat_reset_shared('wal')` moves apart from the checkpoint
/// counters'.
#[derive(Debug, Clone, PartialEq)]
pub struct WalVolume {
    pub bytes: i64,
    pub window: Window,
}

pub const WAL_SQL: &str = concat!(
    tag!(),
    "SELECT wal_bytes::bigint, date_trunc('second', stats_reset)::text, \
     EXTRACT(EPOCH FROM clock_timestamp() - stats_reset)::float8 FROM pg_stat_wal"
);

/// Why ADV005's checkpoints were requested, as far as the WAL tells.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckpointCause {
    /// The WAL written could have forced at least half of them:
    /// `max_wal_size` is what spaces them out.
    WalVolume { wal_forced: f64 },
    /// Far too little WAL for them: something else asked.
    Other { wal_forced: f64 },
    /// pgman can't tell (before PG14, `pg_stat_wal` unreadable or too
    /// newly reset, `max_wal_size` unknown).
    Unknown(String),
}

/// Two counter windows that start within this many seconds of each
/// other are the same window.
const SAME_WINDOW_SECS: f64 = 60.0;

/// The WAL a WAL-triggered checkpoint starts after: Postgres starts one
/// when the WAL since the last reaches
/// `max_wal_size / (1 + checkpoint_completion_target)`.
pub fn wal_trigger_bytes(max_wal_size: i64, completion_target: f64) -> f64 {
    max_wal_size as f64 / (1.0 + completion_target.clamp(0.0, 1.0))
}

/// How many of the requested checkpoints the WAL could have forced,
/// and so whether `max_wal_size` is the setting to blame. An upper
/// bound: it counts every byte as if no timed checkpoint used any.
/// When `pg_stat_wal` was reset apart from the checkpoint counters,
/// the WAL rate is carried over to the checkpoints' window.
pub fn checkpoint_cause(
    c: &Checkpoints,
    max_wal_size: Option<i64>,
    completion_target: f64,
    th: &Thresholds,
) -> CheckpointCause {
    let wal = match &c.wal {
        Ok(w) => w,
        Err(why) => return CheckpointCause::Unknown(why.clone()),
    };
    let Some(mws) = max_wal_size.filter(|b| *b > 0) else {
        return CheckpointCause::Unknown("max_wal_size couldn't be read".into());
    };
    if wal.window.young(th) {
        return CheckpointCause::Unknown(format!(
            "pg_stat_wal was reset too recently to compare ({})",
            wal.window.describe()
        ));
    }
    let scale = if same_window(&wal.window, &c.window) {
        1.0
    } else {
        c.window.secs() / wal.window.secs().max(1.0)
    };
    let wal_forced = wal.bytes as f64 * scale / wal_trigger_bytes(mws, completion_target);
    if wal_forced * 2.0 >= c.requested as f64 {
        CheckpointCause::WalVolume { wal_forced }
    } else {
        CheckpointCause::Other { wal_forced }
    }
}

fn same_window(a: &Window, b: &Window) -> bool {
    match (a.reset_age_secs, b.reset_age_secs) {
        (Some(x), Some(y)) => (x - y).abs() <= SAME_WINDOW_SECS,
        (None, None) => true,
        _ => false,
    }
}

/// What asks for a checkpoint besides WAL volume.
const OTHER_CHECKPOINT_CAUSES: &str = "CREATE DATABASE and DROP DATABASE (and ALTER DATABASE … SET TABLESPACE), a base backup (pg_basebackup, pg_backup_start), a manual CHECKPOINT, a shutdown or restart";

/// ADV005: a large share of checkpoints requested rather than timed,
/// blamed on `max_wal_size` only when the WAL written could have forced
/// them.
pub fn checkpoint_finding(
    ctx: &Context,
    c: &Checkpoints,
    max_wal_size: Option<&Setting>,
    completion_target: Option<&Setting>,
    th: &Thresholds,
) -> Option<Finding> {
    if ctx.kind == ServerKind::Aurora || c.window.young(th) {
        return None;
    }
    let total = c.timed.checked_add(c.requested)?;
    if total < th.min_checkpoints || total <= 0 {
        return None;
    }
    let share = c.requested as f64 / total as f64;
    if share <= CHECKPOINT_REQ_SHARE {
        return None;
    }
    let p = mapping("max_wal_size");
    let current = max_wal_size.and_then(Setting::bytes);
    let cct = completion_target
        .and_then(|s| s.setting.parse::<f64>().ok())
        .unwrap_or(0.9);
    let cause = checkpoint_cause(c, current, cct, th);
    let restart = restart_of(max_wal_size, p);
    let view = if ctx.version_num >= 170_000 {
        "pg_stat_checkpointer"
    } else {
        "pg_stat_bgwriter"
    };
    let mut evidence = vec![format!(
        "{view}: {} requested · {} timed",
        c.requested, c.timed
    )];
    if let Ok(w) = &c.wal {
        evidence.push(format!(
            "pg_stat_wal: {} written{}",
            human_size(w.bytes),
            if same_window(&w.window, &c.window) {
                " over the same window".to_string()
            } else {
                format!(
                    " — {}, not the checkpoint counters' window: compared as rates, an estimate",
                    w.window.describe()
                )
            }
        ));
    }
    if let (
        Some(mws),
        CheckpointCause::WalVolume { wal_forced } | CheckpointCause::Other { wal_forced },
    ) = (current, &cause)
    {
        evidence.push(format!(
            "a WAL-triggered checkpoint starts every {} (max_wal_size {} / (1 + checkpoint_completion_target {cct})): room for at most about {:.0} of the {} requested",
            human_size(wal_trigger_bytes(mws, cct) as i64),
            pg_size(mws),
            wal_forced.floor(),
            c.requested
        ));
    }
    let evidence_sql = if ctx.version_num >= 140_000 {
        format!("{};\n{WAL_SQL}", checkpoints_sql(ctx.version_num))
    } else {
        checkpoints_sql(ctx.version_num)
    };
    let doubled = current.map(|cur| {
        let next = cur.saturating_mul(2);
        let group = p
            .unit
            .from_bytes(next)
            .map(|v| v.to_string())
            .unwrap_or_default();
        (
            pg_size(next),
            server_suggestion(ctx.kind, p, &pg_size(next), &group, restart),
        )
    });
    let current_shown = max_wal_size
        .map(Setting::display)
        .unwrap_or_else(|| "unknown".into());
    let pct = share * 100.0;
    let (severity, title, detail, suggestion, change) = match &cause {
        CheckpointCause::WalVolume { .. } => (
            Severity::Medium,
            format!("{pct:.0}% of checkpoints forced by WAL volume"),
            "a checkpoint is requested when the WAL since the last one nears max_wal_size; frequent ones write more (full-page images after each) and make I/O spiky. More room spaces them out to checkpoint_timeout; recovery after a crash takes longer in proportion".to_string(),
            doubled.as_ref().map(|(_, s)| s.clone()),
            Some(SettingChange {
                name: p.name,
                current: current_shown,
                suggested: doubled
                    .as_ref()
                    .map(|(v, _)| v.clone())
                    .unwrap_or_else(|| "unknown".into()),
                reasoning: "double it, then watch the requested share fall; it needs that much more disk for WAL at peak".into(),
                restart,
                rds: rds::describe(p, false),
            }),
        ),
        CheckpointCause::Other { wal_forced } => (
            Severity::Low,
            format!("{pct:.0}% of checkpoints requested, but not by WAL volume"),
            format!(
                "the WAL written could have forced at most about {:.0} of the {} requested checkpoints, so the rest were asked for: {OTHER_CHECKPOINT_CAUSES}. Raising max_wal_size won't change those; with log_checkpoints = on the server log names each checkpoint's reason",
                wal_forced.floor(),
                c.requested
            ),
            None,
            None,
        ),
        CheckpointCause::Unknown(why) => (
            Severity::Low,
            format!("{pct:.0}% of checkpoints requested rather than timed"),
            format!(
                "pgman can't tell how many came from WAL volume ({why}). Only those does max_wal_size space out; the rest come from {OTHER_CHECKPOINT_CAUSES}. With log_checkpoints = on the server log says which: \"checkpoint starting: wal\" is WAL volume"
            ),
            doubled.as_ref().map(|(_, s)| s.clone()),
            Some(SettingChange {
                name: p.name,
                current: current_shown,
                suggested: doubled
                    .as_ref()
                    .map(|(v, _)| v.clone())
                    .unwrap_or_else(|| "unknown".into()),
                reasoning: "double it only if the server log shows most checkpoints starting for \"wal\"; it needs that much more disk for WAL at peak".into(),
                restart,
                rds: rds::describe(p, false),
            }),
        ),
    };
    Some(Finding {
        severity,
        code: "ADV005",
        title,
        object: "server".to_string(),
        detail,
        suggestion,
        advice: Some(Box::new(Advice {
            window: c.window.describe(),
            evidence,
            evidence_sql,
            change,
        })),
    })
}

/// What [`fetch`] found, and the checks that couldn't run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub errors: Vec<String>,
}

fn db_err(e: tokio_postgres::Error) -> String {
    crate::conn::chain_message(&e)
}

async fn read_context(client: &tokio_postgres::Client) -> Result<Context, String> {
    let row = client.query_one(CONTEXT_SQL, &[]).await.map_err(db_err)?;
    let g = |e: tokio_postgres::Error| db_err(e);
    Ok(Context {
        version_num: row.try_get(0).map_err(g)?,
        block_size: row.try_get::<_, i32>(1).map_err(g)? as i64,
        kind: server_kind(row.try_get(2).map_err(g)?, row.try_get(3).map_err(g)?),
        in_recovery: row.try_get(4).map_err(g)?,
        can_read_all_stats: row.try_get(5).map_err(g)?,
        uptime_secs: row.try_get(6).map_err(g)?,
        server_start: row.try_get(7).map_err(g)?,
        database: row.try_get(8).map_err(g)?,
        has_pg_stat_statements: row.try_get(9).map_err(g)?,
    })
}

async fn read_settings(client: &tokio_postgres::Client) -> Result<Settings, String> {
    let rows = client.query(&settings_sql(), &[]).await.map_err(db_err)?;
    let mut out = Settings::new();
    for r in &rows {
        out.insert(
            r.try_get(0).map_err(db_err)?,
            Setting {
                setting: r.try_get(1).map_err(db_err)?,
                unit: r.try_get(2).map_err(db_err)?,
                context: r.try_get(3).map_err(db_err)?,
            },
        );
    }
    Ok(out)
}

fn window(ctx: &Context, reset_at: Option<String>, reset_age_secs: Option<f64>) -> Window {
    Window {
        reset_at,
        reset_age_secs,
        uptime_secs: ctx.uptime_secs,
        server_start: ctx.server_start.clone(),
    }
}

async fn read_db_stats(client: &tokio_postgres::Client, ctx: &Context) -> Result<DbStats, String> {
    let r = client.query_one(DB_STATS_SQL, &[]).await.map_err(db_err)?;
    Ok(DbStats {
        blks_hit: r.try_get(0).map_err(db_err)?,
        blks_read: r.try_get(1).map_err(db_err)?,
        temp_files: r.try_get(2).map_err(db_err)?,
        temp_bytes: r.try_get(3).map_err(db_err)?,
        window: window(
            ctx,
            r.try_get(4).map_err(db_err)?,
            r.try_get(5).map_err(db_err)?,
        ),
    })
}

async fn read_spillers(client: &tokio_postgres::Client, ctx: &Context) -> Spillers {
    if !ctx.has_pg_stat_statements {
        return Spillers::Unavailable(crate::query::slow_queries::PGSS_MISSING.to_string());
    }
    match client.query(SPILLERS_SQL, &[]).await {
        Ok(rows) => Spillers::Known(
            rows.iter()
                .filter_map(|r| {
                    let blocks: i64 = r.try_get(2).ok()?;
                    Some(Spiller {
                        query: r.try_get(0).ok()?,
                        calls: r.try_get(1).ok()?,
                        temp_bytes: blocks.saturating_mul(ctx.block_size),
                        role: r.try_get(3).ok()?,
                    })
                })
                .collect(),
        ),
        Err(e) => Spillers::Unavailable(format!(
            "pg_stat_statements couldn't be read: {}",
            db_err(e)
        )),
    }
}

async fn read_vacuum(
    client: &tokio_postgres::Client,
    th: &Thresholds,
) -> Result<Vec<TableVacuum>, String> {
    let rows = client.query(&vacuum_sql(th), &[]).await.map_err(db_err)?;
    rows.iter()
        .map(|r| {
            Ok(TableVacuum {
                schema: r.try_get(0).map_err(db_err)?,
                table: r.try_get(1).map_err(db_err)?,
                quoted: r.try_get(2).map_err(db_err)?,
                live: r.try_get(3).map_err(db_err)?,
                dead: r.try_get(4).map_err(db_err)?,
                reloptions: r.try_get(5).map_err(db_err)?,
                since_vacuum_secs: r.try_get(6).map_err(db_err)?,
            })
        })
        .collect()
}

async fn read_locks(
    client: &tokio_postgres::Client,
    ctx: &Context,
) -> Result<Vec<LockSession>, String> {
    let rows = client
        .query(&locks_sql(ctx.version_num), &[])
        .await
        .map_err(db_err)?;
    rows.iter()
        .map(|r| {
            Ok(LockSession {
                pid: r.try_get(0).map_err(db_err)?,
                user: r.try_get(1).map_err(db_err)?,
                state: r.try_get(2).map_err(db_err)?,
                xact_secs: r.try_get(3).map_err(db_err)?,
                wait_secs: r.try_get(4).map_err(db_err)?,
                blockers: r.try_get(5).map_err(db_err)?,
                query: r.try_get(6).map_err(db_err)?,
            })
        })
        .collect()
}

async fn read_checkpoints(
    client: &tokio_postgres::Client,
    ctx: &Context,
) -> Result<Checkpoints, String> {
    let r = client
        .query_one(&checkpoints_sql(ctx.version_num), &[])
        .await
        .map_err(db_err)?;
    Ok(Checkpoints {
        timed: r.try_get(0).map_err(db_err)?,
        requested: r.try_get(1).map_err(db_err)?,
        window: window(
            ctx,
            r.try_get(2).map_err(db_err)?,
            r.try_get(3).map_err(db_err)?,
        ),
        wal: read_wal(client, ctx).await,
    })
}

/// `pg_stat_wal`, which ADV005 needs to tell WAL-forced checkpoints
/// from the rest. Its absence only hedges the finding.
async fn read_wal(client: &tokio_postgres::Client, ctx: &Context) -> Result<WalVolume, String> {
    if ctx.version_num < 140_000 {
        return Err("pg_stat_wal, which counts the WAL written, is PG14+".into());
    }
    let r = client
        .query_one(WAL_SQL, &[])
        .await
        .map_err(|e| format!("pg_stat_wal couldn't be read: {}", db_err(e)))?;
    let g = |e: tokio_postgres::Error| format!("pg_stat_wal couldn't be read: {}", db_err(e));
    Ok(WalVolume {
        bytes: r.try_get(0).map_err(g)?,
        window: window(ctx, r.try_get(1).map_err(g)?, r.try_get(2).map_err(g)?),
    })
}

/// Run every check. Each runs on its own: one the role can't read is
/// named in `errors` and the rest still report. `Err` when the server
/// context can't be read, or no check ran.
pub async fn fetch(client: &tokio_postgres::Client, th: &Thresholds) -> Result<Report, String> {
    let ctx = read_context(client).await?;
    let mut report = Report::default();
    let settings = match read_settings(client).await {
        Ok(s) => s,
        Err(e) => {
            report.errors.push(format!("settings: {e}"));
            Settings::new()
        }
    };
    let mut ran = 0;
    if ctx.in_recovery {
        report.findings.push(standby_note(&ctx));
    }

    ran += 1;
    match read_db_stats(client, &ctx).await {
        Ok(s) => {
            report
                .findings
                .extend(young_window_note(&ctx, &s.window, th));
            report.findings.extend(cache_hit_finding(
                &ctx,
                &s,
                settings.get("shared_buffers"),
                th,
            ));
            if !s.window.young(th) && s.temp_bytes >= th.min_temp_bytes {
                let spillers = read_spillers(client, &ctx).await;
                report.findings.extend(temp_spill_finding(
                    &ctx,
                    &s,
                    settings.get("work_mem"),
                    &spillers,
                    th,
                ));
            }
        }
        Err(e) => report.errors.push(format!("ADV001/ADV002: {e}")),
    }

    if !ctx.in_recovery {
        ran += 1;
        match read_vacuum(client, th).await {
            Ok(tables) => {
                let defaults = VacuumDefaults::from_settings(&settings);
                report.findings.extend(
                    tables
                        .iter()
                        .filter_map(|t| dead_tuple_finding(&ctx, t, defaults, th)),
                );
            }
            Err(e) => report.errors.push(format!("ADV003: {e}")),
        }
    }

    ran += 1;
    match read_locks(client, &ctx).await {
        Ok(sessions) => report
            .findings
            .extend(lock_finding(&ctx, &sessions, &settings, th)),
        Err(e) => report.errors.push(format!("ADV004: {e}")),
    }

    if !ctx.in_recovery && ctx.kind != ServerKind::Aurora {
        ran += 1;
        match read_checkpoints(client, &ctx).await {
            Ok(c) => report.findings.extend(checkpoint_finding(
                &ctx,
                &c,
                settings.get("max_wal_size"),
                settings.get("checkpoint_completion_target"),
                th,
            )),
            Err(e) => report.errors.push(format!("ADV005: {e}")),
        }
    }

    let failed = report
        .errors
        .iter()
        .filter(|e| e.starts_with("ADV"))
        .count();
    if failed == ran {
        return Err(report.errors.join("; "));
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
