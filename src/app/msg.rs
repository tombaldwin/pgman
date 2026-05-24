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
}

impl AppMsg {
    /// The generation this message was produced for.
    pub fn generation(&self) -> u64 {
        match self {
            AppMsg::Booted { generation, .. }
            | AppMsg::BootFailed { generation, .. }
            | AppMsg::QueryOk { generation, .. }
            | AppMsg::QueryFailed { generation, .. }
            | AppMsg::TxClosed { generation, .. }
            | AppMsg::Notice { generation, .. }
            | AppMsg::SlowQueriesLoaded { generation, .. }
            | AppMsg::SessionsLoaded { generation, .. } => *generation,
        }
    }
}
