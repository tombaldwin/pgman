# pgman backlog

## Done

- **Scaffold.** Repo, `Cargo.toml`, lib+bin split, `CLAUDE.md`. Lifted
  `theme.rs` + `util.rs` from ebman. Pure modules implemented with tests:
  `safety` (statement classification + per-DB guards), `query::subst`
  (placeholder substitution), `query::nplus1` (fingerprint + clustering),
  `query::reconstruct` (shared types), `conn` (DSN parsing),
  `creds::spring` (`.properties` parsing + Java-project detection), `splash`.
- **Query reconstruction parsers.** `query::pglog` (Postgres / RDS server
  logs — `statement` / `parse`-`bind`-`execute`, parameters paired by pid)
  and `query::hibernate` (Hibernate 5 & 6 logs — SQL paired with binds by
  thread). Both pure and tested. Stub module remaining: `query::jdbc`.

## v1 — the wedge

Ship: "paste a log or open me in a Spring project → runnable SQL → run it,
safely." Nothing else.

### M0 — shell + connection
- TUI event loop: `app.rs`, `app/msg.rs`, `ui.rs`, `tui.rs`. One frame clock,
  gated on splash/loading — DONE.
- `splash.rs`: animated elephant, dismissed on keypress or connect — DONE.
- `font_probe.rs`: lifted from ebman; resolves `auto` → IconStyle — DONE.
- `conn.rs`: real connection via `tokio-postgres`; applies safety session
  settings (`default_transaction_read_only`, `statement_timeout`) — DONE.
- `grid.rs`: results grid type + column-width / truncation helpers — DONE.
- IntelliJ data-source discovery: pre-TUI scan of `.idea/dataSources.xml`;
  one postgres source → auto-DSN; multiple → `Mode::ConnPick` picker;
  `PGPASSWORD` fills passwords (IntelliJ keeps them in its keychain, not
  the XML) — DONE.
- Scrollable help overlay (j/k/g/G + PageUp/PageDown, "↑/↓ N more" hints) — DONE.
- Diagnostic connection-failure view: target DSN, source/origin
  ("--dsn flag" / "auto-picked IntelliJ data source 'x'"), full error
  chain (walks `Error::source` so "Connection refused" actually appears),
  and an actionable hint for known failure modes (refused / timeout /
  DNS / auth / missing db / TLS-required). Plus `r` retry and `p`
  re-open picker keys on the failure screen — DONE.
- Project config at `.pgman/pgman.toml` (intended for git): named
  `[[connections]]` feed the startup picker; `[safety]` overrides merge
  with the global `~/.config/pgman/safety.toml` per-key so a team can
  commit just the production rules. Discovery walks up from cwd to find
  the file. Passwords come from `PGPASSWORD` / per-connection
  `password_env`, never the file — DONE.
- IntelliJ multifile parsing: `dataSources.local.xml` contributes
  `<user-name>` and schema-mapping db names (the latter recovers the
  real dbname when the committed `<jdbc-url>` has no path, e.g.
  `localhost:5432/`). One pick emitted per database when schema-mapping
  has multiple — DONE.
- Spring properties discovery: scans `src/main/resources/application*.properties`
  and emits one pick per `<prefix>.url` + `.username` + `.password` triple
  (so `dataSource.*`, `logDataSource.*` etc. all surface). Non-JDBC URLs
  filtered. Credentials are read from the file but only redacted DSNs
  are logged. yml is still stubbed — DONE.
- Row-detail modal (Enter on a grid row): psql `\x`-style expanded view,
  one labelled value per column, values wrapped to popup width — DONE.
- Row-detail field cursor: j/k navigate between fields with the
  focused row highlighted and auto-scrolled into view; `y` yanks the
  focused value to the system clipboard via `arboard` — DONE. Follow-ups:
  per-cell zoom for very long values (e.g. JSON); NULL vs empty-string
  disambiguation once the grid tracks NULL distinctly.
- Splash 3s minimum + always-shown: brought back across the ConnPick
  path (was being skipped when multiple data sources were discovered);
  `Booted`/`BootFailed` no longer dismiss it early. Keypress still
  skips. `A` opens an `:about`-style overlay with the same content
  (elephant + version + credits) — DONE. Follow-up: real `:about`
  command once a typed-command palette lands.
