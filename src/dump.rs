//! `pgman dump <target> [FILE]`: a dump of a database pgman knows, taken
//! by the operator's own `pg_dump` through pgman's connection — its
//! secret read at that moment, its tunnel — from the replica when the
//! environment has one.
//!
//! The dump is written under a temporary name next to where it is going
//! and renamed into place only once pg_dump has succeeded, so a failed
//! or cancelled dump never replaces a good one. With no FILE (or a
//! directory for one), the dump is named after the target and the time,
//! and `<target>-latest.<ext>` is pointed at it.
//!
//! Before anything runs, the local pg_dump's major version is checked
//! against the server's: an older pg_dump can't dump a newer server.
//! Afterwards a failure is read for the two causes worth naming — a
//! replica cancelling a long read (a recovery conflict), and a user who
//! can't read everything — rather than left as pg_dump's last line.

use crate::inventory::target::{self, Want};
use crate::pgtool::{self, Major};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "\
usage: pgman dump <target> [FILE] [--primary] [--format custom|plain|directory|tar]
                  [--pg-bin DIR] [--verbose] [-- PG_DUMP_OPTIONS...]

Dump a database with your own pg_dump, through pgman's connection: the
password read from its secret (or password_env) now, the ssh tunnel
opened for it. Nothing secret goes on a command line or to disk.

<target> is ENV, ENV/DATABASE or ENV/SERVER/DATABASE from
environments.toml, a connection name from connections.toml, or a
postgres:// URL. An environment's replica is used when it has one;
--primary takes the primary instead.

FILE defaults to <target>-<UTC time>.dump in the current directory (or
in FILE, when it is a directory), and <target>-latest.dump is pointed
at the newest. The dump is written to a temporary name and renamed
into place when pg_dump succeeds: a failed dump never replaces a file.

  --format       pg_dump's format (default custom, for pg_restore)
  --pg-bin DIR   where pg_dump is (default: PATH, then Homebrew's libpq)
  --verbose      show pg_dump's progress
  --             anything after it goes to pg_dump as it is
                 (--schema-only, --exclude-table-data=audit_log, -j 4 …);
                 the connection and output options are pgman's

  pgman dump staging/ledger
  pgman dump staging/ledger ~/dumps/ -- --exclude-table-data=audit_log";

/// pg_dump's output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Custom,
    Plain,
    Directory,
    Tar,
}

impl Format {
    pub fn parse(s: &str) -> Result<Format, String> {
        match s {
            "custom" | "c" => Ok(Format::Custom),
            "plain" | "p" => Ok(Format::Plain),
            "directory" | "d" => Ok(Format::Directory),
            "tar" | "t" => Ok(Format::Tar),
            other => Err(format!(
                "--format {other:?}: use custom, plain, directory or tar"
            )),
        }
    }

    /// pg_dump's `--format` letter.
    pub fn letter(self) -> &'static str {
        match self {
            Format::Custom => "c",
            Format::Plain => "p",
            Format::Directory => "d",
            Format::Tar => "t",
        }
    }

    /// What a default name ends in.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Custom => ".dump",
            Format::Plain => ".sql",
            Format::Directory => ".dir",
            Format::Tar => ".tar",
        }
    }
}

/// Parsed `pgman dump` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    pub target: String,
    pub out: Option<PathBuf>,
    pub primary: bool,
    pub format: Format,
    pub pg_bin: Option<PathBuf>,
    pub verbose: bool,
    /// After `--`: passed to pg_dump.
    pub extra: Vec<String>,
}

