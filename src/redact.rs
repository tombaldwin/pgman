//! Redaction mode (`pgman --redact`, `:redact`, `redact_default`).
//!
//! For screenshots of a real database: where it lives is blocked out,
//! what is in it is not. Hosts, IPs, AWS account IDs, ARNs, URL user
//! names, your login name (and so your home directory in a path) and
//! the names pgman knows from its own state (environments, RDS
//! instances, bastions, secrets, profiles) become `▓` blocks of the
//! same width; schema and data stay readable.
//!
//! It works on the finished frame: [`apply`] runs over every row of the
//! buffer after `ui::draw` has rendered it, so a hostname in an error
//! footer is caught the same as one in the connection card, and a new
//! panel needs no call of its own. Display only — the clipboard, the
//! editor's real text, logs and files are untouched.
//!
//! Names made of segments keep the generic ones (`prod`, `replica`,
//! `eu-west-2`, `rds.amazonaws.com`), so `acme-prod-replica` shows as
//! `▓▓▓▓-prod-replica` and a screenshot still says which database it is.
//!
//! [`Redactor`] is pure; [`app_names`] reads the names out of an `App`.

use std::ops::Range;

use ratatui::buffer::Buffer;
use unicode_width::UnicodeWidthStr;

/// The block every redacted column becomes (as ebman draws it).
pub const BLOCK: &str = "▓";

/// Segments that say what a server is, not whose: kept visible inside
/// a redacted name. Compared lowercased.
const GENERIC: &[&str] = &[
    "prod",
    "production",
    "prd",
    "dev",
    "development",
    "staging",
    "stage",
    "stg",
    "test",
    "uat",
    "qa",
    "demo",
    "local",
    "localhost",
    "primary",
    "replica",
    "read",
    "only",
    "readonly",
    "reader",
    "writer",
    "ro",
    "rw",
    "db",
    "database",
    "rds",
    "aurora",
    "cluster",
    "instance",
    "postgres",
    "postgresql",
    "pg",
    "main",
    "master",
    "standby",
    "blue",
    "green",
    "eu",
    "us",
    "ap",
    "sa",
    "ca",
    "me",
    "af",
    "il",
    "gov",
    "cn",
    "north",
    "south",
    "east",
    "west",
    "central",
    "northeast",
    "northwest",
    "southeast",
    "southwest",
    "amazonaws",
    "com",
    "aws",
    "arn",
    "secretsmanager",
    "secret",
    "ssm",
    "parameter",
    "credentials",
    "creds",
    "password",
    "user",
    "admin",
    "app",
    "root",
    "internal",
    "proxy",
    "endpoint",
    "bastion",
    "jump",
    "tunnel",
    "ec2",
    "compute",
    "docker",
    "default",
    "public",
];

/// Shortest segment matched on its own, away from the rest of its name.
/// Shorter ones (`ab`) would black out ordinary words all over the
/// screen; they are still redacted inside a full match.
const MIN_SEGMENT: usize = 3;

/// What to redact on one frame: the patterns (built in) plus the names
/// pgman knows from its own state.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// Whole names, lowercased. Order doesn't matter: overlapping
    /// matches are merged.
    names: Vec<String>,
    /// Their non-generic segments, lowercased, matched as whole words.
    segments: Vec<String>,
}

