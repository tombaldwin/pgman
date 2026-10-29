//! The journal: one JSON line per step in a copy's life — a dump taken,
//! restored, deleted — appended to `journal.jsonl` in pgman's data
//! directory. It is the developer's own record of what they took, why,
//! and when it was gone, and what `pgman copies` is built from. See
//! `docs/design/masked-dumps.md`, sections 6 and 7.
//!
//! It holds no values and no secrets: names, sizes, times, a reason.
//! Appends are one `write` to a file opened `O_APPEND`, so the CLI, the
//! TUI and the MCP server can all add to it at once.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE: &str = "journal.jsonl";
const VERSION: u32 = 1;
/// Longest reason kept, in characters.
pub const REASON_MAX: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Dump,
    Restore,
    Delete,
    /// An event a newer pgman writes: kept, not understood.
    #[serde(other)]
    Other,
}

/// One line of the journal. Which fields are set depends on `event`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub v: u32,
    /// This event's id.
    pub id: String,
    pub event: Option<Kind>,
    /// Unix seconds, and the same as UTC text for a reader.
    pub ts: u64,
    pub at: String,
    /// The dump this is about: its id, given when it started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// How the target was named: `production/ledger`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub protected: bool,
    /// `raw` or `schema`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `cli`, `unattended` or `mcp`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// The dump's path on this machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The `-latest` link pointed at it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// The dump's fingerprint ([`crate::copies::fingerprint`]), to know
    /// it again copied or moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub print: Option<String>,
    /// When the copy expires, unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<u64>,
    /// A dump that failed: nothing written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// A restore's target: server and database.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub into_server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub into_database: Option<String>,
    /// The restore's target as named (`local`, a URL without its
    /// password), to reach it again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub into_target: Option<String>,
    /// Why something was deleted: `by hand`, `expired`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// This machine's random id: which machine, without its name.
    pub machine: String,
}

impl Event {
    /// A new event of `kind` at `ts`, with a fresh id. Reads randomness.
    pub fn new(kind: Kind, ts: u64, machine: &str) -> Event {
        Event {
            v: VERSION,
            id: new_id(),
            event: Some(kind),
            ts,
            at: crate::app::format_unix_secs_utc(ts),
            machine: machine.to_string(),
            ..Event::default()
        }
    }
}

/// Seconds since the epoch, now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 16 random bytes, from the operating system.
fn random16() -> [u8; 16] {
    let mut b = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b));
    if read.is_err() {
        // No /dev/urandom: the time and process, hashed. Unique enough
        // for an id; never used as a key.
        use std::hash::{BuildHasher, Hasher};
        let s = std::collections::hash_map::RandomState::new();
        for half in b.chunks_mut(8) {
            let mut h = s.build_hasher();
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            );
            h.write_u32(std::process::id());
            half.copy_from_slice(&h.finish().to_le_bytes());
        }
    }
    b
}

/// A version-4 uuid. Reads randomness.
pub fn new_id() -> String {
    uuid_text(random16())
}

