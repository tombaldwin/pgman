//! The database side of masked dumps: reading the catalog masking works
//! from ([`crate::masking::catalog`]), holding the one snapshot a masked
//! dump reads everything in, and moving rows with `COPY` — out of the
//! source, and into a restored copy. See `docs/design/masked-dumps.md`,
//! section 4.

use crate::masking::catalog::{self, Catalog, RawColumn, RawKey, RawTable, RawType};

/// The schemas never dumped: the system's, and temporary ones.
const USER_RELATIONS: &str = "\
    n.nspname NOT IN ('pg_catalog', 'information_schema') \
    AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp\\_%' \
    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d \
        WHERE d.classid = 'pg_catalog.pg_class'::regclass AND d.objid = c.oid AND d.deptype = 'e')";

/// A partition's root is its topmost ancestor that isn't itself a
/// partition, walked up pg_inherits (`pg_partition_root` would say the
/// same, from PostgreSQL 12 only).
fn tables_sql() -> String {
    format!(
        "SELECT c.oid::int8, n.nspname::text, c.relname::text, c.relkind::text, \
                CASE WHEN c.relispartition THEN ( \
                  WITH RECURSIVE up(oid) AS ( \
                    SELECT i.inhparent FROM pg_catalog.pg_inherits i WHERE i.inhrelid = c.oid \
                    UNION SELECT i.inhparent FROM pg_catalog.pg_inherits i JOIN up ON i.inhrelid = up.oid) \
                  SELECT u.oid::int8 FROM up u JOIN pg_catalog.pg_class p ON p.oid = u.oid \
                  WHERE NOT p.relispartition LIMIT 1) END, \
                (SELECT i.inhparent::int8 FROM pg_catalog.pg_inherits i \
                  WHERE i.inhrelid = c.oid ORDER BY i.inhseqno LIMIT 1) \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('r', 'p', 'f', 'm') AND {USER_RELATIONS} \
         ORDER BY 2, 3"
    )
}

/// `attgenerated` is PostgreSQL 12's: before it (`version_num` under
/// 120000) there are no generated columns, and the column isn't there
/// to read.
fn columns_sql(version_num: i64) -> String {
    let generated = if version_num >= 120000 {
        "a.attgenerated::text"
    } else {
        "''::text"
    };
    format!(
        "SELECT a.attrelid::int8, a.attnum, a.attname::text, a.atttypid::int8, a.atttypmod, \
                a.attnotnull, {generated}, \
                pg_catalog.format_type(a.atttypid, a.atttypmod) \
         FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE a.attnum > 0 AND NOT a.attisdropped AND c.relkind IN ('r', 'p', 'f', 'm') \
           AND {USER_RELATIONS}"
    )
}

const TYPES_SQL: &str = "\
    SELECT t.oid::int8, t.typname::text, n.nspname::text, t.typtype::text, t.typcategory::text, \
           t.typbasetype::int8, t.typtypmod, t.typnotnull, t.typelem::int8, \
           (SELECT e.enumlabel::text FROM pg_catalog.pg_enum e WHERE e.enumtypid = t.oid \
             ORDER BY e.enumsortorder LIMIT 1) \
    FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
    WHERE t.typtype <> 'p'";

/// Primary and foreign keys from pg_constraint; every other unique
/// index — a UNIQUE constraint's, expression and partial ones too — by
/// its key columns and the columns its expressions depend on. (A
/// constraint's index depends on the constraint, not on its columns, so
/// pg_depend alone misses it.)
const KEYS_SQL: &str = "\
    SELECT conrelid::int8, contype::text, conkey, confrelid::int8, confkey \
    FROM pg_catalog.pg_constraint WHERE contype IN ('p', 'f') \
    UNION ALL \
    SELECT i.indrelid::int8, 'u', \
           ARRAY(SELECT k FROM unnest(i.indkey::int2[]) k WHERE k > 0 \
                 UNION SELECT d.refobjsubid::int2 FROM pg_catalog.pg_depend d \
                 WHERE d.classid = 'pg_catalog.pg_class'::regclass AND d.objid = i.indexrelid \
                   AND d.refobjid = i.indrelid AND d.refobjsubid > 0 ORDER BY 1)::int2[], \
           NULL::int8, NULL::int2[] \
    FROM pg_catalog.pg_index i WHERE i.indisunique AND NOT i.indisprimary";

