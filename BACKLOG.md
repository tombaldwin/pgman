# pgman backlog

The shape: `## Done` is the historical record. `## Open` is what's
actually open. v2+ sections live below. Anything that's shipped lives
under Done, no matter which milestone it came from.

## Open

### M0 — connection + chrome polish
- IntelliJ `dataSources.local.xml` password parsing — the
  `parse_local_passwords` referenced in `creds::intellij`'s doc
  comment doesn't exist yet. Today `PGPASSWORD` fills the gap.
- NUMERIC / unknown-type cell rendering in `conn::cell_to_string` —
  currently best-effort `Debug`-style for unknown OIDs.
- TLS `verify-ca` hostname-skip via a custom rustls verifier — the
  `sslmode=verify-ca` URL param currently behaves like `verify-full`
  because we don't override the hostname check.

### M1 — reconstruction follow-ups
- Hibernate `format_sql=true` multi-line SQL reassembly.
- pglog: a SQL line containing a log-level token still confuses
  line-splitting.
- N+1: time-window heuristic once `ReconstructedQuery` carries
  timestamps.
- **Hibernate session summary** — when log-importing, show a header
  above the LogPick list: `47 queries, 3 N+1 clusters totalling
  1.2s, slow leader: …`. Today LogPick just lists individual
  queries; the summary turns the import into a triage view.

### M1.5 — Spring auto-connect follow-ups
- Profile-specific overrides (`application-prod.yml` overrides
  `application.yml`).
- `${...}` placeholder resolution — verify real Spring / SSM /
  1Password mechanics first (the `${op://}`-as-property-source
  assumption is still unconfirmed).

### M2 — editor follow-ups
- **Saved queries** — named, persisted under `util::data_dir()`,
  searchable, with `:param` substitution. The param prompt on run
  is what turns this from a snippet library into something that
  earns its keybinding.
- N+1 cluster view (group reconstructed queries by fingerprint, show
  per-cluster stats + a representative SQL).
- Capture-current-state → write a fixture (reverse of the DBUnit
  apply script).
- Per-database `CleanMode` config (which truncate strategy each db
  uses for DBUnit apply).

### Result grid
- **Yank focused row as INSERT** — generates `INSERT INTO <table>
  (col, …) VALUES (…)` for the focused row. Cell yank already
  exists; row-as-INSERT is the natural pair. Requires the result
  to know which table it came from (single-source SELECT —
  multi-table joins can fall back to a generic row literal).
- **Cell editing → UPDATE generation** — Enter on a cell in the
  grid (when the result is from a single table with a primary key)
  opens an inline editor; on save, generates `UPDATE … SET col =
  newval WHERE pk = …` and routes through the existing safety
  guard for confirmation. Every GUI tool has this; matches what
  TablePlus / DataGrip operators expect.
- **Foreign-key navigation** — when the focused cell is an FK
  column (looked up via `cache.constraints` for the
  result-source table), `→` / `Enter` runs `SELECT * FROM <parent>
  WHERE pk = <focused value>` in a new result frame, with breadcrumb
  back to the source row. The TablePlus / Postico click-through.
- **JSON path navigator** — JSONB cells in CellDetail render as a
  collapsible tree (object keys → values; array indices → values).
  `y` yanks the focused path (`.foo[0].bar`); Enter expands /
  collapses. Today they render as flat wrapped text.

### Schema browser
- **`:schema` tree view** — schemas → tables → cols / indexes /
  constraints / FKs. Left pane is the tree; right pane shows DDL +
  a sample-rows preview + table size for the focused node. Removes
  the "what was that table called again" lookup that's currently
  served only by completion.
- One-key actions on the focused node: copy DDL, generate
  `SELECT * FROM …`, generate `INSERT INTO … (cols) VALUES (…)`
  template.

### Performance / DBA
- **EXPLAIN plan visualizer** — tree view of the plan with the
  hottest node highlighted (by `actual_total_time` for ANALYZE, or
  `total_cost` for plain EXPLAIN). Collapsible subtrees. The single
  feature most likely to make someone download pgman over psql.
