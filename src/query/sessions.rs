//! Active-sessions + locks inventory from `pg_stat_activity`
//! joined with `pg_blocking_pids()`. Same shape as
//! [`crate::query::slow_queries`]: pure parsing here; the catalog
//! query dispatch and rendering are App-side.

use crate::grid::Grid;

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub pid: i32,
    pub user: String,
    pub application: String,
    pub state: String,
    pub wait_event: Option<String>,
    /// Comma-joined PIDs (Postgres returns an array) of backends
    /// holding a lock this session is blocked on. Empty when this
    /// session isn't blocked.
    pub blocked_by: String,
    /// Most recent query the session was running. Truncated to a
    /// reasonable display width by the renderer; carried full here
    /// in case the operator copies it.
    pub query: String,
    /// Seconds since the query started. Useful for spotting runaway
    /// statements at a glance.
    pub age_secs: f64,
}

impl SessionRow {
    pub fn is_blocked(&self) -> bool {
        !self.blocked_by.is_empty() && self.blocked_by != "{}"
    }
}

/// The SQL we issue. The `pg_blocking_pids` call returns an array;
/// `array_to_string` flattens it so the Grid carries a single
/// string per row. We exclude our own PID via `pg_backend_pid()` so
/// the panel doesn't list itself.
pub const PANEL_SQL: &str = "/* pgman:sessions */ \
SELECT \
  pid, \
  COALESCE(usename, '') AS usename, \
  COALESCE(application_name, '') AS application_name, \
  COALESCE(state, '') AS state, \
  COALESCE(wait_event_type || ':' || wait_event, '') AS wait_event, \
  COALESCE(array_to_string(pg_blocking_pids(pid), ','), '') AS blocked_by, \
  COALESCE(query, '') AS query, \
  COALESCE(EXTRACT(EPOCH FROM (now() - query_start))::float8, 0)::float8 AS age_secs \
FROM pg_stat_activity \
WHERE pid <> pg_backend_pid() \
  AND backend_type = 'client backend' \
ORDER BY \
  (CASE WHEN COALESCE(array_length(pg_blocking_pids(pid), 1), 0) > 0 THEN 0 ELSE 1 END), \
  query_start NULLS LAST";

pub fn parse(grid: &Grid) -> Vec<SessionRow> {
    let idx = |name: &str| -> Option<usize> {
        grid.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
    };
    let pid_idx = idx("pid");
    let user_idx = idx("usename");
    let app_idx = idx("application_name");
    let state_idx = idx("state");
    let wait_idx = idx("wait_event");
    let bb_idx = idx("blocked_by");
    let q_idx = idx("query");
    let age_idx = idx("age_secs");
    grid.rows
        .iter()
        .map(|r| {
            let pid = pid_idx
                .and_then(|i| r.get(i))
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            let user = user_idx.and_then(|i| r.get(i).cloned()).unwrap_or_default();
            let application = app_idx.and_then(|i| r.get(i).cloned()).unwrap_or_default();
            let state = state_idx
                .and_then(|i| r.get(i).cloned())
                .unwrap_or_default();
            // `:` from the COALESCE join — strip when both halves
            // are empty so we don't render `:` for "no wait event".
            let wait_event_raw = wait_idx.and_then(|i| r.get(i).cloned()).unwrap_or_default();
            let wait_event = if wait_event_raw == ":" || wait_event_raw.is_empty() {
                None
            } else {
                Some(wait_event_raw)
            };
            let blocked_by = bb_idx.and_then(|i| r.get(i).cloned()).unwrap_or_default();
            let query = q_idx.and_then(|i| r.get(i).cloned()).unwrap_or_default();
            let age_secs = age_idx
                .and_then(|i| r.get(i))
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            SessionRow {
                pid,
                user,
                application,
                state,
                wait_event,
                blocked_by,
                query,
                age_secs,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_grid(rows: &[&[&str]]) -> Grid {
        Grid {
            columns: vec![
                "pid".into(),
                "usename".into(),
                "application_name".into(),
                "state".into(),
                "wait_event".into(),
                "blocked_by".into(),
                "query".into(),
                "age_secs".into(),
            ],
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| (*s).to_string()).collect())
                .collect(),
            truncated: false,
        }
    }

    #[test]
    fn parse_typed_session_rows() {
        let g = make_grid(&[
            &[
                "1234",
                "alice",
                "psql",
                "active",
                "Lock:transactionid",
                "5678",
                "UPDATE accounts SET balance = 0",
                "12.5",
            ],
            &[
                "5678",
                "bob",
                "pgman",
                "idle in transaction",
                "",
                "",
                "BEGIN",
                "300.0",
            ],
        ]);
        let parsed = parse(&g);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].pid, 1234);
        assert_eq!(parsed[0].user, "alice");
        assert_eq!(parsed[0].state, "active");
        assert_eq!(parsed[0].wait_event.as_deref(), Some("Lock:transactionid"));
        assert_eq!(parsed[0].blocked_by, "5678");
        assert!(parsed[0].is_blocked());
        assert!((parsed[0].age_secs - 12.5).abs() < 1e-9);

        assert_eq!(parsed[1].pid, 5678);
        assert!(parsed[1].wait_event.is_none());
        assert_eq!(parsed[1].blocked_by, "");
        assert!(!parsed[1].is_blocked());
    }

    #[test]
    fn wait_event_stripped_when_only_separator() {
        let g = make_grid(&[&["1", "x", "x", "x", ":", "", "SELECT 1", "0"]]);
        let parsed = parse(&g);
        assert!(parsed[0].wait_event.is_none());
    }

    #[test]
    fn parse_handles_empty_grid() {
        let g = Grid::default();
        assert!(parse(&g).is_empty());
    }
}
