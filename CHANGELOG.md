# Changelog

All notable changes to pgman are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

### Added

- The JDBC tap's List view shows the focused statement's bound
  parameters beneath the list, labelled `params (some redacted)` when
  the producer masked a value and `params (withheld by source)` for
  OpenTelemetry spans, which carry none.
- A Postgres log's `ERROR:` + `STATEMENT:` pair is reconstructed as a
  query marked failed with the error message — what the default
  `log_min_error_statement` logs with nothing else on. A statement
  `log_statement` already logged is marked failed rather than listed
  twice. The log picker marks a failed pick `✗` and shows its error.

### Changed

- One TLS crypto provider (ring) instead of two: no cmake / C build of
  aws-lc, and a smaller binary (x86_64 macOS 11.9 MB to 9.1 MB). The
  Postgres connection no longer offers the post-quantum X25519MLKEM768
  key exchange, which only the aws-lc provider had.

### Fixed

- The tap monitor's status line kept reading "no events yet" after
  events had arrived; it now follows them.
- OpenTelemetry spans were stamped with their end time rather than
  their start, which inflated the Pools view's peak concurrency.
- A burst of tap events no longer piles up in memory behind the
  screen. pgman redrew once per event, so 4 000 large events took over
  30 s to land and held more than 800 MiB on the way; they now land in
  a few frames (under a second, 100–200 MiB at peak). The hop into the
  app is bounded, so events pgman cannot keep up with are dropped and
  counted on the `DROP` badge instead of queueing without limit.
- A tap listener that cannot bind (port in use) is now reported on the
  status line and on stderr — `--tap-listen`, `--tap-udp`, `--tap-otlp`
  and the listener a Java project turns on by itself. The status line
  used to announce the port as listening while the failure went only
  to the log.
- `--tap-otlp` accepts gzip-compressed bodies, which the OpenTelemetry
  Collector's `otlphttp` exporter sends by default; they failed with
  `400 not utf-8`. The inflated body has the same 4 MiB cap as the raw
  one. Any other `Content-Encoding` gets a 415 saying to send gzip or
  none.
- A pgman-tap event claiming an absurd duration is clamped to one hour,
  as OpenTelemetry spans already were; `u64::MAX` showed as
  `18446744073709.55s`, broke the column alignment and topped every
  sort by time.
- The tap List view's app column is cut by display width, so a CJK app
  name no longer pushes its row's SQL out of line. A name too long for
  the column ends in `…`.
- The tap monitor's status line says `1 query · 1 heartbeat`, not
  `1 queries · 1 heartbeats`, as its title already did.
- `--tap-listen`, `--tap-udp` and `--tap-otlp` take a host name
  (`localhost:7432`), which failed as `invalid socket address syntax`.
  A name that does not resolve says to use an IP address instead.
- The tap monitor's empty state and status line say where pgman is
  listening (`listening on TCP 127.0.0.1:7432 · OTLP/HTTP …`). They
  told the operator to start pgman with `--tap-otlp :4318` even with a
  listener — that one included — already bound; the hint now points
  the JVM at the bound OTLP port.
- The tap monitor's title is no longer cut mid-word by its border: the
  room it was fitted to left out the decoration round the panel name.
- A producer's huge drop counter no longer pushes the tap monitor's key
  hints off its title: counts from 10 000 up read `12.3k`, `4.5M`, …,
  `1.0e18`, and the drop count now comes after the hints, so a narrow
  panel gives it up first (the status line still shows it).
- The tap monitor's OpenTelemetry hint gives
  `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318/v1/traces`.
  Without the path, which the exporter does not add to that variable,
  the agent's exports got a 404.
- A write refused by a read-only connection no longer puts the full
  `safety.toml` path in the footer, where at 80 columns it took three
  rows and was cut anyway. The footer names `safety.toml`; the F2
  overlay and `--batch` stderr give the path.
- A completion popup the editor opened on its own (after `.` or `FROM `)
  that narrows to nothing as you type on now closes without leaving
  `completion: no matches for …` in the footer. A popup Tab opened still
  says why it closed.