impl Redactor {
    /// A redactor for these known names (hosts, environment names,
    /// secret ids…). Empty and all-generic names are dropped.
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = Redactor::default();
        for name in names {
            let name = name.as_ref().trim().to_ascii_lowercase();
            let segs: Vec<&str> = alnum_runs(&name)
                .map(|r| &name[r])
                .filter(|s| !is_generic(s))
                .collect();
            // A one- or two-letter name (`u`) would black out that word
            // all over the screen, the same as a short segment would.
            if segs.is_empty() || name.chars().count() < MIN_SEGMENT {
                continue;
            }
            for s in segs {
                if s.chars().count() >= MIN_SEGMENT && !out.segments.iter().any(|x| x == s) {
                    out.segments.push(s.to_string());
                }
            }
            if !out.names.contains(&name) {
                out.names.push(name);
            }
        }
        out
    }

    /// Byte ranges of `line` to block out, sorted and merged. Always on
    /// char boundaries.
    pub fn ranges(&self, line: &str) -> Vec<Range<usize>> {
        // ASCII lowercasing keeps every byte offset where it was.
        let lower = line.to_ascii_lowercase();
        let mut out = Vec::new();
        account_ids(&lower, &mut out);
        ipv4(&lower, &mut out);
        ipv6(&lower, &mut out);
        aws_hosts(&lower, &mut out);
        arns(&lower, &mut out);
        url_userinfo(&lower, &mut out);
        for name in &self.names {
            for at in word_matches(&lower, name) {
                segment_ranges(&lower[at..at + name.len()], at, &mut out);
            }
        }
        for seg in &self.segments {
            for at in word_matches(&lower, seg) {
                out.push(at..at + seg.len());
            }
        }
        merge(out)
    }

    /// `line` with its redacted chars replaced by [`BLOCK`], one per
    /// display column. What [`apply`] does to a buffer row.
    pub fn redact(&self, line: &str) -> String {
        let ranges = self.ranges(line);
        let mut out = String::with_capacity(line.len());
        for (i, c) in line.char_indices() {
            if ranges.iter().any(|r| r.contains(&i)) {
                let w = UnicodeWidthStr::width(c.encode_utf8(&mut [0; 4]) as &str).max(1);
                out.push_str(&BLOCK.repeat(w));
            } else {
                out.push(c);
            }
        }
        out
    }
}

/// Block out, in place, every cell of `buf` that `r` redacts. Each row
/// is read as the text it shows (a wide char's trailing cell skipped,
/// as ratatui draws it), and a redacted wide char becomes one block
/// per column it covers, so nothing shifts. Styles are kept.
pub fn apply(buf: &mut Buffer, r: &Redactor) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut text = String::new();
        // (byte offset in `text`, x, columns)
        let mut cells: Vec<(usize, u16, u16)> = Vec::new();
        let mut x = area.left();
        while x < area.right() {
            let sym = buf[(x, y)].symbol();
            let w = (UnicodeWidthStr::width(sym) as u16).max(1);
            cells.push((text.len(), x, w));
            text.push_str(sym);
            x = x.saturating_add(w);
        }
        let ranges = r.ranges(&text);
        if ranges.is_empty() {
            continue;
        }
        for (i, &(start, x, w)) in cells.iter().enumerate() {
            let end = cells.get(i + 1).map_or(text.len(), |c| c.0);
            if ranges.iter().any(|r| r.start < end && start < r.end) {
                for dx in 0..w {
                    if x + dx < area.right() {
                        buf[(x + dx, y)].set_symbol(BLOCK);
                    }
                }
            }
        }
    }
}

/// Every infrastructure name `app` knows: its connection, the picker's
/// data sources, the environments overview (servers, hosts, databases,
/// bastions, secrets, password variables) and an open discovery, plus
/// [`env_names`]. Fed to [`Redactor::new`] on each frame.
pub fn app_names(app: &crate::app::App) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let dsn = |d: &crate::conn::Dsn, out: &mut Vec<String>| {
        out.push(d.host.clone());
        out.push(d.dbname.clone());
        out.extend(d.user.clone());
        if let Some(t) = &d.ssh_tunnel {
            out.push(t.host.clone());
            out.extend(t.user.clone());
        }
    };
    if let Some(d) = &app.dsn {
        dsn(d, &mut out);
    }
    for p in &app.conn_pick.picks {
        out.push(p.name.clone());
        if let Some(d) = &p.dsn {
            dsn(d, &mut out);
        }
    }
    for e in &app.environments.reports {
        out.push(e.name.clone());
        for t in &e.targets {
            out.push(t.server.clone());
            out.push(t.database.clone());
            out.extend(t.replica_of.clone());
            out.extend(t.host.clone());
            if let Ok(spec) = &t.connect {
                dsn(&spec.dsn, &mut out);
                if let Some(s) = &spec.secret {
                    out.push(s.id.clone());
                    out.extend(s.profile.clone());
                }
                out.extend(spec.password_env.clone());
            }
        }
    }
    if let Some(found) = app
        .environments
        .discovery
        .as_ref()
        .and_then(|d| d.found.as_ref())
    {
        for s in &found.scopes {
            out.extend(s.profile.clone());
        }
        for p in &found.proposals {
            out.push(p.env.name.clone());
            out.extend(p.servers.iter().cloned());
            out.extend(p.env.ssh_tunnel.clone());
            out.extend(p.env.user.clone());
            out.extend(p.env.password_env.clone());
            out.extend(p.env.secrets.values().cloned());
            if let Some(scope) = p.env.source.rds_scope() {
                out.extend(scope.profile);
            }
        }
    }
    // The start card's `databases` line lists every database on the
    // server, not only the one connected to.
    out.extend(app.databases.iter().map(|d| d.name.clone()));
    out.extend(app.redact_env_names.iter().cloned());
    out
}