const PARTITION_KEYS_SQL: &str =
    "SELECT partrelid::int8, partattrs::int2[] FROM pg_catalog.pg_partitioned_table";

const LARGE_OBJECTS_SQL: &str = "SELECT count(*) FROM pg_catalog.pg_largeobject_metadata";

fn message(e: &tokio_postgres::Error) -> String {
    e.as_db_error()
        .map(|d| d.message().to_string())
        .unwrap_or_else(|| e.to_string())
}

/// The catalog masking works from. Reads no rows.
pub async fn catalog(client: &tokio_postgres::Client) -> Result<Catalog, String> {
    let fail = |what: &str, e: tokio_postgres::Error| {
        format!("could not read the catalog's {what}: {}", message(&e))
    };
    let tables: Vec<RawTable> = client
        .query(&tables_sql(), &[])
        .await
        .map_err(|e| fail("tables", e))?
        .iter()
        .map(|r| RawTable {
            oid: r.get(0),
            schema: r.get(1),
            name: r.get(2),
            kind: r.get(3),
            root: r.get(4),
            parent: r.get(5),
        })
        .collect();
    let version_num: i64 = client
        .query_one(
            "SELECT pg_catalog.current_setting('server_version_num')::int8",
            &[],
        )
        .await
        .map_err(|e| fail("version", e))?
        .get(0);
    let columns: Vec<RawColumn> = client
        .query(&columns_sql(version_num), &[])
        .await
        .map_err(|e| fail("columns", e))?
        .iter()
        .map(|r| RawColumn {
            table: r.get(0),
            num: r.get(1),
            name: r.get(2),
            type_oid: r.get(3),
            typmod: r.get(4),
            not_null: r.get(5),
            generated: r.get(6),
            display: r.get(7),
        })
        .collect();
    let types: Vec<RawType> = client
        .query(TYPES_SQL, &[])
        .await
        .map_err(|e| fail("types", e))?
        .iter()
        .map(|r| RawType {
            oid: r.get(0),
            name: r.get(1),
            schema: r.get(2),
            kind: r.get(3),
            category: r.get(4),
            base: r.get(5),
            typmod: r.get(6),
            not_null: r.get(7),
            elem: r.get(8),
            first_label: r.get(9),
        })
        .collect();
    let keys: Vec<RawKey> = client
        .query(KEYS_SQL, &[])
        .await
        .map_err(|e| fail("keys", e))?
        .iter()
        .map(|r| RawKey {
            table: r.get(0),
            kind: r.get(1),
            columns: r.get::<_, Option<Vec<i16>>>(2).unwrap_or_default(),
            ref_table: r.get(3),
            ref_columns: r.get::<_, Option<Vec<i16>>>(4).unwrap_or_default(),
        })
        .collect();
    let partition_keys: Vec<(i64, Vec<i16>)> = client
        .query(PARTITION_KEYS_SQL, &[])
        .await
        .map_err(|e| fail("partition keys", e))?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let large_objects: i64 = client
        .query_one(LARGE_OBJECTS_SQL, &[])
        .await
        .map_err(|e| fail("large objects", e))?
        .get(0);
    Ok(catalog::build(
        &tables,
        &columns,
        &types,
        &keys,
        &partition_keys,
        large_objects,
    ))
}

/// Start the one transaction a masked dump reads in, and export its
/// snapshot for pg_dump: repeatable read and read-only, no timeouts (a
/// dump takes as long as it takes; RDS parameter groups often set
/// them), and row security off, so a table the user may see only part
/// of fails loudly rather than being dumped in part. Works on a standby.
pub async fn begin_snapshot(client: &tokio_postgres::Client) -> Result<String, String> {
    client
        .batch_execute(
            "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; \
             SET LOCAL statement_timeout = 0; SET LOCAL lock_timeout = 0; \
             SET LOCAL idle_in_transaction_session_timeout = 0; SET LOCAL row_security = off",
        )
        .await
        .map_err(|e| format!("could not start the dump's transaction: {}", message(&e)))?;
    client
        .query_one("SELECT pg_catalog.pg_export_snapshot()", &[])
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("could not export the dump's snapshot: {}", message(&e)))
}

