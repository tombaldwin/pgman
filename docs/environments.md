# Environments

Press `E` for the environments overview: every database in each
environment, replicas included, side by side. It's like ebman's main
screen, but for Postgres. The same rows are available to an MCP client
through the `environments` tool ([MCP server](mcp.md)).

This version shows what the sources know without connecting to any
database: role, server, status, engine version and endpoint. Schema
version, pending patches and replica lag need a connection per database,
and come next.

```
staging · RDS 123456789012 eu-west-2
  database  role     server             status     engine         endpoint
  shop      primary  staging            available  postgres 16.4  staging.….rds.amazonaws.com:5432
  shop      replica  └ staging-replica  available  postgres 16.4  staging-replica.….rds.amazonaws.com:5432
```

## `~/.config/pgman/environments.toml`

Environments live in your own config dir, never in a project file:
like `connections.toml`, they name accounts and hosts you chose.
pgman never writes this file.

```toml
# Every database on the demo instance and its read replica.
[[environments]]
name = "demo"
databases = ["ledger", "ledgerlog"]    # optional; default is the instance's own DBName
source = { kind = "rds", instances = ["demo"], region = "us-west-1", account = "210987654321" }

# Hand-listed: names from connections.toml.
[[environments]]
name = "local"
source = { kind = "static", connections = ["ledger-local", "ledgerlog-local"] }
```

A key pgman doesn't know is an error, not ignored, so a typo in a filter
can't silently widen what an environment covers.

### Sources

| `kind` | Rows | Needs |
| --- | --- | --- |
| `rds` | Every Postgres instance whose identifier matches one of `instances` (globs, `*` only), plus the read replicas of every match, each listed under its primary. One row per entry in `databases`. | The `aws` CLI on `PATH`. `region`, `profile` and `account` are optional; when `account` is set, `aws sts get-caller-identity` must match it before anything is listed. |
| `static` | One row per named connection in `connections.toml`, with its role. There's no engine version or status yet. | Nothing. |

Environments that share an `rds` scope (region, profile, account) share
one listing per refresh. A source that fails puts its error on that
environment's heading; the other environments still show.

Docker, Cloud SQL, Azure Database and others are planned as further
sources behind the same model (BACKLOG.md).

## Keys

| Key | Action |
| --- | --- |
| `E` | Open from the grid. No connection is needed. |
| `j` / `k`, `g` / `G` | Move between databases. |
| `r` | List again. |
| `q` / `Esc` | Close. |

`status` is RDS's view of the instance (`available`, `stopped`,
`modifying`, …), not proof that Postgres answers. Stopped instances are
grey; any state other than `available` or stopped is yellow.
