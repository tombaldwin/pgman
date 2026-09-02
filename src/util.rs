//! pgman-specific path helpers. The neutral primitive (`parse_bool`)
//! lives in the shared `tb-tui-common` crate; callers go through
//! `tui_common::util::*` directly. `write_private` used to route
//! through `tui_common::util::write_atomic` too, but that helper
//! writes its temp file at the platform default mode (usually
//! `0644`) before the caller gets a chance to chmod it — a window
//! where a file that will end up holding query text or table data
//! sits world-readable. `write_private` now has its own atomic
//! writer that is private from the first byte; see its doc comment.

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
/// Does **not** repair a pre-existing directory's mode — see
/// `ensure_private_dir` for that. Every directory pgman creates
/// under `config_dir()` / `data_dir()` / `cache_dir()` should go
/// through one of these two rather than `std::fs::create_dir_all`,
/// so query history, saved queries, the draft auto-save, and cached
/// state aren't listable by other local users even if a file inside
/// ever ends up with looser permissions.
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

/// Create `dir` if missing, then unconditionally `chmod` it to
/// owner-only (`0700`) on unix — repairing a directory that was
/// already there at a looser mode (a stale umask, a `tar`/backup
/// restore that didn't preserve permissions, a directory that
/// predates this hardening). `create_dir_all_private`'s `mode(0o700)`
/// only applies to directories it actually creates; a pre-existing
/// one is left untouched by a plain recursive create. Every place
/// pgman first touches `config_dir()` / `data_dir()` / `cache_dir()`
/// (or a subdirectory under them) should go through this instead.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    create_dir_all_private(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// `O_CREAT|O_EXCL` a new file at `tmp`, at mode `0600` on unix from
/// the syscall itself (a plain create on other platforms) — split
/// out from `write_atomic_private` so a test can assert the mode is
/// right the instant the file exists, with no window (not even a
/// theoretical one) where it's readable by anyone but the owner.
#[cfg(unix)]
fn create_private_temp_file(tmp: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
}

#[cfg(not(unix))]
fn create_private_temp_file(tmp: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
}

/// Write `contents` to a freshly-created sibling temp file (via
/// `create_private_temp_file`, so it's `0600` from the moment it
/// exists — see its doc comment), then rename it over `path`. Unlike
/// `tui_common::util::write_atomic` (which writes at the platform
/// default mode, usually `0644`, and relies on the caller to chmod
/// afterward) there is no window where the temp file, or the
/// renamed-over target, is readable by anyone but the owner.
/// `sync_all` before the rename so a crash right after doesn't leave
/// a rename pointing at not-yet-durable data.
///
/// Temp name: `<file>.tmp.<pid>.<nanos>.<counter>` — pid + wall-clock
/// nanos (same scheme `tui_common::util::write_atomic` used) plus a
/// process-wide counter, so back-to-back calls in the same
/// nanosecond (plausible on a fast clock, and cargo's test runner
/// hammers this in parallel threads) still land on distinct paths.
fn write_atomic_private(path: &Path, contents: &str) -> io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "write_private: path has no file name",
        )
    })?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut tmp_name = name.to_owned();
    tmp_name.push(format!(".tmp.{}.{}.{}", std::process::id(), nanos, counter));
    let tmp = dir.join(tmp_name);

    let result = (|| -> io::Result<()> {
        use std::io::Write;
        let mut file = create_private_temp_file(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Write `contents` to `path` via `write_atomic_private` (see its
/// doc comment for why pgman has its own atomic writer instead of
/// `tui_common::util::write_atomic`), then unconditionally restrict
/// the file to the owner (`0600`) on unix — belt-and-braces in case
/// `path` pre-existed at a looser mode on a platform/filesystem where
/// `rename` doesn't fully replace the destination's metadata. A
/// no-op restriction on non-unix.
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
            // Create-only, not `ensure_private_dir`: `path`'s parent
            // isn't necessarily a directory pgman owns — a `\report
            // ~/notes/` destination the operator picked, or (in
            // tests) a bare file dropped straight in the OS temp
            // dir. Unconditionally `chmod`ing an arbitrary
            // pre-existing directory pgman didn't create would be
            // reaching well past "make pgman's own files private".
            // The known-owned roots (`config_dir()` / `data_dir()` /
            // `cache_dir()`) get their repair explicitly at startup
            // instead — see `main.rs::init_logging` and its callers.
            create_dir_all_private(parent)?;
        }
    }
    write_atomic_private(path, contents)?;
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

    #[cfg(unix)]
    #[test]
    fn create_private_temp_file_is_owner_only_from_the_syscall() {
        // The strongest form of "no 0644 file ever exists": the mode
        // is right the instant the file is created (via the mode
        // bits on the O_CREAT|O_EXCL syscall itself), checked before
        // write_private's belt-and-braces final chmod ever runs —
        // that chmod would mask a regression here if we only checked
        // the end state.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "pgman-util-private-create-mode-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join("probe.tmp");

        let file = create_private_temp_file(&tmp).expect("create_private_temp_file");
        let mode = file.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "temp file mode was {mode:o} at creation, want 0600"
        );
        drop(file);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_private_leaves_no_0644_leftover_in_the_directory() {
        // The whole point of pgman's own atomic writer over
        // `tui_common::util::write_atomic`: at no point does a
        // world/group-readable file exist in the target directory,
        // not even transiently as the `.tmp.*` sibling. Sweep the
        // directory after several writes (including an overwrite of
        // an existing file, which is the rename-over-existing path)
        // and check every entry that exists.
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("pgman-util-private-no0644-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.toml");

        write_private(&path, "first").expect("first write_private");
        write_private(&path, "second — a bit longer this time").expect("second write_private");

        let mut swept = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o600,
                "{:?} was left at {mode:o}, want 0600 (no 0644 leftover)",
                entry.file_name()
            );
            swept += 1;
        }
        assert_eq!(
            swept, 1,
            "expected only the final renamed-over file, no leftover temp files"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "second — a bit longer this time"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_private_rename_over_existing_keeps_content_atomic() {
        // Companion to tests/crash_recovery.rs's atomic-rename
        // contract, run directly against the new writer: a second
        // write must fully replace the first, never interleave or
        // truncate.
        let dir =
            std::env::temp_dir().join(format!("pgman-util-private-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.toml");
        write_private(&path, "first version").expect("first write_private");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first version");
        write_private(&path, "second version, completely different length")
            .expect("second write_private");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "second version, completely different length"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_repairs_a_preexisting_looser_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "pgman-util-ensure-private-dir-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        ensure_private_dir(&dir).expect("ensure_private_dir");

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "dir mode was {mode:o}, want 0700");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
