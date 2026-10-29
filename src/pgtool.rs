//! Running the operator's own `pg_dump` / `pg_restore` against a target
//! pgman found and opened: what `pgman dump` and `pgman restore` share.
//!
//! pgman does the parts it already does well — finding the database
//! ([`crate::inventory::target`]), reading the secret at connect, the
//! ssh tunnel — and hands the tool a connection that needs none of
//! them: `hostaddr=127.0.0.1` and the tunnel's port, with `host` left
//! as the server's real name so a verifying `sslmode` still checks the
//! certificate against it.
//!
//! The password reaches the tool only as `PGPASSWORD` in its own
//! environment: never on its command line (which any user's `ps` shows)
//! and never in a file. Every other `PG*` variable of pgman's own
//! environment is removed first, so an operator's `PGHOST` or
//! `PGPASSWORD` for some other server can't redirect the tool or follow
//! it there. [`conninfo`] and [`child_env`] are pure and tested for
//! exactly that.

use crate::conn::Dsn;
use crate::inventory::target::Resolved;
use std::path::{Path, PathBuf};

/// A PostgreSQL major version, comparable across the 9.x and 10+
/// numbering: 9.6 and 16 order as they should.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Major(u32);

impl Major {
    /// From `server_version_num` (`160010`, `90624`). Pure.
    pub fn from_num(num: i64) -> Option<Major> {
        if num <= 0 {
            return None;
        }
        let num = num as u32;
        Some(if num >= 100_000 {
            Major(num / 10_000 * 10_000)
        } else {
            Major(num / 100 * 100)
        })
    }

    /// From a version string's leading numbers: `16.10`, `9.6.24`,
    /// `18beta1`, `17devel`. Pure.
    pub fn parse(text: &str) -> Option<Major> {
        let digits = |s: &str| -> Option<u32> {
            let n: String = s.chars().take_while(char::is_ascii_digit).collect();
            n.parse().ok()
        };
        let mut parts = text.trim().split('.');
        let first = digits(parts.next()?)?;
        if first >= 10 {
            Some(Major(first * 10_000))
        } else {
            let minor = parts.next().and_then(digits).unwrap_or(0);
            Some(Major(first * 10_000 + minor * 100))
        }
    }
}

impl std::fmt::Display for Major {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let major = self.0 / 10_000;
        if major >= 10 {
            write!(f, "{major}")
        } else {
            write!(f, "{major}.{}", self.0 % 10_000 / 100)
        }
    }
}

/// The version a tool's `--version` prints: `pg_dump (PostgreSQL)
/// 16.10 (Ubuntu 16.10-1.pgdg24.04+1)`. Pure.
pub fn parse_tool_version(out: &str) -> Option<Major> {
    let line = out.lines().next()?;
    let after = line.split_once(')').map(|(_, r)| r).unwrap_or(line);
    after.split_whitespace().find_map(Major::parse)
}

/// Homebrew's prefixes, tried when `$HOMEBREW_PREFIX` doesn't say:
/// Apple silicon's, then Intel's.
pub const BREW_PREFIXES: &[&str] = &["/opt/homebrew", "/usr/local"];

/// The variable that names where pg_dump and pg_restore are. The
/// Homebrew formula sets it to its libpq dependency's `bin` (libpq is
/// keg-only, so nothing of it is on PATH), so a brew install finds
/// them with nothing linked.
pub const PG_BIN_VAR: &str = "PGMAN_PG_BIN";

/// One place pgman looks for the tools, and what to call it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub dir: PathBuf,
    /// `--pg-bin`, `PGMAN_PG_BIN`, `Homebrew's libpq`, `PATH`.
    pub source: &'static str,
}