/// Parse the arguments after `pgman dump`. Pure.
pub fn parse_cli(args: &[String]) -> Result<Opts, String> {
    let mut positional: Vec<String> = Vec::new();
    let mut primary = false;
    let mut format = Format::Custom;
    let mut pg_bin = None;
    let mut verbose = false;
    let mut extra = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))
        };
        match a.as_str() {
            "--" => {
                extra = it.by_ref().cloned().collect();
                break;
            }
            "--primary" => primary = true,
            "--verbose" | "-v" => verbose = true,
            "--format" => format = Format::parse(&value("--format")?)?,
            "--pg-bin" => pg_bin = Some(PathBuf::from(value("--pg-bin")?)),
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown option {other:?} (pg_dump's own go after --)\n\n{USAGE}"
                ))
            }
            other => positional.push(other.to_string()),
        }
    }
    let mut positional = positional.into_iter();
    let target = positional
        .next()
        .ok_or_else(|| format!("which database? name a target\n\n{USAGE}"))?;
    let out = positional.next().map(PathBuf::from);
    if let Some(more) = positional.next() {
        return Err(format!("unexpected argument {more:?}\n\n{USAGE}"));
    }
    if let Some(why) = refused_passthrough(&extra) {
        return Err(why);
    }
    Ok(Opts {
        target,
        out,
        primary,
        format,
        pg_bin,
        verbose,
        extra,
    })
}

/// pg_dump's short options that take a value: in a bundle (`-sxt foo`)
/// the rest of the bundle, or the next argument, is that value.
const SHORT_WITH_VALUE: &str = "dEefFhjnNpStTUZ";
/// The ones pgman sets: where to connect, as whom, and where to write.
const SHORT_REFUSED: &str = "dhpUfFWw";
const LONG_REFUSED: &[&str] = &[
    "dbname",
    "host",
    "port",
    "username",
    "file",
    "format",
    "password",
    "no-password",
];
/// pg_dump's long options that take a separate value (`--table foo`).
const LONG_WITH_VALUE: &[&str] = &[
    "compress",
    "encoding",
    "exclude-schema",
    "exclude-table",
    "exclude-table-and-children",
    "exclude-table-data",
    "exclude-table-data-and-children",
    "exclude-extension",
    "extension",
    "extra-float-digits",
    "filter",
    "jobs",
    "lock-wait-timeout",
    "role",
    "rows-per-insert",
    "schema",
    "section",
    "snapshot",
    "superuser",
    "table",
    "table-and-children",
];

/// Why `extra` can't go to pg_dump: it sets the connection or the
/// output, which pgman owns (a `--dbname` there would bypass the
/// tunnel; `--file` the temporary name). `None` when it can. Pure.
pub fn refused_passthrough(extra: &[String]) -> Option<String> {
    passthrough_refusal(
        extra,
        SHORT_WITH_VALUE,
        SHORT_REFUSED,
        LONG_REFUSED,
        LONG_WITH_VALUE,
    )
}

/// [`refused_passthrough`] for any tool's option table (pg_restore's
/// too). Pure.
pub fn passthrough_refusal(
    extra: &[String],
    short_with_value: &str,
    short_refused: &str,
    long_refused: &[&str],
    long_with_value: &[&str],
) -> Option<String> {
    let refuse = |opt: &str| {
        Some(format!(
            "{opt} is pgman's to set: the connection and the output come from the target and FILE"
        ))
    };
    let mut skip_value = false;
    for arg in extra {
        if std::mem::take(&mut skip_value) {
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, _)) => (n, true),
                None => (long, false),
            };
            // getopt_long takes any unambiguous prefix: `--db=` is
            // `--dbname=`. So a prefix of a refused name is refused too.
            if !name.is_empty() && long_refused.iter().any(|r| r.starts_with(name)) {
                return refuse(&format!("--{name}"));
            }
            skip_value = !inline && long_with_value.iter().any(|r| r.starts_with(name));
        } else if let Some(bundle) = arg.strip_prefix('-').filter(|b| !b.is_empty()) {
            for (i, c) in bundle.char_indices() {
                if short_refused.contains(c) {
                    return refuse(&format!("-{c}"));
                }
                if short_with_value.contains(c) {
                    skip_value = i + c.len_utf8() == bundle.len();
                    break;
                }
            }
        }
    }
    None
}