- **pg_stat_statements top-N panel** — `:slow` opens a sorted list
  of the worst queries in the DB by `total_exec_time` /
  `mean_exec_time` / `calls`. One key copies the representative SQL
  into the editor for tuning. Requires the extension to be enabled;
  detect at connect and gate the keybinding.
- **Active sessions + locks view** — `pg_stat_activity` joined with
  `pg_blocking_pids()` so blocked / blocker relationships render as
  a tree. One key terminates a runaway session (with confirm). The
  "prod is wedged at 3 a.m." feature.
- **Streaming results for huge SELECTs** — today `conn::run_query`
  fetches every row into RAM, so `SELECT * FROM huge_table` brings
  the TUI down. Switch to a cursor / portal-based fetch with a
  configurable page size; the grid shows what it has and pages on
  scroll. Pairs with the existing `statement_timeout` safety knob.
- **LISTEN / NOTIFY display** — `:listen <channel>` subscribes;
  incoming `NOTIFY` payloads tail into a status strip with
  timestamps. Niche but trivial once the run-loop hosts a
  side-channel reader.

### UX
- **Esc shouldn't quit** — today Esc quits from Normal mode and from
  the ConnPick picker. Make Esc "close any overlay; otherwise no-op"
  so a reflex Esc never loses the session.
- **Real `:about` command** once a typed-command palette lands. The
  `A` keybinding works in the meantime.
- **Keymap customization** — bindings are hardcoded today. A
  `~/.config/pgman/keymap.toml` letting operators rebind (or alias)
  keys would help muscle-memory transitions from k9s / Vim / Emacs
  and is a baseline accessibility requirement.
- **Multi-tab / multi-session** — a single editor buffer + single
  result grid is the bottleneck once an operator is debugging two
  things at once. Tabs (Ctrl-Tab / Ctrl-Shift-Tab) holding
  independent editor + result + history state. Each tab can target
  a different connection — extends [Connection switching mid-
  session] in [psql parity] from "switch the current session" to
  "have several at once".

### Completion — next round
- **DDL column-type completion** — `CREATE TABLE t (col |)` should
  offer the existing `TYPE_NAMES` vocab. Today the column-position
  arm of CREATE TABLE doesn't flip context to `TypeName`. Small,
  contained.
- **Non-ASCII identifier tokenizer** — `extract_identifier` walks
  byte-by-byte and rejects identifiers ending in non-ASCII letters
  (`café.|` doesn't complete end-to-end even though the dot
  auto-trigger already uses char-aware lookup). Widen the walk to
  `char_indices` + Unicode alphabetic.
- **`nextval('|')` literal-context completion** — when the cursor is
  inside a single-quoted string immediately after `nextval(`, offer
  sequence names from `cache.sequences`. Needs in-string cursor
  handling (the extractor currently bails on quoted contexts).
- **JOIN ON FK suggestions** — in `JOIN orders o ON `, look at
  `pg_constraint` FK rows linking in-scope tables and offer
  `users.id = orders.user_id`-style predicates as multi-token
  candidates. Catalog fetch needs a new query for foreign-key edges.
  Higher-value, bigger scope than the other completion items here.

### Reuse from ebman (`/Users/tom/git/ebman/src/`)
Survey is in [Done]. Lift as the milestones reach them:
- `shell.rs` — PTY wrapper + key→bytes; verbatim. Use for `psql` /
  `pg_dump` / `claude` handoff (M2 / advisor / snapshots).
- Toast stack, Braille spinner, pill-chain widgets (`ui.rs`) — lift
  as chrome is built out.
- `state.rs` — line-oriented persisted state; adapt for saved
  connections / query history.
- `mode_action.rs` — `ConfirmModal` + countdown state machine; adapt
  for `safety::Guard::Confirm` prompts and backup / restore.
