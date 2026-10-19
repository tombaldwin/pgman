//! Recent errors from an RDS instance's Postgres log, through the aws
//! CLI: the newest log files, their `ERROR` / `FATAL` / `PANIC` lines,
//! grouped by message. `STATEMENT` and `DETAIL` lines are left out —
//! they can carry the data a statement touched.
//!
//! RDS writes `%t:%r:%u@%d:[%p]:` before every line:
//! `2026-10-19 20:54:06 UTC:10.0.1.25(42092):ledger@ledger:[14136]:ERROR:  …`.

use crate::aws_cli::AwsScope;

/// One distinct error, with how often it appeared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogError {
    pub level: String,
    pub message: String,
    /// `user@database`, when the line names one.
    pub who: String,
    pub count: usize,
    /// When it was last seen, as the log prints it.
    pub last_seen: String,
}

/// Log levels worth surfacing.
const LEVELS: [&str; 3] = ["ERROR", "FATAL", "PANIC"];

/// Most log files read: RDS rotates hourly, so this is a few hours.
pub const DEFAULT_FILES: usize = 6;

/// Lines read from the tail of each file.
const LINES_PER_FILE: u32 = 2000;

pub fn describe_files_args(scope: &AwsScope, instance: &str) -> Vec<String> {
    let mut args = vec![
        "rds".to_string(),
        "describe-db-log-files".to_string(),
        "--db-instance-identifier".to_string(),
        instance.to_string(),
    ];
    args.extend(scope.common_args("json"));
    args
}

/// Log file names, newest first. Pure.
pub fn parse_files(json: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("describe-db-log-files returned unreadable JSON: {e}"))?;
    let mut files: Vec<(u64, String)> = v
        .get("DescribeDBLogFiles")
        .and_then(|l| l.as_array())
        .ok_or("describe-db-log-files returned no list")?
        .iter()
        .filter_map(|f| {
            Some((
                f.get("LastWritten")?.as_u64()?,
                f.get("LogFileName")?.as_str()?.to_string(),
            ))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    Ok(files.into_iter().map(|(_, n)| n).collect())
}

pub fn download_args(scope: &AwsScope, instance: &str, file: &str) -> Vec<String> {
    let mut args = vec![
        "rds".to_string(),
        "download-db-log-file-portion".to_string(),
        "--db-instance-identifier".to_string(),
        instance.to_string(),
        "--log-file-name".to_string(),
        file.to_string(),
        "--number-of-lines".to_string(),
        LINES_PER_FILE.to_string(),
    ];
    args.extend(scope.common_args("json"));
    args
}

/// The log text from a download. Pure.
pub fn parse_download(json: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("download-db-log-file-portion returned unreadable JSON: {e}"))?;
    Ok(v.get("LogFileData")
        .and_then(|d| d.as_str())
        .unwrap_or_default()
        .to_string())
}

/// One log line's (time, who, level, message), if it has RDS's prefix.
/// Pure.
fn parse_line(line: &str) -> Option<(String, String, String, String)> {
    // The prefix ends at `[<pid>]:` — the first bracketed run of digits;
    // an unknown user prints as `[unknown]`, which isn't one.
    let end = line.match_indices("]:").find_map(|(i, _)| {
        let open = line[..i].rfind('[')?;
        let pid = &line[open + 1..i];
        (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())).then_some(i)
    })?;
    let (prefix, rest) = (&line[..end], &line[end + 2..]);
    let (level, message) = rest.split_once(':')?;
    let time = prefix.get(..23)?.to_string();
    let parts: Vec<&str> = prefix.rsplitn(3, ':').collect();
    // rsplitn from the right: ["[pid", "user@db", "time:client"].
    let who = parts.get(1).copied().unwrap_or_default();
    let who = if who == "@" {
        String::new()
    } else {
        who.to_string()
    };
    Some((time, who, level.to_string(), message.trim().to_string()))
}

/// The errors in some log text, grouped by level and message, the most
/// recently seen first. Pure.
pub fn parse_errors(text: &str) -> Vec<LogError> {
    let mut out: Vec<LogError> = Vec::new();
    for line in text.lines() {
        let Some((time, who, level, message)) = parse_line(line) else {
            continue;
        };
        if !LEVELS.contains(&level.as_str()) {
            continue;
        }
        match out
            .iter_mut()
            .find(|e| e.level == level && e.message == message)
        {
            Some(e) => {
                e.count += 1;
                if time > e.last_seen {
                    e.last_seen = time;
                    e.who = who;
                }
            }
            None => out.push(LogError {
                level,
                message,
                who,
                count: 1,
                last_seen: time,
            }),
        }
    }
    out.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
    out
}

