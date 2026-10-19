# Environments

Press `E` for the environments overview: every database in each
environment, replicas included, side by side. It's like ebman's main
screen, but for Postgres. The same rows are available to an MCP client
through the `environments` tool ([MCP server](mcp.md)).

By default the overview shows what the sources know without connecting
to any database: role, server, status, engine version and endpoint. An
environment with `probe = true` also gets a `probe` column: its patch
level, pending patches and replica lag, read from each database (see
[Probes](#probes)). Press `Enter` on a row to connect to that database.

```
staging · RDS 123456789012 eu-west-2
  database  role     server             status     engine         endpoint
  shop      primary  staging            available  postgres 16.4  staging.….rds.amazonaws.com:5432
  shop      replica  └ staging-replica  available  postgres 16.4  staging-replica.….rds.amazonaws.com:5432
```

## `~/.config/pgman/environments.toml`

Environments live in your own config dir, never in a project file:
like `connections.toml`, they name accounts and hosts you chose.
pgman only ever appends to it, when you save a discovery, and never
rewrites what's already there.

## Discovering environments from AWS

In the overview, press `d`. pgman discovers with your ambient AWS
profile and region, and lists one proposal per primary. `p` discovers
again in the next scope: each one your environments already name, then
each profile from `aws configure list-profiles`. The heading says which
scope you're looking at. `space` chooses
a proposal, and the lower half shows exactly what `s` will append for
the one under the cursor, notes included. `s` saves the chosen ones and
returns to the overview, which lists them at once. `q` backs out
without saving.

From a shell, with a choice of profile, region and account:

```sh
pgman env discover                       # print proposals as TOML
pgman env discover --save                # append every new one
pgman env discover --save demo test      # append just these
pgman env discover --region eu-west-2 --profile staging --account 123456789012
```

Discovery reads names and endpoints only. It doesn't connect to any
database or read any secret's value:

- **One environment per RDS Postgres primary**, named after it, with
  its read replicas. A replica whose primary isn't listed gets an
  environment of its own.
- **Databases** come from secret paths like
  `<env>/database/<db>/credentials` when there are any, otherwise from
  the instance's `DBName`.
- **Secrets** are matched by name. The first path segment is the
  environment, or a prefix of it (`dev` for `development`) that isn't
  another environment's name. A segment names the database, and a
  replica prefers a `<db>_replica` secret. Master-password secrets like
  `demo-database-password` never match. A tie isn't guessed: it becomes
  a note listing the candidates.
- **The ssh tunnel** goes through the running EC2 host with a public DNS
  name in the instance's VPC. With several, the one named
  `bastion`/`jump` wins; otherwise a note lists them. The ssh user
  comes from `~/.ssh/config`.
- `sslmode = "require"`, and the account (from `sts
  get-caller-identity`) and region are pinned.
- **`version_table`**: when you discover from inside a project whose
  `.pgman/pgman.toml` has `[patches]`, each proposal gets that table,
  column and patch directory, ready for [probes](#probes). `probe`
  itself stays off; a note says how to turn it on, because it connects
  to each database on every refresh.

Every proposal carries its notes as comments; read them before saving.
A primary that an existing environment already lists (by name or
`instances` glob) is shown as covered and never saved twice. Without
permission to list secrets or EC2 hosts, discovery still proposes
environments, without secrets or tunnels, and says why.

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

- **A replica** opens read-only, whatever `safety.toml` says, and stays
  that way for the session: `\c <db>` keeps it, and `:readonly off` is
  refused. A write can only fail there.
- **A primary or a standalone instance** gets its database's safety
  profile, like any other connection.
- **Credentials**: with a secret configured, the password (and user) is
  read from AWS Secrets Manager when you press `Enter`, never before,
  and never stored. The status line names the secret, never its
  value. Without one, `password_env = "VAR"` reads the password from
  that environment variable when you connect. pgman reads neither
  `~/.pgpass` nor `PGPASSWORD` for environment databases.
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
password_env = "DEMO_PW"                      # optional; for databases without a secret

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
| `docker` | Local containers that expose 5432 or run a `postgres` image, whose names match one of `containers` (globs; default all). One row per entry in `databases` (default `postgres`), reached on the port 5432 is published to. A stopped container, or one that doesn't publish 5432, is listed with the reason it can't connect. | The `docker` CLI on `PATH`. `user`, `password_env` and `sslmode` apply; `ssh_tunnel` and `secrets` don't. |

Environments that share an `rds` scope (region, profile, account) share
one listing per refresh. A source that fails puts its error on that
environment's heading; the other environments still show.

Cloud SQL, Azure Database and others are planned as further sources
behind the same model (BACKLOG.md).

```toml
# Local containers: the app's database in ledger-db.
[[environments]]
name = "local"
source = { kind = "docker", containers = ["ledger-*"] }
databases = ["ledger"]
user = "ledger"
password_env = "LEDGER_DB_PASSWORD"
sslmode = "disable"
```

## Probes

Probes are off unless an environment asks for them, because a refresh
then connects to every database in it:

```toml
[[environments]]
name = "demo"
# … source, ssh_tunnel, secrets as above …
probe = true
version_table = { table = "public.db_version", column = "id", patches = "~/src/app/db/patches" }
```

Every refresh (`E`, `r`, and every 30 s after `R`) then reads, from each database (and so does
the MCP `environments` tool when called with `probe: true`):

- **Version, recovery and lag**, in one round trip. A replica shows
  `lag 0.4s`; one that is not in recovery shows `NOT IN RECOVERY` in
  red, because it's a primary posing as a replica.
- **Patch level**, the highest id in `version_table`, as `v251`.
- **Pending patches**: files in `patches` whose id the table hasn't
  recorded, as `up to date` or `2 pending` (yellow). It's the same
  matching as the MCP `pending_patches` tool.

How a probe connects:

- **A primary with a replica of the same database is not connected
  to.** It reads `via replica`, and the replica answers for it. A
  primary is probed only when there's no replica to read instead.
- **One ssh per bastion.** Every database behind the same bastion rides
  one `ssh` carrying a forward each, closed when the probe ends.
- **Read-only, bounded.** Every connection is read-only, with a 5 s
  statement timeout and a 20 s overall deadline per database. A secret
  is read once per refresh, however many databases share it.
- A failure reads `✗` plus its first line in the row; the detail line
  under the table has the whole message.

## Keys

| Key | Action |
| --- | --- |
| `E` | Open from the grid or the connection picker. No connection is needed. |
| `j` / `k`, `g` / `G` | Move between databases. |
| `Enter` | Connect to the database under the cursor. |
| `R` | Probe again every 30 s while the overview is open (environments with `probe = true`; off until pressed). |
| `d` | Discover environments from AWS (`space` choose, `s` save, `p` next profile/scope, `q` back). |
| `r` | List again. |
| `q` / `Esc` | Close, cancelling a connect that is still reading its secret. |

`status` is RDS's view of the instance (`available`, `stopped`,
`modifying`, …), not proof that Postgres answers. Stopped instances are
grey; any state other than `available` or stopped is yellow.
