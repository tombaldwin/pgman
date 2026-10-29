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
            if !p.to_string_lossy().ends_with(archive::EXTENSION) {
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
            unconfirmed = tokio::task::spawn_blocking(move || confirm_rules_cli(&r, &d))
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
    let copied: Vec<&classify::TablePlan> = plan
        .tables
        .iter()
        .filter(|t| !t.drop_rows && !skipped.contains(&t.table))
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
    let sequences = crate::query::masking::sequences(&client).await?;

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
) -> Result<Vec<String>, String> {
    use std::io::Read;
    let mut notes = Vec::new();
    for t in &m.tables {
        let path = dir.join("data").join(&t.file);
        let f = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut gz = flate2::read::GzDecoder::new(std::io::BufReader::new(f));
        let mut chunks = Vec::new();
        loop {
            let mut buf = vec![0u8; 1 << 16];
            let n = gz
                .read(&mut buf)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if n == 0 {
                break;
            }
            buf.truncate(n);
            chunks.push(buf);
        }
        let sql = crate::query::masking::copy_in_sql(&t.table, &t.columns);
        let rows = crate::query::masking::copy_in(client, &sql, chunks.into_iter())
            .await
            .map_err(|e| format!("{}: {e}", t.table))?;
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
pub fn confirmed(digest: &str) -> bool {
    std::fs::read_to_string(confirmed_file())
        .unwrap_or_default()
        .lines()
        .any(|l| l.split_whitespace().next() == Some(digest))
}

/// Record `digest` of `file` as confirmed. Writes the data directory.
pub fn confirm(digest: &str, file: &Path) -> Result<(), String> {
    let path = confirmed_file();
    crate::util::ensure_private_dir(&crate::journal::dir()).map_err(|e| e.to_string())?;
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
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
    if !confirmed(digest) {
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
/// is used, unrecorded, and the result says so.
pub fn confirm_rules_cli(
    rules: &crate::masking::rules::RuleSet,
    digest: &str,
) -> Result<Option<String>, String> {
    use std::io::IsTerminal;
    let Some(file) = &rules.file else {
        return Ok(None);
    };
    if confirmed(digest) {
        return Ok(None);
    }
    if !std::io::stdin().is_terminal() {
        return Ok(Some(format!(
            "{} isn't confirmed at a terminal yet: an agent can't use it until it is",
            file.display()
        )));
    }
    let loose = crate::masking::rules::loosening(rules);
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