- `commands.rs` — `:command` registry feeding help + Ctrl-K palette.
- `form.rs` — multi-field modal; connection editor + safety.toml
  editor.
- `mode_detail.rs` — tabbed drill-down pattern.
- Smaller: `keys.rs`, `plugins.rs`, `update_check.rs`, `control.rs`,
  `cost_cache.rs` (→ schema / plan cache), `report_bug.rs` (PII
  scrubber).
- Skip (AWS-specific): `profiles.rs`, `sso.rs`, `aws.rs`.

## v2 — AWS (not started)

- **RDS context panel** — for the current connection, pull the last
  hour of CloudWatch CPU / connections / IOPS / read-write latency
  into a corner widget. Connects the slow query you're tuning to the
  metrics that are actually on fire. Wants the same AWS-creds
  resolution as the items below.
- `discover_aws.rs` + `mode_databases.rs`; `--rds <id>` launch handoff
  from ebman. (Add a one-line backlog item in *ebman* for the
  spawn-pgman action.)
- `rds_logs.rs` + `mode_logs.rs` — `DescribeDBLogFiles` / CloudWatch;
  feeds pglog.
- `creds/{ssm,secrets}.rs` + `creds/onepassword.rs` — placeholder
  resolution.
- `migrate/` — `MigrationStrategy` trait + `flyway` / `liquibase` /
  `custom` (config-driven table mapping; uflexi is one preset). Note:
  `pending()` is not implementable for a DB-table-only strategy —
  make it an optional capability.
- `mode_params.rs` — RDS parameter groups; view / diff +
  `ModifyDBParameterGroup` with confirmation; surface pending-reboot.
- `mode_dashboard.rs` — live PG stats + RDS CloudWatch metrics.
- `advisor.rs` + `mode_advisor.rs` — health-snapshot → `claude` CLI
  review; interactive handoff via `handoff.rs`. Snapshot must be
  scrubbable (egress).

## v2 — local DB sync & backups (not started)

Pull a remote database down for local testing; keep tagged backups.

- `snapshot.rs` — snapshot store under `util::data_dir()`: each
  snapshot is a `pg_dump` artifact (custom format) plus a metadata
  record (source DSN redacted, timestamp, size, db version, tag,
  pinned flag). A TOML / JSON index lists them.
- `mode_snapshots.rs` — list / create / restore / tag / pin / delete.
  - Create: `pg_dump` a remote DB → store. Capture the server major
    version.
  - Restore: `pg_restore` (or `psql`) into a chosen local target DB,
    with a confirm step (restore is destructive to the target).
  - Pin protects a snapshot from prune; tags group them
    (`pre-migration`, `prod-2026-05`, …).
- Version skew: `pg_dump` / `pg_restore` must be ≥ the server major
  version — detect and warn, or locate a matching client binary.
- Retention: optional prune of un-pinned snapshots past a count / age
  limit.

## v3+ — deferred

- **Redaction / anonymisation** on snapshot restore — declarative
  rules (`null` a column, fake an email, hash an id) applied as data
  lands locally, so local testing never holds real PII.
- `catalog/` — version-aware catalog trait; EPAS support.
- JPA entity ↔ table mapping.
- Migration safety linter (lock-heavy operations).
- EXPLAIN plan diff.

## Done

Historical record. Newest at the top within each section.

### Foundations (M0)

- **Scaffold.** Repo, `Cargo.toml`, lib+bin split, `CLAUDE.md`. Lifted
  `theme.rs` + `util.rs` from ebman. Pure modules implemented with
  tests: `safety` (statement classification + per-DB guards),
  `query::subst` (placeholder substitution), `query::nplus1`
  (fingerprint + clustering), `query::reconstruct` (shared types),
  `conn` (DSN parsing), `creds::spring` (`.properties` parsing +
  Java-project detection), `splash`.
- **TUI event loop** (`app.rs`, `app/msg.rs`, `ui.rs`, `tui.rs`). One
  frame clock, gated on splash / loading.
