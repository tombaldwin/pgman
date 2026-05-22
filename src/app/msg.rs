//! Async messages delivered back to the app from spawned tasks.
//!
//! Every variant carries the `generation` it was launched at; the app drops
//! results whose generation is stale after a context switch (see CLAUDE.md).

use crate::grid::Grid;

#[derive(Debug)]
pub enum AppMsg {
    /// Connection succeeded and the bootstrap query returned.
    Booted {
        generation: u64,
        server_version: String,
        grid: Grid,
    },
    /// Connection or the bootstrap query failed.
    BootFailed { generation: u64, error: String },
}

impl AppMsg {
    /// The generation this message was produced for.
    pub fn generation(&self) -> u64 {
        match self {
            AppMsg::Booted { generation, .. } | AppMsg::BootFailed { generation, .. } => {
                *generation
            }
        }
    }
}
