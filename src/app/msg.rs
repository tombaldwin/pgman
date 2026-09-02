//! Async messages delivered back to the app from spawned tasks.
//!
//! Every variant carries the `generation` it was launched at; the app drops
//! results whose generation is stale after a context switch (see CLAUDE.md).

use crate::grid::Grid;
use crate::query::schema::SchemaCache;
use std::sync::Arc;

#[derive(Debug)]
pub enum AppMsg {
    /// Connection succeeded and the bootstrap query returned. The client is
    /// shared (held in `App`) so subsequent queries can run on the same
    /// session.
    Booted {
        generation: u64,
        server_version: String,
        grid: Grid,
        client: Arc<tokio_postgres::Client>,
        schema_cache: SchemaCache,
        /// SSH tunnel kept alive for the session when the connection
        /// went via a bastion. App owns it after this message lands;
        /// dropping it terminates the ssh subprocess.
        tunnel: Option<crate::tunnel::SshTunnel>,
    },
    /// Connection or the bootstrap query failed.
    BootFailed { generation: u64, error: String },
    /// A user-initiated query (Run / EXPLAIN / EXPLAIN ANALYZE) finished.
    QueryOk {
        generation: u64,
        grid: Grid,
        kind_label: String,
        /// True if the run wrapped the statement in a transaction that's still
        /// open — the app should prompt for commit/rollback.
        tx_open_after: bool,
    },
    /// A user-initiated query failed.
    QueryFailed {
        generation: u64,
        error: String,
        /// 1-indexed character position into the submitted SQL when
        /// Postgres flagged a syntax error there. App jumps the editor
        /// cursor to this position so the operator sees the offending
        /// token highlighted.
        position: Option<u32>,
        /// Full server-side error detail (hint / detail / where /
        /// affected schema/table/column/constraint/type). `None`
        /// for non-Postgres failures (TLS, IO, our own validation).
        /// Stashed on App so the rich-error overlay can render it.
        detail: Option<crate::conn::QueryErrDetail>,
    },
    /// A `COMMIT` or `ROLLBACK` of the open transaction finished.
    TxClosed {
        generation: u64,
        committed: bool,
        error: Option<String>,
    },
    /// Server-emitted notice (`RAISE NOTICE`, `RAISE WARNING`, …)
    /// piped through the connection driver. Generation-tagged so a
    /// stale notice from the previous connection (still draining
    /// after the operator reconnected) doesn't surface as if the
    /// new session raised it.
    Notice {
        generation: u64,
        notice: crate::conn::NoticeMsg,
    },
    /// A `NOTIFY` arrival from the server for a channel the
    /// operator subscribed to with `LISTEN`. Appended to App's
    /// notification ring; surfaced in `Mode::Notifications`.
    Notification {
        generation: u64,
        notification: crate::conn::NotificationMsg,
    },
    /// `pg_stat_statements` snapshot finished loading.
    SlowQueriesLoaded {
        generation: u64,
        result: Result<Vec<crate::query::slow_queries::SlowQueryRow>, String>,
    },
    /// `pg_stat_activity` snapshot finished loading.
    SessionsLoaded {
        generation: u64,
        result: Result<Vec<crate::query::sessions::SessionRow>, String>,
    },
    /// Schema-lint live-query checks (LINT101+) finished. Merged
    /// into `schema_lint_findings` if the operator is still on
    /// the lint panel; silently dropped otherwise (a follow-on
    /// open re-fires the fetch). Failures surface in the status
    /// footer but don't disturb the already-displayed pure
    /// findings.
    LiveLintLoaded {
        generation: u64,
        result: Result<Vec<crate::query::lint::Finding>, String>,
    },
    /// Pre-flight `EXPLAIN (FORMAT JSON)` finished. The handler
    /// decides between proceeding directly (estimate under
    /// threshold) and opening a Confirm prompt (over threshold).
    /// `estimated` carries either the top-node row estimate or an
    /// error string (EXPLAIN itself failed); either way the run
    /// proceeds — a failed pre-flight isn't a hard stop.
    CostPreviewLoaded {
        generation: u64,
        sql: String,
        decision: crate::safety::Decision,
        estimated: Result<f64, String>,
        threshold: u64,
    },
    /// A post-DDL schema re-fetch finished. Replaces `App.schema_cache`
    /// so completion / schema browser / lint / FK-nav reflect a
    /// `CREATE`/`ALTER`/`DROP` run from the editor without a full
    /// reconnect. Generation-tagged so a refetch from a prior connection
    /// can't clobber the new one.
    SchemaRefreshed {
        generation: u64,
        schema_cache: SchemaCache,
    },
    /// One JDBC-tap event from the pgman-tap JAR (query,
    /// heartbeat, or txn boundary). The tap listener is bound
    /// at app startup and is independent of the DB connection,
    /// so tap events are NOT generation-tagged — they always
    /// process. `generation()` returns 0 for this variant.
    TapEvent { event: crate::tap::TapEvent },
    /// The crates.io update check finished — `Some` only when a
    /// strictly-newer release than the running binary exists.
    /// Fired at most once per session, independent of the DB
    /// connection, so (like `TapEvent`) it is NOT
    /// generation-tagged. `generation()` returns 0 for this
    /// variant.
    UpdateCheck(Option<crate::update_check::LatestRelease>),
}

impl AppMsg {
    /// The generation this message was produced for. Tap events
    /// return 0 (they aren't tied to a connection generation).
    pub fn generation(&self) -> u64 {
        match self {
            AppMsg::Booted { generation, .. }
            | AppMsg::BootFailed { generation, .. }
            | AppMsg::QueryOk { generation, .. }
            | AppMsg::QueryFailed { generation, .. }
            | AppMsg::TxClosed { generation, .. }
            | AppMsg::Notice { generation, .. }
            | AppMsg::Notification { generation, .. }
            | AppMsg::SlowQueriesLoaded { generation, .. }
            | AppMsg::SessionsLoaded { generation, .. }
            | AppMsg::CostPreviewLoaded { generation, .. }
            | AppMsg::SchemaRefreshed { generation, .. }
            | AppMsg::LiveLintLoaded { generation, .. } => *generation,
            AppMsg::TapEvent { .. } | AppMsg::UpdateCheck(_) => 0,
        }
    }
}
