//! pgman-specific path helpers. The neutral primitives
//! (`parse_bool`, `write_atomic`) live in the shared
//! `tb-tui-common` crate; callers go through
//! `tui_common::util::*` directly.

use std::io;
use std::path::{Path, PathBuf};

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

/// Create `dir` (and any missing parents) restricted to the owner
/// (`0700`) on unix; a plain recursive create on other platforms.
/// Every directory pgman creates under `config_dir()` / `data_dir()`
/// / `cache_dir()` should go through this rather than
/// `std::fs::create_dir_all`, so query history, saved queries, the
/// draft auto-save, and cached state aren't listable by other local
/// users even if a file inside ever ends up with looser permissions.
#[cfg(unix)]
pub fn create_dir_all_private(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
pub fn create_dir_all_private(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Write `contents` to `path` atomically (via
/// `tui_common::util::write_atomic`), then restrict the file to the
/// owner (`0600`) on unix; a no-op restriction on other platforms.
///
/// Every file pgman writes under `config_dir()` / `data_dir()` /
/// `cache_dir()` — query history, saved queries, the draft auto-save,
/// `\report` / `\fixture` output, the update-check cache — can hold
/// query text or table data and must not be left at the umask default
/// (usually world-readable). Route every such write through this
/// instead of calling `tui_common::util::write_atomic` directly.
pub fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            create_dir_all_private(parent)?;
        }
    }
    tui_common::util::write_atomic(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn write_private_sets_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pgman-util-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/state.toml");

        write_private(&path, "secret=1").expect("write_private");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file mode was {mode:o}, want 0600");

        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "dir mode was {dir_mode:o}, want 0700");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_private_rechmods_a_preexisting_looser_file() {
        // A file left over from before write_private existed (or
        // written by some other umask) must still end up 0600 after
        // a write_private call, not just newly-created files.
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("pgman-util-private-rechmod-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.toml");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&path, "new").expect("write_private");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file mode was {mode:o}, want 0600");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_private_round_trips_content() {
        let dir =
            std::env::temp_dir().join(format!("pgman-util-private-rw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.toml");
        write_private(&path, "hello").expect("write_private");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
