//! Async messages delivered back to the app from spawned tasks.
//!
//! Every variant carries the `generation` it was launched at; the app drops
//! results whose generation is stale after a context switch (see CLAUDE.md).

use crate::grid::Grid;
use crate::query::schema::SchemaCache;
use std::sync::Arc;

/// What a run through the pager (`AppMsg::QueryOk`'s `paging`) says
/// about the result's cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPaging {
    /// The statement, for `M` to re-run once the cursor closes.
    pub sql: String,
    /// `Some` while the cursor stays open for `M`.
    pub cursor: Option<crate::query::paging::Limits>,
    /// The session was inside a transaction, so it ran unpaged.
    pub in_transaction: bool,
    /// On a standby: no cursor held past the first page.
    pub standby: bool,
}

/// `M`'s answer (`AppMsg::PageLoaded`).
#[derive(Debug)]
pub enum PageOutcome {
    /// The next page of the open cursor.
    More(crate::query::paging::Page),
    /// A re-run behind a fresh cursor, replacing the held rows.
    Rerun(crate::query::paging::Opened),
}

#[derive(Debug)]
pub enum AppMsg {
    /// Connection succeeded and the bootstrap query returned. The client is
    /// shared (held in `App`) so subsequent queries can run on the same
    /// session.
    Booted {
        generation: u64,
        server_version: String,
        /// The server is a standby (`pg_is_in_recovery()`); `None` if
        /// unknown.
        in_recovery: Option<bool>,
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
    /// The live connection closed under the App: the server ended the
    /// session (`pg_terminate_backend`, a recovery conflict, a restart)
    /// or the network went. `reason` carries the server's own message
    /// when it sent one ([`connection_lost_reason`]).
    ConnectionLost { generation: u64, reason: String },
    /// A user-initiated query (Run / EXPLAIN / EXPLAIN ANALYZE) finished.
    QueryOk {
        generation: u64,
        grid: Grid,
        /// Where each column came from (`conn::run_statement_with_origins`);
        /// empty for any result but a plain editor run's.
        origins: Vec<Option<crate::grid::ColumnOrigin>>,
        kind_label: String,
        /// True if the run wrapped the statement in a transaction that's still
        /// open — the app should prompt for commit/rollback.
        tx_open_after: bool,
        /// `Some` when the run went to the pager (a plain `SELECT`,
        /// docs/backlog/paging.md): whether its cursor stayed open.
        paging: Option<RunPaging>,
    },
    /// `M`'s answer for the result `result_id`: the next page, or the
    /// re-run that replaces the rows once the cursor had closed.
    PageLoaded {
        generation: u64,
        result_id: u64,
        outcome: Result<PageOutcome, crate::conn::QueryErr>,
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
    /// A cell edit's UPDATE changed its one row. `value` is the column
    /// as `RETURNING` rendered it; `tx` says whether it is committed.
    CellUpdated {
        generation: u64,
        pending: crate::app::PendingCellEdit,
        value: crate::grid::Cell,
        tx: crate::conn::EditTx,
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
        result: Result<Vec<crate::query::slow_queries::SlowQueryRow>, crate::conn::QueryErr>,
    },
    /// The environments gather finished. Not tied to a connection, so it
    /// bypasses the generation filter like tap events.
    EnvironmentsLoaded(Result<Vec<crate::inventory::EnvReport>, String>),
    /// One environment's probes finished. `seq` names the listing they
    /// belong to; `report` its index in it.
    EnvProbed {
        seq: u64,
        report: usize,
        outcomes: Vec<crate::inventory::probe::Outcome>,
    },
    /// A server's log errors arrived (or didn't).
    EnvLogs {
        environment: String,
        server: String,
        result: Result<Vec<crate::inventory::rds_logs::LogError>, String>,
    },
    /// A patch plan arrived (or didn't).
    EnvPatchPlanned {
        environment: String,
        server: String,
        database: String,
        result: Result<crate::inventory::patching::PatchPlan, String>,
    },
    /// The confirmed patches ran.
    EnvPatched {
        plan: crate::inventory::patching::PatchPlan,
        report: crate::inventory::patching::ApplyReport,
    },
    /// Environment discovery finished. Not tied to a connection;
    /// matched to its discovery by `token`, so the result of one the
    /// operator backed out of doesn't land in the next.
    EnvDiscovered {
        token: u64,
        result: Result<crate::inventory::discover::Found, String>,
    },
    /// An environment connect's secret was read (or not). Matched to its
    /// connect by `token`, not the generation: no connection owns it yet.
    EnvSecretResolved {
        token: u64,
        result: Result<crate::creds::aws_secrets::SecretCreds, String>,
    },
    /// `pg_stat_activity` snapshot finished loading.
    SessionsLoaded {
        generation: u64,
        result: Result<Vec<crate::query::sessions::SessionRow>, crate::conn::QueryErr>,
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
    /// The config advisor's checks (ADV…) finished. Merged into the
    /// wizard's findings the same way as [`AppMsg::LiveLintLoaded`].
    AdvisorLoaded {
        generation: u64,
        result: Result<crate::query::advisor::Report, String>,
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
    /// The schema browser's DDL copy (`d`) read its table's pieces
    /// (or didn't). `table` is `schema.table`, for the status line.
    DdlLoaded {
        generation: u64,
        table: String,
        result: Result<crate::query::ddl::TableDdl, String>,
    },
    /// `:readonly on|off` was applied to the live session (or wasn't).
    ReadOnlyApplied {
        generation: u64,
        want: bool,
        result: Result<(), String>,
    },
    /// The crates.io update check finished — `Some` only when a
    /// strictly-newer release than the running binary exists.
    /// Fired at most once per session, independent of the DB
    /// connection, so it is NOT
    /// generation-tagged. `generation()` returns 0 for this
    /// variant.
    UpdateCheck(Option<crate::update_check::LatestRelease>),
    /// The terminal hung up (SIGHUP, a closed tab) or the process was
    /// asked to stop (SIGTERM). Quits through the normal exit path, so
    /// the draft, history and saved queries are written as on `q`.
    Terminate,
}

/// What the App says when the connection closed under it: the server's
/// parting `FATAL`, if it sent one, else that the connection closed.
/// Pure.
pub fn connection_lost_reason(fatal: Option<&crate::conn::NoticeMsg>) -> String {
    match fatal {
        Some(n) => format!("connection lost · {}", n.message),
        None => "connection lost · the connection to the server closed".to_string(),
    }
}

impl AppMsg {
    /// The generation this message was produced for. Tap events
    /// return 0 (they aren't tied to a connection generation).
    pub fn generation(&self) -> u64 {
        match self {
            AppMsg::Booted { generation, .. }
            | AppMsg::BootFailed { generation, .. }
            | AppMsg::ConnectionLost { generation, .. }
            | AppMsg::QueryOk { generation, .. }
            | AppMsg::PageLoaded { generation, .. }
            | AppMsg::QueryFailed { generation, .. }
            | AppMsg::TxClosed { generation, .. }
            | AppMsg::CellUpdated { generation, .. }
            | AppMsg::Notice { generation, .. }
            | AppMsg::Notification { generation, .. }
            | AppMsg::SlowQueriesLoaded { generation, .. }
            | AppMsg::SessionsLoaded { generation, .. }
            | AppMsg::CostPreviewLoaded { generation, .. }
            | AppMsg::SchemaRefreshed { generation, .. }
            | AppMsg::LiveLintLoaded { generation, .. }
            | AppMsg::AdvisorLoaded { generation, .. }
            | AppMsg::DdlLoaded { generation, .. }
            | AppMsg::ReadOnlyApplied { generation, .. } => *generation,
            AppMsg::UpdateCheck(_)
            | AppMsg::Terminate
            | AppMsg::EnvironmentsLoaded(_)
            | AppMsg::EnvSecretResolved { .. }
            | AppMsg::EnvDiscovered { .. }
            | AppMsg::EnvProbed { .. }
            | AppMsg::EnvLogs { .. }
            | AppMsg::EnvPatchPlanned { .. }
            | AppMsg::EnvPatched { .. } => 0,
        }
    }
}
