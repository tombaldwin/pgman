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
                    "{}: a masked dump is a directory named …{}; name one, or a directory to put it in",
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
    let header = Header {
        format: archive::FORMAT,
        pgman: env!("CARGO_PKG_VERSION").to_string(),
        copy: copy.to_string(),
        label: label.clone(),
        env: resolved.environment.clone(),
        database: resolved.database.clone(),
        created: now,
        masked: true,
        protected: resolved.protected,
    };
    let manifest = Manifest {
        header: header.clone(),
        schema_sha256: archive::sha256_file(&p.partial.join(archive::SCHEMA))?,
        tables,
        large_objects: rules.large_objects_kept,
        rules: rules_digest,
        key: "fresh".to_string(),
        words: crate::masking::transform::WORDS_VERSION,
        sequences,
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
        let mut m = private_file(&p.partial.join(archive::MANIFEST))?;
        m.write_all(
            serde_json::to_string_pretty(&manifest)
                .unwrap_or_default()
                .as_bytes(),
        )
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
    notes.push(format!(
        "every masked password is {}; a fresh key for this copy, so its pseudonyms match no other",
        crate::masking::transform::TEST_PASSWORD
    ));
    notes.extend(pre.notes);
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
}

/// pg_dump's no-rows archive, then each table's masked rows, into `dir`.
async fn write_archive(r: ArchiveRun<'_>) -> Result<Vec<TableEntry>, String> {
    let conninfo = pgtool::conninfo(&r.opened.dsn, r.opened.via, "pgman-dump", r.have);
    let mut args = vec![
        format!("--dbname={conninfo}"),
        "--format=c".to_string(),
        format!("--snapshot={}", r.snapshot),
        "--exclude-table-data=*.*".to_string(),
        "--no-comments".to_string(),
        "--no-password".to_string(),
    ];
    if !r.plan.large_objects {
        args.push("--no-blobs".to_string());
    }
    args.extend(r.excludes.iter().cloned());
    args.extend(r.opts.extra.iter().cloned());
    let schema = private_file(&r.dir.join(archive::SCHEMA))?;
    let mut cmd = pgtool::private_command(r.tool, &args);
    cmd.stdout(schema);
    pgtool::set_child_env(&mut cmd, r.opened.password());
    let (status, tail) = pgtool::run_tool_cli(cmd, r.opts.verbose)
        .await
        .map_err(|e| format!("pg_dump {e}"))?;
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
        let f = private_file(&path)?;
        let mut gz =
            flate2::write::GzEncoder::new(std::io::BufWriter::new(f), flate2::Compression::fast());
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
            .and_then(|mut w| w.flush())
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
    m: &Manifest,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Vec<String>, String> {
    use std::io::Read;
    let mut notes = Vec::new();
    for t in &m.tables {
        let path = dir.join("data").join(&t.file);
        let f = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut gz = flate2::read::GzDecoder::new(std::io::BufReader::new(f));
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
/// confirmed at a terminal. Reads the data directory.
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

/// Record `digest` of `file` as confirmed. Writes the data directory.
pub fn confirm(digest: &str, file: &Path) -> Result<(), String> {
    let path = confirmed_file();
    crate::util::ensure_private_dir(&crate::journal::dir()).map_err(|e| e.to_string())?;
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    text.push_str(&format!("{digest}  {}\n", file.display()));
    crate::util::write_private(&path, &text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Why the rules file at `file` can't be used by an agent, if it can't:
/// it must be committed, as it is, and confirmed at a terminal — so an
/// agent that edits it (to keep a column) can't use the edit until the
/// operator has seen it. Reads git and the data directory.
pub fn agent_rules_problem(file: &Path, digest: &str) -> Option<String> {
    let dir = file.parent().unwrap_or(Path::new("."));
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .arg(file)
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
    };
    let tracked = git(&["ls-files", "--error-unmatch", "--"]).is_ok_and(|o| o.status.success());
    let clean = git(&["status", "--porcelain", "--"])
        .is_ok_and(|o| o.status.success() && o.stdout.is_empty());
    if !tracked || !clean {
        return Some(format!(
            "{} isn't committed as it is: an agent's dump uses committed rules only. Commit it, then \
             run pgman dump --masked at a terminal once to confirm it",
            file.display()
        ));
    }
    if !confirmed(digest, file) {
        return Some(format!(
            "{} hasn't been confirmed at a terminal: run pgman dump <target> --masked there once, \
             which shows what it keeps and asks",
            file.display()
        ));
    }
    None
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
    eprintln!(
        "pgman dump: {} is new or changed since you last confirmed it.{}",
        file.display(),
        if loose.is_empty() {
            " It masks no less than the defaults.".to_string()
        } else {
            format!(
                " What it keeps that the defaults would mask:\n  {}",
                loose.join("\n  ")
            )
        }
    );
    if !crate::pgtool::confirm_cli("use these rules? [y/N] ", "y")? {
        return Err("rules not confirmed; nothing dumped".into());
    }
    confirm(digest, file)?;
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
            "{file} isn't confirmed at a terminal yet: an agent can't use it until it is"
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

/// The manifest of the archive at `dir`, checked: this pgman's format,
/// generated file names, and every file's checksum. Reads it all.
pub fn checked_manifest(dir: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(dir.join(archive::MANIFEST))
        .map_err(|e| format!("{}: {e}", dir.join(archive::MANIFEST).display()))?;
    let m: Manifest = serde_json::from_str(&text)
        .map_err(|e| format!("{}: not a pgman manifest ({e})", dir.display()))?;
    if let Some(why) = archive::manifest_problem(&m) {
        return Err(why);
    }
    // header.json is what the archive says it is at a glance (copies
    // lists it): the manifest's copy of it, which it must match.
    let header: Header = std::fs::read_to_string(dir.join(archive::HEADER))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| format!("{}: no readable {}", dir.display(), archive::HEADER))?;
    if header != m.header {
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
    check(dir.join(archive::SCHEMA), &m.schema_sha256)?;
    for t in &m.tables {
        check(dir.join("data").join(&t.file), &t.sha256)?;
    }
    Ok(m)
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
