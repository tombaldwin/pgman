# MCP server

`pgman mcp serve` is a stdio [MCP](https://modelcontextprotocol.io)
server. Claude Code, or any MCP client, can then query Postgres through
the same safety rails as the TUI and `--batch`.

```
claude mcp add pgman -- pgman mcp serve
```

The server runs in the directory the client starts it in, and that
directory is where it discovers connections. stdout carries the protocol
only. Diagnostics go to the log file (`~/.cache/pgman/pgman.log.*`).

## What writes

`query` never writes. Two tools do, and `--read-only` refuses both:
`environments_save` appends to your `environments.toml`, and
`apply_patches` runs an environment database's pending patches — only
after **you** confirm through the client's elicitation prompt (a
protected environment by typing its name), and only on a database your
`safety.toml` lets write. Every `query` session opens with
`default_transaction_read_only = on`, **whatever your `safety.toml`
says**. Statements go through the same guards as `--batch` with no
`--yes`: a statement a guard would ask a human to confirm is refused,
because nobody is there to confirm it. `--allow-writes` is refused at
startup, not accepted and ignored.

## Tools

| Tool | What it does |
| --- | --- |
| `doctor` | The build, the client capabilities it saw, the write surface, the read-only floor, limits, which config files it read and any errors in them. Call it first when something is refused. |
| `connections` | The names `query` accepts: host, port, database, user, tunnel, where the password comes from (never the password), whether the connection is usable and why not, and which entries reach the same database. |
| `query` | One statement against a named connection, returned as typed JSON rows (the same typing as `--batch --format json`). `max_rows` defaults to 200 and is capped at 5000. A text value over 16 KiB comes back cut (ending in `…`, counted in `cells_cut`), and rows past 1 MiB of JSON are left out. `truncated` says whether rows were cut. The statement timeout is your profile's, capped at 30 s. |
| `sessions_locks` | A pre-flight before a schema change. It reports sessions that have held a transaction open for at least `min_xact_age_secs` (default 60), are blocked, are blocking someone, or hold or wait for a lock on one of `tables`, and lists each one's locks on those tables. `server_is_replica` comes from `pg_is_in_recovery()`: on a replica you see the replica's sessions, not the primary's. |
| `pending_patches` | Patch files whose id the database hasn't recorded. Each id comes from the file's own version-row `INSERT`. It also flags recent files with no version row, recorded ids whose file has no live version row (commented out, say), filename/id mismatches and duplicate ids. Configured by `[patches]` in `.pgman/pgman.toml`, or by passing `dir`, `table` and `column`. A file counts as applied when its id is recorded, not when its statements ran. |
| `environments` | Every database in each environment of `environments.toml`, replicas under their primary, with role, status and engine version, from the sources alone. It connects to no database unless `probe: true` is passed. Then the environments with `probe = true` also report server version, recovery state, lag, patch level and pending patches (see [Probes](environments.md#probes)), and the others say probing isn't enabled. |
| `apply_patches` | An environment database's pending patches: planned read-only and checked against `safety.toml`, shown to you through elicitation, then each run in its own transaction with a 5 s `lock_timeout`, stopping at the first failure. Refused under `--read-only`, by a client without elicitation, on a replica, and on a read-only profile. See [applying patches](environments.md#applying-patches). |
| `environment_logs` | Recent errors from an RDS server's Postgres log, for a server an `rds` environment lists: ERROR, FATAL and PANIC lines grouped by message, with a count, who last hit it and when. `STATEMENT` and `DETAIL` lines are left out. |
| `schema_lint` | The schema wizard's findings for a connection (tables without a primary key, unindexed foreign keys, unused or overlapping indexes, bloat, and more), each with a severity, why it matters, and a suggested fix. On a replica it warns that unused-index and bloat figures are the replica's own. |
| `environments_discover` | Proposals for `environments.toml` from an AWS account (optional `region`, `profile`, `account`): per primary, its replicas, matching secret names, the bastion, and exactly the TOML a save appends, with notes to check. Reads names only, never a secret's value. |
| `environments_save` | Appends the named proposals (`names`) from a fresh discovery to `environments.toml`. Only proposals can be saved, never free-form values. What's already in the file is never rewritten, and a primary already covered is refused. Refused under `--read-only`. It's the only tool that writes a file; `apply_patches` is the only one that writes a database. |

Each tool's description ends in a CAVEATS paragraph that says what the
tool cannot see.

## Where connections come from

| Source | Usable |
| --- | --- |
| `~/.config/pgman/connections.toml` (yours) | Yes, including ssh tunnels. This is where anything that tunnels or carries a secret belongs. |
| `.pgman/pgman.toml`, Spring `application*`, IntelliJ data sources (the checkout's) | Only when the server runs with `--project-connections`, the same deliberate act as batch's `--discovered`. A discovered connection that tunnels is refused even then. |

`connections.toml` uses the project file's `[[connections]]` shape,
plus fields only your own file may carry: `secret`, `aws_region`,
`aws_profile`, `aws_account` and `role`.

```toml
[[connections]]
name = "ledger-local"
url = "postgres://ledger@localhost:5432/ledger"
password_env = "LEDGER_LOCAL_PASSWORD"   # read from the server's environment

# Reads go to the replica: a write there is impossible, not just refused.
[[connections]]
name = "demo"
url = "postgres://demo-read-only-replica.example.rds.amazonaws.com:5432/ledger?sslmode=require"
ssh_tunnel = "ec2-user@jumphost.example"
secret = "demo/database/ledger_replica/credentials"
aws_region = "us-west-1"
aws_account = "210987654321"   # checked before the secret is read
role = "replica"

# The primary, for the pre-flight lock check only.
[[connections]]
name = "demo-primary"
url = "postgres://demo.example.rds.amazonaws.com:5432/ledger?sslmode=require"
ssh_tunnel = "ec2-user@jumphost.example"
secret = "demo/database/ledger/credentials"
aws_region = "us-west-1"
aws_account = "210987654321"
role = "primary"
```

`${…}` placeholders are not resolved in `connections.toml`. A connection
that has one is refused; use `password_env` or `secret` instead.

A tunnel stays open between calls to the same database, so a run of
calls pays the ssh handshake once. It closes after 5 minutes unused, or
when the server exits. Each call still opens its own read-only Postgres
connection through it, so nothing about a session carries over.

A tunnel runs the system `ssh` with `BatchMode=yes`, so the key must
work without a prompt: an `IdentityFile` in `~/.ssh/config` or a key
loaded with `ssh-add`. Use `sslmode=require` through a tunnel:
`verify-full` checks the certificate against the tunnel's local end.

### Secrets

`secret` names an AWS Secrets Manager secret, never its value. At each
connect pgman runs the system `aws` CLI, with the ambient profile
unless `aws_profile` names one. When `aws_account` is set, it first checks
`aws sts get-caller-identity`. A wrong account then fails before the
secret is read, instead of reading a same-named secret elsewhere. The
secret must be a JSON object with `password` and optionally `username`
(or `user`), which is the RDS shape. Its `username` replaces the url's
user. The value goes no further than the connection: it never appears in
a tool result, `doctor`, or the log. A connection with a `secret` and
also another password (`password_env`, or one in the url) is refused.
`doctor` says whether `aws` is on the server's `PATH`.

`password_env` is read from the MCP server's environment.
`claude mcp add -e VAR=value` stores that value in plain text in the
client's config, so use `secret` for anything that matters.

### Roles

| `role` | Tools |
| --- | --- |
| *(none)* | All. For a local or single-server database. |
| `replica` | All. Where reads should go. |
| `primary` | `sessions_locks` and `pending_patches` only: pgman's own fixed reads, never client SQL. A replica's sessions and locks are its own, so a pre-flight before a migration needs the primary's view. After a patch, a lagging replica can still show the old version, so "did the patch land?" is checked on the primary too. `query` is refused; it goes to the replica. The future two-phase patch apply will also use the primary. |

Every result from a `replica` connection carries a `replica` object with
`in_recovery`, `lag_secs` (the age of the replica's last replayed change)
and a `note`. The note appears when the lag is over 5 s, and the advice is
to verify on the primary. An idle primary also grows that number, so a
large value isn't necessarily lag. It also appears when the server is
**not** in recovery at all: then a connection marked `replica` is really a
primary, and only pgman's read-only session stands between a write and
the data.

## Limits of this version

- Each `query` call opens and closes its own connection, and its own
  tunnel if it has one. `SET`, temp tables and other session state do
  not carry over between calls.
