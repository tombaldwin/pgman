//! `pgman copies`: every dump pgman has taken on this machine — built
//! from the journal — with its age, expiry, reason and where it was
//! restored; `rm` deletes one (journaled), `report` prints the account
//! of them all for a reviewer. Also what a dump's reason is drawn from:
//! the ticket in the branch's name, else the last reason for the target.
//! See `docs/design/masked-dumps.md`, section 6.
//!
//! Pure registry, listing and parsing; the I/O (`*_cli`, the git branch,
//! the file deletes) is thin.

use crate::journal::{self, Event, Kind};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "\
usage: pgman copies [list] [--all]
       pgman copies show <copy>
       pgman copies rm <copy> [--yes]
       pgman copies report [--since YYYY-MM-DD] [--format text|markdown|json]

Every dump pgman has taken on this machine, from its journal
(~/.local/share/pgman/journal.jsonl): when, of what, why, how big, when
it expires, where it was restored, and whether it is still there.

<copy> is the start of a copy's id (four characters or more, as list
shows it), or the dump's file (its -latest link too).

  list     the copies still on disk (--all: deleted ones too), and dump
           files in this directory the journal doesn't know (taken
           by an older pgman, or by hand)
  show     one copy in full
  rm       delete a copy's file, and its -latest link (pointed at the
           newest copy left, when there is one); asks first unless --yes
  report   the whole account, oldest first, for a ticket or a reviewer

  pgman copies
  pgman copies rm 3f9a
  pgman copies report --since 2026-10-01 --format markdown";

/// A restore of a copy, from the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Restored {
    pub ts: u64,
    pub server: String,
    pub database: String,
}

/// One dump, as the journal tells it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Copy {
    pub id: String,
    pub ts: u64,
    pub label: String,
    pub env: Option<String>,
    pub database: Option<String>,
    pub protected: bool,
    pub mode: String,
    pub ticket: Option<String>,
    pub reason: Option<String>,
    pub via: String,
    pub file: PathBuf,
    pub latest: Option<PathBuf>,
    pub bytes: u64,
    pub expires: Option<u64>,
    /// When it was deleted, and why.
    pub deleted: Option<(u64, String)>,
    pub restores: Vec<Restored>,
}

impl Copy {
    /// The id as `list` shows it.
    pub fn short(&self) -> &str {
        &self.id[..self.id.len().min(8)]
    }

    pub fn expired(&self, now: u64) -> bool {
        self.expires.is_some_and(|e| e <= now)
    }
}

/// Every successful dump in `events`, oldest first, with what happened
/// to it since. Pure.
pub fn registry(events: &[Event]) -> Vec<Copy> {
    let mut out: Vec<Copy> = Vec::new();
    for e in events {
        let Some(id) = &e.copy else { continue };
        match e.event {
            Some(Kind::Dump) if e.ok != Some(false) => {
                let Some(file) = &e.file else { continue };
                out.push(Copy {
                    id: id.clone(),
                    ts: e.ts,
                    label: e
                        .label
                        .clone()
                        .or_else(|| e.database.clone())
                        .unwrap_or_default(),
                    env: e.env.clone(),
                    database: e.database.clone(),
                    protected: e.protected,
                    mode: e.mode.clone().unwrap_or_else(|| "raw".to_string()),
                    ticket: e.ticket.clone(),
                    reason: e.reason.clone(),
                    via: e.via.clone().unwrap_or_else(|| "cli".to_string()),
                    file: PathBuf::from(file),
                    latest: e.latest.as_ref().map(PathBuf::from),
                    bytes: e.bytes.unwrap_or(0),
                    expires: e.expires,
                    deleted: None,
                    restores: Vec::new(),
                });
            }
            Some(Kind::Restore) => {
                if let Some(c) = out.iter_mut().find(|c| &c.id == id) {
                    c.restores.push(Restored {
                        ts: e.ts,
                        server: e.into_server.clone().unwrap_or_default(),
                        database: e.into_database.clone().unwrap_or_default(),
                    });
                }
            }
            Some(Kind::Delete) => {
                if let Some(c) = out.iter_mut().find(|c| &c.id == id) {
                    c.deleted = Some((e.ts, e.why.clone().unwrap_or_else(|| "by hand".into())));
                }
            }
            _ => {}
        }
    }
    out
}

/// The copy `file` is, by its recorded path. Pure.
pub fn by_file<'a>(copies: &'a [Copy], file: &Path) -> Option<&'a Copy> {
    copies
        .iter()
        .rev()
        .find(|c| c.deleted.is_none() && c.file == file)
}

/// The copy whose id starts `prefix`, or why there is none to pick.
/// Pure.
pub fn by_prefix<'a>(copies: &'a [Copy], prefix: &str) -> Result<&'a Copy, String> {
    let p = prefix.to_ascii_lowercase();
    if p.len() < 4 || !p.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(format!(
            "{prefix:?} isn't a copy: give four or more characters of its id, or its file"
        ));
    }
    let hits: Vec<&Copy> = copies.iter().filter(|c| c.id.starts_with(&p)).collect();
    match hits.as_slice() {
        [] => Err(format!(
            "no copy's id starts {prefix}: pgman copies --all lists them"
        )),
        [one] => Ok(one),
        _ => Err(format!(
            "{} copies' ids start {prefix}: give more of it",
            hits.len()
        )),
    }
}

