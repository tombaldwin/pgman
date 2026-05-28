//! L2 — insights. Pure aggregation over the in-memory ring.
//!
//! Split from `tap/mod.rs` for code-health; the schema +
//! parsing + transports live there. Re-exported from
//! `tap/mod.rs` so external callers keep using
//! `crate::tap::Hotspot` etc.

use super::{TapEvent, TapKind, TxnOutcome};

/// How to sort the [`Hotspot`] list. Cycled with `s` in the
/// hotspots view; the default lands on `TotalTime` because
/// "where is the database spending its time" is the most
/// common entry-point question.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HotspotSort {
    /// Sum of `duration_micros` across the group, descending.
    /// Often the right starting question.
    #[default]
    TotalTime,
    /// Count of statements in the group, descending. Surfaces
    /// chatty templates (likely-N+1, hot caches missing, etc.).
    CallCount,
    /// 95th percentile of `duration_micros`, descending.
    /// Surfaces tail-latency offenders the totals can hide.
    P95Latency,
}

impl HotspotSort {
    /// One-shot sort-mode cycler used by the panel's `s` key.
    pub fn next(self) -> Self {
        match self {
            HotspotSort::TotalTime => HotspotSort::CallCount,
            HotspotSort::CallCount => HotspotSort::P95Latency,
            HotspotSort::P95Latency => HotspotSort::TotalTime,
        }
    }

    /// Human-readable label for the chrome / panel title.
    pub fn label(self) -> &'static str {
        match self {
            HotspotSort::TotalTime => "total time",
            HotspotSort::CallCount => "call count",
            HotspotSort::P95Latency => "p95 latency",
        }
    }
}

/// Aggregate stats for one fingerprint bucket in the
/// hotspots view. Built by [`group_hotspots`] from the
/// in-memory ring. `example_sql` is the most recent member's
/// SQL (matches the column the operator pivots from when
/// drilling into the cause).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotspot {
    pub fingerprint: String,
    pub example_sql: String,
    pub count: usize,
    pub error_count: usize,
    /// Sum of `duration_micros` across the group.
    pub total_micros: u64,
    /// 50th-percentile duration (nearest-rank).
    pub p50_micros: u64,
    /// 95th-percentile duration (nearest-rank).
    pub p95_micros: u64,
    /// 99th-percentile duration (nearest-rank).
    pub p99_micros: u64,
    /// Distinct innermost-caller frames seen in the group.
    /// `0` when no event in the group carried a caller stack.
    pub distinct_callers: usize,
    /// Most recently seen innermost-caller frame in the group,
    /// or `None` when no event carried one.
    pub last_caller: Option<String>,
    /// Most recently seen `app` value in the group.
    pub last_app: Option<String>,
}

impl Hotspot {
    /// Per-call mean duration (total / count). `0` when the
    /// group is empty — but a group is never empty by
    /// construction (one event creates one group).
    pub fn mean_micros(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.total_micros / self.count as u64
        }
    }
}

