//! `pgman --upgrade` — upgrade the running binary in place, however it
//! got installed.
//!
//! `SOURCE_PATH` is baked in at compile time via `CARGO_MANIFEST_DIR`, so a
//! binary built from a local git checkout always knows the working tree it
//! came from — that's the `Checkout` channel, and its upgrade (`git pull` +
//! `cargo install --path`) is unchanged from before install channels
//! existed. `Cargo` and `Homebrew` installs get their own one-command
//! upgrade; a `Standalone` binary (a downloaded release tarball, or any
//! layout we don't recognise) has no in-place upgrade at all — this prints
//! the releases page and exits non-zero.
//!
//! Subprocesses inherit stdio so the user sees the upgrade command's output
//! live.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::update_check::InstallChannel;

/// The path the running binary was built from. Set by Cargo at compile time.
pub const SOURCE_PATH: &str = env!("CARGO_MANIFEST_DIR");

/// Run the upgrade flow for whichever channel this binary was installed
/// through. Returns `Ok(())` on success, an `anyhow::Error` (suitable for
/// `Result` propagation in `main`) otherwise.
pub fn run() -> anyhow::Result<()> {
    match crate::update_check::detect_install_channel() {
        InstallChannel::Checkout => run_checkout(),
        InstallChannel::Cargo => {
            run_step("cargo", &["install", "pgman", "--locked", "--force"])?;
            finish()
        }
        InstallChannel::Homebrew => {
            run_step("brew", &["upgrade", "pgman"])?;
            finish()
        }
        InstallChannel::Standalone => {
            eprintln!(
                "pgman {} is a standalone install — there's no in-place upgrade for it.\n\
                 Download the latest release from:\n  {}",
                env!("CARGO_PKG_VERSION"),
                InstallChannel::RELEASES_URL,
            );
            std::process::exit(1);
        }
    }
}

/// The original `--upgrade` path: pull the source repo and reinstall via
/// `cargo install --path`. Requires `SOURCE_PATH` (the compiled-in
/// `CARGO_MANIFEST_DIR`) to still be a git working tree on disk — the same
/// precondition `InstallChannel::detect` uses to classify a binary as
/// `Checkout` in the first place, so reaching this function at all means
/// the check already passed.
fn run_checkout() -> anyhow::Result<()> {
    let repo = SOURCE_PATH;
    let path = Path::new(repo);

    if !path.join(".git").exists() {
        anyhow::bail!(
            "this pgman was not installed from a local git checkout \
             (CARGO_MANIFEST_DIR = `{repo}` is not a working tree). \
             Reinstall manually, e.g. `cargo install --git <url> --force`."
        );
    }

    run_step("git", &["-C", repo, "pull", "--ff-only"])?;
    run_step("cargo", &["install", "--path", repo, "--locked", "--force"])?;
    finish()
}

/// Pick a working directory for an upgrade subprocess: `home` if it's
/// a directory that actually exists, otherwise `fallback`. Pure — the
/// impure caller (`run_step`) resolves `$HOME` and the fallback
/// directory and passes them in, so this stays testable without
/// touching the filesystem or environment.
fn choose_working_dir(home: Option<PathBuf>, home_is_dir: bool, fallback: PathBuf) -> PathBuf {
    match home {
        Some(p) if home_is_dir => p,
        _ => fallback,
    }
}

/// `$HOME` if set and an actual directory, else pgman's own cache
/// directory.
///
/// `cargo install` and `brew upgrade` search upward from the
/// subprocess's *current working directory* for `.cargo/config.toml`
/// — a `build.rustc-wrapper` or `target.*.runner` entry in one runs
/// arbitrary commands. Running the upgrade from wherever the operator
/// happened to invoke `pgman --upgrade` (which might be sitting inside
/// some other, untrusted checkout) would let that checkout's config
/// hijack the upgrade; running from `$HOME` instead means only a
/// config *there* — Homebrew's or cargo's own concern — can affect it.
///
/// The fallback for a missing `$HOME` used to be `std::env::temp_dir()`,
/// which reopened the same hole from a different directory: `/tmp` is
/// world-writable on Linux, and cargo searches upward, so any local
/// user could plant `/tmp/.cargo/config.toml` with a
/// `build.rustc-wrapper` and own the upgrade.
fn home_or_temp_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let home_is_dir = home.as_deref().is_some_and(Path::is_dir);
    choose_working_dir(home, home_is_dir, private_fallback_dir())
}

/// pgman's cache directory, created owner-only — a working directory
/// nobody but the user can drop a `.cargo/config.toml` into.
///
/// `util::cache_dir()` degrades to `.` when neither `XDG_CACHE_HOME`
/// nor `HOME` is set, and `.` is the invoking working directory, which
/// is the one place this must never be. In that single case — no
/// `HOME`, no XDG override, so pgman has no private directory anywhere
/// — there is nothing better than the old temp-dir behaviour.
fn private_fallback_dir() -> PathBuf {
    fallback_from(crate::util::cache_dir(), std::env::temp_dir())
}

