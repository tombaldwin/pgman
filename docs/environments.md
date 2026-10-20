# Environments

Press `E` for the environments overview: every database in each
environment, replicas included, side by side. It's like ebman's main
screen, but for Postgres. The same rows are available to an MCP client
through the `environments` tool ([MCP server](mcp.md)).

It's pgman's home: with environments configured and no `--dsn`, pgman
starts on it, on the row you used last, and it fills the screen. `q`
goes to the editor and `E` comes back.

By default the overview shows what the sources know without connecting
to any database: role, server, status, engine version and endpoint.
Each database's replicas sit directly under it. An environment with
`probe = true` also gets `SCHEMA`, `LAG`, `SESSIONS` and `SIZE`, read
from each database (see [Probes](#probes)). Press `Enter` on a row to
connect to that database.

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
| `rds` | Every Postgres instance whose identifier matches one of `instances` (globs, `*` only), plus the read replicas of every match, each listed under its primary. One row per entry in `databases`. The engine column names the newest minor and major versions RDS will upgrade it to (`postgres 16.10 (↑16.15 · 18.6)`). | The `aws` CLI on `PATH`. `region`, `profile` and `account` are optional; when `account` is set, `aws sts get-caller-identity` must match it before anything is listed. |
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
# Only some databases have the table? Name them; the others show no
# patch level rather than an error:
#   version_table = { …, databases = ["ledger"] }
```

Every refresh (`E`, `r`, and every 30 s after `R`) then reads, from each database (and so does
the MCP `environments` tool when called with `probe: true`):

- **`SCHEMA`**: the patch level, the highest id in `version_table`,
  as `v251`. Patch files the table hasn't recorded show as `v251 (+2)`
  in yellow; it's the same matching as the MCP `pending_patches` tool.
- **`LAG`**, on replicas: `lag 0.4s`. Each refresh adds a reading, and
  the detail line under a replica shows the trend:
  `▁▂▂▃▅▇ · 2s–30s over 12 probes`. A replica that is streaming and has
  replayed all it received reads 0, however long its primary has been
  quiet; one whose stream is down reads the age of its last replayed
  change, so it grows. A replica that isn't in recovery
  shows `NOT IN RECOVERY` in red, because it's a primary posing as a
  replica.
- **`SESSIONS`**: backends connected to the database (the probe's own
  included), with trouble after it: `21 (2 blocked · 1 long tx)`. A
  session waiting on another's lock turns the row red; a transaction
  open over five minutes (idle in transaction included) turns it yellow.
- **`SIZE`**: the database's size.

Version, recovery, lag, sessions and size come back in one round trip.

**Without a `version_table`**, a probe recognises the common migration
tools by the table each keeps, and `SCHEMA` shows that tool's version:

| Tool | Table | `SCHEMA` shows |
|---|---|---|
| Flyway | `flyway_schema_history` | `flyway 2.3.1`: the newest successful version. A failed migration shows `✗` in red. With `migrations_dir` naming its `V<version>__*.sql` directory, pending ones show as `(+2)`. |
| Liquibase | `databasechangelog` | the last changeset run |
| Prisma | `_prisma_migrations` | the last finished migration; a failed one shows `✗` |
| sqlx | `_sqlx_migrations` | the highest version; a failed one shows `✗` |
| Django | `django_migrations` | the last `app.migration` applied |
| Alembic | `alembic_version` | the current revision |
| Rails, golang-migrate | `schema_migrations` | the highest version |

```toml
[[environments]]
name = "staging"
# … source and connection settings …
probe = true
migrations_dir = "~/src/app/src/main/resources/db/migration"   # Flyway only, optional
```

How a probe connects:

- **A primary with a replica of the same database is not connected
  to.** Its `SCHEMA` is the replica's (physical replication makes them
  the same), and the detail line says so. `SESSIONS` and `SIZE` stay
  blank, because they're per server. A primary is probed only when
  there's no replica to read instead.
- **A server that isn't running** (stopped, starting, or a container
  that exited) isn't probed. Its status says why.
- **One ssh per bastion.** Every database behind the same bastion rides
  one `ssh` carrying a forward each, closed when the probe ends.
- **Read-only, bounded.** Every connection is read-only, with a 5 s
  statement timeout and a 20 s overall deadline per database. A secret
  is read once per refresh, however many databases share it.
- A failure reads `✗` plus its first line in the row; the detail line
  under the table has the whole message.

## Applying patches

With a `version_table` that names its `patches` directory, an
environment's pending patches can be applied — `P` on a database in the
overview, or `apply_patches` over [MCP](mcp.md). Either way it is two
steps:

1. **A plan, read-only.** The patches the database hasn't recorded, in
   order, each checked against `safety.toml` like a `--batch` script.
   The plan stops before the first patch that can't run here: the
   database's profile is read-only (the default — set
   `read_only = false` for it), a guard blocks a statement, the patch
   has its own `BEGIN`/`COMMIT`, or it builds an index `CONCURRENTLY`.
   Those you apply by hand.
2. **Your confirmation, then the patches.** Each runs in its own
   transaction with `lock_timeout = 5s`, so a busy table fails fast
   instead of queueing behind live traffic. Before each one the version
   table is read again; a patch someone else applied since the plan
   stops the run. The first failure rolls that patch back and stops.
   What runs is the text you were shown, not a fresh read of the file.

A replica is never patched: its primary is. An environment is
**protected** when it says `protected = true`, or — when it says
nothing — when its name contains `prod`. A protected environment is
confirmed by typing its name; `protected = false` turns that off.

## Keys

| Key | Action |
| --- | --- |
| `E` | Open from the grid or the connection picker. No connection is needed. |
| `j` / `k`, `g` / `G` | Move between databases. |
| `Enter` | Connect to the database under the cursor. |
| `P` | Apply the database's pending patches: a plan to confirm (`y`, or the environment's name when it's protected), then each patch in its own transaction. See [applying patches](#applying-patches). |
| `l` | Recent errors from the focused server's RDS log (ERROR, FATAL, PANIC from the newest files, grouped with a count). `STATEMENT` lines are left out. |
| `v` | Schema drift: each database's version by environment, behind or pending in yellow. `v` again for the list. |
| `R` | Probe again every 30 s while the overview is open (environments with `probe = true`; off until pressed). |
| `d` | Discover environments from AWS (`space` choose, `s` save, `p` next profile/scope, `q` back). |
| `r` | List again. |
| `q` / `Esc` | Close, cancelling a connect that is still reading its secret. |

`status` is RDS's view of the instance (`available`, `stopped`,
`modifying`, …), not proof that Postgres answers. Stopped instances are
grey; any state other than `available` or stopped is yellow.
