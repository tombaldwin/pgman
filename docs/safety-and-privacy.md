# Safety, privacy, and what's stored locally

## Safety model

- **Read-only by default.** Every `SafetyProfile` defaults to
  `read_only = true` (`src/safety.rs`), which opens the connection
  with `SET default_transaction_read_only = on` (`src/conn.rs::connect_inner`).
  A *write* attempted on such a session is rejected by Postgres itself,
  independent of the client-side guards below. The *setting*, however,
  is a plain session GUC that any role may change, so Postgres does not
  protect it: a script could simply turn it off first. pgman blocks that
  — `SET default_transaction_read_only = off`, `RESET` of it, `RESET
  ALL` / `DISCARD ALL`, and the `READ WRITE` transaction modes on
  `SET` / `BEGIN` / `START` are all refused while the profile says
  read-only, `--yes` included
  (`safety::attempts_read_only_escape`). `statement_timeout_ms`
  (default 30000) is applied the same way as the read-only flag.
- **Every statement is classified before it runs.** `safety::classify`
  is pure and heuristic: it strips comments, looks at the leading
  keyword, and checks whether the statement carries a `WHERE`. It is
  deliberately over-cautious on ambiguous input — a CTE fronting a
  `DELETE`, or `EXPLAIN ANALYZE` on DML, is classified as the
  dangerous inner statement, not as a harmless read. One documented
  imprecision: `has_where` is a whole-statement token check, so a
  `DELETE` whose only `WHERE` sits inside a subquery is treated as a
  guarded delete (`Confirm`) rather than an unqualified one
  (`Block`) — a real SQL parser would fix this (see `BACKLOG.md`).
  `MERGE` (PG15+) has no dedicated classification; it maps to `Other`,
  which is always treated as a write and guarded (`Confirm` by
  default) rather than allowed through as if it were a `SELECT`.
  Two more things that look like reads and aren't, and are classified
  `Other` for the same reason: `SELECT … INTO newtable …`, which
  creates and fills a table; and a `SELECT` calling one of a short list
  of functions whose *call* is the destructive act —
  `pg_terminate_backend`, `pg_cancel_backend`, `pg_reload_conf`,
  `lo_import`, `lo_export`, `pg_read_file`, `pg_read_binary_file`,
  `pg_ls_dir`, `dblink`, `dblink_exec`. (`COPY … TO/FROM PROGRAM` was
  already `Other`, being neither a `SELECT` nor recognised DDL.)
  Keywords count only where Postgres would read them as keywords:
  the token scan skips string literals, quoted identifiers,
  dollar-quoted bodies, and comments.
