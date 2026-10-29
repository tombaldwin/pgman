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
usage: pgman dump <target> [FILE] [--masked | --unmasked | --schema-only]
                  [--masking FILE] [--reason TEXT] [--ticket KEY]
                  [--primary] [--format custom|plain|directory|tar]
                  [--skip-unreadable] [--pg-bin DIR] [--verbose] [-- PG_DUMP_OPTIONS...]

Dump a database with your own pg_dump, through pgman's connection: the
password read from its secret (or password_env) now, the ssh tunnel
opened for it. Nothing secret goes on a command line or to disk.

<target> is ENV, ENV/DATABASE or ENV/SERVER/DATABASE from
environments.toml, a connection name from connections.toml, or a
postgres:// URL. An environment's replica is used when it has one;
--primary takes the primary instead.

FILE defaults to a name after the target and the UTC time
(staging-ledger-20261028T140322Z.dump, or the connection's name) in
pgman's dump directory (~/.local/share/pgman/dumps), or in FILE when it
is a directory, and staging-ledger-latest.dump is pointed at the newest.
The dump is written to a temporary name and renamed into place when it
is done: a failed dump never replaces a file.

A protected target (production, or as environments.toml says) is dumped
masked: what looks personal is replaced as it is read, by built-in rules
and .pgman/masking/<database>.toml (pgman masking show lists them), into
a .pgman-dump directory that pgman restore takes. It asks why the copy
is taken, offering the ticket in your branch's name. A raw copy of one
needs dump = \"unmasked\" on it in environments.toml, --unmasked, and its
name typed at a terminal. Each dump goes in the journal: pgman copies
lists them.

  --masked       mask it (the default for a protected target)
  --unmasked     every row as it is
  --masking FILE the masking rules, in place of the project's
  --schema-only  the structure alone, no rows
  --reason TEXT  why, for the journal: a ticket id or a few words, no names
  --ticket KEY   the ticket it is for (APP-101); found in the branch's name
                 when not given
  --format       pg_dump's format (default custom, for pg_restore)
  --pg-bin DIR   where pg_dump is (default: $PGMAN_PG_BIN, then Homebrew's
                 libpq, then PATH)
  --verbose      show pg_dump's progress
  --skip-unreadable
                 leave out the tables, views and sequences the dump's user
                 can't read (named in the result), rather than refuse; by
                 default the dump is refused before pg_dump starts
  --             anything after it goes to pg_dump as it is
                 (--exclude-table-data=audit_log, -j 4 …);
                 the connection and output options are pgman's

  pgman dump production --ticket APP-101
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
    /// Leave out what the dump's user can't read, rather than refuse.
    pub skip_unreadable: bool,
    /// The structure alone.
    pub schema_only: bool,
    /// Why, as given with `--reason`.
    pub reason: Option<String>,
    /// The ticket, as given with `--ticket`.
    pub ticket: Option<String>,
    /// `--masked` / `--unmasked`, when asked for: else the policy's.
    pub masked: Option<bool>,
    /// `--masking FILE`: the rules, in place of the project's.
    pub masking: Option<PathBuf>,
}

/// Parse the arguments after `pgman dump`. Pure.
pub fn parse_cli(args: &[String]) -> Result<Opts, String> {
    let mut positional: Vec<String> = Vec::new();
    let mut primary = false;
    let mut format = Format::Custom;
    let mut pg_bin = None;
    let mut verbose = false;
    let mut skip_unreadable = false;
    let mut schema_only = false;
    let mut reason = None;
    let mut ticket = None;
    let mut masked = None;
    let mut masking = None;
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
            "--skip-unreadable" => skip_unreadable = true,
            "--schema-only" => schema_only = true,
            "--reason" => reason = Some(value("--reason")?),
            "--ticket" => ticket = Some(value("--ticket")?),
            "--masked" => masked = Some(true),
            "--unmasked" => masked = Some(false),
            "--masking" => masking = Some(PathBuf::from(value("--masking")?)),
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
        skip_unreadable,
        schema_only,
        reason,
        ticket,
        masked,
        masking,
    })
}

/// pg_dump options that bring rows (or large objects) into a dump, by
/// long name; and their short letters.
const DATA_LONG: &[&str] = &[
    "data-only",
    "section",
    "large-objects",
    "blobs",
    "include-foreign-data",
];
const DATA_SHORT: &str = "ab";

/// Whether `extra` asks pg_dump for rows: `-a`, `--data-only`,
/// `--section`, large objects — or a prefix of one, as getopt reads it.
/// Pure.
pub fn brings_data(extra: &[String]) -> bool {
    passthrough_refusal(
        extra,
        SHORT_WITH_VALUE,
        DATA_SHORT,
        DATA_LONG,
        LONG_WITH_VALUE,
    )
    .is_some()
}

/// What `opts` dumps: the schema alone (`--schema-only`, or pg_dump's
/// own `--schema-only` / `-s` after `--`, with nothing there asking for
/// rows), masked (`--masked`), raw (`--unmasked`), else `default` — the
/// policy's ([`crate::policy::default_kind`]). Pure.
pub fn kind(opts: &Opts, default: crate::policy::Kind) -> crate::policy::Kind {
    use crate::policy::Kind;
    let asked = opts.schema_only || opts.extra.iter().any(|a| a == "--schema-only" || a == "-s");
    if asked {
        return if brings_data(&opts.extra) {
            Kind::Raw
        } else {
            Kind::Schema
        };
    }
    match opts.masked {
        Some(true) => Kind::Masked,
        Some(false) => Kind::Raw,
        None => default,
    }
}

/// pg_dump's `--exclude-table` for each of `unreadable`: schema and name
/// each double-quoted, so neither case-folds and nothing in them (`*`,
/// `?`, `.`) is read as a pattern. Pure.
pub fn exclude_args(unreadable: &[crate::query::server::Unreadable]) -> Vec<String> {
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    unreadable
        .iter()
        .map(|u| format!("--exclude-table={}.{}", q(&u.schema), q(&u.name)))
        .collect()
}

