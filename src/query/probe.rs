//! What the environments overview reads from each database it probes:
//! the server's version, whether it is in recovery and how old its last
//! replayed change is, and — when the environment names a version table —
//! the highest patch id it has recorded and which patch files it hasn't.
//!
//! Every statement here is a fixed read; the connection it runs on is
//! read-only (`conn::connect_via`).

use super::patches::{self, PatchFile, PatchesConfig};
use crate::conn::{self, TypedRows};

/// A standby's replication lag in seconds, as SQL: 0 when it is
/// streaming and has replayed all it has received (a quiet primary
/// doesn't make it grow), otherwise the age of the last replayed change
/// (so a standby whose stream is down shows how stale it is); NULL on a
/// primary. Shared by the probe and MCP's replica check.
macro_rules! lag_secs_sql {
    () => {
        "(CASE WHEN NOT pg_is_in_recovery() THEN NULL \
           WHEN pg_last_wal_receive_lsn() = pg_last_wal_replay_lsn() \
             AND EXISTS (SELECT 1 FROM pg_stat_wal_receiver WHERE status = 'streaming') THEN 0 \
           ELSE EXTRACT(EPOCH FROM now() - pg_last_xact_replay_timestamp()) END)::float8"
    };
}
pub(crate) use lag_secs_sql;

/// One round trip for everything that needs no configuration.
pub const STATUS_SQL: &str = concat!(
    "SELECT current_setting('server_version') AS server_version, \
     pg_is_in_recovery() AS in_recovery, ",
    lag_secs_sql!(),
    " AS lag_secs, \
     (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database()) AS sessions, \
     pg_size_pretty(pg_database_size(current_database())) AS size, \
     (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
        AND cardinality(pg_blocking_pids(pid)) > 0) AS blocked, \
     CASE WHEN pg_has_role(current_user, 'pg_read_all_stats', 'MEMBER') THEN \
       (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
          AND pid <> pg_backend_pid() AND xact_start < now() - interval '5 minutes') \
     END AS long_tx"
);

/// A probe's findings. `None` is "not known", never "zero".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Probe {
    pub server_version: Option<String>,
    pub in_recovery: Option<bool>,
    /// Replication lag, in seconds. 0 when the standby is streaming and has
    /// replayed all it has received (a quiet primary doesn't make it
    /// grow); otherwise the age of the last replayed change, so a standby
    /// whose stream is down shows how stale it is. NULL on a primary and on
    /// a standby that has replayed nothing yet.
    pub lag_secs: Option<f64>,
    /// Highest id in the version table.
    pub schema_version: Option<i64>,
    /// Patch files whose id the database hasn't recorded.
    pub pending: Option<usize>,
    /// Why the version table couldn't be read, or the migration tool's
    /// record of a failed migration; the rest still stands.
    pub schema_error: Option<String>,
    /// Why the patch or migration files couldn't be read, prefixed with
    /// which (`patch files: …`): the version still stands, how many are
    /// pending is unknown. pgman's side, not the database's.
    pub files_error: Option<String>,
    /// Backends connected to this database, the probe's own included.
    pub sessions: Option<i64>,
    /// The database's size, as `pg_size_pretty` puts it.
    pub size: Option<String>,
    /// Sessions waiting on another session's lock.
    pub blocked: Option<i64>,
    /// Transactions open more than five minutes (idle in transaction
    /// included): what holds locks and blocks vacuum and migrations.
    /// `None` when the probe user can't see other sessions' transactions
    /// (it needs `pg_read_all_stats`): unknown, not zero.
    pub long_tx: Option<i64>,
    /// The migration tool found keeping this database's history, when
    /// the version isn't read from a configured version table.
    pub migration_tool: Option<String>,
    /// That tool's current version, as it names it. From a configured
    /// version table, the highest id as written when none of its ids is
    /// an integer (`schema_version` is then `None`).
    pub schema_label: Option<String>,
    /// How many migrations that tool records as applied.
    pub migrations_applied: Option<usize>,
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

/// The version a probe shows from a version table's ids: the highest
/// integer, or — when the column holds none, only text such as `1.2` —
/// the highest of those as written, rather than no version at all.
/// Pure.
pub fn shown_version(ids: &[i64], others: &[String]) -> (Option<i64>, Option<String>) {
    match ids.iter().copied().max() {
        Some(v) => (Some(v), None),
        None => (
            None,
            others
                .iter()
                .max_by(|a, b| {
                    super::migrations::version_key(a)
                        .cmp(&super::migrations::version_key(b))
                        .then_with(|| a.cmp(b))
                })
                .cloned(),
        ),
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
                    probe.migrations_applied = Some(m.applied);
                    probe.pending = m.pending;
                    // `migrations migration 12 failed` reads as a typo: name
                    // the shared table instead.
                    let who = match m.tool {
                        "migrations" => "schema_migrations:",
                        tool => tool,
                    };
                    probe.schema_error = m.failed.map(|f| format!("{who} migration {f} failed"));
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
        patches::applied_ids_and_others(&typed, truncated, cfg)
    };
    match read.await {
        Ok((applied, others)) => {
            (probe.schema_version, probe.schema_label) = shown_version(&applied, &others);
            // Patch files carry integer ids, so an id that isn't one
            // can't be the record of any of them: leaving it out makes
            // nothing look pending that isn't.
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

    #[test]
    fn a_version_column_without_integer_ids_shows_its_highest_as_written() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(shown_version(&[3, 12, 7], &[]), (Some(12), None));
        assert_eq!(
            shown_version(&[3], &s(&["9.9"])),
            (Some(3), None),
            "an integer is the version when there is one"
        );
        assert_eq!(
            shown_version(&[], &s(&["1.9", "1.10", "1.2"])),
            (None, Some("1.10".into())),
            "1.10 is after 1.9"
        );
        assert_eq!(
            shown_version(&[], &s(&["beta", "alpha"])),
            (None, Some("beta".into())),
            "no numbers: by text"
        );
        assert_eq!(shown_version(&[], &[]), (None, None));
    }
}
