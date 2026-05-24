//! Filesystem helpers. Lifted from ebman — keep the two in sync if either
//! changes. All config/cache paths go through here; no hardcoded paths.

use std::io;
use std::path::{Path, PathBuf};

pub fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

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

/// Atomically write `contents` to `path`. Writes to a sibling `.tmp` file then
/// renames into place — on Unix, `rename` within a single filesystem is atomic,
/// so a crash mid-write leaves either the old file intact or the new file
/// complete, never a truncated/partial file.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = match path.file_name() {
        Some(name) => {
            let mut tmp_name = name.to_owned();
            tmp_name.push(".tmp");
            path.with_file_name(tmp_name)
        }
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write_atomic: path has no file name",
            ));
        }
    };
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_creates_parent_and_replaces_existing() {
        let dir = std::env::temp_dir().join(format!("pgman-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/deep/state.toml");
        write_atomic(&path, "first").expect("first write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        write_atomic(&path, "second").expect("second write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let tmp = path.with_file_name("state.toml.tmp");
        assert!(!tmp.exists(), ".tmp file should be renamed away");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_bool_accepts_canonical_forms() {
        for s in ["true", "1", "yes", "on", "ON", "Yes", "TRUE"] {
            assert_eq!(parse_bool(s), Some(true), "expected true for {s:?}");
        }
        for s in ["false", "0", "no", "off", "OFF", "No"] {
            assert_eq!(parse_bool(s), Some(false), "expected false for {s:?}");
        }
        for s in ["", "maybe", "2", "trueish"] {
            assert_eq!(parse_bool(s), None, "expected None for {s:?}");
        }
    }
}
