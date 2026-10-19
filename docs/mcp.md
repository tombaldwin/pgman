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

## This build is read-only

There are no write tools. Every `query` session opens with
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
| `query` | One statement against a named connection, returned as typed JSON rows (the same typing as `--batch --format json`). `max_rows` defaults to 200 and is capped at 5000. `truncated` says whether rows were cut. The statement timeout is your profile's, capped at 30 s. |
| `sessions_locks` | A pre-flight before a schema change. It reports sessions that have held a transaction open for at least `min_xact_age_secs` (default 60), are blocked, are blocking someone, or hold or wait for a lock on one of `tables`, and lists each one's locks on those tables. `server_is_replica` comes from `pg_is_in_recovery()`: on a replica you see the replica's sessions, not the primary's. |
| `pending_patches` | Patch files whose id the database hasn't recorded. Each id comes from the file's own version-row `INSERT`. It also flags recent files with no version row, recorded ids whose file has no live version row (commented out, say), filename/id mismatches and duplicate ids. Configured by `[patches]` in `.pgman/pgman.toml`, or by passing `dir`, `table` and `column`. A file counts as applied when its id is recorded, not when its statements ran. |

Each tool's description ends in a CAVEATS paragraph that says what the
tool cannot see.

## Where connections come from

| Source | Usable |
| --- | --- |
| `~/.config/pgman/connections.toml` (yours) | Yes, including ssh tunnels. This is where anything that tunnels or carries a secret belongs. |
| `.pgman/pgman.toml`, Spring `application*`, IntelliJ data sources (the checkout's) | Only when the server runs with `--project-connections`, the same deliberate act as batch's `--discovered`. A discovered connection that tunnels is refused even then. |

`connections.toml` uses the same `[[connections]]` shape as the project
file:

```toml
[[connections]]
name = "ledger-local"
url = "postgres://ledger@localhost:5432/ledger"
password_env = "LEDGER_LOCAL_PASSWORD"   # read from the server's environment

[[connections]]
name = "demo-replica"
url = "postgres://readonly@demo-read-only-replica.example:5432/ledger?sslmode=require"
ssh_tunnel = "ec2-user@jumphost.example"
password_env = "LEDGER_DEMO_PASSWORD"
```

`${…}` placeholders are not resolved in `connections.toml`. A connection
that has one is refused; name the password's variable with
`password_env` instead.

A tunnel runs the system `ssh` with `BatchMode=yes`, so the key must
work without a prompt: an `IdentityFile` in `~/.ssh/config` or a key
loaded with `ssh-add`.

**Passwords.** `password_env` is read from the MCP server's environment.
`claude mcp add -e VAR=value` stores that value in plain text in the
client's config, so don't put a production password there. A
Secrets Manager resolver is planned, so a connection can name a secret
instead.

## Limits of this version

- Each `query` call opens and closes its own connection, and its own
  tunnel if it has one. `SET`, temp tables and other session state do
  not carry over between calls.
