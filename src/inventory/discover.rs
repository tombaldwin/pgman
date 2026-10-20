//! Propose environments from what an AWS account already says: RDS
//! instances, Secrets Manager secret *names* and EC2 hosts. Nothing here
//! connects to a database or reads a secret's value.
//!
//! - **Grouping**: each Postgres primary and its read replicas form one
//!   environment, named after the primary. A replica whose primary isn't
//!   listed (another region) is its own.
//! - **Credentials**: secrets are matched by name — first path segment
//!   the environment (or a prefix of it, `dev` for `development`), a
//!   segment naming the database, `_replica` for a replica. A tie is not
//!   guessed: it becomes a note listing the candidates.
//! - **Tunnel**: the one running EC2 host with a public DNS name in the
//!   instance's VPC, preferring a name with `bastion` or `jump` in it.
//!
//! [`propose`] is pure. [`discover`] runs the aws CLI; [`save`] appends
//! chosen proposals to `environments.toml`, never rewriting what is
//! there.

use super::rds::{self, RdsInstance};
use super::{parse_config, Environment, Source};
use crate::aws_cli::AwsScope;
use std::collections::BTreeMap;

/// One running EC2 instance, as far as finding a bastion goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ec2Host {
    pub id: String,
    /// The `Name` tag.
    pub name: Option<String>,
    pub vpc_id: Option<String>,
    pub public_dns: String,
}

/// What [`discover`] read from the account.
#[derive(Debug, Clone)]
pub struct Discovery {
    /// The scope the listings ran in, with the account (and, when the
    /// CLI has one configured, the region) filled in, so a saved
    /// environment is pinned to them.
    pub scope: AwsScope,
    pub instances: Vec<RdsInstance>,
    /// Secret names, or why they couldn't be listed. Not fatal: the
    /// proposals just carry no secrets.
    pub secrets: Result<Vec<String>, String>,
    /// Running EC2 hosts, or why they couldn't be listed.
    pub hosts: Result<Vec<Ec2Host>, String>,
}

/// One proposed environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub env: Environment,
    /// What a person should check before saving: ambiguities, where the
    /// databases came from, which host the tunnel goes through.
    pub notes: Vec<String>,
    /// The name of an existing environment that already covers this
    /// primary. A covered proposal is shown but never saved.
    pub covered_by: Option<String>,
    /// The primary first, then its replicas.
    pub servers: Vec<String>,
}

/// A discovery as the environments overview shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// `AWS account …, region`.
    pub header: String,
    /// What couldn't be listed, and so is missing from the proposals.
    pub warnings: Vec<String>,
    pub proposals: Vec<Proposal>,
    /// The scopes the overview can discover in next (`p`): the ambient
    /// one first. Empty from the CLI, which takes flags instead.
    pub scopes: Vec<AwsScope>,
}

impl Discovery {
    fn header(&self) -> String {
        format!(
            "AWS account {}{}",
            self.scope.account.as_deref().unwrap_or("?"),
            self.scope
                .region
                .as_deref()
                .map(|r| format!(", {r}"))
                .unwrap_or_default()
        )
    }

    fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Err(e) = &self.secrets {
            out.push(format!("secret names not listed, so no secrets: {e}"));
        }
        if let Err(e) = &self.hosts {
            out.push(format!("EC2 hosts not listed, so no tunnels: {e}"));
        }
        out
    }

    pub fn found(&self, existing: &[Environment]) -> Found {
        Found {
            header: self.header(),
            warnings: self.warnings(),
            proposals: proposals(self, existing),
            scopes: Vec::new(),
        }
    }
}