/// `cache` when it is a real directory pgman can make owner-only,
/// else `temp`. Split out so a test can drive both arms without
/// mutating the process environment.
fn fallback_from(cache: PathBuf, temp: PathBuf) -> PathBuf {
    if cache != Path::new(".") && crate::util::ensure_private_dir(&cache).is_ok() {
        return cache;
    }
    temp
}

/// Run one upgrade subprocess with inherited stdio, echoing the command
/// first so the operator sees exactly what ran. Runs from `$HOME` (or
/// pgman's own cache directory) rather than pgman's working directory —
/// see `home_or_temp_dir` for why. `Err` on a failed spawn or a non-zero exit.
fn run_step(program: &str, args: &[&str]) -> anyhow::Result<()> {
    eprintln!("→ {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .current_dir(home_or_temp_dir())
        .status()
        .map_err(|e| anyhow::anyhow!("could not run {program}: {e}"))?;
    if !status.success() {
        anyhow::bail!("{program} failed ({status})");
    }
    Ok(())
}

/// Shared tail of every successful upgrade: skip relaunching when
/// non-interactive (CI, scripts piping output), otherwise exec the
/// just-installed binary with the original args.
fn finish() -> anyhow::Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("\n✓ pgman upgraded · stdin/stdout not a TTY — not relaunching");
        return Ok(());
    }

    eprintln!("\n✓ pgman upgraded · relaunching…\n");
    relaunch()
}

/// Replace this process with the (just-installed) `pgman` binary, passing
/// through the user's original CLI args minus `--upgrade`. The exec'd binary
/// is the same path we were launched from — the upgrade step has overwritten
/// the file there (`cargo install`) or `brew upgrade` has relinked it.
#[cfg(unix)]
fn relaunch() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("locate current exe: {e}"))?;
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--upgrade")
        .collect();
    // `exec` does not return on success — the new binary takes over.
    let err = Command::new(&exe).args(&args).exec();
    Err(anyhow::anyhow!("exec failed: {err}"))
}

#[cfg(not(unix))]
fn relaunch() -> anyhow::Result<()> {
    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("locate current exe: {e}"))?;
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--upgrade")
        .collect();
    let status = Command::new(&exe)
        .args(&args)
        .status()
        .map_err(|e| anyhow::anyhow!("spawn failed: {e}"))?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_fallback_working_dir_is_private_and_not_a_shared_temp_dir() {
        // The reproduction: with `$HOME` unset the upgrade ran from
        // `std::env::temp_dir()`. cargo searches *upward* from the
        // working directory for `.cargo/config.toml`, and `/tmp` is
        // world-writable on Linux — so any local user could plant one
        // with a `build.rustc-wrapper` and own the upgrade. That is
        // the same hijack running from `$HOME` exists to prevent.
        use std::os::unix::fs::PermissionsExt;
        let scratch =
            std::env::temp_dir().join(format!("pgman-upgrade-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let cache = scratch.join("pgman");
        let temp = PathBuf::from("/tmp");

        let dir = fallback_from(cache.clone(), temp.clone());
        assert_eq!(dir, cache, "the fallback must be pgman's cache directory");
        assert_ne!(dir, temp, "and never the shared temp directory");
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "it must be created owner-only so nobody else can plant a \
             .cargo/config.toml in it; got {mode:o}"
        );

        // The one case with no private directory to be had: neither
        // `XDG_CACHE_HOME` nor `HOME` set, so `cache_dir()` degraded to
        // `.` — the invoking working directory, which is the one place
        // this must never run. Temp is the least-bad answer there.
        assert_eq!(
            fallback_from(PathBuf::from("."), temp.clone()),
            temp,
            "`.` is the caller's cwd and must never be chosen"
        );

        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn choose_working_dir_prefers_home_when_it_is_a_real_directory() {
        let home = PathBuf::from("/Users/tester");
        let temp = PathBuf::from("/tmp");
        assert_eq!(choose_working_dir(Some(home.clone()), true, temp), home);
    }

    #[test]
    fn choose_working_dir_falls_back_to_temp_when_home_unset() {
        let temp = PathBuf::from("/tmp");
        assert_eq!(choose_working_dir(None, false, temp.clone()), temp);
    }

    #[test]
    fn choose_working_dir_falls_back_to_temp_when_home_is_not_a_directory() {
        // HOME set but pointing at something that doesn't exist (or
        // isn't a directory) — e.g. a stale/misconfigured env.
        let home = PathBuf::from("/Users/tester");
        let temp = PathBuf::from("/tmp");
        assert_eq!(choose_working_dir(Some(home), false, temp.clone()), temp);
    }
}
