# Design note: paging past `MAX_ROWS`

Status: decided 2026-10-23, for the "Page beyond `MAX_ROWS`" backlog
item. Class: architecture (a new seam between the app and the session).

## The problem

The editor's run path streams a row-returning statement over the
simple-query protocol (`conn::stream_text_rows`) and stops reading at
`grid::MAX_ROWS` (1000). The title says `capped at 1000 · more rows not
fetched`, and there is no way to see row 1001 from the TUI short of
rewriting the query. Dropping the stream does not stop the server, either:
it computes and sends the whole result to a socket nobody reads.

## Choice 1: a server-side cursor, not `OFFSET`

Re-running with `LIMIT 1000 OFFSET n` is wrong for any query without a
total order: Postgres may return the rows in a different order each run,
so page 2 can repeat or skip rows of page 1. It also re-executes the whole
query per page, and every page is a fresh snapshot.

A cursor keeps one execution and one snapshot. Two ways to get one:

- **The extended-protocol portal** (`Bind` + `Execute` with a row limit,
  tokio-postgres's `Transaction::query_portal`). It needs a
  `Transaction<'_>`, which borrows `&mut Client`; the app shares one
  `Arc<Client>` between tasks, so there is no `&mut` to borrow. It also
  returns binary rows, and the grid relies on the text wire so that every
  type renders as psql prints it (docs/backlog/null-cells.md, and the
  `?`-cells bug the text wire fixed).
- **SQL `DECLARE … CURSOR` + `FETCH`** over the simple-query protocol.
  Text rows, the same decoder as today, works through `&Client`.

**Decision: `DECLARE pgman_page NO SCROLL CURSOR FOR <select>` and `FETCH
FORWARD n FROM pgman_page`**, over `simple_query_raw`. `NO SCROLL`
because pgman only reads forward and it lets the executor skip
materialising for backward reads. Not `WITH HOLD`: a holdable cursor is
materialised in full at commit, which is the memory problem moved to the
server.

## Choice 2: which statements get a cursor

Only a statement `safety::classify` calls `Select`, and of those only the
ones `DECLARE` accepts and that are safe to hold open:
`safety::is_pageable_select` says yes to `SELECT` / `TABLE` / `VALUES` /
a read-only `WITH`, and no to `SHOW` (not a query), to anything with a
locking clause (`FOR UPDATE` / `FOR NO KEY UPDATE` / `FOR SHARE` / `FOR
KEY SHARE`, anywhere in the code — a lock held for as long as the cursor
is open is exactly what must not happen on a production database) and to
a statement `classify` would not call `Select` — and to any statement
that calls a function outside `safety::PAGEABLE_FUNCTIONS`, a short list
of built-ins that read and never write (aggregates, window functions,
`coalesce`, string, number, date, array and JSON functions). Behind a
cursor a function runs only for the rows fetched, and its writes stay
uncommitted while the cursor is open: `SELECT log_visit(i) FROM
generate_series(1, 3000) i` would make 1001 calls, invisible to every
other session until the close, where 0.1.0 made all 3000 and committed
them at once. So a user function, `nextval`, `random`, or a name
qualified by any schema but `pg_catalog` runs today's way, unpaged. The
check reads the statement's text, so a function a *view* calls is out
of its sight; `pg_proc.provolatile` would see only the functions named
in the statement too, and a lexical list needs no round trip. And only for a plain
`Run` from the editor: `EXPLAIN`, multi-statement scripts, a write's
auto-tx, `\watch` re-runs and every other path keep today's behaviour.

## Choice 3: the transaction

A non-holdable cursor lives in a transaction block. pgman opens it:
`BEGIN; <read the two timeout settings>; DECLARE …; FETCH FORWARD 1001`,
one round trip.

- **The session may already be in a transaction** — pgman's own auto-tx
  awaiting commit / rollback, or a `BEGIN` the operator typed under
  `auto_tx = false`. Our `BEGIN` would only warn, and closing our cursor
  with `COMMIT` would commit *their* transaction. So before opening,
  pgman asks: `SELECT statement_timestamp() = transaction_timestamp()`.
  Postgres documents the two as equal during the first command of a
  transaction, so the answer is `true` exactly when the probe ran in its
  own implicit transaction, i.e. the session was idle. The probe is
  pipelined with the `prepare` the run already does (tokio-postgres sends
  both before awaiting either), so it adds no round trip. Not idle (or
  the probe failed, e.g. an aborted transaction): today's path, unpaged.
- **A small result never holds a transaction.** The first `FETCH` asks
  for 1001 rows. 1000 or fewer means the result is complete, and pgman
  `COMMIT`s straight away; the extra row is how it knows. Past that, the
  extra row is kept back as a look-ahead and shown at the head of the
  next page, so "more rows exist" is always exact.
- **Closed with `COMMIT`, not `ROLLBACK`**, so whatever a paged
  `SELECT` did is kept as it would be in autocommit. A `SELECT` that
  calls a function that could write never gets here (Choice 2): rows
  never fetched would never run it.
  An error or a cancel aborts the transaction and pgman sends `ROLLBACK`.
- **A standby holds no cursor.** Replaying the primary's DDL or
  `VACUUM` conflicts with an open transaction on a standby, and after
  `max_standby_streaming_delay` (30 s by default; less on an Aurora
  reader) the server terminates the session — pgman's whole connection.
  The probe asks `pg_is_in_recovery()` in the same row, per run (a
  standby promoted since is a primary again), and on a standby the
  pager fetches the first page and `COMMIT`s whatever it found. The
  title says `partial · on a standby · M re-runs for more`; `M` takes
  the re-run path of Choice 6, the rows held plus a page in one piece.
- **Read-only sessions**: `BEGIN` inherits
  `default_transaction_read_only`, so the cursor's transaction is
  read-only too. Nothing changes.
- **`statement_timeout`** applies to each `FETCH`, not to the life of the
  cursor. That is the right reading: each page is one bounded statement.
- **The cancel key** (`Ctrl-C`) cancels the running `FETCH` through the
  same `CancelToken`; the error aborts the transaction, pgman rolls it
  back and keeps the rows it already has.

## Choice 4: one session, many tasks — the pager

The app runs every database task (`T`, `L`, schema refresh, `:readonly`,
the DDL yank, the next query) on the same `Client`. With a cursor open,
any of them would run *inside the cursor's transaction*: an error there
(a missing `pg_stat_statements`) aborts it, killing the cursor and making
every later statement fail with "current transaction is aborted"; a
`pg_stat_activity` read is snapshotted for the life of the transaction,
so the sessions panel would freeze. And tokio-postgres orders requests
by when a future is first polled, so a `COMMIT` spawned as its own task
can race the next query and commit an auto-tx write before anyone
pressed `y`.

**Decision: a pager task per connection (`query::paging::Pager`) owns
the cursor, and every other database task waits at its door.** The pager
reads requests from an unbounded channel, in order: `Open`, `Fetch`,
`Close`, and `Barrier`. `App::db()` is the only way to the client: it
sends a `Barrier` synchronously (so its place in the queue is fixed
before the task is even spawned) and hands back a handle whose `ready()`
waits for the pager to close any open cursor. A guard test pins that no
code outside `App::db` clones `self.client`.

Consequences, all deliberate: leaving the result for a panel that queries
closes the cursor; a new query closes it; the idle timer closes it.

## Choice 5: when to fetch

**`M` (fetch more) in the grid, no auto-fetch.** Each fetch is server
work, possibly on production; a held `j` that paged through 100 000 rows
would be a surprise. When the selection reaches the last row of a partial
result, the status line names `M`. `M` is a plain key and free in Normal
mode (`+` is already the editor size; house rule: no F-keys or Alt chords
as primary bindings).

## Choice 6: limits and closing

What an open cursor costs, plainly: while it is open the session is
**idle in transaction**, holding an `ACCESS SHARE` lock on every table
(and index) the query reads, until the close. `ACCESS SHARE` conflicts
only with `ACCESS EXCLUSIVE`, so reads and writes carry on — but DDL
does not: a migration, `ALTER TABLE`, `TRUNCATE`, `VACUUM FULL`,
`REINDEX` waits for pgman, and every later statement on those tables
queues behind that waiting DDL. The transaction also pins the xmin
horizon, holding back vacuum. The limits below are what bounds it: 60 s
idle, 10 min in all, any other work, or the end of the result. On a
standby none of this applies, because no cursor is held (Choice 3).

- **Ceiling: 100 000 rows held** (`paging::MAX_HELD_ROWS`). At the
  ceiling pgman closes the cursor and the title says `partial · stopped
  at pgman's 100000-row limit`; `M` points at `pgman query … --json`,
  which is not capped.
- **Idle timeout: 60 s** after the last fetch, and **10 min** from the
  open, whichever is first. Both shrink to half the server's
  `idle_in_transaction_session_timeout` / `transaction_timeout` when
  those are set, read in the opening round trip: a server that kills
  idle-in-transaction sessions would otherwise take pgman's whole
  connection with it. The main loop wakes for the deadline the way it
  wakes for the draft save, so an idle app still closes on time.
- **Closed on**: a new query, any other database task (the barrier), the
  timeout, closing the tab that owns it, reconnect / disconnect (the
  pager is dropped with the old client; the server ends the transaction
  with the connection), the ceiling, and the end of the result.
- **After a close**, the title says `partial · cursor closed · M
  re-runs`, and `M` re-runs the statement and fetches as many rows as
  were held plus a page: a fresh snapshot, in one piece, never stitched
  onto the old rows. No second guard check: the statement passed
  `is_pageable_select` when it first ran, a plain `SELECT` every guard
  allows, and that classification is a pure function of the text.
- **Reconnect** closes the cursor on the session being left (the old
  pager queues the `COMMIT`) rather than leave it until the old
  connection drops. A grid that replaces the result without a run (`\l`,
  the bootstrap) drops its paging record and closes its cursor.
- **`Ctrl-C` from the results** cancels a running query or fetch, as it
  does from the editor; it used to arm quit there.

## Choice 7: what works on a partial result

Sort, filter, find, the row detail, `Y`, `I`, `:fixture` and the diff work
on the rows pgman holds; the title says `partial` while more exist. `Y`
and `:fixture` say `(fetched rows only)` in their status when the result
is partial. A fetch appends to the held rows and
re-applies the active sort and filter; bookmarks survive unless a sort
reorders the rows under them.

## Tabs

The cursor belongs to the result it was opened for (a result id kept in
the tab's snapshot). Switching tabs leaves it open; the timeout still
runs. Tab switching is already refused while a query or fetch runs.
Closing the owning tab closes it.

## Tests

Pure: eligibility, the batch SQL and its error-offset, the settings
parse and timeout clamp, the look-ahead split, the title notes, the
append-under-sort. Integration (docker DB): 10 000 rows in pages with no
gap or repeat and `ORDER BY` order kept; cancel during a `FETCH`; a new
statement closes the cursor (`pg_stat_activity.state` for the pager's
backend from a second connection goes from `idle in transaction` to
`idle`; `pg_cursors` only lists the asking session's own cursors, so it
cannot be read from outside); the idle close; a read-only session; a session already in a
transaction falls back without touching it.