- **`splash.rs`** — animated elephant, dismissed on keypress or
  connect. 3-second minimum kept across the ConnPick path.
  Trunk-tip animation + occasional blink.
- **`font_probe.rs`** — lifted from ebman; resolves `auto` → IconStyle.
- **`conn.rs`** — real connection via `tokio-postgres`; applies safety
  session settings (`default_transaction_read_only`,
  `statement_timeout`).
- **`grid.rs`** — results grid type + column-width / truncation
  helpers.
- **TLS via `tokio-postgres-rustls`** — connector tries native trust
  roots (`rustls-native-certs`) then falls back to Mozilla's
  `webpki-roots` so RDS / managed Postgres "just works". `sslmode=`
  URL param honoured (`disable` / `prefer` (default) / `require` /
  `verify-*`).
- **Panic hook** restores the terminal (alt-screen + raw-mode off)
  before the default hook prints the backtrace.
- **Scrollable help overlay** (j/k/g/G + PageUp/PageDown,
  "↑/↓ N more" hints).
- **Diagnostic connection-failure view** — target DSN, source /
  origin (`--dsn flag` / `auto-picked IntelliJ data source 'x'`),
  full error chain (walks `Error::source` so "Connection refused"
  actually appears), and an actionable hint for known failure modes
  (refused / timeout / DNS / auth / missing db / TLS-required). Plus
  `r` retry and `p` re-open picker keys.
- **Project config at `.pgman/pgman.toml`** (intended for git):
  named `[[connections]]` feed the startup picker; `[safety]`
  overrides merge with the global `~/.config/pgman/safety.toml`
  per-key. Passwords come from `PGPASSWORD` / per-connection
  `password_env`, never the file.
- **IntelliJ data-source discovery** — pre-TUI scan of
  `.idea/dataSources.xml` + `.idea/dataSources.local.xml` (merged by
  UUID; local file's `<user-name>` and schema-mapping db names fill
  in for the committed file). One postgres source → auto-DSN;
  multiple → `Mode::ConnPick` picker. Per-database disambiguation
  when schema-mapping has multiple.
- **Spring properties / YAML / bootstrap discovery** — scans
  `src/main/resources/{application,bootstrap}*.{properties,yml,yaml}`,
  emits one pick per `<prefix>.url` + `.username` + `.password`
  triple. YAML support via a focused YAML → dot-notation flattener
  (handles nested mappings, comments, quoted scalars; skips lists /
  anchors). Credentials read from file but only redacted DSNs are
  logged.
- **Row-detail modal** (Enter on a grid row): psql `\x`-style
  expanded view, one labelled value per column, values wrapped.
  Field cursor (j/k) with auto-scroll; `y` yanks the focused value
  via `arboard`.
- **Per-cell zoom** from RowDetail (`Enter` on a focused field) opens
  `Mode::CellDetail` — a larger popup showing the single value
  wrapped + scrollable.
- **`:about` overlay** (`A` key) — same content as splash (elephant +
  version + credits).
