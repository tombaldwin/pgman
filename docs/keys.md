# Keys

Press `?` (or `F1`, which also works while typing) for the in-app cheat sheet — it opens at the section
for whatever mode you opened it from. `q` closes most panels and quits from
the grid / connection picker; `Esc` closes overlays; in Normal mode it goes
back to the environments overview when environments are configured, and
does nothing otherwise (a reflex press can't drop the session). `F1` (help), `F2` (error
detail), `F3` (notifications) and `F4` (JDBC tap monitor) work from **any**
mode, as do `?` (help) and `:` (the command bar) — except while a text input
has focus, where both are characters you meant to type — and except in
the y/n prompts (confirm a statement, commit or roll back, terminate a
session), which keep the keyboard until answered; help still opens
from them. On a Mac keyboard the F-keys take `fn`, so each has a plain
spelling too: `!` opens the error detail wherever you aren't typing, and
`:error`, `:notify` and `:tap` do what `F2`, `F3` and `F4` do. `]` / `[` (next /
previous tab) and `1`..`9` (jump to tab) work from the grid and every panel
but not while typing; `Ctrl-]` (next tab) and `Ctrl-T` (new tab) work from
the editor too; `Ctrl-W` closes the current tab from any mode *except* while
typing (editor, filters, search prompts — where `Ctrl-W` would collide with
`\watch` or word-delete); a tab with SQL in it takes a second `Ctrl-W`. `Ctrl-Tab` / `Ctrl-Shift-Tab`, `Alt-N` / `Alt-P`
and `Alt-1`..`Alt-9` do the same where the terminal delivers them. `z` / `Z`
(zoom the results / the editor) and `+` / `-` / `0` (editor size) work from
the grid; `Alt-Z` and `Alt-=` / `Alt--` / `Alt-0` do the same from the grid,
the editor and every panel where the terminal delivers them, but not from a
prompt, filter or the command bar. `A` (about) works from the grid (Normal
mode); `:about` reaches it from anywhere.

## Connection picker