/// Where the overview can discover: the ambient profile, every distinct
/// scope an `rds` environment already names, then each CLI profile not
/// already among them. In that order, without repeats.
pub fn scope_choices(existing: &[Environment], profiles: &[String]) -> Vec<AwsScope> {
    let mut out = vec![AwsScope::default()];
    for s in existing.iter().filter_map(|e| e.source.scope()) {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    for p in profiles {
        if out.iter().all(|s| s.profile.as_deref() != Some(p.as_str())) {
            out.push(AwsScope {
                profile: Some(p.clone()),
                ..AwsScope::default()
            });
        }
    }
    out
}

/// How a scope reads in the overview.
pub fn scope_label(s: &AwsScope) -> String {
    let mut bits = vec![match &s.profile {
        Some(p) => format!("profile {p}"),
        None => "ambient profile".to_string(),
    }];
    bits.extend(s.account.iter().map(|a| format!("account {a}")));
    bits.extend(s.region.iter().cloned());
    bits.join(" · ")
}

/// `aws configure list-profiles`; none when the CLI can't say. Blocking.
pub fn list_profiles() -> Vec<String> {
    let args = ["configure".to_string(), "list-profiles".to_string()];
    crate::aws_cli::run(&args)
        .map(|out| {
            out.lines()
                .map(str::trim)
                .filter(|p| crate::aws_cli::plain_arg(p, "_.-@+=", 128))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn list_secrets_args(scope: &AwsScope) -> Vec<String> {
    let mut args = vec!["secretsmanager".to_string(), "list-secrets".to_string()];
    args.extend(scope.common_args("json"));
    args
}

/// Names only. `list-secrets` returns no values, and nothing else is kept.
pub fn parse_secret_names(json: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("list-secrets returned unreadable JSON: {e}"))?;
    let list = v
        .get("SecretList")
        .and_then(|l| l.as_array())
        .ok_or("list-secrets returned no SecretList")?;
    Ok(list
        .iter()
        .filter_map(|s| s.get("Name").and_then(|n| n.as_str()).map(str::to_string))
        .collect())
}

pub fn describe_hosts_args(scope: &AwsScope) -> Vec<String> {
    let mut args = vec![
        "ec2".to_string(),
        "describe-instances".to_string(),
        "--filters".to_string(),
        "Name=instance-state-name,Values=running".to_string(),
    ];
    args.extend(scope.common_args("json"));
    args
}

/// Running hosts with a public DNS name; the rest can't be a bastion.
pub fn parse_hosts(json: &str) -> Result<Vec<Ec2Host>, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("describe-instances returned unreadable JSON: {e}"))?;
    let reservations = v
        .get("Reservations")
        .and_then(|r| r.as_array())
        .ok_or("describe-instances returned no Reservations")?;
    let s = |i: &serde_json::Value, k: &str| i.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Ok(reservations
        .iter()
        .filter_map(|r| r.get("Instances").and_then(|i| i.as_array()))
        .flatten()
        .filter_map(|i| {
            let public_dns = s(i, "PublicDnsName").filter(|d| !d.is_empty())?;
            let name = i
                .get("Tags")
                .and_then(|t| t.as_array())
                .and_then(|tags| {
                    tags.iter()
                        .find(|t| t.get("Key").and_then(|k| k.as_str()) == Some("Name"))
                })
                .and_then(|t| t.get("Value"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Some(Ec2Host {
                id: s(i, "InstanceId")?,
                name,
                vpc_id: s(i, "VpcId"),
                public_dns,
            })
        })
        .collect())
}

/// Segments that introduce a database name in a secret path:
/// `<env>/database/<db>/credentials`.
const DATABASE_SEGMENTS: [&str; 3] = ["database", "databases", "db"];

/// Secrets that belong to environment `env`: a path whose first segment
/// is `env`, or a prefix of it at least three long (`dev` for
/// `development`) that isn't itself another environment's name.
fn secrets_for<'a>(env: &str, secrets: &'a [String], other_envs: &[&str]) -> Vec<&'a str> {
    secrets
        .iter()
        .map(String::as_str)
        .filter(|name| {
            let Some((first, _)) = name.split_once('/') else {
                return false;
            };
            first == env
                || (first.len() >= 3 && env.starts_with(first) && !other_envs.contains(&first))
        })
        .collect()
}

/// Database names the secret paths spell out, `_replica` folded away,
/// sorted.
fn databases_in(secrets: &[&str]) -> Vec<String> {
    let mut dbs: Vec<String> = secrets
        .iter()
        .filter_map(|name| {
            let segs: Vec<&str> = name.split('/').collect();
            let i = segs.iter().position(|s| DATABASE_SEGMENTS.contains(s))?;
            let db = segs.get(i + 1)?;
            Some(db.strip_suffix("_replica").unwrap_or(db).to_string())
        })
        .filter(|db| !db.is_empty())
        .collect();
    dbs.sort();
    dbs.dedup();
    dbs
}

fn is_replica_secret(name: &str) -> bool {
    name.split('/').any(|s| s.contains("replica"))
}

/// The secret for `db` on a server of environment `env`, or the tied
/// candidates when there is no single best one (empty when nothing names
/// the database).
fn pick_secret<'a>(
    env: &str,
    candidates: &[&'a str],
    db: &str,
    replica: bool,
) -> Result<&'a str, Vec<&'a str>> {
    let replica_db = format!("{db}_replica");
    let scored: Vec<(i32, &str)> = candidates
        .iter()
        .filter(|name| name.split('/').any(|s| s == db || s == replica_db))
        .filter(|name| replica || !is_replica_secret(name))
        .map(|name| {
            let mut score = 0;
            if name.split('/').next() == Some(env) {
                score += 4;
            }
            if name.ends_with("/credentials") {
                score += 2;
            }
            if replica && is_replica_secret(name) {
                score += 3;
            }
            (score, *name)
        })
        .collect();
    let Some(best) = scored.iter().map(|(s, _)| *s).max() else {
        return Err(Vec::new());
    };
    let top: Vec<&str> = scored
        .iter()
        .filter(|(s, _)| *s == best)
        .map(|(_, n)| *n)
        .collect();
    match top.as_slice() {
        [one] => Ok(one),
        _ => Err(top),
    }
}

/// The bastion for instances in `vpc`: the one public host there, or the
/// one whose name says bastion/jump when there are several.
fn pick_host<'a>(vpc: Option<&str>, hosts: &'a [Ec2Host]) -> Result<&'a Ec2Host, Vec<&'a Ec2Host>> {
    let Some(vpc) = vpc else {
        return Err(Vec::new());
    };
    let here: Vec<&Ec2Host> = hosts
        .iter()
        .filter(|h| h.vpc_id.as_deref() == Some(vpc))
        .collect();
    if let [one] = here.as_slice() {
        return Ok(one);
    }
    let named: Vec<&Ec2Host> = here
        .iter()
        .copied()
        .filter(|h| {
            h.name.as_deref().is_some_and(|n| {
                let n = n.to_ascii_lowercase();
                n.contains("bastion") || n.contains("jump")
            })
        })
        .collect();
    match named.as_slice() {
        [one] => Ok(one),
        _ => Err(here),
    }
}

fn host_label(h: &Ec2Host) -> String {
    match &h.name {
        Some(n) => format!("{n} ({})", h.id),
        None => h.id.clone(),
    }
}

/// An existing environment that already lists `primary`: same name, or
/// an `rds` source whose globs match it.
fn covering(primary: &str, existing: &[Environment]) -> Option<String> {
    existing
        .iter()
        .find(|e| {
            e.name == primary
                || matches!(&e.source, Source::Rds { instances, .. }
                    if instances.iter().any(|p| rds::glob_match(p, primary)))
        })
        .map(|e| e.name.clone())
}