/// Where to look for the tools, in order: `--pg-bin` alone when given;
/// else `$PGMAN_PG_BIN`, Homebrew's libpq (under `$HOMEBREW_PREFIX`,
/// then the standard prefixes — before PATH, so an older PostgreSQL on
/// PATH doesn't win over the libpq Homebrew keeps current), then each
/// PATH entry. `var` reads the environment. Pure.
pub fn candidates(pg_bin: Option<&Path>, var: &dyn Fn(&str) -> Option<String>) -> Vec<Candidate> {
    if let Some(dir) = pg_bin {
        return vec![Candidate {
            dir: dir.to_path_buf(),
            source: "--pg-bin",
        }];
    }
    let mut out = Vec::new();
    if let Some(dir) = var(PG_BIN_VAR).filter(|v| !v.is_empty()) {
        out.push(Candidate {
            dir: PathBuf::from(dir),
            source: PG_BIN_VAR,
        });
    }
    let mut prefixes: Vec<String> = var("HOMEBREW_PREFIX")
        .filter(|v| !v.is_empty())
        .into_iter()
        .collect();
    for p in BREW_PREFIXES {
        if !prefixes.iter().any(|q| q == p) {
            prefixes.push(p.to_string());
        }
    }
    for p in prefixes {
        out.push(Candidate {
            dir: Path::new(&p).join("opt/libpq/bin"),
            source: "Homebrew's libpq",
        });
    }
    if let Some(path) = var("PATH") {
        for dir in std::env::split_paths(&path) {
            out.push(Candidate {
                dir,
                source: "PATH",
            });
        }
    }
    out
}

/// `name` in the first of `dirs` that has it as a file. Pure apart from
/// the existence checks.
pub fn find_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// The first of `candidates` that has `name`, and where it came from.
/// Reads the filesystem.
pub fn first_found(name: &str, candidates: &[Candidate]) -> Option<(PathBuf, &'static str)> {
    candidates
        .iter()
        .map(|c| (c.dir.join(name), c.source))
        .find(|(p, _)| p.is_file())
}

