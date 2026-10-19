//! What the environments overview reads from each database it probes:
//! the server's version, whether it is in recovery and how old its last
//! replayed change is, and — when the environment names a version table —
//! the highest patch id it has recorded and which patch files it hasn't.
//!
//! Every statement here is a fixed read; the connection it runs on is
//! read-only (`conn::connect_via`).

use super::patches::{self, PatchFile, PatchesConfig};
use crate::conn::{self, TypedRows};

/// One round trip for everything that needs no configuration.
pub const STATUS_SQL: &str = "SELECT current_setting('server_version') AS server_version, \
  pg_is_in_recovery() AS in_recovery, \
  EXTRACT(EPOCH FROM now() - pg_last_xact_replay_timestamp())::float8 AS lag_secs, \
  (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database()) AS sessions, \
  pg_size_pretty(pg_database_size(current_database())) AS size, \
  (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
     AND cardinality(pg_blocking_pids(pid)) > 0) AS blocked, \
  (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
     AND pid <> pg_backend_pid() AND xact_start < now() - interval '5 minutes') AS long_tx";

/// A probe's findings. `None` is "not known", never "zero".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Probe {
    pub server_version: Option<String>,
    pub in_recovery: Option<bool>,
    /// Age of the last replayed change, in seconds. NULL on a primary and
    /// on a standby that has replayed nothing yet; an idle primary makes
    /// it grow too, so it is an upper bound on lag, not proof of it.
    pub lag_secs: Option<f64>,
    /// Highest id in the version table.
    pub schema_version: Option<i64>,
    /// Patch files whose id the database hasn't recorded.
    pub pending: Option<usize>,
    /// Why the version table couldn't be read; the rest still stands.
    pub schema_error: Option<String>,
    /// Backends connected to this database, the probe's own included.
    pub sessions: Option<i64>,
    /// The database's size, as `pg_size_pretty` puts it.
    pub size: Option<String>,
    /// Sessions waiting on another session's lock.
    pub blocked: Option<i64>,
    /// Transactions open more than five minutes (idle in transaction
    /// included): what holds locks and blocks vacuum and migrations.
    pub long_tx: Option<i64>,
    /// The migration tool found keeping this database's history, when
    /// the version isn't read from a configured version table.
    pub migration_tool: Option<String>,
    /// That tool's current version, as it names it.
    pub schema_label: Option<String>,
}

/// Where a probe reads the schema version from.
#[derive(Debug, Clone, Copy)]
pub enum SchemaSource<'a> {
    /// Nothing: the environment named a version table that this database
    /// doesn't keep.
    None,
    /// pgman's own version table, with the patch files when they were
    /// read.
    Patches(&'a PatchesConfig, Option<&'a [PatchFile]>),
    /// Whichever migration tool's table is there (`query::migrations`),
    /// with Flyway's migration file names when a directory was given.
    Detect(Option<&'a [String]>),
}

/// [`STATUS_SQL`]'s one row. Pure.
pub fn parse_status(rows: &TypedRows) -> Probe {
    let cell = |i: usize| {
        rows.rows
            .first()
            .and_then(|r| r.get(i))
            .and_then(|c| c.clone())
    };
    Probe {
        server_version: cell(0),
        in_recovery: cell(1).map(|v| v == "t" || v == "true"),
        lag_secs: cell(2).and_then(|v| v.parse().ok()),
        sessions: cell(3).and_then(|v| v.parse().ok()),
        size: cell(4),
        blocked: cell(5).and_then(|v| v.parse().ok()),
        long_tx: cell(6).and_then(|v| v.parse().ok()),
        ..Probe::default()
    }
}

/// Probe one database. `patches` names the version table and, when the
/// patch files were read, the files to reconcile against it.
pub async fn run(
    client: &tokio_postgres::Client,
    source: SchemaSource<'_>,
) -> Result<Probe, String> {
    let (rows, _) = conn::run_statement_typed_capped(client, STATUS_SQL, Some(1))
        .await
        .map_err(|e| e.msg)?;
    let mut probe = parse_status(&rows);
    let (cfg, files) = match source {
        SchemaSource::None => return Ok(probe),
        SchemaSource::Detect(flyway_files) => {
            match super::migrations::read(client, flyway_files).await {
                Ok(Some(m)) => {
                    probe.migration_tool = Some(m.tool.to_string());
                    probe.schema_label = m.version;
                    probe.pending = m.pending;
                    probe.schema_error =
                        m.failed.map(|f| format!("{} migration {f} failed", m.tool));
                }
                Ok(None) => {}
                Err(e) => probe.schema_error = Some(e),
            }
            return Ok(probe);
        }
        SchemaSource::Patches(cfg, files) => (cfg, files),
    };
    let read = async {
        let sql = patches::applied_ids_sql(cfg)?;
        let (typed, truncated) =
            conn::run_statement_typed_capped(client, &sql, Some(patches::MAX_APPLIED_IDS))
                .await
                .map_err(|e| e.msg)?;
        patches::applied_ids(&typed, truncated, cfg)
    };
    match read.await {
        Ok(applied) => {
            probe.schema_version = applied.iter().copied().max();
            if let Some(files) = files {
                probe.pending = Some(patches::reconcile(files.to_vec(), &applied).pending.len());
            }
        }
        Err(e) => probe.schema_error = Some(e),
    }
    Ok(probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(cells: &[Option<&str>]) -> TypedRows {
        TypedRows {
            columns: Vec::new(),
            rows: vec![cells.iter().map(|c| c.map(str::to_string)).collect()],
            affected: None,
        }
    }

    #[test]
    fn status_reads_version_recovery_and_lag_and_null_stays_unknown() {
        let p = parse_status(&rows(&[
            Some("16.10"),
            Some("t"),
            Some("0.42"),
            Some("7"),
            Some("1203 MB"),
            Some("2"),
            Some("1"),
        ]));
        assert_eq!((p.blocked, p.long_tx), (Some(2), Some(1)));
        assert_eq!(p.server_version.as_deref(), Some("16.10"));
        assert_eq!((p.sessions, p.size.as_deref()), (Some(7), Some("1203 MB")));
        assert_eq!(p.in_recovery, Some(true));
        assert_eq!(p.lag_secs, Some(0.42));

        let p = parse_status(&rows(&[Some("16.10"), Some("f"), None]));
        assert_eq!((p.in_recovery, p.lag_secs), (Some(false), None));

        let p = parse_status(&TypedRows {
            columns: Vec::new(),
            rows: Vec::new(),
            affected: None,
        });
        assert_eq!(p, Probe::default(), "no row: nothing known");
    }
}