Shown at startup when more than one data source was discovered, and via `c`
/ `\c` mid-session.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Move selection |
| `g` / `Home` | First |
| `G` / `End` | Last |
| `Enter` | Connect to focused entry |
| `q` | Quit (`Esc` is a no-op — a reflex press can't abandon the picker) |

On a failed connection (Normal mode, "connection failed" screen): `r` retries
the same DSN; `p` re-opens the picker (shown when there are candidates to pick).
A connection that drops while pgman is running — the server ended the
session (`pg_terminate_backend`, a restart, a standby's recovery conflict,
an idle timeout) or the network went — lands on the same screen, its error
`connection lost · ` and the server's own message, and the same `r`
reconnects.

## Start card / grid (Normal mode)

The landing view after connecting (empty "start card") and the result grid
once a query has run share one key map.

| Key | Action |
|---|---|
| `q` | Quit (`Ctrl-C` twice also quits, except from the editor — and from the results while a query or `M` fetch runs — where it cancels the running query) |
| `?` / `F1` | Toggle help (both work from every non-typing mode) |
| `:` | Open the command bar — see [Command bar](#command-bar-) |
| `A` | About pgman |
| `e` / `i` / `Tab` | Focus editor |
| `c` | Change connection (opens the picker mid-session) |
| `S` | Schema browser |
| `W` | Schema wizard: lint and config advisor |
| `Q` | Saved queries |
| `T` | Slow queries (`pg_stat_statements`) |
| `L` | Active sessions + locks |
| `E` | Environments: every database and replica, no connection needed; `Enter` connects, `d` discovers from AWS ([Environments](environments.md)) |
| `Esc` | Back to the environments overview, when environments are configured |
| `F3` / `:notify` | NOTIFY arrivals panel (also reachable from any mode) |
| `!` / `F2` / `:error` | The last error in full |
| `]` / `[` / `1`..`9` / `Ctrl-T` | Next / previous tab, jump to tab N, new tab — see [Tabs](#tabs) |
| `z` | Zoom the results to the whole body; `z` again restores the split exactly as it was |
| `Z` | Zoom the editor and go there; `Esc` then `z` restores the split |
| `+` (or `=`) / `-` | Grow / shrink the editor by one line (the results keep at least one row) |
| `0` | Back to the automatic editor/results split |
| `Alt-Z`, `Alt-=` / `Alt--`, `Alt-0` | The same zoom and size, where the terminal delivers Alt chords |
| `j` / `k` / `↓` / `↑` | Move selection |
| `h` / `l` / `←` / `→` | Move column cursor |
| `g` / `Home` | First row |
| `G` / `End` | Last row |
| `Enter` | Open row detail |
| `m` then `a`–`z` | Set bookmark at focused (row, col) |
| `'` then `a`–`z` | Jump to bookmark |

See [Result grid](#result-grid) for sort / filter / find / yank / FK-follow /
diff, all of which are also dispatched from this mode.

## Editor

| Key | Action |
|---|---|
| `Enter` | Run the statement once it ends with `;` (or is a `\` command) — through safety guards; otherwise insert a newline |
| `Alt-Enter` | Run the statement regardless of the terminator (`F5` / `Ctrl-Enter` / `Ctrl-J` do the same) |
| `Ctrl-E` / `F6` | EXPLAIN (never executes; opens the EXPLAIN tree) |
| `Ctrl-A` / `F7` | EXPLAIN ANALYZE (DML wrapped in a rollback transaction) |
| `Ctrl-C` | Cancel the running query (only while one is in-flight) |
| `Ctrl-Z` | Undo |
| `Ctrl-Y` / `Ctrl-Shift-Z` | Redo |
| `Ctrl-R` | Reverse-incremental history search — see [History search](#history-search) |
| `Ctrl-W` | `\watch` — re-run the buffer (or last history entry) every 2s; a `WATCH` badge shows while it runs; any key stops |
| `Ctrl-X` | Open the buffer in `$EDITOR` (suspends the TUI, resumes on save) |
| `Ctrl-S` | Save the current buffer as a named saved query |
| `Ctrl-O` | Open the saved-queries panel |
| `Ctrl-F` | Format the buffer (built-in; `pg_format` if installed) |
| `Ctrl-L` / `F8` | Parse the buffer as a log → pick a reconstructed query. From the results or the start card too: with a log in the editor it reconstructs, otherwise it opens the editor to paste one. With no log in the buffer (say, after loading a pick replaced it) it reopens the last log's picker |
| `Ctrl-D` / `F9` | Read the buffer as a DBUnit fixture path → load the apply script |
| `Ctrl-/` (or `Ctrl-_`) | Toggle a `-- ` line comment on the current line |
| `Ctrl-P` / `Ctrl-N` | Previous / next history entry (history persists across restarts) |
| `Ctrl-U` | Clear the buffer |
| `( [ {` | Autoclose — inserts the matching close, cursor between |
| `) ] }` | Skips over a matching close immediately ahead of the cursor |
| `'` / `"` | Autoclose/skip, gated so it doesn't fight SQL `''` escaping or in-word apostrophes |
| `Tab` / `Ctrl-Space` | Identifier completion — opens/cycles the popup; while the popup is on screen, `Enter` accepts the highlighted (or first) candidate |
| `.` after an identifier | Auto-triggers qualified completion (`users.|`) |
| `Shift-Enter` | Insert a newline even after a `;` (where the terminal distinguishes it) |
| `↑ ↓ ← →` | Move cursor (column remembered across up/down) |
| `Home` / `End` | Start / end of current line |
| `Ctrl-]` | Next tab (`Ctrl-Tab` / `Alt-N` too, where the terminal delivers them) |
| `Alt-Z` | Zoom the editor to the whole body; `Alt-Z` again (or `Esc` then `z`) restores the split exactly as it was — where the terminal delivers Alt; from the grid, `Z` does the same |
| `Alt-=` (or `Alt-+`) / `Alt--` | Grow / shrink the editor by one line (the results keep at least one row) — from the grid, `+` / `-` |
| `Alt-0` | Back to the automatic editor/results split — from the grid, `0` |
| `Esc` | Back to grid (or, with a completion popup open, abandon the popup and restore the typed prefix) |

Typing a space right after `FROM`, `JOIN`, `INNER`, `LEFT`, `RIGHT`, `FULL`,
`CROSS`, `INTO`, `WHERE`, `AND`, `OR` or `ON` also auto-triggers completion.
Right after `JOIN … ON` the popup leads with the whole predicate from each
foreign key between the table just joined and the ones before it, in either
direction and named the way the query names them (`users u JOIN orders o ON`
offers `o.user_id = u.id`); typing the alias narrows to its own. A
multi-column key offers one predicate per column pair.

A newline inherits the previous line's indentation, plus one level (`[editor]
indent`, default 2) when that line ends with `(`, `,` or a clause keyword such
as `SELECT`, `FROM`, `WHERE`, `AND`, `JOIN`, `ON` or `ORDER BY`; a line ending
in `;` starts the next at column 0, `Backspace` inside the indentation removes
a level and `Tab` there adds one.

### Completion popup

While the popup is open (no candidate committed yet): typing, `Backspace`
and `Delete` re-narrow the candidate list live; a space ends the word and
closes the popup, so an alias typed after a table name is never completed.
`Esc` restores the originally-typed prefix and closes the popup. Repeated
`Tab` cycles through candidates. `Enter` accepts the highlighted (or first)
candidate — but only from a popup that is on screen, and not when nothing
is highlighted and either the word is already a whole candidate (then it is
a newline) or the statement already ends in `;` (then it runs). Any
other key closes the popup and leaves the buffer as it is.

## Result grid

Sort, filter, find, yank, FK-follow and diff are dispatched from Normal
mode on the grid; row/cell detail and the diff view are their own modes.

A result arrives **1000 rows** at a time. A plain `SELECT` run from the
editor reads through a server-side cursor: when it has more rows, the
title says `partial · M fetches more`, and `M` appends the next 1000
(through to the end of the result, or pgman's ceiling of 100 000 rows
held). Sort, filter, find, the row detail, `Y`, `I`, `:fixture` and the
diff work on the rows fetched so far; `Y` and `:fixture` say `fetched
rows only` when there are more. A fetched page keeps the active sort
and filter. `Ctrl-C` cancels a fetch that is taking too long, keeping
the rows already there.

While a partial result's cursor is open, the session is **idle in
transaction**, holding an `ACCESS SHARE` lock on every table the query
reads. Reads and writes go on around it, but DDL on those tables — a
migration, `ALTER TABLE`, `TRUNCATE`, `VACUUM FULL`, `REINDEX` — waits
for the lock, and every later statement on those tables queues behind
that waiting DDL. The open transaction also holds back vacuum. So pgman
closes the cursor after 60 s without a fetch, or 10 min after the query
ran (sooner when the server's `idle_in_transaction_session_timeout` or
`transaction_timeout` is shorter), when the last row is fetched, and as
soon as anything else talks to the database: the next query, a panel
such as `L` or `T`, a reconnect, closing the tab. On a standby no cursor
is held at all (below). The title then says `cursor closed · M
re-runs`, and `M` runs the query again for the rows already held plus
1000 more, as a fresh snapshot.

On a standby (the header's `replica` badge) pgman never holds the cursor
open: a standby replaying the primary's DDL or `VACUUM` cancels a
session that is in its way, and after `max_standby_streaming_delay` (30 s
by default, less on an Aurora reader) it ends the whole connection. So
there the first 1000 rows arrive and the transaction commits at once;
the title says `partial · on a standby · M re-runs for more`, and each
`M` runs the query again for the rows held plus 1000.

Everything else stops at 1000 rows with `capped at 1000 · more rows not
fetched` in the title: `EXPLAIN`, scripts of several statements,
`\watch`, `SELECT … FOR UPDATE` / `FOR SHARE` (a cursor would hold the
locks), a `SELECT` that calls a function pgman does not know to be
read-only, and a `SELECT` run while a transaction is open (`in a
transaction, so no paging`). The functions a paged `SELECT` may call are
a short list of built-ins that never write — `count`, `sum`, `min`,
`max`, `avg`, `coalesce`, `lower`, `upper`, `length`, `now`,
`date_trunc`, `generate_series` and the like; anything else (your own
function, `nextval`, `random`, a name in any schema but `pg_catalog`)
runs the old way, every row computed and committed at once, because
behind a cursor a function runs only for the rows fetched and its writes
stay uncommitted until the cursor closes. pgman cannot see a function a
view calls: a `SELECT` from a view whose definition writes still pages. For every row, run it outside the TUI with
`pgman query 'SQL' --json` (or `--batch --format json`), which isn't
capped; see [commands](commands.md). How the cursor works is in
[docs/backlog/paging.md](backlog/paging.md).

A NULL is drawn as a dimmed, italic `NULL` (`theme.null_cell`) and an
empty string as nothing, so the two never look alike; row and cell
detail say `NULL` and `(empty string)`. Sort puts NULLs where Postgres
does by default — last ascending, first descending — and an empty
string sorts as the smallest value. Filter and find match a NULL on the
whole word `null` (any case), so `/null` finds them; `/nu` does not.

| Key | Action |
|---|---|
| `s` | Cycle sort on the focused column (off → ASC → DESC) |
| `/` | Live row filter (hides non-matching rows) |
| `f` | Find within the grid (highlights + jumps; `n`/`N` step matches) |
| `n` / `N` | Step to next / previous filter or find match |
| `F` | Follow the FK on the focused cell → new tab, `SELECT * FROM parent WHERE pk=value` (a NULL references nothing: no tab) |
| `Y` | Copy the (filtered) grid to clipboard as CSV (NULL an empty field, an empty string `""`) |
| `I` | Yank the focused row as an `INSERT` (single-table SELECTs only): NULL as `NULL`, every value quoted, `''` included |
| `M` | Fetch the next 1000 rows of a partial result; once its cursor has closed, re-run the query for them (see above) |
| `u` | Edit the focused cell — a one-row `UPDATE` through the safety guard; see [Cell edit](#cell-edit-u) |
| `D` | Pin the current result as baseline A; press again after running another query to diff B against A |
| `Enter` | Open row detail |

### Filter / Find text entry

`GridFilter` (`/`) and `GridFind` (`f`) share one shape: type to
narrow/search live, `Backspace` edits, `Enter` accepts (find keeps stepping
with `n`/`N` from Normal mode afterward), `Esc` clears and cancels. While
typing a find, `n` and `N` are letters of the pattern; `↓`/`↑` (or
`Ctrl-N`/`Ctrl-P`) step through the matches.

### Row detail (`Enter` on a row)

| Key | Action |
|---|---|
| `J` / `K` (or `PageDown` / `PageUp`) | Next / previous row of the (filtered, sorted) grid, keeping the highlighted field; the grid selection follows, so closing lands on that row |
| `j` / `k` / `↓` / `↑` | Move to next / previous field |
| `g` / `G` | First / last field |
| `Enter` | Zoom into the focused field (cell detail) |
| `y` | Yank the focused field value (a NULL yanks nothing, and says so) |
| `u` | Edit the focused field — see [Cell edit](#cell-edit-u) |
| `Esc` / `q` | Close |

### Cell detail (`Enter` on a field)

Text cells:

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Scroll |
| `g` / `G` | Top / bottom |
| `PageUp` / `PageDown` | Scroll by 10 |
| `y` | Yank the value |
| `Esc` / `Enter` / `q` | Back to row detail |

JSON cells (value parses as an object/array):

| Key | Action |
|---|---|
| `j` / `k` | Navigate the tree |
| `Enter` / `Space` / `h` / `l` | Expand / collapse the focused container |
| `y` | Yank the jq-style path (`.foo[0].bar`) |
| `Esc` / `q` | Back to row detail |

### Cell edit (`u`)

`u` on a cell in the grid (or on a field in row detail) opens a one-line
editor holding its value. It works on a result the editor ran from **one
table** whose **primary key is in the result**; anything else says why
not (a join, a view, no primary key, the key column not selected, a
computed column such as `upper(name)`, a read-only session, a
transaction awaiting commit or rollback). A column is the table's only if
Postgres says so, so `SELECT id + 1 AS id …` is not keyed on `id`.

| Key | Action |
|---|---|
| `Enter` | Save: `UPDATE schema.table SET col = <value> WHERE <key> = … RETURNING col`, through the safety guard as any editor statement — confirmed (the modal shows the statement), allowed or blocked per database |
| `Ctrl-N` | NULL ↔ empty string: a NULL shows as the grid's dimmed `NULL`; `Ctrl-N` on it makes `''`, and typing starts a text value |
| `Esc` | Cancel |

Text editing is the single-line prompt's: `←` / `→`, `Home` / `End`
(`Ctrl-A` / `Ctrl-E`), `Backspace`, `Delete`, `Ctrl-W`, paste. A value
with a newline or another control character can't be edited here (nor
pasted in); write that UPDATE in the editor.

The UPDATE changes **one row or nothing**: any other count (the row
was deleted since the query ran; an inheritance child shares the key)
is rolled back and the footer says so. So is an edit whose table,
column or key the server no longer calls what pgman's schema cache
does — a column renamed since pgman read the schema, with another
added under its old name, would otherwise take the write: the footer
says `the table changed since pgman read it`, and `S` then `r`
reloads the schema. Inside a transaction you opened
with `BEGIN`, only the edit is undone, under a savepoint. On success
the cell shows the value as the server stored it (`1.5` into a
`numeric(10,2)` reads `1.50`); with `auto_tx` on, the commit / rollback
prompt follows, and a rollback puts the old value back. The UPDATE goes
into the history.

### Result diff (`D` twice)

When A and B are the same columns read from the same table, and its
primary key is in both, rows are matched on the primary key (the title
says `key: id (primary key)`); otherwise on the leftmost column unique
in both, then on the whole row.

| Key | Action |
|---|---|
| `j` / `k` / `g` / `G` / `PageUp` / `PageDown` | Navigate diff rows |
| `r` | Re-pin the current B side as the new baseline A |
| `c` | Clear the pinned baseline |
| `Esc` / `q` | Close |

## Schema browser

Opened with `S`, or `\d [name]` / `\dt` / `\dn`. A database with a
single schema opens with it expanded, on its first table. Partitions
don't list beside their table: an expanded partitioned table shows one
`N partitions` row, sub-partitions included, which expands to list them.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate schemas / tables / columns / constraints / indexes (an index shows its kind and its full `CREATE INDEX` definition) |
| `Enter` / `Space` / `←` / `→` | Expand / collapse the focused schema, table or partitions group |
| `[` / `]` | Jump to previous / next schema (skips past table internals) |
| `+` / `=` | Expand all (cursor stays put) |
| `-` / `_` | Collapse all |
| `PageUp` / `PageDown` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `/` | In-tree filter (live; ancestors of a match stay visible) |
| `s` | Yank `SELECT * FROM <schema>.<table> LIMIT 100` template |
| `i` | Yank `INSERT INTO … (cols) VALUES (NULL, …)` template |
| `p` | Preview: run `SELECT * FROM <schema>.<table> LIMIT 5` and close onto the grid it lands in (a partition previews itself) |
| `d` | Copy the focused table's (or view's) `CREATE` statement, rebuilt from the catalog: columns with defaults, identity and generated expressions (`serial` where the column owns its sequence), its own primary / unique / check / exclusion / foreign keys, `PARTITION BY` / `PARTITION OF`, then a `CREATE INDEX` per index no constraint owns. Not a `pg_dump`: owners, grants, comments and triggers are left out |
| `r` | Reload the schema — picks up tables other sessions made since you connected (`S`, `\d` and `W` also reload by themselves when the cache is empty while connected) |
| `Esc` / `q` | Close (also drops the in-tree filter) |

## Schema wizard

Opened with `W`. Pure checks (LINT001–004: missing PK, mixed-case, reserved
word, mixed naming) run over the cached schema; live checks (LINT101–107: FK
without index, unused index, duplicate indexes, bloat, no comment, mixed
timestamp/timestamptz, a serial or identity column nothing keeps unique) query the database.

The **config advisor** findings (ADV…) join the same list. They come from the
server's statistics, read-only, and each carries the numbers that triggered it
and how long they have accumulated (since `stats_reset`, or "at least since the
server started"), the query that produced them, a conservative setting to change
and why, and what that setting is called in an RDS / Aurora parameter group —
static (applies at reboot) or dynamic, and how Aurora differs (on Aurora, a
parameter in both the cluster and the instance parameter group can be
overridden by the instance's; one AWS's Aurora tables don't list, such as
`lock_timeout`, says to set it per role or session and check your group). On
RDS and Aurora a server setting's suggestion is the
`aws rds modify-db-(cluster-)parameter-group` command; self-hosted,
`ALTER SYSTEM`. pgman never runs it.

| Code | Finding | Read from |
|---|---|---|
| ADV000 | The advisor itself is limited: statistics under an hour old (the ratio checks wait), or a standby | — |
| ADV001 | Buffer cache hit ratio under 0.99 (MED under 0.95), with ≥ 100 000 block requests → `shared_buffers` | `pg_stat_database` |
| ADV002 | ≥ 64 MiB spilled to temp files (MED from 256 MiB an hour) → `work_mem` for the spilling role, with the statements when `pg_stat_statements` is installed | `pg_stat_database`, `pg_stat_statements` |
| ADV003 | Dead tuples ≥ 10 % of live on a table of ≥ 10 000 rows, blamed on autovacuum being off, behind, or its scale factor → `autovacuum_vacuum_scale_factor` on the table | `pg_stat_user_tables` |
| ADV004 | Sessions waiting ≥ 5 s on a lock (HIGH from 60 s or 5 waiters), with the blocking chain → `idle_in_transaction_session_timeout` when the head is idle in a transaction (a timeout over an hour, as AWS's 24 h default, counts as off); otherwise `log_lock_waits`, then `lock_timeout` for the migration / DDL session only — never per role for the application, whose every statement waiting longer would fail | `pg_stat_activity`, `pg_blocking_pids` |
| ADV005 | Over 20 % of checkpoints requested rather than timed (not on Aurora). From PG14 the WAL written says whether they were forced by WAL volume (MED → `max_wal_size`) or asked for by something else — CREATE / DROP DATABASE, a base backup, a manual `CHECKPOINT`, a shutdown (LOW, no setting); before PG14 it hedges | `pg_stat_bgwriter`, `pg_stat_checkpointer` from PG17, `pg_stat_wal` |

Thresholds and their reasoning: `docs/backlog/config-advisor.md`. Without
`pg_read_all_stats` (in `pg_monitor`) other sessions' state and queries are
hidden, and ADV004 says so; without `pg_stat_statements`, ADV002 says how to
install it.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate findings (sorted HIGH → MED → LOW) |
| `g` / `G` | First / last |
| `PageUp` / `PageDown` | Jump 10 rows |
| `y` | Yank the focused finding's SQL suggestion (if any) |
| `e` | Yank an ADV finding's evidence query, to run in the editor |
| `r` | Refresh (re-runs every check) |
| `Esc` / `q` | Close |

## Slow queries

Opened with `T`. Top-N by total execution time from `pg_stat_statements`.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate |
| `g` / `G` | First / last |
| `Enter` | Copy the focused SQL into the editor |
| `r` | Refresh |
| `R` | Toggle auto-refresh (5s polling) |
| `Esc` / `q` | Close |

## Sessions

Opened with `L`. `pg_stat_activity`; blocked sessions sort to the top in red.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate |
| `g` / `G` | First / last |
| `K` | Terminate the focused session (`pg_terminate_backend`, confirm first) |
| `r` | Refresh |
| `R` | Toggle auto-refresh (5s polling) |
| `Esc` / `q` | Close |

The terminate confirm prompt: `y` / `Y` confirms, `n` / `N` / `Esc` cancels.

## EXPLAIN tree

Opened via `Ctrl-E` / `F6` (EXPLAIN) or `Ctrl-A` / `F7` (EXPLAIN ANALYZE).

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate plan nodes (hottest node highlighted red) |
| `g` / `G` | Jump to root / last visible node |
| `Enter` / `Space` | Expand / collapse the focused subtree (no-op on leaf nodes) |
| `Esc` / `q` | Close |

## Saved queries

Opened with `Q`, or `Ctrl-O` from the editor.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate |
| `g` / `G` | First / last |
| `Enter` | Load the focused query (prompts for `:param` values first, if any) |
| `r` | Rename the focused entry |
| `d` | Delete the focused entry (no confirm — the file is rewritten immediately) |
| `/` | Filter the list by name/body (live) |
| `Esc` / `q` | Close (drops any active filter) |

From the editor, `Ctrl-S` prompts for a name and saves the current buffer.
The prompt opens with a name suggested from the SQL, shown selected: typing
replaces it, `Backspace` clears it, `→` / `End` keep it to edit, and `Enter`
saves it as it is. A saved query with `:param` placeholders prompts for each value in turn
(`Enter` accepts and moves to the next; `Esc` cancels back to the list).

## History search

Entered via `Ctrl-R` from the editor. Reverse-incremental, like readline/psql.

| Key | Action |
|---|---|
| Typing / `Backspace` | Edit the search query, re-search from the latest match |
| `Ctrl-R` | Jump to the next-older match |
| `Ctrl-D` | Delete the currently-matched history entry, then re-step |
| `Enter` | Accept — stays in the editor with the matched buffer |
| `Esc` | Cancel — restores the buffer as it was before the search started |

## Tabs

The tab bar sits under the header once connected — one row naming each
tab by the first line of its buffer (`empty` until something is typed),
the active tab highlighted, `[ ] prev/next tab · ctrl-t new · ctrl-w close` on
the right (whole pieces drop from the right when the tabs need the room).
It hides on a very short terminal (fewer than 12 body rows) while there is
only one tab.

`]` / `[` and the digits work from the grid and every panel, but not while
typing (the editor, filters, prompts) — from the editor, `Ctrl-]` steps to
the next tab. The schema browser keeps `[` / `]` for its own schema jump;
the digits still switch tabs there.

| Key | Action |
|---|---|
| `]` / `[` | Next / previous tab (wraps) |
| `1` .. `9` | Jump directly to tab N (a digit past the last tab says how many are open) |
| `Ctrl-]` | Next tab — also from the editor, where `]` is a character |
| `Ctrl-T` | Open a new tab (fresh editor + result) |
| `Ctrl-W` | Close the current tab (no-op on the last one; press twice when it holds SQL) |
| `Ctrl-Tab` / `Ctrl-Shift-Tab` | Next / previous tab, where the terminal delivers them |
| `Alt-N` / `Alt-P`, `Alt-1` .. `Alt-9` | The same, where the terminal delivers Alt chords (iTerm on a Mac needs the option key set to `Esc+`) |

Connection, schema cache, history and saved queries are shared across tabs.
The editor/results split (`+` / `-` / `0`) and the zoom (`z` / `Z`) are
per tab: switching tabs restores each tab's own layout.

## JDBC tap monitor

Opened with `F4` from any mode. Seven views, cycled with `v`: **List**
(recency-ordered event stream, default; the focused query's bound
parameters show beneath it, labelled `params (some redacted)` when the
producer masked any; `Enter` opens the whole event, as the strip's rule says) → **Hotspots** (by SQL fingerprint)
→ **Callers** (by innermost caller frame) → **Transactions** (by synthetic
txn id) → **Pools** (by connection-pool name) → **N+1** (live N+1 detector)
→ **Baseline** (diff vs. a captured snapshot) → back to List.

| Key | Action |
|---|---|
| `v` | Cycle view |
| `Shift-B` | Capture the current hotspots as a baseline (works from every view) |
| `j` / `k` / `↓` / `↑` | Navigate the current view's rows |
| `g` / `G` | First / last |
| `PageUp` / `PageDown` | Page |
| `Enter` | List view: open the focused event (below) |
| `s` | Cycle sort — Hotspots and Callers views only (Total time / Call count / P95 latency) |
| `c` | Clear the event ring |
| `Esc` / `q` | Close |

`Enter` on a List row opens the whole event over the list: its kind and
start time (UTC), duration and rows, `app` / `pool` / `conn` / `txn`, the
full SQL, every bound parameter one per line, the error's cause chain
(`error`, then `caused by`, the last one `root cause`) and the caller
stack, innermost frame first. A field the producer didn't send reads `—`.
It shows the event as it was when opened; new arrivals don't change it.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Scroll a line |
| `PageUp` / `PageDown` | Scroll 10 lines |
| `g` / `G` | Top / bottom |
| `Esc` / `q` / `Enter` | Back to the list |

## Command bar (`:`)

`:` opens a one-line prompt in the footer from any mode that isn't taking
literal text (so a colon still types in the editor, in filters, and in the
bar itself). See [docs/commands.md](commands.md) for what each command does.

| Key | Action |
|---|---|
| `Enter` | Run the command |
| `Tab` | Complete the command name (unique name fills in; several are listed); after `:help `, the section name |
| `Esc` | Cancel — returns to the mode you opened it from |
| `←` / `→` / `Home` / `End` | Move the cursor |
| `Backspace` / `Ctrl-W` | Delete char / word |

## Overlays

### Help (`F1` / `?`)

Help is a list of sections and one section at a time. From 110 columns
the list sits in a left column beside the section; on a narrower
terminal the list comes first, each section with a one-line summary,
and `Enter` opens one full width. Opened from a mode, help shows that
mode's section; opened from the grid, the list.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Scroll the section (in the narrow list: move the selection) |
| `Tab` / `]` | Next section |
| `Shift-Tab` / `[` | Previous section |
| `g` / `G` | Top / bottom (in the narrow list: first / last section) |
| `PageUp` / `PageDown` | Scroll by 10 |
| `Enter` | Open the selected section (narrow list) |
| `/` | Filter every section live: matching rows under their section headings; `Enter` keeps the filter, `Esc` clears it. With a filter up, `Tab` / `]` / `[` jump from section to section |
| `Esc` | Clear the filter; else back to the list (narrow terminals); else close |
| `?` / `q` / `F1` | Close — returns to the mode you opened help from |

Open it at a section with `:help <topic>`: a section's name, or the start
of one (`:help sch` is the schema browser; `Tab` completes the names) — the earlier one-word topics
(`schema`, `saved`, `tap`, `wizard`, …) still work. `:help all` shows every
section on one scrolling page, as help was before it had sections.

### About (`A`)

| Key | Action |
|---|---|
| `Esc` / `Enter` / `q` / `A` | Close |

### Error detail (`F2` / `!` / `:error`)

Expands the most recent query failure (severity / code / detail / hint /
affected schema, table, column, constraint). No-op if there's nothing to
show.

| Key | Action |
|---|---|
| `Esc` / `q` / `!` / `F2` | Close |

### Notifications (`F3`)

The `LISTEN`/`NOTIFY` arrivals panel — subscribe with `LISTEN <channel>` in
the editor.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate |
| `g` / `G` / `Home` / `End` | First / last |
| `PageUp` / `PageDown` | Page |
| `y` | Yank the focused payload |
| `c` | Clear the ring |
| `Esc` / `q` | Close |