/// `brew --prefix libpq`'s `bin`, for a Homebrew in an unusual place
/// that `$HOMEBREW_PREFIX` doesn't name: the last resort, as it runs
/// brew.
fn brew_libpq() -> Option<PathBuf> {
    let out = std::process::Command::new("brew")
        .args(["--prefix", "libpq"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!prefix.is_empty()).then(|| Path::new(&prefix).join("bin"))
}

/// Where `name` (`pg_dump`) is, and where it was found: see
/// [`candidates`] for the order, with `brew --prefix libpq` tried last.
pub fn find_tool_from(
    name: &str,
    bin_dir: Option<&Path>,
) -> Result<(PathBuf, &'static str), String> {
    let env = |k: &str| std::env::var(k).ok();
    let list = candidates(bin_dir, &env);
    if let Some(found) = first_found(name, &list) {
        return Ok(found);
    }
    if let Some(dir) = bin_dir {
        return Err(format!("--pg-bin {}: no {name} there", dir.display()));
    }
    if let Some(found) = brew_libpq().and_then(|d| {
        first_found(
            name,
            &[Candidate {
                dir: d,
                source: "Homebrew's libpq",
            }],
        )
    }) {
        return Ok(found);
    }
    Err(format!(
        "{name} not found — pgman runs your own PostgreSQL client tools. Install them \
         (macOS: brew install libpq; Debian/Ubuntu: apt install postgresql-client), or point at \
         them with --pg-bin DIR or {PG_BIN_VAR}"
    ))
}

/// [`find_tool_from`], the path alone.
pub fn find_tool(name: &str, bin_dir: Option<&Path>) -> Result<PathBuf, String> {
    find_tool_from(name, bin_dir).map(|(p, _)| p)
}

/// `tool --version`, parsed.
pub fn tool_version(tool: &Path) -> Result<Major, String> {
    let out = std::process::Command::new(tool)
        .arg("--version")
        .env_clear()
        .output()
        .map_err(|e| format!("could not run {}: {e}", tool.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_tool_version(&text)
        .ok_or_else(|| format!("{} --version said {:?}", tool.display(), text.trim()))
}

/// The refusal when `tool` (major `have`) is older than the server
/// (`server`), or `None`. pg_dump refuses a newer server itself, but
/// only once connected and with a message that names neither fix.
/// Pure.
pub fn version_refusal(tool: &str, have: Major, server: Major) -> Option<String> {
    (have < server).then(|| {
        format!(
            "your {tool} is version {have}, older than the server's {server}: {tool} can't \
             work with a newer server. Get the {server} client tools (macOS: brew upgrade libpq, \
             or brew install libpq or postgresql@{server}) or point at them with --pg-bin DIR"
        )
    })
}

/// A libpq connection-string value, quoted: `'it''s'` is not how libpq
/// escapes, `'it\'s'` is. Always quoted, so a space or `=` in a name
/// can't start another keyword. Pure.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// The DSN parameters passed on to the tool: the TLS settings and the
/// session options libpq knows. Anything else pgman accepts on a URL
/// (`ssh_tunnel`, `application_name`, which pgman sets itself) stays
/// with pgman; `password` never leaves. `options` (session settings,
/// `-c search_path=…`) is passed: it comes only from the operator's own
/// connections.toml, environments.toml or a URL they typed — a target
/// never comes from a file in the checkout — and pgman, not it, chooses
/// the server.
const PASSED_PARAMS: &[&str] = &[
    "sslmode",
    "sslrootcert",
    "sslcert",
    "sslkey",
    "sslnegotiation",
    "channel_binding",
    "gssencmode",
    "options",
    "target_session_attrs",
];

/// The connection string for `--dbname` / `-d`: `host` is the server's
/// own name (what TLS checks), `hostaddr` the tunnel's end when there is
/// one. No password, whatever `dsn` carries. With a verifying `sslmode`
/// and no `sslrootcert`, `sslrootcert=system` (libpq 16+): the system's
/// roots, as pgman itself trusts, rather than libpq's
/// `~/.postgresql/root.crt`. Pure.
pub fn conninfo(dsn: &Dsn, via: Option<u16>, application: &str, tool: Major) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut kv = |k: &str, v: &str| parts.push(format!("{k}={}", quote(v)));
    kv("host", &dsn.host);
    match via {
        Some(port) => {
            kv("hostaddr", "127.0.0.1");
            kv("port", &port.to_string());
        }
        None => kv("port", &dsn.port.to_string()),
    }
    kv("dbname", &dsn.dbname);
    if let Some(u) = &dsn.user {
        kv("user", u);
    }
    kv("application_name", application);
    kv("connect_timeout", "10");
    let mut verifying = false;
    let mut rootcert = false;
    for (k, v) in &dsn.params {
        let key = k.to_ascii_lowercase();
        if !PASSED_PARAMS.contains(&key.as_str()) {
            continue;
        }
        if key == "sslmode" {
            verifying = v.to_ascii_lowercase().starts_with("verify");
        }
        if key == "sslrootcert" {
            rootcert = true;
        }
        kv(&key, v);
    }
    if verifying && !rootcert && tool >= Major(160_000) {
        kv("sslrootcert", "system");
    }
    parts.join(" ")
}

/// Of `vars` (pgman's environment), the names to remove from the tool's:
/// every `PG*` one. Pure.
pub fn pg_vars(vars: impl IntoIterator<Item = String>) -> Vec<String> {
    vars.into_iter().filter(|k| k.starts_with("PG")).collect()
}

/// What the tool's environment changes from pgman's: each of `inherited`
/// `PG*` names removed, then `PGPASSWORD` set when there is a password.
/// Pure: `(name, Some(value))` sets, `(name, None)` removes.
pub fn child_env(inherited: Vec<String>, password: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> =
        pg_vars(inherited).into_iter().map(|k| (k, None)).collect();
    if let Some(p) = password {
        out.push(("PGPASSWORD".to_string(), Some(p.to_string())));
    }
    out
}

/// A command running `tool` with `args` under a umask of 077, so what
/// it creates — the dump, a directory format's files — is its owner's
/// alone from the moment it exists. pg_dump replaces its output file
/// rather than writing into one made for it, so making it private first
/// isn't enough. Through `sh`, `"$@"` passes every argument as it is:
/// none is read by the shell.
pub fn private_command(tool: &Path, args: &[String]) -> tokio::process::Command {
    #[cfg(unix)]
    {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(PRIVATE_SH).arg(tool).args(args);
        cmd
    }
    #[cfg(not(unix))]
    {
        let mut cmd = tokio::process::Command::new(tool);
        cmd.args(args);
        cmd
    }
}

/// The `sh -c` script [`private_command`] runs: `$0` is the tool.
pub const PRIVATE_SH: &str = "umask 077 && exec \"$0\" \"$@\"";

/// Apply [`child_env`] to `cmd`, from pgman's own environment.
pub fn set_child_env(cmd: &mut tokio::process::Command, password: Option<&str>) {
    let inherited = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .collect();
    for (k, v) in child_env(inherited, password) {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
}

/// Ask on the terminal; the line typed, trimmed.
pub fn ask_cli(prompt: &str) -> Result<String, String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err("stdin isn't a terminal, so pgman can't ask".into());
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("could not read the answer: {e}"))?;
    Ok(line.trim().to_string())
}

/// Ask on the terminal; `Ok(true)` when the answer is `accept`.
pub fn confirm_cli(prompt: &str, accept: &str) -> Result<bool, String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err("stdin isn't a terminal, so pgman can't ask; pass --yes to go ahead".into());
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("could not read the answer: {e}"))?;
    let answer = line.trim();
    Ok(if accept == "y" {
        answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
    } else {
        answer == accept
    })
}

