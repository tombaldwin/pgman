# Logs → runnable SQL, in sixty seconds

Paste a log, get back queries you can actually run — bind parameters
substituted in, terminated, ready for `Enter`. This walks through it end
to end.

## What it reads

Three input shapes are recognised:

- **Hibernate application logs** — an `org.hibernate.SQL` logger line opens a
  statement, followed by the separately-logged bind parameters (`binding
  parameter [1] as [INTEGER] - [42]` on Hibernate 5, `binding parameter
  (1:INTEGER) <- [42]` on Hibernate 6). Multi-line `hibernate.format_sql=true`
  output is reassembled. `hibernate.show_sql=true`'s console form —
  `Hibernate: select …`, what Spring Boot prints by default — works too,
  with its binds attached.

  The bind-parameter lines are logged at `TRACE` and are usually off in
  production, so turn them on deliberately. Hibernate 6 logs them under
  `org.hibernate.orm.jdbc.bind`; Hibernate 5 under
  `org.hibernate.type.descriptor.sql.BasicBinder`. Logback:

  ```xml
  <logger name="org.hibernate.orm.jdbc.bind" level="TRACE"/>
  ```

  (or `org.hibernate.type.descriptor.sql.BasicBinder` on Hibernate 5). Without
  this, pgman still recovers the statement, just with `?` placeholders instead
  of substituted values.
- **Postgres / RDS server logs** — `LOG:  statement: <sql>` for simple
  queries, or a `LOG:  duration: … execute <tag>: <sql>` paired with the
  following `DETAIL:  parameters: $1 = '…'` line for the extended protocol.
  This is the more reliable source: it needs `log_min_duration_statement` /
  `log_statement` turned on server-side, no application redeploy. Whatever
  `log_line_prefix` printed before the `LOG:` is skipped, so the Debian /
  Ubuntu default (`%m [%p] %q%u@%d `), RDS's (`%t:%r:%u@%d:[%p]:`) and
  pgBadger's (`%t [%p]: [%l-1] user=%u,db=%d,app=%a,client=%h `) all work;
  include `%p` in it so a `bind`'s parameters pair with the right backend's
  `execute`.

  A failed statement is reconstructed too: an `ERROR:` followed by its
  `STATEMENT:` from the same backend — which the default
  `log_min_error_statement = error` logs with nothing else turned on —
  becomes a query marked failed with the error message. Its parameters
  come from a `CONTEXT:  … with parameters:` line in the error report
  (Postgres 13+, `log_parameter_max_length_on_error`) or the backend's
  last `bind`. When `log_statement` already logged the statement before
  it ran, that query is the one marked failed rather than a second copy
  appearing, so the failure doesn't read as a ×2 N+1 cluster. A `FATAL`'s
  `STATEMENT:` isn't reconstructed.
- **Pasted JDBC** — no log at all, just a `?`-placeholder statement plus a
  typed parameter list you already have (a debugger watch, a
  `PreparedStatement` toString, whatever). See "Three ways in" below for the
  exact shape.

## The sample

Paste this into the editor — it also matches what `pgman --demo` ships,
so running a pick returns real-looking rows even with no database
connected:

```
2024-01-15 10:00:00.100 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select i.id, i.number, i.total from invoices i where i.status=?
2024-01-15 10:00:00.101 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [VARCHAR] - [open]
2024-01-15 10:00:00.110 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select l.id, l.description, l.amount from invoice_lines l where l.invoice_id=?
2024-01-15 10:00:00.111 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [BIGINT] - [48202]
2024-01-15 10:00:00.120 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select l.id, l.description, l.amount from invoice_lines l where l.invoice_id=?
2024-01-15 10:00:00.121 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [BIGINT] - [48204]
```

That `invoice_lines` select fires twice for two different `invoice_id`s in
the same burst — a classic N+1: the open invoices, then a per-invoice
line lookup that should have been a join.

## Three ways in