- **SSH tunnel built-in** — `[[connections]]` entries gain an
  `ssh_tunnel = "user@bastion"` field; the `--dsn` URL accepts
  `?ssh_tunnel=user@bastion[:port]`. A new `tunnel` module shells
  out to the system `ssh` (subprocess, `-N -L 127.0.0.1:LOCAL:
  REMOTE_HOST:REMOTE_PORT bastion -o BatchMode=yes`) and the
  tunnel handle lives in `Booted` so the App keeps the ssh process
  alive for the session. `tokio-postgres`'s `hostaddr` field lets
  us connect to the local end while TLS still verifies against the
  real server name. Provenance: `redacted()` appends `via
  ssh://user@bastion` and a tunnel-failure error surfaces a
  dedicated `connect_hint` ("try `ssh -v` manually — we run with
  BatchMode=yes"). 13 unit tests covering parsing + the param /
  config plumbing.

### Query reconstruction (M1)

- `query/reconstruct.rs` — shared `ReconstructedQuery` type.
- `query/subst.rs` — `?` + `$N` parameter substitution.
- `query/hibernate.rs` — HB5 + HB6 log parsing, thread-grouped.
- `query/pglog.rs` — Postgres / RDS log parsing
  (`statement` / `parse`-`bind`-`execute`).
- `query/nplus1.rs` — fingerprint + clustering.
- `query/jdbc.rs` — pasted SQL + `TYPE:value` parameter lines.

### Editor (M2)

- **SQL editor** — multi-line buffer with cursor, multi-byte safe.
  Up / Down preserve preferred char-column. Home / End act per line.
  Editor pane grows dynamically (3-row min, 12-row cap) and scrolls
  long buffers to follow the cursor (`clamp_editor_scroll`).
- **Run paths** — `Ctrl-R` / `F5` to run, `Ctrl-E` / `F6` EXPLAIN,
  `Ctrl-A` / `F7` EXPLAIN ANALYZE. Persistent `tokio-postgres` client
  held by `App`; subsequent queries reuse the session.
- **Safety routing** — every run goes through `safety::evaluate`:
  `Block` rejects, `Confirm` opens a modal, `Allow` runs. `auto_tx`
  wraps DML in `BEGIN` / `COMMIT`.
- **DML-aware EXPLAIN ANALYZE** — writes wrap in `BEGIN` /
  `ROLLBACK` so the mutation never lands.
- **Multi-statement run** — `safety::split_statements` splits on `;`
  outside strings / comments. Most-restrictive guard wins; routed
  through `conn::run_batch` / `run_batch_in_tx_open`.
- **Commit / rollback prompt for `auto_tx`** —
  `conn::run_in_tx_open` leaves the tx open; `Mode::TxDecision`
  blocks input until `y` commits or `n` / `esc` rolls back.
- **Affected-row count** for non-row-returning statements via
  `conn::run_statement`.
- **Query history** — 50-entry ring buffer; `Ctrl-P` / `Ctrl-N`
  navigate; live draft preserved on `Ctrl-N` past newest.
- **Log import** — `Ctrl-L` / `F8` parses the buffer through
  `hibernate::parse` + `pglog::parse`; `Mode::LogPick` lists
  reconstructed queries; Enter loads `runnable_sql` into the editor.
- **DBUnit fixtures** — `dbunit::parse_flat_xml` reads a
  FlatXmlDataSet; `generate_clean` / `generate_inserts` /
  `generate_apply_script` produce a `TRUNCATE` (or `DELETE FROM`) +
  `INSERT` script in correct FK order. `Ctrl-D` / `F9` reads the
  buffer as a fixture path and replaces it with the apply script.
- **Bracketed paste** — terminal wraps pasted text in escape codes;
  crossterm delivers `Event::Paste(String)` instead of streaming
  each char. CRLF / CR normalised to LF.

### psql parity (table-stakes credibility gaps)

The eight basics every Postgres user expects on day one. Shipped as a
unified pass after the SSH-tunnel work.

- **Cancel running query** (Ctrl-C) — sends a PostgreSQL
  `CancelRequest` via `tokio_postgres::CancelToken`; the original
  `execute` future resolves with the cancellation error and lands as
  the normal `QueryFailed` message. In tunneled sessions the cancel
  TCP inherits the parent `Config` so it rides through the same ssh
  forward. Top-level Ctrl-C still quits in non-Editor modes; in
  Editor it cancels mid-query or no-ops on idle.
- **Error position cursor jump** — `conn::QueryErr` extracts
  `as_db_error().position()` from `tokio_postgres::Error`; the
  `QueryFailed` handler converts the 1-indexed char position to a
  byte offset (multibyte-safe via `char_indices`) and moves the
  editor cursor. EXPLAIN / EXPLAIN ANALYZE wrappers shift the
  position back by the wrapper prefix so the cursor lands inside
  the user's buffer.
- **History search (Ctrl-R)** — new `Mode::HistorySearch` +
  `HistorySearchState`. Reverse-incremental substring match
  (case-insensitive). Buffer mirrors the current match so the
  operator previews before Enter. Ctrl-R again walks older. Esc
  restores the pre-search snapshot. Bash-style status:
  `(reverse-i-search) 'q'` / `(failed reverse-i-search) 'q'`. Run
  rebinds to F5-only + Ctrl-Enter / Ctrl-J to free Ctrl-R.
- **`\watch` / repeat query** (Ctrl-W) — re-runs the buffer (or
  last history entry) every 2 s, routed through the same safety
  pipeline. Any key stops. Refused during an open auto_tx so we
  can't pile up runs on a paused session.
- **NOTICE / WARNING / RAISE surfacing** — connection driver
  switched from `connection.await` to `Connection::poll_message`,
  intercepting `AsyncMessage::Notice` and forwarding via an
  unbounded channel. App pumps through `AppMsg::Notice` to a
  bounded ring buffer (50 entries) + status footer; tracing
  captures detail / hint at info level.
- **`\e` external editor** (Ctrl-X) — pending-flag pattern: editor
  key handler sets the flag; main `run()` loop suspends the TUI
  (new `Tui::suspend` / `Tui::resume`), writes the buffer to a
  per-pid temp file, execs `$EDITOR` (multi-word like `code --wait`
  split on whitespace; falls back to `$VISUAL`, then `vi`), reads
  back on save. Always resumes the TUI even if the editor errored.
- **Connection switching mid-session** — `c` in Normal mode opens
  the existing `ConnPick` picker. Cancels any in-flight query
  first. The picker's Enter handler already does the reconnect
  (`start_connect` after swapping `self.dsn`); on Booted the new
  client + tunnel land and the old tunnel drops on a worker
  thread.
- **Batch / pipe mode** — `pgman --batch --sql "…" --format
  csv|tsv|json|expanded`. SQL via `--sql` or stdin. New
  `conn::connect_only` skips the schema fetch / bootstrap that
  the TUI needs. `batch::run` writes the formatted result to
  stdout; notices go to stderr. Exit 0 on success, 1 on query
  failure, 2 on connect / arg failure. Pure formatters
  (`format_csv` / `tsv` / `json` / `expanded`) are unit-tested
  with RFC-4180 quoting, control-char escaping, and `\x`-style
  column padding.

### Editor — authoring polish

Three quality-of-life features after the psql-parity sweep.

- **Semantic syntax highlighting.** New `query::highlight` module:
  pure two-pass lex + classify. Keywords / functions / strings /
  comments / numbers / identifiers via hand-rolled lexer (handles
  `'`-escapes, dollar-quoted `$tag$ … $tag$`, nested block
  comments, multi-byte safe). Identifiers then resolve against the
  schema cache + in-scope tables / aliases / CTEs / virtual columns
  — known stays default-coloured, unknown turns red (typo flag).
  Loose resolution: any cache hit anywhere counts. Theme grows two
  fields (`syn_string`, `syn_unknown`) for the dark / light /
  high-contrast variants; everything else reuses existing colours.
  The editor render walks per-line spans and overlays the cursor's
  REVERSED glyph on top of whatever syntax colour it sits in. 17
  unit tests cover the lexer.
- **Auto-save editor buffer.** `util::data_dir()` (new helper —
  `~/.local/share/pgman`) holds `draft.sql`; on quit we
  `write_atomic` the current buffer there, on startup `main` reads
  it back and seeds `editor_buffer` + cursor. Empty buffer clears
  the persisted draft so a deliberate clean-out survives a restart.
  Load lives in `main.rs` (not `App::new`) so unit tests don't
  pull a live developer's draft from disk.
- **`pg_format` buffer reformat (Ctrl-F).** Subprocess to
  `pg_format` (the standard `pgformatter` Perl tool); buffer
  piped to stdin, prettyprinted output replaces the editor. Missing
  binary surfaces an actionable error (`brew install pgformatter
  or apt install pgformatter`). Done inline since pg_format is
  sub-second; spawn_blocking would just add plumbing.

### Result grid — sort / filter / export

- **Sort by column.** `h` / `l` (and Left / Right) move the column
  cursor — the focused header reverses so it's obvious which column
  is targeted. `s` cycles: off → ASC → DESC → off. Snapshots the
  raw row order before the first sort so the "off" state restores
  it without re-running the query. Numeric-aware compare via
  `grid::cmp_cells` (so `2` sorts before `10`); empty strings (NULL
  renderings) sort last per Postgres's default `NULLS LAST`.
- **Live row filter (`/`).** `Mode::GridFilter` — each char updates
  `grid_filter` and rebuilds the visible-row index in place; the
  status footer shows `filter: /pat · m/N row(s)`. Case-
  insensitive substring across every cell. `n` / `N` step through
  matches in the visible order. Enter accepts; Esc clears.
- **Export to clipboard (`Y`).** Serialises the *visible* rows
  (post-sort, post-filter) via the existing `batch::format_csv`
  formatter and pushes to the system clipboard via `arboard`.
  Status shows the copied row count.
- **RowDetail / CellDetail correctness through filter+sort.** New
  `App::selected_grid_row_idx()` maps the visible cursor index
  through `grid_visible_rows` to the actual `grid.rows` index;
  all three downstream features (row detail, cell zoom, cell
  yank) go through it so a filtered or sorted view never opens
  the wrong row.

### Completion

The largest section. Built bottom-up: tokenizer → FROM parser →
clause classifier (scope-stack) → completion engine, all pure-tested.

**Core engine**
- Schema cache fetched at connect via `pg_catalog` (best-effort —
  permission failure → empty cache → completion disabled cleanly).
  Now includes sequences, indexes, and `pg_constraint` rows for
  unique / primary keys.
- Tab cycles candidates; `alias.col` only offers the aliased
  table's columns; `schema.|` only offers that schema's tables;
  bare-identifier completion is biased toward columns of tables in
  the current FROM / JOIN scope (including subquery FROMs after the
  cursor).
- 3-segment qualified completion (`schema.table.col`).
- Popup overlay anchored under the editor, up to 8 visible rows,
  auto-scroll, active row highlighted.
- Esc during a cycle restores the originally-typed prefix.

**Grammar awareness**
- `query::clause` classifier scans tokens of the current statement
  and returns `ClauseContext` + an optional write target. Tolerant
  of mid-typed buffers.
- Scope-stack: subquery `(...)` no longer leaks ctx after `)`;
  `WITH cte AS (SELECT em` classifies correctly inside the body.
- Branches: `StatementStart`, `TableRef`, `SelectList`, `Predicate`,
  `HavingPredicate`, `OrderOrGroup`, `InsertColumns(t)`,
  `UpdateAssign(t)`, `Values`, `ExplainOptions`, `VacuumOptions`,
  `GucParameter`, `GucValue`, `TypeName`, `ConstraintName`,
  `DropTarget(DropKind::{Table|Index|Sequence})`.
- UPDATE / DELETE write target folded into in-scope so WHERE
  completion works without a FROM.

**Vocabulary** — single source of truth at `query::vocabulary`.
Module-level tests assert "all entries uppercase (or lowercase for
GUCs / types / values), no dups". Adding a function / operator is a
one-line append.
- Aggregate / scalar / window functions in SelectList.
- Predicate operators (`LIKE`, `IN`, `BETWEEN`, `IS NULL`, `EXISTS`,
  `IS DISTINCT FROM`, `SIMILAR TO`, …).
