# Design note: NULL cells in the result grid

Status: decided 2026-10-22, landed with the "NULL and the empty string
look the same" backlog item.

## The problem

`grid::Grid` held every cell as a `String`, and `conn::display_cells`
turned SQL NULL into `""` on the way in. From then on nothing could tell
a NULL from an empty string:

- the grid, row detail (`Enter`) and cell detail (`Enter` again) drew
  both as blank (row detail dimmed both, which read as "this is NULL"
  for an empty string too);
- the `I` INSERT yank (`format_sql_literal`) wrote `NULL` for an empty
  string: a wrong-data bug, since pasting it back changes the row;
- `F` (follow a foreign key) on a NULL built `WHERE id = NULL`, which
  never matches;
- the `Y` CSV yank and `--format csv` wrote both as an empty field;
- sort put empty strings with the NULLs at the end;
- the result diff (`D`) counted a NULL → `''` change as unchanged;
- a DBUnit capture (`:fixture save`) wrote `col=""` for a NULL.

The data was there: the text wire hands every cell over as
`Option<&str>`, and the typed paths (`conn::run_statement_typed`, used by
`--format json` and every MCP tool) already kept it.

## How cells flow

Every result set that reaches `Grid` comes through one function,
`conn::stream_text_rows` (used by `run_statement` for the editor and
`--batch`, and by `run_query` for the `T` / `L` panels), plus the demo's
canned answers, `conn::status_grid`, the `\l` grid and `\df` in demo
mode. `--format json` (single statement) and MCP `query` never touch
`Grid`: they read `TypedRows`, whose cells are already
`Option<String>`, so they emit JSON `null` for NULL today.

Readers of `Grid.rows`: the grid renderer, row and cell detail, sort
(`App::cycle_sort`, which sorts `grid.rows` **in place** and keeps a
clone in `grid_view.raw_rows` to restore the original order), filter
(`compute_visible_rows` produces `grid_view.visible_rows`, an index
list), find (`compute_grid_find_matches`, over the index list), the `y`,
`I`, `Y` and `F` yanks, the `D` result diff (which clones whole rows into
`PinnedResult` / `ResultDiffState`), the DBUnit capture, the `--batch`
formatters, and the catalog parsers in `query/` (`sessions`,
`slow_queries`, `migrations`, `probe`) and the start card's bootstrap
parse.

So rows are copied wholesale (raw_rows, pinned A, diff B, the CSV
export subset, demo filter / projection / truncation) and reordered in
place. Views mostly go through index lists, but sort does not.

## Options

### (a) `Option<String>` cells

`Grid.rows: Vec<Vec<Option<String>>>`, `None` for NULL: the same shape
`TypedRows` already has.

- **Blast radius**: every reader and every constructor. About 160 uses
  of `.rows` plus ~70 test fixtures. Mechanical, and the compiler finds
  every one: a site cannot silently keep treating NULL as `""`, it has
  to say so (`as_deref().unwrap_or("")`).
- **Drift**: none possible. The NULL bit is the cell.
- **Performance**: `Option<String>` is the same 24 bytes as `String`
  (the niche in `String`'s pointer), a NULL allocates nothing, exactly
  as `""` did. A 1000×N grid costs what it cost before.

### (b) A parallel null mask on `Grid`

`Grid { rows: Vec<Vec<String>>, nulls: Vec<Vec<bool>> }` (or a bitset).

- **Blast radius**: smallest diff. Readers that do not care keep
  compiling.
- **Drift**: the real risk. `cycle_sort` sorts `rows` in place with
  `sort_by`, which cannot permute a second vector; it would have to
  become an index sort. `raw_rows`, `PinnedResult`, `ResultDiffState`,
  the CSV export subset, and demo's filter / project / truncate all copy
  or cut `rows` and would each have to carry the mask too. Every one is a
  place for the two to fall out of step, and nothing would fail to
  compile when a new one is added — the failure mode is a NULL marker on
  the wrong row.
- **Performance**: one more allocation per row, and the sort becomes
  index-based. Negligible at 1000 rows, but no better than (a).

### (c) A `Row` type carrying its own mask

`struct Row { cells: Vec<String>, nulls: Vec<usize> }` with
`Deref<Target = [String]>`. Considered as the middle way: the mask
travels with the row, so sort / clone / truncate cannot desync it, and
readers that do not care keep compiling through `Deref`.

That last point is also its weakness. `row.join(" | ")`,
`row.to_vec()`, `row.get(i)` all keep compiling and keep conflating, so
every NULL-sensitive reader has to be found by grep rather than by the
compiler, today and every time one is added. It also adds a type the
rest of the code (`TypedRows`, `project_cells`) does not use.

## Decision: (a), `Option<String>`

The bug class is "a site that forgot NULL exists". Only (a) makes that a
compile error. The extra churn is one-off and mechanical; drift in (b)
is permanent. (a) also matches `TypedRows`, so the grid and the typed
paths speak the same cell type, and `conn::display_cells` (whose only
job was to throw the NULL away) goes.

`grid::Cell` is a type alias for `Option<String>`; `grid::cell_text`
gives a reader that genuinely does not care (a catalog parser, a
bootstrap size) the old `""`.

## Behaviour, per path

| path | NULL | empty string |
|---|---|---|
| grid, row detail, cell detail | `NULL`, dimmed italic in `theme.null_cell` | blank; cell detail says `(empty string)` |
| `I` INSERT yank | `NULL` | `''` (every value is quoted, see below) |
| `F` follow foreign key | refused: a NULL references nothing | `= ''` |
| `y` field yank | copies nothing, says `NULL` | copies `""` |
| `Y` / `--format csv` | empty unquoted field | `""` (quoted) — `COPY … CSV`'s rule, RFC 4180-valid |
| `--format tsv` | `\N` — `COPY` text format's NULL | empty field |
| `--format expanded` | blank (psql's default `\pset null`) | blank |
| `--format json`, `pgman query --json`, MCP `query` | `null` | `""` |
| sort | NULLS LAST ascending, NULLS FIRST descending: Postgres's defaults | sorts as the smallest string |
| filter `/`, find `f` | matches only the whole word `null` (any case) | matches nothing but an empty needle |
| result diff `D` | shown as `NULL`; NULL ↔ `''` is a change | shown as `''` |
| DBUnit capture | attribute omitted (flat XML's NULL) | `col=""` |

The filter rule: the grid shows NULL as the word `NULL`, so `/null`
finding those cells is what an operator expects. Matching any substring
of `null` would make `/nu` (looking for `number`) light up every row with
a NULL in it; a NULL has no text to contain anything, so only the whole
word matches. A text cell containing `null` still matches as before.

`--format expanded` stays as it was: it is for reading in a terminal,
it follows psql's default, and inventing a marker there would make the
text `NULL` ambiguous instead.

Found while doing it: `format_sql_literal` wrote number- and
boolean-looking text bare, which changed a text column's `007` to `7`.
With the grid holding text and no types, the yank now quotes every
value — an untyped literal, which Postgres reads as the column's type.
