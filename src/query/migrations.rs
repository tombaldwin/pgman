//! Schema versions kept by the common migration tools, found without any
//! configuration: each tool records what it applied in a table of its
//! own, and that table's name says which tool it is.
//!
//! | tool | table | version shown |
//! |---|---|---|
//! | Flyway | `flyway_schema_history` | newest successful version; failures flagged |
//! | Liquibase | `databasechangelog` | the last changeset run |
//! | Rails, golang-migrate | `schema_migrations` | the highest version |
//! | sqlx | `_sqlx_migrations` | the highest version; failures flagged |
//! | Django | `django_migrations` | the last migration applied |
//! | Alembic | `alembic_version` | the current revision |
//! | Prisma | `_prisma_migrations` | the last finished migration; failures flagged |
//!
//! Flyway can also count pending migrations against a directory of
//! `V<version>__<name>.sql` files. Every statement here is a fixed read.

use crate::conn::{self, TypedRows};

/// Which tool's table exists, one column per tool, in [`Tool::ALL`]
/// order. `to_regclass` follows the search path and needs no privilege.
pub const DETECT_SQL: &str = "SELECT \
  to_regclass('flyway_schema_history') IS NOT NULL, \
  to_regclass('databasechangelog') IS NOT NULL, \
  to_regclass('_prisma_migrations') IS NOT NULL, \
  to_regclass('_sqlx_migrations') IS NOT NULL, \
  to_regclass('django_migrations') IS NOT NULL, \
  to_regclass('alembic_version') IS NOT NULL, \
  to_regclass('schema_migrations') IS NOT NULL";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Flyway,
    Liquibase,
    Prisma,
    Sqlx,
    Django,
    Alembic,
    SchemaMigrations,
}

impl Tool {
    /// Detection order: the most specific table names first —
    /// `schema_migrations` is shared by several tools, so it goes last.
    pub const ALL: [Tool; 7] = [
        Tool::Flyway,
        Tool::Liquibase,
        Tool::Prisma,
        Tool::Sqlx,
        Tool::Django,
        Tool::Alembic,
        Tool::SchemaMigrations,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Flyway => "flyway",
            Tool::Liquibase => "liquibase",
            Tool::Prisma => "prisma",
            Tool::Sqlx => "sqlx",
            Tool::Django => "django",
            Tool::Alembic => "alembic",
            Tool::SchemaMigrations => "migrations",
        }
    }

    /// The read for this tool's history. Each returns whole history in
    /// apply order (oldest first), capped by the caller.
    pub fn sql(self) -> &'static str {
        match self {
            Tool::Flyway => {
                "SELECT version, success::text, type FROM flyway_schema_history \
                 WHERE version IS NOT NULL ORDER BY installed_rank"
            }
            Tool::Liquibase => "SELECT id, author FROM databasechangelog ORDER BY orderexecuted",
            Tool::Prisma => {
                "SELECT migration_name, (finished_at IS NOT NULL)::text, \
                 (rolled_back_at IS NOT NULL)::text FROM _prisma_migrations ORDER BY started_at"
            }
            Tool::Sqlx => {
                "SELECT version::text, success::text FROM _sqlx_migrations ORDER BY version"
            }
            Tool::Django => "SELECT app, name FROM django_migrations ORDER BY applied, id",
            Tool::Alembic => "SELECT version_num FROM alembic_version",
            Tool::SchemaMigrations => "SELECT version::text FROM schema_migrations",
        }
    }
}

/// What a tool's history says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrations {
    pub tool: &'static str,
    /// The current version, as the tool names it.
    pub version: Option<String>,
    /// Migrations recorded as applied.
    pub applied: usize,
    /// Migration files not yet applied, when a directory was given.
    pub pending: Option<usize>,
    /// A migration recorded as failed (and not since repaired).
    pub failed: Option<String>,
}

/// The tool whose table [`DETECT_SQL`] found, if any. Pure.
pub fn detect(rows: &TypedRows) -> Option<Tool> {
    let row = rows.rows.first()?;
    Tool::ALL
        .into_iter()
        .zip(row)
        .find_map(|(tool, cell)| matches!(cell.as_deref(), Some("t" | "true")).then_some(tool))
}