- **Guards.** Each statement category (`insert`, `update`,
  `update_without_where`, `delete`, `delete_without_where`,
  `truncate`, `drop`, `ddl`, `other`) maps to `Allow` / `Confirm` /
  `Block` in the active `SafetyProfile` (`~/.config/pgman/safety.toml`,
  optionally overridden per-database, and **tightened** — never
  relaxed — by a project's `.pgman/pgman.toml`; see below).
  `SELECT` always resolves to `Allow` and never
  consults the guard table. Defaults block `DROP` and
  unqualified `UPDATE`/`DELETE`; everything else that isn't a `SELECT`
  defaults to `Confirm`.
- **Interactive confirm.** A `Confirm`-guarded statement puts pgman
  into the confirm prompt: `y`/`Y` runs it, `n`/`N`/`Esc` cancels.
  A `Block`-guarded statement never reaches this prompt — it's
  refused outright with an error, and the only way past it is to
  change the guard in `~/.config/pgman/safety.toml` (see "Changing a
  guard" below). A multi-statement script takes the *most
  restrictive* guard across every statement in it, and shows a
  per-kind summary in the confirm prompt.
- **Statement splitting, and what happens when it can't be trusted.**
  The script is split by one lexer (`safety::scan`), shared by the
  splitter and the comment stripper so the two cannot disagree. It
  tracks string literals (`''` escapes), `E'…'` escape strings
  (backslash escapes), `"…"` quoted identifiers (`""` escapes),
  dollar-quoted bodies — where a `$` following an identifier character
  is an identifier character, not a quote opener — and `--` / nested
  `/* … */` comments. Identifier characters follow Postgres's own rule,
  so every byte from 0x80 up — and `$`, which continues an identifier
  though it cannot start one — keeps an identifier going: `é$b$c`,
  `中$b$c` and `a$e` are each one identifier rather than a name followed
  by a quote opener. A `--` comment ends at a carriage return as well as
  a newline. Comments stay *in* the statement they sit in, because
  `/*+ IndexScan(t idx) */` is a pg_hint_plan directive the server acts
  on, and what runs is the re-joined statements rather than the buffer.
  `safety::split_verified` then checks the result:
  every construct must close, and re-joining the pieces must reproduce
  the input. **A script the splitter cannot verify is refused outright**
  ("could not split this script safely — run the statements one at a
  time"), because a guard computed from guessed statement boundaries
  approves the wrong statements. What is sent to the server is the
  re-joined verified statements, not the original buffer, so the server
  executes exactly the text the classifier saw.
- **Rollback-able writes.** When `auto_tx` is on (default), any write
  is wrapped in an explicit `BEGIN` and left **open** on success — the
  statement runs, but nothing is durable until you decide. pgman then
  shows a commit/rollback prompt: `y`/`Y` commits, `n`/`N`/`Esc` rolls
  back. On a statement error, the transaction is rolled back
  immediately so the session doesn't sit aborted. `EXPLAIN ANALYZE`
  on DML is a special
  case: the inner statement genuinely executes, so it's always wrapped
  in a transaction that is unconditionally rolled back regardless of
  the `auto_tx` setting — the mutation is guaranteed never to land.
- **`EXPLAIN` (without `ANALYZE`) bypasses guards entirely** — it
  never executes the inner statement, so there's nothing to guard.
- **`--batch --yes` semantics.** Non-interactive batch mode
  (`pgman --batch --sql "…"`) runs the same `classify`/guard pipeline
  as the editor (`batch::check_batch_safety`), evaluated statement-by-
  statement before ever opening the connection. `Guard::Allow` always
  proceeds. `Guard::Confirm` is refused unless `--yes` is passed.
  `Guard::Block` is refused **regardless of `--yes`** — the only way
  to permit a blocked statement in batch mode is to change its guard
  to `confirm` in `~/.config/pgman/safety.toml` first (see "Changing
  a guard" below). A safe leading statement does not excuse a later
  one in the same script; the first refusal wins.
  The same split-verification and run-what-was-checked rules apply here
  as in the editor.
- **Optional pre-flight cost preview.** When
  `cost_preview_threshold_rows` is set above 0 for a database, a plain
  `SELECT` (via the normal Run, not `EXPLAIN`) first runs an
  `EXPLAIN (FORMAT JSON)` and prompts if the estimated row count
  exceeds the threshold. Disabled by default.
- **This is a client-side guard rail, not a substitute for
  least-privilege database roles** — see `SECURITY.md`, which this
  document defers to for the vulnerability-reporting process.

### Changing a guard

Your personal guard rails live in `~/.config/pgman/safety.toml`
(honouring `XDG_CONFIG_HOME` — see
[docs/configuration.md](configuration.md)). It doesn't exist by
default; create it to change any guard from its built-in value.

For example, an unqualified `DELETE` (no `WHERE`) is `block`ed by
default — nothing gets past the block, `--yes` included. To allow it
past a confirm prompt instead, for every database:

```toml
[default.guards]
delete_without_where = "confirm"
```

Or for one database only, leaving your personal default alone:

```toml
[databases.production.guards]
delete_without_where = "confirm"
```

See [docs/configuration.md](configuration.md#configpgmansafetytoml)
for every field and its built-in default.

## Running pgman inside a checkout you did not write

pgman reads connection details out of the working tree — a project's
`.pgman/pgman.toml`, Spring's `application*.{properties,yml,yaml}`, and
IntelliJ's `.idea/dataSources.xml` — walking up from the current
directory, so a parent directory counts too. **Everything found that
way is untrusted**: the repo's author chose those hosts, not you.
Nothing discovered connects without a keypress — a single candidate
lands in the picker exactly like ten, and the row shows the origin,
`user@host:port/db`, the `sslmode` and any `tunnel → <bastion>` before
you press enter. `PGPASSWORD` is only used with `--dsn`, so a
discovered connection can never borrow it, and a `${…}` placeholder is
never resolved into a URL's host, port or query parameters (that is
where `ssh_tunnel=` and `sslmode=` live), so a committed config can't
turn one of your environment variables into a DNS lookup it controls.
The two components a placeholder *is* resolved in — the username /
password and the database name — additionally refuse any value that
introduces a `?`, `&`, `/`, `@` or `=`, so a database name of
`db?ssh_tunnel=x.evil.example` can't reach past its own component. All
three discovered sources go through that same resolution — Spring,
IntelliJ's `.idea/dataSources.xml` and `.pgman/pgman.toml` — and a
`${…}` left in any connection-critical field (host, database, user,
password, tunnel target, URL parameter) is refused rather than sent.
A project's `[safety]` block can only *tighten* your personal
`~/.config/pgman/safety.toml`, never relax it. And a discovered
`ssh_tunnel` asks before pgman runs `ssh` with your keys, because that
happens before any Postgres traffic.

The escape hatch from all of the above is `--dsn`, which is your own
typed choice and behaves as it always has.

## What's stored locally

| File | Contains |
| --- | --- |
| `~/.config/pgman/safety.toml` | Your personal guard-rail configuration. No secrets. |
| `<repo>/.pgman/pgman.toml` | Project-committed connection URLs (host/port/dbname/user) and safety overrides. **Never a resolved password** — only an optional `password_env` variable *name*. |
| `~/.local/share/pgman/draft.sql` | The editor buffer, auto-saved on quit and restored on next launch — whatever SQL you last had open, including any literal values you typed into it. |
| `~/.local/share/pgman/history.log` | Up to the last 50 statements you've run, one per line (multi-line entries escaped onto one line). Same caveat: literal values in your `WHERE`/`INSERT` bodies persist here. |
| `~/.local/share/pgman/saved.toml` | Named queries you explicitly saved. |
| `~/.cache/pgman/pgman.log` | Application log. Connection strings are always logged with the password masked (see "Redaction of connection strings" below). Resolved passwords are never logged. |
| `~/.cache/pgman/update_check.json` | The last crates.io check timestamp and the latest version string it returned. No identifying data. |
| `~/.cache/pgman/report-*.md` / `.html`, `~/.cache/pgman/*-fixture-*.xml` | `\report` and `\fixture` output — advisor/tap findings and DBUnit fixtures respectively. Can contain table/column names and row data from your session. `--batch --format csv` / `tsv` drop control characters other than `\t`, `\n` and `\r` from cell values for the same reason — the output is written to be piped or `cat`ed, and a value carrying an ESC would otherwise be a terminal-escape-sequence injection at that point. Every value a report renders is escaped for its format, headers included: for Markdown that means `\`, `|`, `` ` ``, `[`, `]` and the HTML-significant characters, so text arriving from a JDBC tap cannot become a link, a live `<script>`, or an extra table column in the shared artifact. |

The files pgman itself writes — `draft.sql`, `history.log`,
`saved.toml`, `update_check.json`, `pgman.log`, and `\report`/
`\fixture` output — are `0600` (owner read/write only) regardless of
your umask; `draft.sql`, `history.log`, `saved.toml`,
`update_check.json`, and `\report`/`\fixture` output go through
pgman's own atomic writer (`util::write_private`), which opens the
file `0600` from the moment it's created rather than writing at a
looser default mode and `chmod`ing afterward — there is no window
where a half-written temp file is world-readable. `pgman.log` is
opened by the logging library, not `write_private`, so it's `chmod`ed
`0600` separately right after — and again when the daily rotation
opens a new file at midnight UTC, so a pgman left running past
midnight does not leave the new log at the umask default; the config/data/cache directories
themselves (`~/.config/pgman/`, `~/.local/share/pgman/`,
`~/.cache/pgman/`) are `0700`, and pgman repairs that mode on every
startup even if the directory already existed looser (an old pgman
version, a backup restore, a stale umask) — unless the path is a
**symlink**, in which case pgman leaves the mode alone entirely
rather than following the link and re-permissioning a directory it
does not own; if you point `~/.cache/pgman` at a shared volume, its
mode stays yours to set. `pgman --upgrade` runs its subprocesses from
`$HOME`, or from that cache directory when `$HOME` is unusable, never
from a shared temp directory — `cargo` and `brew` search upward from
the working directory for a `.cargo/config.toml`, and one in a
world-writable `/tmp` would be anyone's to write. That's a floor, not a
substitute for filesystem hygiene: if your `~` itself isn't otherwise
locked down (shared account, backup that preserves world-readable
ACLs, etc.), still treat the files above as no more private than a
shell history file. `safety.toml` and `pgman.toml` are yours, not
pgman's — it never writes them, so their permissions are whatever you
set.

**Passwords are never written to disk by pgman.** They live only in
process memory for the duration of the connection, sourced from
`PGPASSWORD` (for a `--dsn` only), a `password_env`-named variable, a
Spring config file's own plaintext `password` key (if present in the
file — pgman doesn't add a new place for it to live), or a URL's
embedded password. pgman does not read `dataSources.local.xml`'s
`<secret-storage>` / OS-keychain-backed password at all.

## What leaves the machine

- **The database connection itself** — plaintext TCP or TLS to the
  host/port in your DSN (or through an SSH tunnel; see below).
- **The optional update check** — at most once every six hours, one
  HTTPS GET to `https://crates.io/api/v1/crates/pgman` carrying only
  the running version in a `User-Agent` header
  (`pgman/<version> (https://github.com/tombaldwin/pgman)`) and
  nothing else. Turn it off with `--no-update-check` or
  `PGMAN_NO_UPDATE_CHECK` (any value). Every failure mode — no
  network, TLS trust-store issues, a malformed response — degrades
  silently to "no update known"; it's a courtesy notice, never a hard
  dependency.
- **An SSH tunnel**, when `ssh_tunnel` is configured — the system
  `ssh` binary is shelled out to (`BatchMode=yes`), honouring your
  `~/.ssh/config`, agent, and `ProxyCommand`. When the connection came
  from discovery rather than `--dsn`, this is gated behind an explicit
  confirmation naming the bastion (`ssh <user>@<bastion> → <db
  host>:<port>`), because it runs before any Postgres traffic.
- **Nothing else.** No telemetry, no anonymous identifier, no crash
  reporting, no analytics endpoint.

**Inbound surface, not outbound**: the JDBC-tap listeners
(`--tap-listen` / `--tap-udp` / `--tap-otlp`) accept connections
rather than initiate them, but they matter for the same reason — they
are **unauthenticated ingest**. `--tap-listen`/`--tap-udp` bind
`127.0.0.1` by default when given a bare port or `:port` (e.g.
`--tap-listen :7432`); a full `host:port` (e.g. `0.0.0.0:7432`) binds
exactly what you ask for, with no authentication check at any layer.
Auto-enabling only ever picks `127.0.0.1:7432` (triggered by detecting
a Java project in the launch directory) — a non-loopback bind is
always an explicit, deliberate choice. Only bind a non-loopback
address on a trusted/firewalled network. Events ingested this way
(reconstructed SQL, bound-parameter values unless the JAR redacts
them) are held only in memory (a capped ring buffer) unless you pass
`--tap-record PATH`, which appends them to a JSONL file you chose
(created owner-only, `0600`, alongside its parent directory).
Each listener also caps concurrent connections — and closes one
that goes 30 seconds without completing a frame, so a peer that
connects and then says nothing cannot hold a connection slot
indefinitely — caps every event
field — both the length of each field and, for the list-shaped
`params`, `caller` and `error`, the number of entries in it (64,
the last being a `… +N more` marker), since a list of many short
entries is cheap to send and expensive to hold —
and throttles every warning a peer can trigger — malformed
frames, refused connections, accept failures, idle closes, a
connection ending in error, and a bad line in a `--tap-replay`
file — to at most one per second per log site, with a
suppressed-count on the next line so a hostile or broken client can't
blow up memory or flood the app log — which itself rolls daily
(`pgman.log.YYYY-MM-DD` under `~/.cache/pgman/`, see the table
above).

## TLS

`sslmode` follows libpq's semantics (`src/conn.rs::apply_ssl_mode`,
`build_tls_connector`):

| `sslmode` | Encrypted | Certificate verified |
| --- | --- | --- |
| `disable` | no | — |
| `allow` | yes if the server demands it, otherwise no | no. Same wire outcome as `prefer` — pgman makes a single connection attempt, so it can't preserve libpq's "try plaintext, retry with TLS" negotiation order; that order has no observable effect once the server states its own requirement anyway. |
| `prefer` (default when unset) | yes, falls back to plaintext if the server refuses | no |
| `require` | yes, connection fails if the server refuses | no |
| `verify-ca` | yes | yes — currently collapsed onto the same check as `verify-full`, i.e. **including hostname**. This makes pgman's `verify-ca` strictly *stricter* than libpq's (which checks the chain but not the hostname for `verify-ca`) — deliberate, and safe in the sense that it can only reject a connection libpq's `verify-ca` would accept, never the reverse. A `verify-ca`-without-hostname-check custom verifier, to match libpq exactly, is a tracked follow-up (`BACKLOG.md`) |
| `verify-full` | yes | yes, including hostname |

For `prefer`/`require`/`allow`, pgman installs a rustls verifier that
accepts any server certificate — equivalent to libpq's "encrypt
without authenticating the peer." Use `verify-full` on any network
where a MITM is a real concern. When verification is on, trust roots
come from the OS keychain (`rustls-native-certs`) unioned with the
Mozilla bundle (`webpki-roots`), so a fresh container with no
populated system trust store still connects to RDS.

**An unrecognised `sslmode` is a hard `Dsn::parse` error, not a silent
downgrade.** Values are trimmed and ASCII-lowercased before matching
(so `VERIFY-FULL`, and a value with a stray trailing space or `\r`
from a Windows-authored config file, are accepted and normalised) —
but anything outside the six modes above (a typo like `verify_full`,
an empty `sslmode=`) refuses to parse. Before this fix, an unrecognised
value fell through to `prefer` (encrypt without verifying, and fall
back to plaintext if the server declines) with only a `tracing::warn!`
that the alternate screen never surfaces — the weakest mode of the
five, chosen silently.

## Redaction of connection strings

Every place a DSN could be logged or shown routes through one of two
pure redactors (`src/conn.rs`):

- **`Dsn::redacted()`** — used for a successfully-parsed DSN. Renders
  `postgres://user:***@host:port/db`, appending `via ssh://user@host`
  when a tunnel is configured. The password is masked; nothing else
  is.
- **`redact_url()`** — used on the fallback path, for a
  connection-string-shaped value that failed to parse (so
  `Dsn::redacted()` isn't available). Masks inline userinfo
  (`user:pass@` → `***@`) and any `password=`/`pwd=`/`passwd=` query
  parameter, case-insensitively.

Both redactors — and `Dsn::parse` itself — split the authority using
the *last* `@`, not the first. A password may contain `/`, `@`, `?` or
`#` unescaped (common with generated cloud-provider credentials);
using the first `@` as the userinfo boundary used to both mis-parse
the DSN and let the tail of such a password leak past `redact_url`'s
masking. The search for that `@` stops at the first `?`/`#` **after
the path's first `/`**, since before the path a `?` is still password
material — cutting at the first `?` anywhere made
`postgres://u:p?ss@host/db` parse `p` as the port and redact to
itself.

`redact_url` goes further than the parser, because it runs on strings
that *failed* to parse: it cuts at the last `@` that could be a
userinfo boundary (one followed by the host, and so by the path's
`/`), rather than the parser's answer. A password mixing `@`, `/` and
`?` cannot be split back out by any rule, and this is what guarantees
none of it is printed anyway; the cost when the guess goes the other
way is a masked host in one log line.

**Percent-encoding.** `Dsn::parse` percent-decodes `user` and
`password` (leniently — a malformed `%XX` escape is kept literal
rather than erroring), matching libpq's URI-connection-string
behaviour. A raw `?` or `#` in a password is now
parsed and redacted correctly, but percent-encoding remains the only
way to represent one unambiguously — a password holding both a `/` and
a `?` is genuinely un-splittable (it is still fully redacted). `host` and `dbname` are **not**
percent-decoded.

Discovery logging (project connections, Spring picks, IntelliJ picks)
always goes through one of these before hitting `tracing::info!`, so
`~/.cache/pgman/pgman.log` never carries a resolved password — see
CLAUDE.md's "never log credentials" rule, which this codebase treats
as a hard constraint rather than a guideline.

## Reporting a vulnerability

See [`SECURITY.md`](../SECURITY.md) — do not open a public issue for
security problems.