- **Paste it.** Press `e` to focus the editor, paste the lines above, then
  press `F8` (or `ctrl-l` — same binding, for terminals that eat function
  keys). pgman recognises the shape of what landed in the buffer and hints
  at this in the status line: `looks like a hibernate log · ctrl-l / F8 to
  reconstruct queries`.
- **Skip the paste.** `pgman --log app.log` (or `--log -` to read stdin)
  loads the file straight into the editor and runs the same reconstruction
  immediately — you land straight in the picker below, no keypress needed.
- **No log? Paste the statement and its params instead.** If all you have is
  a `?`-placeholder statement and a list of bound values — no log framing at
  all — paste both into the editor separated by one blank line: the
  statement first, then one `TYPE:value` parameter per line, in order:

  ```
  select * from invoices where customer_id = ? and status = ?

  BIGINT:2107
  VARCHAR:open
  ```

  `F8` / `ctrl-l` recognise this shape exactly like a log paste (the hint
  reads `looks like a jdbc log · ctrl-l / F8 to reconstruct queries`) and
  land you in the same picker below with one pick: `select * from invoices
  where customer_id = 2107 and status = 'open'`. A `NULL` value works for any type
  (`VARCHAR:NULL`); lines without a `TYPE:` prefix are skipped, so stray
  blank lines or comments in the tail don't break the parse.

All three paths run through the same picker.

## The pick list

pgman opens the pick list with a one-line triage summary up top:

```
3 queries · 1 N+1 cluster (2 of 3 repeated) · view: all queries (press `c` to toggle)
leader (×2): select l.id, l.description, l.amount from invoice_lines l where l.invoice_id=?
```

(The leader line and the cluster view below both show the raw, unsubstituted
SQL — that's the *shape* a cluster groups by. The per-row list underneath
shows the runnable, substituted form.)

Below it, one row per reconstructed query, tagged by source and showing the
runnable SQL with bind values substituted in:

```
▶ [hibernate] select i.id, i.number, i.total from invoices i where i.status='open'
  [hibernate] select l.id, l.description, l.amount from invoice_lines l where l.invoice_id=48202
  [hibernate] select l.id, l.description, l.amount from invoice_lines l where l.invoice_id=48204
```

Press `c` to flip to the cluster view — the same queries grouped by shape,
most-repeated first, so a loop-driven select stands out instead of hiding
among near-identical rows in a longer log:

```
▶ ×2   select l.id, l.description, l.amount from invoice_lines l where l.invoice_id=?
```

`c` again toggles back. `j`/`k` (or the arrow keys) move the selection,
`g`/`G` jump to the first/last row.

## Load it and run it

`Enter` loads the highlighted query's runnable SQL into the editor — a `;`
appended if the log had none, cursor at the end — and drops you back into
the editor with a `loaded query · N char(s)` status. In the cluster view,
`Enter` loads the cluster's first
member whose values are all bound — a runnable statement, not the `?`
template the row shows; when the log bound no values for that shape at all
(bind logging off), the template loads instead and the status says so:
`no bound values in the log for this query — ? placeholders remain`. From
there it's an ordinary pgman query: `Enter` runs it — the buffer ends with
`;`, which is what makes a bare `Enter` run rather than break the line
(`Alt-Enter`, `F5`, `ctrl-Enter` and `ctrl-J` run it regardless) — through
the normal safety guard (see
[docs/safety-and-privacy.md](safety-and-privacy.md)) — same read-only
defaults, same per-database rules — and the result lands in the grid like any
other statement. `Esc` or `q` from the picker cancels back to the editor
without loading anything.

## N+1 detection

The clustering behind both the summary line and the cluster view
fingerprints each statement (lowercased, whitespace collapsed,
string/numeric literals and placeholders all reduced to `?`) and
groups by that shape. Any fingerprint that fires **twice or more**
becomes a cluster, sorted most-repeated first. Two structurally
different one-off queries never cluster; two copies of the same query with
different literals always do — which is exactly the loop-driven-select
signature this is built to catch.

## Implementation notes

Reconstruction: `src/query/hibernate.rs`, `src/query/pglog.rs`,
`src/query/jdbc.rs`. Clustering: `src/query/nplus1.rs`.
