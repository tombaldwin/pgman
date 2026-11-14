//! `pgman dump --masked`: a copy of a database with what looks personal
//! replaced as it is read. One repeatable-read snapshot holds everything:
//! the catalog the plan is made from, pg_dump's archive of the schema,
//! sequences and large objects with no table rows, and each table's rows
//! read with `COPY` and masked here, by a key made for this copy alone —
//! so the key never reaches the server, and no NULLed column's values
//! leave it. See `docs/design/masked-dumps.md`, section 4.
//!
//! The pure parts are in [`crate::masking`]; this is the runner.

use crate::dump::{Done, Opts};
use crate::inventory::target::Resolved;
use crate::masking::archive::{self, Header, Manifest, TableEntry};
use crate::masking::classify::{self, Plan, Rule};
use crate::masking::copytext;
use crate::masking::transform::Masker;
use crate::pgtool::{self, Major};
use futures::StreamExt;
use std::io::Write;
use std::path::{Path, PathBuf};

/// pg_dump options a masked dump passes on: ones that leave things out.
/// Anything that could bring rows back (`--data-only`, `--section`,
/// `--table`, large objects, `--inserts`) is pgman's to refuse.
const MASKED_PASSTHROUGH: &[&str] = &[
    "--exclude-table",
    "--exclude-table-and-children",
    "--exclude-table-data",
    "--exclude-table-data-and-children",
    "--exclude-schema",
    "--exclude-extension",
    "-T",
    "-N",
];

/// Why `extra` can't go with a masked dump, if it can't. Pure.
pub fn passthrough_problem(extra: &[String]) -> Option<String> {
    let mut value_next = false;
    for a in extra {
        if std::mem::take(&mut value_next) {
            continue;
        }
        let name = a.split_once('=').map(|(n, _)| n).unwrap_or(a);
        if !MASKED_PASSTHROUGH.contains(&name) {
            return Some(format!(
                "{a} doesn't go with a masked dump: after -- it takes only what leaves things out \
                 (--exclude-table, --exclude-schema, --exclude-table-data)"
            ));
        }
        value_next = !a.contains('=');
    }
    None
}

/// What the `--exclude-*` options after `--` leave out, as pg_dump reads
/// them: patterns of tables (with their children, for the
/// `-and-children` forms), of tables whose rows only are left out, and
/// of schemas.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Excludes {
    tables: Vec<(String, bool)>,
    rows: Vec<(String, bool)>,
    schemas: Vec<String>,
}

impl Excludes {
    /// From `extra`, already checked by [`passthrough_problem`]. Pure.
    pub fn parse(extra: &[String]) -> Excludes {
        let mut out = Excludes::default();
        let mut it = extra.iter();
        while let Some(a) = it.next() {
            let (name, value) = match a.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (a.as_str(), None),
            };
            let Some(value) = value.or_else(|| it.next().cloned()) else {
                break;
            };
            match name {
                "--exclude-table" | "-T" => out.tables.push((value, false)),
                "--exclude-table-and-children" => out.tables.push((value, true)),
                "--exclude-table-data" => out.rows.push((value, false)),
                "--exclude-table-data-and-children" => out.rows.push((value, true)),
                "--exclude-schema" | "-N" => out.schemas.push(value),
                _ => {}
            }
        }
        out
    }

    /// Whether `schema.name` is left out by `patterns`, itself or (where
    /// a pattern says so) as a child of a table in `ancestors`. Pure.
    fn hit(
        patterns: &[(String, bool)],
        schema: &str,
        name: &str,
        ancestors: &[(String, String)],
    ) -> bool {
        patterns.iter().any(|(p, children)| {
            pattern_matches(p, schema, name)
                || (*children && ancestors.iter().any(|(s, n)| pattern_matches(p, s, n)))
        })
    }

    /// Whether the table's rows are left out: its schema, the table, or
    /// its rows excluded. `ancestors` are what it is a partition or child
    /// of, nearest first. Pure.
    pub fn rows_of(&self, schema: &str, name: &str, ancestors: &[(String, String)]) -> bool {
        self.schemas.iter().any(|p| part_matches(p, schema))
            || Excludes::hit(&self.tables, schema, name, ancestors)
            || Excludes::hit(&self.rows, schema, name, ancestors)
    }

    /// Whether a sequence is left out: by its schema or as a table
    /// pattern (pg_dump's -T leaves out sequences too), or with the
    /// table that owns it, `owner`. Pure.
    pub fn sequence(&self, schema: &str, name: &str, owner: Option<(&str, &str)>) -> bool {
        self.schemas.iter().any(|p| part_matches(p, schema))
            || Excludes::hit(&self.tables, schema, name, &[])
            || owner.is_some_and(|(s, t)| {
                self.schemas.iter().any(|p| part_matches(p, s))
                    || Excludes::hit(&self.tables, s, t, &[])
            })
    }
}

/// Whether pg_dump's pattern `pattern` (`name` or `schema.name`; `*` and
/// `?` wildcards; double quotes keep case and dots) matches. An
/// unqualified pattern matches the name in any schema. Pure.
pub fn pattern_matches(pattern: &str, schema: &str, name: &str) -> bool {
    let parts = split_pattern(pattern);
    match parts.as_slice() {
        [n] => glob(n, name),
        [s, n] => glob(s, schema) && glob(n, name),
        // database.schema.name: the database is this one.
        [_, s, n] => glob(s, schema) && glob(n, name),
        _ => false,
    }
}

fn part_matches(pattern: &str, value: &str) -> bool {
    matches!(split_pattern(pattern).as_slice(), [p] if glob(p, value))
}

/// A pattern's dot-separated parts, each as (char, literal) pairs:
/// unquoted letters folded to lower case, `*`/`?` wildcards unless
/// quoted. Pure.
fn split_pattern(pattern: &str) -> Vec<Vec<(char, bool)>> {
    let mut parts = vec![Vec::new()];
    let mut quoted = false;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                parts.last_mut().unwrap().push(('"', true));
            }
            '"' => quoted = !quoted,
            '.' if !quoted => parts.push(Vec::new()),
            c if quoted => parts.last_mut().unwrap().push((c, true)),
            c => parts
                .last_mut()
                .unwrap()
                .extend(c.to_lowercase().map(|l| (l, false))),
        }
    }
    parts
}

fn glob(p: &[(char, bool)], s: &str) -> bool {
    let s: Vec<char> = s.chars().collect();
    fn go(p: &[(char, bool)], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(('*', false)) => (0..=s.len()).any(|i| go(&p[1..], &s[i..])),
            Some(('?', false)) => !s.is_empty() && go(&p[1..], &s[1..]),
            Some((c, _)) => s.first() == Some(c) && go(&p[1..], &s[1..]),
        }
    }
    go(p, &s)
}

/// Where a masked dump of `label` taken at `unix_secs` goes, given FILE
/// (`out`): a directory to put it in, or the archive's own name (ending
/// `.pgman-dump`). Pure.
pub fn paths(
    label: &str,
    out: Option<&Path>,
    out_is_dir: bool,
    unix_secs: u64,
    pid: u32,
) -> Result<crate::dump::Paths, String> {
    let base = crate::dump::slug(label);
    let (file, latest) = match out {
        Some(p) if !out_is_dir => {
            let text = p.to_string_lossy();
            if text.ends_with('/') || (!p.exists() && p.extension().is_none()) {
                return Err(format!(
                    "{}: no such directory; create it, or name the dump …{}",
                    p.display(),
                    archive::EXTENSION
                ));
            }
            if !text.ends_with(archive::EXTENSION) {
                return Err(format!(
                    "{}: a masked or encrypted dump is a directory named …{}; name one, or a \
                     directory to put it in",
                    p.display(),
                    archive::EXTENSION
                ));
            }
            (p.to_path_buf(), None)
        }
        dir => {
            let dir = dir.map(Path::to_path_buf).unwrap_or_default();
            (
                dir.join(format!(
                    "{base}-{}{}",
                    crate::dump::stamp(unix_secs),
                    archive::EXTENSION
                )),
                Some(dir.join(format!("{base}-latest{}", archive::EXTENSION))),
            )
        }
    };
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let partial = file.with_file_name(format!(".{name}.partial-{pid}"));
    Ok(crate::dump::Paths {
        file,
        latest,
        partial,
    })
}

/// The row's key for a date shift: its primary key's values, as read.
/// Pure.
pub fn row_key(row: &copytext::Row, pk: &[usize]) -> Vec<u8> {
    let mut k = Vec::new();
    for &i in pk {
        if let Some(Some(v)) = row.get(i) {
            k.extend_from_slice(v);
        }
        k.push(0);
    }
    k
}

/// One row masked: each field through its column's rule. Pure but for
/// the key.
pub fn mask_row(
    masker: &Masker,
    plan: &classify::TablePlan,
    row: &copytext::Row,
    pk: &[usize],
) -> copytext::Row {
    let key = row_key(row, pk);
    plan.columns
        .iter()
        .enumerate()
        .map(|(i, d)| masker.apply(d, row.get(i).and_then(|v| v.as_deref()), &key))
        .collect()
}

