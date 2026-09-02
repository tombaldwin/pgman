# pgman

[![CI](https://github.com/tombaldwin/pgman/actions/workflows/ci.yml/badge.svg)](https://github.com/tombaldwin/pgman/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![status: public beta](https://img.shields.io/badge/status-public%20beta-orange.svg)](https://github.com/tombaldwin/pgman/releases)

A k9s-style Postgres TUI aimed at Java / AWS shops. Sibling project to
[`ebman`](https://github.com/tombaldwin/ebman).

> ### ⚠️ Public beta
> pgman is pre-1.0 — expect rough edges and breaking changes before 1.0.
> It defaults to **read-only** connections and routes every write through
> per-statement safety guards, but don't point it at a production database
> without reviewing your `safety.toml` first. Bug reports and feedback very
> welcome — [open an issue](https://github.com/tombaldwin/pgman/issues).

![pgman demo — paste a Hibernate log, F8 to reconstruct queries, N+1 clusters, load + run](demo.gif)

> Captured from `pgman --demo`, the synthetic-data mode baked into the binary
> (no database needed). Regenerate after a code change with `vhs demo.tape`.

At a glance:

- **Connect fast** — auto-discovers datasources from Spring
  `application*.yml`/`.properties`, IntelliJ `.idea/dataSources.xml`, and a
  committed `.pgman/pgman.toml`.
- **Logs / pasted code → runnable SQL** — Hibernate logs, Postgres/RDS server
  logs, and pasted JDBC, with N+1 detection. Paste a log into the editor and
  press F8 (the start card names it), or launch straight into it with
  `pgman --log app.log`.
- **Safe by default** — read-only connections, `statement_timeout`, and
  per-database guard rails on every statement.
- **Editor** — syntax highlighting, `pg_format`, history, saved queries
  (`:param` prompts, rename, search), DBUnit fixture apply + capture.
- **DBA panels** — schema browser, EXPLAIN tree, slow queries, active
  sessions/locks, schema-lint wizard, result diff.
- **JDBC tap** — live app-side query observability (TCP / UDP / OTLP) with
  hotspots, per-caller / per-pool rollups, transaction view, and live N+1
  detection.

## The wedge

Point it at a Postgres database, then turn logs and pasted code into runnable SQL:

- **Hibernate logs → runnable SQL.** Reconstruct executable statements from
  `org.hibernate.SQL` lines plus the separately-logged bind parameters.
- **Postgres / RDS server logs → runnable SQL.** Reconstruct from `log_statement`
  output plus `DETAIL: parameters: $1 = …` lines (the more reliable source — it
  needs no application redeploy).
- **N+1 detection.** Cluster reconstructed queries by shape to surface
  loop-driven selects.

Two ways in: paste a log into the editor and press F8 (or `ctrl-l`), or skip
the paste and launch straight into the reconstructed queries with
`pgman --log app.log`. Walkthrough: [`docs/logs-to-sql.md`](docs/logs-to-sql.md).

Run it inside a Spring project and it picks up `spring.datasource.*` to connect.

## Project config (commit this)

Drop a `.pgman/pgman.toml` at the root of your repo. pgman walks up from the
current directory to find it, so launching `pgman` from any subdirectory of
the project works.

```toml
# .pgman/pgman.toml — commit this. No passwords here.
# Passwords come from PGPASSWORD or per-connection password_env.

[[connections]]
name = "local"
url  = "postgres://postgres@localhost:5432/myapp"

[[connections]]
name = "staging"
url  = "postgres://stg-db.internal:5432/myapp"
user = "app"
password_env = "STAGING_DB_PASSWORD"   # optional override of PGPASSWORD

# Per-database safety overrides. Project values win on collision, so you can
# commit just `[safety.databases.production]` and keep your personal
# `~/.config/pgman/safety.toml` defaults for everything else.
[safety.databases.production]
read_only = true
statement_timeout_ms = 5000
```

Project connections show up in the startup picker alongside any IntelliJ
data sources found in `.idea/dataSources.xml`.

## Safety

pgman connects to production databases. It opens read-only by default, enforces
a `statement_timeout`, classifies every statement, and applies **per-database
guard rails** (`safety.rs`) — e.g. block `DROP`, confirm `TRUNCATE` /
unqualified `DELETE`, and wrap DML in a transaction you can roll back. (This is
a guard rail, not a replacement for least-privilege database roles — scope the
role you connect with.)

Two things worth knowing:

- **TLS:** `sslmode=require` / `prefer` encrypt the connection but do **not**
  verify the server certificate (matching libpq). Use `sslmode=verify-full` on
  untrusted networks.
- **JDBC tap:** the `--tap-listen` / `--tap-otlp` / `--tap-udp` listeners are
  unauthenticated and bind to `127.0.0.1` by default. Only bind a non-loopback
  address on a trusted/firewalled network.

## Install

**Homebrew (macOS / Linux):**

```sh
brew tap tombaldwin/tap
brew install pgman
```

**Cargo:**

```sh
cargo install pgman --locked
```

**Pre-built binary:** download the tarball for your platform from the
[GitHub Releases page](https://github.com/tombaldwin/pgman/releases), extract,
and put `pgman` on your `PATH`. Built for `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, and
`x86_64-apple-darwin`.

The crates.io and Homebrew-tap routes go live with v0.2.0. Until then, or if
you want to hack on it, install from a checkout:

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
`cargo install` install, and a Homebrew install. A standalone binary (a
downloaded release tarball) has no in-place upgrade — `--upgrade` prints the
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
- [`docs/safety-and-privacy.md`](docs/safety-and-privacy.md) — safety guard rails, what's stored locally.
- [`docs/logs-to-sql.md`](docs/logs-to-sql.md) — the logs → runnable SQL walkthrough.
- [`docs/development.md`](docs/development.md) — build / test / clippy + distribution notes.
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — module map and the invariants the compiler doesn't enforce.

## Status

Pre-v1. See `BACKLOG.md` for what's shipped and what's next.

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Unless you explicitly state otherwise, any contribution you
submit for inclusion shall be dual-licensed as above, without additional terms.

---

Built by [Polymorphism Ltd](https://polymorphism.co.uk). pgman is one of the
tools we build for ourselves and our clients — if you want this kind of
internal developer tooling for your team, get in touch.
