//! Paging past `grid::MAX_ROWS` with a server-side cursor
//! (docs/backlog/paging.md).
//!
//! A plain `SELECT` from the editor runs as `DECLARE pgman_page NO
//! SCROLL CURSOR FOR …` inside a transaction pgman opens, and `M` in the
//! grid sends `FETCH FORWARD n` for the next page. The cursor's
//! transaction is the session's, so every other piece of database work
//! must close it first, in order: the [`Pager`] task owns the cursor and
//! reads requests from one queue, and [`Db`] — the app's only way to the
//! client — holds a place in that queue before its task is spawned.
//!
//! The SQL builders and the page arithmetic are pure and tested here;
//! the database round trips are pinned by `tests/integration.rs`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::conn::QueryErr;
use crate::grid::{Cell, Grid};

/// Rows one `M` fetches — the size of the first page, too.
pub const PAGE_ROWS: usize = crate::grid::MAX_ROWS;

/// The most rows pgman holds for one result. At the ceiling the cursor
/// is closed; `pgman query … --json` is the way to every row.
pub const MAX_HELD_ROWS: usize = 100_000;

/// The one cursor a session can have open. A fixed name: the pager
/// closes the old cursor before it declares a new one.
pub const CURSOR_NAME: &str = "pgman_page";

/// How long an open cursor may sit unfetched before pgman closes it,
/// unless the server's `idle_in_transaction_session_timeout` asks for
/// less (see [`close_limits`]).
pub const IDLE_CLOSE: Duration = Duration::from_secs(60);

/// How long a cursor may stay open at all, fetched or not — an open
/// transaction holds back vacuum — unless the server's
/// `transaction_timeout` asks for less.
pub const LIFETIME_CLOSE: Duration = Duration::from_secs(10 * 60);

/// The shortest close deadline pgman sets, however small the server's
/// timeouts: the main loop's clock is no finer than this.
const MIN_CLOSE: Duration = Duration::from_millis(500);

/// `true` exactly when it runs as the first command of its transaction
/// — which, sent on its own, means the session was not inside one.
/// Postgres documents `statement_timestamp()` and
/// `transaction_timestamp()` as equal during the first command of a
/// transaction.
const IDLE_PROBE_SQL: &str = "SELECT statement_timestamp() = transaction_timestamp()";

/// The two server settings that would end the session under an open
/// cursor, in milliseconds (`pg_settings.setting` is in the GUC's base
/// unit). `transaction_timeout` is new in Postgres 17; before that the
/// subquery is NULL.
const SETTINGS_SQL: &str = "SELECT \
(SELECT setting FROM pg_settings WHERE name = 'idle_in_transaction_session_timeout'), \
(SELECT setting FROM pg_settings WHERE name = 'transaction_timeout')";

/// The statement indices of [`open_batch`]'s pieces, as counted by
/// `CommandComplete`s: `BEGIN`, the settings read, `DECLARE`, `FETCH`.
const SETTINGS_STMT: usize = 1;
const FETCH_STMT: usize = 3;

/// The one round trip that opens a cursor: begin, read the timeouts,
/// declare, fetch `n` rows. Returns the batch and the character offset
/// of `sql` inside it, to map an error position back to the editor.
///
/// `sql` is one statement `safety::split_verified` vouched for, so it
/// adds no statement of its own; the newline before its `;` ends a
/// trailing `-- comment` (the same reason as `conn::wrap_in_transaction`).
/// Pure.
pub fn open_batch(sql: &str, n: usize) -> (String, usize) {
    let head = format!("BEGIN;\n{SETTINGS_SQL};\nDECLARE {CURSOR_NAME} NO SCROLL CURSOR FOR\n");
    let offset = head.chars().count();
    let batch = format!("{head}{sql}\n;\n{}", fetch_sql(n));
    (batch, offset)
}

/// `FETCH FORWARD n FROM pgman_page`. Pure.
pub fn fetch_sql(n: usize) -> String {
    format!("FETCH FORWARD {n} FROM {CURSOR_NAME}")
}