/// A file made new, readable by its owner alone. Writes it.
fn private_file(path: &Path) -> Result<std::fs::File, String> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn private_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|e| format!("{}: {e}", path.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// A fresh masking key, from the operating system.
fn fresh_key() -> Result<[u8; 32], String> {
    use ring::rand::SecureRandom;
    let mut k = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut k)
        .map_err(|_| "could not make a masking key: no randomness from the system".to_string())?;
    Ok(k)
}

/// The plan for `resolved`'s database, read inside the dump's snapshot.
fn refuse_plan(plan: &Plan) -> Result<(), String> {
    if plan.problems.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the masking plan can't go as it is:\n  {}\n  pgman masking show <target> lists every \
         column's rule",
        plan.problems.join("\n  ")
    ))
}

/// Take a masked dump of `resolved` as `opts` says, with the rules found
/// from `cwd`. `copy` is its id in the journal.
pub async fn run(
    opts: &Opts,
    resolved: Resolved,
    copy: &str,
    cwd: &Path,
    agent: bool,
    unattended: bool,
    say: &(dyn Fn(String) + Send + Sync),
) -> Result<Done, String> {
    if opts.format != crate::dump::Format::Custom {
        return Err("a masked dump is pgman's own archive: --format doesn't apply".into());
    }
    if let Some(why) = passthrough_problem(&opts.extra) {
        return Err(why);
    }
    let (tool, found_in) = pgtool::find_tool_from("pg_dump", opts.pg_bin.as_deref())?;
    let have = pgtool::tool_version(&tool)?;
    let rules = {
        let (cwd, db, file) = (
            cwd.to_path_buf(),
            resolved.database.clone(),
            opts.masking.clone(),
        );
        tokio::task::spawn_blocking(move || crate::masking::rules::find(&cwd, &db, file.as_deref()))
            .await
            .map_err(|e| format!("rules task failed: {e}"))??
    };
    let rules_digest = match &rules.file {
        Some(f) => archive::sha256_file(f)?,
        None => "defaults".to_string(),
    };
    let mut unconfirmed = None;
    if let Some(f) = &rules.file {
        if agent {
            if let Some(why) = agent_rules_problem(f, &rules_digest) {
                return Err(why);
            }
        } else {
            let (r, d) = (rules.clone(), rules_digest.clone());
            unconfirmed =
                tokio::task::spawn_blocking(move || confirm_rules_cli(&r, &d, unattended))
                    .await
                    .map_err(|e| format!("prompt task failed: {e}"))??;
        }
    }
    say(format!(
        "{} — {}, masked by {}",
        resolved.label,
        resolved.describe(),
        rules
            .file
            .as_ref()
            .map(|f| format!("the defaults and {}", f.display()))
            .unwrap_or_else(|| "the defaults (no rules file)".to_string())
    ));
    let now = crate::journal::now();
    let out_is_dir = opts
        .out
        .as_deref()
        .is_some_and(|p| p.is_dir() && !p.to_string_lossy().ends_with(archive::EXTENSION));
    let label = match &resolved.environment {
        Some(env) => format!("{env}/{}", resolved.database),
        None => resolved.label.clone(),
    };
    let p = paths(
        &label,
        opts.out.as_deref(),
        out_is_dir,
        now,
        std::process::id(),
    )?;
    if let Some(dir) = p.file.parent().filter(|d| !d.as_os_str().is_empty()) {
        if !dir.is_dir() {
            return Err(format!(
                "{} doesn't exist; create it, or name a directory that does",
                dir.display()
            ));
        }
    }
    if std::fs::symlink_metadata(&p.file).is_ok_and(|m| !m.is_dir()) {
        return Err(format!(
            "{} is a file; a masked dump is a directory. Name another",
            p.file.display()
        ));
    }

    let opened = pgtool::open(&resolved).await?;
    let client = crate::conn::connect_via(opened.dsn.clone(), opened.via, 10_000)
        .await
        .map_err(|e| pgtool::explain_connect(e, &resolved))?;
    let facts = crate::query::server::facts(&client).await?;
    let server = Major::from_num(facts.version_num)
        .ok_or_else(|| format!("unreadable server_version_num {}", facts.version_num))?;
    if let Some(why) = pgtool::version_refusal("pg_dump", have, server) {
        return Err(why);
    }
    let unreadable = crate::query::server::unreadable(&client, false).await?;
    let large_objects = if rules.large_objects_kept {
        crate::query::server::unreadable_large_objects(&client).await?
    } else {
        0
    };
    let pre = crate::dump::preflight(&facts.user, &unreadable, large_objects, opts)?;
    let skipped: Vec<String> = if opts.skip_unreadable {
        unreadable
            .iter()
            .filter(|u| u.skippable())
            .map(|u| format!("{}.{}", u.schema, u.name))
            .collect()
    } else {
        Vec::new()
    };
    say(format!(
        "PostgreSQL {}{}, as {}, with pg_dump {have} ({found_in}: {})",
        facts.version,
        if facts.in_recovery { " (standby)" } else { "" },
        facts.user,
        tool.display()
    ));

    let snapshot = crate::query::masking::begin_snapshot(&client).await?;
    let catalog = crate::query::masking::catalog(&client).await?;
    let plan = classify::plan(&catalog, &rules);
    refuse_plan(&plan)?;
    // What the -- --exclude-* options leave out, pg_dump leaves out of
    // the schema and pgman out of the rows and sequences.
    let excludes = Excludes::parse(&opts.extra);
    let left_out = |q: &str| -> bool {
        let Some(t) = catalog.table(q) else {
            return false;
        };
        let mut ancestors = Vec::new();
        let mut at = t.parent.clone();
        while let Some(p) = at.take().filter(|_| ancestors.len() < 16) {
            if let Some(pt) = catalog.table(&p) {
                ancestors.push((pt.schema.clone(), pt.name.clone()));
                at = pt.parent.clone();
            }
        }
        excludes.rows_of(&t.schema, &t.name, &ancestors)
    };
    let copied: Vec<&classify::TablePlan> = plan
        .tables
        .iter()
        .filter(|t| !t.drop_rows && !skipped.contains(&t.table) && !left_out(&t.table))
        .collect();
    let to_lock: Vec<(String, String)> = copied
        .iter()
        .filter_map(|t| {
            t.table
                .split_once('.')
                .map(|(s, n)| (s.to_string(), n.to_string()))
        })
        .collect();
    crate::query::masking::lock(&client, &to_lock).await?;
    let owners = crate::query::masking::sequence_owners(&client).await?;
    let sequences: Vec<_> = crate::query::masking::sequences(&client)
        .await?
        .into_iter()
        .filter(|(schema, name, _, _)| {
            let owner = owners
                .iter()
                .find(|o| &o.0 == schema && &o.1 == name)
                .map(|o| (o.2.as_str(), o.3.as_str()));
            !excludes.sequence(schema, name, owner)
        })
        .collect();

    let mut header = Header {
        format: archive::FORMAT,
        pgman: env!("CARGO_PKG_VERSION").to_string(),
        copy: copy.to_string(),
        label: label.clone(),
        env: resolved.environment.clone(),
        database: resolved.database.clone(),
        created: now,
        masked: true,
        protected: resolved.protected,
        seal: None,
    };
    // Encrypted before a byte is written: the seal is made first, a KMS
    // wrap bound to the header's fields.
    let to = match opts.encrypt.clone() {
        None => None,
        Some((enc, scope)) => {
            let context = header.context();
            let sealed = tokio::task::spawn_blocking(move || {
                crate::crypt::seal(&enc, &scope, &context, &crate::aws_cli::run_with_input)
            })
            .await
            .map_err(|e| format!("encryption task failed: {e}"))??;
            sealed.map(|(seal, to)| {
                header.seal = Some(seal);
                to
            })
        }
    };

    crate::dump::create_partial(&p.partial, crate::dump::Format::Directory)?;
    let made = crate::dump::identity(&p.partial);
    // Ctrl-C stops it at the command line, between chunks of rows. Not
    // under MCP: listening for it there would take it from the server
    // for good.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = (!agent).then(|| {
        let stop = stop.clone();
        tokio::spawn(async move {
            pgtool::interrupted().await;
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    });
    let result = write_archive(ArchiveRun {
        dir: &p.partial,
        tool: &tool,
        have,
        opened: &opened,
        client: &client,
        snapshot: &snapshot,
        excludes: &pre.args,
        opts,
        plan: &plan,
        copied: &copied,
        stop: &stop,
        say,
        to: to.as_ref(),
    })
    .await;
    if let Some(w) = watcher {
        w.abort();
    }
    if !result.as_ref().is_err_and(|e| e == pgtool::STOPPED) {
        let _ = crate::query::masking::end(&client).await;
    }
    // Stopped mid-COPY: closing the connection ends the snapshot.
    drop(client);
    drop(opened);
    let tables = match result {
        Ok(t) => t,
        Err(e) => {
            crate::dump::remove_partial(&p.partial);
            return Err(format!("{e}\n  nothing written"));
        }
    };
    let manifest = Manifest {
        header: header.clone(),
        schema_sha256: archive::sha256_file(&p.partial.join(archive::SCHEMA))?,
        tables,
        large_objects: rules.large_objects_kept,
        rules: rules_digest.clone(),
        key: "fresh".to_string(),
        words: crate::masking::transform::WORDS_VERSION,
        sequences,
        raw: None,
    };
    let written = (|| -> Result<(), String> {
        // In a work tree (an agent's dumps land in the project), git
        // leaves it out: `git add -A` doesn't push a copy of production.
        private_file(&p.partial.join(".gitignore"))?
            .write_all(b"# A pgman dump: never committed.\n*\n")
            .map_err(|e| format!(".gitignore: {e}"))?;
        let mut h = private_file(&p.partial.join(archive::HEADER))?;
        h.write_all(
            serde_json::to_string_pretty(&header)
                .unwrap_or_default()
                .as_bytes(),
        )
        .map_err(|e| format!("header: {e}"))?;
        let mut m = crate::crypt::Sink::new(
            private_file(&p.partial.join(archive::MANIFEST))?,
            to.as_ref(),
        )
        .map_err(|e| format!("manifest: {e}"))?;
        m.write_all(
            serde_json::to_string_pretty(&manifest)
                .unwrap_or_default()
                .as_bytes(),
        )
        .and_then(|_| m.finish())
        .map_err(|e| format!("manifest: {e}"))
    })();
    if let Err(e) = written {
        crate::dump::remove_partial(&p.partial);
        return Err(format!("{e}\n  nothing written"));
    }
    if crate::dump::identity(&p.partial) != made {
        return Err(format!(
            "{} was replaced while the dump ran; it isn't moved into place. Check what is there",
            p.partial.display()
        ));
    }
    let replaced = std::fs::symlink_metadata(&p.file).is_ok();
    crate::dump::commit(&p.partial, &p.file).map_err(|e| {
        format!(
            "{e}\n  the finished dump is kept at {}",
            p.partial.display()
        )
    })?;
    let mut notes = archive::summary(&plan);
    if let Some(seal) = &header.seal {
        notes.push(seal.describe());
    }
    notes.push(format!(
        "every masked password is {}; a fresh key for this copy, so its pseudonyms match no other",
        crate::masking::transform::TEST_PASSWORD
    ));
    notes.extend(pre.notes);
    // A file used without the operator's confirmation (no terminal to
    // ask at) says so in the notes, and the journal records it.
    let rules_unconfirmed = unconfirmed.is_some();
    notes.extend(unconfirmed);
    let mut latest = None;
    if let Some(l) = &p.latest {
        match crate::dump::point_latest(l, &p.file) {
            Ok(()) => latest = Some(l.clone()),
            Err(e) => notes.push(e),
        }
    }
    Ok(Done {
        bytes: pgtool::path_size(&p.file),
        file: p.file,
        latest,
        source: resolved.describe(),
        notes,
        replaced,
        copy: None,
        rules_confirmed: rules.file.as_ref().map(|_| !rules_unconfirmed),
        rules: Some(rules_digest),
    })
}

