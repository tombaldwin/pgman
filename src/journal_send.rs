//! The journal's events sent on to CloudWatch Logs, for an environment
//! whose `environments.toml` entry names a group:
//! `journal = { group = "/pgman/dumps" }`. The local journal is written
//! first and stays the developer's own record; this copy is the one in
//! the client's account, which the developer can add to but — with the
//! policy in docs/dump-restore.md — not delete from. See
//! `docs/design/masked-dumps.md`, section 7.
//!
//! What leaves the machine is less than the local line: the file's name
//! without its path (which carries the OS user name), no server address,
//! and, as `by`, the IAM identity that sent it. One log stream per
//! machine id. What has been sent is recorded beside the journal
//! (`journal.sent`, one event id per line), so a send that fails is
//! retried on the next run, and an event is sent once.
//!
//! Through the `aws` CLI, as pgman's other AWS calls are. The pure parts
//! (what is pending, what is sent, the CLI arguments) are tested here;
//! [`flush`] is the thin runner.

use crate::aws_cli::AwsScope;
use crate::journal::{Event, Kind};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

/// Where the sent ids are kept, beside the journal.
pub const SENT_FILE: &str = "journal.sent";
/// Events per `put-log-events` call: well inside its limits (10,000
/// events, 1 MB), and a command line of a few tens of kilobytes.
const BATCH: usize = 50;
/// CloudWatch refuses an event older than 14 days: one older than this,
/// from before the group was set, isn't sent (it would block every event
/// after it).
pub const MAX_AGE: u64 = 13 * 86_400;
/// A batch's events must span less than a day.
const SPAN: u64 = 23 * 3_600;

/// One environment's group, and where to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub env: String,
    pub group: String,
    pub scope: AwsScope,
}

/// The environment `e` is about: its own `env`, else the dump its `copy`
/// names. Pure.
pub fn env_of<'a>(e: &'a Event, dumps: &'a [Event]) -> Option<&'a str> {
    e.env.as_deref().or_else(|| {
        let copy = e.copy.as_deref()?;
        dumps
            .iter()
            .find(|d| d.event == Some(Kind::Dump) && d.copy.as_deref() == Some(copy))
            .and_then(|d| d.env.as_deref())
    })
}

/// The events not yet sent, grouped by target, oldest first: those of an
/// environment with a group, whose id isn't in `sent`, and no older than
/// [`MAX_AGE`] at `now`. Pure.
pub fn pending<'a>(
    events: &'a [Event],
    sent: &HashSet<String>,
    targets: &[Target],
    now: u64,
) -> Vec<(Target, Vec<&'a Event>)> {
    let mut out: Vec<(Target, Vec<&Event>)> = Vec::new();
    for e in events {
        if sent.contains(&e.id)
            || e.event.is_none()
            || e.event == Some(Kind::Other)
            || e.ts.saturating_add(MAX_AGE) < now
        {
            continue;
        }
        let Some(env) = env_of(e, events) else {
            continue;
        };
        let Some(t) = targets.iter().find(|t| t.env == env) else {
            continue;
        };
        match out.iter_mut().find(|(o, _)| o == t) {
            Some((_, list)) => list.push(e),
            None => out.push((t.clone(), vec![e])),
        }
    }
    out
}

/// `path`'s last part: a file's name without where it is. Pure.
fn base_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `e` as it is sent: the file's and link's names without their paths,
/// no server address, the sender's identity as `by`. Pure.
pub fn outgoing(e: &Event, by: Option<&str>) -> Value {
    let mut v = serde_json::to_value(e).unwrap_or_else(|_| json!({}));
    if let Some(obj) = v.as_object_mut() {
        if let Some(f) = &e.file {
            obj.insert("file".into(), json!(base_name(f)));
        }
        if let Some(l) = &e.latest {
            obj.insert("latest".into(), json!(base_name(l)));
        }
        obj.remove("into_server");
        obj.remove("into_target");
        if let Some(by) = by {
            obj.insert("by".into(), json!(by));
        }
    }
    v
}

