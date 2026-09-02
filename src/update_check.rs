//! Install-channel detection + a rate-limited crates.io version check.
//!
//! Two independent jobs live here:
//!
//! 1. **Channel detection** (`InstallChannel`) — pure, path-based —
//!    tells `--upgrade` (`src/upgrade.rs`) and the About overlay what
//!    command upgrades the running binary in place.
//! 2. **The version check** (`check_async`) — a single crates.io GET,
//!    cached to `util::cache_dir()/update_check.json` and re-run at
//!    most every six hours, feeding `AppMsg::UpdateCheck` so the
//!    header badge / About overlay can say "a newer version exists"
//!    without ever blocking startup.
//!
//! Every failure mode (no network, a stale TLS root store, a
//! malformed response, a crates.io outage) degrades silently to
//! `None` — this is a courtesy notice, never a hard dependency.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A newer release than the one currently running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    pub version: String,
}

/// How the running binary got onto this machine. Determines both the
/// label shown in the About overlay and the command `--upgrade`
/// (or the overlay's copy-paste line) offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallChannel {
    /// A local git checkout — `cargo install --path …` from a clone.
    /// This is also `--upgrade`'s original (and still primary) path.
    Checkout,
    /// Installed via `brew install pgman` / a Homebrew tap.
    Homebrew,
    /// Installed via `cargo install pgman` (crates.io).
    Cargo,
    /// Anything else — a downloaded release tarball, a custom
    /// packaging step, or a layout we don't recognise. No in-place
    /// upgrade; point at the releases page.
    Standalone,
}

impl InstallChannel {
    /// Classify an install. Pure and path-based so it's testable
    /// without touching the filesystem or the environment.
    ///
    /// `manifest_dir_is_git_tree` is the answer to "does
    /// `CARGO_MANIFEST_DIR/.git` exist?" — the existing `--upgrade`
    /// precondition (see `src/upgrade.rs::SOURCE_PATH`). It takes
    /// priority: a binary built from a live checkout is a `Checkout`
    /// install regardless of where the compiled binary happens to
    /// live on disk (e.g. `cargo install --path .` copies it into
    /// `~/.cargo/bin`, which would otherwise misclassify as `Cargo`).
    pub fn detect(exe_path: &Path, manifest_dir_is_git_tree: bool) -> InstallChannel {
        if manifest_dir_is_git_tree {
            return InstallChannel::Checkout;
        }
        let s = exe_path.to_string_lossy();
        if s.contains("/Cellar/") || s.contains("/homebrew/") || s.contains("/.linuxbrew/") {
            return InstallChannel::Homebrew;
        }
        if s.contains("/.cargo/bin/") {
            return InstallChannel::Cargo;
        }
        InstallChannel::Standalone
    }

    /// How to name this channel to an operator.
    pub fn label(self) -> &'static str {
        match self {
            InstallChannel::Checkout => "a local git checkout",
            InstallChannel::Homebrew => "Homebrew",
            InstallChannel::Cargo => "cargo install",
            InstallChannel::Standalone => "a standalone binary",
        }
    }

    /// The GitHub releases page — the fallback for channels with no
    /// in-place upgrade command.
    pub const RELEASES_URL: &'static str = "https://github.com/tombaldwin/pgman/releases/latest";

    /// The command (or, for `Standalone`, the URL) that upgrades this
    /// install in place. `Checkout` describes the two-step
    /// git-pull-then-reinstall `--upgrade` actually runs; the other
    /// channels are single commands, directly runnable.
    pub fn upgrade_command(self) -> String {
        match self {
            InstallChannel::Checkout => format!(
                "git -C {dir} pull && cargo install --path {dir} --locked --force",
                dir = crate::upgrade::SOURCE_PATH
            ),
            InstallChannel::Homebrew => "brew upgrade pgman".to_string(),
            InstallChannel::Cargo => "cargo install pgman --locked --force".to_string(),
            InstallChannel::Standalone => Self::RELEASES_URL.to_string(),
        }
    }
}

/// Resolve `exe`'s symlinks (falling back to `exe` itself if that
/// fails — e.g. it doesn't exist), then classify it. Split out of
/// `detect_install_channel` so a test can drive the symlink
/// resolution against a real scratch symlink without depending on
/// `std::env::current_exe()`.
///
/// Homebrew's Intel-macOS layout symlinks `/usr/local/bin/pgman` into
/// the Cellar (`/usr/local/Cellar/pgman/<version>/bin/pgman`), and
/// macOS doesn't resolve that for us — `current_exe()` returns the
/// symlink path, which `InstallChannel::detect`'s string match
/// against `/Cellar/` would miss entirely, misclassifying a Homebrew
/// install as `Standalone` and sending `--upgrade` to the releases
/// page instead of running `brew upgrade`.
fn detect_resolved(exe: &Path, manifest_is_git_tree: bool) -> InstallChannel {
    let resolved = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    InstallChannel::detect(&resolved, manifest_is_git_tree)
}