/// `"schema"."name"`, quoted. Pure.
pub fn quoted(schema: &str, name: &str) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    format!("{}.{}", q(schema), q(name))
}

/// Lock `tables` as pg_dump does (ACCESS SHARE), so a table altered or
/// dropped mid-dump fails it rather than leaving schema and rows apart.
pub async fn lock(
    client: &tokio_postgres::Client,
    tables: &[(String, String)],
) -> Result<(), String> {
    for chunk in tables.chunks(200) {
        let list: Vec<String> = chunk.iter().map(|(s, n)| quoted(s, n)).collect();
        client
            .batch_execute(&format!(
                "LOCK TABLE ONLY {} IN ACCESS SHARE MODE",
                list.join(", ")
            ))
            .await
            .map_err(|e| format!("could not lock the tables to dump: {}", message(&e)))?;
    }
    Ok(())
}

/// The `COPY … TO STDOUT` that reads `columns` of a table's own rows —
/// `NULL` in place of each `nulled` one, so its values never leave the
/// server. Pure.
pub fn copy_out_sql(schema: &str, name: &str, columns: &[(String, bool)]) -> String {
    let list: Vec<String> = columns
        .iter()
        .map(|(c, nulled)| {
            if *nulled {
                "NULL".to_string()
            } else {
                format!("\"{}\"", c.replace('"', "\"\""))
            }
        })
        .collect();
    format!(
        "COPY (SELECT {} FROM ONLY {}) TO STDOUT",
        if list.is_empty() {
            "NULL".to_string()
        } else {
            list.join(", ")
        },
        quoted(schema, name)
    )
}

/// Rows out, in COPY's text format: a stream of chunks.
pub async fn copy_out(
    client: &tokio_postgres::Client,
    sql: &str,
) -> Result<tokio_postgres::CopyOutStream, String> {
    client
        .copy_out(sql)
        .await
        .map_err(|e| format!("could not read the rows: {}", message(&e)))
}

/// End the dump's transaction (it only read).
pub async fn end(client: &tokio_postgres::Client) -> Result<(), String> {
    client
        .batch_execute("COMMIT")
        .await
        .map_err(|e| format!("could not end the dump's transaction: {}", message(&e)))
}

/// `COPY … FROM STDIN` into `columns` of a restored table. Pure.
pub fn copy_in_sql(table: &str, columns: &[String]) -> String {
    let (schema, name) = table.split_once('.').unwrap_or(("public", table));
    let list: Vec<String> = columns
        .iter()
        .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
        .collect();
    format!(
        "COPY {} ({}) FROM STDIN",
        quoted(schema, name),
        list.join(", ")
    )
}

/// Load rows (COPY text, as `chunks`, read as they are needed) into a
/// restored table with `sql` ([`copy_in_sql`]); how many rows the server
/// took. A chunk that is an error ends it with that error, and the COPY
/// is abandoned.
pub async fn copy_in(
    client: &tokio_postgres::Client,
    sql: &str,
    chunks: impl Iterator<Item = Result<Vec<u8>, String>>,
) -> Result<u64, String> {
    use futures::SinkExt;
    let sink = client
        .copy_in::<_, std::io::Cursor<Vec<u8>>>(sql)
        .await
        .map_err(|e| format!("could not load the rows: {}", message(&e)))?;
    futures::pin_mut!(sink);
    for c in chunks {
        sink.send(std::io::Cursor::new(c?))
            .await
            .map_err(|e| format!("could not load the rows: {}", message(&e)))?;
    }
    sink.finish()
        .await
        .map_err(|e| format!("could not load the rows: {}", message(&e)))
}