/// Proposed environments, one per primary, in identifier order.
pub fn propose(
    all: &[RdsInstance],
    secrets: &[String],
    hosts: &[Ec2Host],
    scope: &AwsScope,
    existing: &[Environment],
) -> Vec<Proposal> {
    let postgres: Vec<&RdsInstance> = all.iter().filter(|i| i.is_postgres()).collect();
    let listed = |id: &str| postgres.iter().any(|i| i.id == id);
    // A primary, or a replica whose primary isn't in the listing.
    let mut roots: Vec<&RdsInstance> = postgres
        .iter()
        .copied()
        .filter(|i| i.source.as_deref().is_none_or(|s| !listed(s)))
        .collect();
    roots.sort_by(|a, b| a.id.cmp(&b.id));
    let root_names: Vec<&str> = roots.iter().map(|r| r.id.as_str()).collect();

    roots
        .iter()
        .map(|root| {
            let name = root.id.clone();
            let mut notes = Vec::new();
            let servers = rds::select(all, std::slice::from_ref(&name));
            let others: Vec<&str> = root_names
                .iter()
                .copied()
                .filter(|n| *n != name)
                .collect();
            let candidates = secrets_for(&name, secrets, &others);

            let from_secrets = databases_in(&candidates);
            let own: Vec<String> = root.db_name.iter().cloned().collect();
            let databases = if from_secrets.is_empty() {
                own.clone()
            } else {
                if from_secrets != own {
                    notes.push(format!(
                        "databases {} come from secret names; the instance was created with {}",
                        from_secrets.join(", "),
                        root.db_name.as_deref().unwrap_or("no database")
                    ));
                }
                from_secrets
            };

            let mut chosen = BTreeMap::new();
            for server in &servers {
                let replica = server.source.is_some();
                for db in &databases {
                    let key = if databases.len() == 1 {
                        server.id.clone()
                    } else {
                        format!("{}/{db}", server.id)
                    };
                    match pick_secret(&name, &candidates, db, replica) {
                        Ok(secret) => {
                            if replica && !is_replica_secret(secret) {
                                notes.push(format!(
                                    "{}/{db} uses the primary's secret {secret}: no replica secret names {db}",
                                    server.id
                                ));
                            }
                            chosen.insert(key, secret.to_string());
                        }
                        Err(tied) if tied.is_empty() => notes.push(format!(
                            "no secret names {db} for {}; add one under secrets, or set password_env",
                            server.id
                        )),
                        Err(tied) => notes.push(format!(
                            "pick a secret for {}/{db} (secrets.\"{key}\"): {}",
                            server.id,
                            tied.join(", ")
                        )),
                    }
                }
            }

            let ssh_tunnel = match pick_host(root.vpc_id.as_deref(), hosts) {
                Ok(h) => {
                    notes.push(format!(
                        "tunnels through {}; the ssh user comes from ~/.ssh/config unless you write user@host",
                        host_label(h)
                    ));
                    Some(h.public_dns.clone())
                }
                Err(several) if !several.is_empty() => {
                    notes.push(format!(
                        "several public hosts in {}; set ssh_tunnel to one of: {}",
                        root.vpc_id.as_deref().unwrap_or("the VPC"),
                        several
                            .iter()
                            .map(|h| format!("{} = {}", host_label(h), h.public_dns))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    None
                }
                Err(_) => None,
            };

            let env = Environment {
                name: name.clone(),
                source: Source::Rds {
                    instances: vec![name.clone()],
                    region: scope.region.clone(),
                    profile: scope.profile.clone(),
                    account: scope.account.clone(),
                },
                databases: if databases == own {
                    Vec::new()
                } else {
                    databases
                },
                ssh_tunnel,
                sslmode: Some("require".to_string()),
                user: None,
                password_env: None,
                secrets: chosen,
                probe: false,
                version_table: None,
                migrations_dir: None,
            protected: None,
            };
            Proposal {
                covered_by: covering(&name, existing),
                servers: servers.iter().map(|s| s.id.clone()).collect(),
                env,
                notes,
            }
        })
        .collect()
}

/// A TOML string, quoted and escaped.
fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// One proposal as an `environments.toml` block, notes as comments.
pub fn render(p: &Proposal) -> String {
    let e = &p.env;
    let mut out = String::new();
    for n in &p.notes {
        out.push_str(&format!("# {n}\n"));
    }
    out.push_str("[[environments]]\n");
    out.push_str(&format!("name = {}\n", q(&e.name)));
    if !e.databases.is_empty() {
        let dbs: Vec<String> = e.databases.iter().map(|d| q(d)).collect();
        out.push_str(&format!("databases = [{}]\n", dbs.join(", ")));
    }
    if let Source::Rds {
        instances,
        region,
        profile,
        account,
    } = &e.source
    {
        let inst: Vec<String> = instances.iter().map(|i| q(i)).collect();
        let mut src = format!("{{ kind = \"rds\", instances = [{}]", inst.join(", "));
        for (k, v) in [
            ("region", region),
            ("profile", profile),
            ("account", account),
        ] {
            if let Some(v) = v {
                src.push_str(&format!(", {k} = {}", q(v)));
            }
        }
        src.push_str(" }");
        out.push_str(&format!("source = {src}\n"));
    }
    for (k, v) in [
        ("ssh_tunnel", &e.ssh_tunnel),
        ("sslmode", &e.sslmode),
        ("user", &e.user),
    ] {
        if let Some(v) = v {
            out.push_str(&format!("{k} = {}\n", q(v)));
        }
    }
    if e.probe {
        out.push_str("probe = true\n");
    }
    if let Some(d) = &e.migrations_dir {
        out.push_str(&format!("migrations_dir = {}\n", q(d)));
    }
    if let Some(v) = &e.version_table {
        let mut t = format!("table = {}, column = {}", q(&v.table), q(&v.column));
        if let Some(dir) = &v.patches {
            t.push_str(&format!(", patches = {}", q(dir)));
        }
        if !v.databases.is_empty() {
            let dbs: Vec<String> = v.databases.iter().map(|d| q(d)).collect();
            t.push_str(&format!(", databases = [{}]", dbs.join(", ")));
        }
        out.push_str(&format!("version_table = {{ {t} }}\n"));
    }
    if !e.secrets.is_empty() {
        out.push_str("\n[environments.secrets]\n");
        for (k, v) in &e.secrets {
            out.push_str(&format!("{} = {}\n", q(k), q(v)));
        }
    }
    out
}

/// `existing` with `proposals` appended, checked to parse. Covered
/// proposals are refused, not skipped: the caller chose them by name.
pub fn append(existing: &str, proposals: &[&Proposal]) -> Result<String, String> {
    // Covered as of the text being appended to, not only as of the
    // discovery: the file may have changed since.
    let current = parse_config(existing)
        .map_err(|e| format!("the existing file doesn't load, so nothing was saved: {e}"))?;
    for p in proposals {
        let primary = p.servers.first().unwrap_or(&p.env.name);
        if let Some(by) = p.covered_by.clone().or_else(|| covering(primary, &current)) {
            return Err(format!(
                "{} is already covered by environment {by:?}",
                p.env.name
            ));
        }
    }
    let mut out = existing.to_string();
    for p in proposals {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
        }
        out.push_str(&render(p));
    }
    parse_config(&out)
        .map_err(|e| format!("the result would not load, so nothing was saved: {e}"))?;
    Ok(out)
}

/// The proposals named in `names`, or every uncovered one when `names`
/// is empty. An unknown name is an error.
pub fn choose<'a>(
    proposals: &'a [Proposal],
    names: &[String],
) -> Result<Vec<&'a Proposal>, String> {
    if names.is_empty() {
        return Ok(proposals
            .iter()
            .filter(|p| p.covered_by.is_none())
            .collect());
    }
    names
        .iter()
        .map(|n| {
            proposals.iter().find(|p| &p.env.name == n).ok_or_else(|| {
                format!(
                    "no proposed environment {n:?}; proposed: {}",
                    proposals
                        .iter()
                        .map(|p| p.env.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
        })
        .collect()
}

/// List the account. The RDS listing is required; secrets and hosts are
/// best effort (a role without `secretsmanager:ListSecrets` still gets
/// environments, just no secrets). Blocking.
pub fn discover(scope: &AwsScope) -> Result<Discovery, String> {
    scope.validate()?;
    let mut scope = scope.clone();
    match &scope.account {
        Some(_) => scope.check_account("the account")?,
        None => {
            let got = crate::aws_cli::run(&scope.caller_account_args())?;
            scope.account = Some(got.trim().to_string());
        }
    }
    if scope.region.is_none() {
        let mut args = vec![
            "configure".to_string(),
            "get".to_string(),
            "region".to_string(),
        ];
        if let Some(p) = &scope.profile {
            args.extend(["--profile".to_string(), p.clone()]);
        }
        if let Ok(r) = crate::aws_cli::run(&args) {
            let r = r.trim();
            if !r.is_empty() {
                scope.region = Some(r.to_string());
            }
        }
    }
    scope.validate()?;
    let instances = rds::parse_describe(&crate::aws_cli::run(&rds::describe_args(&scope))?)?;
    let secrets =
        crate::aws_cli::run(&list_secrets_args(&scope)).and_then(|j| parse_secret_names(&j));
    let hosts = crate::aws_cli::run(&describe_hosts_args(&scope)).and_then(|j| parse_hosts(&j));
    Ok(Discovery {
        scope,
        instances,
        secrets,
        hosts,
    })
}

/// Proposals from a discovery against the environments already loaded.
pub fn proposals(d: &Discovery, existing: &[Environment]) -> Vec<Proposal> {
    let empty = Vec::new();
    propose(
        &d.instances,
        d.secrets.as_ref().unwrap_or(&empty),
        d.hosts.as_ref().map(Vec::as_slice).unwrap_or(&[]),
        &d.scope,
        existing,
    )
}

/// How long a save waits for another process's save to finish.
const SAVE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// An exclusive advisory lock on `path` (created `0600` when missing),
/// held until the returned file is dropped. Waits up to `wait` for
/// whoever holds it.
fn lock_exclusive(
    path: &std::path::Path,
    wait: std::time::Duration,
) -> Result<std::fs::File, String> {
    if let Some(dir) = path.parent() {
        crate::util::create_dir_all_private(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let mut open = std::fs::OpenOptions::new();
    open.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let file = open
        .open(path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    let deadline = std::time::Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "another pgman is saving environments.toml (lock {} held); try again",
                    path.display()
                ))
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!("could not lock {}: {e}", path.display()))
            }
        }
    }
}

