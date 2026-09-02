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
/// Release metadata derived from `CHANGELOG.md` at build time.
///
/// `build.rs` is the real consumer and reaches this file through
/// `include!`, independently of the module system — so the library
/// itself compiles it only under `cfg(test)`, where its own tests
/// exercise the same parser the build script ran.
#[cfg(test)]
pub(crate) mod release_meta;
pub mod report;
pub mod safety;
pub mod saved;
pub mod splash;
pub mod tap;
pub mod theme;
pub mod tui;
pub mod tunnel;
pub mod ui;
pub mod update_check;
pub mod upgrade;
pub mod util;

/// The current release's date, `YYYY-MM-DD`, parsed by `build.rs` from
/// `CHANGELOG.md`'s `## [<version>] — <date>` heading for the crate's
/// version. Empty when the working `CARGO_PKG_VERSION` has no dated
/// heading yet — callers show the version alone rather than a wrong or
/// invented date.
pub const RELEASE_DATE: &str = env!("PGMAN_RELEASE_DATE");