/// Group an event slice by SQL fingerprint, computing
/// per-bucket counts, error counts, total duration, and
/// p50 / p95 / p99 durations. Heartbeat and txn-boundary
/// events are skipped — only query events fingerprint
/// meaningfully. Pure; called from the panel renderer.
///
/// Sort order is applied at the end per `sort`.
pub fn group_hotspots<'a, I>(events: I, sort: HotspotSort) -> Vec<Hotspot>
where
    I: IntoIterator<Item = &'a TapEvent>,
{
    use std::collections::HashMap;
    // Aggregator: fingerprint → accumulator.
    #[derive(Default)]
    struct Acc {
        example_sql: String,
        count: usize,
        error_count: usize,
        total_micros: u64,
        durations: Vec<u64>,
        distinct_callers: std::collections::HashSet<String>,
        last_caller: Option<String>,
        last_app: Option<String>,
    }
    let mut buckets: HashMap<String, Acc> = HashMap::new();
    for e in events {
        if !matches!(e.kind, TapKind::Query) {
            continue;
        }
        let Some(sql) = e.sql.as_deref() else {
            continue;
        };
        let fp = crate::query::nplus1::fingerprint(sql);
        let acc = buckets.entry(fp).or_default();
        acc.example_sql = sql.to_string(); // most recent wins
        acc.count += 1;
        if e.is_error() {
            acc.error_count += 1;
        }
        let d = e.duration_micros.unwrap_or(0);
        acc.total_micros = acc.total_micros.saturating_add(d);
        acc.durations.push(d);
        if let Some(c) = e.innermost_caller() {
            acc.distinct_callers.insert(c.to_string());
            acc.last_caller = Some(c.to_string());
        }
        if let Some(app) = e.app.as_deref() {
            acc.last_app = Some(app.to_string());
        }
    }
    let mut out: Vec<Hotspot> = buckets
        .into_iter()
        .map(|(fingerprint, mut acc)| {
            // Nearest-rank percentile: sort durations ascending,
            // index = ceil(p * N) - 1, clamped to [0, N-1].
            acc.durations.sort_unstable();
            let p50 = percentile(&acc.durations, 0.50);
            let p95 = percentile(&acc.durations, 0.95);
            let p99 = percentile(&acc.durations, 0.99);
            Hotspot {
                fingerprint,
                example_sql: acc.example_sql,
                count: acc.count,
                error_count: acc.error_count,
                total_micros: acc.total_micros,
                p50_micros: p50,
                p95_micros: p95,
                p99_micros: p99,
                distinct_callers: acc.distinct_callers.len(),
                last_caller: acc.last_caller,
                last_app: acc.last_app,
            }
        })
        .collect();
    sort_hotspots(&mut out, sort);
    out
}

/// In-place sort of a hotspot list per `sort`. Exposed so
/// the panel can resort without re-aggregating when the
/// operator presses `s`.
pub fn sort_hotspots(out: &mut [Hotspot], sort: HotspotSort) {
    match sort {
        HotspotSort::TotalTime => out.sort_by(|a, b| {
            b.total_micros
                .cmp(&a.total_micros)
                .then_with(|| a.fingerprint.cmp(&b.fingerprint))
        }),
        HotspotSort::CallCount => out.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.fingerprint.cmp(&b.fingerprint))
        }),
        HotspotSort::P95Latency => out.sort_by(|a, b| {
            b.p95_micros
                .cmp(&a.p95_micros)
                .then_with(|| a.fingerprint.cmp(&b.fingerprint))
        }),
    }
}

/// Aggregate stats keyed by innermost caller frame —
/// answers "which app code path is responsible for the
/// database time?" Sibling to [`Hotspot`] but the grouping
/// key is the app side, not the SQL side. Built by
/// [`group_by_caller`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerStats {
    /// Innermost non-framework frame the caller was at when
    /// the JAR sampled the stack. `"<unknown>"` when no event
    /// in the bucket carried a caller frame (the JAR may have
    /// caller-capture disabled or threshold-gated).
    pub caller: String,
    pub count: usize,
    pub error_count: usize,
    pub total_micros: u64,
    pub p50_micros: u64,
    pub p95_micros: u64,
    pub p99_micros: u64,
    /// Distinct SQL fingerprints observed under this caller.
    /// High count + many distinct fingerprints = a method
    /// driving a lot of varied work; high count + few
    /// fingerprints = a hot loop (often the N+1 shape).
    pub distinct_fingerprints: usize,
    /// Most recent SQL fingerprint seen — gives the operator
    /// a concrete pointer when drilling in.
    pub last_fingerprint: Option<String>,
    pub last_app: Option<String>,
}

impl CallerStats {
    pub fn mean_micros(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.total_micros / self.count as u64
        }
    }
}

/// Sentinel rendered when an event has no caller frame.
/// Surfaced in the panel so operators see the "unknown"
/// bucket explicitly rather than mistaking missing data for
/// "no traffic from that caller."
pub const UNKNOWN_CALLER: &str = "<unknown>";