/// The names that come from the process environment — `AWS_PROFILE`
/// and the login name — read once at startup into
/// `App::redact_env_names`, so drawing a frame reads no environment.
pub fn env_names() -> Vec<String> {
    let mut out: Vec<String> = std::env::var("AWS_PROFILE").ok().into_iter().collect();
    out.extend(login_names(
        std::env::var_os("HOME").map(std::path::PathBuf::from),
        std::env::var("USER").ok(),
    ));
    out
}

/// `redact_default = true` in the personal `config.toml`: start every
/// TUI session redacted. A missing file is off; a file that doesn't
/// parse is ON — this is a privacy setting, so it fails closed — and
/// the error comes back to say why.
pub fn config_default() -> (bool, Option<String>) {
    let path = crate::util::config_file("config.toml");
    config_default_from(std::fs::read_to_string(&path).ok().as_deref(), &path)
}

/// [`config_default`] given the file's text (`None`: no file).
fn config_default_from(text: Option<&str>, path: &std::path::Path) -> (bool, Option<String>) {
    match text.map(parse_config) {
        None => (false, None),
        Some(Ok(on)) => (on, None),
        Some(Err(e)) => (
            true,
            Some(format!(
                "{}: {e} — redaction is on until it parses",
                path.display()
            )),
        ),
    }
}