/// A connect error for `target`, worded for the command line: the
/// library's "password missing" — the server asked for a password and
/// the target supplied none — says where one comes from. Pure.
pub fn explain_connect(err: String, target: &Resolved) -> String {
    if !err.contains("password missing") {
        return err;
    }
    let dsn = &target.connect.dsn;
    let who = format!(
        "{}@{}",
        dsn.user.as_deref().unwrap_or("(no user)"),
        crate::conn::host_port(&dsn.host, dsn.port)
    );
    if let Some(var) = &target.connect.password_env {
        return format!("no password for {who}: password_env names ${var}, which is not set");
    }
    format!(
        "no password for {who}, and the server asks for one. Put it in the URL \
         (postgres://user:password@…), or use a connections.toml entry with password_env or a \
         secret. pgman doesn't read PGPASSWORD here"
    )
}

/// A target with its secret read and its tunnel up: what the tool
/// connects through. Dropping it closes the tunnel.
pub struct Opened {
    /// With the password, without the tunnel (it is up: [`Opened::via`]).
    pub dsn: Dsn,
    /// The tunnel's local port, when there is one.
    pub via: Option<u16>,
    _tunnel: Option<crate::tunnel::SshTunnel>,
}

impl Opened {
    /// The password, for [`set_child_env`].
    pub fn password(&self) -> Option<&str> {
        self.dsn.password.as_deref()
    }
}

/// Read `target`'s secret (now, not before) and open its tunnel.
pub async fn open(target: &Resolved) -> Result<Opened, String> {
    let fetch = crate::inventory::patching::aws_secrets();
    let mut dsn = crate::inventory::patching::spec_dsn(target.connect.clone(), &fetch).await?;
    let tunnel = match dsn.ssh_tunnel.take() {
        Some(spec) => {
            let (host, port) = (dsn.host.clone(), dsn.port);
            Some(
                tokio::task::spawn_blocking(move || {
                    crate::tunnel::SshTunnel::open(&spec, &host, port)
                })
                .await
                .map_err(|e| format!("ssh tunnel task failed: {e}"))??,
            )
        }
        None => None,
    };
    Ok(Opened {
        dsn,
        via: tunnel.as_ref().map(|t| t.local_port),
        _tunnel: tunnel,
    })
}

/// Lines a tool wrote to stderr, the last `cap` kept: what an error
/// message is read from once it exits.
#[derive(Debug, Default)]
pub struct Tail {
    lines: std::collections::VecDeque<String>,
    /// Every `error:` line and the `Command was:` line after it, however
    /// long the output: what an error count is read from, which a
    /// `--verbose` run's progress would otherwise push out of the tail.
    errors: Vec<String>,
    after_error: bool,
}

impl Tail {
    pub const CAP: usize = 200;
    /// Error lines kept, at most.
    pub const ERRORS_CAP: usize = 20_000;

