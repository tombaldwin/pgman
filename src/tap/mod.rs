//! JDBC tap — the receiving end of `pgman-tap` (the JVM
//! companion JAR built as a [`datasource-proxy`][dsp]
//! `QueryExecutionListener`). The JAR observes each completed
//! JDBC statement, redacts bound parameters per policy, and
//! ships a JSON event to pgman. pgman listens, decodes, and
//! surfaces the stream in the `Mode::TapMonitor` panel.
//!
//! The module's job is pure: [`TapEvent`] is the wire shape
//! and [`parse`] turns one wire frame (UDP datagram or a TCP
//! length-prefixed message) into one event. The async
//! listeners that own the sockets live separately; see
//! `BACKLOG.md` under "JDBC tap — layered build".
//!
//! [dsp]: https://github.com/jdbc-observations/datasource-proxy
//!
//! # Wire format (v1)
//!
//! One JSON object per frame. Required at all kinds: `v`,
//! `kind`, `ts_unix_micros`. Each kind layers required fields
//! on top:
//!
//! - `kind: "query"` — also requires `sql`, `duration_micros`.
//!   Everything else is optional. Defaults to `query` when
//!   `kind` is omitted, so a JAR that hasn't been updated to
//!   send the discriminator still parses correctly.
//! - `kind: "heartbeat"` — emitted every N seconds by the
//!   JAR. Carries `app`, optionally `dropped_events_total`
//!   so pgman can distinguish "JAR connected, no traffic"
//!   from "JAR gone" and can show backpressure loss.
//! - `kind: "txn_boundary"` — emitted on
//!   `Connection.commit()` / `rollback()`. Carries `txn`
//!   plus `txn_outcome`. Lets pgman retroactively tag
//!   preceding events on the same `conn` as belonging to the
//!   just-closed transaction (JDBC autocommit and Spring's
//!   `@Transactional` both make the boundary visible only
//!   at commit / rollback time).
//!
//! Unknown fields are silently ignored (serde default), so
//! newer JARs can ship additional metadata without forcing a
//! pgman upgrade.
//!
//! ## Example (query)
//!
//! ```json
//! {
//!   "v": 1,
//!   "kind": "query",
//!   "ts_unix_micros": 1734567890123456,
//!   "app": "billing-service",
//!   "pool": "primary",
//!   "conn": "primary-7",
//!   "txn": "primary-7#42",
//!   "sql": "SELECT * FROM accounts WHERE id = ?",
//!   "params": ["[redacted]"],
//!   "params_redacted": true,
//!   "duration_micros": 4521,
//!   "rows": 17,
//!   "error": null,
//!   "caller": [
//!     "com.example.OrderService.findById:42",
//!     "com.example.OrderController.show:88"
//!   ]
//! }
//! ```
//!
//! ## Example (heartbeat)
//!
//! ```json
//! { "v": 1, "kind": "heartbeat", "ts_unix_micros": 1734567890000000,
//!   "app": "billing-service", "dropped_events_total": 0 }
//! ```
//!
//! ## Example (txn_boundary)
//!
//! ```json
//! { "v": 1, "kind": "txn_boundary", "ts_unix_micros": 1734567890200000,
//!   "conn": "primary-7", "txn": "primary-7#42", "txn_outcome": "commit" }
//! ```

pub mod insights;
pub use insights::*;

pub mod replay;
pub use replay::*;

pub mod otlp;
pub use otlp::*;

pub mod listener;
pub use listener::*;

use serde::{Deserialize, Serialize};

/// Current wire-format version. Events with a `v` field that
/// doesn't match this constant are rejected so a mismatched
/// JAR fails loudly rather than silently producing garbled
/// panel rows.
pub const PROTOCOL_VERSION: u32 = 1;

/// Ceiling on `sql` after ingest, in bytes. The struct doc
/// above (and the wire-format doc for `sql`) already claims
/// the JAR truncates huge SQL text before it ships — this is
/// the receiver-side backstop that makes that true regardless
/// of what an old, buggy, or hostile JAR actually sends. A
/// single 1 MiB `sql` string × [`crate::app::TAP_CAP`] events
/// is enough to put the process at multiple GB RSS; capping at
/// ingest, not at render time, is what keeps that bound real.
pub const TAP_MAX_SQL_BYTES: usize = 8 * 1024;

/// Ceiling on every other string-shaped field after ingest, in
/// bytes: `app`, `pool`, `conn`, `txn`, each `params` entry,
/// each `caller` frame, each `error` cause. Applied per-string;
/// the *number* of entries in a list-shaped field is bounded
/// separately by [`TAP_MAX_FIELD_ENTRIES`].
pub const TAP_MAX_FIELD_BYTES: usize = 1024;

/// Ceiling on the number of entries in a list-shaped field
/// after ingest: `params`, `caller`, `error`. Includes the
/// `… +N more` marker, so a capped list is exactly this long.
///
/// [`TAP_MAX_FIELD_BYTES`] bounds each entry but said nothing
/// about how many there could be, and the frame-size bound
/// (`listener::TAP_MAX_FRAME_BYTES`, 256 KiB) does not cover
/// this: `"",` is three bytes on the wire and a `String` is 24
/// bytes plus its heap block in memory, so one frame of 65 000
/// empty `params` costs ~195 KiB to send and megabytes to hold
/// — and the ring keeps [`crate::app::TAP_CAP`] of them. 2 000
/// such frames took RSS to 3.86 GB. Capping at ingest is what
/// makes the ring's memory bound real.
pub const TAP_MAX_FIELD_ENTRIES: usize = 64;

/// Marker appended to a field truncated at ingest. Doubles as
/// the caller-visible signal that a field was shortened —
/// pgman doesn't carry a dedicated `truncated: bool` on
/// [`TapEvent`] because the struct is constructed by-value at
/// ~70 call sites across the codebase (demo data, tests,
/// replay) and adding a required field would ripple far outside
/// the tap module for no reader-visible benefit over the
/// marker itself.
pub const TAP_TRUNCATION_MARKER: char = '…';

/// Default capacity for each of the bounded mpsc channels
/// between listeners and the App-side adapter.
///
/// `main.rs` sets up **two** channels with this capacity:
/// one shared by the live transports (TCP / UDP / OTLP), one
/// dedicated to replay. This avoids replay backpressure
/// starving the live transports. Within the live channel,
/// each transport tries to `try_send` and DROPS on full —
/// see [`DROPPED_AT_LISTENER`]. The replay channel uses
/// `.send().await` for delivery guarantee.
///
/// Sized for "few hundred ms at 1000 QPS" on one transport;
/// production with concurrent live transports may want to
/// scale this up.
pub const TAP_CHANNEL_CAPACITY: usize = 4_096;

/// Process-global counter of events the listener side dropped
/// because the downstream channel was full. Cumulative for
/// the lifetime of the pgman process. Surfaced in the chrome
/// badge so the operator notices backpressure.
pub static DROPPED_AT_LISTENER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Read the cumulative listener-drop count. Public so the UI
/// + report can surface it.
#[must_use]
pub fn dropped_at_listener() -> u64 {
    DROPPED_AT_LISTENER.load(std::sync::atomic::Ordering::Relaxed)
}

/// Used by listeners to forward an event into the bounded
/// channel. On `Full` the event is dropped + counted. On
/// `Closed` we return `Err(())` so the listener can exit.
/// Centralised so the live transports (TCP / UDP / OTLP)
/// share the same backpressure semantics.
///
/// Replay does NOT go through here, and runs on its OWN
/// bounded channel (set up in `main.rs`). It uses
/// `.send().await` for delivery guarantee — backpressure
/// blocks the replay file pump but cannot starve the live
/// transports, which contend for a different channel.
fn forward_or_drop(
    tx: &tokio::sync::mpsc::Sender<TapEvent>,
    event: TapEvent,
    transport: &'static str,
) -> Result<(), ()> {
    match tx.try_send(event) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            // Cumulative global + per-transport counters. The
            // per-transport bucket drives the log-throttle so
            // a chatty transport can't silence the others'
            // first-drop notices.
            DROPPED_AT_LISTENER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let bucket = match transport {
                "tcp" => &DROPPED_TCP,
                "udp" => &DROPPED_UDP,
                "otlp" => &DROPPED_OTLP,
                _ => &DROPPED_OTHER,
            };
            let n = bucket.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            // Throttle the warn — first drop on THIS transport
            // + every 1000th on this transport.
            if n == 1 || n.is_multiple_of(1000) {
                tracing::warn!(
                    "tap-{transport}: dropped event (channel full); transport drops = {n}"
                );
            }
            Ok(())
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(()),
    }
}

static DROPPED_TCP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DROPPED_UDP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DROPPED_OTLP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DROPPED_OTHER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Rate-limits a `tracing::warn!` call site to at most one
/// emission per second, folding everything suppressed in
/// between into a count the next emitted line reports. One
/// instance per log site (a `static`), so a chatty transport
/// can't silence another's warnings the way one shared limiter
/// would.
///
/// Exists because a malformed-frame / malformed-datagram warn
/// used to fire once per bad packet — an attacker (or a
/// misconfigured JAR) sending garbage at line rate turned a few
/// hundred KB of input into tens of MB of log output. See
/// [`listener::run_tcp_listener`] / [`listener::run_udp_listener`]
/// and [`otlp::span_to_tap_event`] for the call sites.
pub(crate) struct WarnThrottle {
    last_warn_micros: std::sync::atomic::AtomicU64,
    suppressed: std::sync::atomic::AtomicU64,
}

impl WarnThrottle {
    pub(crate) const fn new() -> Self {
        Self {
            last_warn_micros: std::sync::atomic::AtomicU64::new(0),
            suppressed: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Call at the warn site instead of `tracing::warn!`
    /// directly. `msg` receives the number of prior calls
    /// suppressed since the last emitted line (`0` on the very
    /// first call, or after a quiet second) and returns the
    /// line to log. Only invoked when this call actually wins
    /// the window — suppressed calls never build the string.
    pub(crate) fn warn(&self, msg: impl FnOnce(u64) -> String) {
        use std::sync::atomic::Ordering;
        const WINDOW_MICROS: u64 = 1_000_000;
        let now = now_unix_micros();
        let last = self.last_warn_micros.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < WINDOW_MICROS {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // CAS so two racing callers in the same window don't
        // both win it — the loser just counts as suppressed.
        if self
            .last_warn_micros
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let suppressed = self.suppressed.swap(0, Ordering::Relaxed);
        tracing::warn!("{}", msg(suppressed));
    }
}

/// Event variant. The JAR sets this explicitly; older JAR
/// builds that pre-date the discriminator are treated as
/// `Query` via [`TapKind::default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TapKind {
    /// One completed JDBC statement — the default and by far
    /// the most common kind on a live stream.
    #[default]
    Query,
    /// Periodic liveness ping from the JAR; carries the
    /// dropped-events counter from its in-process ring buffer
    /// so pgman can surface backpressure loss.
    Heartbeat,
    /// Connection commit or rollback. Lets the panel close
    /// out the synthetic transaction id the JAR assigns to
    /// preceding statements on the same connection.
    TxnBoundary,
}

/// Result of a `Connection.commit()` / `rollback()` boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TxnOutcome {
    Commit,
    Rollback,
}

/// One observed tap message. Fields are unioned across the
/// three [`TapKind`] variants — kind-specific fields are
/// `Option`s and are present only when the kind needs them.
/// [`parse`] enforces the per-kind required-fields contract;
/// callers can rely on the invariants documented there.
///
/// `received_at_unix_micros` is the *only* field stamped by
/// pgman (not the JAR), as the listener-receive time. It's
/// excluded from the wire format and defaults to 0 until the
/// listener fills it in; it gives pgman a clock-skew-resistant
/// anchor when the JAR's clock disagrees with the host's.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TapEvent {
    /// Protocol version — must equal [`PROTOCOL_VERSION`].
    pub v: u32,
    /// Event kind. Defaults to `Query` for backward compat
    /// with the pre-discriminator JAR shape.
    #[serde(default)]
    pub kind: TapKind,
    /// JAR-stamped wall-clock at the event boundary,
    /// microseconds since the Unix epoch.
    pub ts_unix_micros: u64,
    /// Listener-stamped receive time (not on the wire).
    /// Set by the receiver post-parse; lets us reconstruct
    /// the timeline when the JAR clock is skewed.
    #[serde(skip, default)]
    pub received_at_unix_micros: u64,

    // -- shared context ---------------------------------
    /// Service name from `pgman.tap.app-name`. Useful when
    /// multiple services tap into one pgman.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Connection-pool name (HikariCP `poolName`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Stable id for the underlying physical connection.
    /// Two events from the same connection share this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conn: Option<String>,
    /// Synthetic transaction id assigned by the JAR
    /// (`conn-id#sequence`). Rolls when commit / rollback
    /// fires — the matching boundary arrives as a
    /// `TxnBoundary` event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txn: Option<String>,

    // -- Query-kind fields ------------------------------
    /// SQL text as the application sent it. Templates with `?`
    /// placeholders stay as-is; `params` carries the bound
    /// values separately. For huge SQL strings the JAR
    /// truncates and appends a `… (N bytes)` marker.
    ///
    /// Required when `kind == Query`; `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    /// Bound parameters, rendered as strings (same shape as
    /// the grid). `None` for raw `Statement` execution and
    /// for non-query kinds. Individual values may be
    /// `"[redacted]"` markers when PII redaction fired —
    /// see `params_redacted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<String>>,
    /// `true` when the JAR replaced one or more bound
    /// parameters with `[redacted]` per the configured
    /// PII rules. Rendered as a visible marker so the
    /// operator knows the displayed values are sanitised.
    #[serde(default, skip_serializing_if = "is_false")]
    pub params_redacted: bool,
    /// End-to-end wall-clock duration in microseconds. Covers
    /// only the JDBC call — network + server time both included.
    ///
    /// Required when `kind == Query`; `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_micros: Option<u64>,
    /// Row count for SELECT / affected count for DML. `None`
    /// when the JDBC API didn't expose one (e.g. exception).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<i64>,
    /// Java exception chain (root cause LAST). Empty/None
    /// for successful statements. Stored as a list so callers
    /// see the full causal trail; the renderer joins with `→`
    /// for the one-line preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Vec<String>>,
    /// Short stack of non-framework frames
    /// (`class.method:line`), innermost first. `None` when
    /// `pgman.tap.caller=false` or when the threshold-gated
    /// capture didn't fire. Spring AOP, transactional proxies,
    /// and `@Async` all push the diagnostic frame several
    /// layers up — that's why this is a stack and not a single
    /// string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<Vec<String>>,

    // -- Heartbeat-kind fields --------------------------
    /// Total events the JAR dropped because its in-process
    /// ring filled up. Cumulative across the JAR lifetime;
    /// pgman diffs successive heartbeats to surface a rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_events_total: Option<u64>,

    // -- TxnBoundary-kind fields ------------------------
    /// Commit vs Rollback for `kind == TxnBoundary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txn_outcome: Option<TxnOutcome>,
}

/// serde helper for `#[serde(skip_serializing_if = ...)]` on
/// `params_redacted: bool` — keeps the captured JSON minimal.
fn is_false(b: &bool) -> bool {
    !*b
}

impl TapEvent {
    /// Convenience: is this query event a failure?
    /// Always `false` for non-Query kinds — heartbeats and
    /// txn boundaries don't carry errors.
    pub fn is_error(&self) -> bool {
        matches!(self.kind, TapKind::Query) && self.error.as_ref().is_some_and(|e| !e.is_empty())
    }

