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

## Database / connection

- `\l` — list databases (name + on-disk size) as a result grid. Reads
  from data already fetched at connect time; sends no new query.
- `\c` — open the connection picker.
- `\c <name>` — connect to the picker entry named `<name>` (case-
  insensitive). If no picker entry matches, swaps `dbname` on the
  *current* DSN to `<name>` and reconnects. Errors if there's no active
  connection to swap and no matching entry.
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
  result (the source table becomes the XML element name). No path picks
  a timestamped default under the cache directory.

## Meta

- `\?` / `\h` — open the help overlay.
- `\q` / `\quit` — quit pgman.

Anything else starting with `\` is reported as an unknown command rather
than sent to the server.

## Command line

```
pgman [OPTIONS] [DSN]
```

pgman is a TUI: every path except `--batch`, `--upgrade`, `--version`, and
`--help` needs a real terminal on both stdin and stdout. A launch from a
pipe or script without one is refused with `pgman needs a terminal. For
pipes and scripts use --batch (see pgman --help).` (exit code `2`) rather
than the raw crossterm error a bare terminal probe used to surface.

| Flag | Behaviour |
|---|---|
| `[DSN]` (positional) | Connect using a `postgres://` DSN — same as `--dsn`. `pgman postgres://app@localhost/appdb` works without the flag name. Passing both `--dsn` and the positional form is fine only when they're identical; disagreeing values are a hard error. |
| `--dsn <DSN>` | Connect using a `postgres://` DSN. |
| `--theme <THEME>` | Colour theme: `dark` \| `light` \| `high-contrast` (default `dark`). |
| `--demo` | Run against a synthetic, self-contained dataset — no database, no network, no disk writes; identical frame every launch. For screenshots / demo GIFs / talks. |
| `--init-config` | Write a commented default `safety.toml` under the config dir (honouring `XDG_CONFIG_HOME`), `0600`, then exit. Refuses to overwrite an existing file (exit `1`, says so on stderr) rather than clobbering your edits. |
| `--log <PATH>` | Preload the editor with a Hibernate or Postgres server log from `PATH` (`-` for stdin) and open straight into the reconstructed-query picker — same as pasting the log and pressing `Ctrl-L` / `F8`. Refused past 64 MB (`-` / stdin is exempt — its size isn't known ahead of reading it) — trim the file first (`grep` for `org.hibernate.SQL` or `LOG:`). |
| `--batch` | Run one SQL statement, write the result to stdout, then exit — no TUI. For scripts/CI. |
| `--sql <SQL>` | The statement to run in `--batch` mode; omit to read stdin until EOF. |
| `--format <FORMAT>` | `--batch` output format: `csv` (default) \| `tsv` \| `json` \| `expanded`. `json` is typed: SQL `NULL` → JSON `null` (never confused with an empty string), `int2`/`int4`/`int8`/`float4`/`float8`/`numeric` → a JSON number (numeric falls back to a quoted string only for the non-numeric spellings `NaN`/`Infinity`/`-Infinity`), `bool` → JSON `true`/`false`, everything else → a string. `csv`/`tsv`/`expanded` render every value as text, unchanged. |
| `--yes` | In `--batch` mode, proceed past statements the safety guard would otherwise only *confirm* (e.g. `INSERT`/`UPDATE`/`DELETE`-with-`WHERE`). Statements configured to *block* (`DROP`, unqualified `DELETE`/`UPDATE`, …) stay blocked regardless, and it does not lift `read_only` — a write on a read-only connection is still refused by Postgres itself. Without this flag, a non-interactive batch refuses anything that would have prompted interactively. |
| `--tap-listen <ADDR>` | Bind a TCP listener for the pgman-tap JAR (length-prefixed JSON events); `:PORT` or bare `PORT` binds `127.0.0.1`. Events stream into the JDBC tap monitor (`F4`). Auto-enabled on `127.0.0.1:7432` when a Java project is detected in the cwd and this flag isn't passed. |
| `--tap-otlp <ADDR>` | Bind an OTLP/HTTP listener (`POST /v1/traces`, JSON) so any OpenTelemetry-equipped JVM can stream Postgres spans without the pgman-tap JAR. Opt-in only — never auto-enabled, since its usual port (4318) collides with a standard OTel collector. |
| `--tap-udp <ADDR>` | Bind a UDP listener for fire-and-forget tap events (one tap event as JSON per datagram, no framing). Lossy — dropped events are silently gone. |
| `--tap-replay <PATH>` | Replay a captured JSONL tap event stream through the same pipeline the live listeners use. |
| `--tap-record <PATH>` | Append every incoming tap event (from any active transport) to `PATH` as JSONL, for later `--tap-replay`. Opened append-only; warns at startup if set with no transport active. |
| `--upgrade` | Upgrade this install in place, then exit: `git pull` + `cargo install --path .` for a git checkout, `cargo install` for a crates.io install, or the Homebrew formula; prints the GitHub releases page for anything else. |
| `--no-update-check` | Skip the startup check for a newer release on crates.io (same effect as the `PGMAN_NO_UPDATE_CHECK` env var). Without either, pgman makes at most one crates.io request every six hours, sending only the running version and a user-agent string. |

A `--batch --dsn` connect failure prints the driver/server message, then a
`hint: …` line when `conn::connect_hint` recognises it (wrong password,
nothing listening, unknown host, …) — the same hint the TUI shows on a
failed connection. A write refused because the session is read-only
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
live tail instead of a file with `-`:

```
tail -c 2M myapp-hibernate.log | pgman --log -
```

### `--init-config` example

```
pgman --init-config
```

Writes a fully-commented `safety.toml` (every field at its built-in
default) to the config dir and exits — see
[Configuration](configuration.md#configpgmansafetytoml) for the file it
writes and what each field does.