/// The last reason given for a dump of `label`, to offer again. Pure.
pub fn last_reason<'a>(copies: &'a [Copy], label: &str) -> Option<&'a str> {
    copies
        .iter()
        .rev()
        .filter(|c| c.label == label)
        .find_map(|c| c.reason.as_deref())
}

/// The first ticket key in `text` (`APP-101` in
/// `feature/APP-101-invoice-rounding`, `app-101` too, upper-cased):
/// letters and digits starting with a letter, a dash, digits — whole,
/// not inside a longer word or number. With `keys`, only those
/// projects' keys. Pure.
pub fn ticket_in(text: &str, keys: &[String]) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let starts =
            chars[i].is_ascii_alphabetic() && (i == 0 || !chars[i - 1].is_ascii_alphanumeric());
        if !starts {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && chars[j].is_ascii_alphanumeric() {
            j += 1;
        }
        if j < chars.len() && chars[j] == '-' {
            let mut k = j + 1;
            while k < chars.len() && chars[k].is_ascii_digit() {
                k += 1;
            }
            let ends = k == chars.len() || !chars[k].is_ascii_alphanumeric();
            if k > j + 1 && ends {
                let key: String = chars[i..j].iter().collect::<String>().to_ascii_uppercase();
                let num: String = chars[j + 1..k].iter().collect();
                // `utf-8`, `v-2`: a key is two letters or more.
                let wanted = if keys.is_empty() {
                    key.len() >= 2 && key.chars().filter(char::is_ascii_alphabetic).count() >= 2
                } else {
                    keys.iter().any(|k| k.eq_ignore_ascii_case(&key))
                };
                if wanted && !key.eq_ignore_ascii_case("utf") {
                    return Some(format!("{key}-{num}"));
                }
            }
        }
        i = j.max(i + 1);
    }
    None
}

/// A ticket's link from the project's `ticket_url`. Pure.
pub fn ticket_link(url: Option<&str>, ticket: &str) -> Option<String> {
    url.filter(|u| u.contains("{ticket}"))
        .map(|u| u.replace("{ticket}", ticket))
}

/// What the reason prompt offers: the branch's ticket, else the last
/// reason for the target. Pure.
pub fn reason_default(branch_ticket: Option<&str>, last: Option<&str>) -> Option<String> {
    branch_ticket.or(last).map(str::to_string)
}

/// The answer to the reason prompt as a reason and a ticket: empty takes
/// the default. Pure.
pub fn read_reason(
    answer: &str,
    default: Option<&str>,
    keys: &[String],
) -> Result<(Option<String>, Option<String>), String> {
    let text = match answer.trim() {
        "" => default.unwrap_or(""),
        t => t,
    };
    let reason = journal::clean_reason(text)?;
    let ticket = reason.as_deref().and_then(|r| ticket_in(r, keys));
    Ok((reason, ticket))
}

/// The current git branch under `cwd`, if it is in a repository and on
/// one — a new repository's first branch, with no commit yet, too. Runs
/// git.
pub fn git_branch(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let b = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !b.is_empty() && b != "HEAD").then_some(b)
}

/// Whether `name` is what `pgman dump` calls a dump:
/// `<target>-YYYYMMDDTHHMMSSZ.<ext>`. Pure.
pub fn dump_named(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if !["dump", "sql", "dir", "tar"].contains(&ext) {
        return false;
    }
    let Some((_, stamp)) = stem.rsplit_once('-') else {
        return false;
    };
    let b = stamp.as_bytes();
    b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[9..15].iter().all(u8::is_ascii_digit)
}

/// `2026-10-28 14:03Z`. Pure.
fn when(ts: u64) -> String {
    let t = crate::app::format_unix_secs_utc(ts);
    format!("{} {}Z", &t[..10], &t[11..16])
}

/// `in 13 days`, `2 days ago`. Pure.
fn relative(ts: u64, now: u64) -> String {
    let (d, future) = if ts >= now {
        (ts - now, true)
    } else {
        (now - ts, false)
    };
    if d < 60 {
        return "just now".to_string();
    }
    let text = match d {
        d if d < 3_600 => format!("{} min", d / 60),
        d if d < 2 * 86_400 => format!("{} h", d / 3_600),
        // To the nearest day: a copy kept 14 days is "in 14 days" when
        // it is taken, not 13 and some hours.
        d => format!("{} days", (d + 43_200) / 86_400),
    };
    if future {
        format!("in {text}")
    } else {
        format!("{text} ago")
    }
}

/// The ticket, else the reason, for a column. Pure.
fn why_short(c: &Copy) -> String {
    c.ticket
        .as_deref()
        .or(c.reason.as_deref())
        .unwrap_or("-")
        .to_string()
}

/// Where a copy stands: deleted, gone from disk, expired, or kept.
fn status(c: &Copy, present: bool, now: u64) -> &'static str {
    match (&c.deleted, present) {
        (Some(_), _) => "deleted",
        (None, false) => "missing",
        (None, true) if c.expired(now) => "EXPIRED",
        _ => "kept",
    }
}

