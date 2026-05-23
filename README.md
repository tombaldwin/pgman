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

## Status

Pre-v1. See `BACKLOG.md` for what's shipped and what's next.
