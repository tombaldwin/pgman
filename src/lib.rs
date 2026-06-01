//! pgman — a k9s-style Postgres TUI for Java / AWS shops.
//!
//! The crate is split lib + bin: this library holds the testable logic, and
//! `main.rs` is a thin binary. See `CLAUDE.md` for working rules and
//! `BACKLOG.md` for the milestone plan.

pub mod app;
pub mod batch;
pub mod conn;
pub mod creds;
pub mod dbunit;
pub mod demo;
// `font_probe` now lives in the `tb-tui-common` crate (shared with
// ebman). Re-exported here so existing `pgman::font_probe::…` call
// sites and tests keep working untouched.
pub use tui_common::font_probe;
pub mod grid;
pub mod project;
pub mod query;
pub mod report;
pub mod safety;
pub mod saved;
pub mod splash;
pub mod tap;
pub mod theme;
pub mod tui;
pub mod tunnel;
pub mod ui;
pub mod upgrade;
pub mod util;