struct ArchiveRun<'a> {
    dir: &'a Path,
    tool: &'a Path,
    have: Major,
    opened: &'a pgtool::Opened,
    client: &'a tokio_postgres::Client,
    snapshot: &'a str,
    excludes: &'a [String],
    opts: &'a Opts,
    plan: &'a Plan,
    copied: &'a [&'a classify::TablePlan],
    /// Set by Ctrl-C.
    stop: &'a std::sync::atomic::AtomicBool,
    say: &'a (dyn Fn(String) + Send + Sync),
    /// Every file is encrypted to it, when there is one.
    to: Option<&'a age::x25519::Recipient>,
}

/// pg_dump's arguments for a masked archive's schema: no rows, no
/// comments, no subscriptions (their connection strings can hold a
/// password; pg_dump 10 on), large objects only when kept. Pure.
fn schema_args(
    conninfo: &str,
    snapshot: &str,
    have: Major,
    large_objects: bool,
    excludes: &[String],
    extra: &[String],
) -> Vec<String> {
    let mut args = vec![
        format!("--dbname={conninfo}"),
        "--format=c".to_string(),
        format!("--snapshot={snapshot}"),
        "--exclude-table-data=*.*".to_string(),
        "--no-comments".to_string(),
        "--no-password".to_string(),
    ];
    if Major::from_num(100_000).is_some_and(|ten| have >= ten) {
        args.push("--no-subscriptions".to_string());
    }
    if !large_objects {
        args.push("--no-blobs".to_string());
    }
    args.extend(excludes.iter().cloned());
    args.extend(extra.iter().cloned());
    args
}

/// pg_dump's no-rows archive, then each table's masked rows, into `dir`.
async fn write_archive(r: ArchiveRun<'_>) -> Result<Vec<TableEntry>, String> {
    let conninfo = pgtool::conninfo(&r.opened.dsn, r.opened.via, "pgman-dump", r.have);
    let args = schema_args(
        &conninfo,
        r.snapshot,
        r.have,
        r.plan.large_objects,
        r.excludes,
        &r.opts.extra,
    );
    let schema = private_file(&r.dir.join(archive::SCHEMA))?;
    let mut cmd = pgtool::private_command(r.tool, &args);
    // Encrypted, pg_dump's archive goes through a pipe into the file: no
    // plaintext of it is ever on disk.
    let copier = match r.to {
        None => {
            cmd.stdout(schema);
            None
        }
        Some(to) => {
            let (mut from, into) = std::io::pipe().map_err(|e| format!("a pipe: {e}"))?;
            cmd.stdout(into);
            let mut sink = crate::crypt::Sink::new(schema, Some(to))
                .map_err(|e| format!("{}: {e}", archive::SCHEMA))?;
            Some(std::thread::spawn(move || -> std::io::Result<()> {
                std::io::copy(&mut from, &mut sink)?;
                sink.finish()
            }))
        }
    };
    pgtool::set_child_env(&mut cmd, r.opened.password());
    let ran = pgtool::run_tool_cli(cmd, r.opts.verbose).await;
    if let Some(c) = copier {
        c.join()
            .map_err(|_| format!("encrypting {} failed", archive::SCHEMA))?
            .map_err(|e| format!("{}: {e}", archive::SCHEMA))?;
    }
    let (status, tail) = ran.map_err(|e| format!("pg_dump {e}"))?;
    if !status.success() {
        return Err(crate::dump::explain_failure(&tail.text(), false, None));
    }
    let data = r.dir.join("data");
    private_dir(&data)?;
    let key = fresh_key()?;
    let masker = Masker::new(&key);
    let mut entries = Vec::new();
    for (n, t) in r.copied.iter().enumerate() {
        let Some((schema, name)) = t.table.split_once('.') else {
            continue;
        };
        let cols: Vec<(String, bool)> = t
            .columns
            .iter()
            .map(|d| (d.column.clone(), d.rule == Rule::Null))
            .collect();
        let pk: Vec<usize> = t
            .primary_key
            .iter()
            .filter_map(|k| t.columns.iter().position(|d| &d.column == k))
            .collect();
        let started = std::time::Instant::now();
        let file_name = archive::data_file(n);
        let path = data.join(&file_name);
        let sink = crate::crypt::Sink::new(private_file(&path)?, r.to)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut gz = flate2::write::GzEncoder::new(sink, flate2::Compression::fast());
        let stream = crate::query::masking::copy_out(
            r.client,
            &crate::query::masking::copy_out_sql(schema, name, &cols),
        )
        .await?;
        futures::pin_mut!(stream);
        let mut rest: Vec<u8> = Vec::new();
        let mut rows = 0u64;
        while let Some(chunk) = stream.next().await {
            if r.stop.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(pgtool::STOPPED.to_string());
            }
            let chunk = chunk.map_err(|e| format!("reading {}: {e}", t.table))?;
            rest.extend_from_slice(&chunk);
            let mut start = 0;
            while let Some(end) = rest[start..].iter().position(|b| *b == b'\n') {
                let line = &rest[start..start + end];
                let masked = mask_row(&masker, t, &copytext::decode(line), &pk);
                gz.write_all(&copytext::encode(&masked))
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                rows += 1;
                start += end + 1;
            }
            rest.drain(..start);
        }
        gz.finish()
            .and_then(crate::crypt::Sink::finish)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if r.opts.verbose {
            (r.say)(format!(
                "{}: {rows} rows in {:.1}s",
                t.table,
                started.elapsed().as_secs_f64()
            ));
        }
        entries.push(TableEntry {
            table: t.table.clone(),
            columns: t.columns.iter().map(|d| d.column.clone()).collect(),
            rows,
            file: file_name,
            sha256: archive::sha256_file(&path)?,
        });
    }
    Ok(entries)
}

