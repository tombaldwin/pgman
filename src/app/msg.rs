//! Async messages delivered back to the app from spawned tasks.
//!
//! Every variant carries the `generation` it was launched at; the app drops
//! results whose generation is stale after a context switch (see CLAUDE.md).

use crate::grid::Grid;
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
    },
    /// Connection or the bootstrap query failed.
    BootFailed { generation: u64, error: String },
    /// A user-initiated query (Run / EXPLAIN / EXPLAIN ANALYZE) finished.
    QueryOk {
        generation: u64,
        grid: Grid,
        kind_label: String,
    },
    /// A user-initiated query failed.
    QueryFailed { generation: u64, error: String },
}

impl AppMsg {
    /// The generation this message was produced for.
    pub fn generation(&self) -> u64 {
        match self {
            AppMsg::Booted { generation, .. }
            | AppMsg::BootFailed { generation, .. }
            | AppMsg::QueryOk { generation, .. }
            | AppMsg::QueryFailed { generation, .. } => *generation,
        }
    }
}