- **NULL and the empty string no longer look the same.** The grid
  draws a NULL as a dimmed `NULL` (a new theme colour, `null_cell`) and
  an empty string as nothing; row and cell detail say `NULL` and
  `(empty string)`. The `I` INSERT yank wrote `NULL` for an empty
  string, changing the row it copied; it now writes `''`.
- **The `I` yank writes back the row it copied.** Every value is a
  quoted literal (a text `007` came back as `7`, `TRUE` as `true`),
  column names are quoted when they need it (`createdAt`), and reserved
  words are quoted everywhere pgman builds SQL (a column named `order`).
- `--format csv` and `Y` write a NULL as an empty field and an empty
  string as `""`, as `COPY … CSV` does; `--format tsv` writes a NULL as
  `\N`, as `COPY`'s text format does. `--format json` already wrote
  `null`, and now does for a multi-statement script too.
- Sort puts NULLs last ascending and first descending (Postgres's
  defaults) and an empty string first ascending, not with the NULLs.
  `/null` and `f null` find NULL cells.
- `F` on a NULL foreign key says so instead of opening `WHERE id = NULL`;
  `y` on a NULL field leaves the clipboard alone.
- The result diff counts a NULL → `''` change and shows it; `\fixture`
  leaves NULL cells out (flat XML's NULL) instead of writing `col=""`.

## [0.1.0] — 2026-10-22

First public release. pgman is a terminal UI for PostgreSQL in the k9s
mould, built for teams running Java services on AWS. Point it at a
database — or let it find the connection in your Spring
`application*.yml`, IntelliJ data sources, or a committed
`.pgman/pgman.toml` — and you get a query editor with completion and
history, a schema browser, EXPLAIN trees, slow-query and session panels,
and a results grid you can filter, sort, diff and export. What nothing
else does: paste a Hibernate log, a Postgres server log, or a JDBC
statement with its parameters, and pgman reconstructs the runnable SQL,
clusters N+1 loops, and runs it — no application changes, works offline.
Connections are read-only by default, every statement is classified and
guarded before it reaches the server, and anything discovered in a
working tree is treated as untrusted until you confirm it. A live JDBC
tap (OpenTelemetry today) shows what the application is actually
sending. Install with `brew install tombaldwin/tap/pgman`, `cargo install
pgman --locked`, or a tarball from the releases page; `pgman --demo`
runs the whole thing against synthetic data. Pre-1.0: expect rough edges
and breaking changes.

### What's in it

- **Connect** — auto-discovery from Spring `application*.yml` /
  `.properties` (with profile overlays and `${VAR}` / `${VAR:default}`
  from the environment), IntelliJ `.idea/dataSources.xml`, and a
  committed `.pgman/pgman.toml`; a picker that shows host, `sslmode`,
  tunnel and credential provenance before anything connects; a start
  card on connect with databases, sizes and the keys that matter.
- **Logs and pasted code → runnable SQL** — Hibernate logs (with
  bind-parameter trace logging), Postgres/RDS server logs, and pasted
  JDBC statement + parameters; N+1 detection; a pasted log is recognised
  in the editor; `pgman --log PATH` opens straight into the picks.
- **Editor** — Enter runs a statement that ends in `;` (psql's rule),
  Alt-Enter runs regardless; opens five lines tall and grows with its
  content; syntax highlighting, identifier and keyword completion,
  auto-indent, Ctrl-F formatting (built-in, `pg_format` when
  installed; never on run or paste), history, undo/redo, saved queries
  with `:param` prompts, `$EDITOR`, `\watch`, DBUnit fixture apply and
  capture.
- **Safety** — read-only by default, `statement_timeout`, per-database
  guards (block / confirm / allow) on every statement, DML left in an
  open transaction until you commit or roll it back, `EXPLAIN ANALYZE`
  always rolled back, and a
  project's `[safety]` block can only tighten yours. A read-only
  session also refuses the server-control functions Postgres's own
  read-only mode lets through (`pg_promote`, `pg_wal_replay_pause`,
  replication slots, `pg_stat_reset`…).
- **DBA panels** — schema browser, schema-lint wizard, EXPLAIN tree,
  slow queries, sessions and locks, result diff, `\l`, `\x`, `\c`, `\i`,
  the `\d` family, `\timing`.
- **MCP server** — `pgman mcp serve` gives Claude Code (or any MCP
  client) tools: `query`, `sessions_locks` (a pre-flight before a schema
  change), `pending_patches`, `connections`, `environments` and `doctor`,
  with more below. Every session is read-only whatever `safety.toml`
  says, except `apply_patches` (see Applying patches below);
  personal connections can tunnel and read their password from AWS
  Secrets Manager; a `primary` connection serves only the lock check.
- **Environments overview** — `E` lists every database in each
  environment, replicas under their primary, from RDS (via the `aws`
  CLI) or hand-listed connections, without connecting to any of them.
  `Enter` connects to a row: replicas read-only, the password read from
  AWS Secrets Manager at that moment, through the environment's ssh
  tunnel.
- **`pgman env discover`** proposes environments from an AWS account:
  one per RDS primary with its replicas, the secret matching each
  database by name, the bastion in its VPC. It reads names only, never a
  secret's value; `--save` appends to `environments.toml`. In the
  overview, `d` does the same with a preview of what each save appends.
  Over MCP, `environments_discover` and `environments_save` do it for an
  agent (save refused under `--read-only`).
- **ebman's look** — rounded panels with `[ ◆ name ◆ ]` titles
  (a plain ` name ` under Powerline fonts, as in ebman), ebman's two-box
  header on terminals 24 rows or taller (server, database, user,
  session, and where the connection came from, beside the current view
  and the global keys), coloured key hints in the footer, and uppercase
  panel column headers. The drawing vocabulary now lives in
  `tb-tui-common`, shared with ebman.
- **`--batch --discovered`** accepts a checkout that declares one
  database twice (identical URL and credentials) as a single candidate.
- **MCP tunnels stay open between calls** to the same database (5
  minutes idle), so a run of `query` calls pays the ssh handshake once;
  each call still gets a fresh read-only session.
- **Partitioned tables fold** in the schema browser: one `N partitions`
  row under the table instead of a screenful of partitions. A database
  with one schema opens with it expanded.
- **Slow queries says why it's empty**: without `pg_stat_statements`
  the panel stays open with how to install it, instead of flashing
  shut.
- **Esc from the results goes back to the environments overview.**
- **Environments are home.** pgman starts on the overview (with
  environments configured and no `--dsn`), full screen, on the row you
  used last. Replicas sit under each database. Probing environments
  show `SCHEMA`, `LAG`, `SESSIONS` and `SIZE` columns.
- **Connecting to RDS works for ordinary users.** The start card sized
  every database on the server, and RDS's `rdsadmin` refuses that to
  anyone but RDS, so the whole connection failed. Only databases you
  can connect to are sized now, and a failed list never fails the
  connection.
- **A `docker` environment source** — local Postgres containers, from
  `docker ps`, reached on their published port. Environments can name
  `password_env` for databases without a secret. (Earlier docs said
  `~/.pgpass` and `PGPASSWORD` applied; pgman never read them for
  environments.)
- **Probes** — `probe = true` on an environment adds each database's
  patch level, pending patches and replica lag to the overview. It reads
  through replicas (a primary with a replica isn't touched), uses one
  ssh per bastion, and every connection is read-only.
- **More at a glance in the overview.** RDS rows show the minor and
  major upgrades available (`↑16.15 · 18.6`). Without a `version_table`,
  a probe recognises Flyway, Liquibase, Prisma, sqlx, Django, Alembic,
  and Rails / golang-migrate (their shared `schema_migrations`) by their
  own tables (and counts pending Flyway files
  from `migrations_dir`). `SESSIONS` adds blocked sessions and
  transactions open over 5 minutes. A replica's detail line sketches its
  recent lag. `v` shows schema drift: each database's version by
  environment.
- **Recent errors from RDS logs** — `l` on a server in the overview,
  `environment_logs` over MCP: ERROR, FATAL and PANIC lines from the
  newest log files, grouped with a count. `STATEMENT` and `DETAIL` lines
  are left out, since they can carry row data.
- **Applying patches** — `P` in the overview, `apply_patches` over MCP.
  A read-only plan lists the pending patches, each checked against
  `safety.toml`; after you confirm (a protected environment — `prod` in
  its name, or `protected = true` — by typing its name; over MCP through
  elicitation), each runs in its own transaction with a 5 s
  `lock_timeout`, stopping at the first failure; the result says which
  version the database records afterwards. A pending file numbered
  below the recorded version is left out and named, not run out of
  order. A patch that can't run in a transaction (`VACUUM`,
  `CONCURRENTLY`, …) is refused up front; in the TUI, `o` plans it to
  run outside one, statement by statement and with no statement
  timeout, behind a typed confirm. Esc twice cancels a running patch,
  between statements too: the patch rolls back. The plan names the
  risky statements (`TRUNCATE`, a `DELETE` without `WHERE`…) its one
  confirmation answers; the timeouts are set again before every
  statement, so a `DO` block can't lift them for the rest of the patch;
  a project's `[safety]` block applies here as everywhere else.
  Tab in the plan (`mode` over MCP) runs a **dry run** instead — every
  patch in one transaction, rolled back, with the same locks as the
  real run — or applies the patches to a **copy** of the database made
  on the same server, dropped afterwards.
  Never on a replica, and only on a database `safety.toml` lets write.
- **Safer and more honest, from a four-reviewer robustness pass.** A
  unicode-escaped name (`U&"pg_\0072ead_file"`) no longer slips past the
  safety classifier; MCP `query` results are capped at 16 KiB a value
  and 1 MiB in all; a DSN with two different `sslmode`s is refused;
  connects give up after 10 s; `:connect` takes a URL and never echoes
  its password; `:readonly` applies to the live session, so the header
  tells the truth. `--batch` says when a CSV/TSV export stopped at 1000
  rows (exit 3). A caught-up replica of a quiet primary shows no lag.
  Hibernate's `show_sql` console lines reconstruct. Find no longer eats
  `n`. `ctrl-l` works from the start card. `--demo`'s slow queries,
  sessions and wizard work. The help fits small terminals.
- **The rest of that pass.** Plain keys for the F-keys a Mac keyboard
  hides behind `fn`: `!` opens the error detail (`:error`, `:notify`,
  `:tap` spell F2–F4), and the footer names the key that works where
  you are. `ctrl-c` outside the editor and `ctrl-w` over a tab holding
  SQL take a second press; a `WATCH` badge shows while `\watch` runs;
  a cancel reads `cancelled`, not as the server's error. A later
  success takes a stale error off the footer, and the confirm modal
  keeps its `y`/`n` keys. `r` in the schema browser reloads the schema
  (tables other sessions made), and an empty cache on a live connection
  reloads by itself; `\d` on a name nothing matches says so. Esc after
  Tab gives back what you typed, and Enter runs a finished statement
  under a popup nobody picked from. Bracketed IPv6 hosts parse
  (`postgres://u@[::1]:5433/db`); `\i` refuses files over 8 MB and
  anything that isn't a regular file; `--log -` reads a piped log and
  takes keys from the terminal; `--demo` honours `--log`.
  Environments: Aurora readers sit under their writer; docker finds
  postgis and registry-prefixed images, IPv6-only and ranged ports; a
  dirty golang-migrate version shows as failed; a text id in a version
  table shows instead of an error; drift compares Liquibase, Django and
  Alembic versions as names; `migrations_dir` takes Flyway `locations`,
  recursively, Java and Kotlin migrations included. Long lines in the
  patch and log views wrap instead of being cut, the detail line cuts
  an endpoint at a dot, and the pickers sit centred.
- **The backlog pass.** `\df [pattern]` lists functions with psql's
  patterns. In the schema browser, tables list their indexes, `p`
  previews five rows and `d` copies the `CREATE` statement rebuilt from
  the catalog. LINT107 flags a serial or identity column nothing keeps
  unique. Right after `JOIN … ON`, completion offers the foreign-key
  predicate. `pgman query 'SQL' [--json]` runs one statement without
  the TUI. `sslmode=verify-ca` checks the certificate chain but not the
  hostname, as libpq does. Safety: a `WHERE` inside a subquery no
  longer qualifies an `UPDATE` / `DELETE`, and a non-ASCII dollar-quote
  tag is read as Postgres reads it. The `T` / `L` panels show numeric,
  date, interval and array values instead of `?`. A long error wraps
  onto up to three footer rows; the connection-failure card wraps its
  paths whole so they can be copied; `--demo` opens with the tap quiet
  and its burst arrives two seconds in. The update check caps a
  response while reading it; `--upgrade` refuses rather than running
  from the temp directory; `--batch` repairs pgman's directories to
  owner-only as the TUI does.
- **`schema_lint` over MCP** — the schema wizard's findings (missing
  primary keys, unindexed foreign keys, unused indexes, bloat, …) for an
  agent to work through.
- **A `:` command bar** — `:about`, `:help`, `:update`, `:readonly`,
  `:connect`, and every `\` command; `?` opens help from any mode.
- **Layout you control** — `z` zooms the results and `Z` the editor to
  the whole screen and back; `+` `-` `0` size the editor; a tab bar
  that is always there, labelled by each tab's query, switched with
  `]` `[` and the digits, `ctrl-t` / `ctrl-w` to open and close, and
  `ctrl-]` from inside the editor. Every one of these is a key a Mac
  terminal actually delivers; the Alt chords remain as secondaries.
- **Row detail steps rows** — `J` / `K` (or PageDown / PageUp) move to
  the next and previous row without leaving the view, keeping the
  field you had highlighted, and the grid selection follows.
- **JDBC tap** — live app-side query observability via OpenTelemetry
  (TCP and UDP listeners for the forthcoming `pgman-tap` JAR), hotspots,
  per-caller and per-pool rollups, transaction view, baseline diff,
  live N+1 detection, Markdown / HTML report export.
- **Batch mode** — `--batch --sql … --format csv|tsv|json|expanded`
  for scripts and CI, with typed JSON and the same guards; `--yes`
  confirms guarded writes and never lifts read-only.
- **Install and upgrade** — Homebrew, crates.io, or a release tarball
  for four targets with build provenance; `pgman --upgrade` knows
  which channel installed it; an update check at most every six hours
  that never delays the first frame (`--no-update-check`).
- **`--demo`** — the whole tool against synthetic data, no database,
  answering queries through the real guards. Its database is `ledger`,
  the billing database of a made-up subscriptions product: 26 relations
  across `public` and `reporting` (partitioned usage events, views,
  identity keys, partial indexes), answers that honour a WHERE's
  `column = value` terms, EXPLAIN plans from its row estimates, and
  four environments — production with a replica, staging, a stopped
  dev, a local container — whose versions, drift, pending patches,
  log errors, sessions and slow queries tell one story.
- **Redaction mode** — `--redact`, `:redact` or `redact_default = true`
  blocks out hosts, IPs, account IDs, ARNs and every server, environment
  and secret name pgman knows, on every screen, for screenshots of a
  real database; schema and data stay visible.

### Hardening before release

A five-reviewer release panel, run four times, found and fixed — each
with a test demonstrated failing first — a hostile-checkout trust model
(a repo's config could once auto-connect with your `PGPASSWORD`, resolve
an environment variable into a hostname, or relax your safety profile);
a statement lexer that now understands quoted identifiers, `$` and
non-ASCII in identifiers, `E''` strings and nested comments, refuses a
script it cannot verify, and executes exactly the statements it
checked; passwords with `/ @ ? #` parsed and redacted correctly; an
unknown `sslmode` refused rather than silently downgraded; a read-only
session that cannot be flipped from inside; bounded and throttled JDBC
tap ingest; owner-only files from the first byte; escapes stripped from
reports and batch output; the placeholder resolver and the DSN parser
agreeing on where a URL's host starts, or refusing the URL; the
read-only floor closed against `set_config`, a quoted GUC name, `DO`
and `CALL`, and a `COMMIT` mid-script (batch runs inside one explicit
transaction); the picker naming where every credential comes from;
decision modals that no global chord can escape; `--demo` never
touching your saved queries; and a few dozen first-minute polish
defects found by rendering every screen at five terminal sizes and
by driving the release binary against a live server.

### Dependencies

ratatui 0.30, crossterm 0.29, reqwest 0.12 (rustls only), `tb-tui-common`
0.2 shared with ebman. `toml` 1.x is used but saved-query files are
written with TOML 1.0-compatible escapes, pinned by test.

[Unreleased]: https://github.com/tombaldwin/pgman/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tombaldwin/pgman/releases/tag/v0.1.0
