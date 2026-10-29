//! `pgman restore <FILE> <target>`: a dump restored with the operator's
//! own `pg_restore` into a database pgman knows — never a protected one,
//! and by default only one on this machine.
//!
//! Before anything changes, [`plan`] decides what the restore does from
//! what is there: restore into the existing database, create it
//! (`--create`), or drop and recreate it (`--clean`); and it refuses
//! while other sessions are connected to it, unless `--terminate` ends
//! them. The operator confirms that plan — typing the database's name
//! when it would be dropped — or passes `--yes`.
//!
//! `--as NAME` restores into a database of another name on the target's
//! server: pgman creates the database itself and has pg_restore fill it,
//! rather than pg_restore's own `--create`, which only ever recreates the
//! name the dump was taken from.
//!
//! Ownership and grants are left out by default (`--no-owner
//! --no-privileges`): a dump of a remote environment names its roles,
//! which a local server doesn't have. `--keep-owners` keeps them.

use crate::dump::passthrough_refusal;
use crate::inventory::target::{self, Resolved, Want};
use crate::pgtool::{self, Major};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "\
usage: pgman restore <FILE> <target> [--as NAME] [--create] [--clean] [--terminate]
                     [--keep-owners] [--jobs N] [--allow-remote] [--yes]
                     [--pg-bin DIR] [--verbose] [-- PG_RESTORE_OPTIONS...]

Restore a custom, directory or tar dump with your own pg_restore into
<target> (as for pgman dump: ENV[/SERVER]/DATABASE, a connection name,
or a postgres:// URL), through pgman's connection.

Never into a protected database: one of an environment with
protected = true (or \"prod\" in its name), a name with \"prod\" in it, or
any database on a server a protected environment reaches. Only into a database on this machine unless
--allow-remote. Refused while other sessions are connected to it,
unless --terminate.

  --as NAME       restore into database NAME on the target's server
                  instead of the target's own database
  --create        create the database when it doesn't exist
  --clean         drop the database and create it again first
                  (asks you to type its name, unless --yes)
  --terminate     end the other sessions on it first
  --keep-owners   keep the dump's owners and grants (default: left
                  out, since the roles rarely exist here)
  --jobs N        restore with N parallel jobs
  --allow-remote  allow a target that isn't on this machine (still never
                  a protected one)
  --yes           don't ask; for scripts
  --pg-bin DIR    where pg_restore is (default: $PGMAN_PG_BIN, then
                  Homebrew's libpq, then PATH)
  --verbose       show pg_restore's progress
  --              anything after it goes to pg_restore as it is

  pgman restore staging-ledger-latest.dump local --as ledger_copy --create
  pgman restore staging-ledger-latest.dump local --clean --terminate";

/// Parsed `pgman restore` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Opts {
    pub file: PathBuf,
    pub target: String,
    pub as_name: Option<String>,
    pub create: bool,
    pub clean: bool,
    pub terminate: bool,
    pub keep_owners: bool,
    pub jobs: Option<u32>,
    pub allow_remote: bool,
    pub yes: bool,
    pub pg_bin: Option<PathBuf>,
    pub verbose: bool,
    pub extra: Vec<String>,
}

/// pg_restore's short options that take a value.
const SHORT_WITH_VALUE: &str = "dfFhIjLnNpPStTU";
/// The ones pgman sets: where to connect, as whom, and that it restores
/// into an existing database (`-C` would restore into the dump's own).
const SHORT_REFUSED: &str = "dhpUWwCcf";
const LONG_REFUSED: &[&str] = &[
    "dbname",
    "host",
    "port",
    "username",
    "password",
    "no-password",
    "create",
    "clean",
    "file",
];
const LONG_WITH_VALUE: &[&str] = &[
    "exclude-schema",
    "filter",
    "format",
    "function",
    "index",
    "jobs",
    "list",
    "use-list",
    "role",
    "schema",
    "section",
    "superuser",
    "table",
    "trigger",
];

/// Parse the arguments after `pgman restore`. Pure.
pub fn parse_cli(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut positional: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))
        };
        match a.as_str() {
            "--" => {
                o.extra = it.by_ref().cloned().collect();
                break;
            }
            "--as" => o.as_name = Some(value("--as")?),
            "--create" => o.create = true,
            "--clean" => o.clean = true,
            "--terminate" => o.terminate = true,
            "--keep-owners" => o.keep_owners = true,
            "--allow-remote" => o.allow_remote = true,
            "--yes" | "-y" => o.yes = true,
            "--verbose" | "-v" => o.verbose = true,
            "--pg-bin" => o.pg_bin = Some(PathBuf::from(value("--pg-bin")?)),
            "--jobs" | "-j" => {
                let v = value("--jobs")?;
                o.jobs = Some(
                    v.parse()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| format!("--jobs {v:?}: a number from 1"))?,
                );
            }
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown option {other:?} (pg_restore's own go after --)\n\n{USAGE}"
                ))
            }
            other => positional.push(other.to_string()),
        }
    }
    let mut positional = positional.into_iter();
    o.file = positional
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("which dump? name the file\n\n{USAGE}"))?;
    o.target = positional
        .next()
        .ok_or_else(|| format!("restore into what? name a target\n\n{USAGE}"))?;
    if let Some(more) = positional.next() {
        return Err(format!("unexpected argument {more:?}\n\n{USAGE}"));
    }
    if let Some(name) = &o.as_name {
        check_name(name)?;
    }
    if let Some(why) = passthrough_refusal(
        &o.extra,
        SHORT_WITH_VALUE,
        SHORT_REFUSED,
        LONG_REFUSED,
        LONG_WITH_VALUE,
    ) {
        return Err(why);
    }
    Ok(o)
}