/// Group an event slice by innermost caller frame
/// (`caller[0]`), computing the same per-bucket totals as
/// [`group_hotspots`]. Events without any caller frame land
/// in the [`UNKNOWN_CALLER`] bucket so the rollup remains
/// total-conserving (sum of counts = total query events).
/// Pure; called from the panel renderer.
pub fn group_by_caller<'a, I>(events: I, sort: HotspotSort) -> Vec<CallerStats>
where
    I: IntoIterator<Item = &'a TapEvent>,
{
    use std::collections::HashMap;
    #[derive(Default)]
    struct Acc {
        count: usize,
        error_count: usize,
        total_micros: u64,
        durations: Vec<u64>,
        distinct_fingerprints: std::collections::HashSet<String>,
        last_fingerprint: Option<String>,
        last_app: Option<String>,
    }
    let mut buckets: HashMap<String, Acc> = HashMap::new();
    for e in events {
        if !matches!(e.kind, TapKind::Query) {
            continue;
        }
        let key = e
            .innermost_caller()
            .map(str::to_string)
            .unwrap_or_else(|| UNKNOWN_CALLER.to_string());
        let acc = buckets.entry(key).or_default();
        acc.count += 1;
        if e.is_error() {
            acc.error_count += 1;
        }
        let d = e.duration_micros.unwrap_or(0);
        acc.total_micros = acc.total_micros.saturating_add(d);
        acc.durations.push(d);
        if let Some(sql) = e.sql.as_deref() {
            let fp = crate::query::nplus1::fingerprint(sql);
            acc.last_fingerprint = Some(fp.clone());
            acc.distinct_fingerprints.insert(fp);
        }
        if let Some(app) = e.app.as_deref() {
            acc.last_app = Some(app.to_string());
        }
    }
    let mut out: Vec<CallerStats> = buckets
        .into_iter()
        .map(|(caller, mut acc)| {
            acc.durations.sort_unstable();
            CallerStats {
                caller,
                count: acc.count,
                error_count: acc.error_count,
                total_micros: acc.total_micros,
                p50_micros: percentile(&acc.durations, 0.50),
                p95_micros: percentile(&acc.durations, 0.95),
                p99_micros: percentile(&acc.durations, 0.99),
                distinct_fingerprints: acc.distinct_fingerprints.len(),
                last_fingerprint: acc.last_fingerprint,
                last_app: acc.last_app,
            }
        })
        .collect();
    sort_callers(&mut out, sort);
    out
}

/// In-place sort of a caller-stats list per `sort`. Mirrors
/// [`sort_hotspots`] so the panel can resort on `s` without
/// re-aggregating.
pub fn sort_callers(out: &mut [CallerStats], sort: HotspotSort) {
    match sort {
        HotspotSort::TotalTime => out.sort_by(|a, b| {
            b.total_micros
                .cmp(&a.total_micros)
                .then_with(|| a.caller.cmp(&b.caller))
        }),
        HotspotSort::CallCount => out.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.caller.cmp(&b.caller))
        }),
        HotspotSort::P95Latency => out.sort_by(|a, b| {
            b.p95_micros
                .cmp(&a.p95_micros)
                .then_with(|| a.caller.cmp(&b.caller))
        }),
    }
}

/// Aggregate stats for one transaction — populated by
/// [`group_by_txn`]. Surfaces long-held transactions,
/// read-after-write patterns, and the classic
/// "47 SELECTs + 1 COMMIT" N+1 shape at the txn level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxnStats {
    /// Synthetic transaction id from the JAR. `None` for
    /// events that didn't carry one (autocommit traffic
    /// not yet routed via the conn-fallback path) — kept
    /// distinct from synthetic-id transactions so the
    /// renderer can label them differently.
    pub txn: Option<String>,
    pub conn: Option<String>,
    pub app: Option<String>,
    /// HikariCP / connection-pool name the txn ran against
    /// (e.g. `primary` vs `replica`). Populated from the
    /// first event in the bucket that carried one. Surfaces
    /// "did this write hit the replica pool by mistake?"
    /// without waiting on the JAR to ship an explicit
    /// `pool_role` field.
    pub pool: Option<String>,
    /// Number of query events inside the transaction.
    pub statement_count: usize,
    pub error_count: usize,
    /// Distinct SQL fingerprints — `1` for the canonical
    /// N+1 shape, high for legitimate varied work.
    pub distinct_fingerprints: usize,
    /// Most recent SQL fingerprint observed — gives the
    /// operator a concrete pointer.
    pub last_fingerprint: Option<String>,
    /// Earliest event ts in the txn (microseconds since
    /// the Unix epoch).
    pub first_ts_unix_micros: u64,
    /// Latest event ts in the txn — equals the boundary
    /// event's ts when `outcome` is `Some`.
    pub last_ts_unix_micros: u64,
    /// Wall-clock span from first to last event. Doesn't
    /// include time after the last observed event for an
    /// open transaction — that gap is "we haven't seen
    /// it yet."
    pub span_micros: u64,
    /// Sum of `duration_micros` across the txn's queries.
    /// Total DB time, separate from wall-clock span.
    pub total_query_micros: u64,
    /// `None` when no TxnBoundary event has closed the
    /// txn — i.e. the transaction is open as far as
    /// pgman can tell. `Some(Commit)` / `Some(Rollback)`
    /// when the boundary arrived.
    pub outcome: Option<TxnOutcome>,
}