    pub fn push(&mut self, line: String) {
        let is_error = line.contains("error:");
        let keep = is_error || (self.after_error && line.starts_with("Command was:"));
        self.after_error = is_error;
        if keep && self.errors.len() < Self::ERRORS_CAP {
            self.errors.push(line.clone());
        }
        if self.lines.len() == Self::CAP {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn text(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// The kept error lines, as text.
    pub fn errors(&self) -> String {
        self.errors.join("\n")
    }
}

/// What a run stopped by [`interrupted`] says.
pub const STOPPED: &str = "stopped (Ctrl-C, or the process was signalled)";

/// Completes when the operator stops a command-line run: Ctrl-C, and on
/// Unix also SIGTERM (`kill`) and SIGHUP (the terminal closing). Each
/// way, the run then cleans up as for Ctrl-C — the tool killed, the
/// partial file removed, the transaction rolled back — rather than the
/// process ending with pg_dump left running and the tunnel up.
pub async fn interrupted() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).ok();
        let mut hup = signal(SignalKind::hangup()).ok();
        let term = async {
            match term.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let hup = async {
            match hup.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term => {}
            _ = hup => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Run `cmd` (stderr piped), echoing its stderr lines to ours when
/// `echo`, and keep their tail. Cancelled by Ctrl-C: the tool is killed
/// and the run says so.
pub async fn run_tool_cli(
    cmd: tokio::process::Command,
    echo: bool,
) -> Result<(std::process::ExitStatus, Tail), String> {
    run_tool_shown_cli(cmd, echo, |l| l.to_string()).await
}

/// [`run_tool_cli`], each echoed line passed through `shown` first (a
/// restore's, with values redacted). The tail keeps the lines as they
/// were.
pub async fn run_tool_shown_cli(
    mut cmd: tokio::process::Command,
    echo: bool,
    shown: impl Fn(&str) -> String,
) -> Result<(std::process::ExitStatus, Tail), String> {
    use tokio::io::AsyncBufReadExt;
    cmd.stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("could not start: {e}"))?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    let mut tail = Tail::default();
    let mut ctrl_c = std::pin::pin!(interrupted());
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    if echo {
                        eprintln!("{}", shown(&l));
                    }
                    tail.push(l);
                }
                Ok(None) | Err(_) => break,
            },
            _ = &mut ctrl_c => {
                let _ = child.kill().await;
                return Err(STOPPED.to_string());
            }
        }
    }
    let status = tokio::select! {
        s = child.wait() => s.map_err(|e| format!("waiting for it failed: {e}"))?,
        _ = &mut ctrl_c => {
            let _ = child.kill().await;
            return Err(STOPPED.to_string());
        }
    };
    Ok((status, tail))
}

/// `1.2 GB`, through the one byte formatter. Pure.
pub fn size_of(bytes: u64) -> String {
    crate::query::schema::format_bytes(bytes)
}

