# Keys

Press `F1` or `?` for the in-app cheat sheet — it auto-scrolls to the section
for whatever mode you opened it from. `q` closes most panels and quits from
the grid / connection picker; `Esc` closes overlays but is a no-op in Normal
mode (a reflex press can't drop the session). `F1` (help), `F2` (error
detail), `F3` (notifications) and `F4` (JDBC tap monitor) work from **any**
mode, as do `?` (help) and `:` (the command bar) — except while a text input
has focus, where both are characters you meant to type. `Ctrl-T` (new tab),
`Ctrl-Tab` / `Ctrl-Shift-Tab` (cycle tabs) and `Alt-1`..`Alt-9` (jump to tab)
are also global; `Ctrl-W` closes the current tab from any mode *except* while
typing (editor, filters, search prompts — where `Ctrl-W` would collide with
`\watch` or word-delete). `A` (about) works from the grid (Normal mode);
`:about` reaches it from anywhere.

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
the same DSN; `p` re-opens the picker (only shown when ≥2 candidates exist).

## Start card / grid (Normal mode)

The landing view after connecting (empty "start card") and the result grid
once a query has run share one key map.

| Key | Action |
|---|---|
| `q` | Quit (`Ctrl-C` also quits, except mid-query in the editor) |
| `?` / `F1` | Toggle help (both work from every non-typing mode) |
| `:` | Open the command bar — see [Command bar](#command-bar-) |
| `A` | About pgman |
| `e` / `i` / `Tab` | Focus editor |
| `c` | Change connection (opens the picker mid-session) |
| `S` | Schema browser |
| `W` | Schema wizard / lint |
| `Q` | Saved queries |
| `T` | Slow queries (`pg_stat_statements`) |
| `L` | Active sessions + locks |
| `F3` | NOTIFY arrivals panel (also reachable from any mode) |
| `Ctrl-T` / `Ctrl-Tab` / `Alt-1..9` | Tabs — see [Tabs](#tabs) |
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
| `F5` / `Ctrl-Enter` / `Ctrl-J` | Run the statement (through safety guards) |
| `Ctrl-E` / `F6` | EXPLAIN (never executes; opens the EXPLAIN tree) |
| `Ctrl-A` / `F7` | EXPLAIN ANALYZE (DML wrapped in a rollback transaction) |
| `Ctrl-C` | Cancel the running query (only while one is in-flight) |
| `Ctrl-Z` | Undo |
| `Ctrl-Y` / `Ctrl-Shift-Z` | Redo |
| `Ctrl-R` | Reverse-incremental history search — see [History search](#history-search) |
| `Ctrl-W` | `\watch` — re-run the buffer (or last history entry) every 2s; any key stops |
| `Ctrl-X` | Open the buffer in `$EDITOR` (suspends the TUI, resumes on save) |
| `Ctrl-S` | Save the current buffer as a named saved query |
| `Ctrl-O` | Open the saved-queries panel |
| `Ctrl-F` | Pretty-print the buffer via `pg_format` (requires it on `PATH`) |
| `Ctrl-L` / `F8` | Parse the buffer as a log → pick a reconstructed query |
| `Ctrl-D` / `F9` | Read the buffer as a DBUnit fixture path → load the apply script |
| `Ctrl-/` (or `Ctrl-_`) | Toggle a `-- ` line comment on the current line |
| `Ctrl-P` / `Ctrl-N` | Previous / next history entry (history persists across restarts) |
| `Ctrl-U` | Clear the buffer |
| `( [ {` | Autoclose — inserts the matching close, cursor between |
| `) ] }` | Skips over a matching close immediately ahead of the cursor |
| `'` / `"` | Autoclose/skip, gated so it doesn't fight SQL `''` escaping or in-word apostrophes |
| `Tab` / `Ctrl-Space` | Identifier completion — opens/cycles the popup |
| `.` after an identifier | Auto-triggers qualified completion (`users.|`) |
| `Enter` | Insert newline |
| `↑ ↓ ← →` | Move cursor (column remembered across up/down) |
| `Home` / `End` | Start / end of current line |
| `Esc` | Back to grid (or, with a completion popup open, abandon the popup and restore the typed prefix) |

Typing a space right after `FROM`, `JOIN`, `INNER`, `LEFT`, `RIGHT`, `FULL`,
`CROSS`, `INTO`, `WHERE`, `AND`, `OR` or `ON` also auto-triggers completion.

### Completion popup

While the popup is open (no candidate committed yet): typing, `Backspace`
and `Delete` re-narrow the candidate list live; `Esc` restores the
originally-typed prefix and closes the popup. Any other key commits the
current candidate. Repeated `Tab` cycles through candidates.

## Result grid

Sort, filter, find, yank, FK-follow and diff are dispatched from Normal
mode on the grid; row/cell detail and the diff view are their own modes.

| Key | Action |
|---|---|
| `s` | Cycle sort on the focused column (off → ASC → DESC) |
| `/` | Live row filter (hides non-matching rows) |
| `f` | Find within the grid (highlights + jumps; `n`/`N` step matches) |
| `n` / `N` | Step to next / previous filter or find match |
| `F` | Follow the FK on the focused cell → new tab, `SELECT * FROM parent WHERE pk=value` |
| `Y` | Copy the (filtered) grid to clipboard as CSV |
| `I` | Yank the focused row as an `INSERT` (single-table SELECTs only) |
| `D` | Pin the current result as baseline A; press again after running another query to diff B against A |
| `Enter` | Open row detail |

### Filter / Find text entry

`GridFilter` (`/`) and `GridFind` (`f`) share one shape: type to
narrow/search live, `Backspace` edits, `Enter` accepts (find keeps stepping
with `n`/`N` from Normal mode afterward), `Esc` clears and cancels.

### Row detail (`Enter` on a row)

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Move to next / previous field |
| `g` / `G` | First / last field |
| `PageUp` / `PageDown` | Jump 10 fields |
| `Enter` | Zoom into the focused field (cell detail) |
| `y` | Yank the focused field value |
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

### Result diff (`D` twice)

| Key | Action |
|---|---|
| `j` / `k` / `g` / `G` / `PageUp` / `PageDown` | Navigate diff rows |
| `r` | Re-pin the current B side as the new baseline A |
| `c` | Clear the pinned baseline |
| `Esc` / `q` | Close |

## Schema browser

Opened with `S`, or `\d [name]` / `\dt` / `\dn`.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate schemas / tables / columns / constraints |
| `Enter` / `Space` / `←` / `→` | Expand / collapse the focused schema or table |
| `[` / `]` | Jump to previous / next schema (skips past table internals) |
| `+` / `=` | Expand all (cursor stays put) |
| `-` / `_` | Collapse all |
| `PageUp` / `PageDown` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `/` | In-tree filter (live; ancestors of a match stay visible) |
| `s` | Yank `SELECT * FROM <schema>.<table> LIMIT 100` template |
| `i` | Yank `INSERT INTO … (cols) VALUES (NULL, …)` template |
| `Esc` / `q` | Close (also drops the in-tree filter) |

## Schema wizard

Opened with `W`. Pure checks (LINT001–004: missing PK, mixed-case, reserved
word, mixed naming) run over the cached schema; live checks (LINT101–106: FK
without index, unused index, duplicate indexes, bloat, no comment, mixed
timestamp/timestamptz) query the database.

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Navigate findings (sorted HIGH → MED → LOW) |
| `g` / `G` | First / last |
| `PageUp` / `PageDown` | Jump 10 rows |
| `y` | Yank the focused finding's SQL suggestion (if any) |
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
A saved query with `:param` placeholders prompts for each value in turn
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

Global except while typing (editor / filter / search prompts).

| Key | Action |
|---|---|
| `Ctrl-T` | Open a new tab (fresh editor + result) |
| `Ctrl-W` | Close the current tab (no-op on the last one) |
| `Ctrl-Tab` | Next tab |
| `Ctrl-Shift-Tab` | Previous tab |
| `Alt-1` .. `Alt-9` | Jump directly to tab N |

Connection, schema cache, history and saved queries are shared across tabs.

## JDBC tap monitor

Opened with `F4` from any mode. Seven views, cycled with `v`: **List**
(recency-ordered event stream, default) → **Hotspots** (by SQL fingerprint)
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
| `s` | Cycle sort — Hotspots and Callers views only (Total time / Call count / P95 latency) |
| `c` | Clear the event ring |
| `Esc` / `q` | Close |

## Command bar (`:`)

`:` opens a one-line prompt in the footer from any mode that isn't taking
literal text (so a colon still types in the editor, in filters, and in the
bar itself). See [docs/commands.md](commands.md) for what each command does.

| Key | Action |
|---|---|
| `Enter` | Run the command |
| `Tab` | Complete the command name (unique name fills in; several are listed) |
| `Esc` | Cancel — returns to the mode you opened it from |
| `←` / `→` / `Home` / `End` | Move the cursor |
| `Backspace` / `Ctrl-W` | Delete char / word |

## Overlays

### Help (`F1` / `?`)

| Key | Action |
|---|---|
| `j` / `k` / `↓` / `↑` | Scroll |
| `g` / `G` | Top / bottom |
| `PageUp` / `PageDown` | Scroll by 10 |
| `Esc` / `?` / `q` / `F1` | Close — returns to the mode you opened help from |

Open it at a named section with `:help <topic>` — `grid`, `editor`,
`commands`, `schema`, `saved`, `slow`, `sessions`, `tap`, `explain`, `diff`,
`wizard`.

### About (`A`)

| Key | Action |
|---|---|
| `Esc` / `Enter` / `q` / `A` | Close |

### Error detail (`F2`)

Expands the most recent query failure (severity / code / detail / hint /
affected schema, table, column, constraint). No-op if there's nothing to
show.

| Key | Action |
|---|---|
| `Esc` / `q` / `F2` | Close |

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