/// `events` in the batches one call can take: oldest first, at most
/// [`BATCH`] of them, spanning less than a day. Pure.
pub fn batches<'a>(events: &[&'a Event]) -> Vec<Vec<&'a Event>> {
    let mut sorted: Vec<&Event> = events.to_vec();
    sorted.sort_by_key(|e| e.ts);
    let mut out: Vec<Vec<&Event>> = Vec::new();
    for e in sorted {
        match out.last_mut() {
            Some(b) if b.len() < BATCH && e.ts - b[0].ts < SPAN => b.push(e),
            _ => out.push(vec![e]),
        }
    }
    out
}

/// The `--log-events` value for `events`: oldest first, as CloudWatch
/// requires, timestamps in milliseconds. Pure.
pub fn log_events(events: &[&Event], by: Option<&str>) -> String {
    let mut sorted: Vec<&&Event> = events.iter().collect();
    sorted.sort_by_key(|e| e.ts);
    let items: Vec<Value> = sorted
        .iter()
        .map(|e| {
            json!({
                "timestamp": e.ts.saturating_mul(1000),
                "message": outgoing(e, by).to_string(),
            })
        })
        .collect();
    Value::Array(items).to_string()
}

/// `aws logs create-log-stream …` for `stream` in `t`'s group. Pure.
pub fn create_stream_args(t: &Target, stream: &str) -> Vec<String> {
    let mut a = vec![
        "logs".to_string(),
        "create-log-stream".to_string(),
        "--log-group-name".to_string(),
        t.group.clone(),
        "--log-stream-name".to_string(),
        stream.to_string(),
    ];
    a.extend(t.scope.common_args("json"));
    a
}

/// `aws logs put-log-events …` sending `events_json`. Pure.
pub fn put_args(t: &Target, stream: &str, events_json: &str) -> Vec<String> {
    let mut a = vec![
        "logs".to_string(),
        "put-log-events".to_string(),
        "--log-group-name".to_string(),
        t.group.clone(),
        "--log-stream-name".to_string(),
        stream.to_string(),
        "--log-events".to_string(),
        events_json.to_string(),
    ];
    a.extend(t.scope.common_args("json"));
    a
}

/// `aws sts get-caller-identity …` for the sender's ARN. Pure.
pub fn identity_args(scope: &AwsScope) -> Vec<String> {
    let mut a = vec![
        "sts".to_string(),
        "get-caller-identity".to_string(),
        "--query".to_string(),
        "Arn".to_string(),
    ];
    a.extend(scope.common_args("text"));
    a
}

/// Whether the CLI's error says the stream is there already. Pure.
pub fn already_exists(err: &str) -> bool {
    err.contains("ResourceAlreadyExistsException")
}

/// The ids recorded as sent. Pure.
pub fn parse_sent(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// What a flush did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flushed {
    pub sent: usize,
    /// Events still waiting, and why the last attempt failed.
    pub pending: usize,
    pub errors: Vec<String>,
}

impl Flushed {
    /// A note for a command's result, when something is still waiting.
    /// Pure.
    pub fn note(&self) -> Option<String> {
        (self.pending > 0).then(|| {
            format!(
                "{} journal event{} not yet in CloudWatch ({}); sent on the next pgman dump or \
                 pgman copies",
                self.pending,
                if self.pending == 1 { " is" } else { "s are" },
                self.errors
                    .first()
                    .map(String::as_str)
                    .unwrap_or("not sent")
            )
        })
    }
}

/// Each environment with a group, from environments.toml. Reads it.
pub fn targets() -> Vec<Target> {
    crate::inventory::load()
        .environments
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            e.journal.as_ref().map(|j| Target {
                env: e.name.clone(),
                group: j.group.clone(),
                scope: e.journal_scope(),
            })
        })
        .collect()
}

