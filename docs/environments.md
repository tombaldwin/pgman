# Environments

Press `E` for the environments overview: every database in each
environment, replicas included, side by side. It's like ebman's main
screen, but for Postgres. The same rows are available to an MCP client
through the `environments` tool ([MCP server](mcp.md)).

The overview shows what the sources know without connecting to any
database: role, server, status, engine version and endpoint. Press
`Enter` on a row to connect to that database. Schema version, pending
patches and replica lag per row need a connection per database, and
come next.

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

## Connecting

`Enter` connects to the database under the cursor:

- **A replica** opens read-only, whatever `safety.toml` says. A write
  can only fail there.
- **A primary or a standalone instance** gets its database's safety
  profile, like any other connection.
- **Credentials**: with a secret configured, the password (and user) is
  read from AWS Secrets Manager when you press `Enter`, never before,
  and never stored. The status line names the secret, never its
  value. Without one, the usual sources apply: `user`, `~/.pgpass`,
  `PGPASSWORD`.
- A `static` environment's row connects with its `connections.toml`
  entry: that entry's tunnel, secret and role.

An `rds` environment's connection settings sit beside its source:

```toml
[[environments]]
name = "demo"
source = { kind = "rds", instances = ["demo"], region = "us-west-1", account = "210987654321" }
ssh_tunnel = "ec2-user@bastion.example.com"   # optional: [user@]host[:port]
sslmode = "require"                           # optional
user = "readonly"                             # optional; a secret's username wins

# Secrets Manager secret per server, or per server/database.
# Read with the source's region, profile and account.
[environments.secrets]
"demo" = "demo/database/ledger/credentials"
"demo-read-only-replica" = "demo/database/ledger_replica/credentials"
"demo/ledgerlog" = "demo/database/ledgerlog/credentials"
```

`"server/database"` beats `"server"`. A `static` environment can't take
these keys; put them on its connections in `connections.toml`.

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
| `E` | Open from the grid or the connection picker. No connection is needed. |
| `j` / `k`, `g` / `G` | Move between databases. |
| `Enter` | Connect to the database under the cursor. |
| `r` | List again. |
| `q` / `Esc` | Close, cancelling a connect that is still reading its secret. |

`status` is RDS's view of the instance (`available`, `stopped`,
`modifying`, …), not proof that Postgres answers. Stopped instances are
grey; any state other than `available` or stopped is yellow.