/// `20261028T140322Z`. Pure.
pub fn stamp(unix_secs: u64) -> String {
    crate::app::format_unix_secs_utc(unix_secs).replace(['-', ':'], "")
}

/// `staging/ledger` as a file-name part: `staging-ledger`. Pure.
pub fn slug(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches(|c| c == '-' || c == '.').to_string();
    if s.is_empty() {
        "dump".to_string()
    } else {
        s
    }
}

/// Where a dump goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub file: PathBuf,
    /// The `-latest` link to point at it, for a default name.
    pub latest: Option<PathBuf>,
    /// What pg_dump writes, renamed to `file` on success.
    pub partial: PathBuf,
}

/// Where a dump of `label` taken at `unix_secs` goes, given FILE
/// (`out`, `out_is_dir` when it names a directory). Pure.
pub fn paths(
    label: &str,
    format: Format,
    out: Option<&Path>,
    out_is_dir: bool,
    unix_secs: u64,
    pid: u32,
) -> Paths {
    let base = slug(label);
    let ext = format.extension();
    let (file, latest) = match out {
        Some(p) if !out_is_dir => (p.to_path_buf(), None),
        dir => {
            let dir = dir.map(Path::to_path_buf).unwrap_or_default();
            (
                dir.join(format!("{base}-{}{ext}", stamp(unix_secs))),
                Some(dir.join(format!("{base}-latest{ext}"))),
            )
        }
    };
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{base}{ext}"));
    let partial = file.with_file_name(format!(".{name}.partial-{pid}"));
    Paths {
        file,
        latest,
        partial,
    }
}

/// The last error line of a tool's stderr, else its last line. Pure.
pub fn last_error(tail: &str) -> Option<&str> {
    tail.lines()
        .rev()
        .find(|l| l.contains("error:") || l.contains("ERROR:") || l.contains("FATAL:"))
        .or_else(|| tail.lines().rev().find(|l| !l.trim().is_empty()))
        .map(str::trim)
}

/// What a failed pg_dump's stderr (`tail`) means, for the operator.
/// `replica`: it ran on a standby. `user`: who it connected as. Pure.
pub fn explain_failure(tail: &str, replica: bool, user: Option<&str>) -> String {
    let lower = tail.to_ascii_lowercase();
    let last = last_error(tail).unwrap_or("pg_dump failed with no message");
    // A standby reports some recovery conflicts as a deadlock: its
    // replay of the primary's locks is one of the parties.
    if lower.contains("conflict with recovery")
        || lower.contains("deadlock with recovery")
        || (replica && lower.contains("deadlock detected"))
    {
        let primary = if replica {
            "; if it keeps happening, take it from the primary with --primary (which then carries the dump's reads)"
        } else {
            ""
        };
        return format!(
            "the replica cancelled the dump: a long read conflicted with changes it was replaying \
             from the primary (a recovery conflict).\n  Try again, at a quieter time if you can{primary}, \
             or raise max_standby_streaming_delay on the replica.\n  pg_dump said: {last}"
        );
    }
    if lower.contains("permission denied") {
        let who = user.map(|u| format!(" ({u})")).unwrap_or_default();
        return format!(
            "{last}\n  The dump's user{who} can't read everything the dump covers. Dump as a user \
             that can (the owner, or one granted pg_read_all_data), or leave those objects out: \
             -- --exclude-table=… / --exclude-schema=…"
        );
    }
    if lower.contains("server version mismatch") {
        return format!("{last}\n  Use a pg_dump at least as new as the server (--pg-bin DIR).");
    }
    if lower.contains("password authentication failed") {
        return format!(
            "{last}\n  The password pgman read (secret or password_env) was refused for this user."
        );
    }
    last.to_string()
}

/// Remove what a failed dump left: a file, or a directory format's
/// directory.
fn remove_partial(p: &Path) {
    let _ = if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    };
}