pub const MASKING_USAGE: &str = "\
usage: pgman masking show <target> [--masking FILE]
       pgman masking check <target> [--masking FILE] [--strict]

What a masked dump of <target> does to each column, read from its
catalog alone (no rows): the built-in defaults, framework presets, and
.pgman/masking/<database>.toml found from here (or --masking FILE).

  show    every column's rule and why, NULLed ones first, each with the
          line to put in the rules file to keep it
  check   whether the plan can go as it is: a rule that can't be stored
          (a fixed value too long, a lossy rule on a unique column, a
          NOT NULL type with no empty value), a rule naming something the
          database doesn't have. Exit 1 when not. --strict also wants
          every column the defaults mask decided in the rules file.
          Runs against an empty migrated database too, as in CI.";

/// Parsed `pgman masking` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskingCmd {
    pub check: bool,
    pub target: String,
    pub masking: Option<PathBuf>,
    pub strict: bool,
}

/// Parse the arguments after `pgman masking`. Pure.
pub fn parse_masking(args: &[String]) -> Result<MaskingCmd, String> {
    let mut it = args.iter();
    let check = match it.next().map(String::as_str) {
        Some("show") => false,
        Some("check") => true,
        Some(other) => {
            return Err(format!(
                "unknown masking command {other:?}\n\n{MASKING_USAGE}"
            ))
        }
        None => return Err(MASKING_USAGE.to_string()),
    };
    let (mut target, mut masking, mut strict) = (None, None, false);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--masking" => {
                masking = Some(PathBuf::from(it.next().ok_or("--masking needs a file")?))
            }
            "--strict" if check => strict = true,
            o if o.starts_with('-') => {
                return Err(format!("unknown option {o:?}\n\n{MASKING_USAGE}"))
            }
            o if target.is_none() => target = Some(o.to_string()),
            o => return Err(format!("unexpected argument {o:?}\n\n{MASKING_USAGE}")),
        }
    }
    Ok(MaskingCmd {
        check,
        target: target
            .ok_or_else(|| format!("which database? name a target\n\n{MASKING_USAGE}"))?,
        masking,
        strict,
    })
}

/// `pgman masking show|check`: the plan, from the catalog. What to print,
/// and whether it passed.
pub async fn masking_cli(cmd: &MaskingCmd) -> Result<(String, bool), String> {
    let text = cmd.target.clone();
    let resolved = tokio::task::spawn_blocking(move || {
        crate::inventory::target::resolve(&text, crate::inventory::target::Want::PreferReplica)
    })
    .await
    .map_err(|e| format!("resolve task failed: {e}"))??;
    let cwd = std::env::current_dir().map_err(|e| format!("the current directory: {e}"))?;
    let rules = crate::masking::rules::find(&cwd, &resolved.database, cmd.masking.as_deref())?;
    let opened = pgtool::open(&resolved).await?;
    let client = crate::conn::connect_via(opened.dsn.clone(), opened.via, 10_000)
        .await
        .map_err(|e| pgtool::explain_connect(e, &resolved))?;
    let catalog = crate::query::masking::catalog(&client).await?;
    let plan = classify::plan(&catalog, &rules);
    let file = rules.file.as_ref().map(|f| f.display().to_string());
    Ok(if cmd.check {
        crate::masking::report::check(&plan, cmd.strict)
    } else {
        (
            crate::masking::report::show(&plan, file.as_deref()),
            plan.problems.is_empty(),
        )
    })
}

/// Load a checked archive's rows into a restored database (its tables
/// made, their keys not yet), then set its sequences. What to note.
pub async fn load(
    client: &tokio_postgres::Client,
    dir: &Path,
    m: &OpenArchive,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Vec<String>, String> {
    use std::io::Read;
    let mut notes = Vec::new();
    for t in &m.tables {
        let path = dir.join("data").join(&t.file);
        let f = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Checked as it is read: the file the manifest vouched for when
        // the restore was planned, not one put in its place since.
        let mut hashing = Hashing {
            inner: f,
            ctx: ring::digest::Context::new(&ring::digest::SHA256),
        };
        let r = crate::crypt::source(&mut hashing, m.key.as_ref())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut gz = flate2::read::GzDecoder::new(r);
        // Read as COPY takes them, a chunk at a time: a table's rows are
        // never all in memory, however big (or however a tampered file
        // decompresses).
        let mut done = false;
        let chunks = std::iter::from_fn(|| {
            if done {
                return None;
            }
            if stop.load(std::sync::atomic::Ordering::SeqCst) {
                done = true;
                return Some(Err(pgtool::STOPPED.to_string()));
            }
            let mut buf = vec![0u8; 1 << 16];
            match gz.read(&mut buf) {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some(Ok(buf))
                }
                Err(e) => {
                    done = true;
                    Some(Err(format!("{}: {e}", path.display())))
                }
            }
        });
        let sql = crate::query::masking::copy_in_sql(&t.table, &t.columns);
        let rows = crate::query::masking::copy_in(client, &sql, chunks)
            .await
            .map_err(|e| format!("{}: {}", t.table, crate::restore::redact(&e)))?;
        drop(gz);
        hashing
            .finish_against(&t.sha256)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if rows != t.rows {
            notes.push(format!(
                "{}: {rows} rows loaded, {} dumped",
                t.table, t.rows
            ));
        }
    }
    for (schema, name, value, called) in &m.sequences {
        crate::query::masking::set_sequence(client, schema, name, *value, *called).await?;
    }
    Ok(notes)
}

/// Where confirmed rules files' digests are kept.
fn confirmed_file() -> PathBuf {
    crate::journal::dir().join("masking.confirmed")
}

/// Whether `digest` (a rules file's sha256) is one the operator has
/// confirmed, at a terminal or over MCP. Reads the data directory.
pub fn confirmed(digest: &str, file: &Path) -> bool {
    confirmed_in(
        &std::fs::read_to_string(confirmed_file()).unwrap_or_default(),
        digest,
        file,
    )
}

/// Whether `text` (the confirmed list: `digest  path` lines) holds
/// `digest` for `file` — the file too, since its name says which
/// database it is for: the same rules confirmed for one database aren't
/// for another. Reads the filesystem, to compare paths.
pub fn confirmed_in(text: &str, digest: &str, file: &Path) -> bool {
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let want = real(file);
    text.lines().any(|l| {
        l.split_once("  ")
            .is_some_and(|(d, p)| d == digest && real(Path::new(p)) == want)
    })
}

/// The confirmed list's lines for `digest` of `file`, confirmed `how`
/// ("at a terminal", "over MCP") at `at` (unix seconds): a comment
/// saying when and how, then the `digest  path` line [`confirmed_in`]
/// reads. Pure.
pub fn confirmation_lines(digest: &str, file: &Path, how: &str, at: u64) -> String {
    format!(
        "# {} confirmed {how}\n{digest}  {}\n",
        crate::app::format_unix_secs_utc(at),
        file.display()
    )
}

