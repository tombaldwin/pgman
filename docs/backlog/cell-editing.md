# Design note: cell editing and the primary-key-keyed result diff

Status: decided 2026-10-23, landed with the "Cell editing → UPDATE
generation" and "Result diff — PK refinement" backlog items. Both need
the same thing — the schema cache knowing each table's real primary
key — so they ship together.

## What the cache learns

`SchemaCache` gains:

- `primary_keys: HashMap<(schema, table), Vec<String>>`: the PK's
  columns in key order, from `pg_index.indisprimary` with
  `unnest(indkey) WITH ORDINALITY` (`PRIMARY_KEYS_SQL`, one more
  catalog query beside the others in `query/schema.rs`, same
  best-effort stance: a failure means no PK knowledge, never a dead
  cache).
- `relations_by_oid: HashMap<u32, (schema, name)>` and an `attnum` on
  every `ColumnMeta`: `SCHEMA_SQL` already walks `pg_class` and
  `pg_attribute`, and now selects `c.oid` and `a.attnum` too.

The second pair exists because of the next section.

## Which table a result column comes from

Name matching is not enough. `SELECT id + 1 AS id, name FROM users`
has a column called `id` that is not the key: an UPDATE keyed on it
writes to the *next* user. `SELECT name AS title FROM users` has a
real column under a name the table does not have. A CTE, a view or a
search_path that resolves `users` to another schema all break the
"FROM names one table, so its columns are that table's" shortcut too.

Postgres already answers the question. Every column in the
RowDescription a `prepare` returns carries the table OID and attribute
number it was read from, or zero for an expression
(`tokio_postgres::Column::table_oid` / `column_id`). The editor's run
path (`conn::run_statement_with_origins`) keeps them as
`grid::ColumnOrigin { table_oid, attnum }`, one `Option` per column,
and they ride on `GridView` beside `source`, so they travel with tabs
and are cleared with the grid.

The resolver (`query::cell_edit::resolve_key`, pure) then decides:

| check | refusal |
|---|---|
| no live connection / `--demo` | needs a live connection |
| `grid_view.source` is `None` (a join, a subquery, no FROM, a write) | not a single-table SELECT |
| no origins (a panel, EXPLAIN, a batch, the bootstrap grid) | only a result the editor ran can be edited |
| the edited column has no origin | `col` is computed, not a table column |
| the origin's table isn't in the cache | unknown table — `S` then `r` reloads the schema |
| the origin table's name differs from the FROM table | columns come from X, not the FROM table Y (a CTE / view name) |
| it is a view / matview / foreign table | `t` is a view — edit the table under it |
| no primary key | `t` has no primary key |
| a PK column has no plain origin column in the grid | PK column(s) `a, b` not in the result — select them |

The schema comes from the OID, not from the FROM clause: an unqualified
`FROM users` is `public.users` to `infer_single_source_table`, but the
server may have read `app.users` through the search_path; the OID is
what the server actually read. The SET column is the catalog's
attribute name for the origin's `attnum`, so an aliased column updates
the real one.

## Editing

**Key: `u`** (UPDATE) in the grid and in row detail, on the focused
cell / field. `Enter` keeps meaning "row detail" (and "cell detail"
from there); `e` / `i` already focus the editor. A plain key, as the
Mac/iTerm guidance asks for non-typing modes.

**Editor**: a popup (`Mode::CellEdit`, a text-input mode) holding a
`tui_common::TextInput` seeded with the current value, the target
(`public.users.name`) and the key (`where id = 3`) above it.

- `Enter` saves, `Esc` cancels.
- **`Ctrl-N` toggles NULL.** Set, the buffer is empty and drawn as the
  grid's dimmed `NULL` marker; typing a character leaves NULL and
  starts a text value. `Ctrl-N` on a NULL makes it the empty string.
  So NULL, `''` and text are three distinct, visible states.
- An unchanged value is a no-op ("no change"), not an UPDATE.
- A value holding a control character (a newline, a tab) is refused:
  the single-line widget can't show it faithfully. Edit it with an
  UPDATE in the editor (follow-up).

## The statement

```sql
UPDATE schema.table SET col = <literal> WHERE pk1 = <literal> AND pk2 = <literal> RETURNING col
```

Identifiers through `app::quote_ident`, values through
`app::format_sql_literal` (`NULL`, `''`, `E'…'` for backslashes; every
value quoted, which Postgres reads as the column's type). The `WHERE`
compares every PK column; PK values are never NULL.

## Safety

The statement goes through `safety::evaluate` exactly as an editor
statement: `Block` refuses with the guard's message, `Confirm` opens the
existing confirm modal with the generated SQL as its body, `Allow` runs.
A read-only session (profile, `:readonly on`, or a replica) refuses at
`u`, before anything is typed, with the same advice `:readonly off`
gives. An auto-tx transaction already awaiting commit/rollback refuses
too ("commit (y) or roll back (n) first").

## Never more than one row

`conn::run_single_row_update` (the only new `Db` code; in `conn.rs`):

1. `SAVEPOINT pgman_cell_edit`. If the session is outside a
   transaction this fails with `25P01` and we `BEGIN` instead; if the
   operator has their own `BEGIN` open (pgman doesn't track those), the
   savepoint keeps our rollback from taking their work with it.
2. Run the UPDATE over the simple-query protocol, counting rows.
3. Anything but exactly one (`cell_edit::check_one_row`): roll back to
   the savepoint / `ROLLBACK`, and say "matched N rows — rolled back,
   nothing changed". A PK makes more than one impossible on a plain
   table, but not across an inheritance tree; zero means the row was
   deleted or its key changed since the query ran.
4. One row: inside the operator's transaction, release the savepoint
   and say it is not committed yet; under `auto_tx`, leave our
   transaction open and hand over to the existing commit/rollback
   prompt; otherwise `COMMIT`.

## After it lands

The row is located again by its full old contents (the grid may have
been sorted, or the tab switched, while the UPDATE ran) and the cell
takes the value `RETURNING` sent back — the server's rendering, so
`1.5` typed into a `numeric(10,2)` shows `1.50`. The unsorted copy
behind sort (`raw_rows`) gets the same change. If the row isn't there
any more, the status says the database changed and to re-run. Under
`auto_tx` a later rollback puts the old value back.

## The result diff

`PinnedResult` keeps the source and origins it was pinned with. When A
and B have the same columns and both resolve (the same resolver, minus
the edited-column check) to the same table with every PK column
present, the diff keys on those columns and the header says
`key: id (primary key)`. Otherwise it falls back to today's
leftmost-unique-column heuristic (`key: id (unique)`) and then the full
row.

## Not in this round

- `--demo`: its grids carry no origins, so editing refuses there.
- Multi-line values (above).
- Editing several cells of a row as one UPDATE.
