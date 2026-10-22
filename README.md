# pgman

[![CI](https://github.com/tombaldwin/pgman/actions/workflows/ci.yml/badge.svg)](https://github.com/tombaldwin/pgman/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![status: beta](https://img.shields.io/badge/status-beta-orange.svg)](#install)

A k9s-style Postgres TUI aimed at Java / AWS shops. Where psql, pgcli, and
DataGrip stop at the database, pgman also turns Hibernate logs, Postgres
server logs, and pasted JDBC into runnable SQL, offline and with zero
application changes — with N+1 detection built in. Sibling project to
[`ebman`](https://github.com/tombaldwin/ebman).

> ### ⚠️ Beta
> pgman is beta — expect rough edges and breaking changes. It defaults to
> **read-only** connections and routes every write through per-statement
> safety guards, but don't point it at a production database without
> reviewing your `safety.toml` first. Bug reports and feedback very welcome —
> [open an issue](https://github.com/tombaldwin/pgman/issues).
>
> Install: `brew install tombaldwin/tap/pgman` or `cargo install pgman --locked`
> ([more](#install)) · try it with no database: `pgman --demo`.

![pgman demo — paste a Hibernate log, ctrl-l to reconstruct queries, N+1 clusters, load + run](https://raw.githubusercontent.com/tombaldwin/pgman/main/demo.gif)

> Captured from `pgman --demo`, the synthetic-data mode baked into the binary
> (no database needed). Regenerate after a code change with `vhs demo.tape`.

At a glance:

- **Every environment on one screen** — `E` lists each environment's
  databases (dev, staging, prod…) with replicas under their primary, from
  RDS, docker or hand-listed connections, without connecting. `Enter`
  connects (replicas read-only, the password read from AWS Secrets Manager
  at that moment, through the bastion when there is one); opt-in probes add
  versions, migration level, pending patches, lag, sessions and size;
  `pgman env discover` proposes the whole file from an AWS account. See
  [`docs/environments.md`](docs/environments.md).
- **Applying patches** — `P` plans an environment's pending patches
  read-only, checks each against your guard rails, and after you confirm
  runs each in its own transaction; a dry run or a throwaway copy rehearses
  it first. `l` shows the RDS server's recent errors.
- **For your coding agent** — `pgman mcp serve` gives Claude Code (or any
  MCP client) read-only tools for queries, lock pre-flights, pending
  patches, schema lint and environments, plus `apply_patches`, the one
  that writes to a database, confirmed by you. See
  [`docs/mcp.md`](docs/mcp.md).
- **Connect fast** — auto-discovers datasources from Spring
  `application*.yml`/`.properties`, IntelliJ `.idea/dataSources.xml`, and a
  committed `.pgman/pgman.toml`.
- **Logs / pasted code → runnable SQL** — Hibernate logs, Postgres/RDS server
  logs, and pasted JDBC, with N+1 detection — see "Logs and pasted code →
  runnable SQL" below.
- **Safe by default** — read-only connections, `statement_timeout`, and
  per-database guard rails on every statement.
- **Editor** — syntax highlighting, completion, formatting, history, saved
  queries (`:param` prompts, rename, search), DBUnit fixture apply + capture.
- **DBA panels** — schema browser, EXPLAIN tree, slow queries, active
  sessions/locks, schema-lint wizard, result diff.
- **JDBC tap** — live app-side queries from any JVM over OpenTelemetry, with
  hotspots and live N+1 detection. A dedicated Java library adding
  per-caller, per-pool and per-transaction views is planned.
- **Screenshots** — `--demo` runs the whole tool against a synthetic billing
  database, and `--redact` blocks out hosts, accounts and server names on a
  real one.

## Logs and pasted code → runnable SQL

Point it at a Postgres database, then turn logs and pasted code into runnable SQL:

- **Hibernate logs → runnable SQL.** Reconstruct executable statements from
  `org.hibernate.SQL` lines plus the separately-logged bind parameters. Needs
  bind-parameter trace logging turned on (`org.hibernate.orm.jdbc.bind` at
  `TRACE` on Hibernate 6, `org.hibernate.type.descriptor.sql.BasicBinder` at
  `TRACE` on Hibernate 5) — without it you still get the statement, just with
  `?` placeholders instead of substituted values.
- **Postgres / RDS server logs → runnable SQL.** Reconstruct from `log_statement`
  output plus `DETAIL: parameters: $1 = …` lines (the more reliable source — it
  needs no application redeploy).
- **Pasted JDBC → runnable SQL.** No log needed — a `?`-placeholder statement
  plus a typed parameter list (`TYPE:value` per line) reconstructs the same way.
- **N+1 detection.** Cluster reconstructed queries by shape to surface
  loop-driven selects.

Three ways in: paste a log into the editor, press `ctrl-l` (or F8), pick a
query — `Enter` loads it terminated with `;`, and a second `Enter` runs it; skip
the paste and launch straight into the reconstructed queries with
`pgman --log app.log`; or, with no log at all, paste a `?`-placeholder
statement plus its typed parameter list. Walkthrough:
[`docs/logs-to-sql.md`](docs/logs-to-sql.md).

Run it inside a Spring project and it picks up `spring.datasource.*` to connect.

## Project config (commit this)

Drop a `.pgman/pgman.toml` at the root of your repo. pgman walks up from the
current directory to find it, so launching `pgman` from any subdirectory of
the project works.

```toml
# .pgman/pgman.toml — commit this. No passwords here.
# Passwords come from the variable a connection's password_env names.
# PGPASSWORD is only used with --dsn, never for a discovered connection.

[[connections]]
name = "local"
url  = "postgres://postgres@localhost:5432/myapp"

[[connections]]
name = "staging"
url  = "postgres://stg-db.internal:5432/myapp"
user = "app"
password_env = "STAGING_DB_PASSWORD"   # env var holding the password

# Per-database safety overrides. These can only TIGHTEN your personal
# `~/.config/pgman/safety.toml` — a committed file can't relax your guard
# rails. Commit just `[safety.databases.production]` and your own defaults
# still apply everywhere else.
[safety.databases.production]
read_only = true
statement_timeout_ms = 5000
```

Project connections show up in the startup picker alongside any IntelliJ
data sources found in `.idea/dataSources.xml`. Nothing in that picker
connects until you choose it — see [Safety](#safety).

## Safety

pgman connects to production databases. It opens read-only by default, enforces
a `statement_timeout`, classifies every statement, and applies **per-database
guard rails** — e.g. block `DROP`, confirm `TRUNCATE` /
unqualified `DELETE`, and wrap DML in a transaction you can roll back. (This is
a guard rail, not a replacement for least-privilege database roles — scope the
role you connect with.)

Three things worth knowing:

- **Running pgman inside a checkout you did not write is a trust decision.**
  pgman discovers connections from files in the working tree, so nothing it
  finds there connects without a keypress, `PGPASSWORD` is only used with
  `--dsn`, and a project's `[safety]` block can only *tighten* your own guard
  rails — never relax them. Full trust model in
  [docs/safety-and-privacy.md](docs/safety-and-privacy.md#running-pgman-inside-a-checkout-you-did-not-write).
- **TLS:** `sslmode=require` / `prefer` encrypt the connection but do **not**
  verify the server certificate (matching libpq). Use `sslmode=verify-full` on
  untrusted networks. `sslmode=verify-ca` checks the chain but not the hostname,
  as libpq does.
- **JDBC tap:** the `--tap-listen` / `--tap-otlp` / `--tap-udp` listeners are
  unauthenticated and bind to `127.0.0.1` by default. Only bind a non-loopback
  address on a trusted/firewalled network.

## Install

**Homebrew (macOS / Linux):**

```sh
brew install tombaldwin/tap/pgman
```

**Cargo:**

```sh
cargo install pgman --locked
```

**Pre-built binary:** download the tarball for your platform (Linux and
macOS, x86_64 and arm64) from the
[releases page](https://github.com/tombaldwin/pgman/releases/latest), check
the `*.sha256` next to it, extract, and put `pgman` on your `PATH`. Each
tarball also carries a build-provenance attestation:
`gh attestation verify pgman-*.tar.gz --repo tombaldwin/pgman`.

Then try it with no database: `pgman --demo`.

To hack on it, install from a checkout:

```sh
cargo install --path . --locked
```

The binary lands at `~/.cargo/bin/pgman`, which is on `$PATH` if your shell
sources `~/.cargo/env` (rustup does this for you).

## Upgrade

```sh
pgman --upgrade
```

Works in place for a checkout (`git pull` + `cargo install --path .`), a
crates.io `cargo install` install, and a Homebrew install. An install made
with `cargo install --git …` isn't one of these: re-run that
`cargo install --git` command to upgrade it. A standalone binary (a downloaded release tarball) has no
in-place upgrade — `--upgrade` prints the
[releases page](https://github.com/tombaldwin/pgman/releases/latest) instead.

pgman also checks crates.io for a newer release at most once every six hours
and shows a header badge when one exists. Turn that off with
`--no-update-check` (or `PGMAN_NO_UPDATE_CHECK`); it never blocks startup and
degrades silently if there's no network.

## Testing

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

See [`docs/development.md`](docs/development.md) for the integration suite,
coverage, fuzzing, and what CI runs on every push.

## Docs

- [`docs/keys.md`](docs/keys.md) — every keybinding, by mode.
- [`docs/commands.md`](docs/commands.md) — command reference.
- [`docs/configuration.md`](docs/configuration.md) — config file locations and options.
- [`docs/environments.md`](docs/environments.md) — the environments view, `environments.toml` and `pgman env discover`.
- [`docs/mcp.md`](docs/mcp.md) — `pgman mcp serve`, the MCP server for coding agents.
- [`docs/safety-and-privacy.md`](docs/safety-and-privacy.md) — safety guard rails, what's stored locally.
- [`docs/logs-to-sql.md`](docs/logs-to-sql.md) — the logs → runnable SQL walkthrough.
- [`docs/development.md`](docs/development.md) — build / test / clippy + distribution notes.
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — module map and the invariants the compiler doesn't enforce.

## Status

Beta. See `BACKLOG.md` for what's next.

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Unless you explicitly state otherwise, any contribution you
submit for inclusion shall be dual-licensed as above, without additional terms.

---

Built by [Polymorphism Ltd](https://www.poly.io). pgman is one of the
tools we build for ourselves and our clients — if you want this kind of
internal developer tooling for your team, get in touch.