- Identifier completion in the editor (Tab):
  - Schema cache fetched at connect via `pg_catalog` (best-effort —
    permission failure → empty cache → completion disabled cleanly).
  - Pass 1 (dumb): Tab cycles through tables / columns / schemas /
    aliases matching the partial identifier under the cursor.
  - Pass 2 (FROM-aware): `alias.col` only offers columns of the
    aliased table; `schema.|` only offers that schema's tables;
    bare-identifier completion is biased toward columns of tables in
    the current `FROM` / `JOIN` clauses (including subquery FROMs
    after the cursor). A tolerant tokenizer handles partial / unfinished
    SQL (`SELECT u.| FROM users u ...`).
  - Cycle resets on any non-Tab editor keypress. Footer status shows
    `completion N/M · kind` while cycling. — DONE.
  - Candidates popup: small overlay anchored under the editor with
    up to 8 visible rows; active row highlighted; auto-scrolls when
    cycling past the visible window — DONE.
  - Esc during a cycle restores the originally-typed prefix (so
    you can back out of an unwanted match cleanly) — DONE.
  - Follow-ups: SQL keyword completion; nested `schema.table.col`
    qualification.
- Panic hook restores the terminal (alt-screen + raw-mode off) before
  the default hook prints the backtrace, so a crash leaves the trace
  readable on the user's regular shell instead of buried in the alt
  screen — DONE.
- Per-cell zoom from RowDetail (`Enter` on a focused field) opens
  `Mode::CellDetail` — a larger popup showing the single value wrapped
  + scrollable so long JSON / text fits. `y` still yanks. `Esc`/
  `Enter` pops back to the row view; `Esc` in RowDetail now closes
  to Normal (Enter rebinds to zoom) — DONE.
- TLS via `tokio-postgres-rustls` — connector tries native trust roots
  (`rustls-native-certs`) then falls back to Mozilla's `webpki-roots` so
  RDS / managed Postgres "just works". `sslmode=` URL param honoured
  (`disable` / `prefer` (default) / `require` / `verify-*`) — DONE.
