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

    eprintln!("\n✓ pgman upgraded · `pgman --version` to check");
    Ok(())
}
