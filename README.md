# pgman

A k9s-style Postgres TUI aimed at Java / AWS shops. Sibling project to
[`ebman`](https://github.com/tombaldwin/ebman).

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
unqualified `DELETE`, and wrap DML in a transaction you can roll back.

## Install

From a local checkout (recommended while pgman is private and pre-v1):

```sh
cargo install --path ~/git/pgman --locked
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

CI runs all of the above on every push (`.github/workflows/test.yml`):
the unit / render / subprocess / doc tests on linux + macos, the
integration tests against a live `postgres:16` service container, a
coverage report via `cargo-llvm-cov`, and `cargo fmt --check` +
`cargo clippy` (advisory).

## Status

Pre-v1. See `BACKLOG.md` for what's shipped and what's next.