/// `pgman copies` as text: one line per copy (deleted ones only with
/// `all`), then the untracked files. `present` says whether a copy's
/// file is on disk. Pure.
pub fn render_list(
    copies: &[Copy],
    untracked: &[PathBuf],
    present: &dyn Fn(&Path) -> bool,
    all: bool,
    now: u64,
) -> String {
    let shown: Vec<&Copy> = copies
        .iter()
        .filter(|c| all || c.deleted.is_none())
        .collect();
    let mut out = String::new();
    if shown.is_empty() {
        out.push_str("no copies in the journal");
        if !all && copies.iter().any(|c| c.deleted.is_some()) {
            out.push_str(" still on disk (--all lists the deleted ones)");
        }
        out.push('\n');
    } else {
        let rows: Vec<[String; 8]> = shown
            .iter()
            .map(|c| {
                [
                    c.short().to_string(),
                    when(c.ts),
                    c.label.clone(),
                    if c.protected {
                        format!("{}, protected", c.mode)
                    } else {
                        c.mode.clone()
                    },
                    crate::pgtool::size_of(c.bytes),
                    match c.expires {
                        Some(e) => relative(e, now),
                        None => "-".into(),
                    },
                    status(c, present(&c.file), now).to_string(),
                    why_short(c),
                ]
            })
            .collect();
        let head = [
            "ID", "TAKEN", "TARGET", "KIND", "SIZE", "EXPIRES", "STATUS", "WHY",
        ];
        let width: Vec<usize> = (0..8)
            .map(|i| {
                rows.iter()
                    .map(|r| crate::ui::display_width(&r[i]))
                    .chain([head[i].len()])
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let line = |cells: &[&str]| {
            let mut l = String::new();
            for (i, cell) in cells.iter().enumerate() {
                l.push_str(cell);
                if i + 1 < cells.len() {
                    let pad = width[i].saturating_sub(crate::ui::display_width(cell)) + 2;
                    l.push_str(&" ".repeat(pad));
                }
            }
            l.trim_end().to_string()
        };
        out.push_str(&line(&head));
        out.push('\n');
        for r in &rows {
            let cells: Vec<&str> = r.iter().map(String::as_str).collect();
            out.push_str(&line(&cells));
            out.push('\n');
        }
        let expired = shown
            .iter()
            .filter(|c| status(c, present(&c.file), now) == "EXPIRED")
            .count();
        if expired > 0 {
            out.push_str(&format!(
                "{expired} past {} expiry: pgman copies rm <id> when you're done with {}\n",
                if expired == 1 { "its" } else { "their" },
                if expired == 1 { "it" } else { "them" }
            ));
        }
    }
    if !untracked.is_empty() {
        out.push_str("\nnot in the journal (taken by an older pgman, or by hand):\n");
        for f in untracked {
            out.push_str(&format!("  {}\n", f.display()));
        }
    }
    out.trim_end().to_string()
}

/// `pgman copies show`: one copy in full. Pure.
pub fn render_show(c: &Copy, present: bool, now: u64, ticket_url: Option<&str>) -> String {
    let mut out = vec![
        format!("copy      {}", c.id),
        format!("taken     {} ({})", when(c.ts), relative(c.ts, now)),
        format!("of        {}", c.label),
        format!(
            "kind      {}{}",
            c.mode,
            if c.protected {
                ", from a protected target"
            } else {
                ""
            }
        ),
        format!("via       {}", c.via),
        format!(
            "file      {} ({}{})",
            c.file.display(),
            crate::pgtool::size_of(c.bytes),
            if present || c.deleted.is_some() {
                ""
            } else {
                ", not there now"
            }
        ),
    ];
    if let Some(t) = &c.ticket {
        out.push(match ticket_link(ticket_url, t) {
            Some(link) => format!("ticket    {t}  {link}"),
            None => format!("ticket    {t}"),
        });
    }
    if let Some(r) = &c.reason {
        out.push(format!("reason    {r}"));
    }
    if let Some(e) = c.expires {
        out.push(format!("expires   {} ({})", when(e), relative(e, now)));
    }
    for r in &c.restores {
        out.push(format!(
            "restored  into {} on {}, {}",
            r.database,
            r.server,
            when(r.ts)
        ));
    }
    if let Some((ts, why)) = &c.deleted {
        out.push(format!("deleted   {} ({why})", when(*ts)));
    }
    out.join("\n")
}

/// How `report` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
    Json,
}

/// `pgman copies report`: every copy since `since`, oldest first. Pure.
pub fn render_report(
    copies: &[Copy],
    since: u64,
    format: Format,
    ticket_url: Option<&str>,
) -> String {
    let shown: Vec<&Copy> = copies.iter().filter(|c| c.ts >= since).collect();
    let why = |c: &Copy| {
        let t = c
            .ticket
            .as_ref()
            .map(|t| match (format, ticket_link(ticket_url, t)) {
                (Format::Markdown, Some(link)) => format!("[{t}]({link})"),
                (_, Some(link)) => format!("{t} {link}"),
                _ => t.clone(),
            });
        match (t, &c.reason) {
            // The reason names the ticket already: once is enough, as
            // a link where there is one.
            (Some(t), Some(r)) if c.ticket.as_deref().is_some_and(|k| r.contains(k)) => {
                match c.ticket.as_deref() {
                    Some(k) if r.trim() == k => t,
                    Some(k) => r.replacen(k, &t, 1),
                    None => r.clone(),
                }
            }
            (Some(t), Some(r)) => format!("{t}: {r}"),
            (Some(t), None) => t,
            (None, Some(r)) => r.clone(),
            (None, None) => "no reason given".to_string(),
        }
    };
    let after = |c: &Copy| {
        let mut parts: Vec<String> = c
            .restores
            .iter()
            .map(|r| {
                format!(
                    "restored into {} on {} {}",
                    r.database,
                    r.server,
                    when(r.ts)
                )
            })
            .collect();
        parts.push(match &c.deleted {
            Some((ts, w)) => format!("deleted {} ({w})", when(*ts)),
            None => "not deleted".to_string(),
        });
        parts.join("; ")
    };
    match format {
        Format::Json => serde_json::to_string_pretty(&shown).unwrap_or_else(|_| "[]".into()),
        Format::Markdown => {
            let mut out = String::from(
                "| Taken | Target | Kind | Size | Why | Since |\n|---|---|---|---|---|---|\n",
            );
            for c in &shown {
                out.push_str(&format!(
                    "| {} | {} | {}{} | {} | {} | {} |\n",
                    when(c.ts),
                    c.label,
                    c.mode,
                    if c.protected { ", protected" } else { "" },
                    crate::pgtool::size_of(c.bytes),
                    why(c).replace('|', "\\|"),
                    after(c)
                ));
            }
            if shown.is_empty() {
                out.push_str("| | no copies | | | | |\n");
            }
            out.trim_end().to_string()
        }
        Format::Text => {
            if shown.is_empty() {
                return "no copies in that time".to_string();
            }
            shown
                .iter()
                .map(|c| {
                    format!(
                        "{}  {} ({}{}, {}) — {}\n    {}",
                        when(c.ts),
                        c.label,
                        c.mode,
                        if c.protected { ", protected" } else { "" },
                        crate::pgtool::size_of(c.bytes),
                        why(c),
                        after(c)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

/// `YYYY-MM-DD` as unix seconds at its start. Pure.
pub fn parse_date(s: &str) -> Result<u64, String> {
    let bad = || format!("--since {s:?}: give a date, YYYY-MM-DD");
    let parts: Vec<&str> = s.split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return Err(bad());
    };
    let (y, m, d): (i64, i64, i64) = (
        y.parse().map_err(|_| bad())?,
        m.parse().map_err(|_| bad())?,
        d.parse().map_err(|_| bad())?,
    );
    if y < 1970 || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok((days * 86_400) as u64)
}

/// Parsed `pgman copies` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    List { all: bool },
    Show(String),
    Rm { copy: String, yes: bool },
    Report { since: u64, format: Format },
}

/// Parse the arguments after `pgman copies`. Pure.
pub fn parse_cli(args: &[String]) -> Result<Cmd, String> {
    let mut it = args.iter().map(String::as_str);
    let sub = it.next();
    let rest: Vec<&str> = it.collect();
    let one = |what: &str, rest: &[&str]| -> Result<String, String> {
        match rest {
            [c] if !c.starts_with('-') => Ok(c.to_string()),
            _ => Err(format!("pgman copies {what} takes one copy\n\n{USAGE}")),
        }
    };
    match sub {
        None | Some("list") | Some("--all") => {
            let mut all = sub == Some("--all");
            for a in &rest {
                match *a {
                    "--all" => all = true,
                    other => return Err(format!("unexpected {other:?}\n\n{USAGE}")),
                }
            }
            Ok(Cmd::List { all })
        }
        Some("show") => Ok(Cmd::Show(one("show", &rest)?)),
        Some("rm") => {
            let yes = rest.contains(&"--yes");
            let left: Vec<&str> = rest.iter().copied().filter(|a| *a != "--yes").collect();
            Ok(Cmd::Rm {
                copy: one("rm", &left)?,
                yes,
            })
        }
        Some("report") => {
            let mut since = 0;
            let mut format = Format::Text;
            let mut i = 0;
            while i < rest.len() {
                let value = rest
                    .get(i + 1)
                    .ok_or_else(|| format!("{} needs a value\n\n{USAGE}", rest[i]))?;
                match rest[i] {
                    "--since" => since = parse_date(value)?,
                    "--format" => {
                        format = match *value {
                            "text" => Format::Text,
                            "markdown" | "md" => Format::Markdown,
                            "json" => Format::Json,
                            other => {
                                return Err(format!(
                                    "--format {other:?}: use text, markdown or json"
                                ))
                            }
                        }
                    }
                    other => return Err(format!("unexpected {other:?}\n\n{USAGE}")),
                }
                i += 2;
            }
            Ok(Cmd::Report { since, format })
        }
        Some(other) => Err(format!("unknown copies command {other:?}\n\n{USAGE}")),
    }
}

/// Whether a dump file is there (a directory-format dump too); a link
/// in its place doesn't count. Reads the filesystem.
pub fn present(file: &Path) -> bool {
    std::fs::symlink_metadata(file).is_ok_and(|m| !m.file_type().is_symlink())
}

/// Dump files in `dir` that no copy in `copies` is. Reads `dir`.
pub fn untracked(dir: &Path, copies: &[Copy]) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let known: Vec<PathBuf> = copies
        .iter()
        .filter(|c| c.deleted.is_none())
        .map(|c| c.file.canonicalize().unwrap_or_else(|_| c.file.clone()))
        .collect();
    let mut out: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(dump_named))
        .map(|e| e.path())
        .filter(|p| !p.is_symlink())
        .filter(|p| {
            let c = p.canonicalize().unwrap_or_else(|_| p.clone());
            !known.contains(&c)
        })
        .collect();
    out.sort();
    out
}

/// What `rm` is asked to delete: a copy the journal knows, or a dump
/// file it doesn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Known(Box<Copy>),
    Untracked(PathBuf),
}

/// What `arg` names: a copy by id, a copy by its file (or a `-latest`
/// link to it), or an untracked dump file. Reads the filesystem.
pub fn find(copies: &[Copy], arg: &str) -> Result<Target, String> {
    let path = Path::new(arg);
    let meta = std::fs::symlink_metadata(path);
    if meta.is_err() && !arg.contains('/') && !arg.contains('.') {
        return by_prefix(copies, arg).map(|c| Target::Known(Box::new(c.clone())));
    }
    let m = meta.map_err(|e| format!("{arg}: {e}"))?;
    let real = if m.file_type().is_symlink() {
        let to = std::fs::read_link(path).map_err(|e| format!("{arg}: {e}"))?;
        path.parent().unwrap_or(Path::new(".")).join(to)
    } else {
        path.to_path_buf()
    };
    let canon = real
        .canonicalize()
        .map_err(|e| format!("{}: {e}", real.display()))?;
    if let Some(c) = copies
        .iter()
        .rev()
        .find(|c| c.deleted.is_none() && c.file.canonicalize().ok().as_ref() == Some(&canon))
    {
        return Ok(Target::Known(Box::new(c.clone())));
    }
    let name = canon.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !dump_named(name) {
        return Err(format!(
            "{arg} isn't a copy the journal knows, and isn't named like a pgman dump; \
             delete it yourself if it should go"
        ));
    }
    Ok(Target::Untracked(canon))
}

/// Delete the dump at `file`: a file, or a directory-format dump (a
/// directory with pg_dump's `toc.dat` in it). Never follows a link.
/// Writes the filesystem.
pub fn delete_dump(file: &Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(file).map_err(|e| format!("{}: {e}", file.display()))?;
    if m.file_type().is_symlink() {
        return Err(format!(
            "{} is a link now, not the dump; nothing deleted",
            file.display()
        ));
    }
    if m.is_dir() {
        if !file.join("toc.dat").is_file() {
            return Err(format!(
                "{} is a directory without pg_dump's toc.dat; nothing deleted",
                file.display()
            ));
        }
        std::fs::remove_dir_all(file).map_err(|e| format!("{}: {e}", file.display()))
    } else {
        std::fs::remove_file(file).map_err(|e| format!("{}: {e}", file.display()))
    }
}

/// After `gone` is deleted: its `-latest` link, when it still points at
/// it, is pointed at the newest copy left that shares the link, or
/// removed. Never follows the link. What happened, for the result.
/// Writes the filesystem.
pub fn fix_latest(copies: &[Copy], gone: &Copy) -> Option<String> {
    let link = gone.latest.as_ref()?;
    let m = std::fs::symlink_metadata(link).ok()?;
    if !m.file_type().is_symlink() {
        return None;
    }
    let to = std::fs::read_link(link).ok()?;
    let target = link.parent().unwrap_or(Path::new(".")).join(&to);
    let points_at_gone = target.file_name() == gone.file.file_name()
        && (to.components().count() == 1 || target == gone.file);
    if !points_at_gone {
        return None;
    }
    let next = copies
        .iter()
        .rev()
        .filter(|c| c.id != gone.id && c.deleted.is_none() && c.latest.as_ref() == Some(link))
        .find(|c| present(&c.file));
    match next {
        Some(n) => match crate::dump::point_latest(link, &n.file) {
            Ok(()) => Some(format!(
                "{} now points at {}",
                link.display(),
                n.file.file_name().unwrap_or_default().to_string_lossy()
            )),
            Err(e) => Some(e),
        },
        None => match std::fs::remove_file(link) {
            Ok(()) => Some(format!("{} removed", link.display())),
            Err(e) => Some(format!("{}: {e}", link.display())),
        },
    }
}

/// Run `pgman copies`, printing the result.
pub fn run_cli(cmd: &Cmd) -> Result<(), String> {
    let dir = journal::dir();
    let (events, bad) = journal::read_in(&dir);
    let copies = registry(&events);
    let now = journal::now();
    let cwd = std::env::current_dir().map_err(|e| format!("the current directory: {e}"))?;
    let dumps = crate::project::load_dumps(&cwd)
        .ok()
        .flatten()
        .map(|(_, d)| d);
    let ticket_url = dumps.as_ref().and_then(|d| d.ticket_url.clone());
    if bad > 0 {
        eprintln!(
            "pgman copies: {bad} line{} of {} couldn't be read and {} left out",
            if bad == 1 { "" } else { "s" },
            dir.join(journal::FILE).display(),
            if bad == 1 { "is" } else { "are" }
        );
    }
    match cmd {
        Cmd::List { all } => {
            let loose = untracked(&cwd, &copies);
            println!("{}", render_list(&copies, &loose, &present, *all, now));
        }
        Cmd::Show(arg) => match find(&copies, arg)? {
            Target::Known(c) => println!(
                "{}",
                render_show(&c, present(&c.file), now, ticket_url.as_deref())
            ),
            Target::Untracked(p) => println!(
                "{} isn't in the journal: taken by an older pgman, or by hand",
                p.display()
            ),
        },
        Cmd::Report { since, format } => {
            println!(
                "{}",
                render_report(&copies, *since, *format, ticket_url.as_deref())
            );
        }
        Cmd::Rm { copy, yes } => {
            let target = find(&copies, copy)?;
            let (file, known) = match &target {
                Target::Known(c) => {
                    if c.deleted.is_some() {
                        return Err(format!("copy {} was deleted already", c.short()));
                    }
                    (c.file.clone(), Some((**c).clone()))
                }
                Target::Untracked(p) => (p.clone(), None),
            };
            if !present(&file) {
                if let Some(c) = &known {
                    // Gone already (deleted by hand): record it, so the
                    // account says so.
                    let mut e = Event::new(Kind::Delete, now, &journal::machine_id());
                    e.copy = Some(c.id.clone());
                    e.file = Some(file.display().to_string());
                    e.why = Some("already gone from disk".into());
                    let _ = journal::append(&e);
                    println!(
                        "{} was already gone; the journal now says copy {} is deleted",
                        file.display(),
                        c.short()
                    );
                    return Ok(());
                }
                return Err(format!("{} isn't there", file.display()));
            }
            if !yes {
                let size = crate::pgtool::size_of(crate::pgtool::path_size(&file));
                let prompt = format!("delete {} ({size})? [y/N] ", file.display());
                if !crate::pgtool::confirm_cli(&prompt, "y")? {
                    return Err("not confirmed; nothing deleted".to_string());
                }
            }
            delete_dump(&file)?;
            let mut e = Event::new(Kind::Delete, now, &journal::machine_id());
            e.copy = known.as_ref().map(|c| c.id.clone());
            e.file = Some(file.display().to_string());
            e.why = Some("by hand".into());
            let warn = journal::append(&e);
            println!("deleted {}", file.display());
            if let Some(c) = &known {
                if let Some(l) = fix_latest(&copies, c) {
                    println!("{l}");
                }
            }
            if let Some(w) = warn {
                eprintln!("pgman copies: {w}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    fn dump_event(id: &str, ts: u64, label: &str, file: &str) -> Event {
        let mut e = Event::new(Kind::Dump, ts, "m-000000000000");
        e.copy = Some(id.into());
        e.label = Some(label.into());
        e.file = Some(file.into());
        e.mode = Some("raw".into());
        e.bytes = Some(2048);
        e.expires = Some(ts + 86_400);
        e
    }

    #[test]
    fn tickets_come_out_of_branch_names() {
        let none: Vec<String> = vec![];
        assert_eq!(
            ticket_in("feature/APP-101-invoice-rounding", &none).as_deref(),
            Some("APP-101")
        );
        assert_eq!(ticket_in("app-7_fix", &none).as_deref(), Some("APP-7"));
        assert_eq!(
            ticket_in("bugfix/ops2-33", &none).as_deref(),
            Some("OPS2-33")
        );
        for no in [
            "main",
            "release/1.2-3",
            "fix-utf-8-names",
            "v-2",
            "x-12",
            "APP-",
            "APP-12b",
            "2024-10-28",
        ] {
            assert_eq!(ticket_in(no, &none), None, "{no}");
        }
        let keys = vec!["APP".to_string()];
        assert_eq!(
            ticket_in("OPS-1-and-APP-2", &keys).as_deref(),
            Some("APP-2")
        );
        assert_eq!(ticket_in("OPS-1", &keys), None);
    }

    #[test]
    fn a_ticket_links_where_the_project_says() {
        assert_eq!(
            ticket_link(Some("https://jira.example/browse/{ticket}"), "APP-1").as_deref(),
            Some("https://jira.example/browse/APP-1")
        );
        assert_eq!(ticket_link(Some("https://jira.example/"), "APP-1"), None);
        assert_eq!(ticket_link(None, "APP-1"), None);
    }

    #[test]
    fn the_reason_prompt_offers_the_branch_then_the_last() {
        assert_eq!(
            reason_default(Some("APP-1"), Some("old")).as_deref(),
            Some("APP-1")
        );
        assert_eq!(reason_default(None, Some("old")).as_deref(), Some("old"));
        assert_eq!(reason_default(None, None), None);
        let none: Vec<String> = vec![];
        assert_eq!(
            read_reason("", Some("APP-1"), &none),
            Ok((Some("APP-1".into()), Some("APP-1".into())))
        );
        assert_eq!(
            read_reason("repro for APP-9", Some("APP-1"), &none),
            Ok((Some("repro for APP-9".into()), Some("APP-9".into())))
        );
        assert_eq!(read_reason("  ", None, &none), Ok((None, None)));
        assert!(read_reason("a\u{7}b", None, &none).is_err());
    }

    #[test]
    fn the_registry_follows_a_copy_through_its_life() {
        let mut failed = dump_event("bad", 50, "production/ledger", "/d/x.dump");
        failed.ok = Some(false);
        let mut a = dump_event("aaaa1111", 100, "production/ledger", "/d/a.dump");
        a.reason = Some("APP-1".into());
        let b = dump_event("bbbb2222", 200, "staging/ledger", "/d/b.dump");
        let mut r = Event::new(Kind::Restore, 300, "m");
        r.copy = Some("aaaa1111".into());
        r.into_server = Some("localhost:55432".into());
        r.into_database = Some("ledger_dev".into());
        let mut d = Event::new(Kind::Delete, 400, "m");
        d.copy = Some("aaaa1111".into());
        d.why = Some("by hand".into());
        let copies = registry(&[failed, a, b, r, d]);
        assert_eq!(copies.len(), 2);
        assert_eq!(copies[0].restores.len(), 1);
        assert_eq!(copies[0].restores[0].database, "ledger_dev");
        assert_eq!(copies[0].deleted, Some((400, "by hand".into())));
        assert!(copies[1].deleted.is_none());
        assert_eq!(last_reason(&copies, "production/ledger"), Some("APP-1"));
        assert_eq!(last_reason(&copies, "staging/ledger"), None);
        assert_eq!(
            by_file(&copies, Path::new("/d/b.dump")).unwrap().id,
            "bbbb2222"
        );
        assert!(
            by_file(&copies, Path::new("/d/a.dump")).is_none(),
            "deleted"
        );
    }

    #[test]
    fn a_copy_is_picked_by_a_unique_prefix() {
        let copies = registry(&[
            dump_event("abcd1111", 1, "a", "/a"),
            dump_event("abcd2222", 2, "b", "/b"),
        ]);
        assert_eq!(by_prefix(&copies, "abcd1").unwrap().id, "abcd1111");
        assert!(by_prefix(&copies, "abcd")
            .unwrap_err()
            .contains("give more"));
        assert!(by_prefix(&copies, "abc").unwrap_err().contains("four"));
        assert!(by_prefix(&copies, "ffff").unwrap_err().contains("no copy"));
        assert!(by_prefix(&copies, "zzzz").unwrap_err().contains("four"));
    }

    #[test]
    fn dump_names_are_recognised() {
        assert!(dump_named("staging-ledger-20261028T140322Z.dump"));
        assert!(dump_named("x-20261028T140322Z.dir"));
        assert!(!dump_named("staging-ledger-latest.dump"));
        assert!(!dump_named("staging-ledger-20261028T140322Z.txt"));
        assert!(!dump_named("notes.dump"));
        assert!(!dump_named("x-2026102XT140322Z.dump"));
    }

    #[test]
    fn times_are_said_to_the_nearest_day() {
        assert_eq!(relative(100, 100), "just now");
        assert_eq!(relative(100 + 14 * 86_400, 100 + 30), "in 14 days");
        assert_eq!(relative(100, 100 + 3 * 86_400), "3 days ago");
        assert_eq!(relative(100, 100 + 30 * 60), "30 min ago");
    }

    #[test]
    fn the_branch_is_read_before_the_first_commit() {
        let dir = scratch("branch");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        // No git on this machine: nothing to read a branch with.
        if !git(&["init", "-q", "-b", "feature/APP-4-x"]) {
            return;
        }
        assert_eq!(git_branch(&dir).as_deref(), Some("feature/APP-4-x"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            git_branch(&std::env::temp_dir().join("pgman-no-such-dir")),
            None
        );
    }

    #[test]
    fn dates_parse_to_the_start_of_the_day() {
        assert_eq!(parse_date("1970-01-01"), Ok(0));
        assert_eq!(parse_date("2026-10-28"), Ok(1_793_145_600));
        assert_eq!(
            crate::app::format_unix_secs_utc(parse_date("2024-02-29").unwrap()),
            "2024-02-29T00:00:00Z"
        );
        for bad in ["2026-13-01", "2026-10", "yesterday", "1969-12-31"] {
            assert!(parse_date(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_list_shows_what_is_kept_and_what_expired() {
        let mut a = dump_event("aaaa1111ffff", 100, "production/ledger", "/d/a.dump");
        a.protected = true;
        a.ticket = Some("APP-1".into());
        let b = dump_event("bbbb2222ffff", 200_000, "staging/ledger", "/d/b.dump");
        let copies = registry(&[a, b]);
        let now = 150_000;
        let text = render_list(
            &copies,
            &[PathBuf::from("./old-20250101T000000Z.dump")],
            &|_| true,
            false,
            now,
        );
        assert!(text.starts_with("ID"), "{text}");
        assert!(text.contains("aaaa1111"), "{text}");
        assert!(!text.contains("aaaa1111ffff"), "{text}");
        assert!(text.contains("raw, protected"), "{text}");
        assert!(text.contains("EXPIRED"), "{text}");
        assert!(text.contains("APP-1"), "{text}");
        assert!(text.contains("1 past its expiry"), "{text}");
        assert!(text.contains("old-20250101T000000Z.dump"), "{text}");
        let gone = render_list(&copies, &[], &|_| false, false, now);
        assert!(gone.contains("missing"), "{gone}");
    }

    #[test]
    fn the_report_is_the_whole_account() {
        let mut a = dump_event("aaaa1111", 1_793_145_600, "production/ledger", "/d/a.dump");
        a.protected = true;
        a.ticket = Some("APP-1".into());
        a.reason = Some("APP-1".into());
        let mut d = Event::new(Kind::Delete, 1_793_232_000, "m");
        d.copy = Some("aaaa1111".into());
        d.why = Some("by hand".into());
        let copies = registry(&[a, d]);
        let md = render_report(
            &copies,
            0,
            Format::Markdown,
            Some("https://jira.example/browse/{ticket}"),
        );
        assert!(
            md.contains("| 2026-10-28 00:00Z | production/ledger | raw, protected |"),
            "{md}"
        );
        assert!(
            md.contains("[APP-1](https://jira.example/browse/APP-1)"),
            "{md}"
        );
        assert!(md.contains("deleted 2026-10-29 00:00Z (by hand)"), "{md}");
        let text = render_report(&copies, 0, Format::Text, None);
        assert!(text.contains("— APP-1\n"), "{text}");
        let mut b = dump_event("bbbb2222", 1_793_145_600, "staging/ledger", "/d/b.dump");
        b.ticket = Some("APP-2".into());
        b.reason = Some("APP-2 nightly refresh".into());
        let mut c = dump_event("cccc3333", 1_793_145_600, "staging/ledger", "/d/c.dump");
        c.ticket = Some("APP-3".into());
        c.reason = Some("totals".into());
        let both = registry(&[b, c]);
        let md = render_report(
            &both,
            0,
            Format::Markdown,
            Some("https://jira.example/browse/{ticket}"),
        );
        assert!(
            md.contains("| [APP-2](https://jira.example/browse/APP-2) nightly refresh |"),
            "the ticket once, as a link: {md}"
        );
        assert!(
            md.contains("| [APP-3](https://jira.example/browse/APP-3): totals |"),
            "{md}"
        );
        assert_eq!(
            render_report(&copies, 1_793_145_601, Format::Text, None),
            "no copies in that time"
        );
        let json: serde_json::Value =
            serde_json::from_str(&render_report(&copies, 0, Format::Json, None)).unwrap();
        assert_eq!(json[0]["id"], "aaaa1111");
    }

    #[test]
    fn subcommands_parse() {
        assert_eq!(parse_cli(&[]), Ok(Cmd::List { all: false }));
        assert_eq!(parse_cli(&args(&["--all"])), Ok(Cmd::List { all: true }));
        assert_eq!(
            parse_cli(&args(&["rm", "--yes", "3f9a"])),
            Ok(Cmd::Rm {
                copy: "3f9a".into(),
                yes: true
            })
        );
        assert!(parse_cli(&args(&["rm"])).is_err());
        assert!(parse_cli(&args(&["show", "a", "b"])).is_err());
        assert_eq!(
            parse_cli(&args(&[
                "report",
                "--format",
                "md",
                "--since",
                "1970-01-02"
            ])),
            Ok(Cmd::Report {
                since: 86_400,
                format: Format::Markdown
            })
        );
        assert!(parse_cli(&args(&["report", "--since"])).is_err());
        assert!(parse_cli(&args(&["frob"])).is_err());
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pgman-copies-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn rm_repoints_latest_and_never_follows_links() {
        let dir = scratch("rm");
        let old = dir.join("s-l-20261001T000000Z.dump");
        let new = dir.join("s-l-20261002T000000Z.dump");
        let latest = dir.join("s-l-latest.dump");
        std::fs::write(&old, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        std::os::unix::fs::symlink("s-l-20261002T000000Z.dump", &latest).unwrap();
        let mut a = dump_event("aaaa1111", 1, "s/l", old.to_str().unwrap());
        a.latest = Some(latest.display().to_string());
        let mut b = dump_event("bbbb2222", 2, "s/l", new.to_str().unwrap());
        b.latest = Some(latest.display().to_string());
        let copies = registry(&[a, b]);

        // By the -latest link: the copy it points at.
        let Target::Known(c) = find(&copies, latest.to_str().unwrap()).unwrap() else {
            panic!("not known")
        };
        assert_eq!(c.id, "bbbb2222");
        delete_dump(&c.file).unwrap();
        let said = fix_latest(&copies, &c).unwrap();
        assert!(
            said.contains("now points at s-l-20261001T000000Z.dump"),
            "{said}"
        );
        assert_eq!(
            std::fs::read_link(&latest).unwrap(),
            PathBuf::from("s-l-20261001T000000Z.dump")
        );

        // The last one: the link goes.
        let left = registry(&[{
            let mut a = dump_event("aaaa1111", 1, "s/l", old.to_str().unwrap());
            a.latest = Some(latest.display().to_string());
            a
        }]);
        delete_dump(&old).unwrap();
        let said = fix_latest(&left, &left[0]).unwrap();
        assert!(said.contains("removed"), "{said}");
        assert!(std::fs::symlink_metadata(&latest).is_err());

        // A link where the dump was is never followed.
        let victim = dir.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let planted = dir.join("p-20261003T000000Z.dump");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        assert!(delete_dump(&planted).unwrap_err().contains("a link"));
        assert!(victim.exists());

        // A directory that isn't a dump isn't deleted.
        let notdump = dir.join("q-20261004T000000Z.dir");
        std::fs::create_dir(&notdump).unwrap();
        assert!(delete_dump(&notdump).unwrap_err().contains("toc.dat"));
        std::fs::write(notdump.join("toc.dat"), "").unwrap();
        delete_dump(&notdump).unwrap();
        assert!(!notdump.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn untracked_dumps_are_found_and_known_ones_are_not() {
        let dir = scratch("untracked");
        let known = dir.join("a-20261001T000000Z.dump");
        let loose = dir.join("b-20261001T000000Z.dump");
        std::fs::write(&known, "").unwrap();
        std::fs::write(&loose, "").unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        std::os::unix::fs::symlink(&loose, dir.join("b-latest.dump")).unwrap();
        let copies = registry(&[dump_event("aaaa1111", 1, "a", known.to_str().unwrap())]);
        assert_eq!(untracked(&dir, &copies), vec![loose.clone()]);
        assert_eq!(
            find(&copies, loose.to_str().unwrap()).unwrap(),
            Target::Untracked(loose)
        );
        assert!(find(&copies, dir.join("notes.txt").to_str().unwrap())
            .unwrap_err()
            .contains("isn't named like"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