/// A server error's position (1-indexed, into the batch that was sent)
/// as a position into the user's statement, which starts `offset`
/// characters in and is `len` characters long. `None` when it points
/// at pgman's own wrapping. Pure.
pub fn statement_position(position: Option<u32>, offset: usize, len: usize) -> Option<u32> {
    let p = usize::try_from(position?).ok()?;
    let inner = p.checked_sub(offset).filter(|&q| q >= 1 && q <= len)?;
    u32::try_from(inner).ok()
}

/// A `pg_settings.setting` timeout as a duration; `0`, a NULL or
/// anything unreadable is "no timeout". Pure.
pub fn parse_setting_ms(setting: Option<&str>) -> Option<Duration> {
    let ms: u64 = setting?.trim().parse().ok()?;
    (ms > 0).then(|| Duration::from_millis(ms))
}

/// When pgman closes an open cursor on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Since the last fetch.
    pub idle: Duration,
    /// Since the cursor was opened.
    pub lifetime: Duration,
}

/// [`IDLE_CLOSE`] / [`LIFETIME_CLOSE`], each cut to half the server's
/// matching timeout when that is set: a server that ends an
/// idle-in-transaction session would take pgman's whole connection with
/// it, so pgman has to get there first. Never below half a second. Pure.
pub fn close_limits(idle_in_tx: Option<Duration>, transaction: Option<Duration>) -> Limits {
    let cut = |own: Duration, server: Option<Duration>| {
        server.map(|s| own.min(s / 2)).unwrap_or(own).max(MIN_CLOSE)
    };
    Limits {
        idle: cut(IDLE_CLOSE, idle_in_tx),
        lifetime: cut(LIFETIME_CLOSE, transaction),
    }
}

/// How many rows to `FETCH` to deliver `want` and still know whether
/// more exist: one past `want`, less the look-ahead row already held.
/// Pure.
pub fn fetch_count(have_lookahead: bool, want: usize) -> usize {
    want + 1 - usize::from(have_lookahead)
}

/// Split the look-ahead row plus what a `FETCH` returned into the rows
/// to show (at most `want`) and the next look-ahead. A look-ahead means
/// more rows exist; none means the result is complete. Pure.
pub fn split_page(
    lookahead: Option<Vec<Cell>>,
    fetched: Vec<Vec<Cell>>,
    want: usize,
) -> (Vec<Vec<Cell>>, Option<Vec<Cell>>) {
    let mut rows: Vec<Vec<Cell>> = lookahead.into_iter().chain(fetched).collect();
    if rows.len() <= want {
        return (rows, None);
    }
    let next = rows.split_off(want).into_iter().next();
    (rows, next)
}

/// What [`Pager::open`] hands back.
#[derive(Debug)]
pub struct Opened {
    /// The first page. `truncated` iff more rows exist.
    pub grid: Grid,
    /// `Some` while the cursor stays open for `M`; `None` when the
    /// result was complete (or ran unpaged).
    pub cursor: Option<Limits>,
    /// The session was already inside a transaction (pgman's auto-tx,
    /// or a `BEGIN` the operator typed), so the statement ran the old
    /// way, unpaged: closing a cursor there would end *their*
    /// transaction.
    pub in_transaction: bool,
    /// Where each column came from ([`crate::conn::column_origins`]),
    /// so a cell of a paged result can still be edited.
    pub origins: Vec<Option<crate::grid::ColumnOrigin>>,
}

/// What [`Pager::fetch`] hands back.
#[derive(Debug)]
pub struct Page {
    pub rows: Vec<Vec<Cell>>,
    /// More rows exist and the cursor is still open. `false`: the
    /// result is complete and pgman has committed.
    pub more: bool,
}

type Reply<T> = oneshot::Sender<Result<T, QueryErr>>;

enum Req {
    Open {
        sql: String,
        want: usize,
        reply: Reply<Opened>,
    },
    Fetch {
        want: usize,
        reply: Reply<Page>,
    },
    /// Close any open cursor, then answer (a barrier) or not.
    Close(Option<oneshot::Sender<()>>),
}

/// The cursor's server-side state, owned by the pager task.
struct OpenCursor {
    lookahead: Option<Vec<Cell>>,
    n_columns: usize,
}