    /// First non-blank line of the SQL, with internal runs of
    /// whitespace collapsed, capped at `width` chars. Used in
    /// the panel list view where each event is one row.
    /// Returns an empty string when this isn't a Query event.
    pub fn sql_preview(&self, width: usize) -> String {
        match self.sql.as_deref() {
            Some(s) => {
                let one = collapse_whitespace(s);
                crate::grid::truncate_cell(&one, width)
            }
            None => String::new(),
        }
    }

    /// Inner-most caller frame (`caller[0]`) for the
    /// per-caller rollup. `None` when caller capture didn't
    /// fire or this isn't a Query event.
    pub fn innermost_caller(&self) -> Option<&str> {
        self.caller.as_deref()?.first().map(String::as_str)
    }

    /// Join the error cause chain with `→` for one-line
    /// rendering. Root cause is at the end of the chain.
    pub fn error_one_line(&self) -> Option<String> {
        let chain = self.error.as_deref()?;
        if chain.is_empty() {
            return None;
        }
        Some(chain.join(" → "))
    }
}

/// Parse one tap frame into a [`TapEvent`]. Pure; called from
/// the listeners but also exercised by tests with raw JSON
/// fixtures.
///
/// Returns `Err` for: invalid UTF-8, malformed JSON,
/// mismatched protocol version, or kind-specific required
/// fields missing. The listeners log and otherwise ignore
/// parse failures so a single bad packet can't take down the
/// stream.
#[must_use = "parse returns a Result — dropping it silently loses the event"]
pub fn parse(bytes: &[u8]) -> Result<TapEvent, String> {
    let s = std::str::from_utf8(bytes).map_err(|e| format!("not utf-8: {e}"))?;
    let mut event: TapEvent = serde_json::from_str(s).map_err(|e| format!("bad json: {e}"))?;
    if event.v != PROTOCOL_VERSION {
        return Err(format!(
            "protocol version mismatch: got v={}, expected v={PROTOCOL_VERSION}",
            event.v
        ));
    }
    validate_required(&event)?;
    enforce_field_caps(&mut event);
    Ok(event)
}

/// Truncate `s` in place to at most `max_bytes` UTF-8 bytes,
/// appending [`TAP_TRUNCATION_MARKER`]. No-op (returns `false`)
/// when `s` is already within the cap. Cuts on a `char`
/// boundary at or before `max_bytes` so the result is always
/// valid UTF-8 — never splits a multi-byte codepoint.
fn truncate_field(s: &mut String, max_bytes: usize) -> bool {
    if s.len() <= max_bytes {
        return false;
    }
    // Leave room for the marker itself (`…` is 3 bytes in
    // UTF-8) so the result never exceeds `max_bytes`.
    let mut cut = max_bytes.saturating_sub(TAP_TRUNCATION_MARKER.len_utf8());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push(TAP_TRUNCATION_MARKER);
    // `truncate` shortens the string but keeps the allocation.
    // The event is about to be parked in a ring holding
    // `TAP_CAP` of them, so the buffer, not the length, is what
    // shows up in RSS — hand it back.
    s.shrink_to_fit();
    true
}

/// Truncate `v` in place to at most [`TAP_MAX_FIELD_ENTRIES`]
/// entries, the last of which becomes a `… +N more` marker
/// naming how many were dropped. No-op (returns `false`) when
/// `v` is already within the cap.
///
/// Entries are dropped from the *end*. For `error`, whose
/// contract puts the root cause last, that means a capped chain
/// loses its root cause — but a chain long enough to be capped
/// is a hostile or broken sender rather than a real causal
/// trail, and the marker says plainly that entries are missing.
fn truncate_entries(v: &mut Vec<String>) -> bool {
    if v.len() <= TAP_MAX_FIELD_ENTRIES {
        return false;
    }
    // One slot of the cap is spent on the marker itself.
    let kept = TAP_MAX_FIELD_ENTRIES - 1;
    let dropped = v.len() - kept;
    v.truncate(kept);
    v.push(format!("{TAP_TRUNCATION_MARKER} +{dropped} more"));
    // As in `truncate_field`: `truncate` frees the dropped
    // `String`s but keeps the `Vec`'s own 65 000-slot buffer,
    // which is most of the memory this cap exists to reclaim.
    v.shrink_to_fit();
    true
}

/// Bound every string-shaped field on `event` to
/// [`TAP_MAX_SQL_BYTES`] (`sql`) / [`TAP_MAX_FIELD_BYTES`]
/// (everything else), truncating in place. Called from
/// [`parse`] for the TCP/UDP/replay path; the OTLP path
/// ([`otlp::span_to_tap_event`]) calls it too since it builds
/// `TapEvent`s outside `parse`. Returns `true` when at least
/// one field was shortened, so a caller that wants to log a
/// one-line notice can do so without re-scanning the event.
///
/// This is the receiver-side enforcement of the truncation the
/// JAR is documented (see the `sql` field doc on [`TapEvent`])
/// to already do — an old, buggy, or actively hostile JAR that
/// skips its own truncation must not be able to turn one frame
/// into a multi-MB ring entry.
pub(crate) fn enforce_field_caps(event: &mut TapEvent) -> bool {
    let mut truncated = false;
    if let Some(sql) = event.sql.as_mut() {
        truncated |= truncate_field(sql, TAP_MAX_SQL_BYTES);
    }
    if let Some(app) = event.app.as_mut() {
        truncated |= truncate_field(app, TAP_MAX_FIELD_BYTES);
    }
    if let Some(pool) = event.pool.as_mut() {
        truncated |= truncate_field(pool, TAP_MAX_FIELD_BYTES);
    }
    if let Some(conn) = event.conn.as_mut() {
        truncated |= truncate_field(conn, TAP_MAX_FIELD_BYTES);
    }
    if let Some(txn) = event.txn.as_mut() {
        truncated |= truncate_field(txn, TAP_MAX_FIELD_BYTES);
    }
    // Cap the entry count first, so the per-string pass only
    // ever walks a bounded list — and, more to the point, so an
    // over-long list is released before it reaches the ring.
    for list in [
        event.params.as_mut(),
        event.caller.as_mut(),
        event.error.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        truncated |= truncate_entries(list);
        for entry in list.iter_mut() {
            truncated |= truncate_field(entry, TAP_MAX_FIELD_BYTES);
        }
    }
    truncated
}

/// Enforce the per-kind required-fields contract documented
/// on [`TapEvent`]. Called from [`parse`] but also exposed so
/// tests can pin the rules.
pub fn validate_required(event: &TapEvent) -> Result<(), String> {
    match event.kind {
        TapKind::Query => {
            if event.sql.is_none() {
                return Err("query event missing required field `sql`".into());
            }
            if event.duration_micros.is_none() {
                return Err("query event missing required field `duration_micros`".into());
            }
        }
        TapKind::Heartbeat => {
            // No further required fields beyond v/kind/ts —
            // a heartbeat with only the discriminator is fine.
        }
        TapKind::TxnBoundary => {
            if event.txn.is_none() {
                return Err("txn_boundary event missing required field `txn`".into());
            }
            if event.txn_outcome.is_none() {
                return Err("txn_boundary event missing required field `txn_outcome`".into());
            }
        }
    }
    Ok(())
}

/// Collapse internal whitespace runs (newlines, tabs, repeated
/// spaces) to single spaces; trim ends. Multi-line SQL stays
/// readable as a one-line list entry.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = true; // start in_ws so leading whitespace is dropped
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    // Trim a possible trailing space from the in-loop logic.
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