fn truthy(v: Option<&str>) -> bool {
    matches!(v, Some("t" | "true"))
}

/// `1.2.10` as numbers, for ordering and comparing versions. Trailing
/// zeros don't count: Flyway reads `1.0` and `1` as one version. Pure.
fn version_key(v: &str) -> Vec<u64> {
    let mut key: Vec<u64> = v
        .split(['.', '_'])
        .map(|p| p.trim().parse().unwrap_or(0))
        .collect();
    while key.len() > 1 && key.last() == Some(&0) {
        key.pop();
    }
    key
}

/// A Flyway versioned-migration file's version: `V1_2__add.sql` → `1.2`.
/// Repeatable (`R__`) and undo (`U`) files have none. Pure.
pub fn flyway_file_version(file: &str) -> Option<String> {
    let rest = file.strip_prefix('V')?;
    let (version, _) = rest.split_once("__")?;
    if version.is_empty() || !file.to_ascii_lowercase().ends_with(".sql") {
        return None;
    }
    Some(version.replace('_', "."))
}

/// A tool's history rows into [`Migrations`]. Pure.
pub fn parse(tool: Tool, rows: &TypedRows, files: Option<&[String]>) -> Migrations {
    let cell = |r: &Vec<Option<String>>, i: usize| r.get(i).and_then(|c| c.clone());
    let mut m = Migrations {
        tool: tool.name(),
        version: None,
        applied: 0,
        pending: None,
        failed: None,
    };
    match tool {
        Tool::Flyway | Tool::Sqlx => {
            let mut ok: Vec<String> = Vec::new();
            // Flyway: versions at or below a BASELINE never run, so their
            // files aren't pending; a successful UNDO takes its version
            // back out. (sqlx has no type column.)
            let mut baseline: Option<Vec<u64>> = None;
            for r in &rows.rows {
                let Some(v) = cell(r, 0) else { continue };
                let kind = cell(r, 2).unwrap_or_default();
                let success = truthy(cell(r, 1).as_deref());
                if kind.starts_with("UNDO") {
                    if success {
                        let key = version_key(&v);
                        ok.retain(|o| version_key(o) != key);
                    }
                    continue;
                }
                if success {
                    if kind == "BASELINE" {
                        baseline = baseline.max(Some(version_key(&v)));
                    }
                    ok.push(v);
                } else {
                    m.failed = Some(v);
                }
            }
            // A failure later repaired by a successful run of the same
            // version isn't a failure any more.
            if m.failed.as_ref().is_some_and(|f| ok.contains(f)) {
                m.failed = None;
            }
            m.applied = ok.len();
            m.version = ok.iter().max_by_key(|v| version_key(v)).cloned();
            if let (Tool::Flyway, Some(files)) = (tool, files) {
                let applied: Vec<Vec<u64>> = ok.iter().map(|v| version_key(v)).collect();
                m.pending = Some(
                    files
                        .iter()
                        .filter_map(|f| flyway_file_version(f))
                        .map(|v| version_key(&v))
                        .filter(|k| baseline.as_ref().is_none_or(|b| k > b))
                        .filter(|k| !applied.contains(k))
                        .count(),
                );
            }
        }
        Tool::Prisma => {
            for r in &rows.rows {
                let Some(name) = cell(r, 0) else { continue };
                let finished = truthy(cell(r, 1).as_deref());
                let rolled_back = truthy(cell(r, 2).as_deref());
                if finished {
                    m.applied += 1;
                    m.version = Some(name);
                } else if !rolled_back {
                    m.failed = Some(name);
                }
            }
        }
        Tool::Liquibase => {
            m.applied = rows.rows.len();
            m.version = rows.rows.last().and_then(|r| cell(r, 0));
        }
        Tool::Django => {
            m.applied = rows.rows.len();
            m.version = rows
                .rows
                .last()
                .and_then(|r| Some(format!("{}.{}", cell(r, 0)?, cell(r, 1)?)));
        }
        Tool::Alembic => {
            let heads: Vec<String> = rows.rows.iter().filter_map(|r| cell(r, 0)).collect();
            m.applied = heads.len();
            m.version = (!heads.is_empty()).then(|| heads.join("+"));
        }
        Tool::SchemaMigrations => {
            m.applied = rows.rows.len();
            m.version = rows
                .rows
                .iter()
                .filter_map(|r| cell(r, 0))
                .max_by_key(|v| version_key(v));
        }
    }
    m
}