- JOIN variants (`INNER JOIN`, `LEFT OUTER JOIN`, `NATURAL JOIN`,
  `LATERAL JOIN`, …).
- DDL verbs, session keywords, perms, Postgres catalog helpers
  (`PG_SIZE_PRETTY`, `VERSION`, `TXID_CURRENT`, …), string /
  numeric / array / JSON families.
- `EXPLAIN (...)` options, `VACUUM (...)` / `ANALYZE (...)` options
  (shared list).
- `SHOW` / `SET` GUC parameters + values (`on` / `off` / `true` /
  `false` / `default`).
- `CAST(expr AS |)` type names (~40 entries).
- `INSERT … ON CONFLICT (col) DO UPDATE SET col = EXCLUDED.col` —
  EXCLUDED registered as a virtual table; constraint names scoped
  to the write target.
- `DROP TABLE / INDEX / SEQUENCE / VIEW / MATERIALIZED VIEW`,
  `REINDEX <kind> name`.
- `COPY tab (col_list)`, `TRUNCATE tab`.

**CTE / subquery column inference**
- `extract_ctes` returns `Vec<CteDef>` with inferred columns.
  Explicit `WITH foo(a, b) AS …` lists win; otherwise the body's
  SELECT list is parsed.
- `parse_from_tables` registers subquery aliases
  (`FROM (SELECT ...) sub`) and stores inferred columns on the
  `TableRefInQuery.virtual_columns` field.