- Follow-ups: `deadpool`
  pooling once interactive queries land (M2); panic hook to restore the
  terminal; `NUMERIC` / unknown-type cell rendering in `conn::cell_to_string`;
  IntelliJ `dataSources.local.xml` password parsing (the
  `parse_local_passwords` referenced in `creds::intellij`'s doc comment
  doesn't exist yet); Spring `application*.yml` parsing → picker entries.

### Reuse from ebman (`/Users/tom/git/ebman/src/`)
Survey done — lift as the milestones reach them:
- **`shell.rs`** — PTY wrapper + key→bytes; verbatim. Use for `psql` /
  `pg_dump` / `claude` handoff (M2 / advisor / snapshots).
- **`font_probe.rs`** — DONE (lifted at M0).
- Toast stack, Braille spinner, pill-chain widgets (`ui.rs`) — lift as chrome
  is built out.
- **`state.rs`** — line-oriented persisted state; adapt for saved connections
  / query history (M2).
- **`mode_action.rs`** — `ConfirmModal` + countdown state machine; adapt for
  `safety::Guard::Confirm` prompts and backup/restore (M2 / v2).
- **`commands.rs`** — `:command` registry feeding help + Ctrl-K palette;
  rewrite entries for Postgres.
- **`form.rs`** — multi-field modal; connection editor + safety.toml editor.
- **`mode_detail.rs`** — tabbed drill-down pattern.
- Smaller: `keys.rs`, `plugins.rs`, `update_check.rs`, `control.rs`,
  `cost_cache.rs` (→ schema/plan cache), `report_bug.rs` (PII scrubber).
- Skip (AWS-specific): `profiles.rs`, `sso.rs`, `aws.rs`.
- Note: adopt ebman's splash *rendering* technique but NOT its 3s minimum
  duration — overlap the splash with connect, keep it instantly dismissable.

### M1 — query reconstruction (the hero)
- `query/reconstruct.rs`: shared types — DONE.
- `query/subst.rs`: `?` + `$N` substitution — DONE.
- `query/hibernate.rs`: HB5 + HB6 log parsing, thread-grouped — DONE.
  Follow-ups: reassemble `hibernate.format_sql=true` multi-line SQL.
- `query/pglog.rs`: Postgres / RDS log parsing — DONE. Follow-ups: a SQL line
  containing a log-level token still confuses line-splitting.
- `query/nplus1.rs`: fingerprint + clustering — DONE. Follow-up: time-window
  heuristic once `ReconstructedQuery` carries timestamps.
- `query/jdbc.rs`: parse pasted SQL + `TYPE:value` parameter lines — DONE.
- `mode_hibernate.rs`: log-import view feeding hibernate + pglog parsers —
  pending (not wired into the editor yet).

### M1.5 — Spring auto-connect
- `creds/spring.rs`: `.properties` parsing + Java detection — DONE (scaffold).
- Add `application.yml` parsing (profiles, `${}` placeholders) — needs
  `serde_yaml`. **Verify real Spring/SSM/1Password mechanics before building
  placeholder resolution** — the `${op://}`-as-property-source assumption is
  unconfirmed.
- Auto-detect on launch; show provenance; **require a keypress to confirm** the
  resolved target before connecting.

### M2 — editor
- SQL editor mode (single-line buffer with cursor; multi-byte safe) — DONE.
- F5 / Enter to run, F6 EXPLAIN, F7 EXPLAIN ANALYZE — DONE.
- Persistent `tokio-postgres` client held by `App` (subsequent queries reuse
  the same session) — DONE.
- Every run routes through `safety::evaluate`; `Block` rejects, `Confirm`
  opens a modal, `Allow` runs. `auto_tx` wraps DML in `BEGIN`/`COMMIT` — DONE.
- DML-aware `EXPLAIN ANALYZE`: writes wrap in `BEGIN`/`ROLLBACK` so the
  mutation never lands — DONE.
- Non-row-returning statements (UPDATE/DELETE/DDL) render an affected-row
  count via the unified `conn::run_statement` — DONE.
- Multi-line editor: `Enter` inserts a newline, `F5` runs; Up/Down move
  the cursor across lines preserving the preferred char-column;
  Home/End act per line; editor pane grows dynamically with the buffer
  (3-row min, 12-row cap) — DONE.
- Query history: every run is pushed to a 50-entry ring buffer
  (consecutive duplicates skipped); Ctrl-P / Ctrl-N navigate; the live
  draft is preserved and restored on Ctrl-N past the newest entry — DONE.
- Commit / rollback prompt for `auto_tx` writes: `conn::run_in_tx_open`
  leaves the transaction open on success; `Mode::TxDecision` (header
  badge + footer prompt) blocks input until `y` commits or `n`/`esc`
  rolls back. `conn::tx_commit` / `tx_rollback` finish via a
  `TxClosed` message — DONE.
- Log import: `F8` in the editor parses the buffer through
  `query::hibernate::parse` and `query::pglog::parse`; `Mode::LogPick`
  shows a list of reconstructed queries; Enter loads the selection's
  `runnable_sql` into the editor — DONE.
- IntelliJ integration: `creds::intellij` parses `.idea/dataSources.xml`,
  yields data sources with name / jdbc-url / user; `detect_intellij_project`
  + `jdbc_to_dsn`. Startup logs the discovered sources to `pgman.log`
  alongside the existing Spring detection — DONE. Follow-up: wire into a
  connection picker (currently just informational; `--rds`-style handoff
  with `--intellij <name>` would close the loop).
- DBUnit fixtures: `dbunit::parse_flat_xml` reads a FlatXmlDataSet;
  `generate_clean` / `generate_inserts` / `generate_apply_script`
  produce a `TRUNCATE` (or `DELETE FROM`) + `INSERT` script in correct
  FK order. `Ctrl-D` / `F9` in the editor reads the buffer as a fixture
  path and replaces it with the apply script — DONE.
- Multi-statement run: `safety::split_statements` splits on `;` outside
  string literals/comments. The editor's run path detects multi-statement
  buffers, classifies each piece, takes the most-restrictive guard, and
  routes through `conn::run_batch` / `run_batch_in_tx_open` (the
  `auto_tx` + commit/rollback prompt still wraps the whole batch). The
  confirm modal shows a kind-tagged summary instead of the (less useful
  for batches) single-statement classification — DONE.
- Editor vertical scrolling: the pane stays capped at 12 rows (10 content
  + 2 border) but long buffers now follow the cursor — `clamp_editor_scroll`
  computes the offset each frame to keep the cursor visible. Title shows
  `line N/M` once the buffer exceeds the pane — DONE.
- Bracketed paste: terminal wraps pasted text in escape codes; crossterm
  delivers a single `Event::Paste(String)` instead of streaming each
  character through `Event::Key`. Pasted into the editor at the cursor,
  CRLF / CR normalised to LF. Best-effort — older terminals ignore the
  enable sequence and the char-by-char path still works — DONE.
- Follow-ups: saved queries; N+1 cluster view;
  connection picker that uses IntelliJ
  data sources at startup; capture-current-state → write a fixture (the
  reverse of apply); per-database `CleanMode` config; `dataSources.local.xml`
  password integration.

## v2 — AWS (not started)

- `discover_aws.rs` + `mode_databases.rs`; `--rds <id>` launch handoff from ebman.
  (Add a one-line backlog item in *ebman* for the spawn-pgman action.)
- `rds_logs.rs` + `mode_logs.rs` — `DescribeDBLogFiles` / CloudWatch; feeds pglog.
- `creds/{ssm,secrets}.rs` + `creds/onepassword.rs` — placeholder resolution.
- `migrate/` — `MigrationStrategy` trait + `flyway` / `liquibase` / `custom`
  (config-driven table mapping; uflexi is one preset). Note: `pending()` is not
  implementable for a DB-table-only strategy — make it an optional capability.
- `mode_params.rs` — RDS parameter groups; view/diff + `ModifyDBParameterGroup`
  with confirmation; surface pending-reboot.
- `mode_dashboard.rs` — live PG stats + RDS CloudWatch metrics.
- `advisor.rs` + `mode_advisor.rs` — health-snapshot → `claude` CLI review;
  interactive handoff via `handoff.rs`. Snapshot must be scrubbable (egress).

## v2 — local DB sync & backups (not started)

Pull a remote database down for local testing; keep tagged backups.

- `snapshot.rs` — snapshot store under `util::data_dir()`: each snapshot is a
  `pg_dump` artifact (custom format) plus a metadata record (source DSN
  redacted, timestamp, size, db version, tag, pinned flag). A TOML/JSON index
  lists them.
- `mode_snapshots.rs` — list / create / restore / tag / pin / delete.
  - Create: `pg_dump` a remote DB → store. Capture the server major version.
  - Restore: `pg_restore` (or `psql`) into a chosen local target DB, with a
    confirm step (restore is destructive to the target).
  - Pin protects a snapshot from prune; tags group them (`pre-migration`,
    `prod-2026-05`, …).
- Version skew: `pg_dump` / `pg_restore` must be ≥ the server major version —
  detect and warn, or locate a matching client binary.
- Retention: optional prune of un-pinned snapshots past a count/age limit.
- Future (v3+): on-restore redaction / anonymisation of sensitive columns —
  a per-table/column rule set applied during restore. Listed below.

### UX backlog

- **Esc shouldn't quit** — `q` (and Ctrl-C) should be the only quit
  keys. Today Esc quits from Normal mode and from the ConnPick picker.
  Make Esc a no-op (or "close any overlay; otherwise no-op") so a
  reflex Esc never loses the session.
- **Grammar-aware completion** — current completion is a tolerant
  tokenizer + FROM-clause heuristic. A real SQL grammar (or at least a
  statement-position classifier: SELECT-list / FROM / WHERE / GROUP /
  ORDER / RETURNING) would let us:
  - in SELECT after `SELECT`: only columns of in-scope tables, plus
    aggregations
  - in FROM/JOIN position: only tables / schemas, never columns
  - in WHERE: columns of in-scope tables; suggest comparison operators
  - after `ORDER BY` / `GROUP BY`: in-scope columns
  - inside `INSERT INTO foo (`: columns of `foo`
  Probably reuse `sqlparser` crate rather than rolling our own.

## v3+ — deferred

- **Redaction / anonymisation** on snapshot restore — declarative rules
  (`null` a column, fake an email, hash an id) applied as data lands locally,
  so local testing never holds real PII.
- `catalog/` — version-aware catalog trait; EPAS support.
- JPA entity ↔ table mapping.
- Migration safety linter (lock-heavy operations).
- EXPLAIN plan diff. Minimal schema-identifier autocomplete.