/// Record `digest` of `file` as confirmed `how`. Writes the data
/// directory.
pub fn confirm(digest: &str, file: &Path, how: &str) -> Result<(), String> {
    let path = confirmed_file();
    crate::util::ensure_private_dir(&crate::journal::dir()).map_err(|e| e.to_string())?;
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    text.push_str(&confirmation_lines(
        digest,
        &file,
        how,
        crate::journal::now(),
    ));
    crate::util::write_private(&path, &text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Where an agent's rules file stands.
#[derive(Debug, PartialEq, Eq)]
pub enum AgentRules {
    /// Committed as it is, and confirmed: used.
    Confirmed,
    /// Committed as it is, not yet confirmed (new, or changed since):
    /// the operator confirms it, over MCP or at a terminal, first.
    Unconfirmed,
    /// Not usable by an agent at all: why.
    Refused(String),
}

/// Where the rules file at `file` stands for an agent: it must be
/// committed, as it is, and confirmed — so an agent that edits it (to
/// keep a column) can't use the edit until the operator has seen it.
/// Reads git and the data directory.
pub fn agent_rules(file: &Path, digest: &str) -> AgentRules {
    let dir = file.parent().unwrap_or(Path::new("."));
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let git = |args: &[&str]| {
        crate::util::git()
            .args(args)
            .current_dir(dir)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    // Committed as it is: the file's blob in HEAD is the file's bytes.
    // Not `git status`, which runs the checkout's filters and fsmonitor.
    let committed = git(&["rev-parse", "--verify", "-q", &format!("HEAD:./{name}")]);
    let here = git(&["hash-object", "--no-filters", "--", &name]);
    if committed.is_none() || committed != here {
        return AgentRules::Refused(format!(
            "{} isn't committed as it is: an agent's dump uses committed rules only. Commit it; \
             the next dump asks the operator to confirm it",
            file.display()
        ));
    }
    if confirmed(digest, file) {
        AgentRules::Confirmed
    } else {
        AgentRules::Unconfirmed
    }
}

/// The commit that last changed `file`, abbreviated, if git says.
/// Reads git.
pub fn rules_commit(file: &Path) -> Option<String> {
    let out = crate::util::git()
        .args(["log", "-1", "--no-show-signature", "--format=%h", "--"])
        .arg(file)
        .current_dir(file.parent().unwrap_or(Path::new(".")))
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !h.is_empty()).then_some(h)
}

/// Why the rules file at `file` can't be used by an agent now, if it
/// can't: [`agent_rules`] short of confirmed. What a dump checks again
/// as it starts, after any confirmation. Reads git and the data
/// directory.
pub fn agent_rules_problem(file: &Path, digest: &str) -> Option<String> {
    match agent_rules(file, digest) {
        AgentRules::Confirmed => None,
        AgentRules::Unconfirmed => Some(format!(
            "{} is new or changed since the operator confirmed it: the dump asks them to confirm \
             it first (or run pgman dump <target> --masked at a terminal, which shows what it \
             keeps and asks)",
            file.display()
        )),
        AgentRules::Refused(why) => Some(why),
    }
}

/// At a terminal, a rules file not yet confirmed: what it loosens, and a
/// yes to use it (recorded, so an agent may then). Without a terminal it
/// is used only if it masks no less than the defaults, or the operator
/// set `unattended` for the environment; the result says so.
pub fn confirm_rules_cli(
    rules: &crate::masking::rules::RuleSet,
    digest: &str,
    unattended: bool,
) -> Result<Option<String>, String> {
    use std::io::IsTerminal;
    let Some(file) = &rules.file else {
        return Ok(None);
    };
    if confirmed(digest, file) {
        return Ok(None);
    }
    let loose = crate::masking::rules::loosening(rules);
    if !std::io::stdin().is_terminal() {
        return unconfirmed_without_terminal(&file.display().to_string(), &loose, unattended);
    }
    let sets = crate::masking::rules::overrides(rules);
    eprintln!(
        "pgman dump: {} is new or changed since you last confirmed it.{}{}",
        file.display(),
        if loose.is_empty() {
            " It masks no less than the defaults.".to_string()
        } else {
            format!(
                " What it keeps that the defaults would mask:\n  {}",
                loose.join("\n  ")
            )
        },
        if sets.is_empty() {
            String::new()
        } else {
            format!("\nWhat it sets itself:\n  {}", sets.join("\n  "))
        }
    );
    if !crate::pgtool::confirm_cli("use these rules? [y/N] ", "y")? {
        return Err("rules not confirmed; nothing dumped".into());
    }
    confirm(digest, file, "at a terminal")?;
    Ok(None)
}

/// A rules file nobody has confirmed, with no terminal to ask on: used
/// when it keeps nothing the defaults would mask (with a note), or when
/// the operator set `unattended` for the environment; otherwise refused,
/// since what it keeps is a raw copy of those columns. Pure.
pub fn unconfirmed_without_terminal(
    file: &str,
    loose: &[String],
    unattended: bool,
) -> Result<Option<String>, String> {
    if loose.is_empty() {
        return Ok(Some(format!(
            "{file} isn't confirmed yet: an agent's dump asks the operator to confirm it first"
        )));
    }
    if unattended {
        return Ok(Some(format!(
            "{file} isn't confirmed, and is used because this environment is unattended; it keeps:\n  {}",
            loose.join("\n  ")
        )));
    }
    Err(format!(
        "{file} is new or changed since it was confirmed, and keeps what the defaults would mask:\n  {}\n\
         a rules file like that is confirmed at a terminal first: run this dump at one. Nothing dumped",
        loose.join("\n  ")
    ))
}

/// Where an agent's masked dump goes: `file` (a `.pgman-dump` name or an
/// existing directory, inside `cwd`), or `cwd`. Reads the filesystem.
pub fn agent_out(cwd: &Path, file: Option<&str>) -> Result<PathBuf, String> {
    let Some(rel) = file else {
        return crate::mcp::dumps::inside(cwd, ".", true);
    };
    let p = crate::mcp::dumps::inside(cwd, rel, false)?;
    if p.is_dir() && !rel.ends_with(archive::EXTENSION) {
        return crate::mcp::dumps::inside(cwd, rel, true);
    }
    if !rel.ends_with(archive::EXTENSION) {
        return Err(format!(
            "{rel:?}: a masked dump is a directory named …{} (or name a directory to put it in)",
            archive::EXTENSION
        ));
    }
    Ok(p)
}

/// Whether `path` is a masked dump: a directory with pgman's header.
/// Reads the filesystem.
pub fn is_archive(path: &Path) -> bool {
    path.is_dir() && path.join(archive::HEADER).is_file() && path.join(archive::MANIFEST).is_file()
}

/// An archive checked and opened for a restore: its manifest, the key
/// its files open with, and its schema as pg_restore reads it — a
/// private copy in pgman's cache, checked as it was copied (decrypted,
/// when the archive is encrypted), removed when this is dropped. So
/// what pg_restore runs is what was checked, whatever happens to the
/// archive meanwhile.
pub struct OpenArchive {
    pub manifest: Manifest,
    pub key: Option<age::x25519::Identity>,
    pub schema: PathBuf,
    /// The manifest's fingerprint, from the bytes that were checked:
    /// what the journal recorded for the dump ([`crate::copies::print_of`]).
    pub print: String,
    temp: Option<PathBuf>,
}

impl std::ops::Deref for OpenArchive {
    type Target = Manifest;
    fn deref(&self) -> &Manifest {
        &self.manifest
    }
}

impl Drop for OpenArchive {
    fn drop(&mut self) {
        // Only ever a directory this pgman named.
        if let Some(t) = self.temp.as_ref().filter(|t| restore_temp(t)) {
            let _ = std::fs::remove_dir_all(t);
        }
    }
}

/// Whether `path` is a restore's temp directory as [`open_archive_with`]
/// names it: `restore-<pid>-<random hex>`, nothing else. Pure.
fn restore_temp(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("restore-"))
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(pid, tag)| {
            !pid.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && !tag.is_empty()
                && tag.bytes().all(|b| b.is_ascii_hexdigit())
        })
}

/// Whether process `pid` is running.
fn alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 delivers nothing; it only asks whether the
        // process exists.
        let r = unsafe { libc::kill(pid, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Remove what restores that never ended left in `cache` (a killed
/// process, a crash): their decrypted copies, plaintext. A running
/// restore's is left alone. Reads and removes.
pub fn sweep_restores(cache: &Path) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if !restore_temp(&path) {
            continue;
        }
        let pid = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("restore-"))
            .and_then(|r| r.split('-').next())
            .and_then(|p| p.parse::<i32>().ok());
        if pid.is_some_and(|p| p != std::process::id() as i32 && !alive(p)) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

impl OpenArchive {
    /// Whether its schema was decrypted to a private temp file.
    pub fn decrypted(&self) -> bool {
        self.key.is_some()
    }

    /// An encrypted raw dump: [`OpenArchive::schema`] is its whole
    /// pg_dump archive, restored as any other.
    pub fn is_raw(&self) -> bool {
        self.manifest.raw.is_some()
    }
}

/// Write `header` and an encrypted raw dump's manifest into `dir`, beside
/// its [`archive::RAW`]: the header plain, the manifest encrypted to `to`.
pub fn finish_raw_archive(
    dir: &Path,
    header: &Header,
    to: &age::x25519::Recipient,
) -> Result<(), String> {
    use std::io::Write;
    let manifest = Manifest {
        header: header.clone(),
        schema_sha256: String::new(),
        tables: Vec::new(),
        large_objects: true,
        rules: "raw".into(),
        key: "none".into(),
        words: 0,
        sequences: Vec::new(),
        raw: Some(archive::sha256_file(&dir.join(archive::RAW))?),
    };
    private_file(&dir.join(".gitignore"))?
        .write_all(b"# A pgman dump: never committed.\n*\n")
        .map_err(|e| format!(".gitignore: {e}"))?;
    private_file(&dir.join(archive::HEADER))?
        .write_all(
            serde_json::to_string_pretty(header)
                .unwrap_or_default()
                .as_bytes(),
        )
        .map_err(|e| format!("header: {e}"))?;
    let mut m = crate::crypt::Sink::new(private_file(&dir.join(archive::MANIFEST))?, Some(to))
        .map_err(|e| format!("manifest: {e}"))?;
    m.write_all(
        serde_json::to_string_pretty(&manifest)
            .unwrap_or_default()
            .as_bytes(),
    )
    .and_then(|_| m.finish())
    .map_err(|e| format!("manifest: {e}"))
}

/// The plain header of the archive at `dir`. Reads it.
pub fn read_header(dir: &Path) -> Result<Header, String> {
    std::fs::read_to_string(dir.join(archive::HEADER))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| format!("{}: no readable {}", dir.display(), archive::HEADER))
}

/// Where a KMS key of a dump of `env` is used: that environment's AWS
/// source, if this machine knows it. Reads the config.
fn kms_scope(env: Option<&str>) -> crate::aws_cli::AwsScope {
    let envs = crate::inventory::load().environments.unwrap_or_default();
    env.and_then(|n| envs.iter().find(|e| e.name == n))
        .map(|e| e.dump_settings().encrypt_scope)
        .unwrap_or_default()
}

/// Open the archive at `dir` for a restore: its key (the operator's, or
/// KMS's unwrap of its own), its manifest checked, and its schema where
/// pg_restore can read it. Blocking; KMS is a subprocess.
pub fn open_archive(dir: &Path) -> Result<OpenArchive, String> {
    let header = read_header(dir)?;
    let key = match &header.seal {
        None => None,
        Some(seal) => Some(crate::crypt::open(
            seal,
            &kms_scope(header.env.as_deref()),
            &header.context(),
            &crate::aws_cli::run_with_input,
        )?),
    };
    open_archive_with(dir, &header, key, &crate::util::cache_dir())
}

/// [`open_archive`] with its key found. Reads it.
fn open_archive_with(
    dir: &Path,
    header: &Header,
    key: Option<age::x25519::Identity>,
    cache: &Path,
) -> Result<OpenArchive, String> {
    let (manifest, print) = checked_manifest_with(dir, header, key.as_ref())?;
    // What pg_restore reads: the schema, or a raw dump's whole archive.
    let (inner, want) = match &manifest.raw {
        Some(sha) => (archive::RAW, sha.clone()),
        None => (archive::SCHEMA, manifest.schema_sha256.clone()),
    };
    let temp = restore_dir(cache)?;
    let out = temp.join(inner);
    let copied = copy_checked(&dir.join(inner), &out, key.as_ref(), &want);
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(e);
    }
    Ok(OpenArchive {
        manifest,
        key,
        schema: out,
        print,
        temp: Some(temp),
    })
}