/// `106 partitions, 2 sequences; no SELECT`: kinds by count, then why.
/// Pure.
fn kinds(unreadable: &[crate::query::server::Unreadable]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    let mut reasons: Vec<&str> = Vec::new();
    for u in unreadable {
        match counts.iter_mut().find(|(k, _)| *k == u.kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((&u.kind, 1)),
        }
        if !reasons.contains(&u.reason.as_str()) {
            reasons.push(&u.reason);
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    let kinds = counts
        .iter()
        .map(|(k, n)| format!("{n} {k}{}", if *n == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{kinds}; {}", reasons.join(", "))
}

/// The first few of `unreadable`, by name, and how many more. Pure.
fn some_names(unreadable: &[crate::query::server::Unreadable]) -> String {
    let shown: Vec<String> = unreadable
        .iter()
        .take(8)
        .map(|u| format!("{}.{}", u.schema, u.name))
        .collect();
    let more = unreadable.len().saturating_sub(shown.len());
    if more > 0 {
        format!("{} and {more} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// What the pre-flight decided: what to leave out, and what to say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preflight {
    /// `--exclude-table`s, and `--no-blobs` for unreadable large objects.
    pub args: Vec<String>,
    /// For the result: what was left out.
    pub notes: Vec<String>,
    /// For progress: what wasn't checked because the options after `--`
    /// choose what the dump covers.
    pub warning: Option<String>,
}

/// Decide what a dump as `user` does about `unreadable` and
/// `large_objects` (unreadable ones): refuse before pg_dump starts,
/// leave them out (`--skip-unreadable`), or — with options after `--`,
/// which may leave them out already — only warn. A sequence a readable
/// table owns can't be left out at all, so it is refused even with
/// `--skip-unreadable`. Pure.
pub fn preflight(
    user: &str,
    unreadable: &[crate::query::server::Unreadable],
    large_objects: i64,
    opts: &Opts,
) -> Result<Preflight, String> {
    let (skippable, blocking): (Vec<_>, Vec<_>) =
        unreadable.iter().cloned().partition(|u| u.skippable());
    if !opts.extra.is_empty() {
        let n = unreadable.len() + usize::from(large_objects > 0);
        return Ok(Preflight {
            warning: (n > 0).then(|| {
                format!(
                    "note: {user} can't read {} object(s) here{}; if the dump covers one, it stops there",
                    unreadable.len(),
                    if large_objects > 0 {
                        format!(" and {large_objects} large object(s)")
                    } else {
                        String::new()
                    }
                )
            }),
            ..Preflight::default()
        });
    }
    if !blocking.is_empty() {
        let names: Vec<String> = blocking
            .iter()
            .take(6)
            .map(|u| {
                let table = u.owned_by.as_ref().map(|(t, _)| t.as_str()).unwrap_or("?");
                format!("{}.{} (of {table})", u.schema, u.name)
            })
            .collect();
        let more = blocking.len().saturating_sub(names.len());
        return Err(format!(
            "{user} can read these tables but not their sequences: {}{}.\n  pg_dump reads a \
             table's own sequences (serial and identity columns) along with it, so they can't be \
             left out on their own. GRANT SELECT ON SEQUENCE … TO {user} (USAGE isn't enough), or \
             use a role with pg_read_all_data; or leave those tables out: -- --exclude-table=…",
            names.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ));
    }
    if skippable.is_empty() && large_objects == 0 {
        return Ok(Preflight::default());
    }
    let lo = if large_objects > 0 {
        format!(", and {large_objects} large object(s)")
    } else {
        String::new()
    };
    if !opts.skip_unreadable {
        let mut owners: Vec<&str> = skippable.iter().map(|u| u.owner.as_str()).collect();
        owners.sort_unstable();
        owners.dedup();
        let what = if skippable.is_empty() {
            "nothing else".to_string()
        } else {
            format!(
                "{} object(s) ({}; owned by {}): {}",
                skippable.len(),
                kinds(&skippable),
                owners.join(", "),
                some_names(&skippable)
            )
        };
        let rls = if skippable.iter().any(|u| u.reason == "row-level security") {
            " For tables under row-level security, -- --enable-row-security dumps the rows it can see."
        } else {
            ""
        };
        return Err(format!(
            "{user} can't read what the dump would cover: {what}{lo}.\n  pg_dump would stop at \
             the first. --skip-unreadable leaves exactly these out{} and says so; or dump as a \
             user that can read them (the owner, or one granted pg_read_all_data).{rls}",
            if large_objects > 0 {
                " (large objects all together, as pg_dump -B does)"
            } else {
                ""
            }
        ));
    }
    let mut args = exclude_args(&skippable);
    let mut notes = Vec::new();
    if !skippable.is_empty() {
        notes.push(format!(
            "left out {} object(s) {user} can't read ({}): {}",
            skippable.len(),
            kinds(&skippable),
            some_names(&skippable)
        ));
    }
    if large_objects > 0 {
        args.push("-B".to_string());
        notes.push(format!(
            "left out the large objects: {user} can't read {large_objects} of them, and pg_dump \
             can't leave out some"
        ));
    }
    Ok(Preflight {
        args,
        notes,
        warning: None,
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
    if lower.contains("permission denied for large object") {
        return format!(
            "{last}\n  Large objects can't be left out one by one: -- -B leaves out all of them, \
             or dump as a user that can read them"
        );
    }
    if lower.contains("row-level security") {
        return format!(
            "{last}\n  pg_dump reads every row or none: dump as the table's owner or a role with \
             BYPASSRLS, or -- --enable-row-security to dump the rows this user can see"
        );
    }
    // The server's wording ("permission denied for table …"), not a
    // local file's ("could not open output file …: Permission denied").
    if lower.contains("permission denied for") {
        let who = user.map(|u| format!(" ({u})")).unwrap_or_default();
        return format!(
            "{last}\n  The dump's user{who} can't read everything the dump covers. --skip-unreadable \
             leaves out what it can't read; or dump as a user that can (the owner, or one granted \
             pg_read_all_data), or leave objects out yourself: -- --exclude-table=…"
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

/// Make the partial pg_dump writes into, before it runs: for a file
/// format the file itself (returned, for pg_dump's stdout), for the
/// directory format the directory — both owner-only, and both created
/// exclusively. Whatever is already at that name — a symlink, a hard
/// link to someone's file, a directory symlinked elsewhere — is refused
/// rather than written through: the name is predictable (the target's
/// and the pid), so something could be waiting there. Reads the
/// filesystem.
pub fn create_partial(partial: &Path, format: Format) -> Result<Option<std::fs::File>, String> {
    let refuse = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "{} already exists; pgman won't write a dump through something it didn't make. Remove it and run again",
                partial.display()
            )
        } else {
            format!("could not create {}: {e}", partial.display())
        }
    };
    if format == Format::Directory {
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(partial).map_err(refuse)?;
        return Ok(None);
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(partial).map(Some).map_err(refuse)
}

/// What `path` is, not followed if a link: its device and inode, so a
/// swap shows. `None` when it can't be read.
pub(crate) fn identity(path: &Path) -> Option<(u64, u64, bool)> {
    let m = std::fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((m.dev(), m.ino(), m.file_type().is_symlink()))
    }
    #[cfg(not(unix))]
    {
        Some((0, 0, m.file_type().is_symlink()))
    }
}

/// Remove what a failed dump left: a file, or a directory format's
/// directory.
pub(crate) fn remove_partial(p: &Path) {
    let _ = if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    };
}

/// Move the finished dump into place: a file is renamed over whatever
/// was there; a directory replaces an old one only once it is aside.
pub(crate) fn commit(partial: &Path, file: &Path) -> Result<(), String> {
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

/// Why the dump can't be written where `p` says, before anything runs:
/// FILE names a directory that isn't there (`dumps/`), a directory to
/// put the dump in doesn't exist, or the directory format would replace
/// a file (or a file format a directory). Reads the filesystem.
pub fn output_problem(opts: &Opts, p: &Paths) -> Option<String> {
    if let Some(out) = &opts.out {
        let text = out.to_string_lossy();
        if (text.ends_with('/') || text.ends_with(std::path::MAIN_SEPARATOR)) && !out.is_dir() {
            return Some(format!(
                "{} doesn't exist; create it, or name a file in a directory that does",
                out.display()
            ));
        }
    }
    if let Some(dir) = p.file.parent().filter(|d| !d.as_os_str().is_empty()) {
        if !dir.is_dir() {
            return Some(if dir.exists() {
                format!("{} is not a directory", dir.display())
            } else {
                format!(
                    "{} doesn't exist; create it, or name a file in a directory that does",
                    dir.display()
                )
            });
        }
    }
    let existing = std::fs::symlink_metadata(&p.file).ok();
    match (opts.format, existing) {
        (Format::Directory, Some(m)) if !m.is_dir() => Some(format!(
            "{} is a file; a directory-format dump would replace it with a directory. Name another",
            p.file.display()
        )),
        (f, Some(m)) if f != Format::Directory && m.is_dir() => {
            Some(format!("{} is a directory", p.file.display()))
        }
        _ => None,
    }
}

/// The pg_dump command line for `conninfo`, writing `partial`. The
/// password is not an argument: [`pgtool::set_child_env`] gives it to
/// the process alone. Pure.
pub fn pg_dump_args(conninfo: &str, opts: &Opts, partial: &Path) -> Vec<String> {
    let mut args = vec![
        format!("--dbname={conninfo}"),
        format!("--format={}", opts.format.letter()),
    ];
    // A file format goes to stdout, into the file [`create_partial`]
    // made: pg_dump never opens a path, so nothing planted at one is
    // followed. A directory has to be named; pgman made that too.
    // The directory format runs inside the directory pgman made
    // (`current_dir`), told `.`: entered as pg_dump starts, not looked up
    // by name after it has connected.
    let _ = partial;
    if opts.format == Format::Directory {
        args.push("--file=.".to_string());
    }
    args.push("--no-password".to_string());
    if opts.schema_only {
        args.push("--schema-only".to_string());
    }
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
    /// FILE was there before, and this dump replaced it.
    pub replaced: bool,
    /// Its id in the journal, once recorded.
    pub copy: Option<String>,
}

/// What a dump is, for the journal: settled before it runs.
#[derive(Debug, Clone)]
pub struct Entry {
    pub copy: String,
    pub kind: crate::policy::Kind,
    pub protected: bool,
    pub reason: Option<String>,
    pub ticket: Option<String>,
    /// `cli`, `unattended` or `mcp`.
    pub via: &'static str,
    pub ttl_secs: u64,
}

/// What a dump's reason is settled from.
#[derive(Debug, Clone, Copy)]
pub struct ReasonQuestion<'a> {
    /// `--reason`.
    pub reason: Option<&'a str>,
    /// `--ticket`.
    pub ticket: Option<&'a str>,
    /// The policy asks for one.
    pub needed: bool,
    /// The ticket in the branch's name.
    pub branch_ticket: Option<&'a str>,
    /// What the prompt offers: [`crate::copies::reason_default`].
    pub offer: Option<&'a str>,
    /// The project's ticket keys.
    pub keys: &'a [String],
    /// The target, for messages.
    pub name: &'a str,
}

/// Asks the operator for a reason, offering a default.
pub type Ask<'a> = &'a dyn Fn(Option<&str>) -> Result<String, String>;

/// The reason and ticket a dump records, from `--reason` / `--ticket`,
/// else (when one is needed and there is a terminal) `ask`ed with
/// `offer` as the default, else the branch's ticket alone. Pure but for
/// `ask`.
pub fn settle_reason(
    q: &ReasonQuestion,
    ask: Option<Ask>,
) -> Result<(Option<String>, Option<String>), String> {
    let ReasonQuestion {
        reason: given_reason,
        ticket: given_ticket,
        needed,
        branch_ticket,
        offer,
        keys,
        name,
    } = *q;
    let ticket = match given_ticket {
        Some(t) => Some(
            crate::copies::ticket_in(t, &[])
                .filter(|k| k.eq_ignore_ascii_case(t.trim()))
                .ok_or_else(|| format!("--ticket {t:?} isn't a ticket key: APP-101"))?,
        ),
        None => None,
    };
    if let Some(r) = given_reason {
        let reason = crate::journal::clean_reason(r)?;
        let ticket = ticket.or_else(|| {
            reason
                .as_deref()
                .and_then(|r| crate::copies::ticket_in(r, keys))
        });
        if needed && reason.is_none() && ticket.is_none() {
            return Err(format!(
                "a dump of {name} takes a reason: --reason is empty"
            ));
        }
        return Ok((reason, ticket));
    }
    if ticket.is_some() || !needed {
        return Ok((None, ticket.or_else(|| branch_ticket.map(str::to_string))));
    }
    let Some(ask) = ask else {
        return Err(format!(
            "a dump of {name} takes a reason, and there is no terminal to ask on: pass --reason \"…\" or --ticket KEY"
        ));
    };
    let answer = ask(offer)?;
    let (reason, ticket) = crate::copies::read_reason(&answer, offer, keys)?;
    if reason.is_none() {
        return Err(format!("a dump of {name} takes a reason; nothing dumped"));
    }
    Ok((reason, ticket))
}

/// The warning before a raw dump of a protected target. Pure.
pub fn raw_warning(name: &str, why: &str) -> String {
    format!(
        "{name} is protected ({why}). This is a raw copy: every row, personal data included, \
         written to this machine unencrypted. pgman copies rm deletes it when you're done."
    )
}

/// Ask a line on the terminal, offering `offer`.
fn ask_reason_cli(offer: Option<&str>) -> Result<String, String> {
    let prompt = match offer {
        Some(o) => format!("why is this copy taken? ticket id or a few words, no names [{o}]: "),
        None => "why is this copy taken? ticket id or a few words, no names: ".to_string(),
    };
    crate::pgtool::ask_cli(&prompt)
}

/// Record `result` of the dump `entry` describes in the journal. What
/// to add to the result's notes, if the journal couldn't be written.
pub fn journal_dump(
    entry: &Entry,
    env: Option<&str>,
    database: &str,
    label: &str,
    result: &Result<Done, String>,
) -> Option<String> {
    use crate::journal::{Event, Kind};
    let now = crate::journal::now();
    let mut e = Event::new(Kind::Dump, now, &crate::journal::machine_id());
    e.copy = Some(entry.copy.clone());
    e.env = env.map(str::to_string);
    e.database = Some(database.to_string());
    e.label = Some(label.to_string());
    e.protected = entry.protected;
    e.mode = Some(entry.kind.as_str().to_string());
    e.reason = entry.reason.clone();
    e.ticket = entry.ticket.clone();
    e.via = Some(entry.via.to_string());
    match result {
        Ok(done) => {
            let file = done
                .file
                .canonicalize()
                .unwrap_or_else(|_| done.file.clone());
            e.file = Some(file.display().to_string());
            e.latest = done.latest.as_ref().map(|l| {
                let l = std::path::absolute(l).unwrap_or_else(|_| l.clone());
                l.display().to_string()
            });
            e.bytes = Some(done.bytes);
            e.print = crate::copies::fingerprint(&file);
            e.expires = Some(now.saturating_add(entry.ttl_secs));
        }
        Err(_) => e.ok = Some(false),
    }
    crate::journal::append(&e)
}

/// pgman's dump directory, where a dump goes when FILE isn't given:
/// readable by its owner alone, out of synced folders.
pub fn default_dir() -> PathBuf {
    crate::util::data_dir().join("dumps")
}

/// The sync service `path` is inside, if it is one pgman knows: a copy
/// there leaves the machine on its own. Pure.
pub fn synced(path: &Path) -> Option<&'static str> {
    // With a slash after it, so the folder itself matches, not only
    // what is in it.
    let p = format!("{}/", path.to_string_lossy());
    [
        ("/Library/Mobile Documents/", "iCloud Drive"),
        ("/iCloud Drive/", "iCloud Drive"),
        ("/Library/CloudStorage/Dropbox", "Dropbox"),
        ("/Library/CloudStorage/OneDrive", "OneDrive"),
        ("/Library/CloudStorage/GoogleDrive", "Google Drive"),
        ("/Library/CloudStorage/", "a cloud storage folder"),
        ("/Dropbox/", "Dropbox"),
        ("/OneDrive", "OneDrive"),
        ("/Google Drive/", "Google Drive"),
        ("/Nextcloud/", "Nextcloud"),
    ]
    .iter()
    .find(|(needle, _)| p.contains(needle))
    .map(|(_, name)| *name)
}

/// Whether `dir` is in `home`'s Desktop or Documents while iCloud syncs
/// them ("Desktop & Documents Folders"): they stay where they are, and
/// iCloud Drive holds them too. Reads the filesystem.
pub fn icloud_desktop(dir: &Path, home: &Path) -> bool {
    let cloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
    ["Desktop", "Documents"]
        .iter()
        .any(|f| dir.starts_with(home.join(f)) && cloud.join(f).is_dir())
}

/// Where a dump of `kind` from a target (`protected` or not) may be
/// written: a raw copy of a protected target never into a synced folder;
/// anything else there with a warning. Reads the filesystem (for
/// iCloud's Desktop & Documents in `home`).
pub fn place_problem(
    dir: &Path,
    kind: crate::policy::Kind,
    protected: bool,
    home: Option<&Path>,
) -> Result<Option<String>, String> {
    let service = synced(dir).or_else(|| {
        home.filter(|h| icloud_desktop(dir, h))
            .map(|_| "iCloud Drive (Desktop & Documents)")
    });
    let Some(service) = service else {
        return Ok(None);
    };
    if protected && kind == crate::policy::Kind::Raw {
        return Err(format!(
            "{} is in {service}, which copies it off this machine: a raw copy of a protected \
             target isn't written there. Name a folder outside it, or leave out FILE for pgman's \
             dump directory",
            dir.display()
        ));
    }
    Ok(Some(format!(
        "{} is in {service}: the dump is copied off this machine",
        dir.display()
    )))
}

/// The extended attribute `tmutil addexclusion` sets (a binary plist of
/// the string `com.apple.backupd`), in hex.
#[cfg(target_os = "macos")]
const BACKUP_EXCLUDE: (&str, &str) = (
    "com.apple.metadata:com_apple_backup_excludeItem",
    "62706C69737430305F1011636F6D2E6170706C652E6261636B757064080000000000000101\
     000000000000000100000000000000000000000000001C",
);

/// Keep `path` out of Time Machine's backups, where a deleted dump would
/// live on (macOS; elsewhere nothing). Sets the attribute `tmutil
/// addexclusion` would: tmutil itself takes some ten seconds on a
/// directory. Runs xattr, or tmutil in the background when that fails.
pub fn exclude_from_backups(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};
        let set = Command::new("/usr/bin/xattr")
            .args(["-wx", BACKUP_EXCLUDE.0, BACKUP_EXCLUDE.1])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !set {
            let _ = Command::new("tmutil")
                .arg("addexclusion")
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = path;
}

/// A note when the disk isn't encrypted (macOS FileVault off). Runs
/// fdesetup.
pub fn disk_note() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("fdesetup")
            .arg("status")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.contains("FileVault is Off").then(|| {
            "FileVault is off: this disk isn't encrypted, and a lost laptop is a lost copy"
                .to_string()
        })
    }
    #[cfg(not(target_os = "macos"))]
    None
}

/// Copies past their expiry still on disk, for a note after a dump.
/// Reads the journal.
pub fn expired_note(except: &str) -> Option<String> {
    let (events, _) = crate::journal::read_in(&crate::journal::dir());
    let now = crate::journal::now();
    let n = crate::copies::registry(&events)
        .iter()
        .filter(|c| {
            c.id != except
                && c.deleted.is_none()
                && c.expired(now)
                && crate::copies::present(&c.file)
        })
        .count();
    (n > 0).then(|| {
        format!(
            "{n} older cop{} past {} expiry: pgman copies lists {}",
            if n == 1 { "y is" } else { "ies are" },
            if n == 1 { "its" } else { "their" },
            if n == 1 { "it" } else { "them" }
        )
    })
}

/// Mark `resolved` protected when it is a database pgman restored from a
/// raw dump of a protected environment — by the journal, or by the
/// database's own comment — so a copy of it is treated as a copy of the
/// original: masked by default, raw only as the original allows. Reads
/// the journal; connects when the journal doesn't say.
pub async fn inherit_raw_protection(resolved: &mut crate::inventory::target::Resolved) {
    if resolved.protected {
        return;
    }
    let server = crate::conn::host_port(&resolved.connect.dsn.host, resolved.connect.dsn.port);
    let (events, _) = crate::journal::read_in(&crate::journal::dir());
    let copies = crate::copies::registry(&events);
    let by_journal = crate::copies::raw_protected_at(&copies, &server, &resolved.database)
        .map(|c| format!("it is a raw copy of {}, restored by pgman", c.label));
    let why = match by_journal {
        Some(why) => Some(why),
        None => raw_copy_comment(resolved)
            .await
            .map(|_| "its comment says it is a raw copy of a protected environment".into()),
    };
    if let Some(why) = why {
        resolved.protected = true;
        resolved.protected_why = Some(why);
    }
}

/// The database's comment, when it says a raw protected copy.
async fn raw_copy_comment(resolved: &crate::inventory::target::Resolved) -> Option<String> {
    let opened = pgtool::open(resolved).await.ok()?;
    let client = crate::conn::connect_via(opened.dsn.clone(), opened.via, 10_000)
        .await
        .ok()?;
    crate::query::masking::comment(&client, &resolved.database)
        .await
        .filter(|c| crate::mcp::tools::raw_protected_comment(c))
}

/// Run a dump at the command line: the policy's gate (the typed name,
/// the reason) asked on the terminal, the dump, the journal. Progress
/// goes to stderr; the result is what to print.
pub async fn run_cli(opts: &Opts) -> Result<Done, String> {
    use std::io::IsTerminal;
    let mut resolved = resolve(opts).await?;
    inherit_raw_protection(&mut resolved).await;
    let cwd = std::env::current_dir().map_err(|e| format!("the current directory: {e}"))?;
    let (policy, dumps, branch, copies) = {
        let resolved = resolved.clone();
        let cwd = cwd.clone();
        tokio::task::spawn_blocking(move || -> Result<_, String> {
            let policy = crate::policy::for_target(&resolved, &cwd)?;
            let dumps = crate::project::load_dumps(&cwd)?
                .map(|(_, d)| d)
                .unwrap_or_default();
            let branch = crate::copies::git_branch(&cwd);
            let (events, _) = crate::journal::read_in(&crate::journal::dir());
            Ok((policy, dumps, branch, crate::copies::registry(&events)))
        })
        .await
        .map_err(|e| format!("policy task failed: {e}"))??
    };
    let tty = std::io::stdin().is_terminal();
    // With no FILE, pgman's own directory: private, out of synced folders.
    let mut opts = opts.clone();
    if opts.out.is_none() {
        let dir = default_dir();
        crate::util::ensure_private_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        opts.out = Some(dir);
    }
    let opts = &opts;
    let via = if tty {
        crate::policy::Via::Terminal
    } else {
        crate::policy::Via::NoTerminal
    };
    let kind = kind(opts, crate::policy::default_kind(&policy));
    let name = resolved
        .environment
        .clone()
        .unwrap_or_else(|| resolved.label.clone());
    let gate = crate::policy::gate(&policy, kind, via, &name)?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let place_note = place_problem(
        &place_dir(opts.out.as_deref(), &cwd),
        kind,
        policy.protected,
        home.as_deref(),
    )?;
    if gate.typed.is_some() || gate.unattended {
        eprintln!("pgman dump: {}", raw_warning(&name, &resolved.protection()));
    }
    let keys = dumps.ticket_keys.clone();
    let branch_ticket = branch
        .as_deref()
        .and_then(|b| crate::copies::ticket_in(b, &keys));
    let last = crate::copies::last_reason(&copies, &resolved.label).map(str::to_string);
    let offer = crate::copies::reason_default(branch_ticket.as_deref(), last.as_deref());
    let ask: Ask = &ask_reason_cli;
    let (reason, ticket) = settle_reason(
        &ReasonQuestion {
            reason: opts.reason.as_deref(),
            ticket: opts.ticket.as_deref(),
            needed: gate.reason,
            branch_ticket: branch_ticket.as_deref(),
            offer: offer.as_deref(),
            keys: &keys,
            name: &name,
        },
        tty.then_some(ask),
    )?;
    if let Some(typed) = &gate.typed {
        let prompt = format!("type {typed} to dump it: ");
        let typed2 = typed.clone();
        let ok = tokio::task::spawn_blocking(move || crate::pgtool::confirm_cli(&prompt, &typed2))
            .await
            .map_err(|e| format!("prompt task failed: {e}"))??;
        if !ok {
            return Err(format!("the name typed isn't {typed}; nothing dumped"));
        }
    }
    let entry = Entry {
        copy: crate::journal::new_id(),
        kind,
        protected: policy.protected,
        reason,
        ticket,
        via: if gate.unattended { "unattended" } else { "cli" },
        ttl_secs: policy.ttl_secs,
    };
    let env = resolved.environment.clone();
    let database = resolved.database.clone();
    let label = resolved.label.clone();
    let say = |line: String| eprintln!("pgman dump: {line}");
    let mut result = if kind == crate::policy::Kind::Masked {
        crate::masked::run(
            opts,
            resolved,
            &entry.copy,
            &cwd,
            false,
            policy.unattended,
            &say,
        )
        .await
    } else {
        run(opts, resolved, &say).await
    };
    let warn = journal_dump(&entry, env.as_deref(), &database, &label, &result);
    let unsent = crate::journal_send::flush_note().await;
    if let Ok(done) = &mut result {
        placed(done, place_note, policy.protected);
        done.copy = Some(entry.copy.clone());
        done.notes.extend(warn);
        done.notes.extend(unsent);
        done.notes.extend(expired_note(&entry.copy));
    }
    result
}

/// The directory a dump to `out` lands in, for [`place_problem`]: `out`
/// itself when it is one, else its parent; `cwd` with none. Reads the
/// filesystem.
pub fn place_dir(out: Option<&Path>, cwd: &Path) -> PathBuf {
    let dir = match out {
        Some(o)
            if o.is_dir()
                && !o
                    .to_string_lossy()
                    .ends_with(crate::masking::archive::EXTENSION) =>
        {
            o.to_path_buf()
        }
        Some(o) => o.parent().map(Path::to_path_buf).unwrap_or_default(),
        None => cwd.to_path_buf(),
    };
    dir.canonicalize().unwrap_or(dir)
}

/// After a dump, wherever it was asked from: kept out of Time Machine,
/// and the notes on where it is (`place_note`, from [`place_problem`])
/// and, for a protected target, the disk's encryption.
pub fn placed(done: &mut Done, place_note: Option<String>, protected: bool) {
    exclude_from_backups(&done.file);
    done.notes.extend(place_note);
    if protected {
        done.notes.extend(disk_note());
    }
}

/// The database `opts` names, for a dump: its replica unless
/// `--primary`. Reads no secret.
pub async fn resolve(opts: &Opts) -> Result<target::Resolved, String> {
    let want = if opts.primary {
        Want::Primary
    } else {
        Want::PreferReplica
    };
    let text = opts.target.clone();
    tokio::task::spawn_blocking(move || target::resolve(&text, want))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))?
}

/// Dump `resolved` as `opts` says, telling `say` what it is doing.
pub async fn run(
    opts: &Opts,
    resolved: target::Resolved,
    say: &(dyn Fn(String) + Send + Sync),
) -> Result<Done, String> {
    let (tool, found_in) = pgtool::find_tool_from("pg_dump", opts.pg_bin.as_deref())?;
    let have = pgtool::tool_version(&tool)?;
    say(format!("{} — {}", resolved.label, resolved.describe()));

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
    if let Some(why) = output_problem(opts, &p) {
        return Err(why);
    }
    let opened = pgtool::open(&resolved).await?;
    let client = crate::conn::connect_via(opened.dsn.clone(), opened.via, 10_000)
        .await
        .map_err(|e| pgtool::explain_connect(e, &resolved))?;
    let facts = crate::query::server::facts(&client).await?;
    let foreign = opts
        .extra
        .iter()
        .any(|a| a.starts_with("--include-foreign-data"));
    let unreadable = crate::query::server::unreadable(&client, foreign).await?;
    let large_objects = crate::query::server::unreadable_large_objects(&client).await?;
    drop(client);
    let pre = preflight(&facts.user, &unreadable, large_objects, opts)?;
    if let Some(w) = &pre.warning {
        say(w.clone());
    }
    let excludes = pre.args;
    let skipped = pre.notes;
    let server = Major::from_num(facts.version_num)
        .ok_or_else(|| format!("unreadable server_version_num {}", facts.version_num))?;
    if let Some(why) = pgtool::version_refusal("pg_dump", have, server) {
        return Err(why);
    }
    let mut notes: Vec<String> = skipped;
    if resolved.role == crate::mcp::connections::Role::Replica && !facts.in_recovery {
        notes.push(format!(
            "{} is listed as a replica but isn't in recovery: the dump came from a primary",
            resolved.server
        ));
    }
    say(format!(
        "PostgreSQL {}{}, as {}, with pg_dump {have} ({found_in}: {})",
        facts.version,
        if facts.in_recovery { " (standby)" } else { "" },
        facts.user,
        tool.display()
    ));

    let conninfo = pgtool::conninfo(&opened.dsn, opened.via, "pgman-dump", have);
    let mut args = pg_dump_args(&conninfo, opts, &p.partial);
    args.extend(excludes);
    let out_file = create_partial(&p.partial, opts.format)?;
    let made = identity(&p.partial);
    let mut cmd = pgtool::private_command(&tool, &args);
    // Never pgman's own stdout, which under MCP is the protocol.
    match out_file {
        Some(f) => cmd.stdout(f),
        None => cmd
            .current_dir(&p.partial)
            .stdout(std::process::Stdio::null()),
    };
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
    let replaced = std::fs::symlink_metadata(&p.file).is_ok();
    // A finished dump is kept whatever went wrong moving it: it may
    // have taken an hour.
    // What is moved into place is what pgman made: a directory swapped for
    // another (or a link) while pg_dump ran isn't.
    if identity(&p.partial) != made {
        return Err(format!(
            "{} was replaced while pg_dump ran; the dump isn't moved into place. Check what is \
             there before using it",
            p.partial.display()
        ));
    }
    commit(&p.partial, &p.file).map_err(|e| {
        format!(
            "{e}\n  the finished dump is kept at {}",
            p.partial.display()
        )
    })?;
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
        replaced,
        copy: None,
    })
}

