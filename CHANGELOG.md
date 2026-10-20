# Changelog

All notable changes to pgman are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

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
  guards (block / confirm / allow) on every statement, DML wrapped in a
  rollback transaction, `EXPLAIN ANALYZE` always rolled back, and a
  project's `[safety]` block can only tighten yours.
- **DBA panels** — schema browser, schema-lint wizard, EXPLAIN tree,
  slow queries, sessions and locks, result diff, `\l`, `\x`, `\c`, `\i`,
  the `\d` family, `\timing`.
- **MCP server** — `pgman mcp serve` gives Claude Code (or any MCP
  client) read-only tools: `query`, `sessions_locks` (a pre-flight before
  a schema change), `pending_patches`, `connections`, `environments` and
  `doctor`. Every session is read-only whatever `safety.toml` says;
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
  `lock_timeout`, stopping at the first failure. Never on a replica, and
  only on a database `safety.toml` lets write.
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
  answering queries through the real guards.

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