/// `bytes` as a version-4 uuid. Pure.
fn uuid_text(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// `reason` as the journal keeps it, or why it can't be: one line, no
/// control characters, at most [`REASON_MAX`] characters. Empty is
/// `None`. Pure.
pub fn clean_reason(reason: &str) -> Result<Option<String>, String> {
    let r = reason.trim();
    if r.is_empty() {
        return Ok(None);
    }
    if r.chars().any(char::is_control) {
        return Err("a reason is one line, with no control characters".to_string());
    }
    if r.chars().count() > REASON_MAX {
        return Err(format!(
            "a reason is at most {REASON_MAX} characters: a ticket id, or a few words"
        ));
    }
    Ok(Some(r.to_string()))
}

/// This machine's id in `dir`, made the first time: `m-` and 12 random
/// hex digits, so events say which machine without naming it. Reads and
/// may write `dir`.
pub fn machine_id_in(dir: &Path) -> String {
    let path = dir.join("machine-id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let t = text.trim();
        if t.starts_with("m-") && t.len() == 14 {
            return t.to_string();
        }
    }
    let id = format!(
        "m-{}",
        random16()[..6]
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect::<String>()
    );
    if let Err(e) = crate::util::write_private(&path, &id) {
        tracing::warn!(target: "pgman::journal", "could not save the machine id: {e}");
    }
    id
}

/// Append `event` to the journal in `dir`. Writes `dir`.
pub fn append_in(dir: &Path, event: &Event) -> Result<(), String> {
    crate::util::ensure_private_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(FILE);
    let mut line = serde_json::to_string(event).map_err(|e| format!("journal: {e}"))?;
    line.push('\n');
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Every event in the journal in `dir`, oldest first, and how many lines
/// couldn't be read. A missing journal is empty. Reads `dir`.
pub fn read_in(dir: &Path) -> (Vec<Event>, usize) {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else {
        return (Vec::new(), 0);
    };
    parse(&text)
}

/// The journal's text as events, and the count of lines that aren't
/// one. Pure.
pub fn parse(text: &str) -> (Vec<Event>, usize) {
    let mut bad = 0;
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Event>(line) {
            Ok(e) if e.event.is_some() => out.push(e),
            _ => bad += 1,
        }
    }
    (out, bad)
}

/// pgman's data directory, where the journal lives.
pub fn dir() -> PathBuf {
    crate::util::data_dir()
}

/// [`append_in`] the data directory, logging rather than failing: a
/// journal that can't be written never undoes the work it records.
pub fn append(event: &Event) -> Option<String> {
    match append_in(&dir(), event) {
        Ok(()) => None,
        Err(e) => {
            tracing::warn!(target: "pgman::journal", "{e}");
            Some(format!("the journal couldn't be written ({e})"))
        }
    }
}

/// This machine's id, in the data directory.
pub fn machine_id() -> String {
    machine_id_in(&dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pgman-journal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_uuid_is_version_4() {
        let u = uuid_text([0xff; 16]);
        assert_eq!(u, "ffffffff-ffff-4fff-bfff-ffffffffffff");
        let a = new_id();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert_ne!(a, new_id());
    }

    #[test]
    fn reasons_are_one_short_line() {
        assert_eq!(clean_reason("  APP-101  "), Ok(Some("APP-101".into())));
        assert_eq!(clean_reason("   "), Ok(None));
        assert!(clean_reason("a\nb").is_err());
        assert!(clean_reason("a\u{1b}[2Jb").is_err());
        assert!(clean_reason(&"x".repeat(REASON_MAX + 1)).is_err());
        assert!(clean_reason(&"é".repeat(REASON_MAX)).is_ok());
    }

    #[test]
    fn events_round_trip_and_bad_lines_are_counted() {
        let dir = scratch("roundtrip");
        let mut e = Event::new(Kind::Dump, 1_800_000_000, "m-000000000000");
        e.copy = Some("c1".into());
        e.reason = Some("APP-101".into());
        e.protected = true;
        append_in(&dir, &e).unwrap();
        let mut d = Event::new(Kind::Delete, 1_800_000_100, "m-000000000000");
        d.copy = Some("c1".into());
        append_in(&dir, &d).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(FILE))
            .unwrap()
            .write_all(b"not json\n{\"v\":1}\n{\"v\":2,\"event\":\"rules_confirmed\",\"ts\":5}\n")
            .unwrap();
        let (events, bad) = read_in(&dir);
        assert_eq!(bad, 2, "a newer pgman's event isn't a bad line");
        assert_eq!(events.len(), 3);
        assert_eq!(events[..2], [e, d]);
        assert_eq!(events[2].event, Some(Kind::Other));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let line = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(line.contains("\"at\":\"2027-01-15T08:00:00Z\""), "{line}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_machine_id_is_made_once() {
        let dir = scratch("machine");
        let a = machine_id_in(&dir);
        assert!(a.starts_with("m-") && a.len() == 14, "{a}");
        assert_eq!(machine_id_in(&dir), a);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