/// Impure wrapper around [`InstallChannel::detect`] — resolves the
/// running binary's path (symlinks and all) and whether the
/// compiled-in manifest dir is still a git working tree.
pub fn detect_install_channel() -> InstallChannel {
    let exe = std::env::current_exe().unwrap_or_default();
    let manifest_is_git_tree = Path::new(crate::upgrade::SOURCE_PATH).join(".git").exists();
    detect_resolved(&exe, manifest_is_git_tree)
}

/// Pull `crate.max_stable_version` out of a crates.io
/// `/api/v1/crates/<name>` JSON response.
pub fn parse_crates_io_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("crate")?
        .get("max_stable_version")?
        .as_str()
        .map(str::to_string)
}

/// Split a version into its numeric core (padded/truncated to three
/// components) and an optional pre-release tail (the part after the
/// first `-`). Build metadata (a `+` and everything after — semver
/// permits it after the core or after a pre-release tail, e.g.
/// `0.2.1+meta` or `0.2.0-rc1+build.5`) is stripped first and
/// otherwise ignored entirely: semver precedence never considers it,
/// and treating it as significant is exactly the bug this comment is
/// here to prevent regressing — `"0.2.1+meta"` was previously parsed
/// with the core `"1+meta"` for its patch component, which fails to
/// parse as a number and silently falls back to `0`, so `0.2.1+meta`
/// compared equal to `0.2.0` instead of newer. Non-numeric or missing
/// core components (still) parse as `0` — callers only ever feed this
/// well-formed crates.io version strings, and a best-effort parse
/// degrades gracefully rather than panicking.
fn parse_version(v: &str) -> ([u64; 3], Option<&str>) {
    let v = match v.split_once('+') {
        Some((base, _build_metadata)) => base,
        None => v,
    };
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut parts = [0u64; 3];
    for (i, p) in core.split('.').take(3).enumerate() {
        parts[i] = p.parse().unwrap_or(0);
    }
    (parts, pre)
}

/// Compare two dot-separated pre-release identifiers the way semver
/// precedence does: purely-numeric identifiers compare numerically;
/// anything else compares as a string; a pre-release with fewer
/// identifiers than an otherwise-equal one sorts first.
fn compare_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(xn), Ok(yn)) => xn.cmp(&yn),
                    _ => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// Is `candidate` a newer release than `current`? Numeric core first
/// (`0.10.0` beats `0.9.9`); on a tied core, a release beats its own
/// pre-releases (`0.2.0` beats `0.2.0-rc1`) and pre-releases compare
/// against each other by semver precedence (`rc2` beats `rc1`).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    use std::cmp::Ordering;
    let (c_core, c_pre) = parse_version(candidate);
    let (r_core, r_pre) = parse_version(current);
    match c_core.cmp(&r_core) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => match (c_pre, r_pre) {
            (None, None) => false,
            (None, Some(_)) => true, // release beats a pre-release of itself
            (Some(_), None) => false, // pre-release never beats its own release
            (Some(c), Some(r)) => compare_prerelease(c, r) == Ordering::Greater,
        },
    }
}