/// Handle to one connection's pager task. Every request is queued the
/// moment the method is called (an unbounded send never waits), so the
/// order of calls on the UI thread is the order on the wire. Dropping
/// every handle ends the task.
#[derive(Clone)]
pub struct Pager {
    tx: mpsc::UnboundedSender<Req>,
}

impl Pager {
    /// Start the pager for `client`. Must be called inside a Tokio
    /// runtime.
    pub fn spawn(client: Arc<tokio_postgres::Client>) -> Pager {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(client, rx));
        Pager { tx }
    }

    /// Run `sql` (a statement `safety::is_pageable_select` accepted)
    /// behind a fresh cursor and deliver its first `want` rows, closing
    /// any cursor already open.
    pub fn open(&self, sql: String, want: usize) -> oneshot::Receiver<Result<Opened, QueryErr>> {
        let (reply, rx) = oneshot::channel();
        self.send(Req::Open { sql, want, reply });
        rx
    }

    /// The next `want` rows of the open cursor.
    pub fn fetch(&self, want: usize) -> oneshot::Receiver<Result<Page, QueryErr>> {
        let (reply, rx) = oneshot::channel();
        self.send(Req::Fetch { want, reply });
        rx
    }

    /// Close the open cursor, if any, without waiting.
    pub fn close(&self) {
        self.send(Req::Close(None));
    }

    /// A place in the queue: resolves once every earlier request is done
    /// and no cursor is open.
    pub fn barrier(&self) -> oneshot::Receiver<()> {
        let (reply, rx) = oneshot::channel();
        self.send(Req::Close(Some(reply)));
        rx
    }

    fn send(&self, req: Req) {
        // The task only ends when every handle is gone, or a panic took
        // it; either way the reply channel's drop tells the caller.
        let _ = self.tx.send(req);
    }
}

/// The client for one piece of database work that is not the cursor's:
/// it waits its turn behind the pager, which closes any open cursor
/// first, so the work never runs inside the cursor's transaction.
pub struct Db {
    client: Arc<tokio_postgres::Client>,
    barrier: Option<oneshot::Receiver<()>>,
}

impl Db {
    /// Take a place in `pager`'s queue now; `None` (no pager — a test
    /// harness) waits for nothing.
    pub fn new(client: Arc<tokio_postgres::Client>, pager: Option<&Pager>) -> Db {
        Db {
            client,
            barrier: pager.map(Pager::barrier),
        }
    }

    /// The client, once the work queued before this is done.
    pub async fn ready(self) -> Arc<tokio_postgres::Client> {
        if let Some(barrier) = self.barrier {
            // A dropped barrier means the pager is gone, and with it any
            // cursor it held: nothing left to wait for.
            let _ = barrier.await;
        }
        self.client
    }
}

async fn run(client: Arc<tokio_postgres::Client>, mut rx: mpsc::UnboundedReceiver<Req>) {
    let mut cursor: Option<OpenCursor> = None;
    while let Some(req) = rx.recv().await {
        match req {
            Req::Close(reply) => {
                if cursor.take().is_some() {
                    end(&client, "COMMIT").await;
                }
                if let Some(reply) = reply {
                    let _ = reply.send(());
                }
            }
            Req::Open { sql, want, reply } => {
                if cursor.take().is_some() {
                    end(&client, "COMMIT").await;
                }
                let result = open(&client, &sql, want).await.map(|(opened, open)| {
                    cursor = open;
                    opened
                });
                let _ = reply.send(result);
            }
            Req::Fetch { want, reply } => {
                let result = match cursor.take() {
                    None => Err(QueryErr::msg("the result cursor is closed")),
                    Some(open) => fetch(&client, open, want).await.map(|(page, open)| {
                        cursor = open;
                        page
                    }),
                };
                let _ = reply.send(result);
            }
        }
    }
}

/// `COMMIT` or `ROLLBACK` the cursor's transaction. A failure means the
/// connection is gone (and the transaction with it), so it is logged,
/// not returned.
async fn end(client: &tokio_postgres::Client, verb: &str) {
    if let Err(e) = client.batch_execute(verb).await {
        tracing::warn!("closing the result cursor ({verb}) failed: {e}");
    }
}