/// Most history rows read.
const MAX_HISTORY: usize = 100_000;

/// The migration tool's state in this database, or `None` when no known
/// tool's table is there. `flyway_files` are file names in a Flyway
/// migration directory, to count pending ones.
pub async fn read(
    client: &tokio_postgres::Client,
    flyway_files: Option<&[String]>,
) -> Result<Option<Migrations>, String> {
    let (found, _) = conn::run_statement_typed_capped(client, DETECT_SQL, Some(1))
        .await
        .map_err(|e| e.msg)?;
    let Some(tool) = detect(&found) else {
        return Ok(None);
    };
    let (rows, truncated) = conn::run_statement_typed_capped(client, tool.sql(), Some(MAX_HISTORY))
        .await
        .map_err(|e| format!("{}: {}", tool.name(), e.msg))?;
    if truncated {
        return Err(format!(
            "{} history has more than {MAX_HISTORY} rows; not read",
            tool.name()
        ));
    }
    Ok(Some(parse(tool, &rows, flyway_files)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(r: &[&[Option<&str>]]) -> TypedRows {
        TypedRows {
            columns: Vec::new(),
            rows: r
                .iter()
                .map(|row| row.iter().map(|c| c.map(str::to_string)).collect())
                .collect(),
            affected: None,
        }
    }

    #[test]
    fn the_first_table_found_names_the_tool() {
        let f = Some("f");
        let t = Some("t");
        assert_eq!(detect(&rows(&[&[f, f, f, f, f, f, f]])), None);
        assert_eq!(
            detect(&rows(&[&[f, f, f, f, f, f, t]])),
            Some(Tool::SchemaMigrations)
        );
        assert_eq!(
            detect(&rows(&[&[t, f, f, f, f, f, t]])),
            Some(Tool::Flyway),
            "schema_migrations is shared: a specific table wins"
        );
        assert_eq!(
            detect(&rows(&[&[f, f, f, f, Some("true"), f, f]])),
            Some(Tool::Django)
        );
    }

    #[test]
    fn flyway_reads_the_newest_success_flags_failures_and_counts_pending() {
        let history = rows(&[
            &[Some("1"), Some("true")],
            &[Some("1.1"), Some("true")],
            &[Some("1.10"), Some("true")],
            &[Some("1.2"), Some("false")],
        ]);
        let files: Vec<String> = [
            "V1__init.sql",
            "V1_1__users.sql",
            "V1_2__orders.sql",
            "V1_10__index.sql",
            "V2__new.sql",
            "R__views.sql",
            "README.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let m = parse(Tool::Flyway, &history, Some(&files));
        assert_eq!(m.version.as_deref(), Some("1.10"), "1.10 is newer than 1.2");
        assert_eq!(m.applied, 3);
        assert_eq!(m.failed.as_deref(), Some("1.2"));
        assert_eq!(
            m.pending,
            Some(2),
            "1.2 (failed) and 2; R__ isn't versioned"
        );

        // Repaired: the failure followed by a success of the same version.
        let repaired = rows(&[&[Some("1.2"), Some("false")], &[Some("1.2"), Some("true")]]);
        assert_eq!(parse(Tool::Flyway, &repaired, None).failed, None);
        assert_eq!(
            parse(Tool::Flyway, &repaired, None).pending,
            None,
            "no files, no count"
        );
    }

    #[test]
    fn flyway_skips_versions_below_a_baseline_and_undone_ones() {
        let files: Vec<String> = [
            "V1__a.sql",
            "V2__b.sql",
            "V3__c.sql",
            "V101__d.sql",
            "V1_0__e.sql",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // baselineOnMigrate at 100: V1–V3 never run; only V101 is pending.
        let baselined = rows(&[&[Some("100"), Some("true"), Some("BASELINE")]]);
        let m = parse(Tool::Flyway, &baselined, Some(&files));
        assert_eq!(m.pending, Some(1), "V101 only");
        assert_eq!(m.version.as_deref(), Some("100"));
        // 1.0 is version 1 to Flyway.
        let applied = rows(&[
            &[Some("1"), Some("true"), Some("SQL")],
            &[Some("2"), Some("true"), Some("SQL")],
            &[Some("3"), Some("true"), Some("SQL")],
        ]);
        let m = parse(Tool::Flyway, &applied, Some(&files));
        assert_eq!(
            m.pending,
            Some(1),
            "V1_0 is V1, already applied; V101 pending"
        );
        // An undo takes its version back out.
        let undone = rows(&[
            &[Some("1"), Some("true"), Some("SQL")],
            &[Some("2"), Some("true"), Some("SQL")],
            &[Some("2"), Some("true"), Some("UNDO_SQL")],
        ]);
        let m = parse(Tool::Flyway, &undone, None);
        assert_eq!((m.version.as_deref(), m.applied), (Some("1"), 1));
    }

    #[test]
    fn flyway_file_names_carry_versions() {
        assert_eq!(flyway_file_version("V1_2__add.sql").as_deref(), Some("1.2"));
        assert_eq!(
            flyway_file_version("V20240101__x.SQL").as_deref(),
            Some("20240101")
        );
        assert_eq!(flyway_file_version("R__views.sql"), None);
        assert_eq!(flyway_file_version("V__x.sql"), None);
        assert_eq!(flyway_file_version("V1__x.txt"), None);
    }

    #[test]
    fn the_other_tools_name_their_current_version() {
        let lq = parse(
            Tool::Liquibase,
            &rows(&[&[Some("1"), Some("a")], &[Some("add-users"), Some("tom")]]),
            None,
        );
        assert_eq!((lq.version.as_deref(), lq.applied), (Some("add-users"), 2));

        let dj = parse(
            Tool::Django,
            &rows(&[
                &[Some("auth"), Some("0001_initial")],
                &[Some("shop"), Some("0007_orders")],
            ]),
            None,
        );
        assert_eq!(dj.version.as_deref(), Some("shop.0007_orders"));

        let al = parse(Tool::Alembic, &rows(&[&[Some("ae1027a6acf")]]), None);
        assert_eq!(al.version.as_deref(), Some("ae1027a6acf"));

        let rails = parse(
            Tool::SchemaMigrations,
            &rows(&[
                &[Some("20240101120000")],
                &[Some("20231231000000")],
                &[Some("9")],
            ]),
            None,
        );
        assert_eq!(rails.version.as_deref(), Some("20240101120000"));

        let prisma = parse(
            Tool::Prisma,
            &rows(&[
                &[Some("20240101_init"), Some("true"), Some("false")],
                &[Some("20240201_bad"), Some("false"), Some("true")],
                &[Some("20240301_users"), Some("true"), Some("false")],
                &[Some("20240401_half"), Some("false"), Some("false")],
            ]),
            None,
        );
        assert_eq!(prisma.version.as_deref(), Some("20240301_users"));
        assert_eq!(
            prisma.failed.as_deref(),
            Some("20240401_half"),
            "rolled back isn't failed"
        );
        assert_eq!(prisma.applied, 2);

        let sqlx = parse(
            Tool::Sqlx,
            &rows(&[
                &[Some("20240101"), Some("t")],
                &[Some("20240202"), Some("f")],
            ]),
            None,
        );
        assert_eq!(
            (sqlx.version.as_deref(), sqlx.failed.as_deref()),
            (Some("20240101"), Some("20240202"))
        );
    }
}
