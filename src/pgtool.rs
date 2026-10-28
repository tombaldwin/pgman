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

/// Directories tried after `PATH`: Homebrew's libpq is keg-only, so a
/// Mac with `brew install libpq` has `pg_dump` but not on `PATH`.
pub const FALLBACK_DIRS: &[&str] = &["/opt/homebrew/opt/libpq/bin", "/usr/local/opt/libpq/bin"];

/// `name` in the first of `dirs` that has it as a file. Pure apart from
/// the existence checks.
pub fn find_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Where `name` (`pg_dump`) is: in `bin_dir` when given (`--pg-bin`),
/// else on `PATH`, else in [`FALLBACK_DIRS`].
pub fn find_tool(name: &str, bin_dir: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(dir) = bin_dir {
        return find_in(name, &[dir.to_path_buf()])
            .ok_or_else(|| format!("--pg-bin {}: no {name} there", dir.display()));
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(FALLBACK_DIRS.iter().map(PathBuf::from));
    find_in(name, &dirs).ok_or_else(|| {
        format!(
            "{name} not found on PATH — pgman runs your own PostgreSQL client tools. \
             Install them (macOS: brew install libpq; Debian/Ubuntu: apt install postgresql-client) \
             or point at them with --pg-bin DIR"
        )
    })
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
             work with a newer server. Install the {server} client tools (macOS: brew install \
             libpq, or postgresql@{server}) or point at them with --pg-bin DIR"
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
/// with pgman; `password` never leaves.
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
}

impl Tail {
    pub const CAP: usize = 200;

    pub fn push(&mut self, line: String) {
        if self.lines.len() == Self::CAP {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn text(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

/// Run `cmd` (stderr piped), echoing its stderr lines to ours when
/// `echo`, and keep their tail. Cancelled by Ctrl-C: the tool is killed
/// and the run says so.
pub async fn run_tool_cli(
    mut cmd: tokio::process::Command,
    echo: bool,
) -> Result<(std::process::ExitStatus, Tail), String> {
    use tokio::io::AsyncBufReadExt;
    cmd.stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("could not start: {e}"))?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    let mut tail = Tail::default();
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    if echo {
                        eprintln!("{l}");
                    }
                    tail.push(l);
                }
                Ok(None) | Err(_) => break,
            },
            _ = &mut ctrl_c => {
                let _ = child.kill().await;
                return Err("cancelled (Ctrl-C)".to_string());
            }
        }
    }
    let status = tokio::select! {
        s = child.wait() => s.map_err(|e| format!("waiting for it failed: {e}"))?,
        _ = &mut ctrl_c => {
            let _ = child.kill().await;
            return Err("cancelled (Ctrl-C)".to_string());
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
    fn the_tail_keeps_the_last_lines() {
        let mut t = Tail::default();
        for i in 0..(Tail::CAP + 5) {
            t.push(format!("line {i}"));
        }
        let text = t.text();
        assert!(!text.contains("line 4\n") && text.ends_with(&format!("line {}", Tail::CAP + 4)));
    }
}