/// Send what is pending at `now` from the journal in `dir` to `targets`,
/// through `aws` (the CLI runner, for tests). Records each event sent.
/// Blocking.
pub fn flush_in(
    dir: &Path,
    targets: &[Target],
    machine: &str,
    now: u64,
    aws: &dyn Fn(&[String]) -> Result<String, String>,
) -> Flushed {
    let mut done = Flushed::default();
    if targets.is_empty() {
        return done;
    }
    let (events, _) = crate::journal::read_in(dir);
    let sent_path = dir.join(SENT_FILE);
    let sent = parse_sent(&std::fs::read_to_string(&sent_path).unwrap_or_default());
    for (t, list) in pending(&events, &sent, targets, now) {
        let ready = check_account(&t, aws)
            .and_then(|()| aws(&identity_args(&t.scope)))
            .map(|arn| arn.trim().to_string())
            .and_then(|arn| match aws(&create_stream_args(&t, machine)) {
                Ok(_) => Ok(arn),
                Err(e) if already_exists(&e) => Ok(arn),
                Err(e) => Err(e),
            });
        let arn = match ready {
            Ok(a) => a,
            Err(e) => {
                done.pending += list.len();
                done.errors.push(e);
                continue;
            }
        };
        let mut left = list.len();
        for chunk in batches(&list) {
            let body = log_events(&chunk, Some(&arn));
            match aws(&put_args(&t, machine, &body)) {
                Ok(reply) => {
                    // Taken, but some refused (too old, too new): they
                    // would be refused again, so they count as sent.
                    if reply.contains("rejectedLogEventsInfo") {
                        tracing::warn!(target: "pgman::journal", reply = %reply.trim(), "CloudWatch refused some journal events");
                    }
                    left -= chunk.len();
                    let ids: String = chunk.iter().map(|e| format!("{}\n", e.id)).collect();
                    if let Err(e) = append_sent(&sent_path, &ids) {
                        tracing::warn!(target: "pgman::journal", "{e}");
                    }
                    done.sent += chunk.len();
                }
                Err(e) => {
                    done.pending += left;
                    done.errors.push(e);
                    break;
                }
            }
        }
    }
    if !done.errors.is_empty() {
        tracing::warn!(target: "pgman::journal", errors = ?done.errors, "journal events not sent");
    }
    done
}

/// Fail unless the credentials in use are in `t`'s account, when the
/// environment names one: a wrong ambient profile then sends nothing,
/// rather than writing the journal into another account's group.
fn check_account(
    t: &Target,
    aws: &dyn Fn(&[String]) -> Result<String, String>,
) -> Result<(), String> {
    let Some(want) = &t.scope.account else {
        return Ok(());
    };
    let got = aws(&t.scope.caller_account_args())?;
    let got = got.trim();
    if got != want {
        return Err(format!(
            "the AWS credentials in use are for account {got}, not {want}; the journal isn't \
             sent to {} for {}",
            t.group, t.env
        ));
    }
    Ok(())
}

