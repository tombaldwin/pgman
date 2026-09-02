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

![pgman demo — live filter, schema browser, saved-query :param prompt](demo.gif)

> Captured from `pgman --demo`, the synthetic-data mode baked into the binary
> (no database needed). Regenerate after a code change with `vhs demo.tape`.

At a glance:

- **Connect fast** — auto-discovers datasources from Spring
  `application*.yml`/`.properties`, IntelliJ `.idea/dataSources.xml`, and a
  committed `.pgman/pgman.toml`.
- **Logs / pasted code → runnable SQL** — Hibernate logs, Postgres/RDS server
  logs, and pasted JDBC, with N+1 detection.
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
- **Pasted JDBC → runnable SQL.** Substitute `?` placeholders with bound values.
- **N+1 detection.** Cluster reconstructed queries by shape to surface
  loop-driven selects.

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

Straight from the repo:

```sh
cargo install --git https://github.com/tombaldwin/pgman --locked
```

Or from a local checkout (handy while hacking on it):

```sh
cargo install --path . --locked
```

The binary lands at `~/.cargo/bin/pgman`, which is on `$PATH` if your shell
sources `~/.cargo/env` (rustup does this for you).

## Upgrade

```sh
pgman --upgrade
```

That's it. `--upgrade` pulls the source repo it was built from (baked in at
compile time via `CARGO_MANIFEST_DIR`), reinstalls via
`cargo install --path … --locked --force`, then `exec`s the new binary —
so the upgrade command effectively becomes the new pgman. Any other args
you passed (`--dsn`, `--theme`) are forwarded; `--upgrade` is stripped so
it doesn't loop. Run from a non-TTY (CI / piped) and it stops after
installing rather than launching a TUI with no terminal.

Subprocesses inherit stdio so you see `git pull` and `cargo install`
output live.

If you installed via `cargo install --git`, `--upgrade` will tell you to
reinstall manually — it can't know the git URL.

## Testing

```bash
cargo test                              # unit + render + subprocess + doctests
cargo test --doc                        # just the doctests
docker compose -f docker-compose.test.yml up -d
cargo test --features integration       # adds the Postgres-driving tests
docker compose -f docker-compose.test.yml down
```

Coverage (requires `cargo install cargo-llvm-cov`):

```bash
cargo llvm-cov --all-targets            # summary in the terminal
cargo llvm-cov --all-targets --html     # writes target/llvm-cov/html/
```

Fuzzing (requires nightly + `cargo install cargo-fuzz`):

```bash
cd fuzz
cargo +nightly fuzz run dsn_parse       # also: tokenize, hibernate_parse,
                                        # pglog_parse, project_parse,
                                        # safety_classify
```

Property-based tests (`cargo test`) and benchmarks (`cargo bench`)
both run on stable. See `benches/hot_paths.rs` for the perf baseline.

The integration tests cover end-to-end batch / pipe mode against a
real Postgres on `127.0.0.1:55432`. Subprocess paths (`\e` editor,
`pg_format`) are covered without integration by PATH-stubbed
`tests/bin/fake_*` shell scripts. Render-path snapshots use
ratatui's `TestBackend` and inspect specific cells / strings rather
than full snapshots, so they survive minor layout shifts.

CI runs all of the above on every push (`.github/workflows/ci.yml`):
the unit / render / subprocess / doc tests on linux + macos, the
integration tests against a live `postgres:16` service container, a
coverage report via `cargo-llvm-cov`, and `cargo fmt --check` +
`cargo clippy` (advisory).

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