impl TxnStats {
    /// Whether pgman has seen a `TxnBoundary` close this
    /// transaction. Open transactions are usually the
    /// diagnostic target (held locks, blocking other
    /// sessions).
    pub fn is_open(&self) -> bool {
        self.outcome.is_none()
    }
}

/// Group events by synthetic transaction id. Walks the
/// ring once, bucketing query events by `txn` (falling back
/// to `conn` so autocommit traffic groups usefully) and
/// closing each bucket out with the matching `TxnBoundary`
/// event when one arrives.
///
/// Sort: open transactions first (sorted by span desc —
/// longest-held are most diagnostic), then closed
/// transactions by statement_count desc, fingerprint as
/// the final tiebreak for determinism.
pub fn group_by_txn<'a, I>(events: I) -> Vec<TxnStats>
where
    I: IntoIterator<Item = &'a TapEvent>,
{
    use std::collections::HashMap;
    #[derive(Default)]
    struct Acc {
        txn: Option<String>,
        conn: Option<String>,
        app: Option<String>,
        pool: Option<String>,
        statement_count: usize,
        error_count: usize,
        distinct_fingerprints: std::collections::HashSet<String>,
        last_fingerprint: Option<String>,
        first_ts: Option<u64>,
        last_ts: Option<u64>,
        total_query_micros: u64,
        outcome: Option<TxnOutcome>,
    }
    let mut buckets: HashMap<String, Acc> = HashMap::new();
    let key_of = |e: &TapEvent| -> Option<String> {
        // Prefer txn; fall back to conn so autocommit
        // traffic groups under "one txn per autocommit
        // statement" via the conn id. Events with neither
        // can't be grouped meaningfully — drop them rather
        // than pool them all under a single sentinel.
        e.txn.clone().or_else(|| e.conn.clone())
    };
    for e in events {
        let Some(key) = key_of(e) else {
            continue;
        };
        match e.kind {
            TapKind::Query => {
                let acc = buckets.entry(key).or_default();
                if acc.txn.is_none() {
                    acc.txn = e.txn.clone();
                }
                if acc.conn.is_none() {
                    acc.conn = e.conn.clone();
                }
                if acc.app.is_none() {
                    acc.app = e.app.clone();
                }
                if acc.pool.is_none() {
                    acc.pool = e.pool.clone();
                }
                acc.statement_count += 1;
                if e.is_error() {
                    acc.error_count += 1;
                }
                if let Some(sql) = e.sql.as_deref() {
                    let fp = crate::query::nplus1::fingerprint(sql);
                    acc.last_fingerprint = Some(fp.clone());
                    acc.distinct_fingerprints.insert(fp);
                }
                acc.total_query_micros = acc
                    .total_query_micros
                    .saturating_add(e.duration_micros.unwrap_or(0));
                if acc.first_ts.is_none() {
                    acc.first_ts = Some(e.ts_unix_micros);
                }
                acc.last_ts = Some(e.ts_unix_micros);
            }
            TapKind::TxnBoundary => {
                let acc = buckets.entry(key).or_default();
                if acc.txn.is_none() {
                    acc.txn = e.txn.clone();
                }
                if acc.conn.is_none() {
                    acc.conn = e.conn.clone();
                }
                acc.outcome = e.txn_outcome;
                acc.last_ts = Some(e.ts_unix_micros);
            }
            TapKind::Heartbeat => {
                // Heartbeats don't belong to any txn.
            }
        }
    }
    let mut out: Vec<TxnStats> = buckets
        .into_iter()
        .filter_map(|(_, acc)| {
            // Drop empty buckets — a TxnBoundary with no
            // preceding queries isn't useful (the ring may
            // have evicted them).
            if acc.statement_count == 0 {
                return None;
            }
            let first = acc.first_ts.unwrap_or(0);
            let last = acc.last_ts.unwrap_or(first);
            Some(TxnStats {
                txn: acc.txn,
                conn: acc.conn,
                app: acc.app,
                pool: acc.pool,
                statement_count: acc.statement_count,
                error_count: acc.error_count,
                distinct_fingerprints: acc.distinct_fingerprints.len(),
                last_fingerprint: acc.last_fingerprint,
                first_ts_unix_micros: first,
                last_ts_unix_micros: last,
                span_micros: last.saturating_sub(first),
                total_query_micros: acc.total_query_micros,
                outcome: acc.outcome,
            })
        })
        .collect();
    // Open transactions first (sorted by span desc), then
    // closed by statement_count desc.
    out.sort_by(|a, b| {
        let open_order = match (a.is_open(), b.is_open()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        };
        let key_tiebreak = || {
            a.txn
                .as_deref()
                .unwrap_or("")
                .cmp(b.txn.as_deref().unwrap_or(""))
                .then_with(|| {
                    a.conn
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.conn.as_deref().unwrap_or(""))
                })
        };
        open_order
            .then_with(|| {
                if a.is_open() {
                    b.span_micros.cmp(&a.span_micros)
                } else {
                    b.statement_count.cmp(&a.statement_count)
                }
            })
            .then_with(key_tiebreak)
    });
    out
}