/// A database name pgman will create or drop: 1–63 bytes, no NUL. Pure.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 63 || name.contains('\0') {
        return Err(format!("--as {name:?}: a database name is 1 to 63 bytes"));
    }
    Ok(())
}

/// Why `target` may not be restored into at all, before connecting.
/// Pure.
pub fn refusal(target: &Resolved, database: &str, allow_remote: bool) -> Option<String> {
    if target::named_like_prod(database) && database != target.database {
        return Some(format!(
            "{database} has \"prod\" in its name, which makes it protected; pgman never \
             restores into a protected database"
        ));
    }
    if target.protected {
        return Some(format!(
            "{} is protected ({}); pgman never restores into a protected database",
            target.label,
            target.protection()
        ));
    }
    if !target.is_local() && !allow_remote {
        return Some(format!(
            "{} isn't on this machine ({}{}); restoring replaces data, so pgman only does it \
             locally unless you pass --allow-remote",
            target.label,
            crate::conn::host_port(&target.connect.dsn.host, target.connect.dsn.port),
            if target.connect.dsn.ssh_tunnel.is_some() {
                ", through an ssh tunnel"
            } else {
                ""
            }
        ));
    }
    None
}

/// What the restore does to the database before pg_restore fills it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prepare {
    /// Restore into it as it is.
    Existing,
    /// Create it.
    Create,
    /// Drop it and create it again.
    Replace,
}

/// What [`plan`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub database: String,
    pub prepare: Prepare,
    /// Sessions to end first (`--terminate`).
    pub terminate: Vec<crate::query::server::Session>,
}

/// What to do to `database` (which `exists` or not, with `sessions`
/// other than pgman's) for `opts`, or why not. Pure.
pub fn plan(
    opts: &Opts,
    database: &str,
    exists: bool,
    sessions: Vec<crate::query::server::Session>,
) -> Result<Plan, String> {
    let prepare = match (exists, opts.clean, opts.create) {
        (true, true, _) => Prepare::Replace,
        (true, false, true) => {
            return Err(format!(
                "database {database} already exists; --clean replaces it, or leave out --create \
                 to restore into it as it is"
            ))
        }
        (true, false, false) => Prepare::Existing,
        (false, _, true) | (false, true, _) => Prepare::Create,
        (false, false, false) => {
            return Err(format!(
                "database {database} doesn't exist; --create makes it"
            ))
        }
    };
    if prepare == Prepare::Replace && ["postgres", "template0", "template1"].contains(&database) {
        return Err(format!(
            "{database} is one of the server's own databases; --clean won't drop it"
        ));
    }
    if !sessions.is_empty() && !opts.terminate {
        let who: Vec<String> = sessions
            .iter()
            .take(5)
            .map(|s| {
                let app = if s.application.is_empty() {
                    String::new()
                } else {
                    format!(" {}", s.application)
                };
                format!("pid {} ({}{app})", s.pid, s.user)
            })
            .collect();
        return Err(format!(
            "{} other session(s) connected to {database}: {}{}. Close them, or pass --terminate \
             to end them",
            sessions.len(),
            who.join(", "),
            if sessions.len() > 5 { ", …" } else { "" }
        ));
    }
    Ok(Plan {
        database: database.to_string(),
        prepare,
        terminate: sessions,
    })
}