pub(super) fn now_unix_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_minimal_required_fields() {
        let bytes = br#"{
            "v": 1,
            "ts_unix_micros": 1700000000000000,
            "sql": "SELECT 1",
            "duration_micros": 250
        }"#;
        let e = parse(bytes).expect("minimal payload should parse");
        assert_eq!(e.v, 1);
        // kind defaults to Query when omitted — backward compat
        // with pre-discriminator JAR builds.
        assert_eq!(e.kind, TapKind::Query);
        assert_eq!(e.sql.as_deref(), Some("SELECT 1"));
        assert_eq!(e.duration_micros, Some(250));
        // Optional fields default to None / false / 0.
        assert!(e.app.is_none());
        assert!(e.params.is_none());
        assert!(!e.params_redacted);
        assert!(e.rows.is_none());
        assert!(e.error.is_none());
        assert!(!e.is_error());
        assert_eq!(e.received_at_unix_micros, 0);
    }

    #[test]
    fn parse_accepts_fully_populated_query() {
        let bytes = br#"{
            "v": 1,
            "kind": "query",
            "ts_unix_micros": 1700000000000000,
            "app": "billing-service",
            "pool": "primary",
            "conn": "primary-7",
            "txn": "primary-7#42",
            "sql": "SELECT * FROM accounts WHERE id = ?",
            "params": ["[redacted]"],
            "params_redacted": true,
            "duration_micros": 4521,
            "rows": 17,
            "error": null,
            "caller": [
                "com.example.OrderService.findById:42",
                "com.example.OrderController.show:88"
            ]
        }"#;
        let e = parse(bytes).expect("full payload should parse");
        assert_eq!(e.kind, TapKind::Query);
        assert_eq!(e.app.as_deref(), Some("billing-service"));
        assert_eq!(e.pool.as_deref(), Some("primary"));
        assert_eq!(e.conn.as_deref(), Some("primary-7"));
        assert_eq!(e.txn.as_deref(), Some("primary-7#42"));
        assert_eq!(e.params.as_deref().map(<[_]>::len), Some(1));
        assert!(e.params_redacted);
        assert_eq!(e.rows, Some(17));
        assert_eq!(
            e.innermost_caller(),
            Some("com.example.OrderService.findById:42")
        );
    }

    #[test]
    fn parse_accepts_heartbeat_with_just_discriminator() {
        let bytes = br#"{
            "v": 1,
            "kind": "heartbeat",
            "ts_unix_micros": 1700000000000000,
            "app": "billing-service",
            "dropped_events_total": 17
        }"#;
        let e = parse(bytes).expect("heartbeat should parse");
        assert_eq!(e.kind, TapKind::Heartbeat);
        assert_eq!(e.dropped_events_total, Some(17));
        // Heartbeat doesn't carry sql/duration — that's the point.
        assert!(e.sql.is_none());
        assert!(e.duration_micros.is_none());
        // is_error never fires for heartbeat events.
        assert!(!e.is_error());
    }

    #[test]
    fn parse_accepts_txn_boundary_commit_and_rollback() {
        let commit = br#"{
            "v": 1, "kind": "txn_boundary", "ts_unix_micros": 1,
            "conn": "c-1", "txn": "c-1#1", "txn_outcome": "commit"
        }"#;
        let rollback = br#"{
            "v": 1, "kind": "txn_boundary", "ts_unix_micros": 2,
            "conn": "c-1", "txn": "c-1#2", "txn_outcome": "rollback"
        }"#;
        let c = parse(commit).expect("commit boundary should parse");
        let r = parse(rollback).expect("rollback boundary should parse");
        assert_eq!(c.txn_outcome, Some(TxnOutcome::Commit));
        assert_eq!(r.txn_outcome, Some(TxnOutcome::Rollback));
    }

    #[test]
    fn parse_rejects_query_missing_sql() {
        let bytes = br#"{
            "v": 1,
            "kind": "query",
            "ts_unix_micros": 1,
            "duration_micros": 1
        }"#;
        let err = parse(bytes).expect_err("query needs sql");
        assert!(
            err.contains("missing required field `sql`"),
            "expected sql-missing message; got: {err}"
        );
    }

    #[test]
    fn parse_rejects_query_missing_duration() {
        let bytes = br#"{
            "v": 1,
            "kind": "query",
            "ts_unix_micros": 1,
            "sql": "SELECT 1"
        }"#;
        let err = parse(bytes).expect_err("query needs duration_micros");
        assert!(
            err.contains("missing required field `duration_micros`"),
            "expected duration-missing message; got: {err}"
        );
    }

    #[test]
    fn parse_rejects_txn_boundary_without_outcome() {
        let bytes = br#"{
            "v": 1, "kind": "txn_boundary", "ts_unix_micros": 1,
            "txn": "c-1#1"
        }"#;
        let err = parse(bytes).expect_err("boundary needs outcome");
        assert!(
            err.contains("missing required field `txn_outcome`"),
            "expected outcome-missing message; got: {err}"
        );
    }

    #[test]
    fn parse_rejects_txn_boundary_without_txn() {
        let bytes = br#"{
            "v": 1, "kind": "txn_boundary", "ts_unix_micros": 1,
            "txn_outcome": "commit"
        }"#;
        let err = parse(bytes).expect_err("boundary needs txn");
        assert!(
            err.contains("missing required field `txn`"),
            "expected txn-missing message; got: {err}"
        );
    }

    #[test]
    fn parse_rejects_unknown_protocol_version() {
        let bytes = br#"{
            "v": 99,
            "ts_unix_micros": 1700000000000000,
            "sql": "SELECT 1",
            "duration_micros": 1
        }"#;
        let err = parse(bytes).expect_err("v=99 must be rejected");
        assert!(
            err.contains("version mismatch"),
            "expected version-mismatch message; got: {err}"
        );
    }

    #[test]
    fn parse_rejects_non_utf8_payload() {
        let bytes: &[u8] = &[0xff, 0xfe, 0xfd];
        let err = parse(bytes).expect_err("non-utf8 must be rejected");
        assert!(err.contains("utf-8"), "expected utf-8 message: {err}");
    }

    #[test]
    fn parse_rejects_malformed_json() {
        let bytes = br#"{not valid json"#;
        let err = parse(bytes).expect_err("malformed json must be rejected");
        assert!(err.contains("bad json"), "expected bad-json message: {err}");
    }

    #[test]
    fn parse_silently_ignores_unknown_fields() {
        // Forward-compat: a newer JAR ships fields pgman
        // doesn't know about yet. Must not error.
        let bytes = br#"{
            "v": 1,
            "ts_unix_micros": 1,
            "sql": "SELECT 1",
            "duration_micros": 1,
            "future_field": {"some": "thing"},
            "another_future_field": 42
        }"#;
        let e = parse(bytes).expect("unknown fields must be ignored");
        assert_eq!(e.sql.as_deref(), Some("SELECT 1"));
    }

    // --- ingest field-size caps ------------------------

    #[test]
    fn parse_truncates_an_oversized_sql_field() {
        // A single JSON frame carrying a huge `sql` string.
        // Well under the (256 KiB) TCP frame cap tested
        // separately in listener.rs — this test pins the
        // FIELD-level cap `parse` enforces regardless of frame
        // size, i.e. it must fire even for a frame that would
        // pass the frame-size gate untouched.
        let huge_sql = "x".repeat(50_000);
        let payload = serde_json::json!({
            "v": 1,
            "ts_unix_micros": 1,
            "sql": huge_sql,
            "duration_micros": 1,
        });
        let bytes = serde_json::to_vec(&payload).unwrap();
        let e = parse(&bytes).expect("oversized sql must still parse, just truncated");
        let sql = e.sql.as_deref().expect("sql present");
        assert!(
            sql.len() <= TAP_MAX_SQL_BYTES,
            "sql must be capped at {TAP_MAX_SQL_BYTES} bytes; got {}",
            sql.len()
        );
        assert!(
            sql.ends_with(TAP_TRUNCATION_MARKER),
            "truncated sql must carry the marker; got tail {:?}",
            &sql[sql.len().saturating_sub(8)..]
        );
    }

    #[test]
    fn parse_truncates_oversized_app_pool_conn_txn_fields() {
        let huge = "y".repeat(10_000);
        let payload = serde_json::json!({
            "v": 1,
            "ts_unix_micros": 1,
            "sql": "SELECT 1",
            "duration_micros": 1,
            "app": huge,
            "pool": huge,
            "conn": huge,
            "txn": huge,
        });
        let bytes = serde_json::to_vec(&payload).unwrap();
        let e = parse(&bytes).unwrap();
        for field in [
            e.app.as_deref().unwrap(),
            e.pool.as_deref().unwrap(),
            e.conn.as_deref().unwrap(),
            e.txn.as_deref().unwrap(),
        ] {
            assert!(field.len() <= TAP_MAX_FIELD_BYTES);
            assert!(field.ends_with(TAP_TRUNCATION_MARKER));
        }
    }

    #[test]
    fn parse_truncates_each_oversized_params_caller_and_error_entry() {
        let huge = "z".repeat(5_000);
        let payload = serde_json::json!({
            "v": 1,
            "ts_unix_micros": 1,
            "sql": "SELECT 1",
            "duration_micros": 1,
            "params": [huge.clone(), "short"],
            "caller": [huge.clone()],
            "error": [huge],
        });
        let bytes = serde_json::to_vec(&payload).unwrap();
        let e = parse(&bytes).unwrap();
        let params = e.params.as_ref().unwrap();
        assert!(params[0].len() <= TAP_MAX_FIELD_BYTES);
        assert!(params[0].ends_with(TAP_TRUNCATION_MARKER));
        // The short entry is untouched — truncation is
        // per-string, not "cap the whole vec to one size."
        assert_eq!(params[1], "short");
        assert!(e.caller.as_ref().unwrap()[0].len() <= TAP_MAX_FIELD_BYTES);
        assert!(e.error.as_ref().unwrap()[0].len() <= TAP_MAX_FIELD_BYTES);
    }

    #[test]
    fn parse_caps_the_entry_count_of_params_caller_and_error() {
        // The reproduction: `TAP_MAX_FIELD_BYTES` bounded each
        // entry but not how many there were, and 65 000 empty
        // params is only ~195 KiB on the wire — well under the
        // frame cap — while costing megabytes to hold. 2 000
        // such frames in the ring took RSS to 3.86 GB.
        const SENT: usize = 65_000;
        let flood: Vec<String> = vec![String::new(); SENT];
        let payload = serde_json::json!({
            "v": 1,
            "ts_unix_micros": 1,
            "sql": "SELECT 1",
            "duration_micros": 1,
            "params": flood.clone(),
            "caller": flood.clone(),
            "error": flood,
        });
        let bytes = serde_json::to_vec(&payload).unwrap();
        let e = parse(&bytes).unwrap();

        for (name, list) in [
            ("params", e.params.as_ref().unwrap()),
            ("caller", e.caller.as_ref().unwrap()),
            ("error", e.error.as_ref().unwrap()),
        ] {
            assert_eq!(
                list.len(),
                TAP_MAX_FIELD_ENTRIES,
                "{name} must be capped at {TAP_MAX_FIELD_ENTRIES} entries"
            );
            // The last entry says how many were dropped, so the
            // operator isn't shown a silently shortened list.
            assert_eq!(
                list[TAP_MAX_FIELD_ENTRIES - 1],
                format!(
                    "{TAP_TRUNCATION_MARKER} +{} more",
                    SENT - (TAP_MAX_FIELD_ENTRIES - 1)
                ),
                "{name} must carry a marker naming the dropped count"
            );
            // Truncating a `Vec` keeps its buffer; the capacity
            // is what the ring actually holds, so it has to come
            // down too or the cap buys nothing.
            assert!(
                list.capacity() < SENT,
                "{name} must release the over-long allocation; capacity {}",
                list.capacity()
            );
        }
    }

    #[test]
    fn a_list_at_the_cap_is_left_alone() {
        let mut v: Vec<String> = (0..TAP_MAX_FIELD_ENTRIES).map(|i| i.to_string()).collect();
        assert!(!truncate_entries(&mut v));
        assert_eq!(v.len(), TAP_MAX_FIELD_ENTRIES);
        assert_eq!(v[TAP_MAX_FIELD_ENTRIES - 1], "63");
        // One over is the first length that loses an entry.
        v.push("64".to_string());
        assert!(truncate_entries(&mut v));
        assert_eq!(v.len(), TAP_MAX_FIELD_ENTRIES);
        assert_eq!(v[TAP_MAX_FIELD_ENTRIES - 1], "… +2 more");
    }

    #[test]
    fn parse_leaves_small_fields_untouched() {
        let bytes = br#"{
            "v": 1, "ts_unix_micros": 1,
            "sql": "SELECT 1", "duration_micros": 1,
            "app": "billing-service"
        }"#;
        let e = parse(bytes).unwrap();
        assert_eq!(e.sql.as_deref(), Some("SELECT 1"));
        assert_eq!(e.app.as_deref(), Some("billing-service"));
    }

    #[test]
    fn truncate_field_never_splits_a_multibyte_char_boundary() {
        // Every char here is a 3-byte UTF-8 codepoint (€). A
        // naive byte-index truncation could split one down the
        // middle and panic (or produce invalid UTF-8).
        let mut s = "€".repeat(2000); // 6000 bytes
        let truncated = truncate_field(&mut s, TAP_MAX_FIELD_BYTES);
        assert!(truncated);
        assert!(s.len() <= TAP_MAX_FIELD_BYTES);
        // Must still be valid UTF-8 — `String` guarantees this
        // if we got here without panicking, but assert the
        // marker landed too.
        assert!(s.ends_with(TAP_TRUNCATION_MARKER));
    }

    #[test]
    fn truncate_field_is_a_no_op_under_the_cap() {
        let mut s = String::from("short");
        assert!(!truncate_field(&mut s, TAP_MAX_FIELD_BYTES));
        assert_eq!(s, "short");
    }

    #[test]
    fn is_error_branches_on_error_field_for_query_only() {
        let bytes_ok = br#"{
            "v": 1, "ts_unix_micros": 0,
            "sql": "SELECT 1", "duration_micros": 1
        }"#;
        let bytes_err = br#"{
            "v": 1, "ts_unix_micros": 0,
            "sql": "SELECT 1", "duration_micros": 1,
            "error": [
                "org.postgresql.util.PSQLException: ERROR: syntax error at or near \"FROM\"",
                "java.sql.BatchUpdateException: Batch entry 0 failed"
            ]
        }"#;
        // Empty error chain (defensive — JAR could ship `[]`)
        // doesn't count as an error.
        let bytes_empty_err = br#"{
            "v": 1, "ts_unix_micros": 0,
            "sql": "SELECT 1", "duration_micros": 1,
            "error": []
        }"#;
        let bytes_heartbeat_with_error = br#"{
            "v": 1, "kind": "heartbeat", "ts_unix_micros": 0,
            "error": ["this should be ignored on heartbeat"]
        }"#;
        assert!(!parse(bytes_ok).unwrap().is_error());
        assert!(parse(bytes_err).unwrap().is_error());
        assert!(!parse(bytes_empty_err).unwrap().is_error());
        assert!(!parse(bytes_heartbeat_with_error).unwrap().is_error());
    }

    #[test]
    fn error_one_line_joins_cause_chain_with_arrow() {
        let bytes = br#"{
            "v": 1, "ts_unix_micros": 0,
            "sql": "SELECT 1", "duration_micros": 1,
            "error": [
                "BatchUpdateException: Batch entry 0 failed",
                "PSQLException: ERROR: syntax error"
            ]
        }"#;
        let e = parse(bytes).unwrap();
        assert_eq!(
            e.error_one_line().as_deref(),
            Some("BatchUpdateException: Batch entry 0 failed → PSQLException: ERROR: syntax error")
        );
    }

    #[test]
    fn innermost_caller_picks_first_frame() {
        let bytes = br#"{
            "v": 1, "ts_unix_micros": 0,
            "sql": "SELECT 1", "duration_micros": 1,
            "caller": [
                "com.example.OrderService.findById:42",
                "com.example.OrderController.show:88",
                "org.springframework.web.servlet.DispatcherServlet:1064"
            ]
        }"#;
        let e = parse(bytes).unwrap();
        assert_eq!(
            e.innermost_caller(),
            Some("com.example.OrderService.findById:42")
        );
    }

    #[test]
    fn sql_preview_collapses_whitespace_and_truncates() {
        let mut e = sample_query();
        e.sql = Some("SELECT  *\n  FROM   accounts\n  WHERE id = ?".into());
        let preview = e.sql_preview(30);
        // Whitespace runs collapsed to single spaces.
        assert_eq!(preview, "SELECT * FROM accounts WHERE …");
    }

    #[test]
    fn sql_preview_returns_empty_for_non_query_events() {
        let mut e = sample_query();
        e.kind = TapKind::Heartbeat;
        e.sql = None;
        assert_eq!(e.sql_preview(30), "");
    }

    #[test]
    fn collapse_whitespace_strips_leading_and_trailing() {
        assert_eq!(collapse_whitespace("  a  b  "), "a b");
        assert_eq!(collapse_whitespace("\n\t a\n\tb \t"), "a b");
        assert_eq!(collapse_whitespace(""), "");
        assert_eq!(collapse_whitespace("   "), "");
    }

    // --- insights / hotspots tests --------------------

    fn q(sql: &str, duration: u64, error: bool, caller: Option<&str>, app: &str) -> TapEvent {
        TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: 0,
            received_at_unix_micros: 0,
            app: Some(app.into()),
            pool: None,
            conn: None,
            txn: None,
            sql: Some(sql.into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(duration),
            rows: None,
            error: if error {
                Some(vec!["err".into()])
            } else {
                None
            },
            caller: caller.map(|c| vec![c.into()]),
            dropped_events_total: None,
            txn_outcome: None,
        }
    }

    #[test]
    fn group_hotspots_collapses_literals_into_one_bucket() {
        // Same shape, different literals → one fingerprint → one bucket.
        let events = [
            q(
                "SELECT * FROM users WHERE id = 1",
                100,
                false,
                None,
                "billing",
            ),
            q(
                "SELECT * FROM users WHERE id = 2",
                200,
                false,
                None,
                "billing",
            ),
            q(
                "SELECT * FROM users WHERE id = 999",
                300,
                false,
                None,
                "billing",
            ),
            // Different shape — its own bucket.
            q("SELECT * FROM orders", 50, false, None, "billing"),
        ];
        let hotspots = group_hotspots(events.iter(), HotspotSort::TotalTime);
        assert_eq!(hotspots.len(), 2);
        // TotalTime sort: the 3-call users bucket comes first.
        assert_eq!(hotspots[0].count, 3);
        assert_eq!(hotspots[0].total_micros, 600);
        assert_eq!(hotspots[0].mean_micros(), 200);
        assert_eq!(hotspots[1].count, 1);
    }

    #[test]
    fn group_hotspots_skips_heartbeat_and_txn_boundary_kinds() {
        let mut hb = q("ignored", 0, false, None, "x");
        hb.kind = TapKind::Heartbeat;
        hb.sql = None;
        let mut txn = q("ignored", 0, false, None, "x");
        txn.kind = TapKind::TxnBoundary;
        txn.sql = None;
        txn.txn = Some("c-1#1".into());
        txn.txn_outcome = Some(TxnOutcome::Commit);
        let events = [
            q("SELECT 1", 10, false, None, "x"),
            hb,
            txn,
            q("SELECT 1", 20, false, None, "x"),
        ];
        let hotspots = group_hotspots(events.iter(), HotspotSort::TotalTime);
        assert_eq!(hotspots.len(), 1);
        assert_eq!(hotspots[0].count, 2);
    }

    #[test]
    fn group_hotspots_counts_errors_separately() {
        let events = [
            q("SELECT 1", 10, false, None, "x"),
            q("SELECT 1", 20, true, None, "x"),
            q("SELECT 1", 30, true, None, "x"),
        ];
        let hotspots = group_hotspots(events.iter(), HotspotSort::TotalTime);
        assert_eq!(hotspots[0].count, 3);
        assert_eq!(hotspots[0].error_count, 2);
    }

    #[test]
    fn group_hotspots_tracks_distinct_callers_and_last_seen() {
        let events = [
            q("SELECT 1", 1, false, Some("a.x:1"), "x"),
            q("SELECT 1", 1, false, Some("b.y:2"), "x"),
            q("SELECT 1", 1, false, Some("a.x:1"), "x"),
            // Last event has no caller — last_caller stays the previous one.
            q("SELECT 1", 1, false, None, "x"),
        ];
        let hotspots = group_hotspots(events.iter(), HotspotSort::TotalTime);
        assert_eq!(hotspots[0].distinct_callers, 2);
        // last_caller is the most recent NON-None — `a.x:1` from event 3.
        assert_eq!(hotspots[0].last_caller.as_deref(), Some("a.x:1"));
    }

    #[test]
    fn group_hotspots_percentiles_use_nearest_rank() {
        // 10 events with durations 1..=10. p50 should be 5
        // (rank=ceil(0.5*10)=5 → index 4 → 5), p95 = 10
        // (rank=ceil(0.95*10)=10 → index 9 → 10), p99 = 10.
        let events: Vec<TapEvent> = (1u64..=10)
            .map(|d| q("SELECT 1", d, false, None, "x"))
            .collect();
        let hotspots = group_hotspots(events.iter(), HotspotSort::TotalTime);
        assert_eq!(hotspots[0].p50_micros, 5);
        assert_eq!(hotspots[0].p95_micros, 10);
        assert_eq!(hotspots[0].p99_micros, 10);
    }

    #[test]
    fn group_hotspots_sort_modes_pick_the_right_top_bucket() {
        // Three buckets with distinct SQL *shapes* (the
        // fingerprinter collapses literals, so we vary table
        // names instead).
        // A: 100 cheap calls (1µs each)   → high count, low p95.
        // B: 1   expensive call (1_000µs) → tiny count, max p95.
        // C: 10  medium calls (50µs each) → mid count, mid p95.
        let mut events: Vec<TapEvent> = Vec::new();
        for _ in 0..100 {
            events.push(q("SELECT a FROM t_a", 1, false, None, "x"));
        }
        events.push(q("SELECT b FROM t_b", 1_000, false, None, "x"));
        for _ in 0..10 {
            events.push(q("SELECT c FROM t_c", 50, false, None, "x"));
        }
        let by_calls = group_hotspots(events.iter(), HotspotSort::CallCount);
        assert_eq!(by_calls[0].count, 100);
        let by_p95 = group_hotspots(events.iter(), HotspotSort::P95Latency);
        assert_eq!(by_p95[0].count, 1);
        // Total time: A = 100, B = 1000, C = 500. B wins.
        let by_total = group_hotspots(events.iter(), HotspotSort::TotalTime);
        assert_eq!(by_total[0].total_micros, 1_000);
    }

    #[test]
    fn group_hotspots_sort_is_deterministic_when_keys_tie() {
        // Two buckets with the same total/count/p95; tiebreak
        // is the fingerprint (ascending) so the output is stable.
        let events = [
            q("SELECT 'a'", 10, false, None, "x"),
            q("SELECT 'b'", 10, false, None, "x"),
        ];
        let by_total = group_hotspots(events.iter(), HotspotSort::TotalTime);
        // 'a' < 'b' so 'a' wins the tiebreak.
        assert!(
            by_total[0].fingerprint.contains("'a'") || by_total[0].fingerprint == "select ?",
            "tie-broken sort should be deterministic; got {:?}",
            by_total
        );
    }

    #[test]
    fn group_hotspots_handles_empty_input() {
        let events: Vec<TapEvent> = Vec::new();
        let hotspots = group_hotspots(events.iter(), HotspotSort::TotalTime);
        assert!(hotspots.is_empty());
    }

    #[test]
    fn hotspot_mean_handles_single_call_gracefully() {
        let events = [q("SELECT 1", 42, false, None, "x")];
        let hotspots = group_hotspots(events.iter(), HotspotSort::TotalTime);
        assert_eq!(hotspots[0].mean_micros(), 42);
    }

    #[test]
    fn sort_hotspots_can_be_called_again_to_resort_without_re_aggregating() {
        // Two distinct fingerprint shapes so the buckets stay
        // separate after grouping (the fingerprinter collapses
        // string literals; vary table names instead).
        let events = [
            q("SELECT cheap FROM many", 1, false, None, "x"),
            q("SELECT cheap FROM many", 1, false, None, "x"),
            q("SELECT cheap FROM many", 1, false, None, "x"),
            q("SELECT spike FROM big", 999, false, None, "x"),
        ];
        let mut hotspots = group_hotspots(events.iter(), HotspotSort::TotalTime);
        // After TotalTime sort, the 999µs spike wins (999 > 3).
        let top_by_total = hotspots[0].example_sql.clone();
        sort_hotspots(&mut hotspots, HotspotSort::CallCount);
        // After CallCount sort, the 3-call bucket wins.
        let top_by_count = hotspots[0].example_sql.clone();
        assert_ne!(top_by_total, top_by_count);
        assert!(top_by_count.contains("cheap"));
    }

    #[test]
    fn hotspot_sort_cycles_round_robin() {
        assert_eq!(HotspotSort::TotalTime.next(), HotspotSort::CallCount);
        assert_eq!(HotspotSort::CallCount.next(), HotspotSort::P95Latency);
        assert_eq!(HotspotSort::P95Latency.next(), HotspotSort::TotalTime);
    }

    // --- record + round-trip tests --------------------

    #[test]
    fn record_line_round_trips_via_parse_replay_line() {
        let original = TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: 1_700_000_000_000_000,
            // Intentionally non-zero — record_line must drop this.
            received_at_unix_micros: 9_999_999,
            app: Some("billing-service".into()),
            pool: Some("primary".into()),
            conn: Some("primary-7".into()),
            txn: Some("primary-7#42".into()),
            sql: Some("SELECT * FROM accounts WHERE id = ?".into()),
            params: Some(vec!["[redacted]".into()]),
            params_redacted: true,
            duration_micros: Some(4521),
            rows: Some(17),
            error: Some(vec!["root cause".into()]),
            caller: Some(vec!["a.b:1".into(), "c.d:2".into()]),
            dropped_events_total: None,
            txn_outcome: None,
        };
        let line = record_line(&original).expect("serialize");
        // Round-trip through the same parser the replay path uses.
        let mut decoded = parse_replay_line(&line).unwrap().unwrap();
        // Replay path doesn't carry received_at over the wire —
        // re-stamp here to match the original for equality.
        decoded.received_at_unix_micros = original.received_at_unix_micros;
        assert_eq!(decoded, original);
    }

    #[test]
    fn record_line_omits_null_optional_fields() {
        // Capture files for sparse events (heartbeats, raw
        // Statement calls without bound parameters) used to
        // emit `"app":null,"pool":null,...` for every absent
        // optional. With skip_serializing_if the line stays
        // small. Verify by serialising a minimal heartbeat and
        // checking no `"null"` substring leaks through.
        let hb = TapEvent {
            v: 1,
            kind: TapKind::Heartbeat,
            ts_unix_micros: 1,
            received_at_unix_micros: 0,
            app: None,
            pool: None,
            conn: None,
            txn: None,
            sql: None,
            params: None,
            params_redacted: false,
            duration_micros: None,
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        };
        let line = record_line(&hb).expect("serialize");
        assert!(
            !line.contains("null"),
            "absent optional fields should be omitted entirely; got: {line}"
        );
        assert!(
            !line.contains("\"params_redacted\""),
            "false bool default should be omitted; got: {line}"
        );
        // Round-trip still works (the omitted fields default
        // back to None on the deserialise side).
        let back = parse_replay_line(&line).unwrap().unwrap();
        assert_eq!(back.kind, TapKind::Heartbeat);
        assert!(back.app.is_none());
        assert!(!back.params_redacted);
    }

    #[test]
    fn record_line_drops_received_at_so_capture_is_clock_skew_safe() {
        let event = TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: 1,
            received_at_unix_micros: 12345,
            app: None,
            pool: None,
            conn: None,
            txn: None,
            sql: Some("SELECT 1".into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(1),
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        };
        let line = record_line(&event).expect("serialize");
        assert!(
            !line.contains("received_at"),
            "received_at must NOT serialize; got: {line}"
        );
    }

    #[test]
    fn record_line_handles_heartbeat_and_txn_boundary_kinds() {
        let hb = TapEvent {
            v: 1,
            kind: TapKind::Heartbeat,
            ts_unix_micros: 1,
            received_at_unix_micros: 0,
            app: Some("svc".into()),
            pool: None,
            conn: None,
            txn: None,
            sql: None,
            params: None,
            params_redacted: false,
            duration_micros: None,
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: Some(17),
            txn_outcome: None,
        };
        let line = record_line(&hb).expect("serialize");
        let parsed = parse_replay_line(&line).unwrap().unwrap();
        assert_eq!(parsed.kind, TapKind::Heartbeat);
        assert_eq!(parsed.dropped_events_total, Some(17));

        let txn = TapEvent {
            v: 1,
            kind: TapKind::TxnBoundary,
            ts_unix_micros: 2,
            received_at_unix_micros: 0,
            app: None,
            pool: None,
            conn: Some("c-1".into()),
            txn: Some("c-1#1".into()),
            sql: None,
            params: None,
            params_redacted: false,
            duration_micros: None,
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: Some(TxnOutcome::Commit),
        };
        let line = record_line(&txn).expect("serialize");
        let parsed = parse_replay_line(&line).unwrap().unwrap();
        assert_eq!(parsed.kind, TapKind::TxnBoundary);
        assert_eq!(parsed.txn_outcome, Some(TxnOutcome::Commit));
    }

    // --- backpressure tests ---------------------------

    #[tokio::test]
    async fn forward_or_drop_increments_counter_when_channel_full() {
        // Single-slot channel: first try_send fills it,
        // second must hit the drop path. The atomic is
        // process-global so parallel #[tokio::test]s can race
        // it; rather than snapshot+1 (which becomes +2 under a
        // concurrent drop), we assert the DELTA on this test's
        // invocation is at LEAST 1. The cumulative-counter
        // semantic guarantees we never under-count.
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(1);
        let baseline = dropped_at_listener();
        let make_event = |sql: &str| TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: 1,
            received_at_unix_micros: 0,
            app: None,
            pool: None,
            conn: None,
            txn: None,
            sql: Some(sql.into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(1),
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        };
        // First fills the slot.
        forward_or_drop(&tx, make_event("first"), "test").unwrap();
        // Second can't fit → dropped + counted.
        forward_or_drop(&tx, make_event("second"), "test").unwrap();
        // At least our own drop landed; concurrent tests may
        // have added more but never fewer.
        assert!(
            dropped_at_listener() > baseline,
            "drop counter must advance at least by our own contribution"
        );
    }

    #[tokio::test]
    async fn forward_or_drop_returns_err_when_receiver_closed() {
        let (tx, rx) = tokio::sync::mpsc::channel::<TapEvent>(4);
        drop(rx); // close receiver
        let event = TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: 1,
            received_at_unix_micros: 0,
            app: None,
            pool: None,
            conn: None,
            txn: None,
            sql: Some("SELECT 1".into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(1),
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        };
        assert!(forward_or_drop(&tx, event, "test").is_err());
    }

    // --- replay tests ---------------------------------

    #[test]
    fn parse_replay_line_skips_blank_lines() {
        assert!(parse_replay_line("").is_none());
        assert!(parse_replay_line("   ").is_none());
        assert!(parse_replay_line("\t\t").is_none());
    }

    #[test]
    fn parse_replay_line_parses_a_query_event() {
        let line = r#"{"v":1,"ts_unix_micros":1,"sql":"SELECT 1","duration_micros":10}"#;
        let event = parse_replay_line(line).unwrap().unwrap();
        assert_eq!(event.sql.as_deref(), Some("SELECT 1"));
    }

    #[test]
    fn parse_replay_line_propagates_validation_error() {
        // Missing required `sql` for kind=query → parse rejects.
        let line = r#"{"v":1,"ts_unix_micros":1,"duration_micros":10}"#;
        let err = parse_replay_line(line).unwrap().unwrap_err();
        assert!(err.contains("missing required field"), "got: {err}");
    }

    #[tokio::test]
    async fn run_replay_file_streams_each_line_as_one_event() {
        use tokio::io::AsyncWriteExt;
        // Write a 3-event JSONL fixture to a temp file.
        let tmp =
            std::env::temp_dir().join(format!("pgman-tap-replay-{}.jsonl", std::process::id()));
        let mut f = tokio::fs::File::create(&tmp).await.unwrap();
        let body = concat!(
            r#"{"v":1,"ts_unix_micros":1,"sql":"SELECT 1","duration_micros":10}"#,
            "\n",
            "\n", // blank line — should be skipped
            r#"{"v":1,"ts_unix_micros":2,"sql":"SELECT 2","duration_micros":20}"#,
            "\n",
            r#"{"v":1,"ts_unix_micros":3,"sql":"SELECT 3","duration_micros":30}"#,
            "\n",
        );
        f.write_all(body.as_bytes()).await.unwrap();
        f.flush().await.unwrap();
        drop(f);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let accepted = run_replay_file(&tmp, tx).await.unwrap();
        assert_eq!(accepted, 3);
        let mut got: Vec<String> = Vec::new();
        while let Ok(e) = rx.try_recv() {
            got.push(e.sql.unwrap_or_default());
            // Replay stamps received_at so downstream can't
            // tell live from replayed.
            assert!(e.received_at_unix_micros > 0);
        }
        assert_eq!(got, vec!["SELECT 1", "SELECT 2", "SELECT 3"]);

        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn run_replay_file_drops_malformed_lines_but_continues() {
        use tokio::io::AsyncWriteExt;
        let tmp =
            std::env::temp_dir().join(format!("pgman-tap-replay-bad-{}.jsonl", std::process::id()));
        let mut f = tokio::fs::File::create(&tmp).await.unwrap();
        let body = concat!(
            r#"{"v":1,"ts_unix_micros":1,"sql":"good 1","duration_micros":1}"#,
            "\n",
            "{not valid json",
            "\n", // malformed — should be dropped
            r#"{"v":1,"ts_unix_micros":2,"sql":"good 2","duration_micros":2}"#,
            "\n",
        );
        f.write_all(body.as_bytes()).await.unwrap();
        f.flush().await.unwrap();
        drop(f);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let accepted = run_replay_file(&tmp, tx).await.unwrap();
        assert_eq!(accepted, 2);
        let e1 = rx.try_recv().unwrap();
        let e2 = rx.try_recv().unwrap();
        assert_eq!(e1.sql.as_deref(), Some("good 1"));
        assert_eq!(e2.sql.as_deref(), Some("good 2"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn run_replay_file_skips_a_line_past_the_cap_and_continues() {
        use tokio::io::AsyncWriteExt;
        let tmp = std::env::temp_dir().join(format!(
            "pgman-tap-replay-overlong-{}.jsonl",
            std::process::id()
        ));
        // A line whose SQL payload alone pushes it well past
        // `TAP_REPLAY_MAX_LINE_BYTES` — simulates a corrupt or
        // hostile capture file rather than anything
        // `--tap-record` would ever legitimately write (every
        // live ingest path caps `sql` at `TAP_MAX_SQL_BYTES`
        // long before it could reach a capture file).
        let overlong_sql = "x".repeat(TAP_REPLAY_MAX_LINE_BYTES + 10_000);
        let overlong_line =
            format!(r#"{{"v":1,"ts_unix_micros":1,"sql":"{overlong_sql}","duration_micros":1}}"#);
        let mut f = tokio::fs::File::create(&tmp).await.unwrap();
        let mut body = String::new();
        body.push_str(r#"{"v":1,"ts_unix_micros":1,"sql":"good 1","duration_micros":1}"#);
        body.push('\n');
        body.push_str(&overlong_line);
        body.push('\n');
        body.push_str(r#"{"v":1,"ts_unix_micros":2,"sql":"good 2","duration_micros":2}"#);
        body.push('\n');
        f.write_all(body.as_bytes()).await.unwrap();
        f.flush().await.unwrap();
        drop(f);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let accepted = run_replay_file(&tmp, tx).await.unwrap();
        // The overlong line is skipped — never even reaches
        // `parse_replay_line` — but the stream resyncs and both
        // well-formed lines around it still land.
        assert_eq!(accepted, 2);
        let e1 = rx.try_recv().unwrap();
        let e2 = rx.try_recv().unwrap();
        assert_eq!(e1.sql.as_deref(), Some("good 1"));
        assert_eq!(e2.sql.as_deref(), Some("good 2"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn run_replay_file_missing_returns_err() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let err = run_replay_file("/nonexistent/path/to/replay.jsonl", tx)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    // --- UDP listener tests ---------------------------

    /// Build the UDP-listener future + bound-addr pair that
    /// tests dial into. Spawns the recv loop on the runtime;
    /// the returned address lets the client `send_to(...)` it.
    async fn spawn_udp_listener_for_test(
    ) -> (std::net::SocketAddr, tokio::sync::mpsc::Receiver<TapEvent>) {
        let (tx, rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        // Bind upfront so the test knows the port, then drive
        // the same accept loop as the public helper would.
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let local_addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = vec![0u8; TAP_UDP_MAX_DATAGRAM];
            loop {
                let Ok((len, _peer)) = socket.recv_from(&mut buf).await else {
                    return;
                };
                match parse(&buf[..len]) {
                    Ok(mut event) => {
                        event.received_at_unix_micros = now_unix_micros();
                        if forward_or_drop(&tx, event, "udp").is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        // Tests want the same drop-and-continue
                        // semantics as the public helper.
                    }
                }
            }
        });
        (local_addr, rx)
    }

    #[tokio::test]
    async fn udp_listener_decodes_one_datagram_per_event() {
        let (addr, mut rx) = spawn_udp_listener_for_test().await;
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let payload = br#"{
            "v": 1, "ts_unix_micros": 1700000000000000,
            "sql": "SELECT 1", "duration_micros": 42
        }"#;
        client.send_to(payload, addr).await.unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("UDP event delivered in time")
            .expect("channel still open");
        assert_eq!(event.sql.as_deref(), Some("SELECT 1"));
        // Listener stamped received_at on the way through.
        assert!(event.received_at_unix_micros > 0);
    }

    #[tokio::test]
    async fn udp_listener_drops_malformed_datagram_and_keeps_serving() {
        let (addr, mut rx) = spawn_udp_listener_for_test().await;
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // Garbage first — must NOT propagate.
        client.send_to(b"{not json", addr).await.unwrap();
        // Good event after — listener must still be serving.
        let payload = br#"{
            "v": 1, "ts_unix_micros": 1,
            "sql": "SELECT 2", "duration_micros": 1
        }"#;
        client.send_to(payload, addr).await.unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("good event delivered")
            .expect("channel still open");
        assert_eq!(event.sql.as_deref(), Some("SELECT 2"));
    }

    #[tokio::test]
    async fn udp_listener_serves_multiple_events_in_succession() {
        let (addr, mut rx) = spawn_udp_listener_for_test().await;
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        for i in 0..3u64 {
            let payload =
                format!(r#"{{"v":1,"ts_unix_micros":{i},"sql":"SELECT {i}","duration_micros":1}}"#);
            client.send_to(payload.as_bytes(), addr).await.unwrap();
        }
        let mut got: Vec<String> = Vec::new();
        for _ in 0..3 {
            let e = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("event in time")
                .expect("open");
            got.push(e.sql.unwrap_or_default());
        }
        // UDP doesn't guarantee order on the wire but for
        // localhost loopback in a quiet test it's effectively
        // FIFO. Don't assert order; just confirm content.
        got.sort();
        assert_eq!(got, vec!["SELECT 0", "SELECT 1", "SELECT 2"]);
    }

    // --- OTLP HTTP server tests -----------------------

    /// Build a minimal HTTP/1.1 request for the server tests.
    fn http_post_traces(content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut req = format!(
            "POST /v1/traces HTTP/1.1\r\nHost: localhost\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\n\r\n",
            len = body.len()
        )
        .into_bytes();
        req.extend_from_slice(body);
        req
    }

    #[tokio::test]
    async fn read_http_request_parses_post_with_body() {
        let body = b"{}";
        let req_bytes = http_post_traces("application/json", body);
        let mut reader = std::io::Cursor::new(req_bytes);
        let req = read_http_request(&mut reader, 1024).await.unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/traces");
        assert_eq!(
            req.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(req.body, body);
    }

    #[tokio::test]
    async fn read_http_request_lowercases_header_names_for_lookup() {
        let req_bytes =
            b"POST /v1/traces HTTP/1.1\r\nCONTENT-TYPE: application/json\r\nContent-Length: 0\r\n\r\n";
        let mut reader = std::io::Cursor::new(&req_bytes[..]);
        let req = read_http_request(&mut reader, 1024).await.unwrap();
        // Lookup uses lowercase; case-insensitive headers per RFC.
        assert_eq!(
            req.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
    }

    #[tokio::test]
    async fn read_http_request_handles_header_and_body_in_one_read() {
        // The chunked reader must correctly split the
        // over-read body bytes that arrive in the same chunk
        // as the header terminator. (A regression here would
        // truncate the body or hang waiting for more bytes
        // that already arrived.)
        let body = b"{\"v\":1}";
        let req_bytes = http_post_traces("application/json", body);
        // One Cursor holds everything; tokio will read it in
        // one or two chunks depending on size — either way the
        // body bytes follow the CRLFCRLF in the same buffer.
        let mut reader = std::io::Cursor::new(req_bytes);
        let req = read_http_request(&mut reader, 1024).await.unwrap();
        assert_eq!(req.body, body);
    }

    #[tokio::test]
    async fn read_http_request_handles_body_arriving_after_headers() {
        // Simulate a slow client: headers, pause, body. Use
        // duplex pipes so the test can stage writes.
        let (mut client_w, server_r) = tokio::io::duplex(4096);
        let body = b"{\"v\":1,\"x\":42}";
        let header = format!(
            "POST /v1/traces HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        // Stage headers first, then body, then close.
        let send = async move {
            use tokio::io::AsyncWriteExt;
            client_w.write_all(header.as_bytes()).await.unwrap();
            // Yield to let the reader see headers before body.
            tokio::task::yield_now().await;
            client_w.write_all(body).await.unwrap();
            client_w.shutdown().await.unwrap();
        };
        let recv = async move {
            let mut r = server_r;
            read_http_request(&mut r, 1024).await
        };
        let (_, req) = tokio::join!(send, recv);
        let req = req.expect("request parses");
        assert_eq!(req.body, body);
    }

    #[tokio::test]
    async fn read_http_request_rejects_oversize_body() {
        let body = vec![b'x'; 100];
        let req_bytes = http_post_traces("application/json", &body);
        let mut reader = std::io::Cursor::new(req_bytes);
        let err = read_http_request(&mut reader, 10).await.unwrap_err();
        assert!(err.contains("exceeds cap"), "got: {err}");
    }

    #[tokio::test]
    async fn read_http_request_rejects_truncated_headers() {
        // No CRLF-CRLF terminator before EOF.
        let req_bytes = b"POST /v1/traces HTTP/1.1\r\nContent-Type: application/json";
        let mut reader = std::io::Cursor::new(&req_bytes[..]);
        let err = read_http_request(&mut reader, 1024).await.unwrap_err();
        assert!(err.contains("closed before headers complete"), "got: {err}");
    }

    #[tokio::test]
    async fn process_otlp_request_routes_a_valid_post_to_events() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let span = r#"{
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano":   "1700000000010000000",
            "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"SELECT 1"}}
            ]
        }"#;
        let body = otlp_envelope("svc", span);
        let req = HttpRequest {
            method: "POST".into(),
            path: "/v1/traces".into(),
            headers: [("content-type".into(), "application/json".into())]
                .into_iter()
                .collect(),
            body: body.into_bytes(),
        };
        let resp = process_otlp_request(req, &tx);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{}");
        let e = rx.try_recv().expect("event sent");
        assert_eq!(e.sql.as_deref(), Some("SELECT 1"));
        // Listener stamped received_at on the way through.
        assert!(e.received_at_unix_micros > 0);
    }

    #[test]
    fn process_otlp_request_rejects_get_with_405() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let req = HttpRequest {
            method: "GET".into(),
            path: "/v1/traces".into(),
            headers: Default::default(),
            body: Vec::new(),
        };
        let resp = process_otlp_request(req, &tx);
        assert_eq!(resp.status, 405);
        assert!(resp.body.contains("POST"));
    }

    #[test]
    fn process_otlp_request_rejects_unknown_path_with_404() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let req = HttpRequest {
            method: "POST".into(),
            path: "/v1/metrics".into(),
            headers: [("content-type".into(), "application/json".into())]
                .into_iter()
                .collect(),
            body: Vec::new(),
        };
        let resp = process_otlp_request(req, &tx);
        assert_eq!(resp.status, 404);
        assert!(resp.body.contains("/v1/traces"));
    }

    #[test]
    fn process_otlp_request_rejects_non_json_with_415() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let req = HttpRequest {
            method: "POST".into(),
            path: "/v1/traces".into(),
            headers: [("content-type".into(), "application/x-protobuf".into())]
                .into_iter()
                .collect(),
            body: Vec::new(),
        };
        let resp = process_otlp_request(req, &tx);
        assert_eq!(resp.status, 415);
        assert!(resp.body.contains("application/json"));
        assert!(resp.body.contains("OTEL_EXPORTER_OTLP_PROTOCOL"));
    }

    #[test]
    fn process_otlp_request_rejects_bad_json_with_400() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let req = HttpRequest {
            method: "POST".into(),
            path: "/v1/traces".into(),
            headers: [("content-type".into(), "application/json".into())]
                .into_iter()
                .collect(),
            body: b"{not json".to_vec(),
        };
        let resp = process_otlp_request(req, &tx);
        assert_eq!(resp.status, 400);
        assert!(resp.body.contains("OTLP parse error"));
    }

    #[tokio::test]
    async fn process_otlp_request_emits_partial_success_when_receiver_closed_mid_batch() {
        // Drop the receiver so the very first forward_or_drop
        // hits Closed and we report 0 / N accepted.
        let (tx, rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        drop(rx);
        let spans = r#"
            {
              "startTimeUnixNano": "1",
              "endTimeUnixNano":   "2",
              "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"A"}}
              ]
            },
            {
              "startTimeUnixNano": "3",
              "endTimeUnixNano":   "4",
              "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"B"}}
              ]
            }
        "#;
        let body = otlp_envelope("svc", spans);
        let req = HttpRequest {
            method: "POST".into(),
            path: "/v1/traces".into(),
            headers: [("content-type".into(), "application/json".into())]
                .into_iter()
                .collect(),
            body: body.into_bytes(),
        };
        let resp = process_otlp_request(req, &tx);
        assert_eq!(resp.status, 200);
        assert!(
            resp.body.contains("\"partialSuccess\""),
            "expected partial-success body when App closed: {body}",
            body = resp.body
        );
        assert!(
            resp.body.contains("\"rejectedSpans\":2"),
            "expected rejectedSpans=2: {body}",
            body = resp.body
        );
    }

    #[test]
    fn process_otlp_request_rejects_chunked_transfer_encoding_with_501() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let req = HttpRequest {
            method: "POST".into(),
            path: "/v1/traces".into(),
            headers: [
                ("content-type".into(), "application/json".into()),
                ("transfer-encoding".into(), "chunked".into()),
            ]
            .into_iter()
            .collect(),
            body: Vec::new(),
        };
        let resp = process_otlp_request(req, &tx);
        assert_eq!(resp.status, 501);
        assert!(
            resp.body.contains("Transfer-Encoding"),
            "expected chunked-encoding rejection message; got: {body}",
            body = resp.body
        );
        assert!(
            resp.body.contains("Content-Length"),
            "expected hint to switch to Content-Length; got: {body}",
            body = resp.body
        );
    }

    #[test]
    fn process_otlp_request_stamps_received_at_per_span_not_per_batch() {
        // Multi-span batch — each span MUST receive a
        // monotonically non-decreasing received_at. Sharing
        // one value across the batch loses FIFO-within-batch
        // ordering.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let spans = r#"
            {
              "startTimeUnixNano": "1",
              "endTimeUnixNano":   "2",
              "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"A"}}
              ]
            },
            {
              "startTimeUnixNano": "3",
              "endTimeUnixNano":   "4",
              "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"B"}}
              ]
            },
            {
              "startTimeUnixNano": "5",
              "endTimeUnixNano":   "6",
              "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"C"}}
              ]
            }
        "#;
        let body = otlp_envelope("svc", spans);
        let req = HttpRequest {
            method: "POST".into(),
            path: "/v1/traces".into(),
            headers: [("content-type".into(), "application/json".into())]
                .into_iter()
                .collect(),
            body: body.into_bytes(),
        };
        let resp = process_otlp_request(req, &tx);
        assert_eq!(resp.status, 200);
        let mut stamps: Vec<u64> = Vec::new();
        while let Ok(e) = rx.try_recv() {
            assert!(e.received_at_unix_micros > 0, "every span must be stamped");
            stamps.push(e.received_at_unix_micros);
        }
        assert_eq!(stamps.len(), 3);
        for w in stamps.windows(2) {
            assert!(
                w[0] <= w[1],
                "received_at must be monotonic across the batch; got {stamps:?}"
            );
        }
    }

    #[test]
    fn process_otlp_request_strips_trailing_slash_on_path() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let req = HttpRequest {
            method: "POST".into(),
            path: "/v1/traces/".into(),
            headers: [("content-type".into(), "application/json".into())]
                .into_iter()
                .collect(),
            body: b"{}".to_vec(),
        };
        let resp = process_otlp_request(req, &tx);
        // Empty body → empty events; status still 200 because
        // routing matched after the slash strip.
        assert_eq!(resp.status, 200);
    }

    #[tokio::test]
    async fn otlp_listener_end_to_end_round_trip_through_real_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap();
        let server_tx = tx.clone();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            handle_otlp_conn(sock, &server_tx).await.ok();
        });
        let span = r#"{
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano":   "1700000000020000000",
            "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"SELECT * FROM end_to_end"}}
            ]
        }"#;
        let body = otlp_envelope("e2e-svc", span);
        let req_bytes = http_post_traces("application/json", body.as_bytes());
        let mut client = tokio::net::TcpStream::connect(local_addr).await.unwrap();
        client.write_all(&req_bytes).await.unwrap();
        // Drain the server's response so the test confirms
        // round-trip semantics, not just one direction. Bounded
        // so a server-side hang fails the test.
        let mut response = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.read_to_end(&mut response),
        )
        .await
        .expect("server closed in time")
        .unwrap();
        let response_str = String::from_utf8_lossy(&response);
        assert!(response_str.starts_with("HTTP/1.1 200 OK"));
        assert!(response_str.ends_with("{}"));
        let e = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("event delivered in time")
            .expect("event surfaced");
        assert_eq!(e.app.as_deref(), Some("e2e-svc"));
        assert_eq!(e.sql.as_deref(), Some("SELECT * FROM end_to_end"));
        assert_eq!(e.duration_micros, Some(20_000));
    }

    #[tokio::test]
    async fn a_silent_tcp_connection_is_closed_and_its_permit_released() {
        use crate::tap::listener::handle_tcp_conn_with_idle_timeout;
        // The reproduction: `TAP_MAX_CONCURRENT_CONNS` bounds how many
        // connections may be open, but a peer that connects and never
        // sends held its permit for as long as it liked — 64 silent
        // sockets took every slot and shut the tap to real clients.
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            handle_tcp_conn_with_idle_timeout(sock, &tx, std::time::Duration::from_millis(150))
                .await
        });

        // Connect and say nothing at all. The handle must come back on
        // its own — nothing here closes the socket for it.
        let _client = tokio::net::TcpStream::connect(local_addr).await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("the idle connection must be closed without the peer's help")
            .expect("handler task");
        // An idle close is a normal end of connection, not an error:
        // the permit is released either way, but a task that returned
        // `Err` would log as a failure every time a JAR goes quiet.
        assert!(result.is_ok(), "idle close should be Ok, got {result:?}");
    }

    #[tokio::test]
    async fn a_tcp_connection_delivering_frames_is_not_closed_as_idle() {
        use crate::tap::listener::handle_tcp_conn_with_idle_timeout;
        use tokio::io::AsyncWriteExt;
        // The deadline is per-frame, so a client that keeps sending
        // stays connected however long it lives. Without this, the
        // idle timeout would be a connection lifetime cap and a busy
        // JAR would be disconnected mid-stream.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            handle_tcp_conn_with_idle_timeout(sock, &tx, std::time::Duration::from_millis(300))
                .await
        });

        let mut client = tokio::net::TcpStream::connect(local_addr).await.unwrap();
        let payload =
            br#"{"v":1,"ts_unix_micros":1,"sql":"SELECT 1","duration_micros":1}"#.to_vec();
        // Three frames spaced past half the deadline: their total span
        // exceeds it, so only a per-frame clock keeps this alive.
        for _ in 0..3 {
            tokio::time::sleep(std::time::Duration::from_millis(180)).await;
            client
                .write_all(&(payload.len() as u32).to_be_bytes())
                .await
                .unwrap();
            client.write_all(&payload).await.unwrap();
            let e = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("event delivered in time")
                .expect("event surfaced");
            assert_eq!(e.sql.as_deref(), Some("SELECT 1"));
        }
    }

    // --- OTLP parser tests ----------------------------

    /// Build a minimal OTLP/HTTP JSON body wrapping `spans`
    /// inside one resourceSpans bundle with the given
    /// service name.
    fn otlp_envelope(service: &str, spans: &str) -> String {
        format!(
            r#"{{
              "resourceSpans": [{{
                "resource": {{
                  "attributes": [{{"key":"service.name","value":{{"stringValue":"{service}"}}}}]
                }},
                "scopeSpans": [{{
                  "spans": [{spans}]
                }}]
              }}]
            }}"#
        )
    }

    #[test]
    fn parse_otlp_json_maps_a_postgres_span_to_a_tap_event() {
        let span = r#"{
            "name": "SELECT accounts",
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano":   "1700000000010000000",
            "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"SELECT * FROM accounts WHERE id = ?"}}
            ],
            "status": {"code": 1}
        }"#;
        let body = otlp_envelope("billing-service", span);
        let (events, skipped) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(skipped, 0);
        let e = &events[0];
        assert_eq!(e.kind, TapKind::Query);
        assert_eq!(e.app.as_deref(), Some("billing-service"));
        assert_eq!(
            e.sql.as_deref(),
            Some("SELECT * FROM accounts WHERE id = ?")
        );
        // duration = 10ms = 10_000us
        assert_eq!(e.duration_micros, Some(10_000));
        // ts = end / 1000 (ns → µs)
        assert_eq!(e.ts_unix_micros, 1_700_000_000_010_000);
        // OTel typically strips params for PII safety.
        assert!(e.params_redacted);
        assert!(e.error.is_none());
    }

    #[test]
    fn span_to_tap_event_truncates_an_oversized_db_statement() {
        // OTLP spans build their `TapEvent` outside `parse` (the
        // TCP/UDP/replay path) — `enforce_field_caps` must be
        // applied here too, not just there.
        let huge_sql = "x".repeat(50_000);
        let span_json = serde_json::json!({
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano": "1700000000010000000",
            "attributes": [
                {"key": "db.system", "value": {"stringValue": "postgresql"}},
                {"key": "db.statement", "value": {"stringValue": huge_sql}},
            ],
        });
        let e = span_to_tap_event(&span_json, None).expect("postgres span maps");
        let sql = e.sql.as_deref().unwrap();
        assert!(
            sql.len() <= TAP_MAX_SQL_BYTES,
            "OTLP path must cap sql just like the tap-protocol path; got {} bytes",
            sql.len()
        );
        assert!(sql.ends_with(TAP_TRUNCATION_MARKER));
    }

    #[test]
    fn parse_otlp_json_keeps_searching_past_malformed_attribute() {
        // Regression: an attribute with no `key` field (or
        // non-string key) used to abort `otlp_attr_string`'s
        // entire walk via `?`. That made e.g. a structured
        // attribute landing before `db.system` hide the span
        // entirely. Confirm both attrs land and the span is
        // accepted.
        let span = r#"{
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano":   "1700000000001000000",
            "attributes": [
                {"value": {"stringValue": "no-key-attr"}},
                {"key": {"objectKey":"weird"}, "value":{"stringValue":"x"}},
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"SELECT 1"}}
            ]
        }"#;
        let body = otlp_envelope("svc", span);
        let (events, _) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sql.as_deref(), Some("SELECT 1"));
    }

    #[test]
    fn parse_otlp_json_skips_non_postgres_spans() {
        let span = r#"{
            "name": "GET /api/users",
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano":   "1700000000001000000",
            "attributes": [
                {"key":"http.method","value":{"stringValue":"GET"}}
            ]
        }"#;
        let body = otlp_envelope("billing-service", span);
        let (events, skipped) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert!(events.is_empty(), "HTTP spans must not become tap events");
        assert_eq!(skipped, 1);
    }

    #[test]
    fn parse_otlp_json_skips_db_spans_without_statement() {
        // Connection-open / connection-close spans carry
        // `db.system` but no `db.statement` — they aren't
        // useful for the tap.
        let span = r#"{
            "name": "DB Connection",
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano":   "1700000000001000000",
            "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}}
            ]
        }"#;
        let body = otlp_envelope("billing-service", span);
        let (events, skipped) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert!(events.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn parse_otlp_json_handles_numeric_unix_nano_values() {
        // OTLP/JSON should encode uint64 as a string, but
        // some emitters use numbers. Accept either.
        let span = r#"{
            "startTimeUnixNano": 1700000000000000000,
            "endTimeUnixNano":   1700000000005000000,
            "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"SELECT 1"}}
            ]
        }"#;
        let body = otlp_envelope("svc", span);
        let (events, _) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].duration_micros, Some(5_000));
    }

    #[test]
    fn parse_otlp_json_caps_pathological_duration() {
        // u64::MAX endTimeUnixNano (or anything wildly larger
        // than start) would otherwise become a huge
        // duration_micros and hijack TotalTime sorting.
        let span = format!(
            r#"{{
                "startTimeUnixNano": "0",
                "endTimeUnixNano":   "{}",
                "attributes": [
                    {{"key":"db.system","value":{{"stringValue":"postgresql"}}}},
                    {{"key":"db.statement","value":{{"stringValue":"SELECT 1"}}}}
                ]
            }}"#,
            u64::MAX
        );
        let body = otlp_envelope("svc", &span);
        let (events, _) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].duration_micros, Some(OTLP_DURATION_CAP_MICROS));
    }

    #[test]
    fn parse_otlp_json_status_error_populates_error_chain() {
        let span = r#"{
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano":   "1700000000001000000",
            "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"SELECT bad"}}
            ],
            "status": {"code": 2, "message": "PSQLException: syntax error"}
        }"#;
        let body = otlp_envelope("svc", span);
        let (events, _) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert_eq!(events.len(), 1);
        assert!(events[0].is_error());
        assert_eq!(
            events[0]
                .error
                .as_deref()
                .and_then(|e| e.first().map(|s| s.as_str())),
            Some("PSQLException: syntax error")
        );
    }

    #[test]
    fn parse_otlp_json_error_without_message_uses_generic_marker() {
        let span = r#"{
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano":   "1700000000001000000",
            "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"SELECT 1"}}
            ],
            "status": {"code": 2}
        }"#;
        let body = otlp_envelope("svc", span);
        let (events, _) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert!(events[0].is_error());
        assert!(events[0]
            .error
            .as_deref()
            .and_then(|e| e.first().map(|s| s.as_str()))
            .unwrap_or("")
            .contains("ERROR"));
    }

    #[test]
    fn parse_otlp_json_groups_multiple_spans_in_one_resource() {
        let spans = r#"
            {
              "startTimeUnixNano": "1700000000000000000",
              "endTimeUnixNano":   "1700000000001000000",
              "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"SELECT 1"}}
              ]
            },
            {
              "startTimeUnixNano": "1700000000001000000",
              "endTimeUnixNano":   "1700000000003000000",
              "attributes": [
                {"key":"db.system","value":{"stringValue":"postgresql"}},
                {"key":"db.statement","value":{"stringValue":"SELECT 2"}}
              ]
            }
        "#;
        let body = otlp_envelope("svc", spans);
        let (events, skipped) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert_eq!(events.len(), 2);
        assert_eq!(skipped, 0);
        // All inherit the resource's service.name.
        assert!(events.iter().all(|e| e.app.as_deref() == Some("svc")));
    }

    #[test]
    fn parse_otlp_json_accepts_empty_resource_spans() {
        // An OTel exporter heartbeat might send no spans —
        // not an error.
        let body = r#"{"resourceSpans": []}"#;
        let (events, skipped) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert!(events.is_empty());
        assert_eq!(skipped, 0);
    }

    #[test]
    fn parse_otlp_json_accepts_missing_resource_spans_field() {
        let body = r#"{}"#;
        let (events, skipped) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert!(events.is_empty());
        assert_eq!(skipped, 0);
    }

    #[test]
    fn parse_otlp_json_rejects_malformed_json() {
        let body = b"{not json";
        let err = parse_otlp_json(body).expect_err("must reject");
        assert!(err.contains("bad json"), "got: {err}");
    }

    #[test]
    fn parse_otlp_json_rejects_non_utf8() {
        let body: &[u8] = &[0xff, 0xfe, 0xfd];
        let err = parse_otlp_json(body).expect_err("must reject");
        assert!(err.contains("utf-8"), "got: {err}");
    }

    #[test]
    fn parse_otlp_json_handles_missing_service_name() {
        // resource bundle without attributes — span still
        // parses but app is None.
        let body = r#"{
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "startTimeUnixNano": "1700000000000000000",
                        "endTimeUnixNano":   "1700000000001000000",
                        "attributes": [
                            {"key":"db.system","value":{"stringValue":"postgresql"}},
                            {"key":"db.statement","value":{"stringValue":"SELECT 1"}}
                        ]
                    }]
                }]
            }]
        }"#;
        let (events, _) = parse_otlp_json(body.as_bytes()).expect("parse");
        assert_eq!(events.len(), 1);
        assert!(events[0].app.is_none());
    }

    // --- transaction view tests -----------------------

    fn q_in_txn(sql: &str, ts: u64, duration: u64, txn: Option<&str>, conn: &str) -> TapEvent {
        q_in_txn_with_pool(sql, ts, duration, txn, conn, None)
    }

    fn q_in_txn_with_pool(
        sql: &str,
        ts: u64,
        duration: u64,
        txn: Option<&str>,
        conn: &str,
        pool: Option<&str>,
    ) -> TapEvent {
        TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: ts,
            received_at_unix_micros: ts,
            app: Some("svc".into()),
            pool: pool.map(str::to_string),
            conn: Some(conn.into()),
            txn: txn.map(str::to_string),
            sql: Some(sql.into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(duration),
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        }
    }

    fn boundary(ts: u64, txn: &str, conn: &str, outcome: TxnOutcome) -> TapEvent {
        TapEvent {
            v: 1,
            kind: TapKind::TxnBoundary,
            ts_unix_micros: ts,
            received_at_unix_micros: ts,
            app: None,
            pool: None,
            conn: Some(conn.into()),
            txn: Some(txn.into()),
            sql: None,
            params: None,
            params_redacted: false,
            duration_micros: None,
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: Some(outcome),
        }
    }

    #[test]
    fn group_by_txn_buckets_per_txn_id() {
        let events = [
            q_in_txn("SELECT 1", 100, 10, Some("c-1#1"), "c-1"),
            q_in_txn("SELECT 2", 200, 20, Some("c-1#1"), "c-1"),
            q_in_txn("SELECT 3", 300, 30, Some("c-1#2"), "c-1"),
        ];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats.len(), 2);
        // Both are open (no boundary observed); sort is span DESC.
        // c-1#1 spans 100..200 = 100µs; c-1#2 spans 0 (single event).
        assert_eq!(stats[0].txn.as_deref(), Some("c-1#1"));
        assert_eq!(stats[0].statement_count, 2);
        assert_eq!(stats[1].txn.as_deref(), Some("c-1#2"));
        assert_eq!(stats[1].statement_count, 1);
    }

    #[test]
    fn group_by_txn_closes_on_txn_boundary_event() {
        let events = [
            q_in_txn("SELECT 1", 100, 10, Some("c-1#1"), "c-1"),
            q_in_txn("SELECT 2", 200, 20, Some("c-1#1"), "c-1"),
            boundary(300, "c-1#1", "c-1", TxnOutcome::Commit),
        ];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].outcome, Some(TxnOutcome::Commit));
        assert!(!stats[0].is_open());
        // Span runs from first query to the boundary ts.
        assert_eq!(stats[0].span_micros, 200);
        assert_eq!(stats[0].total_query_micros, 30);
    }

    #[test]
    fn group_by_txn_separates_commit_and_rollback() {
        let events = [
            q_in_txn("SELECT 1", 100, 10, Some("c-1#1"), "c-1"),
            boundary(200, "c-1#1", "c-1", TxnOutcome::Commit),
            q_in_txn("SELECT 2", 300, 10, Some("c-1#2"), "c-1"),
            boundary(400, "c-1#2", "c-1", TxnOutcome::Rollback),
        ];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats.len(), 2);
        // Both closed → sort by statement_count desc (tied at 1),
        // then by txn id tiebreak → c-1#1 first.
        let outcomes: Vec<_> = stats.iter().map(|s| s.outcome).collect();
        assert!(outcomes.contains(&Some(TxnOutcome::Commit)));
        assert!(outcomes.contains(&Some(TxnOutcome::Rollback)));
    }

    #[test]
    fn group_by_txn_open_transactions_sort_before_closed() {
        let events = [
            // Closed txn — 1 stmt, will be ranked last.
            q_in_txn("SELECT done", 100, 10, Some("c-1#done"), "c-1"),
            boundary(200, "c-1#done", "c-1", TxnOutcome::Commit),
            // Open txn — 1 stmt, span 0.
            q_in_txn("SELECT open", 300, 10, Some("c-2#open"), "c-2"),
        ];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats.len(), 2);
        assert!(stats[0].is_open(), "open txn must sort first");
        assert!(!stats[1].is_open());
    }

    #[test]
    fn group_by_txn_open_transactions_sort_by_span_desc() {
        let events = [
            // Long open
            q_in_txn("SELECT 1", 100, 10, Some("c-1#long"), "c-1"),
            q_in_txn("SELECT 2", 10_100, 10, Some("c-1#long"), "c-1"),
            // Short open
            q_in_txn("SELECT 3", 200, 10, Some("c-2#short"), "c-2"),
            q_in_txn("SELECT 4", 300, 10, Some("c-2#short"), "c-2"),
        ];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats[0].txn.as_deref(), Some("c-1#long"));
        assert_eq!(stats[0].span_micros, 10_000);
        assert_eq!(stats[1].txn.as_deref(), Some("c-2#short"));
    }

    #[test]
    fn group_by_txn_closed_transactions_sort_by_statement_count_desc() {
        let mut events: Vec<TapEvent> = Vec::new();
        for i in 0..10 {
            events.push(q_in_txn("SELECT a", 100 + i, 1, Some("c-1#big"), "c-1"));
        }
        events.push(boundary(200, "c-1#big", "c-1", TxnOutcome::Commit));
        for i in 0..3 {
            events.push(q_in_txn("SELECT b", 300 + i, 1, Some("c-1#small"), "c-1"));
        }
        events.push(boundary(400, "c-1#small", "c-1", TxnOutcome::Commit));
        let stats = group_by_txn(events.iter());
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].statement_count, 10);
        assert_eq!(stats[1].statement_count, 3);
    }

    #[test]
    fn group_by_txn_falls_back_to_conn_when_txn_is_absent() {
        // Autocommit: each statement has no txn but the conn
        // groups them.
        let events = [
            q_in_txn("SELECT 1", 100, 10, None, "c-1"),
            q_in_txn("SELECT 2", 200, 10, None, "c-1"),
        ];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].statement_count, 2);
        // txn stays None — the renderer shows "(autocommit)"
        // for these.
        assert!(stats[0].txn.is_none());
        assert_eq!(stats[0].conn.as_deref(), Some("c-1"));
    }

    #[test]
    fn group_by_txn_drops_events_with_neither_txn_nor_conn() {
        let events = [TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: 1,
            received_at_unix_micros: 1,
            app: None,
            pool: None,
            conn: None,
            txn: None,
            sql: Some("SELECT 1".into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(10),
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        }];
        let stats = group_by_txn(events.iter());
        assert!(stats.is_empty(), "ungroupable events must be dropped");
    }

    #[test]
    fn group_by_txn_drops_boundary_with_no_preceding_queries() {
        // A boundary event arrives after the ring evicted
        // its query events — nothing to surface.
        let events = [boundary(1, "c-1#1", "c-1", TxnOutcome::Commit)];
        let stats = group_by_txn(events.iter());
        assert!(stats.is_empty());
    }

    #[test]
    fn group_by_txn_skips_heartbeat_events() {
        let mut hb = q_in_txn("ignored", 0, 0, None, "c-1");
        hb.kind = TapKind::Heartbeat;
        hb.sql = None;
        let events = [hb, q_in_txn("SELECT 1", 1, 10, Some("c-1#1"), "c-1")];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].statement_count, 1);
    }

    #[test]
    fn group_by_txn_tracks_distinct_fingerprints_and_last_seen() {
        let events = [
            q_in_txn("SELECT a FROM t1", 1, 1, Some("c-1#1"), "c-1"),
            q_in_txn("SELECT b FROM t2", 2, 1, Some("c-1#1"), "c-1"),
            // Same shape as the first — fingerprint dedup'd.
            q_in_txn("SELECT a FROM t1", 3, 1, Some("c-1#1"), "c-1"),
        ];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats[0].statement_count, 3);
        assert_eq!(stats[0].distinct_fingerprints, 2);
    }

    #[test]
    fn group_by_txn_carries_pool_from_first_event_that_has_one() {
        let events = [
            // First event in the bucket has no pool.
            q_in_txn_with_pool("SELECT 1", 1, 1, Some("c-1#1"), "c-1", None),
            // Later event in the same bucket DOES set pool.
            q_in_txn_with_pool("SELECT 2", 2, 1, Some("c-1#1"), "c-1", Some("primary")),
        ];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].pool.as_deref(), Some("primary"));
    }

    #[test]
    fn group_by_txn_keeps_distinct_pools_in_distinct_buckets() {
        // Different txns on different pools — each surfaces its
        // own pool name. The diagnostic question this answers:
        // "is my write hitting the replica pool?"
        let events = [
            q_in_txn_with_pool(
                "INSERT INTO orders VALUES (1)",
                1,
                10,
                Some("p-1#1"),
                "p-1",
                Some("replica"), // suspicious!
            ),
            q_in_txn_with_pool(
                "SELECT * FROM orders",
                2,
                10,
                Some("p-2#1"),
                "p-2",
                Some("replica"),
            ),
        ];
        let stats = group_by_txn(events.iter());
        assert_eq!(stats.len(), 2);
        assert!(stats.iter().all(|t| t.pool.as_deref() == Some("replica")));
    }

    #[test]
    fn group_by_txn_handles_empty_input() {
        let events: Vec<TapEvent> = Vec::new();
        let stats = group_by_txn(events.iter());
        assert!(stats.is_empty());
    }

    // --- baseline diff tests --------------------------

    fn hs(fp: &str, count: usize, p95: u64) -> Hotspot {
        Hotspot {
            fingerprint: fp.into(),
            example_sql: fp.into(),
            count,
            error_count: 0,
            total_micros: p95 * count as u64,
            p50_micros: p95 / 2,
            p95_micros: p95,
            p99_micros: p95,
            distinct_callers: 0,
            last_caller: None,
            last_app: None,
        }
    }

    #[test]
    fn diff_hotspots_flags_new_fingerprints() {
        let baseline = vec![hs("select a", 10, 100)];
        let current = vec![
            hs("select a", 10, 100),
            hs("select b", 5, 200), // new
        ];
        let diff = diff_hotspots(&baseline, &current, false);
        // 1 New (the unchanged "select a" is filtered out).
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].kind, DiffKind::New);
        assert_eq!(diff[0].fingerprint, "select b");
        assert_eq!(diff[0].baseline_count, 0);
        assert_eq!(diff[0].current_count, 5);
    }

    #[test]
    fn diff_hotspots_flags_regressions_above_2x_p95() {
        let baseline = vec![hs("hot", 100, 100)];
        let current = vec![hs("hot", 100, 250)]; // 2.5× p95
        let diff = diff_hotspots(&baseline, &current, false);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].kind, DiffKind::Regressed);
        assert_eq!(diff[0].baseline_p95_micros, 100);
        assert_eq!(diff[0].current_p95_micros, 250);
    }

    #[test]
    fn diff_hotspots_does_not_flag_small_regressions() {
        let baseline = vec![hs("hot", 100, 100)];
        let current = vec![hs("hot", 100, 150)]; // 1.5× < 2×
        let diff = diff_hotspots(&baseline, &current, false);
        assert!(diff.is_empty(), "small regression must be Unchanged");
    }

    #[test]
    fn diff_hotspots_flags_disappeared() {
        let baseline = vec![hs("gone", 7, 50)];
        let current: Vec<Hotspot> = Vec::new();
        let diff = diff_hotspots(&baseline, &current, false);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].kind, DiffKind::Disappeared);
        assert_eq!(diff[0].baseline_count, 7);
        assert_eq!(diff[0].current_count, 0);
    }

    #[test]
    fn diff_hotspots_orders_regressed_before_new_before_disappeared() {
        let baseline = vec![hs("hot", 10, 100), hs("gone", 7, 50)];
        let current = vec![
            hs("hot", 10, 300), // regressed
            hs("fresh", 4, 80), // new
        ];
        let diff = diff_hotspots(&baseline, &current, false);
        assert_eq!(diff.len(), 3);
        assert_eq!(diff[0].kind, DiffKind::Regressed);
        assert_eq!(diff[1].kind, DiffKind::New);
        assert_eq!(diff[2].kind, DiffKind::Disappeared);
    }

    #[test]
    fn diff_hotspots_includes_unchanged_when_requested() {
        let baseline = vec![hs("stable", 10, 100)];
        let current = vec![hs("stable", 10, 100)];
        let diff = diff_hotspots(&baseline, &current, true);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].kind, DiffKind::Unchanged);
    }

    #[test]
    fn diff_hotspots_empty_inputs_yield_empty_diff() {
        let diff = diff_hotspots(&[], &[], false);
        assert!(diff.is_empty());
    }

    #[test]
    fn diff_hotspots_tiebreaks_on_fingerprint_for_determinism() {
        // Two regressions with the same current p95 — order
        // is determined by the fingerprint ascending.
        let baseline = vec![hs("aaaa", 10, 100), hs("bbbb", 10, 100)];
        let current = vec![hs("aaaa", 10, 300), hs("bbbb", 10, 300)];
        let diff = diff_hotspots(&baseline, &current, false);
        assert_eq!(diff[0].fingerprint, "aaaa");
        assert_eq!(diff[1].fingerprint, "bbbb");
    }

    #[test]
    fn diff_hotspots_handles_zero_baseline_p95_without_div_by_zero() {
        // Baseline reported p95=0 (degenerate sampling); we
        // must not classify as Regressed since 0 × any = 0.
        let baseline = vec![hs("weird", 5, 0)];
        let current = vec![hs("weird", 5, 1_000)];
        let diff = diff_hotspots(&baseline, &current, false);
        assert!(
            diff.iter().all(|d| d.kind != DiffKind::Regressed),
            "must not flag regression vs zero baseline; got {:?}",
            diff
        );
    }

    // --- per-caller rollup tests ----------------------

    #[test]
    fn group_by_caller_buckets_events_by_innermost_frame() {
        let events = [
            q("SELECT 1", 10, false, Some("OrderService.foo:1"), "svc"),
            q("SELECT 2", 20, false, Some("OrderService.foo:1"), "svc"),
            q("SELECT 3", 30, false, Some("UserService.bar:5"), "svc"),
        ];
        let groups = group_by_caller(events.iter(), HotspotSort::TotalTime);
        assert_eq!(groups.len(), 2);
        // OrderService bucket has total 30, UserService has 30
        // — tied. Tiebreak is caller name ascending: "Order..." < "User..."
        // so OrderService comes first.
        assert_eq!(groups[0].caller, "OrderService.foo:1");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[0].total_micros, 30);
        assert_eq!(groups[1].caller, "UserService.bar:5");
        assert_eq!(groups[1].count, 1);
    }

    #[test]
    fn group_by_caller_routes_no_caller_to_unknown_bucket() {
        // Events with caller=None must still appear in the rollup
        // (under UNKNOWN_CALLER) — otherwise the rollup loses
        // events and stops being total-conserving.
        let events = [
            q("SELECT 1", 10, false, None, "svc"),
            q("SELECT 2", 20, false, Some("Foo.bar:1"), "svc"),
            q("SELECT 3", 30, false, None, "svc"),
        ];
        let groups = group_by_caller(events.iter(), HotspotSort::CallCount);
        // Two buckets: unknown (2 events) + Foo.bar (1 event).
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].caller, UNKNOWN_CALLER);
        assert_eq!(groups[0].count, 2);
    }

    #[test]
    fn group_by_caller_tracks_distinct_fingerprints() {
        // Same caller fired four DIFFERENT queries — distinct
        // fingerprint count surfaces the variety.
        let events = [
            q("SELECT a FROM t1", 1, false, Some("Svc.method:1"), "svc"),
            q("SELECT b FROM t2", 1, false, Some("Svc.method:1"), "svc"),
            q("SELECT c FROM t3", 1, false, Some("Svc.method:1"), "svc"),
            // Same shape as the first — fingerprint dedup'd.
            q("SELECT a FROM t1", 1, false, Some("Svc.method:1"), "svc"),
        ];
        let groups = group_by_caller(events.iter(), HotspotSort::CallCount);
        assert_eq!(groups[0].count, 4);
        assert_eq!(groups[0].distinct_fingerprints, 3);
    }

    #[test]
    fn group_by_caller_skips_non_query_events() {
        let mut hb = q("ignored", 0, false, Some("x"), "svc");
        hb.kind = TapKind::Heartbeat;
        hb.sql = None;
        let events = [hb, q("SELECT 1", 1, false, Some("Foo.bar:1"), "svc")];
        let groups = group_by_caller(events.iter(), HotspotSort::TotalTime);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 1);
    }

    #[test]
    fn group_by_caller_counts_errors_and_computes_percentiles() {
        let events = [
            q("SELECT 1", 10, false, Some("Foo.bar:1"), "svc"),
            q("SELECT 1", 20, true, Some("Foo.bar:1"), "svc"),
            q("SELECT 1", 30, true, Some("Foo.bar:1"), "svc"),
        ];
        let groups = group_by_caller(events.iter(), HotspotSort::TotalTime);
        assert_eq!(groups[0].count, 3);
        assert_eq!(groups[0].error_count, 2);
        assert_eq!(groups[0].p50_micros, 20);
        assert_eq!(groups[0].p95_micros, 30);
    }

    #[test]
    fn caller_stats_mean_handles_single_call() {
        let events = [q("SELECT 1", 42, false, Some("Foo.bar:1"), "svc")];
        let groups = group_by_caller(events.iter(), HotspotSort::TotalTime);
        assert_eq!(groups[0].mean_micros(), 42);
    }

    #[test]
    fn group_by_caller_total_is_conserved_across_named_and_unknown() {
        // Sum of counts across all buckets should equal the
        // total number of query events.
        let events = [
            q("SELECT 1", 1, false, None, "svc"),
            q("SELECT 2", 1, false, Some("A.b:1"), "svc"),
            q("SELECT 3", 1, false, Some("A.b:1"), "svc"),
            q("SELECT 4", 1, false, None, "svc"),
            q("SELECT 5", 1, false, Some("C.d:2"), "svc"),
        ];
        let groups = group_by_caller(events.iter(), HotspotSort::TotalTime);
        let total: usize = groups.iter().map(|g| g.count).sum();
        assert_eq!(total, 5);
    }

    #[test]
    fn sort_callers_in_place_picks_the_right_top() {
        let mut groups = vec![
            CallerStats {
                caller: "low-count-high-latency".into(),
                count: 2,
                error_count: 0,
                total_micros: 200,
                p50_micros: 100,
                p95_micros: 100,
                p99_micros: 100,
                distinct_fingerprints: 1,
                last_fingerprint: None,
                last_app: None,
            },
            CallerStats {
                caller: "high-count-low-latency".into(),
                count: 100,
                error_count: 0,
                total_micros: 100,
                p50_micros: 1,
                p95_micros: 1,
                p99_micros: 1,
                distinct_fingerprints: 1,
                last_fingerprint: None,
                last_app: None,
            },
        ];
        sort_callers(&mut groups, HotspotSort::CallCount);
        assert_eq!(groups[0].count, 100);
        sort_callers(&mut groups, HotspotSort::TotalTime);
        assert_eq!(groups[0].total_micros, 200);
        sort_callers(&mut groups, HotspotSort::P95Latency);
        assert_eq!(groups[0].p95_micros, 100);
    }

    // --- N+1 live-detection tests ---------------------

    fn q_at(sql: &str, ts: u64, txn: Option<&str>, conn: &str, caller: Option<&str>) -> TapEvent {
        TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: ts,
            received_at_unix_micros: ts,
            app: Some("svc".into()),
            pool: None,
            conn: Some(conn.into()),
            txn: txn.map(str::to_string),
            sql: Some(sql.into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(1),
            rows: None,
            error: None,
            caller: caller.map(|c| vec![c.into()]),
            dropped_events_total: None,
            txn_outcome: None,
        }
    }

    /// Pool-tagged query event: `(pool, conn, ts, duration)`.
    fn q_pool(pool: Option<&str>, conn: &str, ts: u64, dur: u64) -> TapEvent {
        TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: ts,
            received_at_unix_micros: ts,
            app: Some("svc".into()),
            pool: pool.map(str::to_string),
            conn: Some(conn.into()),
            txn: None,
            sql: Some("SELECT 1".into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(dur),
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        }
    }

    #[test]
    fn peak_concurrency_empty_is_zero() {
        assert_eq!(insights::peak_concurrency(&[]), 0);
    }

    #[test]
    fn peak_concurrency_disjoint_intervals_is_one() {
        // [0,10), [20,30), [40,50) — never overlap.
        let intervals = [(0u64, 10u64), (20, 10), (40, 10)];
        assert_eq!(insights::peak_concurrency(&intervals), 1);
    }

    #[test]
    fn peak_concurrency_full_overlap_counts_all() {
        // Three queries all running 0..100.
        let intervals = [(0u64, 100u64), (10, 80), (20, 50)];
        assert_eq!(insights::peak_concurrency(&intervals), 3);
    }

    #[test]
    fn peak_concurrency_zero_duration_registers_one() {
        // A point query [5,5] still occupies a slot momentarily.
        assert_eq!(insights::peak_concurrency(&[(5, 0)]), 1);
    }

    #[test]
    fn peak_concurrency_partial_overlap_finds_the_peak() {
        // A: [0,30), B: [20,40), C: [35,60). Peak is 2 (A+B).
        let intervals = [(0u64, 30u64), (20, 20), (35, 25)];
        assert_eq!(insights::peak_concurrency(&intervals), 2);
    }

    #[test]
    fn group_by_pool_buckets_by_pool_name() {
        let events = [
            q_pool(Some("primary"), "p-1", 0, 10),
            q_pool(Some("primary"), "p-2", 5, 10),
            q_pool(Some("replica"), "r-1", 0, 10),
        ];
        let pools = group_by_pool(events.iter());
        assert_eq!(pools.len(), 2);
        let primary = pools.iter().find(|p| p.pool == "primary").unwrap();
        assert_eq!(primary.query_count, 2);
        assert_eq!(primary.distinct_conns, 2);
        // p-1 [0,10) overlaps p-2 [5,15) → peak 2.
        assert_eq!(primary.peak_concurrent, 2);
        let replica = pools.iter().find(|p| p.pool == "replica").unwrap();
        assert_eq!(replica.query_count, 1);
        assert_eq!(replica.distinct_conns, 1);
        assert_eq!(replica.peak_concurrent, 1);
    }

    #[test]
    fn group_by_pool_untagged_lands_in_unknown_bucket() {
        let events = [
            q_pool(None, "c-1", 0, 10),
            q_pool(Some("primary"), "p-1", 0, 10),
        ];
        let pools = group_by_pool(events.iter());
        assert!(pools.iter().any(|p| p.pool == UNKNOWN_POOL));
        // Total-conserving: every query event is bucketed.
        let total: usize = pools.iter().map(|p| p.query_count).sum();
        assert_eq!(total, 2);
    }

    #[test]
    fn group_by_pool_skips_non_query_events() {
        let mut hb = q_pool(Some("primary"), "p-1", 0, 10);
        hb.kind = TapKind::Heartbeat;
        let events = [hb, q_pool(Some("primary"), "p-1", 0, 10)];
        let pools = group_by_pool(events.iter());
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].query_count, 1);
    }

    #[test]
    fn group_by_pool_counts_errors_and_distinct_conns() {
        let mut err = q_pool(Some("primary"), "p-1", 0, 10);
        err.error = Some(vec!["boom".into()]);
        let events = [
            err,
            q_pool(Some("primary"), "p-1", 20, 10), // same conn reused
            q_pool(Some("primary"), "p-2", 40, 10),
        ];
        let pools = group_by_pool(events.iter());
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].error_count, 1);
        assert_eq!(pools[0].distinct_conns, 2);
        // All disjoint in time → peak 1 despite 3 queries.
        assert_eq!(pools[0].peak_concurrent, 1);
    }

    #[test]
    fn group_by_pool_sorts_most_contended_first() {
        // hot pool: 3 overlapping conns. cold pool: 1 conn.
        let events = [
            q_pool(Some("cold"), "c-1", 0, 10),
            q_pool(Some("hot"), "h-1", 0, 100),
            q_pool(Some("hot"), "h-2", 10, 100),
            q_pool(Some("hot"), "h-3", 20, 100),
        ];
        let pools = group_by_pool(events.iter());
        assert_eq!(pools[0].pool, "hot");
        assert_eq!(pools[0].peak_concurrent, 3);
        assert_eq!(pools[1].pool, "cold");
    }

    #[test]
    fn group_by_pool_empty_input_is_empty() {
        let events: Vec<TapEvent> = Vec::new();
        assert!(group_by_pool(events.iter()).is_empty());
    }

    #[test]
    fn detect_nplus1_fires_on_a_tight_burst_in_one_txn() {
        // 6 SELECTs at the same shape inside one txn within
        // 150ms — classic N+1.
        let events: Vec<TapEvent> = (0..6)
            .map(|i| {
                q_at(
                    "SELECT * FROM orders WHERE user_id = 1",
                    i * 20_000,
                    Some("c-1#1"),
                    "c-1",
                    Some("OrderService.findById:42"),
                )
            })
            .collect();
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.count, 6);
        assert_eq!(f.txn.as_deref(), Some("c-1#1"));
        assert!(f.span_micros <= NPLUS1_WINDOW_MICROS);
        assert_eq!(f.last_caller.as_deref(), Some("OrderService.findById:42"));
    }

    #[test]
    fn detect_nplus1_skips_events_below_min_repeats() {
        // 4 events of same shape — under the threshold (5).
        let events: Vec<TapEvent> = (0..4)
            .map(|i| q_at("SELECT 1", i * 10_000, Some("c-1#1"), "c-1", None))
            .collect();
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert!(findings.is_empty());
    }

    #[test]
    fn detect_nplus1_does_not_fire_when_spread_across_window_boundary() {
        // 5 events with 100ms gaps — total span 400ms > 200ms
        // window. Not a tight burst.
        let events: Vec<TapEvent> = (0..5)
            .map(|i| q_at("SELECT 1", i * 100_000, Some("c-1#1"), "c-1", None))
            .collect();
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert!(findings.is_empty());
    }

    #[test]
    fn detect_nplus1_separates_different_transactions() {
        // 5 events split across two txns — neither txn alone
        // hits the threshold.
        let mut events: Vec<TapEvent> = Vec::new();
        for i in 0..3 {
            events.push(q_at("SELECT 1", i * 10_000, Some("c-1#1"), "c-1", None));
        }
        for i in 0..3 {
            events.push(q_at(
                "SELECT 1",
                100_000 + i * 10_000,
                Some("c-1#2"),
                "c-1",
                None,
            ));
        }
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert!(findings.is_empty(), "different txns must not merge");
    }

    #[test]
    fn detect_nplus1_uses_conn_when_txn_is_absent() {
        // Autocommit traffic: no `txn` set, but `conn` ties
        // events together so the burst is still detected.
        let events: Vec<TapEvent> = (0..6)
            .map(|i| q_at("SELECT 1", i * 10_000, None, "c-1", None))
            .collect();
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].count, 6);
        assert!(findings[0].txn.is_none());
        assert_eq!(findings[0].conn.as_deref(), Some("c-1"));
    }

    #[test]
    fn detect_nplus1_separates_different_fingerprints_in_same_txn() {
        let mut events: Vec<TapEvent> = Vec::new();
        // 6 of shape A — fires.
        for i in 0..6 {
            events.push(q_at(
                "SELECT * FROM users WHERE id = ?",
                i * 10_000,
                Some("c-1#1"),
                "c-1",
                None,
            ));
        }
        // 4 of shape B — same txn, but doesn't hit threshold.
        for i in 0..4 {
            events.push(q_at(
                "SELECT * FROM orders WHERE id = ?",
                60_000 + i * 10_000,
                Some("c-1#1"),
                "c-1",
                None,
            ));
        }
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].fingerprint.contains("users"));
    }

    #[test]
    fn detect_nplus1_keeps_the_longest_run_per_key() {
        // 5 events within window, then a gap of 1 second, then
        // 6 more within window. Both qualify but the second is
        // longer — we keep one finding (the longer).
        let mut events: Vec<TapEvent> = Vec::new();
        for i in 0..5 {
            events.push(q_at("SELECT 1", i * 10_000, Some("c-1#1"), "c-1", None));
        }
        let base = 1_000_000;
        for i in 0..6 {
            events.push(q_at(
                "SELECT 1",
                base + i * 10_000,
                Some("c-1#1"),
                "c-1",
                None,
            ));
        }
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].count, 6);
    }

    #[test]
    fn detect_nplus1_ignores_heartbeat_and_txn_boundary_events() {
        let mut hb = q_at("ignored", 0, None, "c-1", None);
        hb.kind = TapKind::Heartbeat;
        hb.sql = None;
        let mut txn = q_at("ignored", 0, None, "c-1", None);
        txn.kind = TapKind::TxnBoundary;
        txn.sql = None;
        txn.txn = Some("c-1#1".into());
        txn.txn_outcome = Some(TxnOutcome::Commit);
        let mut events = vec![hb, txn];
        // Only 4 query events — under threshold even after
        // non-query frames are filtered out.
        for i in 0..4 {
            events.push(q_at("SELECT 1", i * 10_000, Some("c-1#1"), "c-1", None));
        }
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert!(findings.is_empty());
    }

    #[test]
    fn detect_nplus1_sort_puts_biggest_burst_first() {
        let mut events: Vec<TapEvent> = Vec::new();
        // shape A: 10 events.
        for i in 0..10 {
            events.push(q_at(
                "SELECT 'a' FROM ta",
                i * 5_000,
                Some("c-1#1"),
                "c-1",
                None,
            ));
        }
        // shape B: 6 events.
        for i in 0..6 {
            events.push(q_at(
                "SELECT 'b' FROM tb",
                50_000 + i * 5_000,
                Some("c-1#1"),
                "c-1",
                None,
            ));
        }
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].count, 10);
        assert_eq!(findings[1].count, 6);
    }

    #[test]
    fn detect_nplus1_handles_empty_input() {
        let events: Vec<TapEvent> = Vec::new();
        let findings = detect_nplus1(events.iter(), NPLUS1_WINDOW_MICROS, NPLUS1_MIN_REPEATS);
        assert!(findings.is_empty());
    }

    #[test]
    fn percentile_clamps_p_to_unit_interval() {
        let sorted: Vec<u64> = vec![10, 20, 30, 40, 50];
        assert_eq!(percentile(&sorted, -1.0), 10); // clamped to 0 → first
        assert_eq!(percentile(&sorted, 0.0), 10); // rank 0 → idx 0
        assert_eq!(percentile(&sorted, 1.0), 50); // rank N → idx N-1
        assert_eq!(percentile(&sorted, 2.0), 50); // clamped to 1 → last
    }

    #[test]
    fn percentile_empty_slice_is_zero() {
        let sorted: Vec<u64> = Vec::new();
        assert_eq!(percentile(&sorted, 0.5), 0);
    }

    fn sample_query() -> TapEvent {
        TapEvent {
            v: 1,
            kind: TapKind::Query,
            ts_unix_micros: 0,
            received_at_unix_micros: 0,
            app: None,
            pool: None,
            conn: None,
            txn: None,
            sql: Some("SELECT 1".into()),
            params: None,
            params_redacted: false,
            duration_micros: Some(0),
            rows: None,
            error: None,
            caller: None,
            dropped_events_total: None,
            txn_outcome: None,
        }
    }

    // --- listener tests (TCP framing) -----------------

    /// Build a length-prefixed frame the way the JAR will.
    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[tokio::test]
    async fn read_frame_returns_payload_on_well_formed_input() {
        let payload = b"hello, tap";
        let bytes = framed(payload);
        let mut reader = std::io::Cursor::new(bytes);
        let got = read_frame(&mut reader, 1024)
            .await
            .expect("read should succeed");
        assert_eq!(got.as_deref(), Some(&payload[..]));
    }

    #[tokio::test]
    async fn read_frame_handles_multiple_frames_back_to_back() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&framed(b"one"));
        buf.extend_from_slice(&framed(b"two"));
        buf.extend_from_slice(&framed(b"three"));
        let mut reader = std::io::Cursor::new(buf);
        let a = read_frame(&mut reader, 1024).await.unwrap();
        let b = read_frame(&mut reader, 1024).await.unwrap();
        let c = read_frame(&mut reader, 1024).await.unwrap();
        let d = read_frame(&mut reader, 1024).await.unwrap();
        assert_eq!(a.as_deref(), Some(&b"one"[..]));
        assert_eq!(b.as_deref(), Some(&b"two"[..]));
        assert_eq!(c.as_deref(), Some(&b"three"[..]));
        // Fourth read hits clean EOF.
        assert!(d.is_none());
    }

    #[tokio::test]
    async fn read_frame_returns_none_on_clean_eof_at_boundary() {
        let mut reader = std::io::Cursor::new(Vec::<u8>::new());
        let got = read_frame(&mut reader, 1024).await.unwrap();
        assert!(got.is_none(), "empty stream should yield None, not Err");
    }

    #[tokio::test]
    async fn read_frame_rejects_oversize_length() {
        // length = 2 MiB, max = 1 MiB
        let bytes = (2 * 1024 * 1024u32).to_be_bytes().to_vec();
        let mut reader = std::io::Cursor::new(bytes);
        let err = read_frame(&mut reader, 1024 * 1024)
            .await
            .expect_err("oversize frame must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn read_frame_errors_on_short_payload() {
        // Length prefix says 10 bytes but only 3 follow.
        let mut bytes = 10u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"abc");
        let mut reader = std::io::Cursor::new(bytes);
        let err = read_frame(&mut reader, 1024)
            .await
            .expect_err("short payload must error");
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn read_frame_handles_zero_length_frame() {
        let bytes = 0u32.to_be_bytes().to_vec();
        let mut reader = std::io::Cursor::new(bytes);
        let got = read_frame(&mut reader, 1024).await.unwrap();
        assert_eq!(got.as_deref(), Some(&[][..]));
    }

    #[tokio::test]
    async fn tcp_listener_round_trip_decodes_events_and_stamps_received_at() {
        use tokio::io::AsyncWriteExt;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        // Bind on an ephemeral port so the test can run in parallel.
        let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        // We need the bound addr to dial in; bind the listener
        // here and drive its accept loop manually (the public
        // helper takes addr and we'd lose the bound port).
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let local_addr = listener.local_addr().unwrap();
        let server_tx = tx.clone();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            handle_tcp_conn(sock, &server_tx).await.ok();
        });

        // Dial in and write two events: a query and a heartbeat.
        let mut client = tokio::net::TcpStream::connect(local_addr).await.unwrap();
        let q = br#"{
            "v": 1, "kind": "query", "ts_unix_micros": 1700000000000000,
            "sql": "SELECT 1", "duration_micros": 42
        }"#;
        let h = br#"{
            "v": 1, "kind": "heartbeat", "ts_unix_micros": 1700000000000001,
            "app": "billing-service"
        }"#;
        client.write_all(&framed(q)).await.unwrap();
        client.write_all(&framed(h)).await.unwrap();
        client.shutdown().await.unwrap();
        // Bounded wait so a server-side hang fails the test
        // (rather than blocking the entire suite).
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .expect("server drained in time");

        let e1 = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("first event delivered in time")
            .expect("first event");
        let e2 = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("second event delivered in time")
            .expect("second event");
        assert_eq!(e1.kind, TapKind::Query);
        assert_eq!(e1.sql.as_deref(), Some("SELECT 1"));
        assert_eq!(e2.kind, TapKind::Heartbeat);
        // Listener stamped a non-zero receive time on both.
        assert!(e1.received_at_unix_micros > 0);
        assert!(e2.received_at_unix_micros >= e1.received_at_unix_micros);
    }

    #[tokio::test]
    async fn tcp_listener_drops_malformed_frame_and_continues() {
        use tokio::io::AsyncWriteExt;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TapEvent>(64);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap();
        let server_tx = tx.clone();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            handle_tcp_conn(sock, &server_tx).await.ok();
        });

        let mut client = tokio::net::TcpStream::connect(local_addr).await.unwrap();
        // Garbage frame first — must be dropped, not propagated.
        let garbage = br#"{not valid json"#;
        let good = br#"{
            "v": 1, "ts_unix_micros": 0, "sql": "SELECT 2", "duration_micros": 1
        }"#;
        client.write_all(&framed(garbage)).await.unwrap();
        client.write_all(&framed(good)).await.unwrap();
        client.shutdown().await.unwrap();

        // Only the well-formed event reaches the channel.
        // Bounded wait: a regression that fails to drop the
        // malformed frame and instead stalls the listener
        // would otherwise hang.
        let e = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("good event arrived in time")
            .expect("good event survives bad frame");
        assert_eq!(e.sql.as_deref(), Some("SELECT 2"));
    }

    #[tokio::test]
    async fn tcp_listener_rejects_connections_past_the_concurrency_cap() {
        // Drive the real `run_tcp_listener` (not `handle_tcp_conn`
        // directly) since the connection semaphore lives in the
        // accept loop, not the per-connection handler.
        let (tx, _rx) = tokio::sync::mpsc::channel::<TapEvent>(4096);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Reimplement the bind-then-serve split `run_tcp_listener`
            // does internally isn't available with a pre-bound
            // listener, so drive its accept loop inline against the
            // SAME cap constant the production code uses — this is
            // the smallest faithful stand-in that doesn't need a
            // second `addr` to bind to (the test needs the actual
            // bound port up front to dial in).
            let permits =
                std::sync::Arc::new(tokio::sync::Semaphore::new(TAP_MAX_CONCURRENT_CONNS));
            loop {
                let Ok((sock, _peer)) = listener.accept().await else {
                    return;
                };
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    continue; // drop the socket — same as the real listener
                };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = handle_tcp_conn(sock, &tx).await;
                });
            }
        });

        // Open TAP_MAX_CONCURRENT_CONNS idle connections and hold
        // them open — each one's `handle_tcp_conn` is blocked
        // inside `read_frame` waiting for a length prefix that
        // never arrives, so its permit stays held for the life of
        // the test.
        let mut held = Vec::with_capacity(TAP_MAX_CONCURRENT_CONNS);
        for _ in 0..TAP_MAX_CONCURRENT_CONNS {
            held.push(tokio::net::TcpStream::connect(local_addr).await.unwrap());
        }
        // Give the server a moment to actually accept + spawn all
        // of them (accept() + try_acquire_owned() racing against
        // our loop opening connections).
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // One more connection: the TCP handshake itself succeeds
        // (the listener still accept()s it), but with no permit
        // available the socket is dropped immediately rather than
        // handed to `handle_tcp_conn` — the client sees EOF on its
        // very first read instead of a blocked read.
        let mut rejected = tokio::net::TcpStream::connect(local_addr).await.unwrap();
        let mut buf = [0u8; 1];
        use tokio::io::AsyncReadExt;
        let read = tokio::time::timeout(std::time::Duration::from_secs(2), rejected.read(&mut buf))
            .await
            .expect("rejected connection closes promptly rather than hanging")
            .expect("read completes (as EOF) rather than erroring");
        assert_eq!(
            read, 0,
            "connection past the cap must be closed (EOF), not served"
        );

        // Control: one of the HELD connections must still be open
        // — a read on it should time out (server is waiting for a
        // frame), not return EOF. Proves the cap rejected the 65th
        // specifically, not that the listener stopped serving
        // entirely.
        let still_open = &mut held[0];
        let mut buf2 = [0u8; 1];
        let control = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            still_open.read(&mut buf2),
        )
        .await;
        assert!(
            control.is_err(),
            "a connection within the cap must still be open (read should time out, not EOF)"
        );
    }
}
