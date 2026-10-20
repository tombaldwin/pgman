//! SQL for pre-flight checks before a schema change: who holds a
//! transaction open, who is blocked, and who holds locks on the tables a
//! change touches. Pure builders; the MCP `sessions_locks` tool runs the
//! result on a read-only connection.
//!
//! Table names come from the client, and the statement is built as text
//! (the typed runner has no bind parameters), so every name is checked
//! against [`plain_ident_path`] before it reaches the SQL — anything that
//! is not a plain identifier is refused rather than escaped.

/// Default for "a transaction open this long is worth reporting".
pub const DEFAULT_MIN_XACT_AGE_SECS: u64 = 60;

/// Most table names one call may scope to.
pub const MAX_TABLES: usize = 50;

/// Accept `name` or `schema.name`, each part a plain unquoted identifier
/// (`[A-Za-z_][A-Za-z0-9_$]*`, at most 63 bytes). Returns the parts
/// lowercased — which is what Postgres folds an unquoted name to, and so
/// what the catalogue holds for any table created without quotes.
pub fn plain_ident_path(name: &str) -> Result<(Option<String>, String), String> {
    let refuse = || {
        format!(
            "{name:?} is not a plain table name; use name or schema.name with letters, \
             digits, _ and $ only"
        )
    };
    let part_ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 63
            && p.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    };
    let parts: Vec<&str> = name.split('.').collect();
    match parts.as_slice() {
        [t] if part_ok(t) => Ok((None, t.to_ascii_lowercase())),
        [s, t] if part_ok(s) && part_ok(t) => {
            Ok((Some(s.to_ascii_lowercase()), t.to_ascii_lowercase()))
        }
        _ => Err(refuse()),
    }
}

/// The `sessions_locks` statement. A client backend is reported when it
/// has had a transaction open for at least `min_xact_age_secs`, is
/// blocked, is blocking someone, or holds or waits for a lock on one of
/// `tables`. Each row lists the locks it has on `tables` (all relation
/// locks when `tables` is empty), with ` (waiting)` on the ungranted ones.
pub fn sessions_locks_sql(min_xact_age_secs: u64, tables: &[String]) -> Result<String, String> {
    if tables.len() > MAX_TABLES {
        return Err(format!("at most {MAX_TABLES} tables per call"));
    }
    let mut conds: Vec<String> = Vec::new();
    for t in tables {
        let (schema, table) = plain_ident_path(t)?;
        // Validated above: letters, digits, `_` and `$` only, so a
        // single-quoted literal cannot be broken out of.
        conds.push(match schema {
            Some(s) => format!("(n.nspname = '{s}' AND c.relname = '{table}')"),
            None => format!("c.relname = '{table}'"),
        });
    }
    let on_tables = if conds.is_empty() {
        "TRUE".to_string()
    } else {
        format!("({})", conds.join(" OR "))
    };
    let touches = if conds.is_empty() {
        "FALSE".to_string()
    } else {
        "lk.locks IS NOT NULL".to_string()
    };
    Ok(format!(
        "/* pgman:sessions_locks */ \
SELECT a.pid, \
  a.usename AS username, \
  a.application_name AS application, \
  a.client_addr::text AS client_addr, \
  a.state, \
  NULLIF(COALESCE(a.wait_event_type, '') || ':' || COALESCE(a.wait_event, ''), ':') AS wait_event, \
  EXTRACT(EPOCH FROM now() - a.xact_start)::bigint AS xact_age_secs, \
  EXTRACT(EPOCH FROM now() - a.query_start)::bigint AS query_age_secs, \
  NULLIF(array_to_string(pg_blocking_pids(a.pid), ','), '') AS blocked_by, \
  (SELECT NULLIF(string_agg(b.pid::text, ','), '') FROM pg_stat_activity b \
     WHERE a.pid = ANY(pg_blocking_pids(b.pid))) AS blocking, \
  lk.locks AS locks_on_tables, \
  left(a.query, 500) AS query \
FROM pg_stat_activity a \
LEFT JOIN LATERAL ( \
  SELECT string_agg(DISTINCT n.nspname || '.' || c.relname || ' ' || l.mode \
           || CASE WHEN l.granted THEN '' ELSE ' (waiting)' END, ', ') AS locks \
  FROM pg_locks l \
  JOIN pg_class c ON c.oid = l.relation \
  JOIN pg_namespace n ON n.oid = c.relnamespace \
  WHERE l.pid = a.pid AND {on_tables} \
) lk ON TRUE \
WHERE a.backend_type = 'client backend' \
  AND a.pid <> pg_backend_pid() \
  AND ( \
    (a.xact_start IS NOT NULL AND now() - a.xact_start >= interval '{min_xact_age_secs} seconds') \
    OR cardinality(pg_blocking_pids(a.pid)) > 0 \
    OR EXISTS (SELECT 1 FROM pg_stat_activity b WHERE a.pid = ANY(pg_blocking_pids(b.pid))) \
    OR {touches} \
  ) \
ORDER BY a.xact_start NULLS LAST"
    ))
}

/// Whether the server is a standby. A replica's sessions and locks are
/// the replica's own, not the primary's — the caller says so.
pub const IS_REPLICA_SQL: &str = "SELECT pg_is_in_recovery() AS is_replica";

/// Whether the server is a standby, and its lag (`lag_secs_sql!`).
pub const REPLICA_LAG_SQL: &str = concat!(
    "SELECT pg_is_in_recovery() AS in_recovery, ",
    super::probe::lag_secs_sql!(),
    " AS lag_secs"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_are_lowercased_and_anything_else_refused() {
        assert_eq!(plain_ident_path("t_User"), Ok((None, "t_user".to_string())));
        assert_eq!(
            plain_ident_path("Public.t_user"),
            Ok((Some("public".to_string()), "t_user".to_string()))
        );
        assert_eq!(plain_ident_path("a$b"), Ok((None, "a$b".to_string())));
        for bad in [
            "",
            ".t",
            "t.",
            "a.b.c",
            "1t",
            "t'; DROP TABLE x; --",
            "\"Quoted\"",
            "t u",
            "t\\",
            &"x".repeat(64),
        ] {
            assert!(plain_ident_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_bad_table_name_refuses_the_whole_statement() {
        let err = sessions_locks_sql(60, &["t_user".into(), "x' OR '1'='1".into()]).unwrap_err();
        assert!(err.contains("not a plain table name"), "{err}");
        let many: Vec<String> = (0..=MAX_TABLES).map(|i| format!("t{i}")).collect();
        assert!(sessions_locks_sql(60, &many).is_err());
    }

    #[test]
    fn tables_scope_the_lock_list_and_widen_the_filter() {
        let sql = sessions_locks_sql(30, &["t_user".into(), "audit.t_log".into()]).unwrap();
        assert!(sql.contains("c.relname = 't_user'"), "{sql}");
        assert!(
            sql.contains("(n.nspname = 'audit' AND c.relname = 't_log')"),
            "{sql}"
        );
        assert!(sql.contains("OR lk.locks IS NOT NULL"), "{sql}");
        assert!(sql.contains("interval '30 seconds'"), "{sql}");

        let sql = sessions_locks_sql(60, &[]).unwrap();
        assert!(sql.contains("WHERE l.pid = a.pid AND TRUE"), "{sql}");
        assert!(sql.contains("OR FALSE"), "{sql}");
    }
}