/// One row of the baseline-diff view: a fingerprint
/// classified relative to a captured baseline snapshot.
/// The renderer colour-codes each kind so a glance tells
/// the story.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotspotDiff {
    pub fingerprint: String,
    pub example_sql: String,
    pub kind: DiffKind,
    /// Counts and p95 from the baseline (zero if `kind ==
    /// New`).
    pub baseline_count: usize,
    pub baseline_p95_micros: u64,
    /// Counts and p95 from the current snapshot (zero if
    /// `kind == Disappeared`).
    pub current_count: usize,
    pub current_p95_micros: u64,
}

/// What changed for this fingerprint vs the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Fingerprint wasn't in the baseline ring but is in the
    /// current — the typical "did my deploy introduce this?"
    /// signal.
    New,
    /// Current p95 is at least `BASELINE_REGRESSION_FACTOR`×
    /// the baseline p95. Tail latency got worse, even if the
    /// call count is flat.
    Regressed,
    /// Fingerprint was in the baseline but isn't present in
    /// the current — either the call site went away or the
    /// ring window slid past it. Surfaced because operators
    /// looking for "what disappeared" do exist (rollback
    /// validation).
    Disappeared,
    /// In both snapshots, no notable change. Filtered out by
    /// default — `Unchanged` only surfaces when the operator
    /// asks for everything.
    Unchanged,
}

/// Default regression threshold: 2× p95. Anything ≥ 2× is
/// flagged `Regressed`. The threshold is conservative so
/// pgman doesn't cry wolf for normal jitter.
pub const BASELINE_REGRESSION_FACTOR: u64 = 2;

