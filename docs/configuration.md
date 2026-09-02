# Configuration

pgman reads from a handful of files under `~/.config/pgman/`,
`~/.local/share/pgman/`, `~/.cache/pgman/`, and an optional
`<repo>/.pgman/` project directory. None of them carry resolved
passwords — those come from `PGPASSWORD`, a per-connection
`password_env`, a Spring config file's own `username`/`password`
keys, or (for IntelliJ) `PGPASSWORD` again, since IntelliJ keeps
passwords in the OS keychain rather than `dataSources.xml`.

## File locations

| Path | Purpose |
| --- | --- |
| `~/.config/pgman/safety.toml` | Personal safety guard rails: default profile + per-database overrides (`src/main.rs::load_safety_config`, `src/safety.rs`). |
| `<repo>/.pgman/pgman.toml` | Project-committed connections + safety overrides. Discovered by walking up from cwd (`src/project.rs`). |
| `~/.local/share/pgman/draft.sql` | Auto-saved editor buffer, restored on next launch (`src/app.rs::draft_path`). |
| `~/.local/share/pgman/history.log` | Query history, one entry per line, newest kept up to 50 (`src/app.rs::history_path`, `HISTORY_CAP`). |
| `~/.local/share/pgman/saved.toml` | Named saved queries (`src/saved.rs`). |
| `~/.cache/pgman/pgman.log` | `tracing` output. Level via `RUST_LOG` (default `info`). Single file, not rotated (`src/main.rs::init_logging`). |
| `~/.cache/pgman/update_check.json` | Cached result of the crates.io version check, re-checked at most every 6 hours (`src/update_check.rs`). |
| `~/.cache/pgman/report-<ts>-<pid>.md` (or `.html`) | Default `\report` output path when none is given (`src/app.rs::default_report_path`). |
| `~/.cache/pgman/<table>-fixture-<ts>-<pid>.xml` | Default `\fixture` output path (`src/app.rs::default_fixture_path`). |

None of these files are written with restricted permissions — pgman
never calls `set_permissions`/chmod, so they land with whatever the
process umask gives (typically `0644`). This matters because
`safety.toml` and `pgman.toml` carry no secrets by design, but
`history.log`, `draft.sql`, and `saved.toml` can contain literal
values from queries you've run (including ones you typed into a
`WHERE` clause). Treat `~/.local/share/pgman/` as at least as
sensitive as your shell history.

## `~/.config/pgman/safety.toml`

Optional. Falls back to hard-coded defaults when absent or when it
fails to parse (a parse error is logged and defaults are used
instead of failing startup — `src/main.rs::load_safety_config`).

```toml
# ~/.config/pgman/safety.toml
#
# [default] is the profile for any database with no entry of its own.
# Every field below shows its built-in default (SafetyProfile::default
# in src/safety.rs) — you only need to write the ones you want to change.

[default]
# Open the connection with `default_transaction_read_only = on`. A
# write attempted on a read-only session is rejected by Postgres
# itself, on top of the client-side guards below.
read_only = true

# Session `statement_timeout`, in milliseconds. 0 disables it.
statement_timeout_ms = 30000

# Wrap writes (anything that isn't a plain SELECT) in an explicit
# transaction and leave it open on success — pgman then prompts
# commit (y) / rollback (n / Esc) before you can run anything else.
auto_tx = true

# Row-count threshold above which a SELECT triggers a pre-flight
# EXPLAIN cost preview + confirm prompt. 0 disables the check
# (default) — opt-in per profile.
cost_preview_threshold_rows = 0

# Which strategy `\fixture` apply (Ctrl-D) uses to empty a table
# before inserting: "truncate" (fast, needs TRUNCATE privilege) or
# "delete_from" (slower, works without it, respects triggers).
clean_mode = "truncate"

# Per-statement-category guard: "allow" | "confirm" | "block".
[default.guards]
insert = "confirm"
update = "confirm"
update_without_where = "block"   # UPDATE with no WHERE — touches every row
delete = "confirm"
delete_without_where = "block"   # DELETE with no WHERE — empties the table
truncate = "confirm"
drop = "block"
ddl = "confirm"                  # ALTER / CREATE / GRANT / VACUUM / …
other = "confirm"                # anything unrecognised (e.g. MERGE)

# Per-database override — only list what differs from [default].
# Unlisted fields (including unlisted [databases.NAME.guards] entries)
# fall back to [default].
[databases.production]
read_only = true
statement_timeout_ms = 5000

[databases.production.guards]
truncate = "block"
```

`SELECT` is always `Guard::Allow` and is never routed through the
per-category guard table. Guards key off `classify()`'s heuristic
statement classification — see `docs/safety-and-privacy.md` for how
that works and its known imprecision.

## `<repo>/.pgman/pgman.toml` (project-committed)

Intended to be committed to git so a team shares the same list of
data sources and per-database safety rules. Discovery walks up from
the current directory looking for a `.pgman/` folder, so pgman works
from any subdirectory of the project (`src/project.rs::find_root`). A
malformed file is logged and ignored — pgman falls back to normal
discovery rather than refusing to start.