/// What `pgman dump` prints on success. Pure.
pub fn render(done: &Done) -> String {
    let mut out = format!(
        "{} ({}) from {}{}",
        done.file.display(),
        pgtool::size_of(done.bytes),
        done.source,
        if done.replaced {
            ", replacing the file that was there"
        } else {
            ""
        }
    );
    if let Some(c) = &done.copy {
        out.push_str(&format!(
            "\ncopy {} in the journal: pgman copies",
            &c[..8.min(c.len())]
        ));
    }
    if let Some(l) = &done.latest {
        let name = done
            .file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push_str(&format!("\n{} now points at {name}", l.display()));
    }
    for n in &done.notes {
        out.push_str(&format!("\nnote: {n}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dump is out of Time Machine at once, not after tmutil's ten
    /// seconds on a directory: the attribute tmutil sets is there. (Not
    /// `tmutil isexcluded`: it says a temporary directory is excluded
    /// whatever its attributes.)
    #[cfg(target_os = "macos")]
    #[test]
    fn a_dump_is_excluded_from_time_machine_quickly() {
        let dir = std::env::temp_dir().join(format!("pgman-dump-tm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("x.pgman-dump")).unwrap();
        let started = std::time::Instant::now();
        exclude_from_backups(&dir.join("x.pgman-dump"));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        let has = std::process::Command::new("/usr/bin/xattr")
            .args(["-px", BACKUP_EXCLUDE.0])
            .arg(dir.join("x.pgman-dump"))
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let hex: String = String::from_utf8_lossy(&has.stdout)
            .split_whitespace()
            .collect();
        assert_eq!(hex, BACKUP_EXCLUDE.1);
    }

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
        assert!(parse_cli(&args(&["a", "--no-owner"]))
            .unwrap_err()
            .contains("after --"));
        assert!(parse_cli(&args(&["a", "--format", "zip"]))
            .unwrap_err()
            .contains("custom"));
        let o = parse_cli(&args(&[
            "production",
            "--schema-only",
            "--reason",
            "APP-1 repro",
            "--ticket",
            "APP-1",
        ]))
        .unwrap();
        assert!(o.schema_only);
        assert_eq!(o.reason.as_deref(), Some("APP-1 repro"));
        assert_eq!(o.ticket.as_deref(), Some("APP-1"));
        assert!(parse_cli(&args(&["a", "--reason"]))
            .unwrap_err()
            .contains("needs a value"));
    }

    fn opts(schema_only: bool, extra: &[&str]) -> Opts {
        let mut o = parse_cli(&args(&["t"])).unwrap();
        o.schema_only = schema_only;
        o.extra = args(extra);
        o
    }

    fn kind_of(o: &Opts) -> crate::policy::Kind {
        kind(o, crate::policy::Kind::Raw)
    }

    #[test]
    fn a_dump_is_masked_raw_or_schema_as_asked_else_as_the_policy_says() {
        use crate::policy::Kind;
        let mut o = opts(false, &[]);
        assert_eq!(kind(&o, Kind::Masked), Kind::Masked);
        assert_eq!(kind(&o, Kind::Schema), Kind::Schema);
        o.masked = Some(false);
        assert_eq!(kind(&o, Kind::Masked), Kind::Raw);
        o.masked = Some(true);
        assert_eq!(kind(&o, Kind::Raw), Kind::Masked);
        o.schema_only = true;
        assert_eq!(kind(&o, Kind::Raw), Kind::Schema, "--schema-only wins");
    }

    #[test]
    fn a_dump_is_schema_only_when_nothing_asks_for_rows() {
        use crate::policy::Kind;
        assert_eq!(kind_of(&opts(false, &[])), Kind::Raw);
        assert_eq!(kind_of(&opts(true, &[])), Kind::Schema);
        assert_eq!(kind_of(&opts(false, &["--schema-only"])), Kind::Schema);
        assert_eq!(kind_of(&opts(false, &["-s"])), Kind::Schema);
        assert_eq!(kind_of(&opts(true, &["-n", "app"])), Kind::Schema);
        for rows in [
            &["--data-only"][..],
            &["-a"],
            &["-sa"],
            &["--data"],
            &["--section=data"],
            &["--sec", "data"],
            &["-b"],
            &["--large-objects"],
            &["--blobs"],
            &["--include-foreign-data=srv"],
        ] {
            assert_eq!(kind_of(&opts(true, rows)), Kind::Raw, "{rows:?}");
        }
        // A value isn't an option: `-t a` names a table called a.
        assert_eq!(kind_of(&opts(true, &["-t", "a"])), Kind::Schema);
        // pg_dump's --schema is -n, not --schema-only.
        assert_eq!(kind_of(&opts(false, &["--schema=app"])), Kind::Raw);
        let args = pg_dump_args("x", &opts(true, &[]), Path::new("p"));
        assert!(args.contains(&"--schema-only".to_string()), "{args:?}");
        let args = pg_dump_args("x", &opts(false, &[]), Path::new("p"));
        assert!(!args.contains(&"--schema-only".to_string()), "{args:?}");
    }

    fn rq<'a>(
        reason: Option<&'a str>,
        ticket: Option<&'a str>,
        needed: bool,
        branch_ticket: Option<&'a str>,
        offer: Option<&'a str>,
        name: &'a str,
    ) -> ReasonQuestion<'a> {
        ReasonQuestion {
            reason,
            ticket,
            needed,
            branch_ticket,
            offer,
            keys: &[],
            name,
        }
    }

    #[test]
    fn a_reason_is_given_asked_or_taken_from_the_branch() {
        // Given.
        assert_eq!(
            settle_reason(
                &rq(Some("repro APP-2"), None, true, Some("APP-1"), None, "p"),
                None
            ),
            Ok((Some("repro APP-2".into()), Some("APP-2".into())))
        );
        assert_eq!(
            settle_reason(&rq(None, Some("app-3"), true, None, None, "p"), None),
            Ok((None, Some("APP-3".into())))
        );
        assert!(
            settle_reason(&rq(None, Some("soon"), true, None, None, "p"), None)
                .unwrap_err()
                .contains("isn't a ticket key")
        );
        assert!(
            settle_reason(&rq(Some("  "), None, true, None, None, "p"), None)
                .unwrap_err()
                .contains("takes a reason")
        );
        // Needed, no terminal.
        let e = settle_reason(
            &rq(None, None, true, Some("APP-1"), Some("APP-1"), "production"),
            None,
        )
        .unwrap_err();
        assert!(e.contains("--reason") && e.contains("production"), "{e}");
        // Not needed: the branch's ticket, silently.
        assert_eq!(
            settle_reason(&rq(None, None, false, Some("APP-1"), None, "p"), None),
            Ok((None, Some("APP-1".into())))
        );
        // Asked: Enter takes the offer; an empty answer with no offer refuses.
        let enter = |_: Option<&str>| Ok(String::new());
        let ask: Ask = &enter;
        assert_eq!(
            settle_reason(
                &rq(None, None, true, Some("APP-1"), Some("APP-1"), "p"),
                Some(ask)
            ),
            Ok((Some("APP-1".into()), Some("APP-1".into())))
        );
        assert!(
            settle_reason(&rq(None, None, true, None, None, "p"), Some(ask))
                .unwrap_err()
                .contains("takes a reason")
        );
        let typed = |offer: Option<&str>| {
            assert_eq!(offer, Some("last time"));
            Ok("checking totals".to_string())
        };
        let ask: Ask = &typed;
        assert_eq!(
            settle_reason(
                &rq(None, None, true, None, Some("last time"), "p"),
                Some(ask)
            ),
            Ok((Some("checking totals".into()), None))
        );
    }

    #[test]
    fn synced_folders_take_no_raw_protected_copy() {
        use crate::policy::Kind;
        let icloud = Path::new("/Users/dev/Library/Mobile Documents/com~apple~CloudDocs/dumps");
        let dropbox = Path::new("/Users/dev/Library/CloudStorage/Dropbox/work");
        assert_eq!(synced(icloud), Some("iCloud Drive"));
        assert_eq!(synced(dropbox), Some("Dropbox"));
        assert_eq!(synced(Path::new("/Users/dev/Dropbox/x")), Some("Dropbox"));
        assert_eq!(
            synced(Path::new("/Users/dev/Dropbox")),
            Some("Dropbox"),
            "the folder itself"
        );
        assert_eq!(
            synced(Path::new("/Users/dev/Nextcloud/x")),
            Some("Nextcloud")
        );
        assert_eq!(
            synced(Path::new("/Users/dev/.local/share/pgman/dumps")),
            None
        );
        let e = place_problem(icloud, Kind::Raw, true, None).unwrap_err();
        assert!(
            e.contains("iCloud Drive") && e.contains("isn't written there"),
            "{e}"
        );
        let w = place_problem(dropbox, Kind::Masked, true, None)
            .unwrap()
            .unwrap();
        assert!(w.contains("copied off this machine"), "{w}");
        assert!(
            place_problem(icloud, Kind::Raw, false, None)
                .unwrap()
                .is_some(),
            "a warning"
        );
        assert_eq!(
            place_problem(Path::new("/tmp/x"), Kind::Raw, true, None),
            Ok(None)
        );
    }

    #[test]
    fn icloud_desktop_and_documents_are_synced_when_icloud_holds_them() {
        let home = std::env::temp_dir().join(format!("pgman-icloud-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("Documents/work")).unwrap();
        let dir = home.join("Documents/work");
        assert!(
            !icloud_desktop(&dir, &home),
            "iCloud doesn't hold Documents"
        );
        std::fs::create_dir_all(
            home.join("Library/Mobile Documents/com~apple~CloudDocs/Documents"),
        )
        .unwrap();
        assert!(icloud_desktop(&dir, &home));
        assert!(!icloud_desktop(&home.join("src"), &home), "elsewhere");
        let e = place_problem(&dir, crate::policy::Kind::Raw, true, Some(&home)).unwrap_err();
        assert!(e.contains("Desktop & Documents"), "{e}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_raw_warning_says_what_the_copy_is() {
        let w = raw_warning(
            "production",
            "the environment \"production\" has \"prod\" in its name",
        );
        assert!(
            w.starts_with("production is protected (the environment"),
            "{w}"
        );
        assert!(
            w.contains("personal data") && w.contains("unencrypted"),
            "{w}"
        );
        assert!(w.contains("pgman copies rm"), "{w}");
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

    fn unreadable(schema: &str, name: &str, kind: &str) -> crate::query::server::Unreadable {
        crate::query::server::Unreadable {
            schema: schema.into(),
            name: name.into(),
            kind: kind.into(),
            owner: "postgres".into(),
            reason: "no SELECT".into(),
            owned_by: None,
        }
    }

    #[test]
    fn excludes_name_exactly_what_cant_be_read() {
        let a = exclude_args(&[
            unreadable("public", "t_diary_y2021w1", "partition"),
            unreadable("Odd Schema", "we\"ird*.x", "table"),
        ]);
        assert_eq!(
            a,
            args(&[
                "--exclude-table=\"public\".\"t_diary_y2021w1\"",
                "--exclude-table=\"Odd Schema\".\"we\"\"ird*.x\"",
            ])
        );
        let o = parse_cli(&args(&["x", "--skip-unreadable"])).unwrap();
        assert!(o.skip_unreadable);
        assert_eq!(
            refused_passthrough(&a),
            None,
            "pgman's own excludes are not refused"
        );
    }

    #[test]
    fn the_refusal_counts_kinds_and_names_a_few() {
        let list: Vec<_> = (1..=11)
            .map(|i| unreadable("public", &format!("t_diary_w{i}"), "partition"))
            .collect();
        let o = parse_cli(&args(&["x"])).unwrap();
        let e = preflight("app_ro", &list, 0, &o).unwrap_err();
        assert!(e.starts_with("app_ro can't read what the dump would cover: 11 object(s) (11 partitions; no SELECT; owned by postgres)"), "{e}");
        assert!(
            e.contains("and 3 more") && e.contains("--skip-unreadable"),
            "{e}"
        );
        let o = parse_cli(&args(&["x", "--skip-unreadable"])).unwrap();
        let p = preflight("app_ro", &list[..1], 0, &o).unwrap();
        assert_eq!(p.args, args(&["--exclude-table=\"public\".\"t_diary_w1\""]));
        assert_eq!(
            p.notes,
            vec!["left out 1 object(s) app_ro can't read (1 partition; no SELECT): public.t_diary_w1".to_string()]
        );
    }

    #[test]
    fn a_sequence_a_readable_table_owns_cant_be_skipped() {
        let mut seq = unreadable("app", "accounts_id_seq", "sequence");
        seq.owned_by = Some(("app.accounts".into(), true));
        for flags in [&["x"][..], &["x", "--skip-unreadable"]] {
            let o = parse_cli(&args(flags)).unwrap();
            let e = preflight("app_ro", std::slice::from_ref(&seq), 0, &o).unwrap_err();
            assert!(
                e.contains("app.accounts_id_seq (of app.accounts)")
                    && e.contains("GRANT SELECT ON SEQUENCE"),
                "{flags:?}: {e}"
            );
            assert!(
                !e.contains("--skip-unreadable"),
                "never suggests the flag that can't help: {e}"
            );
        }
        // Owned by a table it can't read either: both are left out.
        seq.owned_by = Some(("app.secrets".into(), false));
        let table = unreadable("app", "secrets", "table");
        let o = parse_cli(&args(&["x", "--skip-unreadable"])).unwrap();
        let p = preflight("app_ro", &[seq, table], 0, &o).unwrap();
        assert_eq!(p.args.len(), 2, "{:?}", p.args);
    }

    #[test]
    fn unreadable_large_objects_go_all_together_and_say_so() {
        let o = parse_cli(&args(&["x"])).unwrap();
        let e = preflight("app_ro", &[], 3, &o).unwrap_err();
        assert!(
            e.contains("and 3 large object(s)") && e.contains("pg_dump -B"),
            "{e}"
        );
        let o = parse_cli(&args(&["x", "--skip-unreadable"])).unwrap();
        let p = preflight("app_ro", &[], 3, &o).unwrap();
        assert_eq!(p.args, args(&["-B"]));
        assert!(
            p.notes[0].contains("left out the large objects"),
            "{:?}",
            p.notes
        );
    }

    #[test]
    fn with_options_after_the_double_dash_it_only_warns() {
        let o = parse_cli(&args(&["x", "--", "--table=app.accounts"])).unwrap();
        let p = preflight("app_ro", &[unreadable("app", "secrets", "table")], 0, &o).unwrap();
        assert!(p.args.is_empty() && p.warning.unwrap().contains("if the dump covers one"));
        assert_eq!(
            preflight("app_ro", &[], 0, &o).unwrap(),
            Preflight::default()
        );
    }

    #[test]
    fn row_level_security_names_the_way_round_it() {
        let mut t = unreadable("app", "tenants", "table");
        t.reason = "row-level security".into();
        let e = preflight("app_ro", &[t], 0, &parse_cli(&args(&["x"])).unwrap()).unwrap_err();
        assert!(
            e.contains("row-level security") && e.contains("--enable-row-security"),
            "{e}"
        );
    }

    #[test]
    fn large_objects_and_row_security_get_their_own_advice() {
        let e = explain_failure(
            "pg_dump: error: query failed: ERROR:  permission denied for large object 93875",
            false,
            None,
        );
        assert!(
            e.contains("-- -B") && !e.contains("--skip-unreadable"),
            "{e}"
        );
        let e = explain_failure("pg_dump: error: Dumping the contents of table \"secrets\" failed: ERROR:  query would be affected by row-level security policy for table \"secrets\"", false, None);
        assert!(e.contains("--enable-row-security"), "{e}");
    }

    #[test]
    fn a_local_file_error_is_not_read_as_the_users_rights() {
        let tail = "pg_dump: error: could not open output file \"ro/.x.dump.partial-1\": Permission denied";
        let e = explain_failure(tail, false, Some("app_ro"));
        assert!(!e.contains("pg_read_all_data"), "{e}");
    }

    #[test]
    fn an_output_that_cant_work_is_refused_before_anything_runs() {
        let dir = std::env::temp_dir().join(format!("pgman-dump-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("adir")).unwrap();
        std::fs::write(dir.join("afile"), "x").unwrap();
        let check = |out: &str, format: &str| {
            let mut a = vec!["t".to_string(), dir.join(out).display().to_string()];
            if out.is_empty() {
                a.pop();
            }
            a.extend(["--format".to_string(), format.to_string()]);
            let o = parse_cli(&a).unwrap();
            let is_dir = o.out.as_deref().is_some_and(Path::is_dir);
            let p = paths("t", o.format, o.out.as_deref(), is_dir, 0, 1);
            output_problem(&o, &p)
        };
        assert!(check("newdir/", "custom")
            .unwrap()
            .contains("doesn't exist; create it"));
        assert!(check("nodir/x.dump", "custom")
            .unwrap()
            .contains("doesn't exist; create it"));
        assert!(check("afile", "directory").unwrap().contains("is a file"));
        assert_eq!(check("adir", "custom"), None, "a directory to put it in");
        assert_eq!(check("afile", "custom"), None, "a file it replaces");
        assert_eq!(check("new.dir", "directory"), None);
        let _ = std::fs::remove_dir_all(&dir);
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
                "--no-password",
                "--verbose",
                "--schema-only",
            ])
        );
    }

    #[test]
    fn a_directory_dump_names_its_directory_and_a_file_dump_names_nothing() {
        let o = parse_cli(&args(&["x", "--format", "directory"])).unwrap();
        let a = pg_dump_args("c", &o, Path::new("/d/.x.partial-1"));
        assert!(a.contains(&"--file=.".to_string()), "run inside it: {a:?}");
        let o = parse_cli(&args(&["x"])).unwrap();
        let a = pg_dump_args("c", &o, Path::new("/d/.x.partial-1"));
        assert!(!a.iter().any(|x| x.starts_with("--file")), "{a:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_partial_is_made_fresh_and_private_and_nothing_planted_is_followed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pgman-dump-plant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
        let victim = dir.join("elsewhere/victim");
        std::fs::write(&victim, "keep me").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let f = dir.join(".a.dump.partial-1");
        assert!(create_partial(&f, Format::Custom).unwrap().is_some());
        assert_eq!(mode(&f), 0o600);
        let d = dir.join(".d.dir.partial-1");
        assert!(create_partial(&d, Format::Directory).unwrap().is_none());
        assert_eq!(mode(&d), 0o700);

        let sym = dir.join(".s.dump.partial-1");
        std::os::unix::fs::symlink(&victim, &sym).unwrap();
        assert!(create_partial(&sym, Format::Custom)
            .unwrap_err()
            .contains("won't write a dump through"));
        let hard = dir.join(".h.dump.partial-1");
        std::fs::hard_link(&victim, &hard).unwrap();
        assert!(create_partial(&hard, Format::Plain).is_err());
        let dsym = dir.join(".ds.dir.partial-1");
        std::os::unix::fs::symlink(dir.join("elsewhere"), &dsym).unwrap();
        assert!(create_partial(&dsym, Format::Directory).is_err());
        let dangling = dir.join(".dg.dump.partial-1");
        std::os::unix::fs::symlink(dir.join("elsewhere/new"), &dangling).unwrap();
        assert!(create_partial(&dangling, Format::Custom).is_err());
        assert!(
            !dir.join("elsewhere/new").exists(),
            "a dangling link isn't followed either"
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_swapped_directory_has_another_identity() {
        let dir = std::env::temp_dir().join(format!("pgman-dump-ident-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
        let d = dir.join(".d.dir.partial-1");
        create_partial(&d, Format::Directory).unwrap();
        let made = identity(&d);
        assert!(made.is_some());
        assert_eq!(identity(&d), made, "unchanged");
        std::fs::rename(&d, dir.join("moved")).unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere"), &d).unwrap();
        assert_ne!(identity(&d), made, "a link in its place");
        std::fs::remove_file(&d).unwrap();
        std::fs::create_dir(&d).unwrap();
        assert_ne!(identity(&d), made, "another directory in its place");
        let _ = std::fs::remove_dir_all(&dir);
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