- `SELECT *` expansion in CTE / subquery bodies via
  `SelectItem::{Named, Star, StarOf}` + the resolved variants
  (`resolve_select_columns`, `extract_ctes_resolved`,
  `parse_from_tables_resolved`).
- UNION column inference is implicit — extractor stops at UNION so
  only the first arm's columns survive.

**Continuations** — `vocabulary::continuations` lists what naturally
follows each ClauseContext (`AFTER_TABLE_REF`, `AFTER_PREDICATE`,
…). Surfaced as Keyword candidates AFTER identifiers so the cycle
prioritises columns / tables / aliases. In SelectList, FROM ranks
BEFORE FORMAT.

**UX polish**
- Case preservation — `sel|` → `select`; `SEL|` → `SELECT`.
- Longest-common-prefix expansion — Bash-style two-phase Tab. First
  Tab on `t_|` against `t_users`, `t_user_logs`, `t_user_roles`
  expands to `t_user_`; second Tab picks the first match.
- Live narrowing while typing — narrowing keys (chars, Backspace,
  Delete) keep the cycle alive and refresh the candidate list. Esc
  still undoes back to the pre-Tab state.
- Exact-match auto-commit — typing the full name + Tab dismisses
  the popup. Honours the cache's canonical case (`USERS` → `users`).
