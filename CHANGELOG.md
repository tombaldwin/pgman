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
- **Editor** — syntax highlighting, identifier and keyword completion,
  `pg_format`, history, undo/redo, saved queries with `:param` prompts,
  `$EDITOR`, `\watch`, DBUnit fixture apply and capture.
- **Safety** — read-only by default, `statement_timeout`, per-database
  guards (block / confirm / allow) on every statement, DML wrapped in a
  rollback transaction, `EXPLAIN ANALYZE` always rolled back, and a
  project's `[safety]` block can only tighten yours.
- **DBA panels** — schema browser, schema-lint wizard, EXPLAIN tree,
  slow queries, sessions and locks, result diff, `\l`, `\x`, `\c`, `\i`,
  the `\d` family, `\timing`.
- **A `:` command bar** — `:about`, `:help`, `:update`, `:readonly`,
  `:connect`, and every `\` command; `?` opens help from any mode.
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