/// Move the finished dump into place: a file is renamed over whatever
/// was there; a directory replaces an old one only once it is aside.
fn commit(partial: &Path, file: &Path) -> Result<(), String> {
    if partial.is_file() {
        std::fs::File::open(partial)
            .and_then(|f| f.sync_all())
            .map_err(|e| format!("could not flush {}: {e}", partial.display()))?;
    }
    if partial.is_dir() && file.is_dir() {
        let aside = file.with_file_name(format!(
            ".{}.old-{}",
            file.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        std::fs::rename(file, &aside)
            .map_err(|e| format!("could not move the old {} aside: {e}", file.display()))?;
        std::fs::rename(partial, file).map_err(|e| {
            let _ = std::fs::rename(&aside, file);
            format!("could not move the dump into place: {e}")
        })?;
        let _ = std::fs::remove_dir_all(&aside);
        return Ok(());
    }
    std::fs::rename(partial, file).map_err(|e| {
        format!(
            "could not move the dump into place at {}: {e}",
            file.display()
        )
    })
}

/// Point `latest` at `file` (same directory): a new link renamed over
/// the old one, so it is never missing. A regular file already there is
/// left alone — it isn't pgman's.
pub fn point_latest(latest: &Path, file: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        if std::fs::symlink_metadata(latest).is_ok_and(|m| !m.file_type().is_symlink()) {
            return Err(format!(
                "{} exists and isn't a link; left alone",
                latest.display()
            ));
        }
        let name = file.file_name().ok_or("the dump has no file name")?;
        let tmp = latest.with_file_name(format!(
            ".{}.tmp-{}",
            latest.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(name, &tmp)
            .and_then(|()| std::fs::rename(&tmp, latest))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("could not point {} at it: {e}", latest.display())
            })
    }
    #[cfg(not(unix))]
    {
        let _ = (latest, file);
        Ok(())
    }
}

/// The pg_dump command line for `conninfo`, writing `partial`. The
/// password is not an argument: [`pgtool::set_child_env`] gives it to
/// the process alone. Pure.
pub fn pg_dump_args(conninfo: &str, opts: &Opts, partial: &Path) -> Vec<String> {
    let mut args = vec![
        format!("--dbname={conninfo}"),
        format!("--format={}", opts.format.letter()),
        format!("--file={}", partial.display()),
        "--no-password".to_string(),
    ];
    if opts.verbose {
        args.push("--verbose".to_string());
    }
    args.extend(opts.extra.iter().cloned());
    args
}

/// What `pgman dump` reports when it succeeds.
#[derive(Debug, Clone)]
pub struct Done {
    pub file: PathBuf,
    pub latest: Option<PathBuf>,
    pub bytes: u64,
    pub source: String,
    pub notes: Vec<String>,
}

/// Run a dump. Progress goes to stderr; the result is what to print.
pub async fn run_cli(opts: &Opts) -> Result<Done, String> {
    let want = if opts.primary {
        Want::Primary
    } else {
        Want::PreferReplica
    };
    let text = opts.target.clone();
    let resolved = tokio::task::spawn_blocking(move || target::resolve(&text, want))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))??;
    let tool = pgtool::find_tool("pg_dump", opts.pg_bin.as_deref())?;
    let have = pgtool::tool_version(&tool)?;
    eprintln!("pgman dump: {} — {}", resolved.label, resolved.describe());

    let opened = pgtool::open(&resolved).await?;
    let client = crate::conn::connect_via(opened.dsn.clone(), opened.via, 10_000).await?;
    let facts = crate::query::server::facts(&client).await?;
    drop(client);
    let server = Major::from_num(facts.version_num)
        .ok_or_else(|| format!("unreadable server_version_num {}", facts.version_num))?;
    if let Some(why) = pgtool::version_refusal("pg_dump", have, server) {
        return Err(why);
    }
    let mut notes = Vec::new();
    if resolved.role == crate::mcp::connections::Role::Replica && !facts.in_recovery {
        notes.push(format!(
            "{} is listed as a replica but isn't in recovery: the dump came from a primary",
            resolved.server
        ));
    }
    eprintln!(
        "pgman dump: PostgreSQL {}{}, as {}, with pg_dump {have}",
        facts.version,
        if facts.in_recovery { " (standby)" } else { "" },
        facts.user
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let out_is_dir = opts.out.as_deref().is_some_and(Path::is_dir);
    let label = match &resolved.environment {
        Some(env) => format!("{env}/{}", resolved.database),
        None => resolved.label.clone(),
    };
    let p = paths(
        &label,
        opts.format,
        opts.out.as_deref(),
        out_is_dir,
        now,
        std::process::id(),
    );
    if let Some(dir) = p.file.parent().filter(|d| !d.as_os_str().is_empty()) {
        if !dir.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
    }
    let conninfo = pgtool::conninfo(&opened.dsn, opened.via, "pgman-dump", have);
    let mut cmd = pgtool::private_command(&tool, &pg_dump_args(&conninfo, opts, &p.partial));
    pgtool::set_child_env(&mut cmd, opened.password());
    tracing::info!(target: "pgman::dump", source = %resolved.describe(), file = %p.file.display(), "pg_dump starting");
    let started = std::time::Instant::now();
    let result = pgtool::run_tool_cli(cmd, opts.verbose).await;
    drop(opened);
    let (status, tail) = match result {
        Ok(r) => r,
        Err(e) => {
            remove_partial(&p.partial);
            return Err(format!("pg_dump {e}; nothing written"));
        }
    };
    if !status.success() {
        remove_partial(&p.partial);
        tracing::warn!(target: "pgman::dump", status = %status, "pg_dump failed");
        return Err(format!(
            "{}\n  nothing written{}",
            explain_failure(&tail.text(), facts.in_recovery, Some(&facts.user)),
            if p.file.exists() {
                format!("; {} is as it was", p.file.display())
            } else {
                String::new()
            }
        ));
    }
    commit(&p.partial, &p.file).inspect_err(|_| remove_partial(&p.partial))?;
    let mut latest = None;
    if let Some(l) = &p.latest {
        match point_latest(l, &p.file) {
            Ok(()) => latest = Some(l.clone()),
            Err(e) => notes.push(e),
        }
    }
    tracing::info!(target: "pgman::dump", file = %p.file.display(), secs = started.elapsed().as_secs(), "pg_dump done");
    Ok(Done {
        bytes: pgtool::path_size(&p.file),
        file: p.file,
        latest,
        source: resolved.describe(),
        notes,
    })
}

