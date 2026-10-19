//! Probes: connect to each database of an environment that opted in
//! (`probe = true`) and read what only the database knows — its version,
//! recovery state and lag, and its patch level (`query::probe`).
//!
//! - **Replicas first.** A primary whose replica of the same database is
//!   in the environment is not connected to; its row reads "via replica".
//!   A primary is touched only when there is no replica to read instead.
//! - **One tunnel per bastion.** Every database behind the same bastion
//!   rides one `ssh` carrying a forward each (`SshTunnel::open_many`),
//!   closed when the probe ends.
//! - **Read-only, bounded.** Each connection is read-only with a short
//!   statement timeout, and each database's probe has an overall deadline.
//! - A secret is read once per probe however many databases share it.
//!
//! [`plan`] is pure; [`run`] does the I/O.

use super::{EnvReport, Target};
use crate::creds::aws_secrets::{SecretCreds, SecretRef};
use crate::mcp::connections::Role;
use crate::query::probe::Probe;
use std::collections::HashMap;

/// Statement timeout on a probe connection.
const PROBE_STATEMENT_TIMEOUT_MS: u64 = 5_000;

/// Deadline for one database's connect and reads, tunnel excluded.
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// What a probe does with one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Connect,
    /// A primary read through its replica instead.
    ViaReplica,
}

/// One [`Step`] per target, in order. Pure.
pub fn plan(targets: &[Target]) -> Vec<Step> {
    targets
        .iter()
        .map(|t| {
            let has_replica = targets.iter().any(|r| {
                r.role == Role::Replica
                    && r.replica_of.as_deref() == Some(t.server.as_str())
                    && r.database == t.database
            });
            if t.role == Role::Primary && has_replica {
                Step::ViaReplica
            } else {
                Step::Connect
            }
        })
        .collect()
}

/// A probe's answer for one row: `None` for a primary read via its
/// replica.
pub type Outcome = Option<Result<Probe, String>>;

/// Reads a secret. Blocking (the aws CLI).
pub type FetchSecret =
    std::sync::Arc<dyn Fn(&SecretRef) -> Result<SecretCreds, String> + Send + Sync>;