/// A new private directory in `cache` for one restore, named by nothing
/// from the archive: what [`restore_temp`] recognises. Makes it.
fn restore_dir(cache: &Path) -> Result<PathBuf, String> {
    sweep_restores(cache);
    let tag: String = crate::journal::new_id()
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(12)
        .collect();
    let temp = cache.join(format!("restore-{}-{tag}", std::process::id()));
    crate::util::ensure_private_dir(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
    Ok(temp)
}

/// Reads through to `inner`, hashing every byte it reads.
struct Hashing<R> {
    inner: R,
    ctx: ring::digest::Context,
}

impl<R: std::io::Read> std::io::Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.ctx.update(&buf[..n]);
        Ok(n)
    }
}

impl<R: std::io::Read> Hashing<R> {
    /// Read what is left, then refuse unless every byte had the sha256
    /// `want`. Reads.
    fn finish_against(mut self, want: &str) -> Result<(), String> {
        std::io::copy(&mut self, &mut std::io::sink()).map_err(|e| e.to_string())?;
        let got: String = self
            .ctx
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if got == want {
            Ok(())
        } else {
            Err("changed since the restore checked it (its checksum differs)".to_string())
        }
    }
}

/// Copy `from` to a new private file `to`, decrypted with `key` when
/// given, and refuse unless the bytes read from `from` — all of them —
/// have the sha256 `want`. Returns nothing more: the copy is what was
/// checked. Reads and writes.
fn copy_checked(
    from: &Path,
    to: &Path,
    key: Option<&age::x25519::Identity>,
    want: &str,
) -> Result<(), String> {
    let name = from
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let file = std::fs::File::open(from).map_err(|e| format!("{name}: {e}"))?;
    let mut hashing = Hashing {
        inner: file,
        ctx: ring::digest::Context::new(&ring::digest::SHA256),
    };
    {
        let mut r = crate::crypt::source(&mut hashing, key).map_err(|e| format!("{name}: {e}"))?;
        let mut w = private_file(to)?;
        std::io::copy(&mut r, &mut w).map_err(|e| format!("copying {name}: {e}"))?;
    }
    // What decryption left unread isn't hashed, so the sha256 differs:
    // refused, as a changed file is.
    let got = hashing.ctx.finish();
    let got: String = got.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if got != want {
        return Err(format!(
            "{} isn't what was dumped (its checksum differs): the archive is damaged or was \
             changed; nothing restored",
            from.display()
        ));
    }
    Ok(())
}

/// A single-file dump copied, as it is, to a private file in pgman's
/// cache: what pg_restore reads, so a change to the dump after it was
/// looked at reaches nothing. Removed when dropped.
pub struct StagedDump {
    pub file: PathBuf,
    /// The sha256 of every byte copied: what the journal recorded.
    pub sha256: String,
    temp: PathBuf,
}

impl Drop for StagedDump {
    fn drop(&mut self) {
        if restore_temp(&self.temp) {
            let _ = std::fs::remove_dir_all(&self.temp);
        }
    }
}

/// Stage the dump at `file` in `cache` ([`StagedDump`]). Reads and
/// writes.
pub fn stage_dump(file: &Path, cache: &Path) -> Result<StagedDump, String> {
    use std::io::{Read, Write};
    if !file.is_file() {
        return Err(format!(
            "{}: not a single-file dump; restore it at a terminal",
            file.display()
        ));
    }
    let temp = restore_dir(cache)?;
    let out = temp.join("dump");
    let staged = (|| -> Result<String, String> {
        let mut r = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let mut w = private_file(&out)?;
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = r
                .read(&mut buf)
                .map_err(|e| format!("{}: {e}", file.display()))?;
            if n == 0 {
                break;
            }
            ctx.update(&buf[..n]);
            w.write_all(&buf[..n])
                .map_err(|e| format!("{}: {e}", out.display()))?;
        }
        Ok(ctx
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    })();
    match staged {
        Ok(sha256) => Ok(StagedDump {
            file: out,
            sha256,
            temp,
        }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp);
            Err(e)
        }
    }
}

/// The manifest of the plain archive at `dir`, checked: this pgman's
/// format, generated file names, and every file's checksum. Reads it all.
pub fn checked_manifest(dir: &Path) -> Result<Manifest, String> {
    let header = read_header(dir)?;
    checked_manifest_with(dir, &header, None).map(|(m, _)| m)
}

