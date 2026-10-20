# Environments

Press `E` for the environments overview: every database in each
environment, replicas included, side by side. It's like ebman's main
screen, but for Postgres. The same rows are available to an MCP client
through the `environments` tool ([MCP server](mcp.md)).

It's pgman's home: with environments configured and no `--dsn`, pgman
starts on it, on the row you used last, and it fills the screen. `q`
goes back to where you opened it from (the results, or the connection
picker), `E` comes back, and `Esc` from the results returns to it.

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
| `rds` | Every Postgres instance whose identifier matches one of `instances` (globs, `*` only), plus the read replicas of every match, each listed under its primary. One row per entry in `databases` (default: the instance's `DBName`). Aurora instances carry no `DBName` — it's on the cluster — so give `databases`; Aurora readers aren't yet told apart from the writer. The engine column names the newest minor and major versions RDS will upgrade it to (`postgres 16.10 (↑16.15 · 18.6)`). | The `aws` CLI on `PATH`. `region`, `profile` and `account` are optional; when `account` is set, `aws sts get-caller-identity` must match it before anything is listed. |
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
  A text column holding no integer ids at all (`1.2`, `1.10`) shows
  its highest as written instead of failing; applying patches still
  refuses such a table.
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
  Long transactions need the probe user to see other sessions'
  transactions (`pg_read_all_stats`, which RDS's master user has);
  without it they aren't counted rather than shown as none.
- **`SIZE`**: the database's size.

Version, recovery, lag, sessions and size come back in one round trip.

**Without a `version_table`**, a probe recognises the common migration
tools by the table each keeps, and `SCHEMA` shows that tool's version:

| Tool | Table | `SCHEMA` shows |
|---|---|---|
| Flyway | `flyway_schema_history` | `flyway 2.3.1`: the newest successful version. A failed migration shows `✗` in red. With `migrations_dir` naming its `V<version>__*.sql` directory, pending ones show as `(+2)`; see below. |
| Liquibase | `databasechangelog` | the last changeset run |
| Prisma | `_prisma_migrations` | the last finished migration; a failed one shows `✗` |
| sqlx | `_sqlx_migrations` | the highest version; a failed one shows `✗` |
| Django | `django_migrations` | the last `app.migration` applied |
| Alembic | `alembic_version` | the current revision |
| Rails, golang-migrate | `schema_migrations` | the highest version; golang-migrate's `dirty` (a migration that failed part way) shows `✗` |

```toml
[[environments]]
name = "staging"
# … source and connection settings …
probe = true
migrations_dir = "~/src/app/src/main/resources/db/migration"   # Flyway only, optional
```

`migrations_dir` takes what `flyway.locations` would: one directory or
several, comma-separated, with or without a `filesystem:` prefix. Each
is read recursively, as Flyway reads it. A directory under
`src/main/resources` also brings in the same package under
`src/main/java` and `src/main/kotlin`, so Java and Kotlin migrations
(`V3__Backfill.java`) count too. A `classpath:` location is refused:
pgman can't see a classpath, so name the directory it maps to instead.

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

1. **A plan, read-only.** The patches the database hasn't recorded and
   that are numbered above the highest version it has, in order, each
   checked against `safety.toml` like a `--batch` script. The plan stops
   before the first patch that can't run here: the database's profile
   is read-only (the default — set `read_only = false` for it), a guard
   blocks a statement, the patch has its own `BEGIN`/`COMMIT` or changes
   session settings, or it holds a statement no transaction can run
   (`VACUUM`, `CREATE INDEX CONCURRENTLY`, `CREATE DATABASE`, …; see
   below). A pending file numbered *below* the highest recorded version
   is a gap the database skipped: it's left out and named in red, since
   running it now would put it after patches numbered above it. Apply
   it by hand if it's needed.
2. **Your confirmation, then the patches.** Each runs in its own
   transaction with `lock_timeout = 5s`, so a busy table fails fast
   instead of queueing behind live traffic. Before each one the version
   table is read again; a patch someone else applied since the plan
   stops the run. The first failure rolls that patch back and stops.
   What runs is the text you were shown, not a fresh read of the file.
   Afterwards the version table is read once more, and the result says
   which version the database records now. **Esc twice** while it runs
   cancels the running statement on the server: a patch in its
   transaction rolls back; outside one, what ran stays.

**Tab** in the plan picks how it runs, and the plan says which in red
before you confirm:

- **for real** (the default), as above;
- **dry run**: every patch in *one* transaction, rolled back at the end,
  so nothing is kept — not the changes, not the version rows. A later
  patch sees what an earlier one did, as in the real run. It takes the
  same locks and does the same work as the real thing, on the same
  server, and holds every lock until the last patch is done, so on a
  busy database it costs what the real run costs. It's confirmed like
  the real run (a protected environment by its name). A patch that
  can't run in a transaction ends it, since it can't be rolled back;
- **on a copy**: `CREATE DATABASE <db>_pgman_copy_<time> TEMPLATE <db>`
  on the same server, the patches applied to the copy for real, then
  the copy dropped. The database itself is untouched. A template copy
  needs nobody else connected to the database, and copies all of it to
  disk, so it suits an idle local or dev database, not a live one;
  pgman refuses when anyone else is connected rather than make the
  copy. If the copy can't be dropped afterwards, the result names it
  for you to drop. That `DROP DATABASE` is the only one pgman runs, and
  it takes only a name pgman made.

Patches are all or nothing *each*, not as a batch: when the third of
three fails, the first two stay applied and recorded, and the database
reads as two patches further on. Each patch records its own version, so
that's a true state to resume from, and no patch's locks are held while
the next one runs.

### Patches that can't run in a transaction

Postgres refuses `VACUUM`, `CREATE`/`DROP INDEX CONCURRENTLY`,
`REINDEX … CONCURRENTLY`, `CREATE`/`DROP DATABASE` or `TABLESPACE`,
`ALTER SYSTEM` and `REINDEX SYSTEM`/`DATABASE` inside a transaction, so
such a patch can't be all or nothing. When the plan stops at one, `o`
(`ctrl-o` while you're typing a name) plans again letting such patches
run **outside a transaction**:

- each one is marked in red in the plan, with what needs it;
- confirming takes typing the environment's name, protected or not;
- its statements run one at a time, each committing as it goes, with
  the 5 s lock timeout set before each and **no statement timeout** (a
  concurrent build on a big table can take hours, and one cut short is
  left `INVALID`); Esc twice cancels one instead, and the session's own
  timeout is back for the patches after it;
- a failure part-way says how many statements stayed applied, whether
  the version row is recorded (the row usually comes first, so the
  patch can read as applied when it isn't), and, for a failed
  `CREATE INDEX CONCURRENTLY`, that it left an `INVALID` index to drop
  before trying again.

Ordinary patches in the same plan still run in their own transactions,
and every other refusal stands. MCP's `apply_patches` never plans this
way: run those patches from the TUI. Its `mode` argument picks a dry
run (`dry_run`) or a copy (`copy`), as Tab does.

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
| `P` | Apply the database's pending patches: a plan to confirm (`y`, or the environment's name when it's protected), then each patch in its own transaction. In the plan, `o` plans again letting patches that can't run in a transaction run outside one. See [applying patches](#applying-patches). |
| `l` | Recent errors from the focused server's RDS log (ERROR, FATAL, PANIC from the newest files, grouped with a count). `STATEMENT` lines are left out. |
| `v` | Schema drift: each database's version by environment, behind or pending in yellow. Versions that are names (Liquibase, Django, Alembic) can't be ordered: fewer migrations recorded is behind, and two different names at the same count are both yellow. An environment with several rows of one database (two primaries, a lagging replica) shows each version that differs. `v` again for the list. |
| `R` | Probe again every 30 s while the overview is open (environments with `probe = true`; off until pressed). |
| `d` | Discover environments from AWS (`space` choose, `s` save, `p` next profile/scope, `q` back). |
| `r` | List again. |
| `q` / `Esc` | Close, cancelling a connect that is still reading its secret. |

`status` is RDS's view of the instance (`available`, `stopped`,
`modifying`, …), not proof that Postgres answers. Stopped instances are
grey; any state other than `available` or stopped is yellow.