/// Pure diff between two hotspot snapshots. Returns one
/// `HotspotDiff` per fingerprint that changed (`New`,
/// `Regressed`, or `Disappeared` by default — `Unchanged`
/// rows are dropped unless `include_unchanged` is set).
///
/// Sort: regressions first (sorted by current p95 desc), then
/// new (by current count desc), then disappeared (by baseline
/// count desc), then unchanged (alphabetical). Within ties,
/// fingerprint ascending for determinism.
pub fn diff_hotspots(
    baseline: &[Hotspot],
    current: &[Hotspot],
    include_unchanged: bool,
) -> Vec<HotspotDiff> {
    use std::collections::{HashMap, HashSet};
    let baseline_by_fp: HashMap<&str, &Hotspot> =
        baseline.iter().map(|h| (h.fingerprint.as_str(), h)).collect();
    let current_by_fp: HashMap<&str, &Hotspot> =
        current.iter().map(|h| (h.fingerprint.as_str(), h)).collect();
    let mut all_fps: HashSet<&str> = HashSet::new();
    all_fps.extend(baseline_by_fp.keys().copied());
    all_fps.extend(current_by_fp.keys().copied());

    let mut out: Vec<HotspotDiff> = Vec::new();
    for fp in all_fps {
        let b = baseline_by_fp.get(fp).copied();
        let c = current_by_fp.get(fp).copied();
        let (kind, example_sql, b_count, b_p95, c_count, c_p95) = match (b, c) {
            (None, Some(cur)) => (
                DiffKind::New,
                cur.example_sql.clone(),
                0,
                0,
                cur.count,
                cur.p95_micros,
            ),
            (Some(base), None) => (
                DiffKind::Disappeared,
                base.example_sql.clone(),
                base.count,
                base.p95_micros,
                0,
                0,
            ),
            (Some(base), Some(cur)) => {
                let regressed = base.p95_micros > 0
                    && cur.p95_micros >= base.p95_micros.saturating_mul(BASELINE_REGRESSION_FACTOR);
                let k = if regressed {
                    DiffKind::Regressed
                } else {
                    DiffKind::Unchanged
                };
                (
                    k,
                    cur.example_sql.clone(),
                    base.count,
                    base.p95_micros,
                    cur.count,
                    cur.p95_micros,
                )
            }
            (None, None) => unreachable!("set membership"),
        };
        if !include_unchanged && matches!(kind, DiffKind::Unchanged) {
            continue;
        }
        out.push(HotspotDiff {
            fingerprint: fp.to_string(),
            example_sql,
            kind,
            baseline_count: b_count,
            baseline_p95_micros: b_p95,
            current_count: c_count,
            current_p95_micros: c_p95,
        });
    }
    // Sort: Regressed (highest current p95 first), New
    // (highest current count first), Disappeared (highest
    // baseline count first), Unchanged (alphabetical).
    out.sort_by(|a, b| {
        let order = |k: DiffKind| match k {
            DiffKind::Regressed => 0,
            DiffKind::New => 1,
            DiffKind::Disappeared => 2,
            DiffKind::Unchanged => 3,
        };
        let oa = order(a.kind);
        let ob = order(b.kind);
        oa.cmp(&ob)
            .then_with(|| match a.kind {
                DiffKind::Regressed => b.current_p95_micros.cmp(&a.current_p95_micros),
                DiffKind::New => b.current_count.cmp(&a.current_count),
                DiffKind::Disappeared => b.baseline_count.cmp(&a.baseline_count),
                DiffKind::Unchanged => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });
    out
}

/// One detected N+1 burst: a `(txn, fingerprint)` pair that
/// fired `count` times inside `window_micros`. The renderer
/// uses `last_caller` as the pointer to the offending app
/// code; `example_sql` is the most-recent member's SQL so the
/// operator can see what's running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NplusOneFinding {
    pub fingerprint: String,
    pub example_sql: String,
    /// Synthetic transaction id from the JAR. `None` when the
    /// events fired outside a transaction (autocommit; each
    /// statement was its own txn but the same `(conn, conn-seq)`
    /// grouped them in time).
    pub txn: Option<String>,
    pub conn: Option<String>,
    pub app: Option<String>,
    pub count: usize,
    /// First and last event's `ts_unix_micros` in the burst —
    /// gives the operator the "how recent" signal.
    pub first_ts_unix_micros: u64,
    pub last_ts_unix_micros: u64,
    /// Span (last - first); `0` for a single-event finding.
    pub span_micros: u64,
    /// Innermost caller frame from the first event in the burst.
    pub last_caller: Option<String>,
}

/// Default sliding-window for live N+1 detection: 200 ms.
/// Application-level N+1 (a service iterating a collection
/// and firing one SELECT per element) typically fits well
/// inside this; longer bursts are usually batch jobs.
pub const NPLUS1_WINDOW_MICROS: u64 = 200_000;

/// Default minimum repetitions for live N+1 detection.
/// 5 is the operating point matching the offline N+1
/// classifier; below this the false-positive rate from
/// legitimate-but-similar queries climbs.
pub const NPLUS1_MIN_REPEATS: usize = 5;

/// Scan a ring for live N+1 bursts. A finding fires when at
/// least `min_repeats` events sharing the same
/// `(txn-or-conn, fingerprint)` key land within
/// `window_micros` of each other. Pure; called from the
/// panel renderer.
///
/// Algorithm: bucket events by key (txn when set, otherwise
/// the connection id so autocommit traffic still groups
/// usefully), then walk each bucket in `ts_unix_micros`
/// order using a sliding window over indices to find runs
/// of `min_repeats` events inside the time window. The
/// finding captures the *longest* such run per key, so
/// repeated bursts collapse to one signal.
pub fn detect_nplus1<'a, I>(
    events: I,
    window_micros: u64,
    min_repeats: usize,
) -> Vec<NplusOneFinding>
where
    I: IntoIterator<Item = &'a TapEvent>,
{
    use std::collections::HashMap;
    // Key by (group_key, fingerprint). group_key prefers
    // `txn`; falls back to `conn` so autocommit traffic from
    // the same connection still groups; falls back to a
    // synthetic "—" so we don't lose all signal when the JAR
    // ships none of those.
    type Key = (String, String);
    let mut buckets: HashMap<Key, Vec<&TapEvent>> = HashMap::new();
    for e in events {
        if !matches!(e.kind, TapKind::Query) {
            continue;
        }
        let Some(sql) = e.sql.as_deref() else {
            continue;
        };
        let group_key = e
            .txn
            .clone()
            .or_else(|| e.conn.clone())
            .unwrap_or_else(|| "—".into());
        let fp = crate::query::nplus1::fingerprint(sql);
        buckets.entry((group_key, fp)).or_default().push(e);
    }
    let mut out: Vec<NplusOneFinding> = Vec::new();
    for ((_group, fingerprint), mut events) in buckets {
        if events.len() < min_repeats {
            continue;
        }
        events.sort_by_key(|e| e.ts_unix_micros);
        // Sliding window: find the longest run of indices
        // [l, r] where ts[r] - ts[l] <= window_micros and
        // r - l + 1 >= min_repeats. We track the longest as
        // we go.
        let mut l: usize = 0;
        let mut best: Option<(usize, usize)> = None; // (l, r)
        for r in 0..events.len() {
            while l < r
                && events[r].ts_unix_micros - events[l].ts_unix_micros > window_micros
            {
                l += 1;
            }
            let run = r - l + 1;
            if run >= min_repeats {
                match best {
                    None => best = Some((l, r)),
                    Some((bl, br)) if r - l + 1 > br - bl + 1 => best = Some((l, r)),
                    _ => {}
                }
            }
        }
        let Some((bl, br)) = best else {
            continue;
        };
        let first = events[bl];
        let last = events[br];
        out.push(NplusOneFinding {
            fingerprint,
            example_sql: last.sql.clone().unwrap_or_default(),
            txn: last.txn.clone(),
            conn: last.conn.clone(),
            app: last.app.clone(),
            count: br - bl + 1,
            first_ts_unix_micros: first.ts_unix_micros,
            last_ts_unix_micros: last.ts_unix_micros,
            span_micros: last.ts_unix_micros - first.ts_unix_micros,
            // Use the LAST event's caller so the field name
            // (`last_caller`) matches the source. In practice
            // first and last frames usually agree inside a tight
            // burst (same loop iteration), but consistency
            // matters for callers that might cross loops.
            last_caller: last.innermost_caller().map(str::to_string),
        });
    }
    // Most-repeating first, then most-recent. Ties broken by
    // fingerprint so the order is deterministic.
    out.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| b.last_ts_unix_micros.cmp(&a.last_ts_unix_micros))
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });
    out
}

/// Nearest-rank percentile on a pre-sorted slice. `p` in
/// `[0.0, 1.0]`. Returns `0` for an empty slice. Pure;
/// covered by the percentile_* tests in `mod.rs`.
pub(super) fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let p = p.clamp(0.0, 1.0);
    let rank = (p * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}