async fn open(
    client: &tokio_postgres::Client,
    sql: &str,
    want: usize,
) -> Result<(Opened, Option<OpenCursor>), QueryErr> {
    // Pipelined: tokio-postgres sends both before awaiting either, so
    // the probe costs no round trip of its own.
    let (stmt, probe) =
        futures::future::join(client.prepare(sql), client.simple_query(IDLE_PROBE_SQL)).await;
    let stmt = stmt.map_err(QueryErr::from)?;
    let columns: Vec<String> = stmt
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    let origins = crate::conn::column_origins(stmt.columns());
    let idle = probe.ok().is_some_and(|msgs| {
        msgs.iter().any(
            |m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(r) if r.get(0) == Some("t")),
        )
    });
    if !idle || columns.is_empty() {
        // Today's path. A zero-column `SELECT FROM t` reports its row
        // count, which a cursor would not.
        let grid = crate::conn::run_statement(client, sql).await?;
        return Ok((
            Opened {
                grid,
                cursor: None,
                in_transaction: !idle,
                origins,
            },
            None,
        ));
    }
    let (batch, offset) = open_batch(sql, fetch_count(false, want));
    let (settings, rows) = match read_open_batch(client, &batch, columns.len()).await {
        Ok(read) => read,
        Err(mut e) => {
            end(client, "ROLLBACK").await;
            e.position = statement_position(e.position, offset, sql.chars().count());
            return Err(e);
        }
    };
    let (rows, lookahead) = split_page(None, rows, want);
    let more = lookahead.is_some();
    if !more {
        end(client, "COMMIT").await;
    }
    let grid = Grid {
        columns,
        rows,
        truncated: more,
    };
    let open = more.then_some(OpenCursor {
        lookahead,
        n_columns: grid.columns.len(),
    });
    let opened = Opened {
        grid,
        cursor: more.then(|| close_limits(settings.0, settings.1)),
        in_transaction: false,
        origins,
    };
    Ok((opened, open))
}

/// Run [`open_batch`]'s round trip: the timeouts from the settings read
/// and the rows from the `FETCH`.
async fn read_open_batch(
    client: &tokio_postgres::Client,
    batch: &str,
    n_columns: usize,
) -> Result<((Option<Duration>, Option<Duration>), Vec<Vec<Cell>>), QueryErr> {
    use futures::StreamExt;
    let stream = client
        .simple_query_raw(batch)
        .await
        .map_err(QueryErr::from)?;
    let mut stream = Box::pin(stream);
    let mut stmt = 0usize;
    let mut settings = (None, None);
    let mut rows = Vec::new();
    while let Some(msg) = stream.next().await {
        match msg.map_err(QueryErr::from)? {
            tokio_postgres::SimpleQueryMessage::CommandComplete(_) => stmt += 1,
            tokio_postgres::SimpleQueryMessage::Row(row) if stmt == SETTINGS_STMT => {
                settings = (
                    parse_setting_ms(row.try_get(0).ok().flatten()),
                    parse_setting_ms(row.try_get(1).ok().flatten()),
                );
            }
            tokio_postgres::SimpleQueryMessage::Row(row) if stmt == FETCH_STMT => {
                rows.push(crate::conn::text_row_cells(&row, n_columns));
            }
            _ => {}
        }
    }
    Ok((settings, rows))
}