/// What `pgman dump` prints on success. Pure.
pub fn render(done: &Done) -> String {
    let mut out = format!(
        "{} ({}) from {}",
        done.file.display(),
        pgtool::size_of(done.bytes),
        done.source
    );
    if let Some(l) = &done.latest {
        out.push_str(&format!("\n{} → it", l.display()));
    }
    for n in &done.notes {
        out.push_str(&format!("\nnote: {n}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn a_target_and_options_parse() {
        let o = parse_cli(&args(&[
            "staging/ledger",
            "out.dump",
            "--primary",
            "--format",
            "plain",
            "--",
            "--schema-only",
            "-t",
            "accounts",
        ]))
        .unwrap();
        assert_eq!(o.target, "staging/ledger");
        assert_eq!(o.out, Some(PathBuf::from("out.dump")));
        assert!(o.primary);
        assert_eq!(o.format, Format::Plain);
        assert_eq!(o.extra, args(&["--schema-only", "-t", "accounts"]));
        assert!(parse_cli(&[]).unwrap_err().contains("name a target"));
        assert!(parse_cli(&args(&["a", "b", "c"]))
            .unwrap_err()
            .contains("unexpected"));
        assert!(parse_cli(&args(&["a", "--schema-only"]))
            .unwrap_err()
            .contains("after --"));
        assert!(parse_cli(&args(&["a", "--format", "zip"]))
            .unwrap_err()
            .contains("custom"));
    }

    #[test]
    fn passthrough_cant_set_the_connection_or_the_output() {
        for bad in [
            &["--dbname=postgres://elsewhere/x"][..],
            &["--host", "elsewhere"],
            &["-h", "elsewhere"],
            &["-helsewhere"],
            &["-sh", "elsewhere"],
            &["--file=/tmp/x"],
            &["-f", "x"],
            &["-Fp"],
            &["-U", "postgres"],
            &["-W"],
            &["--port=1"],
            &["--db=postgres://elsewhere/x"],
            &["--ho", "elsewhere"],
            &["--fil=/tmp/x"],
            &["--user=postgres"],
        ] {
            assert!(refused_passthrough(&args(bad)).is_some(), "{bad:?}");
        }
        for ok in [
            &["--schema-only"][..],
            &["-t", "-h"],
            &["-thfoo"],
            &["--table", "--host"],
            &["--exclude-table-data=audit", "-j", "4", "-Z", "9"],
            &["-sx"],
            &["--role=reader"],
            &["--tab", "-h"],
            &["--filter=rules.txt"],
        ] {
            assert_eq!(refused_passthrough(&args(ok)), None, "{ok:?}");
        }
    }

    #[test]
    fn a_default_name_is_the_target_and_the_time_with_a_latest_link() {
        let p = paths(
            "staging/ledger",
            Format::Custom,
            None,
            false,
            1_793_196_202,
            7,
        );
        assert_eq!(
            p.file,
            PathBuf::from("staging-ledger-20261028T140322Z.dump")
        );
        assert_eq!(p.latest, Some(PathBuf::from("staging-ledger-latest.dump")));
        assert_eq!(
            p.partial,
            PathBuf::from(".staging-ledger-20261028T140322Z.dump.partial-7")
        );
        let p = paths(
            "staging/ledger",
            Format::Plain,
            Some(Path::new("/d")),
            true,
            1_793_196_202,
            7,
        );
        assert_eq!(
            p.file,
            PathBuf::from("/d/staging-ledger-20261028T140322Z.sql")
        );
        assert_eq!(
            p.latest,
            Some(PathBuf::from("/d/staging-ledger-latest.sql"))
        );
    }

    #[test]
    fn a_named_file_gets_no_link_and_a_partial_beside_it() {
        let p = paths(
            "staging/ledger",
            Format::Custom,
            Some(Path::new("/d/mine.dump")),
            false,
            0,
            7,
        );
        assert_eq!(p.file, PathBuf::from("/d/mine.dump"));
        assert_eq!(p.latest, None);
        assert_eq!(p.partial, PathBuf::from("/d/.mine.dump.partial-7"));
    }

    #[test]
    fn a_slug_keeps_names_and_drops_path_characters() {
        assert_eq!(slug("staging/ledger"), "staging-ledger");
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(slug("a b:c"), "a-b-c");
        assert_eq!(slug("///"), "dump");
    }

    #[test]
    fn a_recovery_conflict_is_named_and_suggests_retrying() {
        let tail = "pg_dump: error: Dumping the contents of table \"events\" failed: PQgetResult() failed.\n\
                    pg_dump: detail: Error message from server: ERROR:  canceling statement due to conflict with recovery\n\
                    DETAIL:  User query might have needed to see row versions that must be removed.";
        let e = explain_failure(tail, true, Some("app_ro"));
        assert!(e.starts_with("the replica cancelled the dump"), "{e}");
        assert!(e.contains("Try again") && e.contains("--primary"), "{e}");
        let e = explain_failure(tail, false, None);
        assert!(!e.contains("--primary"), "{e}");
    }

    #[test]
    fn a_deadlock_on_a_replica_is_a_recovery_conflict_too() {
        let tail = "pg_dump: error: query failed: ERROR:  deadlock detected\n\
                    DETAIL:  User transaction caused buffer deadlock with recovery.";
        assert!(explain_failure(tail, true, None).starts_with("the replica cancelled the dump"));
        let plain = "pg_dump: error: query failed: ERROR:  deadlock detected";
        assert!(explain_failure(plain, true, None).starts_with("the replica cancelled"));
        assert_eq!(
            explain_failure(plain, false, None),
            "pg_dump: error: query failed: ERROR:  deadlock detected",
            "on a primary a deadlock is just a deadlock"
        );
    }

    #[test]
    fn permission_denied_names_the_user_and_the_way_round() {
        let tail = "pg_dump: error: query failed: ERROR:  permission denied for table audit_log\n\
                    pg_dump: detail: Query was: LOCK TABLE public.audit_log IN ACCESS SHARE MODE";
        let e = explain_failure(tail, true, Some("app_ro"));
        assert!(
            e.starts_with(
                "pg_dump: error: query failed: ERROR:  permission denied for table audit_log"
            ),
            "{e}"
        );
        assert!(
            e.contains("(app_ro)") && e.contains("--exclude-table"),
            "{e}"
        );
    }

    #[test]
    fn anything_else_is_its_last_error_line() {
        assert_eq!(
            explain_failure(
                "pg_dump: warning: x\npg_dump: error: out of disk\n",
                false,
                None
            ),
            "pg_dump: error: out of disk"
        );
        assert_eq!(
            explain_failure("", false, None),
            "pg_dump failed with no message"
        );
        assert!(explain_failure(
            "FATAL:  password authentication failed for user \"u\"",
            false,
            None
        )
        .contains("was refused"));
    }

    #[test]
    fn the_command_line_has_no_password_and_writes_the_partial() {
        let o = parse_cli(&args(&["x", "--verbose", "--", "--schema-only"])).unwrap();
        let a = pg_dump_args("host='h' dbname='d'", &o, Path::new("/d/.x.partial-1"));
        assert_eq!(
            a,
            args(&[
                "--dbname=host='h' dbname='d'",
                "--format=c",
                "--file=/d/.x.partial-1",
                "--no-password",
                "--verbose",
                "--schema-only",
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn latest_is_repointed_and_a_real_file_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("pgman-dump-latest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, latest) = (
            dir.join("a.dump"),
            dir.join("b.dump"),
            dir.join("x-latest.dump"),
        );
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        point_latest(&latest, &a).unwrap();
        assert_eq!(std::fs::read_to_string(&latest).unwrap(), "a");
        point_latest(&latest, &b).unwrap();
        assert_eq!(std::fs::read_to_string(&latest).unwrap(), "b");
        assert_eq!(
            std::fs::read_link(&latest).unwrap(),
            PathBuf::from("b.dump")
        );
        std::fs::remove_file(&latest).unwrap();
        std::fs::write(&latest, "mine").unwrap();
        assert!(point_latest(&latest, &a)
            .unwrap_err()
            .contains("left alone"));
        assert_eq!(std::fs::read_to_string(&latest).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_dump_replaces_the_old_one_and_a_directory_too() {
        let dir = std::env::temp_dir().join(format!("pgman-dump-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (partial, file) = (dir.join(".f.partial"), dir.join("f.dump"));
        std::fs::write(&file, "old").unwrap();
        std::fs::write(&partial, "new").unwrap();
        commit(&partial, &file).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        assert!(!partial.exists());

        let (pd, fd) = (dir.join(".d.partial"), dir.join("d.dir"));
        std::fs::create_dir_all(&fd).unwrap();
        std::fs::write(fd.join("toc.dat"), "old").unwrap();
        std::fs::create_dir_all(&pd).unwrap();
        std::fs::write(pd.join("toc.dat"), "new").unwrap();
        commit(&pd, &fd).unwrap();
        assert_eq!(std::fs::read_to_string(fd.join("toc.dat")).unwrap(), "new");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "nothing left aside"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