```toml
# .pgman/pgman.toml — commit this. No passwords here: they come from
# PGPASSWORD or a per-connection password_env.

[[connections]]
name = "local"
url  = "postgres://postgres@localhost:5432/myapp"

[[connections]]
name = "staging"
url  = "postgres://stg-db.internal:5432/myapp"
# Override the user embedded in the URL — useful when the URL is
# shared but each teammate should log in as themselves.
user = "app"
# Env var holding the password. Falls back to PGPASSWORD when unset.
# Precedence (most to least specific): password_env, then PGPASSWORD,
# then any password embedded in the URL itself. An empty env var is
# treated as unset (so `unset FOO` doesn't blank out a URL password).
password_env = "STAGING_DB_PASSWORD"

[[connections]]
name = "via-bastion"
url  = "postgres://db.internal:5432/myapp"
# Optional bastion target: [user@]host[:port]. pgman shells out to
# the system `ssh` binary (BatchMode=yes) and opens a local forward
# before connecting, honouring your ~/.ssh/config (keys, ProxyCommand,
# etc). Wins over a `?ssh_tunnel=...` URL param on the same connection;
# a malformed value here clears any URL-embedded tunnel too, rather
# than silently falling back to it.
ssh_tunnel = "tom@bastion.example.com"

# Per-database safety overrides. Project values win on collision, so
# you can commit just [safety.databases.production] and keep your
# personal ~/.config/pgman/safety.toml defaults for everything else.
# [safety.default] is also accepted, overriding the default profile
# the same way.
[safety.databases.production]
read_only = true
statement_timeout_ms = 5000
```

## Datasource discovery and precedence

On startup (and, more restrictively, in `--batch` mode) pgman
collects candidate connections from every applicable source into one
list, then decides what to do with it (`src/main.rs`):

1. **`--dsn <url>`** — if passed, this is used outright. Discovery
   below still runs (so the picker is available if the connection
   later fails), but it never overrides an explicit `--dsn`.
2. **`.pgman/pgman.toml`** (`[[connections]]`) — loaded first into the
   candidate list, if a `.pgman/` directory is found walking up from
   cwd.
3. **Spring** — if `pom.xml` / `build.gradle` / `build.gradle.kts`
   exists in cwd (`creds::spring::detect_java_project`), pgman scans
   `src/main/resources/application*.{properties,yml,yaml}` and
   `bootstrap*.{yml,yaml}`. Spring's own precedence is followed:
   `.properties` outranks `.yml`/`.yaml` for the same base name, and a
   profile file (`application-prod.yml`) overlays the base
   (`application.yml`) field-by-field — an overlay value only wins
   when it's non-empty, so a profile that sets just a password doesn't
   blank out the base URL/username. Both `spring.datasource.*` and
   non-canonical prefixes (`dataSource.*`, `logDataSource.*`, …) are
   recognised; a prefix is only emitted as a candidate when its `.url`
   starts with `jdbc:`.
4. **IntelliJ** — if `.idea/` exists, pgman parses
   `.idea/dataSources.xml` (committed) merged with
   `.idea/dataSources.local.xml` (gitignored, per-user), joined by
   data-source UUID. Username precedence: URL &gt; committed
   `<user-name>` &gt; local `<user-name>`. When the JDBC URL has no
   path component, one pick is emitted per database found in the
   local file's schema-mapping (`<node kind="database" qname="…">`);
   with no local metadata at all it falls back to Postgres's own
   default database name.

**Passwords**: project connections use `password_env` → `PGPASSWORD` →
URL-embedded password. Spring datasource blocks use the file's own
`username`/`password` keys (profile overlay rules apply) — pgman does
not fall back to `PGPASSWORD` for Spring picks. IntelliJ picks use
the URL's embedded password if any, else `PGPASSWORD` (IntelliJ never
writes passwords to `dataSources.xml`, so this is the practical way
to supply one without retyping the DSN).

**Auto-pick**: if `--dsn` was not passed and exactly one candidate was
discovered across all sources combined, pgman connects to it
automatically and shows where it came from (`dsn_origin`, e.g.
`"auto-picked project data source 'staging'"`). Two or more
candidates leave the interactive picker (`Mode::ConnPick`) open. In
`--batch` mode there's no picker to fall back on: zero or more-than-one
candidates is a hard error asking for `--dsn`
(`src/main.rs::resolve_batch_dsn`).

**Unresolved `${...}` placeholders**: Spring config files commonly use
`${DB_HOST}` / `${db.password}`-style placeholders meant to be
resolved by Spring's own environment/property-source machinery at
JVM boot. pgman's discovery path does **not** resolve these — a value
like `jdbc:postgresql://${DB_HOST}:5432/app` is taken literally, so
`${DB_HOST}` becomes the connection's hostname verbatim and the
connection will fail (typically as a DNS lookup failure) with no
warning that the cause was an unresolved placeholder. If your
`application.yml` relies on placeholders for the datasource block,
pass `--dsn` explicitly instead of relying on discovery.

## Environment variables

| Variable | Effect |
| --- | --- |
| `PGPASSWORD` | Fallback password source for project connections (when `password_env` is unset or empty) and for IntelliJ-discovered connections. Not consulted for Spring picks. |
| `PGMAN_NO_UPDATE_CHECK` | Any value disables the crates.io version check, same as `--no-update-check`. |
| `RUST_LOG` | `tracing` filter for `~/.cache/pgman/pgman.log` (e.g. `RUST_LOG=debug`). Falls back to `info` when unset or invalid. Not read in `--batch` mode (which skips file logging entirely). |
| `EDITOR` / `VISUAL` | External editor for `\e` (suspends the TUI, edits the buffer in a temp file, resumes). Checked in that order — `EDITOR` first, then `VISUAL` — falling back to `vi`. Split on whitespace, no shell involved (so quoting/globs in the value aren't supported). |

## Themes

`--theme dark` (default) | `light` | `high-contrast` (aliases:
`highcontrast`, `hc`) — case-insensitive (`src/theme.rs::Theme::resolve`).
An unrecognised name falls back to `dark` with a logged warning
rather than failing startup.