/// True when a check is due: never checked, the last check was more
/// than six hours ago, or the last check's timestamp is in the
/// future. Pure so the six-hour rule is testable without a clock.
///
/// The future-timestamp case matters because `now.saturating_sub(t)`
/// alone would read as "0 seconds ago" (never over the threshold) for
/// any `t > now` — a cache written by a machine with a wrong clock,
/// or restored from a backup/snapshot with a later timestamp, would
/// then permanently disable the check: every subsequent run sees a
/// "fresh" cache it can never age out of.
pub fn should_check(last_checked_at: Option<u64>, now: u64) -> bool {
    const SIX_HOURS_SECS: u64 = 6 * 60 * 60;
    match last_checked_at {
        None => true,
        Some(t) => now < t || now.saturating_sub(t) > SIX_HOURS_SECS,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CacheFile {
    checked_at: u64,
    latest: Option<String>,
}

fn cache_path() -> std::path::PathBuf {
    crate::util::cache_dir().join("update_check.json")
}

fn read_cache_at(path: &Path) -> Option<CacheFile> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache_at(path: &Path, entry: &CacheFile) {
    if let Ok(text) = serde_json::to_string(entry) {
        if let Err(e) = crate::util::write_private(path, &text) {
            tracing::debug!(
                "update check: could not write cache {}: {e}",
                path.display()
            );
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// crates.io requires a descriptive User-Agent on every request.
fn user_agent() -> String {
    format!(
        "pgman/{} (https://github.com/tombaldwin/pgman)",
        env!("CARGO_PKG_VERSION")
    )
}

/// A crates.io `/api/v1/crates/<name>` response is a few hundred
/// bytes of JSON. Cap what we're willing to buffer well above that
/// (but nowhere near unbounded) so a compromised/spoofed endpoint
/// can't hand this a multi-gigabyte body and blow up memory.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// One crates.io GET, 10-second cap. Any failure (network, non-2xx,
/// oversize or unparseable body) logs at `debug` and returns `None` —
/// this must never be noisy or block the caller.
async fn fetch_latest_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        // crates.io's API never redirects; refusing to follow one
        // means a MITM or DNS-hijacked response can't quietly send
        // this client's User-Agent (or, if a redirect could ever
        // carry one, a cookie/auth header) somewhere else entirely.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| tracing::debug!("update check: client build failed: {e}"))
        .ok()?;
    let resp = client
        .get("https://crates.io/api/v1/crates/pgman")
        .header(reqwest::header::USER_AGENT, user_agent())
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("update check: request failed: {e}");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::debug!("update check: crates.io returned {}", resp.status());
        return None;
    }
    // Reject anything claiming to be oversize before buffering it at
    // all. A response with no `Content-Length` (chunked) still gets
    // the post-read length check below.
    if let Some(len) = resp.content_length() {
        if len > MAX_RESPONSE_BYTES {
            tracing::debug!("update check: response body too large ({len} bytes), ignoring");
            return None;
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!("update check: could not read response body: {e}");
            return None;
        }
    };
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        tracing::debug!(
            "update check: response body too large ({} bytes), ignoring",
            bytes.len()
        );
        return None;
    }
    let body = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("update check: response body was not UTF-8: {e}");
            return None;
        }
    };
    let latest = parse_crates_io_body(body);
    if latest.is_none() {
        tracing::debug!("update check: could not parse crates.io response");
    }
    latest
}

/// Check for a newer release, honouring the six-hour cache at
/// `cache_path`. Returns `Some` only when a strictly-newer version
/// than `CARGO_PKG_VERSION` is known (from cache or a live check) —
/// same contract as `check_async`, just with an injectable cache
/// location so tests don't fight over `util::cache_dir()`.
async fn check_with(cache_path: &Path) -> Option<LatestRelease> {
    let now = now_unix();
    let cached = read_cache_at(cache_path);
    let fresh = cached
        .as_ref()
        .map(|c| !should_check(Some(c.checked_at), now))
        .unwrap_or(false);
    let latest_version = if fresh {
        cached.and_then(|c| c.latest)
    } else {
        let fetched = fetch_latest_version().await;
        write_cache_at(
            cache_path,
            &CacheFile {
                checked_at: now,
                latest: fetched.clone(),
            },
        );
        fetched
    };
    let current = env!("CARGO_PKG_VERSION");
    match latest_version {
        Some(v) if is_newer(&v, current) => Some(LatestRelease { version: v }),
        _ => None,
    }
}

