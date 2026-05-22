//! pgman — a k9s-style Postgres TUI for Java / AWS shops.
//!
//! The crate is split lib + bin: this library holds the testable logic, and
//! `main.rs` is a thin binary. See `CLAUDE.md` for working rules and
//! `BACKLOG.md` for the milestone plan.

pub mod conn;
pub mod creds;
pub mod query;
pub mod safety;
pub mod splash;
pub mod theme;
pub mod util;