/// Append `chosen` to `environments.toml` under `util::config_dir()`,
/// creating it (`0600`) when missing. Returns the path written.
pub fn save(chosen: &[&Proposal]) -> Result<std::path::PathBuf, String> {
    // One read-append-write at a time: the MCP server runs calls
    // concurrently, and two saves that both read the old file would each
    // write it back with only their own addition.
    static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    let path = crate::util::config_file("environments.toml");
    // And one across processes: the TUI, `pgman env discover` and an MCP
    // server can all save.
    let _locked = lock_exclusive(
        &path.with_file_name(".environments.toml.lock"),
        SAVE_LOCK_WAIT,
    )?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let text = append(&existing, chosen)?;
    crate::util::write_private(&path, &text)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(path)
}

pub const USAGE: &str =
    "usage: pgman env discover [--region R] [--profile P] [--account A] [--save [NAME ...]]

Proposes environments from the AWS account: one per RDS Postgres primary,
with its replicas, the Secrets Manager secret names that match each
database, and the bastion in its VPC. Reads names and endpoints only:
no database is connected to and no secret value is read.

  --region, --profile   the aws CLI's; default the CLI's own
  --account             refuse unless the credentials are for this account
  --save [NAME ...]     append the named proposals (default: every one not
                        already covered) to environments.toml in pgman's
                        config dir. What is there is never rewritten.";

