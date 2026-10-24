# Command reference

Type a `\`-prefixed line into the editor and hit run (`F5` / `Ctrl-Enter`) —
pgman routes it as a meta-command instead of sending it to the server, the
same familiarity bridge psql migrants expect. `\?` / `\h` opens the help
overlay from anywhere in the editor. The buffer is cleared after dispatch
(so a second `F5` doesn't re-fire it) except for `\timing` and `\x`, which
operators tend to toggle back in the same buffer.

`\watch` and `\e` are **not** typed commands — they're editor key bindings
(`Ctrl-W` and `Ctrl-X` respectively) that mirror psql's `\watch` / `\e`
behaviour without being parsed as backslash text.

## Schema

- `\d` — open the schema browser (default view, no filter).
- `\d <name>` — open the schema browser filtered to `<name>` (matching
  schema/table/column surfaces with its ancestors visible).
- `\dt` — open the schema browser (same as `\d` with no name; psql's
  "list tables" is folded into the one browser view).
- `\dn` — open the schema browser (same as `\dt`; psql's "list schemas"
  likewise folds into the browser).
- `\df` — list functions as a result grid: schema, name, result type,
  arguments and kind (`func`, `proc`, `agg`, `window`). With no pattern it
  lists everything outside `pg_catalog` and `information_schema`.
- `\df <pattern>` — psql's patterns: `*` and `?` wildcards, an optional
  `schema.` prefix (`\df billing.calc*`), unquoted text folded to lower
  case, `"Quoted"` text kept as written. A pattern searches the system
  schemas too, so `\df now` finds `pg_catalog.now`. The query runs like
  any other statement (it lands in history); `--demo` answers from its own
  three functions.

## Database / connection

- `\l` — list databases (name + on-disk size) as a result grid. Reads
  from data already fetched at connect time; sends no new query.
- `\c` — open the connection picker.
- `\c <name>` — connect to the picker entry `<name>` names. Matching is
  case-insensitive: an exact name wins, otherwise a **unique prefix** does
  (so `\c rep` reaches `reports (application)`). Several matches are
  listed rather than guessed. Discovered names contain spaces, so a name
  can also be given **double-quoted**: `\c "dataSource (application)"`.
  An unquoted name with a space in it is an error rather than a guess —
  `\c my db` says `\c takes one name — quote it if it has spaces` instead
  of connecting to a database called `my`.
  If no entry matches, swaps `dbname` on the *current* DSN to `<name>`
  and reconnects. Errors if there's no active connection to swap and no
  matching entry.
- `\i <path>` — read a SQL file into the editor buffer, replacing it.
  Does **not** run it — review, then press run yourself.

## Display

- `\timing` / `\timing on` / `\timing off` — toggle (no arg) or set
  elapsed-ms display in the status footer. An unrecognised arg falls
  back to toggle.
- `\x` / `\x on` / `\x off` — toggle (no arg) or set expanded (row-detail
  style) result output. Same on/off/toggle shape as `\timing`.

## Export

- `\report` / `\report <path>` — write the advisor + tap insights report
  to `<path>` (Markdown, or HTML if the path ends `.html`/`.htm`). No
  path picks a timestamped default under the cache directory.
- `\fixture` / `\fixture <path>` — capture the current result grid as a
  DBUnit `FlatXmlDataSet` at `<path>`. Requires a non-empty, single-table
  result (the source table becomes the XML element name). A NULL cell is
  left out of its row, flat XML's NULL; an empty string is `col=""`.
  No path picks a timestamped default under the cache directory.

## Meta

- `\?` / `\h` — open the help overlay.
- `\q` / `\quit` — quit pgman.

Anything else starting with `\` is reported as an unknown command rather
than sent to the server.

## The `:` command bar

`:` from any mode that isn't taking literal text (so a colon still types in
the editor and in filters) opens a one-line prompt in the footer. `Enter`
runs, `Esc` cancels back to where you were, `Tab` completes the command name
— a unique name fills in whole, several are listed in the status line. The
editor buffer is never touched: unlike the backslash form, the command isn't
the buffer. Opening the bar over a guarded-run confirmation cancels that run
(the statement was never confirmed) and says `cancelled` once the bar closes.

| Command | `\` equivalent | What it does |
|---|---|---|
| `:about` | — | The About card: version, install channel, server version, licence. |
| `:update` | — | The About card, plus a footer line saying where the release check got to. "Up to date" is only claimed once a check has landed. |
| `:help [topic]` | `\?` / `\h` | The help overlay at section `<topic>`: a section's name or the start of one (`editor`, `sch`, `tap`, `commands`, …; `Tab` completes them); `:help all` is every section on one page. An unknown topic lists them all. The topic picks the section to show, not where you end up: closing help returns to the mode the `:` bar was opened from. |
| `:quit`, `:q` | `\q` | Quit pgman. |
| `:error` | — | The last error in full — what `F2` (or `!` outside the editor) opens. |
| `:notify` | — | The `LISTEN`/`NOTIFY` arrivals panel, as `F3`. |
| `:tap` | — | The JDBC tap monitor, as `F4`. |
| `:readonly on\|off` | — | Set the read-only flag: on the live session at once (`SET default_transaction_read_only`, from its next statement; refused while a transaction is open) and on every later connect. `:readonly on` is sticky for the rest of the session: it survives every later `:connect` / `\c`, and can only ever tighten what the next database's safety profile already asks for. **Refused** when `safety.toml` pins the current database read-only — a session cannot vote itself out of that file. |
| `:redact [on\|off]` | — | Redaction mode for screenshots (= `--redact`): infrastructure names on screen become `▓` blocks; schema and data don't. Bare `:redact` toggles. Display only — what you copy (`y`) is the real text. |
| `:connect [NAME\|URL]` | `\c [NAME\|URL]` | The picker, the named data source, or a `postgres://` URL. Same quoting and unique-prefix matching as `\c`. A URL's password is never shown: the origin line and any error carry it redacted. |
| `:l` | `\l` | List databases (name + size). |
| `:x [on\|off]` | `\x` | Expanded (row-detail) output. |
| `:dt` / `:dn` | `\dt` / `\dn` | Open the schema browser. |
| `:df [PATTERN]` | `\df` | List functions, optionally matching a psql pattern. |
| `:d [NAME]` | `\d` | Schema browser, filtered to `NAME` when given. |
| `:i PATH` | `\i` | Load a SQL file into the editor (doesn't run it). |
| `:timing [on\|off]` | `\timing` | Elapsed-ms in the status footer. |
| `:report [PATH]` | `\report` | Write the advisor + tap report. |
| `:fixture [PATH]` | `\fixture` | Capture the current result as a DBUnit fixture. |

An unrecognised name answers `unknown command :foo · :help lists them`.

## Command line

```
pgman [OPTIONS] [DSN]
```

pgman is a TUI: every path except `--batch` (and `pgman query`), `--init-config`, `--upgrade`,
`--version`, and `--help` needs a real terminal on both stdin and stdout. A
launch from a pipe or script without one is refused with `pgman needs a
terminal. For pipes and scripts use --batch (see pgman --help).` (exit code
`2`) rather than the raw crossterm error a bare terminal probe used to
surface.

| Flag | Behaviour |
|---|---|
| `[DSN]` (positional) | Connect using a `postgres://` DSN — same as `--dsn`. `pgman postgres://app@localhost/appdb` works without the flag name. Passing both `--dsn` and the positional form is fine only when they're identical; disagreeing values are a hard error. |
| `--dsn <DSN>` | Connect using a `postgres://` DSN. |
| `--theme <THEME>` | Colour theme: `dark` \| `light` \| `high-contrast` (default `dark`). |
| `--demo` | Run against a synthetic, self-contained dataset (`ledger`, a made-up billing database, and four environments) — no database, no network, no disk writes; the same frames every launch (the JDBC tap's demo burst arrives two seconds in, lighting `TAP` and `N+1`). For screenshots / demo GIFs / talks. `F6` / `F7` answer with a canned plan, so the EXPLAIN tree opens in the demo the same way it does live. |
| `--redact` | Redaction mode from the first frame, for screenshots of a real database: hosts, IPs, AWS account IDs, ARNs, URL user names, your login name and the server / environment / secret / profile names pgman knows are drawn as `▓` blocks of the same width. Schema and data stay visible. A `REDACT` badge sits first in the footer while it is on. `:redact` toggles it; `redact_default = true` in `config.toml` makes it the default ([configuration](configuration.md#configtoml)). Works with `--demo`, which reads no `config.toml`. The TUI only — `--batch`, `pgman query`, the `env` commands and `pgman mcp serve` print unredacted. See [safety-and-privacy](safety-and-privacy.md#redaction-mode-for-screenshots). |
| `--init-config` | Write a commented default `safety.toml` under the config dir (honouring `XDG_CONFIG_HOME`), `0600`, then exit. Refuses to overwrite an existing file (exit `1`, says so on stderr) rather than clobbering your edits. |
| `--log <PATH>` | Preload the editor with a Hibernate or Postgres server log from `PATH` (`-` for stdin) and open straight into the reconstructed-query picker — same as pasting the log and pressing `Ctrl-L` / `F8`. Refused past 64 MB (`-` / stdin is exempt — its size isn't known ahead of reading it) — trim the file first (`grep` for `org.hibernate.SQL` or `LOG:`). |
| `--batch` | Run one SQL statement, write the result to stdout, then exit — no TUI. For scripts/CI. `csv`, `tsv` and `expanded` stop at 1000 rows: past that, stderr says so and the exit status is `3`, so a partial export can't pass for a whole one. `--format json` (one statement) returns every row. A script of several statements runs in one transaction and prints only its status, not their rows: run one statement per call to get rows. |
| `--sql <SQL>` | The statement to run in `--batch` mode; omit to read stdin until EOF. |
| `--format <FORMAT>` | `--batch` output format: `csv` (default) \| `tsv` \| `json` \| `expanded`. `json` is typed: SQL `NULL` → JSON `null` (never confused with an empty string), `int2`/`int4`/`int8`/`float4`/`float8`/`numeric` → a JSON number (numeric falls back to a quoted string only for the non-numeric spellings `NaN`/`Infinity`/`-Infinity`), `bool` → JSON `true`/`false`, everything else → a string. `csv`/`tsv`/`expanded` render every value as text, unchanged. `csv` and `tsv` keep NULL apart from the empty string the way `COPY` does: in `csv` a NULL is an empty unquoted field and an empty string is `""`; in `tsv` a NULL is `\N` (a backslash in a value is written `\\`, so no value can look like it); `expanded`, which is for reading, prints a NULL as nothing, as psql does. |
| `--discovered` | In `--batch` mode with no `--dsn`, accept the single data source discovered in this checkout. Without it a lone discovered candidate is refused — nothing found in the working tree connects without a deliberate act, and batch has no keypress to offer. Entries that are the same connection (one database declared twice, such as `dataSource` and `replicaDataSource` with identical URLs and credentials) count once. Zero or several distinct candidates still need `--dsn`. |
| `--yes` | In `--batch` mode, proceed past statements the safety guard would otherwise only *confirm* (e.g. `INSERT`/`UPDATE`/`DELETE`-with-`WHERE`). Statements configured to *block* (`DROP`, unqualified `DELETE`/`UPDATE`, …) stay blocked regardless, and it does not lift `read_only` — a write on a read-only connection is still refused by Postgres itself. Without this flag, a non-interactive batch refuses anything that would have prompted interactively. |
| `--tap-listen <ADDR>` | (All the `--tap-*` flags: [tap.md](tap.md).) Bind a TCP listener for the planned pgman-tap library (length-prefixed JSON events; not released yet — use `--tap-otlp` today); `:PORT` or bare `PORT` binds `127.0.0.1`. Events stream into the JDBC tap monitor (`:tap` / `F4`). Auto-enabled on `127.0.0.1:7432` when a Java project is detected in the cwd and this flag isn't passed. |
| `--tap-otlp <ADDR>` | Bind an OTLP/HTTP listener (`POST /v1/traces`, JSON) so any OpenTelemetry-equipped JVM can stream Postgres spans; this is the route that works today, with no pgman-tap library needed. Opt-in only — never auto-enabled, since its usual port (4318) collides with a standard OTel collector. |
| `--tap-udp <ADDR>` | Bind a UDP listener for fire-and-forget tap events (one tap event as JSON per datagram, no framing). Lossy — dropped events are silently gone. |
| `--tap-replay <PATH>` | Replay a captured JSONL tap event stream through the same pipeline the live listeners use. |
| `--tap-record <PATH>` | Append every incoming tap event (from any active transport) to `PATH` as JSONL, for later `--tap-replay`. Opened append-only; warns at startup if set with no transport active. |
| `--upgrade` | Upgrade this install in place, then exit: `git pull` + `cargo install --path .` for a git checkout, `cargo install` for a crates.io install, or the Homebrew formula; prints the GitHub releases page for anything else. An install made with `cargo install --git …` isn't one of these: re-run that command to upgrade it. |
| `--no-tap` | Don't start the JDBC tap listener, not even the one a Java project in the working directory turns on by default. |
| `--no-update-check` | Skip the startup check for a newer release on crates.io (same effect as the `PGMAN_NO_UPDATE_CHECK` env var). Without either, pgman makes at most one crates.io request every six hours, sending only the running version and a user-agent string. |

`pgman query 'SQL' [--json]` is `--batch` spelled the way ebman's
subcommands are: `pgman --batch --sql 'SQL'`, with `--json` short for
`--format json`. With no SQL it reads stdin, and every `--batch` flag
applies (`--dsn`, `--format`, `--discovered`, `--yes`):

```
pgman query 'SELECT id, email FROM users LIMIT 5' --json --dsn postgres://app@localhost/appdb
```

Global options may come before the sub-command too: `pgman --dsn
postgres://app@localhost/appdb query 'SELECT 1'` is the same command. The
same holds for `env` and `mcp`.

`pgman mcp serve [--read-only] [--project-connections]` runs the stdio
MCP server instead of the TUI — read-only queries, plus `apply_patches`
once you confirm it; see [MCP server](mcp.md).

A `--batch --dsn` connect failure prints the driver/server message, then a
`hint: …` line when pgman recognises the cause (wrong password, nothing
listening, unknown host, …) — the same hint the TUI shows on a failed
connection. A write refused because the session is read-only
(`safety.toml`'s `read_only = true`) carries its own hint pointing at the
file and key — see [Configuration](configuration.md).

### `--batch` example

```
pgman --batch --dsn postgres://app@localhost/appdb \
  --sql "SELECT id, email FROM users LIMIT 5" --format json
```

Runs the statement, prints JSON to stdout, exits — nothing interactive. A
write statement needs `--yes` to get past the safety guard's confirm step:

```
pgman --batch --dsn postgres://app@localhost/appdb \
  --sql "UPDATE users SET active = false WHERE id = 42" --yes
```

### `--log` example

```
pgman --log app.log
```

Opens pgman with the editor preloaded from `app.log` and drops straight into
the reconstructed-query picker (`Enter` loads the selected query). Pipe a
log in instead of a file with `-`:

```
tail -c 2M myapp-hibernate.log | pgman --log -
```

pgman reads the pipe to its end (up to 64 MB) before the TUI starts, so
`tail -f` won't open; keys then come from the terminal (`/dev/tty`).

### `--init-config` example

```
pgman --init-config
```

Writes a fully-commented `safety.toml` (every field at its built-in
default) to the config dir and exits — see
[Configuration](configuration.md#configpgmansafetytoml) for the file it
writes and what each field does.