/// Record where a restored copy came from, inside it: a `pgman.provenance`
/// row (its own schema, out of the application's sight and out of a
/// Flyway clean of `public`) and the database's comment, which survives
/// anything short of dropping it.
pub async fn write_provenance(
    client: &tokio_postgres::Client,
    database: &str,
    copy: &str,
    masked: bool,
    protected: bool,
    source: &str,
    expires: Option<u64>,
) -> Result<(), String> {
    let fail =
        |e: tokio_postgres::Error| format!("could not record the copy's origin: {}", message(&e));
    client
        .batch_execute(
            "CREATE SCHEMA IF NOT EXISTS pgman; \
             CREATE TABLE IF NOT EXISTS pgman.provenance (\
               copy text NOT NULL, masked boolean NOT NULL, protected boolean NOT NULL, \
               source text NOT NULL, \
               restored timestamptz NOT NULL DEFAULT now(), expires timestamptz, \
               pgman text NOT NULL)",
        )
        .await
        .map_err(fail)?;
    let expires = expires.map(|e| e as f64);
    client
        .execute(
            "INSERT INTO pgman.provenance (copy, masked, protected, source, expires, pgman) \
             VALUES ($1, $2, $3, $4, to_timestamp($5), $6)",
            &[
                &copy,
                &masked,
                &protected,
                &source,
                &expires,
                &env!("CARGO_PKG_VERSION"),
            ],
        )
        .await
        .map_err(fail)?;
    write_comment(client, database, copy, masked, protected, source).await
}

/// The database's comment naming the copy it was restored from — what
/// an MCP tool reads to refuse a raw copy. Written as soon as the
/// database exists, so a restore that fails part-way leaves it marked.
/// `client` may be connected to any database on the server.
pub async fn write_comment(
    client: &tokio_postgres::Client,
    database: &str,
    copy: &str,
    masked: bool,
    protected: bool,
    source: &str,
) -> Result<(), String> {
    let comment = format!(
        "pgman copy {copy} ({}{}, from {source})",
        if masked { "masked" } else { "raw" },
        if protected { ", protected" } else { "" }
    );
    client
        .batch_execute(&format!(
            "COMMENT ON DATABASE \"{}\" IS '{}'",
            database.replace('"', "\"\""),
            comment.replace('\'', "''")
        ))
        .await
        .map_err(|e| format!("could not record the copy's origin: {}", message(&e)))
}

/// Every sequence's state, in the dump's snapshot: pg_dump's data section
/// would carry it, and the no-rows archive leaves it out with the rows.
pub async fn sequences(
    client: &tokio_postgres::Client,
) -> Result<Vec<(String, String, i64, bool)>, String> {
    let names = client
        .query(
            &format!(
                "SELECT n.nspname::text, c.relname::text \
                 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relkind = 'S' AND {USER_RELATIONS} \
                   AND CASE WHEN c.relkind = 'S' THEN pg_catalog.has_sequence_privilege(c.oid, 'SELECT') END \
                 ORDER BY 1, 2"
            ),
            &[],
        )
        .await
        .map_err(|e| format!("could not list the sequences: {}", message(&e)))?;
    let mut out = Vec::new();
    for r in names {
        let (schema, name): (String, String) = (r.get(0), r.get(1));
        let row = client
            .query_one(
                &format!(
                    "SELECT last_value, is_called FROM {}",
                    quoted(&schema, &name)
                ),
                &[],
            )
            .await
            .map_err(|e| format!("could not read sequence {schema}.{name}: {}", message(&e)))?;
        out.push((schema, name, row.get(0), row.get(1)));
    }
    Ok(out)
}