/// [`checked_manifest`], the manifest decrypted with `key` when the
/// archive is encrypted, and its fingerprint from the same bytes. Its
/// checksums are of the files as they are on disk, so they are checked
/// before anything is decrypted.
fn checked_manifest_with(
    dir: &Path,
    header: &Header,
    key: Option<&age::x25519::Identity>,
) -> Result<(Manifest, String), String> {
    use std::io::Read;
    let path = dir.join(archive::MANIFEST);
    let on_disk = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let print = crate::copies::print_of(&on_disk);
    let mut bytes = Vec::new();
    crate::crypt::source(on_disk.as_slice(), key)
        .and_then(|mut r| r.read_to_end(&mut bytes))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let m: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{}: not a pgman manifest ({e})", dir.display()))?;
    if let Some(why) = archive::manifest_problem(&m) {
        return Err(why);
    }
    // header.json is what the archive says it is at a glance (copies
    // lists it): the manifest's copy of it, which it must match — the
    // encrypted manifest's, for an encrypted archive, so a header edited
    // to name another key or source is caught.
    if *header != m.header {
        return Err(format!(
            "{}: {} disagrees with the manifest: the archive was changed; nothing restored",
            dir.display(),
            archive::HEADER
        ));
    }
    let check = |file: PathBuf, want: &str| -> Result<(), String> {
        let got = archive::sha256_file(&file)?;
        if got != want {
            return Err(format!(
                "{} isn't what was dumped (its checksum differs): the archive is damaged or was \
                 changed; nothing restored",
                file.display()
            ));
        }
        Ok(())
    };
    if let Some(sha) = &m.raw {
        check(dir.join(archive::RAW), sha)?;
        return Ok((m, print));
    }
    check(dir.join(archive::SCHEMA), &m.schema_sha256)?;
    for t in &m.tables {
        check(dir.join("data").join(&t.file), &t.sha256)?;
    }
    Ok((m, print))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archive_whose_header_disagrees_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("pgman-header-{}.pgman-dump", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join(archive::SCHEMA), b"schema").unwrap();
        let header = Header {
            format: archive::FORMAT,
            pgman: "0.4.0".into(),
            copy: "c".into(),
            label: "production/ledger".into(),
            env: Some("production".into()),
            database: "ledger".into(),
            created: 0,
            masked: true,
            protected: true,
            seal: None,
        };
        let m = Manifest {
            header: header.clone(),
            schema_sha256: archive::sha256(b"schema"),
            tables: vec![],
            large_objects: false,
            rules: "defaults".into(),
            key: "fresh".into(),
            words: 1,
            sequences: vec![],
            raw: None,
        };
        std::fs::write(
            dir.join(archive::MANIFEST),
            serde_json::to_string(&m).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join(archive::HEADER),
            serde_json::to_string(&header).unwrap(),
        )
        .unwrap();
        assert!(checked_manifest(&dir).is_ok());
        let mut changed = header;
        changed.masked = false;
        std::fs::write(
            dir.join(archive::HEADER),
            serde_json::to_string(&changed).unwrap(),
        )
        .unwrap();
        let e = checked_manifest(&dir).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(e.contains("disagrees with the manifest"), "{e}");
    }

    #[test]
    fn a_masked_schema_leaves_out_rows_comments_and_subscriptions() {
        let v = |n| Major::from_num(n).unwrap();
        let args = schema_args("c", "s", v(170_002), false, &[], &[]);
        for want in [
            "--exclude-table-data=*.*",
            "--no-comments",
            "--no-subscriptions",
            "--no-blobs",
        ] {
            assert!(args.iter().any(|a| a == want), "{want} in {args:?}");
        }
        assert!(
            !schema_args("c", "s", v(170_002), true, &[], &[]).contains(&"--no-blobs".to_string())
        );
        // pg_dump 9.6 has no such flag.
        assert!(!schema_args("c", "s", v(90_624), false, &[], &[])
            .contains(&"--no-subscriptions".to_string()));
    }

    /// The checkout's own config can't make pgman's git run anything: an
    /// agent can write `.git/config`.
    #[cfg(unix)]
    #[test]
    fn a_checkouts_git_config_runs_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pgman-git-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".pgman/masking")).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        // No git on this machine: nothing to run.
        if !git(&["init", "-q"]) {
            return;
        }
        let rules = dir.join(".pgman/masking/ledger.toml");
        std::fs::write(&rules, "version = 1\n").unwrap();
        std::fs::write(dir.join(".gitattributes"), "*.toml filter=planted\n").unwrap();
        assert!(git(&["add", "-A"]) && git(&["commit", "-q", "--no-gpg-sign", "-m", "rules"]));
        let marker = dir.join("ran");
        let script = dir.join("planted.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let s = script.display().to_string();
        for (k, v) in [
            ("core.fsmonitor", s.as_str()),
            ("filter.planted.clean", s.as_str()),
            ("log.showSignature", "true"),
            ("gpg.program", s.as_str()),
        ] {
            assert!(git(&["config", k, v]), "{k}");
        }
        // Stat info changed, so a status would look at the content.
        std::fs::write(&rules, "version = 1\n").unwrap();
        let digest = archive::sha256_file(&rules).unwrap();
        assert!(matches!(
            agent_rules(&rules, &digest),
            AgentRules::Unconfirmed
        ));
        assert!(rules_commit(&rules).is_some());
        assert!(!marker.exists(), "the checkout's config ran a program");
        // And the check still sees an edit.
        std::fs::write(&rules, "version = 1\n[tables.t]\nc = \"keep\"\n").unwrap();
        assert!(matches!(
            agent_rules(&rules, &digest),
            AgentRules::Refused(w) if w.contains("isn't committed as it is")
        ));
        assert!(!marker.exists(), "the checkout's config ran a program");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restore_copy_is_checked_as_it_is_copied() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("pgman-copy-checked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("schema.dump");
        std::fs::write(&plain, b"schema bytes").unwrap();
        let want = archive::sha256_file(&plain).unwrap();
        copy_checked(&plain, &dir.join("a"), None, &want).unwrap();
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), b"schema bytes");
        let e = copy_checked(&plain, &dir.join("b"), None, &"0".repeat(64)).unwrap_err();
        assert!(e.contains("checksum differs"), "{e}");
        // Encrypted: decrypted into the copy, checked against what is on
        // disk, every byte of it.
        let key = age::x25519::Identity::generate();
        let sealed = dir.join("sealed");
        let mut s = crate::crypt::Sink::new(
            std::fs::File::create(&sealed).unwrap(),
            Some(&key.to_public()),
        )
        .unwrap();
        s.write_all(b"secret schema").unwrap();
        s.finish().unwrap();
        let want = archive::sha256_file(&sealed).unwrap();
        copy_checked(&sealed, &dir.join("c"), Some(&key), &want).unwrap();
        assert_eq!(std::fs::read(dir.join("c")).unwrap(), b"secret schema");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&sealed)
            .unwrap()
            .write_all(b"x")
            .unwrap();
        assert!(copy_checked(&sealed, &dir.join("d"), Some(&key), &want).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_staged_dump_is_a_private_copy_that_goes_away() {
        let dir = std::env::temp_dir().join(format!("pgman-staged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.dump");
        std::fs::write(&file, b"PGDMP as it was").unwrap();
        let s = stage_dump(&file, &dir).unwrap();
        assert_eq!(s.sha256, archive::sha256_file(&file).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&s.file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // What the agent does to its file afterwards reaches nothing.
        std::fs::write(&file, b"PGDMP changed").unwrap();
        assert_eq!(std::fs::read(&s.file).unwrap(), b"PGDMP as it was");
        let temp = s.file.parent().unwrap().to_path_buf();
        assert!(restore_temp(&temp));
        drop(s);
        assert!(!temp.exists(), "removed when dropped");
        let e = stage_dump(&dir, &dir).err().unwrap();
        assert!(e.contains("not a single-file dump"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An encrypted archive as a dump writes one: every file but the
    /// header through a [`crate::crypt::Sink`].
    fn sealed_archive(dir: &Path, key: &age::x25519::Identity, header: &Header) {
        use std::io::Write;
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let to = key.to_public();
        let write = |path: PathBuf, bytes: &[u8]| {
            let mut s =
                crate::crypt::Sink::new(std::fs::File::create(&path).unwrap(), Some(&to)).unwrap();
            s.write_all(bytes).unwrap();
            s.finish().unwrap();
        };
        write(dir.join(archive::SCHEMA), b"schema bytes");
        let m = Manifest {
            header: header.clone(),
            schema_sha256: archive::sha256_file(&dir.join(archive::SCHEMA)).unwrap(),
            tables: vec![],
            large_objects: false,
            rules: "defaults".into(),
            key: "fresh".into(),
            words: 1,
            sequences: vec![],
            raw: None,
        };
        write(
            dir.join(archive::MANIFEST),
            serde_json::to_string(&m).unwrap().as_bytes(),
        );
        std::fs::write(
            dir.join(archive::HEADER),
            serde_json::to_string(header).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn an_encrypted_archive_opens_with_its_key_and_its_header_is_bound() {
        let dir =
            std::env::temp_dir().join(format!("pgman-sealed-{}.pgman-dump", std::process::id()));
        let key = age::x25519::Identity::generate();
        let header = Header {
            format: archive::FORMAT,
            pgman: "0.4.5".into(),
            copy: "c0ffee00-1".into(),
            label: "production/ledger".into(),
            env: Some("production".into()),
            database: "ledger".into(),
            created: 0,
            masked: true,
            protected: true,
            seal: Some(crate::crypt::Seal {
                kind: "age".into(),
                recipient: key.to_public().to_string(),
                kms_key: None,
                kms_region: None,
                wrapped: None,
            }),
        };
        sealed_archive(&dir, &key, &header);
        // Nothing readable on disk but the header.
        let manifest = std::fs::read(dir.join(archive::MANIFEST)).unwrap();
        assert!(
            !manifest.windows(9).any(|w| w == b"\"tables\""),
            "manifest encrypted"
        );
        let cache = dir.with_extension("cache");
        let opened = open_archive_with(&dir, &header, Some(key.clone()), &cache).unwrap();
        assert!(opened.decrypted());
        assert_eq!(std::fs::read(&opened.schema).unwrap(), b"schema bytes");
        let temp = opened.schema.parent().unwrap().to_path_buf();
        drop(opened);
        assert!(!temp.exists(), "the decrypted schema is removed with it");
        // Another key opens nothing.
        let other = age::x25519::Identity::generate();
        assert!(open_archive_with(&dir, &header, Some(other), &cache).is_err());
        // A plain header edited to say another source: the encrypted
        // manifest's copy disagrees.
        let mut edited = header.clone();
        edited.protected = false;
        std::fs::write(
            dir.join(archive::HEADER),
            serde_json::to_string(&edited).unwrap(),
        )
        .unwrap();
        let e = open_archive_with(&dir, &edited, Some(key), &cache)
            .map(|_| ())
            .unwrap_err();
        assert!(e.contains("disagrees with the manifest"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_forged_copy_id_is_refused_before_anything_is_written_or_removed() {
        let base = std::env::temp_dir().join(format!("pgman-forged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cache = base.join("cache/pgman");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(base.join("sentinel"), b"keep").unwrap();
        let dir = base.join("a.pgman-dump");
        let key = age::x25519::Identity::generate();
        for copy in ["../../..", "abcdefg\u{e9}", ""] {
            let header = Header {
                format: archive::FORMAT,
                pgman: "0.4.5".into(),
                copy: copy.into(),
                label: "production/ledger".into(),
                env: None,
                database: "ledger".into(),
                created: 0,
                masked: true,
                protected: true,
                seal: Some(crate::crypt::Seal {
                    kind: "age".into(),
                    recipient: key.to_public().to_string(),
                    kms_key: None,
                    kms_region: None,
                    wrapped: None,
                }),
            };
            sealed_archive(&dir, &key, &header);
            let e = open_archive_with(&dir, &header, Some(key.clone()), &cache)
                .map(|_| ())
                .unwrap_err();
            assert!(e.contains("not an id pgman writes"), "{copy:?}: {e}");
            assert!(base.join("sentinel").exists(), "{copy:?}");
            assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0, "{copy:?}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn only_a_restore_temp_pgman_named_is_ever_removed() {
        for ok in ["restore-123-abc0", "restore-1-f"] {
            assert!(restore_temp(Path::new(ok)), "{ok}");
        }
        for bad in [
            "restore-123-../..",
            "restore--abc",
            "restore-12a-abc",
            "restore-123-",
            "schema.dump",
            "..",
        ] {
            assert!(!restore_temp(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn a_dead_restores_decrypted_copy_is_swept_and_a_live_ones_kept() {
        let cache = std::env::temp_dir().join(format!("pgman-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(&cache).unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let mut running = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let gone = cache.join(format!("restore-{dead}-ab12"));
        let mine = cache.join(format!("restore-{}-cd34", std::process::id()));
        let live = cache.join(format!("restore-{}-ef56", running.id()));
        let other = cache.join("journal.sent");
        for d in [&gone, &mine, &live] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("raw.dump"), b"rows").unwrap();
        }
        std::fs::write(&other, b"x").unwrap();
        sweep_restores(&cache);
        assert!(!gone.exists(), "a dead process's plaintext is removed");
        assert!(mine.exists() && other.exists());
        assert!(live.exists(), "another running restore's is left alone");
        let _ = running.kill();
        let _ = running.wait();
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn exclude_patterns_read_as_pg_dump_reads_them() {
        assert!(pattern_matches("weird", "public", "weird"));
        assert!(pattern_matches("public.weird", "public", "weird"));
        assert!(!pattern_matches("other.weird", "public", "weird"));
        assert!(pattern_matches("public.audit_*", "public", "audit_2026"));
        assert!(
            pattern_matches("Weird", "public", "weird"),
            "unquoted folds"
        );
        assert!(
            !pattern_matches("\"Weird\"", "public", "weird"),
            "quoted keeps case"
        );
        assert!(
            pattern_matches("\"a.b\"", "public", "a.b"),
            "a quoted dot is a name's"
        );
        assert!(pattern_matches("t?", "public", "t1"));
        assert!(!pattern_matches("t?", "public", "t12"));
        let x = Excludes::parse(&[
            "--exclude-table=public.weird".to_string(),
            "-N".to_string(),
            "scratch".to_string(),
            "--exclude-table-data-and-children=events".to_string(),
            "--exclude-extension=postgis".to_string(),
        ]);
        assert!(x.rows_of("public", "weird", &[]));
        assert!(x.rows_of("scratch", "anything", &[]));
        assert!(x.rows_of(
            "public",
            "events_2026",
            &[("public".into(), "events".into())]
        ));
        assert!(!x.rows_of("public", "orders", &[]));
        assert!(
            x.sequence("public", "weird", None),
            "-T takes sequences too"
        );
        assert!(x.sequence("scratch", "junk_id_seq", None));
        assert!(
            x.sequence("public", "weird_id_seq", Some(("public", "weird"))),
            "with the table that owns it"
        );
        assert!(
            !x.sequence("public", "events_id_seq", Some(("public", "events"))),
            "rows only: the sequence stays"
        );
    }

    #[test]
    fn a_confirmation_is_for_the_file_it_was_given() {
        let dir = std::env::temp_dir().join(format!("pgman-confirm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (ledger, other) = (dir.join("ledger.toml"), dir.join("other.toml"));
        std::fs::write(&ledger, "version = 1\n").unwrap();
        std::fs::write(&other, "version = 1\n").unwrap();
        let list = format!("abc123  {}\n", ledger.display());
        assert!(confirmed_in(&list, "abc123", &ledger));
        assert!(
            !confirmed_in(&list, "abc123", &other),
            "the same bytes, another database"
        );
        assert!(!confirmed_in(&list, "def456", &ledger), "another digest");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_confirmation_says_when_and_how_and_is_read_back() {
        let dir = std::env::temp_dir().join(format!("pgman-confirm-how-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("ledger.toml");
        std::fs::write(&ledger, "version = 1\n").unwrap();
        let lines = confirmation_lines("abc123", &ledger, "over MCP", 1_794_000_000);
        assert_eq!(
            lines,
            format!(
                "# 2026-11-06T21:20:00Z confirmed over MCP\nabc123  {}\n",
                ledger.display()
            )
        );
        assert!(confirmed_in(&lines, "abc123", &ledger));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unconfirmed_loosening_rules_file_needs_a_terminal() {
        let f = ".pgman/masking/ledger.toml";
        // Masks no less than the defaults: used, with a note.
        assert!(unconfirmed_without_terminal(f, &[], false)
            .unwrap()
            .unwrap()
            .contains("isn't confirmed"));
        // Keeps a column: refused, saying what it keeps.
        let keeps = vec!["customers.notes kept: needed".to_string()];
        let e = unconfirmed_without_terminal(f, &keeps, false).unwrap_err();
        assert!(
            e.contains("customers.notes kept") && e.contains("Nothing dumped"),
            "{e}"
        );
        // Unless the operator said this environment runs unattended.
        assert!(unconfirmed_without_terminal(f, &keeps, true)
            .unwrap()
            .unwrap()
            .contains("unattended"));
    }

    #[test]
    fn masking_commands_parse() {
        let a = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_masking(&a(&[
                "check",
                "production",
                "--strict",
                "--masking",
                "m.toml"
            ])),
            Ok(MaskingCmd {
                check: true,
                target: "production".into(),
                masking: Some(PathBuf::from("m.toml")),
                strict: true
            })
        );
        assert!(
            parse_masking(&a(&["show", "p", "--strict"])).is_err(),
            "--strict is check's"
        );
        assert!(parse_masking(&a(&["show"]))
            .unwrap_err()
            .contains("name a target"));
        assert!(parse_masking(&a(&["frob", "p"])).is_err());
    }

    #[test]
    fn only_options_that_leave_things_out_pass() {
        let a = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            passthrough_problem(&a(&["--exclude-table=audit_*", "-N", "scratch"])),
            None
        );
        assert_eq!(
            passthrough_problem(&a(&["--exclude-table-data", "big"])),
            None
        );
        for bad in [
            &["--data-only"][..],
            &["-a"],
            &["--section=data"],
            &["--table=x"],
            &["--inserts"],
            &["-b"],
        ] {
            assert!(passthrough_problem(&a(bad)).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn a_masked_dump_is_a_directory_named_for_it() {
        let p = paths("production/ledger", None, false, 1_793_145_600, 7).unwrap();
        assert_eq!(
            p.file,
            PathBuf::from("production-ledger-20261028T000000Z.pgman-dump")
        );
        assert_eq!(
            p.latest,
            Some(PathBuf::from("production-ledger-latest.pgman-dump"))
        );
        assert_eq!(
            p.partial,
            PathBuf::from(".production-ledger-20261028T000000Z.pgman-dump.partial-7")
        );
        assert!(paths("p", Some(Path::new("copy.dump")), false, 0, 1)
            .unwrap_err()
            .contains("….pgman-dump"));
        assert!(paths("p", Some(Path::new("x/copy.pgman-dump")), false, 0, 1).is_ok());
        // A directory that isn't there is said to be missing.
        for missing in ["../no-such-dir/", "no-such-dir"] {
            assert!(
                paths("p", Some(Path::new(missing)), false, 0, 1)
                    .unwrap_err()
                    .contains("no such directory"),
                "{missing}"
            );
        }
    }

    #[test]
    fn a_row_is_masked_column_by_column_with_its_key() {
        use crate::masking::classify::{Decision, Origin, TablePlan};
        let dec = |c: &str, rule: Rule| Decision {
            column: c.into(),
            rule,
            origin: Origin::Plain,
            fit: None,
            not_null: false,
            unique: false,
            base: "text".into(),
            enum_first: None,
            array: false,
        };
        let plan = TablePlan {
            table: "public.customers".into(),
            drop_rows: false,
            drop_why: None,
            columns: vec![
                dec("id", Rule::Keep),
                dec("email", Rule::Email),
                dec("notes", Rule::Null),
            ],
            primary_key: vec!["id".into()],
        };
        let m = Masker::new(&[1; 32]);
        let row = copytext::decode(b"7\talice@example.com\t\\N");
        let out = mask_row(&m, &plan, &row, &[0]);
        assert_eq!(out[0].as_deref(), Some(&b"7"[..]));
        let email = String::from_utf8(out[1].clone().unwrap()).unwrap();
        assert!(
            email.ends_with("@example.test") && !email.contains("alice"),
            "{email}"
        );
        assert_eq!(out[2], None);
        assert_eq!(row_key(&row, &[0]), b"7\0");
    }
}
