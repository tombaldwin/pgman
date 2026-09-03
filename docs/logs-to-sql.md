# Logs → runnable SQL, in sixty seconds

Paste a log, get back queries you can actually run — bind parameters
substituted in, ready for `F5`. This walks through it end to end.

## What it reads

Three input shapes are recognised:

- **Hibernate application logs** — an `org.hibernate.SQL` logger line opens a
  statement, followed by the separately-logged bind parameters (`binding
  parameter [1] as [INTEGER] - [42]` on Hibernate 5, `binding parameter
  (1:INTEGER) <- [42]` on Hibernate 6). Multi-line `hibernate.format_sql=true`
  output is reassembled.

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
  `log_statement` turned on server-side, no application redeploy.
- **Pasted JDBC** — no log at all, just a `?`-placeholder statement plus a
  typed parameter list you already have (a debugger watch, a
  `PreparedStatement` toString, whatever). See "Three ways in" below for the
  exact shape.

## The sample

Paste this into the editor — it also matches what `pgman --demo` ships,
so `F5` returns real-looking rows even with no database connected:

```
2024-01-15 10:00:00.100 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select o.id, o.total_cents from orders o where o.user_id=?
2024-01-15 10:00:00.101 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]
2024-01-15 10:00:00.110 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select oi.id, oi.sku from order_items oi where oi.order_id=?
2024-01-15 10:00:00.111 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [101]
2024-01-15 10:00:00.120 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select oi.id, oi.sku from order_items oi where oi.order_id=?
2024-01-15 10:00:00.121 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [102]
```

That `order_items` select fires twice for two different `order_id`s in the
same burst — a classic N+1: an order lookup followed by a per-row item
lookup that should have been a join.

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
  select * from orders where id = ? and status = ?

  INTEGER:42
  VARCHAR:shipped
  ```

  `F8` / `ctrl-l` recognise this shape exactly like a log paste (the hint
  reads `looks like a jdbc log · ctrl-l / F8 to reconstruct queries`) and
  land you in the same picker below with one pick: `select * from orders
  where id = 42 and status = 'shipped'`. A `NULL` value works for any type
  (`VARCHAR:NULL`); lines without a `TYPE:` prefix are skipped, so stray
  blank lines or comments in the tail don't break the parse.

All three paths run through the same picker.

## The pick list

pgman opens the pick list with a one-line triage summary up top:

```
3 queries · 1 N+1 cluster (2 of 3 repeated) · view: all queries (press `c` to toggle)
leader (×2): select oi.id, oi.sku from order_items oi where oi.order_id=?
```

(The leader line and the cluster view below both show the raw, unsubstituted
SQL — that's the *shape* a cluster groups by. The per-row list underneath
shows the runnable, substituted form.)

Below it, one row per reconstructed query, tagged by source and showing the
runnable SQL with bind values substituted in:

```
▶ [hibernate] select o.id, o.total_cents from orders o where o.user_id=42
  [hibernate] select oi.id, oi.sku from order_items oi where oi.order_id=101
  [hibernate] select oi.id, oi.sku from order_items oi where oi.order_id=102
```

Press `c` to flip to the cluster view — the same queries grouped by shape,
most-repeated first, so a loop-driven select stands out instead of hiding
among near-identical rows in a longer log:

```
▶ ×2   select oi.id, oi.sku from order_items oi where oi.order_id=?
```

`c` again toggles back. `j`/`k` (or the arrow keys) move the selection,
`g`/`G` jump to the first/last row.

## Load it and run it

`Enter` loads the highlighted query's runnable SQL into the editor, cursor at
the end, and drops you back into the editor with a `loaded query · N
char(s)` status. From there it's an ordinary pgman query: `F5` runs it (or
`ctrl-Enter` / `ctrl-J`) through the normal safety guard (see
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