/// Each sequence a table owns (a serial's, an identity's): the
/// sequence's schema and name, the table's schema and name. pg_dump
/// leaves it out with its table.
pub async fn sequence_owners(
    client: &tokio_postgres::Client,
) -> Result<Vec<(String, String, String, String)>, String> {
    Ok(client
        .query(
            "SELECT sn.nspname::text, s.relname::text, tn.nspname::text, t.relname::text \
             FROM pg_catalog.pg_depend d \
             JOIN pg_catalog.pg_class s ON s.oid = d.objid AND s.relkind = 'S' \
             JOIN pg_catalog.pg_namespace sn ON sn.oid = s.relnamespace \
             JOIN pg_catalog.pg_class t ON t.oid = d.refobjid \
             JOIN pg_catalog.pg_namespace tn ON tn.oid = t.relnamespace \
             WHERE d.classid = 'pg_catalog.pg_class'::regclass \
               AND d.refclassid = 'pg_catalog.pg_class'::regclass AND d.deptype IN ('a', 'i')",
            &[],
        )
        .await
        .map_err(|e| format!("could not read which tables own sequences: {}", message(&e)))?
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect())
}

/// Set a restored sequence as it was.
pub async fn set_sequence(
    client: &tokio_postgres::Client,
    schema: &str,
    name: &str,
    value: i64,
    called: bool,
) -> Result<(), String> {
    client
        .execute(
            "SELECT pg_catalog.setval($1::text::regclass, $2, $3)",
            &[&quoted(schema, name), &value, &called],
        )
        .await
        .map(|_| ())
        .map_err(|e| format!("could not set sequence {schema}.{name}: {}", message(&e)))
}

/// A database's comment, if it has one and it can be read.
pub async fn comment(client: &tokio_postgres::Client, database: &str) -> Option<String> {
    client
        .query_opt(
            "SELECT pg_catalog.shobj_description(d.oid, 'pg_database') \
             FROM pg_catalog.pg_database d WHERE d.datname = $1",
            &[&database],
        )
        .await
        .ok()
        .flatten()
        .and_then(|r| r.get(0))
}

/// The copy id a database's comment names, if pgman wrote it.
pub async fn provenance_copy(
    client: &tokio_postgres::Client,
    database: &str,
) -> Result<Option<String>, String> {
    let row = client
        .query_opt(
            "SELECT pg_catalog.shobj_description(d.oid, 'pg_database') \
             FROM pg_catalog.pg_database d WHERE d.datname = $1",
            &[&database],
        )
        .await
        .map_err(|e| format!("could not read {database}'s comment: {}", message(&e)))?;
    let comment: Option<String> = row.and_then(|r| r.get(0));
    Ok(comment.as_deref().and_then(parse_comment))
}

/// The copy id in a comment [`write_provenance`] wrote. Pure.
pub fn parse_comment(comment: &str) -> Option<String> {
    let rest = comment.strip_prefix("pgman copy ")?;
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect();
    (id.len() == 36).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_reads_nothing_older_servers_lack() {
        // PostgreSQL 10 and 11 have neither; masking failed there with
        // "function pg_catalog.pg_partition_root(oid) does not exist".
        for sql in [tables_sql(), columns_sql(110000)] {
            assert!(!sql.contains("pg_partition_root"), "{sql}");
            assert!(!sql.contains("attgenerated"), "{sql}");
        }
        assert!(columns_sql(120000).contains("a.attgenerated::text"));
    }

    #[test]
    fn copy_sql_quotes_and_nulls() {
        assert_eq!(
            copy_out_sql(
                "public",
                "cust\"omers",
                &[("id".into(), false), ("email".into(), true)]
            ),
            "COPY (SELECT \"id\", NULL FROM ONLY \"public\".\"cust\"\"omers\") TO STDOUT"
        );
        assert_eq!(
            copy_in_sql("app.orders", &["id".into(), "a\"b".into()]),
            "COPY \"app\".\"orders\" (\"id\", \"a\"\"b\") FROM STDIN"
        );
    }

    #[test]
    fn a_provenance_comment_names_its_copy() {
        let id = "3f9a1c2e-0000-4000-8000-000000000001";
        assert_eq!(
            parse_comment(&format!("pgman copy {id} (masked, from production/ledger)")).as_deref(),
            Some(id)
        );
        assert_eq!(parse_comment("pgman copy 3f9a (raw)"), None);
        assert_eq!(parse_comment("someone's own comment"), None);
    }
}
