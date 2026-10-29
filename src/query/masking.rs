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

fn tables_sql() -> String {
    format!(
        "SELECT c.oid::int8, n.nspname::text, c.relname::text, c.relkind::text, \
                CASE WHEN c.relispartition THEN pg_catalog.pg_partition_root(c.oid)::int8 END \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('r', 'p', 'f', 'm') AND {USER_RELATIONS} \
         ORDER BY 2, 3"
    )
}

fn columns_sql() -> String {
    format!(
        "SELECT a.attrelid::int8, a.attnum, a.attname::text, a.atttypid::int8, a.atttypmod, \
                a.attnotnull, a.attgenerated::text, \
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

/// Primary and foreign keys from pg_constraint; every unique index —
/// expression and partial ones too — by the columns it depends on.
const KEYS_SQL: &str = "\
    SELECT conrelid::int8, contype::text, conkey, confrelid::int8, confkey \
    FROM pg_catalog.pg_constraint WHERE contype IN ('p', 'f') \
    UNION ALL \
    SELECT i.indrelid::int8, 'u', array_agg(DISTINCT d.refobjsubid)::int2[], NULL::int8, NULL::int2[] \
    FROM pg_catalog.pg_index i \
    JOIN pg_catalog.pg_depend d ON d.classid = 'pg_catalog.pg_class'::regclass \
         AND d.objid = i.indexrelid AND d.refobjid = i.indrelid AND d.refobjsubid > 0 \
    WHERE i.indisunique GROUP BY i.indexrelid, i.indrelid";

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
        })
        .collect();
    let columns: Vec<RawColumn> = client
        .query(&columns_sql(), &[])
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