/// Parsed `pgman env discover` flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOpts {
    pub scope: AwsScope,
    /// `Some` with `--save`: the names to save, empty for every uncovered one.
    pub save: Option<Vec<String>>,
}

/// Parse the arguments after `pgman env`. `args[0]` is the sub-verb.
pub fn parse_cli(args: &[String]) -> Result<CliOpts, String> {
    if args.first().map(String::as_str) != Some("discover") {
        return Err(USAGE.to_string());
    }
    let mut opts = CliOpts::default();
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))
        };
        match a.as_str() {
            "--region" => opts.scope.region = Some(value("--region")?),
            "--profile" => opts.scope.profile = Some(value("--profile")?),
            "--account" => opts.scope.account = Some(value("--account")?),
            "--save" => opts.save = Some(Vec::new()),
            other => match opts.save.as_mut() {
                // Names follow `--save`.
                Some(names) if !other.starts_with('-') => names.push(other.to_string()),
                _ => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
            },
        }
    }
    opts.scope.validate()?;
    Ok(opts)
}

/// What `pgman env discover` prints for a discovery: where it looked,
/// what it couldn't list, then each proposal as TOML.
pub fn report(d: &Discovery, proposals: &[Proposal]) -> String {
    let mut out = format!("# discovered in {}\n", d.header());
    for w in d.warnings() {
        out.push_str(&format!("# {w}\n"));
    }
    if proposals.is_empty() {
        out.push_str("# no Postgres instances found\n");
    }
    for p in proposals {
        out.push('\n');
        match &p.covered_by {
            Some(by) => out.push_str(&format!(
                "# {}: already covered by environment {by:?}\n",
                p.env.name
            )),
            None => out.push_str(&render(p)),
        }
    }
    out
}

/// The version table a project in or above `cwd` declares in its
/// `[patches]`, its patch directory made absolute, and the config file
/// it came from. Reads the project file.
pub fn project_version_table(
    cwd: &std::path::Path,
) -> Option<(std::path::PathBuf, super::VersionTable)> {
    let (root, cfg) = crate::project::load_from(cwd)?;
    let p = cfg.patches?;
    let dir = root.join(&p.dir);
    Some((
        crate::project::config_path(&root),
        super::VersionTable {
            table: p.table,
            column: p.column,
            patches: Some(dir.display().to_string()),
            databases: Vec::new(),
        },
    ))
}

/// Give every new proposal the project's version table, saying where it
/// came from. `probe` stays off: turning it on connects to every
/// database on each refresh, and that is the operator's call per
/// environment. Pure.
pub fn with_version_table(
    proposals: &mut [Proposal],
    project: Option<&(std::path::PathBuf, super::VersionTable)>,
) {
    let Some((from, vt)) = project else {
        return;
    };
    for p in proposals.iter_mut().filter(|p| p.covered_by.is_none()) {
        p.env.version_table = Some(vt.clone());
        p.notes.push(format!(
            "version_table from {}; set probe = true to read the patch level, pending patches \
             and lag on every refresh (it connects to each database)",
            from.display()
        ));
    }
}

/// Discovery for the environments overview's `d` and `p`: `scope`, against
/// the environments already configured, plus the scopes `p` can try next.
/// Blocking.
pub fn discover_for_overview(scope: &AwsScope) -> Result<Found, String> {
    let existing = super::load().environments?;
    let mut found = discover(scope)?.found(&existing);
    if let Ok(cwd) = std::env::current_dir() {
        with_version_table(&mut found.proposals, project_version_table(&cwd).as_ref());
    }
    found.scopes = scope_choices(&existing, &list_profiles());
    Ok(found)
}