/// Recent errors on `instance`, from its newest `files` log files.
/// Blocking: the aws CLI runs here.
pub fn fetch(scope: &AwsScope, instance: &str, files: usize) -> Result<Vec<LogError>, String> {
    scope.validate()?;
    if !crate::aws_cli::plain_arg(instance, "-", 63) {
        return Err(format!("{instance:?} is not an RDS instance identifier"));
    }
    scope.check_account("RDS logs")?;
    let names = parse_files(&crate::aws_cli::run(&describe_files_args(scope, instance))?)?;
    let mut text = String::new();
    for name in names.iter().take(files) {
        if !crate::aws_cli::plain_arg(name, "/._-", 256) {
            continue;
        }
        text.push_str(&parse_download(&crate::aws_cli::run(&download_args(
            scope, instance, name,
        ))?)?);
        text.push('\n');
    }
    Ok(parse_errors(&text))
}

/// Recent errors for `server` in environment `environment`, read from
/// `environments.toml`: only an `rds` environment, and only a server its
/// listing includes, so nothing outside the operator's config is read.
/// Blocking.
pub fn for_environment(
    environment: &str,
    server: &str,
    files: usize,
) -> Result<Vec<LogError>, String> {
    let envs = super::load().environments?;
    let env = envs
        .iter()
        .find(|e| e.name == environment)
        .ok_or_else(|| format!("no environment {environment:?} in environments.toml"))?;
    let super::Source::Rds { .. } = &env.source else {
        return Err(format!(
            "{environment} isn't an RDS environment: its logs aren't in RDS"
        ));
    };
    let scope = env.source.rds_scope().unwrap_or_default();
    let listed = super::fetch_rds(&scope)?;
    let ours = super::rds_targets(env, &listed);
    if !ours.iter().any(|t| t.server == server) {
        return Err(format!("{server} isn't one of {environment}'s servers"));
    }
    fetch(&scope, server, files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines as RDS wrote them on 2026-10-19 (demo), plus made-up ones
    /// in the same shape.
    const LOG: &str = "\
2026-10-19 20:54:06 UTC:10.0.1.25(42092):ledger@ledger:[14136]:ERROR:  permission denied for database rdsadmin
2026-10-19 20:54:06 UTC:10.0.1.25(42092):ledger@ledger:[14136]:STATEMENT:  select datname as database from pg_database
2026-10-19 21:43:01 UTC:10.0.1.25(43116):ledger@ledger:[16838]:ERROR:  relation \"pg_stat_statements\" does not exist at character 82
2026-10-19 22:04:49 UTC::@:[1354]:LOG:  checkpoint starting: time
2026-10-19 22:05:00 UTC:10.0.1.25(43120):app@ledger:[16900]:ERROR:  permission denied for database rdsadmin
2026-10-19 22:06:00 UTC:10.0.0.9(5000):[unknown]@[unknown]:[17000]:FATAL:  password authentication failed for user \"app\"
not a log line at all
";

    #[test]
    fn errors_are_grouped_newest_first_without_statements() {
        let e = parse_errors(LOG);
        let summary: Vec<(String, usize, String)> = e
            .iter()
            .map(|x| (x.level.clone(), x.count, x.message.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "FATAL".into(),
                    1,
                    "password authentication failed for user \"app\"".into()
                ),
                (
                    "ERROR".into(),
                    2,
                    "permission denied for database rdsadmin".into()
                ),
                (
                    "ERROR".into(),
                    1,
                    "relation \"pg_stat_statements\" does not exist at character 82".into()
                ),
            ]
        );
        assert_eq!(e[1].last_seen, "2026-10-19 22:05:00 UTC");
        assert_eq!(e[1].who, "app@ledger", "who saw it last");
        assert!(
            e.iter().all(|x| !x.message.contains("select datname")),
            "STATEMENT lines never surface"
        );
    }

    #[test]
    fn log_files_come_newest_first_and_downloads_unwrap() {
        let files = parse_files(
            r#"{"DescribeDBLogFiles": [
              {"LogFileName": "error/postgresql.log.2026-10-19-20", "LastWritten": 1792443589887},
              {"LogFileName": "error/postgresql.log.2026-10-19-22", "LastWritten": 1792448391073},
              {"LogFileName": "error/postgresql.log.2026-10-19-21", "LastWritten": 1792447190454}
            ]}"#,
        )
        .unwrap();
        assert_eq!(files[0], "error/postgresql.log.2026-10-19-22");
        assert_eq!(files[2], "error/postgresql.log.2026-10-19-20");
        assert_eq!(
            parse_download(r#"{"LogFileData": "a\nb", "AdditionalDataPending": false}"#).unwrap(),
            "a\nb"
        );
        assert!(parse_files("{}").is_err());
        let a = download_args(&AwsScope::default(), "demo", "error/x").join(" ");
        assert!(a.contains("--number-of-lines 2000"), "{a}");
    }
}