fn append_sent(path: &Path, ids: &str) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .and_then(|mut f| f.write_all(ids.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// [`flush`] off the async runtime, as a note for a result when events
/// are still waiting.
pub async fn flush_note() -> Option<String> {
    tokio::task::spawn_blocking(flush)
        .await
        .ok()
        .and_then(|f| f.note())
}

/// [`flush_in`] the data directory, to the environments that name a
/// group, with the real CLI. Blocking.
pub fn flush() -> Flushed {
    let targets = targets();
    if targets.is_empty() {
        return Flushed::default();
    }
    flush_in(
        &crate::journal::dir(),
        &targets,
        &crate::journal::machine_id(),
        crate::journal::now(),
        &crate::aws_cli::run,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn ev(kind: Kind, id: &str, ts: u64, env: Option<&str>, copy: &str) -> Event {
        let mut e = Event::new(kind, ts, "m-000000000000");
        e.id = id.into();
        e.env = env.map(str::to_string);
        e.copy = Some(copy.into());
        e
    }

    fn target(env: &str) -> Target {
        Target {
            env: env.into(),
            group: "/pgman/dumps".into(),
            scope: AwsScope {
                region: Some("eu-west-2".into()),
                ..AwsScope::default()
            },
        }
    }

    #[test]
    fn only_events_of_an_environment_with_a_group_are_pending() {
        let mut dump = ev(Kind::Dump, "e1", 10, Some("production"), "c1");
        dump.file = Some("/Users/dev/work/production-ledger-x.dump".into());
        let restore = ev(Kind::Restore, "e2", 20, None, "c1");
        let other = ev(Kind::Dump, "e3", 30, Some("staging"), "c2");
        let loose = ev(Kind::Delete, "e4", 40, None, "unknown");
        let events = vec![dump, restore, other, loose];
        let p = pending(&events, &HashSet::new(), &[target("production")], 100);
        assert_eq!(p.len(), 1);
        let ids: Vec<&str> = p[0].1.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            ["e1", "e2"],
            "the restore follows its dump's environment"
        );
        let sent: HashSet<String> = ["e1".to_string()].into();
        let p = pending(&events, &sent, &[target("production")], 100);
        assert_eq!(p[0].1.len(), 1);
        assert!(pending(&events, &HashSet::new(), &[], 100).is_empty());
        // Older than CloudWatch takes: left alone.
        let late = 40 + MAX_AGE + 1;
        let p = pending(&events, &HashSet::new(), &[target("production")], late);
        assert!(p.is_empty(), "{p:?}");
    }

    #[test]
    fn what_is_sent_has_no_paths_or_servers() {
        let mut e = ev(Kind::Restore, "e1", 10, Some("production"), "c1");
        e.file = Some("/Users/dev/work/production-ledger-x.dump".into());
        e.latest = Some("/Users/dev/work/production-ledger-latest.dump".into());
        e.into_server = Some("db.internal:5432".into());
        e.into_database = Some("ledger_dev".into());
        let v = outgoing(&e, Some("arn:aws:sts::123456789012:assumed-role/dev/ava"));
        assert_eq!(v["file"], "production-ledger-x.dump");
        assert_eq!(v["latest"], "production-ledger-latest.dump");
        assert!(v.get("into_server").is_none());
        assert_eq!(v["into_database"], "ledger_dev");
        assert_eq!(v["by"], "arn:aws:sts::123456789012:assumed-role/dev/ava");
        let text = v.to_string();
        assert!(
            !text.contains("/Users/") && !text.contains("db.internal"),
            "{text}"
        );
    }

    #[test]
    fn batches_are_small_and_span_less_than_a_day() {
        let many: Vec<Event> = (0..120u64)
            .map(|i| ev(Kind::Dump, &format!("e{i}"), 1_000 + i, Some("p"), "c"))
            .collect();
        let refs: Vec<&Event> = many.iter().rev().collect();
        let b = batches(&refs);
        assert_eq!(b.iter().map(Vec::len).collect::<Vec<_>>(), [50, 50, 20]);
        assert_eq!(b[0][0].id, "e0", "oldest first");
        let spread = [
            ev(Kind::Dump, "a", 0, Some("p"), "c"),
            ev(Kind::Dump, "b", SPAN - 1, Some("p"), "c"),
            ev(Kind::Dump, "c", SPAN, Some("p"), "c"),
        ];
        let refs: Vec<&Event> = spread.iter().collect();
        let b = batches(&refs);
        assert_eq!(b.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);
    }

    #[test]
    fn log_events_are_oldest_first_in_milliseconds() {
        let a = ev(Kind::Dump, "a", 20, Some("p"), "c");
        let b = ev(Kind::Delete, "b", 10, Some("p"), "c");
        let body: Value = serde_json::from_str(&log_events(&[&a, &b], None)).unwrap();
        assert_eq!(body[0]["timestamp"], 10_000);
        assert_eq!(body[1]["timestamp"], 20_000);
        let first: Value = serde_json::from_str(body[0]["message"].as_str().unwrap()).unwrap();
        assert_eq!(first["id"], "b");
    }

    #[test]
    fn the_cli_arguments_name_the_group_stream_and_scope() {
        let t = target("production");
        let a = create_stream_args(&t, "m-0123456789ab").join(" ");
        assert!(a.starts_with("logs create-log-stream --log-group-name /pgman/dumps --log-stream-name m-0123456789ab"), "{a}");
        assert!(a.contains("--region eu-west-2"), "{a}");
        let p = put_args(&t, "m-1", "[]");
        assert_eq!(
            &p[..8],
            [
                "logs",
                "put-log-events",
                "--log-group-name",
                "/pgman/dumps",
                "--log-stream-name",
                "m-1",
                "--log-events",
                "[]"
            ]
        );
        assert_eq!(
            identity_args(&t.scope)[..4],
            ["sts", "get-caller-identity", "--query", "Arn"]
        );
        assert!(already_exists(
            "An error occurred (ResourceAlreadyExistsException) when calling"
        ));
        assert!(!already_exists("AccessDeniedException"));
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pgman-send-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_flush_sends_once_and_retries_what_failed() {
        let dir = scratch("flush");
        for i in 0..3u64 {
            crate::journal::append_in(
                &dir,
                &ev(
                    Kind::Dump,
                    &format!("e{i}"),
                    10 + i,
                    Some("production"),
                    &format!("c{i}"),
                ),
            )
            .unwrap();
        }
        let calls = RefCell::new(Vec::<String>::new());
        let fail_put = RefCell::new(true);
        let aws = |args: &[String]| -> Result<String, String> {
            calls.borrow_mut().push(args[..2].join(" "));
            match args[1].as_str() {
                "get-caller-identity" => Ok("arn:aws:sts::123456789012:assumed-role/dev/ava\n".into()),
                "create-log-stream" => Err("aws logs create-log-stream failed: An error occurred (ResourceAlreadyExistsException)".into()),
                "put-log-events" if *fail_put.borrow() => Err("aws logs put-log-events failed: AccessDeniedException".into()),
                _ => Ok("{}".into()),
            }
        };
        let t = [target("production")];
        let first = flush_in(&dir, &t, "m-1", 100, &aws);
        assert_eq!((first.sent, first.pending), (0, 3));
        assert!(first.note().unwrap().contains("3 journal events are not yet in CloudWatch (aws logs put-log-events failed: AccessDeniedException)"));
        *fail_put.borrow_mut() = false;
        let second = flush_in(&dir, &t, "m-1", 100, &aws);
        assert_eq!((second.sent, second.pending), (3, 0));
        assert_eq!(second.note(), None);
        calls.borrow_mut().clear();
        let third = flush_in(&dir, &t, "m-1", 100, &aws);
        assert_eq!(third, Flushed::default());
        assert!(calls.borrow().is_empty(), "nothing pending: no call");
        assert_eq!(
            parse_sent(&std::fs::read_to_string(dir.join(SENT_FILE)).unwrap()).len(),
            3
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_account_sends_nothing() {
        let dir = scratch("account");
        crate::journal::append_in(&dir, &ev(Kind::Dump, "e1", 10, Some("production"), "c1"))
            .unwrap();
        let account = RefCell::new("123456789012");
        let aws = |args: &[String]| -> Result<String, String> {
            match (args[1].as_str(), args.get(3).map(String::as_str)) {
                ("get-caller-identity", Some("Account")) => Ok(format!("{}\n", account.borrow())),
                ("get-caller-identity", _) => Ok("arn".into()),
                _ => Ok("{}".into()),
            }
        };
        let mut t = target("production");
        t.scope.account = Some("210987654321".into());
        let got = flush_in(&dir, std::slice::from_ref(&t), "m-1", 100, &aws);
        assert_eq!((got.sent, got.pending), (0, 1));
        assert!(
            got.errors[0].contains("account 123456789012, not 210987654321"),
            "{got:?}"
        );
        *account.borrow_mut() = "210987654321";
        let got = flush_in(&dir, &[t], "m-1", 100, &aws);
        assert_eq!((got.sent, got.pending), (1, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