/// `pgman env discover`. Returns what to print. Blocking.
pub fn run_cli(opts: &CliOpts) -> Result<String, String> {
    let loaded = super::load();
    let existing = match (&loaded.environments, &opts.save) {
        (Ok(e), _) => e.clone(),
        // Saving appends to that file; it has to load first.
        (Err(e), Some(_)) => return Err(e.clone()),
        (Err(_), None) => Vec::new(),
    };
    let d = discover(&opts.scope)?;
    let mut proposals = proposals(&d, &existing);
    if let Ok(cwd) = std::env::current_dir() {
        with_version_table(&mut proposals, project_version_table(&cwd).as_ref());
    }
    let Some(names) = &opts.save else {
        let mut out = report(&d, &proposals);
        if proposals.iter().any(|p| p.covered_by.is_none()) {
            out.push_str(&format!(
                "\n# check the notes, then save with: pgman env discover --save [NAME ...]\n#   into {}\n",
                loaded.path.display()
            ));
        }
        return Ok(out);
    };
    let chosen = choose(&proposals, names)?;
    if chosen.is_empty() {
        return Ok("nothing to save: every environment found is already configured".into());
    }
    let path = save(&chosen)?;
    Ok(format!(
        "added {} to {}",
        chosen
            .iter()
            .map(|p| p.env.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// examplecorp's secret names on 2026-10-19 — names only.
    const SECRETS: [&str; 18] = [
        "development-database-password",
        "demo-database-password",
        "production-database-password",
        "production/database/ledger",
        "dev/database/ledger",
        "development/database/ledger/credentials",
        "development/database/ledgerlog/credentials",
        "development/database/ledger_replica/credentials",
        "demo/database/ledger_replica/credentials",
        "demo/database/ledger/credentials",
        "demo/database/ledgerlog/credentials",
        "production/database/ledgerlog/credentials",
        "production/database/ledger/credentials",
        "production/database/ledger_replica/credentials",
        "examplecorp/demo/redis_auth_token",
        "examplecorp/demo/oauth_client_secret",
        "monitoring/agent/credentials",
        "elasticbeanstalk/bitbucket/iam/credentials",
    ];

    fn secrets() -> Vec<String> {
        SECRETS.iter().map(|s| s.to_string()).collect()
    }

    fn hosts() -> Vec<Ec2Host> {
        vec![
            Ec2Host {
                id: "i-1".into(),
                name: Some("db-dump".into()),
                vpc_id: Some("vpc-0a1b2c3d".into()),
                public_dns: "ec2-1.compute.amazonaws.com".into(),
            },
            Ec2Host {
                id: "i-2".into(),
                name: Some("elsewhere".into()),
                vpc_id: Some("vpc-other".into()),
                public_dns: "ec2-2.compute.amazonaws.com".into(),
            },
        ]
    }

    fn scope() -> AwsScope {
        AwsScope {
            region: Some("us-west-1".into()),
            profile: None,
            account: Some("210987654321".into()),
        }
    }

    fn all() -> Vec<RdsInstance> {
        rds::parse_describe(rds::tests::FIXTURE).unwrap()
    }

    fn by_name<'a>(ps: &'a [Proposal], n: &str) -> &'a Proposal {
        ps.iter().find(|p| p.env.name == n).unwrap()
    }

    #[test]
    fn one_environment_per_primary_with_its_replicas() {
        let ps = propose(&all(), &[], &[], &scope(), &[]);
        let names: Vec<&str> = ps.iter().map(|p| p.env.name.as_str()).collect();
        assert_eq!(
            names,
            ["demo", "development", "production"],
            "no mysql, no replicas of their own"
        );
        assert!(
            matches!(&ps[0].env.source, Source::Rds { instances, account, .. }
            if instances == &["demo"] && account.as_deref() == Some("210987654321"))
        );

        // A replica whose primary isn't listed stands alone.
        let orphan: Vec<RdsInstance> = all().into_iter().filter(|i| i.id != "demo").collect();
        let ps = propose(&orphan, &[], &[], &scope(), &[]);
        assert!(ps.iter().any(|p| p.env.name == "demo-read-only-replica"));
    }

    #[test]
    fn secrets_are_matched_by_name_replica_to_replica() {
        let ps = propose(&all(), &secrets(), &hosts(), &scope(), &[]);
        let demo = by_name(&ps, "demo");
        assert_eq!(
            demo.env.databases,
            ["ledger", "ledgerlog"],
            "from the secret paths"
        );
        let s = |k: &str| demo.env.secrets.get(k).map(String::as_str);
        assert_eq!(s("demo/ledger"), Some("demo/database/ledger/credentials"));
        assert_eq!(
            s("demo/ledgerlog"),
            Some("demo/database/ledgerlog/credentials")
        );
        assert_eq!(
            s("demo-read-only-replica/ledger"),
            Some("demo/database/ledger_replica/credentials")
        );
        assert_eq!(
            s("demo-read-only-replica/ledgerlog"),
            Some("demo/database/ledgerlog/credentials"),
            "no replica secret for ledgerlog: the primary's credentials work on a physical replica"
        );
        assert!(demo.notes.iter().any(|n| n
            == "demo-read-only-replica/ledgerlog uses the primary's secret \
                demo/database/ledgerlog/credentials: no replica secret names ledgerlog"));
        assert!(
            demo.env
                .secrets
                .values()
                .all(|v| !v.ends_with("-database-password")),
            "never a master-password secret"
        );
        assert!(demo
            .notes
            .iter()
            .any(|n| n.contains("instance was created with ledger")));
    }

    #[test]
    fn a_short_prefix_matches_but_the_full_name_wins() {
        let ps = propose(&all(), &secrets(), &[], &scope(), &[]);
        let dev = by_name(&ps, "development");
        assert_eq!(
            dev.env
                .secrets
                .get("development/ledger")
                .map(String::as_str),
            Some("development/database/ledger/credentials"),
            "beats dev/database/ledger"
        );
        let prod = by_name(&ps, "production");
        assert_eq!(
            prod.env
                .secrets
                .get("production/ledger")
                .map(String::as_str),
            Some("production/database/ledger/credentials"),
            "/credentials beats the bare production/database/ledger"
        );
        assert!(prod
            .notes
            .iter()
            .any(|n| n.contains("instance was created with prod")));
    }

    #[test]
    fn a_tie_is_a_note_not_a_guess() {
        let tied: Vec<String> = ["demo/database/ledger/app", "demo/database/ledger/admin"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ps = propose(&all(), &tied, &[], &scope(), &[]);
        let demo = by_name(&ps, "demo");
        assert!(!demo.env.secrets.contains_key("demo"));
        assert!(
            demo.notes
                .iter()
                .any(|n| n.contains("pick a secret for demo/ledger")
                    && n.contains("demo/database/ledger/admin")),
            "{:?}",
            demo.notes
        );
        // Nothing naming the database at all.
        let ps = propose(&all(), &[], &[], &scope(), &[]);
        assert!(by_name(&ps, "demo")
            .notes
            .iter()
            .any(|n| n.contains("no secret names ledger")));
    }

    #[test]
    fn the_bastion_is_the_public_host_beside_the_instance() {
        let ps = propose(&all(), &[], &hosts(), &scope(), &[]);
        assert_eq!(
            by_name(&ps, "demo").env.ssh_tunnel.as_deref(),
            Some("ec2-1.compute.amazonaws.com")
        );
        assert_eq!(
            by_name(&ps, "development").env.ssh_tunnel,
            None,
            "the fixture's development has no VPC"
        );

        let mut two = hosts();
        two[1].vpc_id = Some("vpc-0a1b2c3d".into());
        let ps = propose(&all(), &[], &two, &scope(), &[]);
        let demo = by_name(&ps, "demo");
        assert_eq!(demo.env.ssh_tunnel, None, "two candidates: not guessed");
        assert!(demo
            .notes
            .iter()
            .any(|n| n.contains("several public hosts")));

        two[1].name = Some("prod-bastion".into());
        let ps = propose(&all(), &[], &two, &scope(), &[]);
        assert_eq!(
            by_name(&ps, "demo").env.ssh_tunnel.as_deref(),
            Some("ec2-2.compute.amazonaws.com"),
            "a bastion by name breaks the tie"
        );
    }

    #[test]
    fn an_environment_already_listing_the_primary_covers_it() {
        let existing = parse_config(
            "[[environments]]\nname = \"prod\"\nsource = { kind = \"rds\", instances = [\"prod*\"] }\n",
        )
        .unwrap();
        let ps = propose(&all(), &[], &[], &scope(), &existing);
        assert_eq!(
            by_name(&ps, "production").covered_by.as_deref(),
            Some("prod")
        );
        assert_eq!(by_name(&ps, "demo").covered_by, None);
        assert!(append("", &[by_name(&ps, "production")]).is_err());
        let chosen = choose(&ps, &[]).unwrap();
        assert!(
            chosen.iter().all(|p| p.env.name != "production"),
            "covered ones aren't chosen"
        );
        assert!(choose(&ps, &["nope".into()])
            .unwrap_err()
            .contains("proposed: demo"));
    }

    #[test]
    fn rendered_proposals_load_back_as_they_were_proposed() {
        let ps = propose(&all(), &secrets(), &hosts(), &scope(), &[]);
        let all_refs: Vec<&Proposal> = ps.iter().collect();
        let existing = "# mine\n[[environments]]\nname = \"local\"\nsource = { kind = \"static\", connections = [\"a\"] }\n";
        let text = append(existing, &all_refs).unwrap();
        assert!(
            text.starts_with(existing),
            "what was there is kept byte for byte"
        );
        let loaded = parse_config(&text).unwrap();
        assert_eq!(loaded.len(), 4);
        for (p, l) in ps.iter().zip(&loaded[1..]) {
            assert_eq!(&p.env, l, "round trip of {}", p.env.name);
        }
        // Appending an environment that exists already is refused.
        let again = append(&text, &[&ps[0]]);
        assert_eq!(
            again.unwrap_err(),
            "demo is already covered by environment \"demo\""
        );
        // A result that wouldn't load is never returned: the same one twice.
        let twice = append("", &[&ps[0], &ps[0]]);
        assert!(twice.unwrap_err().contains("nothing was saved"));
    }

    #[test]
    fn quoting_survives_awkward_names() {
        let mut p = propose(&all(), &[], &[], &scope(), &[]).remove(0);
        p.env.name = "de\"mo \\ 'x'".into();
        let text = append("", &[&p]).unwrap();
        assert_eq!(parse_config(&text).unwrap()[0].name, "de\"mo \\ 'x'");
        // A file without a final newline still gets a block of its own.
        let text = append("# mine, no newline at the end", &[&p]).unwrap();
        assert!(
            text.starts_with("# mine, no newline at the end\n\n"),
            "{text}"
        );
        assert_eq!(parse_config(&text).unwrap().len(), 1);
    }

    #[test]
    fn listings_parse_names_and_public_hosts_only() {
        let names = parse_secret_names(
            r#"{"SecretList": [{"Name": "a/b", "ARN": "arn:x"}, {"ARN": "no-name"}]}"#,
        )
        .unwrap();
        assert_eq!(names, ["a/b"]);
        assert!(parse_secret_names("{}").is_err());

        let hosts = parse_hosts(
            r#"{"Reservations": [{"Instances": [
              {"InstanceId": "i-1", "VpcId": "vpc-1", "PublicDnsName": "ec2-1.x",
               "Tags": [{"Key": "env", "Value": "x"}, {"Key": "Name", "Value": "bastion"}]},
              {"InstanceId": "i-2", "VpcId": "vpc-1", "PublicDnsName": ""}
            ]}]}"#,
        )
        .unwrap();
        assert_eq!(hosts.len(), 1, "no public DNS, no bastion");
        assert_eq!(hosts[0].name.as_deref(), Some("bastion"));
        assert!(parse_hosts("[]").is_err());

        let a = describe_hosts_args(&scope()).join(" ");
        assert!(a.contains("Name=instance-state-name,Values=running"), "{a}");
        assert!(list_secrets_args(&scope())
            .join(" ")
            .contains("--region us-west-1"));
    }

    #[test]
    fn cli_flags_parse_and_mistakes_are_usage_errors() {
        let a = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        let o = parse_cli(&a("discover --region us-west-1 --account 210987654321")).unwrap();
        assert_eq!(o.scope.region.as_deref(), Some("us-west-1"));
        assert_eq!(o.save, None, "printing is the default");
        assert_eq!(parse_cli(&a("discover --save")).unwrap().save, Some(vec![]));
        assert_eq!(
            parse_cli(&a("discover --save demo test")).unwrap().save,
            Some(vec!["demo".to_string(), "test".to_string()])
        );
        for bad in [
            "",
            "list",
            "discover --region",
            "discover demo",
            "discover --bogus",
            "discover --profile --debug",
        ] {
            assert!(parse_cli(&a(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_report_says_what_it_could_not_list() {
        let d = Discovery {
            scope: scope(),
            instances: all(),
            secrets: Err("AccessDenied".into()),
            hosts: Ok(hosts()),
        };
        let existing = parse_config(
            "[[environments]]\nname = \"prod\"\nsource = { kind = \"rds\", instances = [\"production\"] }\n",
        )
        .unwrap();
        let out = report(&d, &proposals(&d, &existing));
        assert!(
            out.starts_with("# discovered in AWS account 210987654321, us-west-1\n"),
            "{out}"
        );
        assert!(out.contains("# secret names not listed, so no secrets: AccessDenied"));
        assert!(out.contains("# production: already covered by environment \"prod\""));
        assert!(out.contains("name = \"demo\""));
        assert!(!out.contains("name = \"production\""));
    }

    #[test]
    fn a_prefix_never_borrows_another_environments_secrets() {
        let names: Vec<String> = [
            "demo/database/ledger/credentials",
            "dev/database/ledger/credentials",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            secrets_for("demo2", &names, &["demo"]),
            Vec::<&str>::new(),
            "demo is an environment of its own"
        );
        assert_eq!(
            secrets_for("demo2", &names, &[]),
            ["demo/database/ledger/credentials"]
        );
        // Both shaped alike: the exact environment wins over the prefix.
        let both = [
            "dev/database/ledger/credentials",
            "development/database/ledger/credentials",
        ];
        assert_eq!(
            pick_secret("development", &both, "ledger", false),
            Ok("development/database/ledger/credentials")
        );
    }

    #[test]
    fn append_rechecks_coverage_against_the_file_it_appends_to() {
        // Proposed when nothing covered demo; the file has it since.
        let p = propose(&all(), &[], &[], &scope(), &[]).remove(0);
        assert_eq!(p.covered_by, None);
        let now = "[[environments]]\nname = \"mine\"\nsource = { kind = \"rds\", instances = [\"de*\"] }\n";
        assert_eq!(
            append(now, &[&p]).unwrap_err(),
            "demo is already covered by environment \"mine\""
        );
        assert!(append("not toml [", &[&p])
            .unwrap_err()
            .contains("existing file doesn't load"));
    }

    #[test]
    fn the_save_lock_waits_for_its_holder_then_gives_up() {
        let dir = std::env::temp_dir().join(format!("pgman-lock-{}", std::process::id()));
        let path = dir.join(".environments.toml.lock");
        let held = lock_exclusive(&path, std::time::Duration::ZERO).unwrap();
        let started = std::time::Instant::now();
        let err = lock_exclusive(&path, std::time::Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("another pgman is saving"), "{err}");
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(200),
            "it waited"
        );
        drop(held);
        assert!(
            lock_exclusive(&path, std::time::Duration::ZERO).is_ok(),
            "free once dropped"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_choices_start_ambient_and_never_repeat() {
        let existing = parse_config(
            "[[environments]]\nname = \"a\"\nsource = { kind = \"rds\", instances = [\"a\"], profile = \"work\", region = \"us-west-1\" }\n\
             [[environments]]\nname = \"b\"\nsource = { kind = \"rds\", instances = [\"b\"], profile = \"work\", region = \"us-west-1\" }\n\
             [[environments]]\nname = \"c\"\nsource = { kind = \"static\", connections = [\"x\"] }\n",
        )
        .unwrap();
        let labels: Vec<String> = scope_choices(
            &existing,
            &["default".into(), "work".into(), "staging".into()],
        )
        .iter()
        .map(scope_label)
        .collect();
        assert_eq!(
            labels,
            [
                "ambient profile",
                "profile work · us-west-1",
                "profile default",
                "profile staging"
            ],
            "work is already there with its region; static sources have no scope"
        );
    }

    #[test]
    fn a_projects_version_table_rides_on_new_proposals_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("pgman-vt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".pgman")).unwrap();
        std::fs::write(
            dir.join(".pgman/pgman.toml"),
            "[patches]\ndir = \"db/patches\"\ntable = \"public.db_version\"\ncolumn = \"id\"\n",
        )
        .unwrap();
        let project = project_version_table(&dir.join(".pgman")).expect("found from inside");
        assert_eq!(project.1.table, "public.db_version");
        let patches = project.1.patches.clone().unwrap();
        assert!(
            std::path::Path::new(&patches).is_absolute() && patches.ends_with("db/patches"),
            "{patches}"
        );

        let existing = parse_config(
            "[[environments]]\nname = \"prod\"\nsource = { kind = \"rds\", instances = [\"production\"] }\n",
        )
        .unwrap();
        let mut ps = propose(&all(), &[], &[], &scope(), &existing);
        with_version_table(&mut ps, Some(&project));
        let demo = by_name(&ps, "demo");
        assert_eq!(demo.env.version_table.as_ref(), Some(&project.1));
        assert!(!demo.env.probe, "probing stays the operator's call");
        assert!(demo.notes.iter().any(|n| n.contains("set probe = true")));
        let prod = by_name(&ps, "production");
        assert!(prod.env.version_table.is_none(), "covered: untouched");

        // What a save writes loads back as proposed, probe included.
        let mut p = demo.clone();
        p.env.probe = true;
        if let Some(v) = p.env.version_table.as_mut() {
            v.databases = vec!["ledger".into()];
        }
        let text = append("", &[&p]).unwrap();
        assert_eq!(parse_config(&text).unwrap()[0], p.env);

        with_version_table(&mut ps, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