/// The sessions to end before the restore, from those connected `now`
/// (after the operator confirmed `plan`): all of them with
/// `--terminate`; without it, any is a refusal — one connected since the
/// plan, which found none. Pure.
pub fn sessions_to_end(
    plan: &Plan,
    now: Vec<crate::query::server::Session>,
    terminate: bool,
) -> Result<Vec<crate::query::server::Session>, String> {
    if terminate || now.is_empty() {
        return Ok(now);
    }
    Err(format!(
        "{} session(s) connected to {} since you confirmed (pid {}); nothing changed. Run it \
         again, or with --terminate",
        now.len(),
        plan.database,
        now.iter()
            .map(|s| s.pid.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The confirmation question for `plan` on `server`, and the answer that
/// accepts it: the database's name when it would be dropped, `y`
/// otherwise. Pure.
pub fn question(plan: &Plan, file: &Path, server: &str) -> (String, String) {
    let what = match plan.prepare {
        Prepare::Existing => format!("into the existing database {}", plan.database),
        Prepare::Create => format!("into a new database {}", plan.database),
        Prepare::Replace => format!(
            "into {}, which is DROPPED and created again first — everything in it now is lost",
            plan.database
        ),
    };
    let end = if plan.terminate.is_empty() {
        String::new()
    } else {
        format!(
            "\n  ending {} other session(s) on it first",
            plan.terminate.len()
        )
    };
    let text = format!("restore {} on {server} {what}{end}", file.display());
    match plan.prepare {
        Prepare::Replace => (
            format!("{text}\ntype {} to go on: ", plan.database),
            plan.database.clone(),
        ),
        _ => (format!("{text}\ngo on? [y/N] "), "y".to_string()),
    }
}

/// What `pg_restore -l` says about an archive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Archive {
    pub dbname: Option<String>,
    pub dumped_by: Option<Major>,
    pub dumped_from: Option<Major>,
    pub from_text: Option<String>,
}

/// Read `pg_restore -l`'s header. Pure.
pub fn parse_archive(listing: &str) -> Archive {
    let mut a = Archive::default();
    for line in listing.lines().take(40) {
        let Some(rest) = line.strip_prefix(';') else {
            continue;
        };
        let rest = rest.trim();
        if let Some(v) = rest.strip_prefix("dbname:") {
            a.dbname = Some(v.trim().to_string());
        } else if let Some(v) = rest.strip_prefix("Dumped by pg_dump version:") {
            a.dumped_by = Major::parse(v.trim());
        } else if let Some(v) = rest.strip_prefix("Dumped from database version:") {
            a.dumped_from = Major::parse(v.trim());
            a.from_text = Some(v.trim().to_string());
        }
    }
    a
}

/// The pg_restore command line, into `conninfo`. Pure.
pub fn pg_restore_args(conninfo: &str, opts: &Opts) -> Vec<String> {
    let mut args = vec![format!("--dbname={conninfo}"), "--no-password".to_string()];
    if !opts.keep_owners {
        args.push("--no-owner".to_string());
        args.push("--no-privileges".to_string());
    }
    if let Some(n) = opts.jobs {
        args.push(format!("--jobs={n}"));
    }
    if opts.verbose {
        args.push("--verbose".to_string());
    }
    args.extend(opts.extra.iter().cloned());
    args.push(opts.file.display().to_string());
    args
}

/// How many errors pg_restore said it ignored, if it said. Pure.
pub fn ignored_errors(tail: &str) -> Option<u64> {
    tail.lines().rev().find_map(|l| {
        l.split_once("errors ignored on restore:")
            .and_then(|(_, n)| n.trim().parse().ok())
    })
}

/// One error pg_restore reported and went past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreError {
    pub message: String,
    /// The statement, from pg_restore's `Command was:` line.
    pub command: Option<String>,
    /// A setting the dump's newer pg_dump writes that this server
    /// doesn't have (`SET transaction_timeout = 0` from pg_dump 17+ into
    /// a 16): nothing is lost.
    pub harmless: bool,
}

/// The errors in pg_restore's stderr `tail`, in order. Pure.
pub fn restore_errors(tail: &str) -> Vec<RestoreError> {
    let lines: Vec<&str> = tail.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some((_, msg)) = l.split_once("pg_restore: error: ") else {
            continue;
        };
        let command = lines
            .get(i + 1)
            .and_then(|n| n.strip_prefix("Command was: "))
            .map(|c| c.trim().to_string());
        let harmless = msg.contains("unrecognized configuration parameter")
            && command
                .as_deref()
                .is_some_and(|c| c.to_ascii_uppercase().starts_with("SET "));
        out.push(RestoreError {
            message: msg.trim().to_string(),
            command,
            harmless,
        });
    }
    out
}

/// What a finished restore reports.
#[derive(Debug, Clone)]
pub struct Done {
    pub database: String,
    pub server: String,
    pub errors: Option<u64>,
    /// What [`restore_errors`] read from pg_restore's output.
    pub reported: Vec<RestoreError>,
    pub notes: Vec<String>,
}

/// Why pg_restore (version `have`) can't read `file` (which starts with
/// `head`), from what `pg_restore --list` said. Pure.
pub fn unreadable(file: &Path, head: &[u8], stderr: &str, have: Major) -> String {
    let why = stderr.trim();
    let hint = if why.contains("text format dump") || head.starts_with(b"--") {
        format!(
            "pgman restore takes custom, directory and tar dumps, not a plain SQL one: run that \
             with psql yourself (psql -d DATABASE -f {})",
            file.display()
        )
    } else if why.contains("unsupported version") {
        format!("The dump was made by a newer pg_dump than your pg_restore {have}; use one at least as new (--pg-bin DIR)")
    } else {
        "pgman restore takes a custom, directory or tar dump".to_string()
    };
    format!(
        "{} isn't a dump pg_restore reads ({why}). {hint}",
        file.display()
    )
}

/// A writable connection to another database on `opened`'s server than
/// the one being restored: `postgres`, or `template1`.
async fn admin(
    opened: &pgtool::Opened,
    avoid: &str,
) -> Result<std::sync::Arc<tokio_postgres::Client>, String> {
    let mut first = None;
    for db in ["postgres", "template1"] {
        if db == avoid {
            continue;
        }
        let mut d = opened.dsn.clone();
        d.dbname = db.to_string();
        match crate::conn::connect_via_writable(d, opened.via, 10_000).await {
            Ok(c) => return Ok(c),
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    Err(format!(
        "could not connect to the server's postgres database: {}",
        first.unwrap_or_default()
    ))
}

/// Run a restore. Progress and the question go to stderr.
pub async fn run_cli(opts: &Opts) -> Result<Done, String> {
    let ready = prepare(opts, &|line| eprintln!("pgman restore: {line}")).await?;
    if !opts.yes {
        let (prompt, accept) = ready.question();
        let ok = tokio::task::spawn_blocking(move || pgtool::confirm_cli(&prompt, &accept))
            .await
            .map_err(|e| format!("prompt task failed: {e}"))??;
        if !ok {
            return Err("not confirmed; nothing changed".to_string());
        }
    }
    let mut done = ready.execute().await?;
    done.notes.extend(journal_restore(&opts.file, &done));
    done.notes.extend(crate::journal_send::flush_note().await);
    Ok(done)
}

/// Record a restore of `file` in the journal, against the copy the
/// journal knows it as (if any). What to add to the notes when the
/// journal couldn't be written.
pub fn journal_restore(file: &Path, done: &Done) -> Option<String> {
    use crate::journal::{Event, Kind};
    let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let (events, _) = crate::journal::read_in(&crate::journal::dir());
    let copies = crate::copies::registry(&events);
    let mut e = Event::new(
        Kind::Restore,
        crate::journal::now(),
        &crate::journal::machine_id(),
    );
    e.copy = crate::copies::by_file(&copies, &file).map(|c| c.id.clone());
    e.file = Some(file.display().to_string());
    e.into_server = Some(done.server.clone());
    e.into_database = Some(done.database.clone());
    crate::journal::append(&e)
}

/// A restore checked, connected and planned, waiting for the operator's
/// yes: [`Ready::question`] is what to ask, [`Ready::execute`] does it.
/// Nothing has changed yet.
pub struct Ready {
    opts: Opts,
    tool: PathBuf,
    have: Major,
    opened: pgtool::Opened,
    client: std::sync::Arc<tokio_postgres::Client>,
    server: String,
    pub plan: Plan,
    notes: Vec<String>,
}

impl Ready {
    /// `host:port` of the server it restores on.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The question for the plan, and the answer that accepts it.
    pub fn question(&self) -> (String, String) {
        question(&self.plan, &self.opts.file, &self.server)
    }

    /// End the sessions, make or replace the database, run pg_restore.
    pub async fn execute(self) -> Result<Done, String> {
        let Ready {
            opts,
            tool,
            have,
            opened,
            client,
            server,
            plan,
            notes,
        } = self;
        let database = plan.database.clone();
        // Listed again: someone may have connected while the operator
        // read the question.
        let now = if plan.prepare == Prepare::Create {
            Vec::new()
        } else {
            crate::query::server::sessions_on(&client, &database).await?
        };
        let to_end = sessions_to_end(&plan, now, opts.terminate)?;
        for s in &to_end {
            crate::conn::terminate_backend(&client, s.pid).await?;
        }
        match plan.prepare {
            Prepare::Existing => {}
            Prepare::Create => crate::query::server::create_database(&client, &database).await?,
            Prepare::Replace => {
                drop_again_and_again(&client, &database).await?;
                crate::query::server::create_database(&client, &database).await?;
            }
        }
        drop(client);
        tracing::info!(target: "pgman::restore", database = %database, server = %server, prepare = ?plan.prepare, "pg_restore starting");

        let mut dsn = opened.dsn.clone();
        dsn.dbname = database.clone();
        let conninfo = pgtool::conninfo(&dsn, opened.via, "pgman-restore", have);
        // Not pgtool::private_command: pg_restore reads files, it makes
        // none. Anything that writes one must go through it (umask 077).
        let mut cmd = tokio::process::Command::new(&tool);
        cmd.args(pg_restore_args(&conninfo, &opts));
        pgtool::set_child_env(&mut cmd, opened.password());
        let (status, tail) = pgtool::run_tool_cli(cmd, opts.verbose)
            .await
            .map_err(|e| format!("pg_restore {e}; {database} is part-restored"))?;
        drop(opened);
        let errors_text = tail.errors();
        let tail = tail.text();
        let errors = ignored_errors(&tail);
        if !status.success() && errors.is_none() {
            return Err(format!(
                "{}\n  {database} may be part-restored",
                crate::dump::last_error(&tail).unwrap_or("pg_restore failed with no message")
            ));
        }
        tracing::info!(target: "pgman::restore", database = %database, errors = ?errors, "pg_restore done");
        Ok(Done {
            database,
            server,
            errors,
            reported: restore_errors(&errors_text),
            notes,
        })
    }
}

/// Everything up to the question: refuse what can't be restored into,
/// read the archive, connect, and plan from what is there.
pub async fn prepare(opts: &Opts, say: &(dyn Fn(String) + Send + Sync)) -> Result<Ready, String> {
    if !opts.file.exists() {
        return Err(format!("{}: no such file", opts.file.display()));
    }
    let text = opts.target.clone();
    let resolved = tokio::task::spawn_blocking(move || target::resolve(&text, Want::Primary))
        .await
        .map_err(|e| format!("resolve task failed: {e}"))??;
    let database = opts
        .as_name
        .clone()
        .unwrap_or_else(|| resolved.database.clone());
    if let Some(why) = refusal(&resolved, &database, opts.allow_remote) {
        return Err(why);
    }
    let (tool, found_in) = pgtool::find_tool_from("pg_restore", opts.pg_bin.as_deref())?;
    let have = pgtool::tool_version(&tool)?;
    let listing = tokio::process::Command::new(&tool)
        .arg("--list")
        .arg(&opts.file)
        .env_clear()
        .output()
        .await
        .map_err(|e| format!("could not run pg_restore: {e}"))?;
    if !listing.status.success() {
        let mut head = [0u8; 2];
        let read = std::fs::File::open(&opts.file)
            .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
            .unwrap_or(0);
        return Err(unreadable(
            &opts.file,
            &head[..read],
            &String::from_utf8_lossy(&listing.stderr),
            have,
        ));
    }
    let archive = parse_archive(&String::from_utf8_lossy(&listing.stdout));
    if let Some(by) = archive.dumped_by.filter(|by| *by > have) {
        return Err(format!(
            "the dump was made by pg_dump {by}, newer than your pg_restore {have}; use a \
             pg_restore at least as new (--pg-bin DIR)"
        ));
    }
    let server = crate::conn::host_port(&resolved.connect.dsn.host, resolved.connect.dsn.port);
    say(format!(
        "{} ({}) → {database} on {server}, with pg_restore {have} ({found_in}: {})",
        opts.file.display(),
        archive
            .from_text
            .as_deref()
            .map(|v| format!("dumped from PostgreSQL {v}"))
            .unwrap_or_else(|| "an archive".to_string()),
        tool.display()
    ));

    let opened = pgtool::open(&resolved).await?;
    let client = admin(&opened, &database)
        .await
        .map_err(|e| pgtool::explain_connect(e, &resolved))?;
    let facts = crate::query::server::facts(&client).await?;
    if facts.in_recovery {
        return Err(format!("{server} is a standby; a restore needs a primary"));
    }
    let mut notes = Vec::new();
    let server_major = Major::from_num(facts.version_num);
    if let (Some(from), Some(to)) = (archive.dumped_from, server_major) {
        if from > to {
            notes.push(format!(
                "the dump is from PostgreSQL {from}, the server here is {to}: some statements may not restore"
            ));
        }
    }
    let exists = crate::query::server::database_exists(&client, &database).await?;
    let sessions = if exists {
        crate::query::server::sessions_on(&client, &database).await?
    } else {
        Vec::new()
    };
    let plan = plan(opts, &database, exists, sessions)?;
    Ok(Ready {
        opts: opts.clone(),
        tool,
        have,
        opened,
        client,
        server,
        plan,
        notes,
    })
}

/// Drop `database`, again for a moment while a just-ended session is
/// still going away.
async fn drop_again_and_again(
    client: &tokio_postgres::Client,
    database: &str,
) -> Result<(), String> {
    let mut tries = 0;
    loop {
        match crate::query::server::drop_database(client, database).await {
            Ok(()) => return Ok(()),
            Err(e) if tries < 20 && e.contains("being accessed by other users") => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

impl Done {
    /// Errors pg_restore went past that aren't [`RestoreError::harmless`]:
    /// a restore with any is incomplete, and exits non-zero.
    ///
    /// Read from what pg_restore printed, not its count: under `--jobs`
    /// each worker prints the harmless SET a newer pg_dump writes, while
    /// the count holds the leader's alone. Any it counted that pgman never
    /// saw are counted as real.
    pub fn real_errors(&self) -> u64 {
        let seen = self.reported.len() as u64;
        let real = self.reported.iter().filter(|e| !e.harmless).count() as u64;
        real + self.errors.unwrap_or(0).saturating_sub(seen)
    }
}

/// How many of `errors` the restore's list shows.
const LISTED: usize = 12;

/// What `pgman restore` prints when it finishes. Pure.
pub fn render(done: &Done) -> String {
    let harmless = done.reported.iter().any(|e| e.harmless);
    let real = done.real_errors();
    let ignored = if harmless {
        " (a newer pg_dump's SET of a setting this server doesn't have: ignored)"
    } else {
        ""
    };
    let mut out = if real == 0 {
        format!(
            "restored into {} on {}{ignored}",
            done.database, done.server
        )
    } else {
        let listed: Vec<&RestoreError> = done.reported.iter().filter(|e| !e.harmless).collect();
        let exists = listed
            .iter()
            .filter(|e| e.message.contains("already exists") || e.message.contains("duplicate key"))
            .count();
        let mut s = format!(
            "restored into {} on {}, with {real} error(s) pg_restore went past{}:",
            done.database,
            done.server,
            if exists * 2 > listed.len() {
                format!(
                    " — {exists} of them \"already exists\" or \"duplicate key\": the database \
                     wasn't empty. --clean replaces it instead"
                )
            } else {
                String::new()
            }
        );
        for e in listed.iter().take(LISTED) {
            s.push_str(&format!("\n  {}", e.message));
            if let Some(c) = &e.command {
                s.push_str(&format!("\n    in: {}", ellipsize(c, 200)));
            }
        }
        let more = real.saturating_sub(LISTED.min(listed.len()) as u64);
        if more > 0 {
            s.push_str(&format!(
                "\n  … and {more} more (--verbose shows pg_restore's own output)"
            ));
        }
        if harmless {
            s.push_str(
                "\n  and a newer pg_dump's SET of a setting this server doesn't have: ignored",
            );
        }
        s
    };
    for n in &done.notes {
        out.push_str(&format!("\nnote: {n}"));
    }
    out
}

/// `text` cut to `max` characters, with an ellipsis. Pure.
fn ellipsize(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::server::Session;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    fn opts(s: &[&str]) -> Opts {
        parse_cli(&args(s)).unwrap()
    }

    #[test]
    fn a_file_a_target_and_options_parse() {
        let o = opts(&[
            "x.dump",
            "local",
            "--as",
            "ledger_copy",
            "--create",
            "--jobs",
            "4",
            "--",
            "--schema=app",
        ]);
        assert_eq!(o.file, PathBuf::from("x.dump"));
        assert_eq!(o.target, "local");
        assert_eq!(o.as_name.as_deref(), Some("ledger_copy"));
        assert!(o.create && !o.clean);
        assert_eq!(o.jobs, Some(4));
        assert_eq!(o.extra, args(&["--schema=app"]));
        assert!(parse_cli(&args(&["x.dump"]))
            .unwrap_err()
            .contains("name a target"));
        assert!(parse_cli(&[]).unwrap_err().contains("name the file"));
        assert!(parse_cli(&args(&["x", "y", "--jobs", "0"]))
            .unwrap_err()
            .contains("--jobs"));
        assert!(parse_cli(&args(&["x", "y", "--as", ""]))
            .unwrap_err()
            .contains("1 to 63"));
    }

    #[test]
    fn passthrough_cant_redirect_the_restore() {
        for bad in [
            &["-d", "postgres://elsewhere/x"][..],
            &["--dbname=other"],
            &["-C"],
            &["--create"],
            &["--clean"],
            &["-c"],
            &["--ho=x"],
        ] {
            let mut a = args(&["x.dump", "local", "--"]);
            a.extend(args(bad));
            assert!(parse_cli(&a).is_err(), "{bad:?}");
        }
        let mut a = args(&["x.dump", "local", "--"]);
        a.extend(args(&[
            "--single-transaction",
            "-t",
            "-h",
            "--exit-on-error",
        ]));
        assert!(parse_cli(&a).is_ok());
    }

    fn target(url: &str) -> Resolved {
        target::from_url(url).unwrap()
    }

    #[test]
    fn a_protected_target_is_refused_even_when_remote_is_allowed() {
        let mut t = target("postgres://u@localhost/ledger");
        t.protected = true;
        assert!(refusal(&t, &t.database.clone(), true)
            .unwrap()
            .contains("never restores"));
        let t = target("postgres://u@localhost/ledger_prod");
        assert!(refusal(&t, &t.database.clone(), false)
            .unwrap()
            .contains("protected"));
    }

    #[test]
    fn as_a_prod_name_is_protected_like_the_url_would_be() {
        let t = target("postgres://u@localhost/ledger");
        assert!(refusal(&t, "ledger_prod", false)
            .unwrap()
            .contains("\"prod\" in its name"));
        assert_eq!(refusal(&t, "ledger_copy", false), None);
    }

    #[test]
    fn clean_never_drops_the_servers_own_databases() {
        for db in ["postgres", "template0", "template1"] {
            let e = plan(&opts(&["f", "t", "--clean"]), db, true, Vec::new()).unwrap_err();
            assert!(e.contains("server's own databases"), "{e}");
        }
        assert!(
            plan(&opts(&["f", "t"]), "postgres", true, Vec::new()).is_ok(),
            "restoring into it is the operator's call"
        );
    }

    #[test]
    fn a_remote_target_needs_the_flag() {
        let t = target("postgres://u@db.example/ledger");
        let e = refusal(&t, &t.database.clone(), false).unwrap();
        assert!(
            e.contains("isn't on this machine (db.example:5432)") && e.contains("--allow-remote"),
            "{e}"
        );
        assert_eq!(refusal(&t, &t.database.clone(), true), None);
        let mut t = target("postgres://u@localhost/ledger");
        assert_eq!(refusal(&t, &t.database.clone(), false), None);
        t.connect.dsn.ssh_tunnel = Some(crate::tunnel::SshTunnelSpec::parse("jump").unwrap());
        assert!(refusal(&t, &t.database.clone(), false)
            .unwrap()
            .contains("through an ssh tunnel"));
    }

    fn session(pid: i32) -> Session {
        Session {
            pid,
            user: "app".into(),
            application: "psql".into(),
        }
    }

    #[test]
    fn the_plan_follows_what_is_there_and_what_was_asked() {
        let p = |o: &[&str], exists| plan(&opts(o), "ledger", exists, Vec::new());
        assert_eq!(p(&["f", "t"], true).unwrap().prepare, Prepare::Existing);
        assert_eq!(
            p(&["f", "t", "--clean"], true).unwrap().prepare,
            Prepare::Replace
        );
        assert_eq!(
            p(&["f", "t", "--create"], false).unwrap().prepare,
            Prepare::Create
        );
        assert_eq!(
            p(&["f", "t", "--clean"], false).unwrap().prepare,
            Prepare::Create
        );
        assert!(p(&["f", "t"], false)
            .unwrap_err()
            .contains("--create makes it"));
        assert!(p(&["f", "t", "--create"], true)
            .unwrap_err()
            .contains("--clean replaces it"));
    }

    #[test]
    fn other_sessions_refuse_the_restore_unless_ended() {
        let e = plan(
            &opts(&["f", "t", "--clean"]),
            "ledger",
            true,
            vec![session(41), session(42)],
        )
        .unwrap_err();
        assert!(
            e.contains("2 other session(s)") && e.contains("pid 41 (app psql)"),
            "{e}"
        );
        assert!(e.contains("--terminate"), "{e}");
        let p = plan(
            &opts(&["f", "t", "--clean", "--terminate"]),
            "ledger",
            true,
            vec![session(41)],
        )
        .unwrap();
        assert_eq!(p.terminate, vec![session(41)]);
    }

    #[test]
    fn a_session_that_connected_after_the_question_stops_it_unless_ending_them() {
        let p = plan(&opts(&["f", "t", "--clean"]), "ledger", true, Vec::new()).unwrap();
        let e = sessions_to_end(&p, vec![session(77)], false).unwrap_err();
        assert!(e.contains("since you confirmed (pid 77)"), "{e}");
        assert_eq!(
            sessions_to_end(&p, vec![session(77)], true).unwrap(),
            vec![session(77)]
        );
        assert_eq!(sessions_to_end(&p, Vec::new(), false).unwrap(), Vec::new());
    }

    #[test]
    fn replacing_asks_for_the_name_and_the_rest_for_yes() {
        let p = plan(&opts(&["f", "t", "--clean"]), "ledger", true, Vec::new()).unwrap();
        let (q, accept) = question(&p, Path::new("x.dump"), "localhost:5432");
        assert!(q.contains("DROPPED") && q.contains("type ledger"), "{q}");
        assert_eq!(accept, "ledger");
        let p = plan(&opts(&["f", "t", "--create"]), "ledger", false, Vec::new()).unwrap();
        let (q, accept) = question(&p, Path::new("x.dump"), "localhost:5432");
        assert!(
            q.contains("into a new database ledger") && q.ends_with("[y/N] "),
            "{q}"
        );
        assert_eq!(accept, "y");
    }

    #[test]
    fn the_archive_header_names_its_versions() {
        let a = parse_archive(
            ";\n; Archive created at 2026-10-28 13:47:45 GMT\n;     dbname: ledger\n;     TOC Entries: 112\n\
             ;     Dumped from database version: 16.15 (Debian 16.15-1.pgdg13+2)\n;     Dumped by pg_dump version: 18.4\n;\n",
        );
        assert_eq!(a.dbname.as_deref(), Some("ledger"));
        assert_eq!(a.dumped_by, Major::parse("18"));
        assert_eq!(a.dumped_from, Major::parse("16"));
        assert_eq!(
            a.from_text.as_deref(),
            Some("16.15 (Debian 16.15-1.pgdg13+2)")
        );
        assert_eq!(parse_archive(""), Archive::default());
    }

    #[test]
    fn owners_are_left_out_unless_kept_and_the_file_comes_last() {
        let a = pg_restore_args("host='h'", &opts(&["x.dump", "t", "--jobs", "2"]));
        assert_eq!(
            a,
            args(&[
                "--dbname=host='h'",
                "--no-password",
                "--no-owner",
                "--no-privileges",
                "--jobs=2",
                "x.dump"
            ])
        );
        let a = pg_restore_args("host='h'", &opts(&["x.dump", "t", "--keep-owners"]));
        assert!(!a.iter().any(|x| x.starts_with("--no-owner")), "{a:?}");
    }

    const RESTORE_TAIL: &str = "pg_restore: error: could not execute query: ERROR:  unrecognized configuration parameter \"transaction_timeout\"\n\
         Command was: SET transaction_timeout = 0;\n\
         pg_restore: error: could not execute query: ERROR:  extension \"aws_commons\" is not available\n\
         Command was: CREATE EXTENSION IF NOT EXISTS aws_commons WITH SCHEMA public;\n\
         pg_restore: warning: errors ignored on restore: 2\n";

    #[test]
    fn a_newer_dumps_settings_are_harmless_and_the_rest_are_listed() {
        let errs = restore_errors(RESTORE_TAIL);
        assert_eq!(errs.len(), 2);
        assert!(errs[0].harmless && !errs[1].harmless);
        assert_eq!(
            errs[0].command.as_deref(),
            Some("SET transaction_timeout = 0;")
        );
        let done = |errors, reported| Done {
            database: "ledger".into(),
            server: "localhost:5432".into(),
            errors: Some(errors),
            reported,
            notes: Vec::new(),
        };
        assert_eq!(done(2, errs.clone()).real_errors(), 1);
        assert_eq!(done(1, vec![errs[0].clone()]).real_errors(), 0);
        let r = render(&done(2, errs.clone()));
        assert!(
            r.contains("with 1 error(s)") && r.contains("aws_commons"),
            "{r}"
        );
        assert!(
            r.contains("in: CREATE EXTENSION")
                && r.contains("SET of a setting this server doesn't have: ignored"),
            "{r}"
        );
        let r = render(&done(1, vec![errs[0].clone()]));
        assert_eq!(
            r,
            "restored into ledger on localhost:5432 (a newer pg_dump's SET of a setting this server doesn't have: ignored)"
        );
        // --jobs: every worker prints the harmless SET, the count holds one.
        let three = vec![errs[0].clone(), errs[0].clone(), errs[0].clone()];
        assert_eq!(done(1, three.clone()).real_errors(), 0);
        assert!(render(&done(1, three)).starts_with("restored into ledger on localhost:5432 ("));
        // A real one among the workers' harmless ones, against the
        // leader's count of one: still real.
        let mixed = vec![errs[0].clone(), errs[0].clone(), errs[1].clone()];
        assert_eq!(done(1, mixed).real_errors(), 1);
        // Counted but never seen (cut off): real, not assumed harmless.
        assert_eq!(done(3, vec![errs[0].clone()]).real_errors(), 2);
        let r = render(&done(0, Vec::new()));
        assert_eq!(r, "restored into ledger on localhost:5432");
    }

    #[test]
    fn a_database_that_wasnt_empty_says_so_and_a_long_list_is_cut() {
        let exists = |i: usize| RestoreError {
            message: format!("could not execute query: ERROR:  relation \"t{i}\" already exists"),
            command: Some(format!("CREATE TABLE public.t{i} (id int);")),
            harmless: false,
        };
        let reported: Vec<RestoreError> = (0..30).map(exists).collect();
        let d = Done {
            database: "ledger".into(),
            server: "localhost:5432".into(),
            errors: Some(30),
            reported,
            notes: Vec::new(),
        };
        let r = render(&d);
        assert!(
            r.contains("with 30 error(s)")
                && r.contains("the database wasn't empty. --clean replaces it"),
            "{r}"
        );
        assert!(r.contains("… and 18 more"), "{r}");
        assert_eq!(r.matches("already exists").count(), LISTED + 1, "{r}");
    }

    #[test]
    fn an_unrecognized_parameter_outside_a_set_is_not_waved_through() {
        let errs = restore_errors(
            "pg_restore: error: could not execute query: ERROR:  unrecognized configuration parameter \"x\"\n\
             Command was: SELECT pg_catalog.set_config('x', '1', false);\n",
        );
        assert!(!errs[0].harmless);
    }

    #[test]
    fn an_unreadable_file_says_why_in_terms_of_what_to_do() {
        let v = Major::parse("16").unwrap();
        let f = Path::new("x");
        assert!(unreadable(
            f,
            b"",
            "pg_restore: error: input file appears to be a text format dump. Please use psql.",
            v
        )
        .contains("with psql yourself"));
        assert!(unreadable(
            f,
            b"PG",
            "pg_restore: error: unsupported version (1.16) in file header",
            v
        )
        .contains("newer pg_dump than your pg_restore 16"));
        let short =
            "pg_restore: error: input file does not appear to be a valid archive (too short?)";
        assert!(unreadable(f, b"\x00\x01", short, v).contains("custom, directory or tar"));
        assert!(
            unreadable(f, b"--", short, v).contains("with psql yourself"),
            "a plain dump starts with a comment"
        );
    }

    #[test]
    fn ignored_errors_are_counted() {
        assert_eq!(
            ignored_errors("pg_restore: error: could not execute query\npg_restore: warning: errors ignored on restore: 3\n"),
            Some(3)
        );
        assert_eq!(ignored_errors("pg_restore: error: connection failed"), None);
    }
}
