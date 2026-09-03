# Configuration

pgman reads a handful of files under `~/.config/pgman/`,
`~/.local/share/pgman/`, `~/.cache/pgman/`, and an optional
`<repo>/.pgman/` project directory. None carry resolved passwords —
those come from `PGPASSWORD`, a per-connection `password_env`, a
Spring config file's own `username`/`password` keys, or a
URL-embedded password.

## File locations

| Path | Purpose |
| --- | --- |
| `~/.config/pgman/safety.toml` | Personal safety guard rails: default profile + per-database overrides. |
| `<repo>/.pgman/pgman.toml` | Project-committed connections + safety overrides. Discovered by walking up from cwd. |
| `~/.local/share/pgman/draft.sql` | Auto-saved editor buffer, restored on next launch. |
| `~/.local/share/pgman/history.log` | Query history, newest kept up to the last 50 statements run. |
| `~/.local/share/pgman/saved.toml` | Named saved queries. |
| `~/.cache/pgman/pgman.log.YYYY-MM-DD` | `tracing` output, rotated daily at midnight UTC. Level via `RUST_LOG` (default `info`). |
| `~/.cache/pgman/update_check.json` | Cached result of the crates.io version check (re-checked at most every 6 hours). |
| `~/.cache/pgman/report-<ts>-<pid>.md`/`.html` | Default `\report` output path when none is given. |
| `~/.cache/pgman/<table>-fixture-<ts>-<pid>.xml` | Default `\fixture` output path. |

Every path above honours `XDG_CONFIG_HOME` / `XDG_DATA_HOME` /
`XDG_CACHE_HOME` when set to an absolute path. Every file pgman writes
is `0600` and every directory it creates is `0700`, regardless of your
umask. `safety.toml` and `pgman.toml` are yours, not pgman's — it
never writes them, so their permissions are whatever you gave them.

## `~/.config/pgman/safety.toml`

Optional — falls back to hard-coded defaults when absent or unparsable.
`pgman --init-config` writes the file below (every field at its
default) and refuses to overwrite an existing one.

```toml
# [default] is the profile for any database with no entry of its own.
[default]
read_only = true                 # open with default_transaction_read_only = on
statement_timeout_ms = 30000     # session statement_timeout; 0 disables it
auto_tx = true                   # wrap writes in a transaction, prompt commit/rollback
cost_preview_threshold_rows = 0  # EXPLAIN-preview a SELECT above this row estimate; 0 = off
clean_mode = "truncate"          # or "delete_from" — how \fixture apply empties a table

[default.guards]                 # "allow" | "confirm" | "block", per statement kind
insert = "confirm"
update = "confirm"
update_without_where = "block"
delete = "confirm"
delete_without_where = "block"
truncate = "confirm"
drop = "block"
ddl = "confirm"
other = "confirm"

# Per-database override — only list what differs from [default].
[databases.production]
read_only = true
statement_timeout_ms = 5000

[databases.production.guards]
truncate = "block"
```

`SELECT` is always allowed and never routed through the guard table.
Default guards:

| Default | Categories |
| --- | --- |
| `confirm` | `insert`, `update`, `delete`, `truncate`, `ddl`, `other` (e.g. `MERGE`) |
| `block` | `update_without_where`, `delete_without_where`, `drop` |

## `<repo>/.pgman/pgman.toml` (project-committed)

Meant to be committed so a team shares the same data sources and
per-database rules. A malformed file is logged and ignored.

```toml
# .pgman/pgman.toml — commit this. No passwords here: they come from
# the variable a connection's password_env names.

[[connections]]
name = "local"
url  = "postgres://postgres@localhost:5432/myapp"

[[connections]]
name = "staging"
url  = "postgres://stg-db.internal:5432/myapp"
user = "app"                         # override the user embedded in the URL
password_env = "STAGING_DB_PASSWORD" # env var holding the password

[[connections]]
name = "via-bastion"
url  = "postgres://db.internal:5432/myapp"
ssh_tunnel = "tom@bastion.example.com"  # asks for confirmation before use

# Can only TIGHTEN your personal ~/.config/pgman/safety.toml, never relax it.
[safety.databases.production]
read_only = true
statement_timeout_ms = 5000
```

Each field takes the stricter of the personal and project value:
`read_only`/`auto_tx` win if either says on, timeouts take the
smaller non-zero value, guards take the stricter of allow < confirm <
block. A `[safety]` block is a *complete* profile — any field it omits
reverts to pgman's own strict default, not your personal one. `--batch`
applies the same merge.

## Discovery order and precedence

`--dsn` bypasses discovery outright. Otherwise pgman gathers
candidates, in order, from `.pgman/pgman.toml`, a detected Spring
project's `application*`/`bootstrap*` files, and IntelliJ's
`dataSources.xml`, listing them all in the connection picker — nothing
connects without a keypress. `--batch` has no picker: a lone candidate
needs `--discovered`, and `--dsn` is required otherwise.

## Placeholders

Spring/IntelliJ files often use `${VAR}` / `${VAR:default}`
placeholders. pgman resolves these against the shell environment for
**username, password, database name, and query parameters** — never
host or port, since that could turn a secret into a DNS lookup.
An unresolved placeholder stays visible in the picker, marked, and
connecting to it is refused.

## Environment variables

| Variable | Effect |
| --- | --- |
| `PGPASSWORD` | Password for a `--dsn` that doesn't carry one. Not consulted for anything discovered (project, Spring, or IntelliJ). |
| `PGMAN_NO_UPDATE_CHECK` | Any value disables the crates.io version check, same as `--no-update-check`. |
| `RUST_LOG` | `tracing` filter for the daily log file (e.g. `RUST_LOG=debug`). Falls back to `info` when unset or invalid. Not read in `--batch` mode. |
| `EDITOR` / `VISUAL` | External editor for `\e`. Checked in that order, falling back to `vi`. Split on whitespace — no shell involved. |

## Themes

`--theme dark` (default) | `light` | `high-contrast` (aliases:
`highcontrast`, `hc`) — case-insensitive. An unrecognised name falls
back to `dark` with a logged warning rather than failing startup.