/// Fire-and-forget update check against the real cache file
/// (`util::cache_dir()/update_check.json`). Spawn this from its own
/// `tokio::spawn` — it never panics and every failure path resolves
/// to `None`.
pub async fn check_async() -> Option<LatestRelease> {
    check_with(&cache_path()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- InstallChannel::detect ----

    #[test]
    fn detect_prefers_checkout_regardless_of_exe_path() {
        // Even a binary sitting in ~/.cargo/bin counts as Checkout
        // when it was built from a live git tree — `cargo install
        // --path .` copies the binary there but the source is still
        // the checkout.
        let p = Path::new("/Users/tom/.cargo/bin/pgman");
        assert_eq!(InstallChannel::detect(p, true), InstallChannel::Checkout);
    }

    #[test]
    fn detect_maps_cellar_path_to_homebrew() {
        let p = Path::new("/opt/homebrew/Cellar/pgman/0.1.0/bin/pgman");
        assert_eq!(InstallChannel::detect(p, false), InstallChannel::Homebrew);
        let p = Path::new("/usr/local/Cellar/pgman/0.1.0/bin/pgman");
        assert_eq!(InstallChannel::detect(p, false), InstallChannel::Homebrew);
    }

    #[test]
    fn detect_maps_homebrew_and_linuxbrew_prefixes() {
        let p = Path::new("/home/linuxbrew/.linuxbrew/bin/pgman");
        assert_eq!(InstallChannel::detect(p, false), InstallChannel::Homebrew);
        let p = Path::new("/opt/homebrew/bin/pgman");
        assert_eq!(InstallChannel::detect(p, false), InstallChannel::Homebrew);
    }

    #[test]
    fn detect_maps_cargo_bin() {
        let p = Path::new("/Users/tom/.cargo/bin/pgman");
        assert_eq!(InstallChannel::detect(p, false), InstallChannel::Cargo);
    }

    #[test]
    fn detect_falls_back_to_standalone() {
        let p = Path::new("/usr/local/bin/pgman");
        assert_eq!(InstallChannel::detect(p, false), InstallChannel::Standalone);
        let p = Path::new("/home/tom/Downloads/pgman");
        assert_eq!(InstallChannel::detect(p, false), InstallChannel::Standalone);
    }

    #[cfg(unix)]
    #[test]
    fn detect_resolved_follows_a_symlink_into_the_cellar() {
        // Homebrew's Intel-macOS layout: `/usr/local/bin/pgman` is a
        // symlink into the Cellar, and macOS doesn't resolve that for
        // `current_exe()`. Build a real scratch symlink so this
        // exercises actual `canonicalize`, not just the string match.
        let dir =
            std::env::temp_dir().join(format!("pgman-update-check-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cellar_bin_dir = dir.join("Cellar/pgman/0.1.0/bin");
        std::fs::create_dir_all(&cellar_bin_dir).unwrap();
        let real_binary = cellar_bin_dir.join("pgman");
        std::fs::write(&real_binary, b"not a real binary, just needs to exist").unwrap();
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let symlink = bin_dir.join("pgman");
        std::os::unix::fs::symlink(&real_binary, &symlink).unwrap();

        assert_eq!(
            detect_resolved(&symlink, false),
            InstallChannel::Homebrew,
            "a symlink resolving into /Cellar/ must classify as Homebrew"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn detect_resolved_falls_back_to_the_given_path_when_it_does_not_exist() {
        // canonicalize fails for a path that doesn't exist (the
        // common case in CI/sandboxes where current_exe() can behave
        // oddly, or plain test fixtures like the Standalone tests
        // above) — detect_resolved must fall back to classifying the
        // path as given, not silently swallow the whole classification.
        let p = Path::new("/usr/local/bin/pgman-definitely-does-not-exist-xyz");
        assert_eq!(detect_resolved(p, false), InstallChannel::Standalone);
    }

    #[test]
    fn upgrade_command_is_concrete_per_channel() {
        assert!(InstallChannel::Checkout
            .upgrade_command()
            .contains("git -C"));
        assert!(InstallChannel::Checkout
            .upgrade_command()
            .contains("cargo install --path"));
        assert_eq!(
            InstallChannel::Homebrew.upgrade_command(),
            "brew upgrade pgman"
        );
        assert_eq!(
            InstallChannel::Cargo.upgrade_command(),
            "cargo install pgman --locked --force"
        );
        assert_eq!(
            InstallChannel::Standalone.upgrade_command(),
            InstallChannel::RELEASES_URL
        );
    }

    // ---- crates.io body parsing ----

    #[test]
    fn parse_crates_io_body_finds_max_stable_version() {
        let body = r#"{"crate":{"id":"pgman","max_stable_version":"0.4.2","other":"x"}}"#;
        assert_eq!(parse_crates_io_body(body).as_deref(), Some("0.4.2"));
    }

    #[test]
    fn parse_crates_io_body_missing_field_returns_none() {
        assert!(parse_crates_io_body(r#"{"crate":{"id":"pgman"}}"#).is_none());
    }

    #[test]
    fn parse_crates_io_body_malformed_json_returns_none() {
        assert!(parse_crates_io_body("not json").is_none());
    }

    // ---- version comparison ----

    #[test]
    fn is_newer_compares_dotted_semver() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.1"));
    }

    #[test]
    fn is_newer_handles_double_digit_minor() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.9.9", "0.10.0"));
    }

    #[test]
    fn is_newer_release_beats_its_own_prerelease() {
        assert!(is_newer("0.2.0", "0.2.0-rc1"));
        assert!(!is_newer("0.2.0-rc1", "0.2.0"));
    }

    #[test]
    fn is_newer_orders_prereleases_by_precedence() {
        assert!(is_newer("0.30.0-rc2", "0.30.0-rc1"));
        assert!(!is_newer("0.30.0-rc1", "0.30.0-rc2"));
        assert!(!is_newer("0.30.0-rc1", "0.30.0-rc1"));
    }

    #[test]
    fn is_newer_prerelease_with_higher_core_beats_lower_release() {
        assert!(is_newer("0.2.0-rc1", "0.1.0"));
    }

    #[test]
    fn is_newer_ignores_build_metadata() {
        // Regression: parse_version used to split only on '-', so
        // "0.2.1+meta" parsed its patch component as "1+meta", which
        // fails to parse as u64 and silently defaulted to 0 — making
        // 0.2.1+meta compare *equal* to 0.2.0 instead of newer.
        assert!(is_newer("0.2.1+meta", "0.2.0"));
        // Build metadata alone must not make an otherwise-equal
        // version look newer.
        assert!(!is_newer("0.2.0+abc", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.2.0+abc"));
        // Build metadata after a pre-release tail (rc1+build.5) is
        // also stripped, not folded into precedence comparison.
        assert!(is_newer("0.2.0-rc2+build.9", "0.2.0-rc1+build.1"));
    }

    // ---- should_check ----

    #[test]
    fn should_check_true_when_never_checked() {
        assert!(should_check(None, 1_000_000));
    }

    #[test]
    fn should_check_false_within_six_hours() {
        let now = 1_000_000u64;
        assert!(!should_check(Some(now - 60), now));
        assert!(!should_check(Some(now - 6 * 3600), now));
    }

    #[test]
    fn should_check_true_after_six_hours() {
        let now = 1_000_000u64;
        assert!(should_check(Some(now - 6 * 3600 - 1), now));
    }

    #[test]
    fn should_check_true_when_last_checked_is_in_the_future() {
        // Regression: `now.saturating_sub(t)` alone reads a future
        // `t` as "0 seconds ago" — never over the six-hour threshold
        // — which would permanently disable the check for a cache
        // written by a machine with a wrong clock, or restored from a
        // backup/snapshot with a later timestamp than "now".
        let now = 1_000_000u64;
        assert!(should_check(Some(now + 1_000_000), now));
    }

    // ---- cache round-trip ----

    #[test]
    fn cache_round_trips_through_a_temp_file() {
        let dir = std::env::temp_dir().join(format!(
            "pgman-update-check-cache-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("update_check.json");

        assert!(read_cache_at(&path).is_none(), "no file yet");

        let entry = CacheFile {
            checked_at: 1_234_567,
            latest: Some("9.9.9".to_string()),
        };
        write_cache_at(&path, &entry);
        assert_eq!(read_cache_at(&path), Some(entry.clone()));

        // A second write (as a real run would do on its next check)
        // replaces the file cleanly rather than appending/corrupting.
        let entry2 = CacheFile {
            checked_at: 7_654_321,
            latest: None,
        };
        write_cache_at(&path, &entry2);
        assert_eq!(read_cache_at(&path), Some(entry2));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- check_with: fresh cache short-circuits the network ----

    #[tokio::test]
    async fn check_with_returns_cached_value_when_fresh_without_network() {
        let dir = std::env::temp_dir().join(format!(
            "pgman-update-check-fresh-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("update_check.json");
        write_cache_at(
            &path,
            &CacheFile {
                checked_at: now_unix(),
                latest: Some("999.0.0".to_string()),
            },
        );

        // A fresh cache must never touch the network — bound the
        // call tightly so a broken `should_check` (always "due")
        // shows up as a timeout here rather than a slow pass.
        let result = tokio::time::timeout(Duration::from_millis(500), check_with(&path))
            .await
            .expect("fresh cache must short-circuit the network call");
        assert_eq!(result.map(|r| r.version), Some("999.0.0".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn check_with_fresh_cache_with_no_newer_version_returns_none() {
        let dir = std::env::temp_dir().join(format!(
            "pgman-update-check-fresh-none-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("update_check.json");
        write_cache_at(
            &path,
            &CacheFile {
                checked_at: now_unix(),
                latest: Some(env!("CARGO_PKG_VERSION").to_string()),
            },
        );

        let result = tokio::time::timeout(Duration::from_millis(500), check_with(&path))
            .await
            .expect("fresh cache must short-circuit the network call");
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