- Column candidates show owning table in popup — `email (column · u)`
  or `email (column · users)`. Aliases show their underlying table.
  Non-public tables show their schema.
- Footer hint switches to `type to narrow · tab cycle · esc undo`
  while the popup is up.
- Bold the matched prefix in popup rows.
- Auto-trigger after `.` — `users.|` pops the column list with no
  Tab. Numeric literals (`3.14`) excluded by char-aware lookup.
- Auto-trigger after identifier-introducing keywords + space
  (`FROM `, `JOIN `, `WHERE `, `AND `, …) opens the popup.
- Tab on whitespace / empty prefix opens the context-aware popup
  with no auto-insert.
- Function completions insert `NAME(` so the cursor lands inside.
- Ctrl-Space alias for Tab — industry-standard "open popup"
  shortcut.
- Fuzzy / subsequence fallback when prefix-anchored returns nothing
  and the typed prefix is ≥3 chars. Ranked by match tightness;
  capped at 30 results. Qualified prefixes narrow to the qualifier's
  children.

### ebman survey

Survey of `/Users/tom/git/ebman/src/` to decide what's worth lifting.
The lifts themselves are in [Open → Reuse from ebman]; the survey
itself is done.
- Adopt ebman's splash *rendering* but NOT its 3s minimum duration —
  overlap with connect, keep instantly dismissable.