async fn fetch(
    client: &tokio_postgres::Client,
    open: OpenCursor,
    want: usize,
) -> Result<(Page, Option<OpenCursor>), QueryErr> {
    use futures::StreamExt;
    let sql = fetch_sql(fetch_count(open.lookahead.is_some(), want));
    let mut fetched = Vec::new();
    let read: Result<(), QueryErr> = async {
        let stream = client
            .simple_query_raw(&sql)
            .await
            .map_err(QueryErr::from)?;
        let mut stream = Box::pin(stream);
        while let Some(msg) = stream.next().await {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg.map_err(QueryErr::from)? {
                fetched.push(crate::conn::text_row_cells(&row, open.n_columns));
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = read {
        // A failed or cancelled FETCH aborted the transaction.
        end(client, "ROLLBACK").await;
        return Err(e);
    }
    let (rows, lookahead) = split_page(open.lookahead, fetched, want);
    let more = lookahead.is_some();
    if !more {
        end(client, "COMMIT").await;
    }
    let next = more.then_some(OpenCursor {
        lookahead,
        n_columns: open.n_columns,
    });
    Ok((Page { rows, more }, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(n: usize) -> Vec<Cell> {
        vec![Some(n.to_string())]
    }

    #[test]
    fn the_open_batch_wraps_the_statement_and_knows_where_it_starts() {
        let (batch, offset) = open_batch("SELECT 1 -- trailing", 1001);
        assert!(batch.starts_with("BEGIN;\n"), "{batch}");
        assert!(
            batch.ends_with("SELECT 1 -- trailing\n;\nFETCH FORWARD 1001 FROM pgman_page"),
            "{batch}"
        );
        let tail: String = batch.chars().skip(offset).collect();
        assert!(tail.starts_with("SELECT 1"), "{tail}");
        // Four statements, the comment unable to swallow the FETCH.
        let parts = crate::safety::split_statements(&batch);
        assert_eq!(parts.len(), 4, "{parts:?}");
        assert!(parts[2].starts_with("DECLARE pgman_page NO SCROLL CURSOR FOR"));
        assert_eq!(parts[3], "FETCH FORWARD 1001 FROM pgman_page");
    }

    #[test]
    fn an_error_position_maps_back_into_the_statement_or_to_nothing() {
        // The statement starts after 10 characters of wrapping and is 8 long.
        assert_eq!(statement_position(Some(11), 10, 8), Some(1));
        assert_eq!(statement_position(Some(18), 10, 8), Some(8));
        assert_eq!(statement_position(Some(19), 10, 8), None, "past the end");
        assert_eq!(statement_position(Some(10), 10, 8), None, "in the wrapping");
        assert_eq!(statement_position(Some(3), 10, 8), None);
        assert_eq!(statement_position(None, 10, 8), None);
    }

    #[test]
    fn a_timeout_setting_reads_in_milliseconds_and_zero_is_none() {
        assert_eq!(parse_setting_ms(Some("0")), None);
        assert_eq!(parse_setting_ms(None), None);
        assert_eq!(parse_setting_ms(Some("abc")), None);
        assert_eq!(
            parse_setting_ms(Some("30000")),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn the_close_limits_undercut_the_servers_timeouts() {
        assert_eq!(
            close_limits(None, None),
            Limits {
                idle: IDLE_CLOSE,
                lifetime: LIFETIME_CLOSE
            }
        );
        // A server killing idle transactions at 30 s: close at 15 s.
        let l = close_limits(
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(300)),
        );
        assert_eq!(l.idle, Duration::from_secs(15));
        assert_eq!(l.lifetime, Duration::from_secs(150));
        // A generous server changes nothing.
        let l = close_limits(Some(Duration::from_secs(3600)), None);
        assert_eq!(l.idle, IDLE_CLOSE);
        // A tiny one is floored.
        let l = close_limits(Some(Duration::from_millis(10)), None);
        assert_eq!(l.idle, MIN_CLOSE);
    }

    #[test]
    fn a_page_keeps_one_row_back_to_know_whether_more_exist() {
        assert_eq!(fetch_count(false, 1000), 1001);
        assert_eq!(fetch_count(true, 1000), 1000);
        // Exactly `want` rows: complete, no look-ahead.
        let (rows, next) = split_page(None, (0..3).map(row).collect(), 3);
        assert_eq!(rows.len(), 3);
        assert_eq!(next, None);
        // One more than `want`: it becomes the look-ahead.
        let (rows, next) = split_page(None, (0..4).map(row).collect(), 3);
        assert_eq!(rows, (0..3).map(row).collect::<Vec<_>>());
        assert_eq!(next, Some(row(3)));
        // The held look-ahead leads the next page, in order.
        let (rows, next) = split_page(Some(row(3)), (4..7).map(row).collect(), 3);
        assert_eq!(rows, (3..6).map(row).collect::<Vec<_>>());
        assert_eq!(next, Some(row(6)));
        // A short last page: everything shown, nothing more.
        let (rows, next) = split_page(Some(row(6)), vec![row(7)], 3);
        assert_eq!(rows, vec![row(6), row(7)]);
        assert_eq!(next, None);
    }
}