/// The size of `path`: a file's, or a directory's files' summed (a
/// directory-format dump).
pub fn path_size(path: &Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => std::fs::read_dir(path)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| path_size(&e.path()))
                    .sum()
            })
            .unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn majors_compare_across_the_old_and_new_numbering() {
        let v96 = Major::from_num(90624).unwrap();
        let v16 = Major::from_num(160010).unwrap();
        assert!(v96 < v16);
        assert_eq!(v96.to_string(), "9.6");
        assert_eq!(v16.to_string(), "16");
        assert_eq!(Major::parse("16.10"), Some(v16));
        assert_eq!(Major::parse("9.6.24"), Some(v96));
        assert_eq!(Major::parse("18beta1"), Major::parse("18.0"));
        assert_eq!(Major::parse("17devel").unwrap().to_string(), "17");
        assert_eq!(Major::parse("x"), None);
        assert_eq!(Major::from_num(0), None);
    }

    #[test]
    fn a_tool_version_line_parses_with_or_without_a_distro_suffix() {
        assert_eq!(
            parse_tool_version("pg_dump (PostgreSQL) 18.4\n")
                .unwrap()
                .to_string(),
            "18"
        );
        assert_eq!(
            parse_tool_version("pg_dump (PostgreSQL) 16.10 (Ubuntu 16.10-1.pgdg24.04+1)")
                .unwrap()
                .to_string(),
            "16"
        );
        assert_eq!(
            parse_tool_version("pg_restore (PostgreSQL) 9.6.24")
                .unwrap()
                .to_string(),
            "9.6"
        );
        assert_eq!(parse_tool_version(""), None);
        assert_eq!(parse_tool_version("command not found"), None);
    }

    #[test]
    fn an_older_tool_is_refused_and_an_equal_or_newer_one_is_not() {
        let v = |s| Major::parse(s).unwrap();
        let e = version_refusal("pg_dump", v("15.4"), v("16.10")).unwrap();
        assert!(e.contains("version 15") && e.contains("server's 16"), "{e}");
        assert_eq!(version_refusal("pg_dump", v("16.1"), v("16.10")), None);
        assert_eq!(version_refusal("pg_dump", v("18.4"), v("9.6")), None);
    }

    fn dsn(url: &str) -> Dsn {
        Dsn::parse(url).unwrap()
    }

    #[test]
    fn the_connection_string_never_carries_the_password() {
        let d = dsn(
            "postgres://app_ro:s3cret-pw@db.example:5432/ledger?sslmode=require&password=other-pw",
        );
        assert_eq!(d.password.as_deref(), Some("s3cret-pw"));
        let ci = conninfo(&d, None, "pgman-dump", Major::parse("16").unwrap());
        assert!(!ci.contains("s3cret-pw"), "{ci}");
        assert!(!ci.contains("other-pw"), "{ci}");
        assert!(!ci.contains("password"), "{ci}");
        assert!(
            ci.contains("user='app_ro'") && ci.contains("sslmode='require'"),
            "{ci}"
        );
    }

    #[test]
    fn through_a_tunnel_the_host_stays_the_servers_name() {
        let d = dsn("postgres://app_ro@db.example:5432/ledger?sslmode=verify-full");
        let ci = conninfo(&d, Some(61234), "pgman-dump", Major::parse("16").unwrap());
        assert!(
            ci.starts_with("host='db.example' hostaddr='127.0.0.1' port='61234' dbname='ledger'"),
            "{ci}"
        );
        assert!(ci.contains("sslrootcert='system'"), "{ci}");
        let direct = conninfo(&d, None, "pgman-dump", Major::parse("16").unwrap());
        assert!(
            direct.contains("port='5432'") && !direct.contains("hostaddr"),
            "{direct}"
        );
    }

    #[test]
    fn system_roots_only_where_libpq_knows_them_and_none_was_named() {
        let d = dsn("postgres://u@h/db?sslmode=verify-ca&sslrootcert=/etc/rds.pem");
        let ci = conninfo(&d, None, "x", Major::parse("16").unwrap());
        assert!(
            ci.contains("sslrootcert='/etc/rds.pem'") && !ci.contains("'system'"),
            "{ci}"
        );
        let d = dsn("postgres://u@h/db?sslmode=verify-full");
        let old = conninfo(&d, None, "x", Major::parse("15").unwrap());
        assert!(!old.contains("sslrootcert"), "{old}");
        let d = dsn("postgres://u@h/db?sslmode=require");
        assert!(!conninfo(&d, None, "x", Major::parse("16").unwrap()).contains("sslrootcert"));
    }

    #[test]
    fn values_are_quoted_so_a_name_cant_add_a_keyword() {
        let mut d = dsn("postgres://u@h/db");
        d.dbname = "it's x password=leak".to_string();
        let ci = conninfo(&d, None, "x", Major::parse("16").unwrap());
        assert!(ci.contains(r"dbname='it\'s x password=leak'"), "{ci}");
        assert_eq!(quote(r"a\b"), r"'a\\b'");
    }

    #[test]
    fn pgmans_only_params_stay_behind() {
        let d = dsn("postgres://u@h/db?application_name=mine&connect_timeout=99&options=-c%20search_path%3Dx");
        let ci = conninfo(&d, None, "pgman-dump", Major::parse("16").unwrap());
        assert!(
            ci.contains("application_name='pgman-dump'") && !ci.contains("mine"),
            "{ci}"
        );
        assert!(!ci.contains("'99'"), "{ci}");
        assert!(ci.contains("options="), "{ci}");
    }

    #[test]
    fn the_child_loses_every_pg_variable_and_gets_only_its_password() {
        let env = child_env(
            vec![
                "PATH".into(),
                "PGPASSWORD".into(),
                "PGHOST".into(),
                "PGSERVICEFILE".into(),
                "HOME".into(),
            ],
            Some("s3cret"),
        );
        assert_eq!(
            env,
            vec![
                ("PGPASSWORD".to_string(), None),
                ("PGHOST".to_string(), None),
                ("PGSERVICEFILE".to_string(), None),
                ("PGPASSWORD".to_string(), Some("s3cret".to_string())),
            ]
        );
        assert_eq!(
            child_env(vec!["PGPASSWORD".into()], None),
            vec![("PGPASSWORD".to_string(), None)],
            "no password: the inherited one is still removed"
        );
    }

    #[test]
    fn the_lookup_order_is_pg_bin_then_the_variable_then_libpq_then_path() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let order = |c: Vec<Candidate>| {
            c.into_iter()
                .map(|c| format!("{}={}", c.source, c.dir.display()))
                .collect::<Vec<_>>()
        };
        let all = env(&[
            ("PGMAN_PG_BIN", "/brew/opt/libpq/bin"),
            ("HOMEBREW_PREFIX", "/custom"),
            ("PATH", "/usr/bin:/opt/pg14/bin"),
        ]);
        assert_eq!(
            order(candidates(None, &all)),
            [
                "PGMAN_PG_BIN=/brew/opt/libpq/bin",
                "Homebrew's libpq=/custom/opt/libpq/bin",
                "Homebrew's libpq=/opt/homebrew/opt/libpq/bin",
                "Homebrew's libpq=/usr/local/opt/libpq/bin",
                "PATH=/usr/bin",
                "PATH=/opt/pg14/bin",
            ]
        );
        assert_eq!(
            order(candidates(Some(Path::new("/mine")), &all)),
            ["--pg-bin=/mine"],
            "--pg-bin alone"
        );
        let bare = env(&[("HOMEBREW_PREFIX", "/opt/homebrew"), ("PGMAN_PG_BIN", "")]);
        assert_eq!(
            order(candidates(None, &bare)),
            [
                "Homebrew's libpq=/opt/homebrew/opt/libpq/bin",
                "Homebrew's libpq=/usr/local/opt/libpq/bin",
            ],
            "an empty variable is unset; a prefix isn't tried twice"
        );
    }

    #[test]
    fn the_first_candidate_that_has_the_tool_wins() {
        let root = std::env::temp_dir().join(format!("pgman-pgtool-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (libpq, path) = (root.join("libpq/bin"), root.join("path/bin"));
        std::fs::create_dir_all(&libpq).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("pg_dump"), "").unwrap();
        let c = |d: &Path, s: &'static str| Candidate {
            dir: d.to_path_buf(),
            source: s,
        };
        let list = [c(&libpq, "Homebrew's libpq"), c(&path, "PATH")];
        assert_eq!(
            first_found("pg_dump", &list),
            Some((path.join("pg_dump"), "PATH"))
        );
        std::fs::write(libpq.join("pg_dump"), "").unwrap();
        assert_eq!(
            first_found("pg_dump", &list),
            Some((libpq.join("pg_dump"), "Homebrew's libpq")),
            "libpq before PATH"
        );
        assert_eq!(first_found("pg_restore", &list), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn find_in_takes_the_first_directory_that_has_it() {
        let root = std::env::temp_dir().join(format!("pgman-pgtool-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("pg_dump"), "").unwrap();
        assert_eq!(
            find_in("pg_dump", &[a.clone(), b.clone()]),
            Some(b.join("pg_dump"))
        );
        assert_eq!(find_in("pg_restore", &[a, b]), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_private_command_makes_private_files_and_passes_arguments_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pgman-pgtool-umask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("made");
        let tricky = "$(echo no) `x` ; 'q' \"dq\"";
        let status = private_command(
            Path::new("/bin/sh"),
            &[
                "-c".to_string(),
                "printf '%s' \"$1\" > \"$2\"".to_string(),
                "sh".to_string(),
                tricky.to_string(),
                out.display().to_string(),
            ],
        )
        .status()
        .await
        .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), tricky);
        let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_error_is_kept_past_the_tail() {
        let mut t = Tail::default();
        t.push("pg_restore: error: could not execute query: ERROR:  x".into());
        t.push("Command was: SET transaction_timeout = 0;".into());
        for i in 0..(Tail::CAP * 3) {
            t.push(format!("pg_restore: processing item {i}"));
        }
        assert!(!t.text().contains("error:"), "scrolled out of the tail");
        assert_eq!(
            t.errors(),
            "pg_restore: error: could not execute query: ERROR:  x\nCommand was: SET transaction_timeout = 0;"
        );
    }

    #[test]
    fn a_missing_password_says_where_one_comes_from() {
        let t = crate::inventory::target::from_url("postgres://app_ro@db.example/ledger").unwrap();
        let e = explain_connect("invalid configuration: password missing".into(), &t);
        assert!(
            e.starts_with("no password for app_ro@db.example:5432")
                && e.contains("doesn't read PGPASSWORD here"),
            "{e}"
        );
        assert_eq!(explain_connect("timed out".into(), &t), "timed out");
    }

    #[test]
    fn the_tail_keeps_the_last_lines() {
        let mut t = Tail::default();
        for i in 0..(Tail::CAP + 5) {
            t.push(format!("line {i}"));
        }
        let text = t.text();
        assert!(!text.contains("line 4\n") && text.ends_with(&format!("line {}", Tail::CAP + 4)));
    }
}