/// The pure half of [`config_default`]. Other keys are ignored.
pub fn parse_config(text: &str) -> Result<bool, String> {
    #[derive(serde::Deserialize)]
    struct Config {
        #[serde(default)]
        redact_default: bool,
    }
    toml::from_str::<Config>(text)
        .map(|c| c.redact_default)
        .map_err(|e| e.message().to_string())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_generic(seg: &str) -> bool {
    seg.chars().all(|c| c.is_ascii_digit()) || GENERIC.contains(&seg)
}

/// The char before byte `at` and the char at byte `end`.
fn neighbours(s: &str, at: usize, end: usize) -> (Option<char>, Option<char>) {
    (s[..at].chars().next_back(), s[end..].chars().next())
}

/// Starts of `needle` in `hay` that aren't part of a longer word.
fn word_matches<'a>(hay: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    hay.match_indices(needle).map(|(i, _)| i).filter(move |&i| {
        let (before, after) = neighbours(hay, i, i + needle.len());
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// Maximal runs of alphanumeric chars.
fn alnum_runs(s: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut start: Option<usize> = None;
    let mut chars = s.char_indices().chain(std::iter::once((s.len(), ' ')));
    std::iter::from_fn(move || {
        for (i, c) in chars.by_ref() {
            match (start, c.is_alphanumeric()) {
                (None, true) => start = Some(i),
                (Some(st), false) => {
                    start = None;
                    return Some(st..i);
                }
                _ => {}
            }
        }
        None
    })
}

/// The non-generic segments of `name` (at `base` in the line).
fn segment_ranges(name: &str, base: usize, out: &mut Vec<Range<usize>>) {
    for r in alnum_runs(name) {
        if !is_generic(&name[r.clone()]) {
            out.push(base + r.start..base + r.end);
        }
    }
}

/// Maximal runs of chars satisfying `f`.
fn runs(s: &str, f: impl Fn(char) -> bool) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match (start, f(c)) {
            (None, true) => start = Some(i),
            (Some(st), false) => {
                out.push(st..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push(st..s.len());
    }
    out
}

/// Standalone 12-digit numbers, and the console's `1234-5678-9012`.
/// A dash may sit next to one (`prod-210987654321`); a digit, letter
/// or underscore may not.
fn account_ids(s: &str, out: &mut Vec<Range<usize>>) {
    let standalone = |r: &Range<usize>, dash_ok: bool| {
        let (before, after) = neighbours(s, r.start, r.end);
        let bad = |c: char| is_word(c) || (!dash_ok && c == '-');
        !before.is_some_and(bad) && !after.is_some_and(bad)
    };
    for r in runs(s, |c| c.is_ascii_digit()) {
        if r.len() == 12 && standalone(&r, true) {
            out.push(r);
        }
    }
    for r in runs(s, |c| c.is_ascii_digit() || c == '-') {
        let pieces: Vec<&str> = s[r.clone()].split('-').collect();
        if pieces.len() == 3 && pieces.iter().all(|p| p.len() == 4) && standalone(&r, false) {
            out.push(r);
        }
    }
}

/// Dotted-quad IPv4 addresses, except loopback and `0.0.0.0`, which say
/// nothing about anyone.
fn ipv4(s: &str, out: &mut Vec<Range<usize>>) {
    for r in runs(s, |c| c.is_ascii_digit() || c == '.') {
        let run = s[r.clone()].trim_end_matches('.');
        let (before, after) = neighbours(s, r.start, r.start + run.len());
        if before.is_some_and(is_word) || after.is_some_and(is_word) {
            continue;
        }
        let parts: Vec<&str> = run.split('.').collect();
        let quad = parts.len() == 4
            && parts
                .iter()
                .all(|p| (1..=3).contains(&p.len()) && p.parse::<u8>().is_ok());
        if quad && !run.starts_with("127.") && run != "0.0.0.0" {
            out.push(r.start..r.start + run.len());
        }
    }
}

/// IPv6 literals: hex groups with `::`, or eight full groups. Times
/// (`17:38:30`) and Postgres casts (`x::int`) don't qualify; `::1` is
/// loopback and stays.
fn ipv6(s: &str, out: &mut Vec<Range<usize>>) {
    for r in runs(s, |c| c.is_ascii_hexdigit() || c == ':') {
        let run = &s[r.clone()];
        let (before, after) = neighbours(s, r.start, r.end);
        if before.is_some_and(is_word) || after.is_some_and(is_word) {
            continue;
        }
        let groups: Vec<&str> = run.split(':').collect();
        let compressed = run.matches("::").count() == 1 && !run.contains(":::");
        let shaped = groups.len() <= 8
            && groups.iter().all(|g| g.len() <= 4)
            && groups.iter().filter(|g| !g.is_empty()).count() >= 2;
        let full = groups.len() == 8 && groups.iter().all(|g| !g.is_empty());
        if shaped && (compressed || full) {
            out.push(r);
        }
    }
}

/// Hostnames under `amazonaws.com` (and `.com.cn`), through the
/// segment rule: `acme.c1x2y3.eu-west-2.rds.amazonaws.com` keeps its
/// region and service.
fn aws_hosts(s: &str, out: &mut Vec<Range<usize>>) {
    for (at, m) in s.match_indices(".amazonaws.com") {
        let host_char = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
        let start = s[..at]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| host_char(c))
            .last()
            .map_or(at, |(i, _)| i);
        let end = at + m.len();
        segment_ranges(&s[start..end], start, out);
    }
}

/// `arn:aws…` up to the next space, quote or closing bracket, through
/// the segment rule (the account inside is caught by [`account_ids`]).
fn arns(s: &str, out: &mut Vec<Range<usize>>) {
    for (at, _) in s.match_indices("arn:aws") {
        if neighbours(s, at, at).0.is_some_and(is_word) {
            continue;
        }
        let len = s[at..]
            .find(|c: char| c.is_whitespace() || !c.is_ascii() || "\"'`,)]}>|".contains(c))
            .unwrap_or(s.len() - at);
        segment_ranges(&s[at..at + len], at, out);
    }
}

/// The login name, as a known name: the home directory's last part
/// (what a path on screen shows) and `$USER`, when they differ.
fn login_names(home: Option<std::path::PathBuf>, user: Option<String>) -> Vec<String> {
    let mut out: Vec<String> = home
        .as_deref()
        .and_then(std::path::Path::file_name)
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .into_iter()
        .collect();
    out.extend(user.filter(|u| !out.contains(u)));
    out
}

/// The user (segment rule) and any unmasked password in a URL's
/// `scheme://user:pass@host`.
fn url_userinfo(s: &str, out: &mut Vec<Range<usize>>) {
    for (at, m) in s.match_indices("://") {
        let from = at + m.len();
        let authority = s[from..]
            .find(|c: char| "/?#\"'`".contains(c) || c.is_whitespace())
            .map_or(&s[from..], |n| &s[from..from + n]);
        let Some(userinfo) = authority.rfind('@').map(|n| &authority[..n]) else {
            continue;
        };
        let (user, pass) = userinfo.split_once(':').unwrap_or((userinfo, ""));
        segment_ranges(user, from, out);
        if !pass.is_empty() && !pass.chars().all(|c| c == '*') {
            let p = from + user.len() + 1;
            out.push(p..p + pass.len());
        }
    }
}

fn merge(mut v: Vec<Range<usize>>) -> Vec<Range<usize>> {
    v.retain(|r| r.start < r.end);
    v.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Redactor {
        Redactor::new(Vec::<String>::new())
    }

    #[test]
    fn a_twelve_digit_account_id_is_blocked() {
        assert_eq!(
            none().redact("RDS 210987654321 eu-west-2"),
            "RDS ▓▓▓▓▓▓▓▓▓▓▓▓ eu-west-2"
        );
        assert_eq!(none().redact("(123456789012)"), "(▓▓▓▓▓▓▓▓▓▓▓▓)");
        assert_eq!(none().redact("2109-8765-4321"), "▓▓▓▓▓▓▓▓▓▓▓▓▓▓");
        assert_eq!(
            none().redact("logs-prod-210987654321"),
            "logs-prod-▓▓▓▓▓▓▓▓▓▓▓▓"
        );
    }

    #[test]
    fn app_names_gathers_every_place_a_server_is_named() {
        use crate::app::{App, DataSourcePick};
        use crate::conn::Dsn;
        use crate::creds::aws_secrets::SecretRef;
        use crate::inventory::{ConnectSpec, EnvReport, Target};
        let dsn = |s: &str| Dsn::parse(s).unwrap();
        let picks = vec![DataSourcePick {
            name: "examplecorp-reporting".into(),
            origin: "IntelliJ",
            dsn: Some(dsn("postgres://pickuser@pick.example.net/pickdb")),
            unresolved: Vec::new(),
            unresolved_host: Vec::new(),
            creds: Default::default(),
        }];
        let mut a = App::new(
            crate::theme::Theme::default(),
            Some(dsn(
                "postgres://liveuser@live.example.net/livedb?ssh_tunnel=jumpuser@jump.example.net",
            )),
            picks,
            crate::safety::SafetyConfig::default(),
        );
        a.environments.reports = vec![EnvReport {
            name: "envname".into(),
            source: "RDS".into(),
            targets: vec![Target {
                database: "envdb".into(),
                server: "envserver".into(),
                role: crate::mcp::connections::Role::Replica,
                replica_of: Some("envprimary".into()),
                host: Some("envhost.example.net".into()),
                port: None,
                engine: None,
                status: None,
                class: None,
                multi_az: None,
                connect: Ok(ConnectSpec {
                    dsn: dsn("postgres://specuser@spechost.example.net/specdb"),
                    secret: Some(SecretRef {
                        id: "examplecorp/secretname".into(),
                        region: None,
                        profile: Some("profilename".into()),
                        account: None,
                    }),
                    password_env: Some("EXAMPLECORP_PW".into()),
                }),
            }],
            error: None,
            probe: None,
        }];
        a.redact_env_names = vec!["loginname".into()];
        a.databases = vec![crate::app::DatabaseInfo {
            name: "otherdb".into(),
            size: "1 GB".into(),
        }];
        let names = app_names(&a);
        for want in [
            "loginname",
            "otherdb",
            "live.example.net",
            "liveuser",
            "livedb",
            "jump.example.net",
            "jumpuser",
            "examplecorp-reporting",
            "pick.example.net",
            "pickuser",
            "envname",
            "envdb",
            "envserver",
            "envprimary",
            "envhost.example.net",
            "spechost.example.net",
            "specuser",
            "examplecorp/secretname",
            "profilename",
            "EXAMPLECORP_PW",
        ] {
            assert!(names.iter().any(|n| n == want), "{want} not in {names:?}");
        }
    }

    #[test]
    fn a_twelve_digit_run_inside_a_longer_number_or_word_is_not() {
        for s in [
            "3210987654321",
            "21098765432",
            "x210987654321",
            "210987654321_a",
            "rows 1234567",
            "1-2109-8765-4321",
            "2026-10-21",
        ] {
            assert_eq!(none().redact(s), s, "{s}");
        }
    }

    #[test]
    fn ipv4_is_blocked_but_loopback_and_versions_are_not() {
        assert_eq!(none().redact("host 10.0.12.5:5432"), "host ▓▓▓▓▓▓▓▓▓:5432");
        assert_eq!(none().redact("at 10.0.12.5."), "at ▓▓▓▓▓▓▓▓▓.");
        for s in [
            "127.0.0.1",
            "0.0.0.0",
            "pg 16.10",
            "1.2.3",
            "300.1.1.1",
            "1.2.3.4.5",
        ] {
            assert_eq!(none().redact(s), s, "{s}");
        }
    }

    #[test]
    fn ipv6_is_blocked_but_times_casts_and_loopback_are_not() {
        assert_eq!(
            none().redact("postgres://u@[fe80::1]:5433/db"),
            "postgres://▓@[▓▓▓▓▓▓▓]:5433/db"
        );
        assert_eq!(
            none().redact("2001:db8:0:0:0:0:2:1"),
            "▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓"
        );
        for s in ["17:38:30", "select id::int", "'5'::text", "a::b::c", "::1"] {
            assert_eq!(none().redact(s), s, "{s}");
        }
    }

    #[test]
    fn an_rds_host_keeps_its_region_and_service() {
        assert_eq!(
            none().redact("acme-prod.c1x2y3.eu-west-2.rds.amazonaws.com:5432"),
            "▓▓▓▓-prod.▓▓▓▓▓▓.eu-west-2.rds.amazonaws.com:5432"
        );
    }

    #[test]
    fn an_arn_keeps_its_service_and_loses_its_names_and_account() {
        assert_eq!(
            none().redact("arn:aws:secretsmanager:us-west-1:210987654321:secret:acme/db-AbCd12 ok"),
            "arn:aws:secretsmanager:us-west-1:▓▓▓▓▓▓▓▓▓▓▓▓:secret:▓▓▓▓/db-▓▓▓▓▓▓ ok"
        );
        // Not an ARN without its partition.
        assert_eq!(none().redact("barn:awsome"), "barn:awsome");
    }

    #[test]
    fn the_login_name_is_a_known_name_so_paths_lose_it() {
        let home = std::path::PathBuf::from("base").join("alice");
        let names = login_names(Some(home.clone()), Some("alice".into()));
        assert_eq!(names, ["alice"], "the same name once");
        assert_eq!(login_names(Some(home), Some("al".into())), ["alice", "al"]);
        assert!(login_names(None, None).is_empty());
        let r = Redactor::new(names);
        assert_eq!(
            r.redact("log base/alice/.cache/pgman/pgman.log"),
            "log base/▓▓▓▓▓/.cache/pgman/pgman.log"
        );
    }

    #[test]
    fn a_url_loses_its_user_and_any_unmasked_password() {
        assert_eq!(
            none().redact("postgres://ledger_app:***@db.example:5432/x"),
            "postgres://▓▓▓▓▓▓_app:***@db.example:5432/x"
        );
        assert_eq!(
            none().redact("postgres://postgres:hunter2@h/x"),
            "postgres://postgres:▓▓▓▓▓▓▓@h/x"
        );
        assert_eq!(
            none().redact("https://example.com/a@b"),
            "https://example.com/a@b"
        );
    }

    #[test]
    fn a_known_name_keeps_its_generic_segments() {
        let r = Redactor::new(["acme-prod-replica"]);
        assert_eq!(r.redact("acme-prod-replica"), "▓▓▓▓-prod-replica");
        // Its own segment, alone, is still a whole word.
        assert_eq!(r.redact("env acme ok"), "env ▓▓▓▓ ok");
        // But not inside another identifier: schema stays visible.
        assert_eq!(r.redact("acme_orders acmeish"), "acme_orders acmeish");
    }

    #[test]
    fn generic_words_stay_including_whole_generic_names() {
        let r = Redactor::new(["production", "local", "db-01", ""]);
        assert_eq!(r.redact("production local db-01"), "production local db-01");
    }

    #[test]
    fn known_names_match_case_insensitively() {
        let r = Redactor::new(["Examplecorp-Dev"]);
        assert_eq!(r.redact("EXAMPLECORP-dev"), "▓▓▓▓▓▓▓▓▓▓▓-dev");
    }

    #[test]
    fn short_segments_only_go_inside_their_full_name() {
        let r = Redactor::new(["ab-prod"]);
        assert_eq!(r.redact("ab-prod"), "▓▓-prod");
        assert_eq!(r.redact("ab cd"), "ab cd");
        // A short name on its own is not a name to hunt for.
        assert_eq!(Redactor::new(["u", "ab"]).redact("u ab x"), "u ab x");
    }

    #[test]
    fn a_wide_char_becomes_one_block_per_column() {
        let r = Redactor::new(["東京db"]);
        assert_eq!(r.redact("x 東京db y"), "x ▓▓▓▓▓▓ y");
    }

    #[test]
    fn apply_blocks_cells_and_keeps_wide_chars_aligned() {
        use ratatui::layout::Rect;
        use ratatui::style::{Modifier, Style};
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        let bold = Style::default().add_modifier(Modifier::BOLD);
        buf.set_string(0, 0, "東 acme-prod |", bold);
        buf.set_string(0, 1, "10.1.2.3", Style::default());
        let r = Redactor::new(["acme-prod"]);
        apply(&mut buf, &r);
        let row = |y: u16| -> String {
            let mut s = String::new();
            let mut x = 0;
            while x < 20 {
                let sym = buf[(x, y)].symbol();
                s.push_str(sym);
                x += (UnicodeWidthStr::width(sym) as u16).max(1);
            }
            s
        };
        assert_eq!(row(0), "東 ▓▓▓▓-prod |      ");
        assert_eq!(row(1), "▓▓▓▓▓▓▓▓            ");
        // Style survives on a blocked cell.
        assert!(buf[(3, 0)].modifier.contains(Modifier::BOLD));
        // A redacted wide char covers both its columns.
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        buf.set_string(0, 0, "東京db", Style::default());
        apply(&mut buf, &Redactor::new(["東京db"]));
        let syms: Vec<&str> = (0..6).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(syms, ["▓", "▓", "▓", "▓", "▓", "▓"]);
    }

    #[test]
    fn config_reads_redact_default_and_ignores_other_keys() {
        assert_eq!(parse_config(""), Ok(false));
        assert_eq!(parse_config("redact_default = true\nother = 1"), Ok(true));
        assert_eq!(parse_config("redact_default = false"), Ok(false));
        assert!(parse_config("redact_default = \"yes\"").is_err());
        assert!(parse_config("redact_default = ").is_err());
    }

    #[test]
    fn a_config_that_does_not_parse_turns_redaction_on() {
        let path = std::path::Path::new("config.toml");
        assert_eq!(config_default_from(None, path), (false, None));
        assert_eq!(
            config_default_from(Some("redact_default = true"), path),
            (true, None)
        );
        let (on, why) = config_default_from(Some("redact_default = yes"), path);
        assert!(on, "a broken privacy setting fails closed");
        assert!(why.is_some_and(|w| w.contains("config.toml") && w.contains("redaction is on")));
    }

    #[test]
    fn ranges_merge_and_sit_on_char_boundaries() {
        let r = Redactor::new(["acme-prod.c1x2y3.eu-west-2.rds.amazonaws.com"]);
        let line = "é acme-prod.c1x2y3.eu-west-2.rds.amazonaws.com";
        for range in r.ranges(line) {
            assert!(line.is_char_boundary(range.start) && line.is_char_boundary(range.end));
        }
        assert_eq!(merge(vec![5..7, 0..2, 1..3, 7..8]), vec![0..3, 5..8]);
    }
}