/// Probe every row of `report`, one [`Outcome`] per target in order.
pub async fn run(report: EnvReport, fetch_secret: FetchSecret) -> Vec<Outcome> {
    let steps = plan(&report.targets);
    let settings = report.probe.clone().unwrap_or(super::ProbeSettings {
        version_table: None,
    });
    let cfg = settings.version_table.as_ref().map(|v| v.config());

    // Patch files, read once for the whole environment.
    let files: Option<Result<Vec<crate::query::patches::PatchFile>, String>> = match settings
        .version_table
        .as_ref()
        .and_then(|v| v.patch_dir().map(|d| (d, v.config())))
    {
        None => None,
        Some((Err(e), _)) => Some(Err(e)),
        Some((Ok(dir), cfg)) => Some(
            tokio::task::spawn_blocking(move || {
                crate::query::patches::read_patch_files(&dir, &cfg)
            })
            .await
            .unwrap_or_else(|e| Err(format!("patch files task failed: {e}"))),
        ),
    };

    // Each connectable row's DSN with its credentials, or why not.
    let mut secrets: HashMap<String, Result<SecretCreds, String>> = HashMap::new();
    let mut ready: Vec<Option<Result<crate::conn::Dsn, String>>> = Vec::new();
    for (t, step) in report.targets.iter().zip(&steps) {
        if *step == Step::ViaReplica {
            ready.push(None);
            continue;
        }
        let spec = match &t.connect {
            Ok(spec) => spec.clone(),
            Err(e) => {
                ready.push(Some(Err(e.clone())));
                continue;
            }
        };
        let mut dsn = spec.dsn;
        if let Some(secret) = spec.secret {
            let creds = match secrets.get(&secret.id) {
                Some(c) => c.clone(),
                None => {
                    let fetch = fetch_secret.clone();
                    let s = secret.clone();
                    let got = tokio::task::spawn_blocking(move || fetch(&s))
                        .await
                        .unwrap_or_else(|e| Err(format!("secret task failed: {e}")));
                    secrets.insert(secret.id.clone(), got.clone());
                    got
                }
            };
            match creds {
                Ok(c) => crate::creds::aws_secrets::apply(&mut dsn, c),
                Err(e) => {
                    ready.push(Some(Err(format!("secret {}: {e}", secret.id))));
                    continue;
                }
            }
        }
        ready.push(Some(Ok(dsn)));
    }

    // One ssh per bastion, a forward per database behind it.
    let mut by_bastion: HashMap<crate::tunnel::SshTunnelSpec, Vec<usize>> = HashMap::new();
    for (i, r) in ready.iter().enumerate() {
        if let Some(Ok(dsn)) = r {
            if let Some(spec) = &dsn.ssh_tunnel {
                by_bastion.entry(spec.clone()).or_default().push(i);
            }
        }
    }
    let mut via: HashMap<usize, u16> = HashMap::new();
    let mut tunnels = Vec::new();
    for (spec, rows) in by_bastion {
        let remotes: Vec<(String, u16)> = rows
            .iter()
            .filter_map(|i| match &ready[*i] {
                Some(Ok(d)) => Some((d.host.clone(), d.port)),
                _ => None,
            })
            .collect();
        let bastion = spec.host.clone();
        let opened = tokio::task::spawn_blocking(move || {
            crate::tunnel::SshTunnel::open_many(&spec, &remotes)
        })
        .await
        .unwrap_or_else(|e| Err(format!("tunnel task failed: {e}")));
        match opened {
            Ok((tunnel, ports)) => {
                for (i, port) in rows.iter().zip(ports) {
                    via.insert(*i, port);
                }
                tunnels.push(tunnel);
            }
            Err(e) => {
                for i in rows {
                    ready[i] = Some(Err(format!("ssh tunnel via {bastion}: {e}")));
                }
            }
        }
    }

    let files_ref = files.as_ref().and_then(|f| f.as_ref().ok());
    let files_err = files.as_ref().and_then(|f| f.as_ref().err()).cloned();
    let probes = ready.into_iter().enumerate().map(|(i, r)| {
        let cfg = cfg.clone();
        let files_err = files_err.clone();
        let port = via.get(&i).copied();
        async move {
            let mut dsn = match r? {
                Ok(d) => d,
                Err(e) => return Some(Err(e)),
            };
            dsn.ssh_tunnel = None;
            let probe = async {
                let client =
                    crate::conn::connect_via(dsn, port, PROBE_STATEMENT_TIMEOUT_MS).await?;
                crate::query::probe::run(
                    &client,
                    cfg.as_ref().map(|c| (c, files_ref.map(Vec::as_slice))),
                )
                .await
            };
            let result = match tokio::time::timeout(PROBE_DEADLINE, probe).await {
                Ok(r) => r,
                Err(_) => Err(format!("no answer within {}s", PROBE_DEADLINE.as_secs())),
            };
            Some(result.map(|mut p| {
                if p.schema_error.is_none() {
                    p.schema_error = files_err.map(|e| format!("patch files: {e}"));
                }
                p
            }))
        }
    });
    let out = futures::future::join_all(probes).await;
    drop(tunnels);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(server: &str, db: &str, role: Role, of: Option<&str>) -> Target {
        Target {
            database: db.into(),
            server: server.into(),
            role,
            replica_of: of.map(str::to_string),
            host: None,
            port: None,
            engine: None,
            status: None,
            class: None,
            multi_az: None,
            connect: Err("not in a test".into()),
        }
    }

    #[test]
    fn a_primary_is_read_through_its_replica_of_the_same_database_only() {
        let rows = [
            t("demo", "ledger", Role::Primary, None),
            t("demo", "ledgerlog", Role::Primary, None),
            t("demo-r", "ledger", Role::Replica, Some("demo")),
            t("solo", "app", Role::Primary, None),
            t("dev", "app", Role::Direct, None),
        ];
        assert_eq!(
            plan(&rows),
            [
                Step::ViaReplica,
                Step::Connect,
                Step::Connect,
                Step::Connect,
                Step::Connect
            ],
            "ledgerlog has no replica row; solo has no replica at all"
        );
    }

    #[tokio::test]
    async fn rows_that_cannot_connect_say_why_and_nothing_is_contacted() {
        let report = EnvReport {
            name: "x".into(),
            source: "test".into(),
            targets: vec![
                t("demo", "ledger", Role::Primary, None),
                t("demo-r", "ledger", Role::Replica, Some("demo")),
            ],
            error: None,
            probe: Some(super::super::ProbeSettings {
                version_table: None,
            }),
        };
        let out = run(
            report,
            std::sync::Arc::new(|_| panic!("no secret is read for an unconnectable row")),
        )
        .await;
        assert_eq!(out[0], None, "via replica");
        assert_eq!(out[1], Some(Err("not in a test".to_string())));
    }

    #[tokio::test]
    async fn a_secret_shared_by_several_databases_is_read_once() {
        use crate::creds::aws_secrets::SecretRef;
        let spec = |db: &str| super::super::ConnectSpec {
            // A closed local port: the connect fails fast, after the secret.
            dsn: crate::conn::Dsn::parse(&format!("postgres://127.0.0.1:1/{db}?sslmode=disable"))
                .unwrap(),
            secret: Some(SecretRef {
                id: "env/database/app/credentials".into(),
                region: None,
                profile: None,
                account: None,
            }),
        };
        let mut a = t("one", "app", Role::Direct, None);
        a.connect = Ok(spec("app"));
        let mut b = t("two", "app", Role::Direct, None);
        b.connect = Ok(spec("app"));
        let report = EnvReport {
            name: "x".into(),
            source: "test".into(),
            targets: vec![a, b],
            error: None,
            probe: Some(super::super::ProbeSettings {
                version_table: None,
            }),
        };
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = reads.clone();
        let out = run(
            report,
            std::sync::Arc::new(move |_| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(crate::creds::aws_secrets::SecretCreds {
                    username: Some("app".into()),
                    password: "pw".into(),
                })
            }),
        )
        .await;
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(out.iter().all(|o| matches!(o, Some(Err(_)))), "{out:?}");
        for o in &out {
            let Some(Err(e)) = o else { unreachable!() };
            assert!(
                !e.contains("pw"),
                "the password never reaches an error: {e}"
            );
        }
    }
}
