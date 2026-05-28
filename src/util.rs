//! pgman-specific path helpers. The neutral primitives
//! (`parse_bool`, `write_atomic`) live in the shared
//! `tb-tui-common` crate; callers go through
//! `tui_common::util::*` directly.

use std::path::PathBuf;

pub fn config_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/pgman");
        return p;
    }
    PathBuf::from(".")
}

pub fn cache_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".cache/pgman");
        return p;
    }
    PathBuf::from(".")
}

/// Directory for persistent app state — query history, the editor
/// draft auto-save, etc. Distinct from `cache_dir` (which is
/// derivable / OK to lose).
pub fn data_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".local/share/pgman");
        return p;
    }
    PathBuf::from(".")
}

pub fn config_file(name: &str) -> PathBuf {
    config_dir().join(name)
}
