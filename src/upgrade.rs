//! `pgman --upgrade` — pull the source repo and reinstall via cargo.
//!
//! `SOURCE_PATH` is baked in at compile time via `CARGO_MANIFEST_DIR`, so the
//! binary always knows the working tree it was built from. If you installed
//! via `cargo install --git …` instead of `--path`, the path won't exist on
//! disk — the upgrade prints what to do and exits non-zero.
//!
//! Subprocesses inherit stdio so the user sees `git pull` / `cargo install`
//! output live.

use std::path::Path;
use std::process::Command;

/// The path the running binary was built from. Set by Cargo at compile time.
pub const SOURCE_PATH: &str = env!("CARGO_MANIFEST_DIR");

/// Run the upgrade flow. Returns `Ok(())` on success, an `anyhow::Error`
/// (suitable for `Result` propagation in `main`) otherwise.
pub fn run() -> anyhow::Result<()> {
    let repo = SOURCE_PATH;
    let path = Path::new(repo);

    if !path.join(".git").exists() {
        anyhow::bail!(
            "this pgman was not installed from a local git checkout \
             (CARGO_MANIFEST_DIR = `{repo}` is not a working tree). \
             Reinstall manually, e.g. `cargo install --git <url> --force`."
        );
    }

    eprintln!("→ git -C {repo} pull --ff-only");
    let status = Command::new("git")
        .args(["-C", repo, "pull", "--ff-only"])
        .status()
        .map_err(|e| anyhow::anyhow!("could not run git: {e}"))?;
    if !status.success() {
        anyhow::bail!("git pull failed ({status})");
    }

    eprintln!("\n→ cargo install --path {repo} --locked --force");
    let status = Command::new("cargo")
        .args(["install", "--path", repo, "--locked", "--force"])
        .status()
        .map_err(|e| anyhow::anyhow!("could not run cargo: {e}"))?;
    if !status.success() {
        anyhow::bail!("cargo install failed ({status})");
    }

    // Skip the relaunch when invoked non-interactively (CI, scripts piping
    // output) so we don't start a TUI that has no terminal.
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
/// is the same path we were launched from — `cargo install` has overwritten
/// the file there.
#[cfg(unix)]
fn relaunch() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("locate current exe: {e}"))?;
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
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("locate current exe: {e}"))?;
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
