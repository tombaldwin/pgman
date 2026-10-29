# Dump and restore

Two commands copy a remote environment into a local database for testing,
with no stored passwords and no hand-rolled tunnel script:

```sh
pgman dump staging/ledger                      # from staging's replica
pgman restore staging-ledger-latest.dump local --as ledger_copy --clean
```

Both run your own `pg_dump` / `pg_restore`. Installed with Homebrew,
pgman brings them: the formula depends on `libpq` and tells pgman where
it is. Installed with cargo, install them yourself (macOS: `brew install
libpq`; Debian/Ubuntu: `apt install postgresql-client`). pgman looks, in
order, in `--pg-bin DIR`, then `$PGMAN_PG_BIN`, then Homebrew's `libpq`
(`$HOMEBREW_PREFIX`, `/opt/homebrew`, `/usr/local`; it is keg-only, so
nothing needs linking), then `PATH`, and says which one it ran. Whichever
it finds must be at least the server's major version. pgman does the parts it
already does for the TUI: finding the database, reading the password from
AWS Secrets Manager (or `password_env`) at that moment, and opening the
ssh tunnel through the bastion.

## Naming a database

Both commands take the same `<target>`:

| Target | What it names |
|---|---|
| `staging` | the one database of environment `staging` in `environments.toml` |
| `staging/ledger` | database `ledger` of `staging` |
| `staging/ledger-db-replica/ledger` | `ledger` on one server of `staging` |
| `local` | a connection in `connections.toml` |
| `postgres://app@127.0.0.1:5432/ledger` | a URL |

An environment name wins over a connection of the same name.

## `pgman dump`

```
pgman dump <target> [FILE] [--schema-only] [--reason TEXT] [--ticket KEY]
                    [--primary] [--format custom|plain|directory|tar]
                    [--skip-unreadable] [--pg-bin DIR] [--verbose] [-- PG_DUMP_OPTIONS...]
```

- **From the replica.** When the environment lists a replica of the
  database (and it isn't stopped), the dump is taken there, so a long read
  never weighs on the primary. `--primary` takes the primary instead.
  pgman says which server it used.
- **What the user can't read, checked first.** An application role
  often can't read everything another role owns: old partitions, a
  schema it has no `USAGE` on, a table under row-level security, large
  objects. pg_dump would stop at the first, however far into the dump.
  pgman asks the server first and refuses, naming them, before pg_dump
  starts. `--skip-unreadable` leaves exactly those out instead (each
  table named, so nothing else is; large objects all together, as
  `pg_dump -B` does), and the result says what it left out. The copy is
  then incomplete; restoring it is your call. Views it can't select from
  are no obstacle: pg_dump reads their definitions, not their rows.
- **A sequence its table needs.** pg_dump reads a table's own sequences
  (`serial` and identity columns) along with it, so a role that can read
  a table but not its sequence can't be dumped around — not even with
  `--skip-unreadable`. pgman says which, and that `GRANT SELECT ON
  SEQUENCE` (`USAGE` isn't enough) or `pg_read_all_data` fixes it.
- **Version check.** Your `pg_dump` must be at least the server's major
  version; an older one is refused before anything runs, with how to get
  a newer one.
- **No password in sight.** The password reaches `pg_dump` only as
  `PGPASSWORD` in that process's environment: never on a command line
  (which `ps` shows other users) and never in a file. Every `PG*` variable
  of your own shell is removed first, so a `PGHOST` or `PGPASSWORD` meant
  for something else can't redirect the dump or follow it there. (Your
  own user and root can read a process's environment while it runs.)
- **Through the tunnel, still verified.** pg_dump connects to the
  tunnel's local end with `host` set to the server's real name, so
  `sslmode=verify-full` checks the right certificate.
- **Never a half-written file.** The dump is written to a hidden name
  beside FILE and renamed into place only when `pg_dump` succeeds. A
  failure, or Ctrl-C, leaves an existing file as it was.
- **Private.** `pg_dump` runs with a umask of 077: the dump is readable
  by you alone from the moment it exists.
- **Names.** With no FILE, or a directory for FILE, the dump is named
  after the target — `<environment>-<database>`, or the connection's
  name — and the UTC time (`staging-ledger-20261028T140322Z.dump`), and
  `staging-ledger-latest.dump` is pointed at the newest one. A FILE you
  name is replaced when the dump succeeds, and the result says so.
- **A protected environment asks.** A dump of a protected target
  (`protected = true`, or "prod" in the name, as for restore below) asks
  why it is taken, and a raw dump of one — every row — also takes the
  environment's name typed, after a warning that it copies personal data
  to this machine. See [the dump policy](#the-dump-policy). Other targets
  are dumped without a question, as before.
- **The structure alone.** `--schema-only` dumps no rows; it never needs
  the typed name.
- **In the journal.** Every dump is recorded, with its reason: `pgman
  copies` lists them, and deletes one when you're done. See [copies](#pgman-copies).
- **Options.** Anything after `--` goes to pg_dump:
  `-- --exclude-table-data=audit_log`, `--format directory -- -j 4`. The connection and output options
  (`--dbname`, `--host`, `--file`… and their abbreviations) are pgman's
  and are refused.

When a dump fails, pgman reads pg_dump's output for the two causes worth
naming:

- **A recovery conflict.** A hot-standby replica cancels a read that holds
  back changes it is replaying from the primary ("canceling statement due
  to conflict with recovery"). Try again, at a quieter time if you can;
  if it keeps happening, take it from the primary with `--primary`, or
  raise `max_standby_streaming_delay` on the replica.
- **Permission denied.** The dump's user can't read every object it
  covers. Dump as a user that can (the owner, or one granted
  `pg_read_all_data`), or leave those objects out with
  `-- --exclude-table=…` / `--exclude-schema=…`.

## `pgman restore`

```
pgman restore <FILE> <target> [--as NAME] [--create] [--clean] [--terminate]
                              [--keep-owners] [--jobs N] [--allow-remote] [--yes]
                              [--pg-bin DIR] [--verbose] [-- PG_RESTORE_OPTIONS...]
```

- **Never a protected database.** An environment is protected with
  `protected = true`, or by default when its name contains "prod"; a
  connection is protected when an environment that lists it is, or when
  its name or its host's contains "prod"; a URL when its database or host
  does; and any target on a server a protected environment reaches (one
  of its connections' host and port, or an RDS endpoint of one of its
  instances), however it is named; and `--as NAME` with "prod" in the name is refused too. There is
  no override. `--clean` never drops `postgres`, `template0` or
  `template1`.
- **Only this machine,** unless `--allow-remote`: the target must be
  `localhost`, a loopback address or a Unix socket, and not reached
  through an ssh tunnel.
- **Into another name.** `--as NAME` restores into database `NAME` on the
  target's server, so several local copies can sit side by side.
- **Create, replace, or neither.** `--create` makes the database when it
  doesn't exist; `--clean` drops it and creates it again; with neither,
  the dump is restored into the existing database as it is. A database
  pgman makes is created from `template0` with the server's defaults,
  as `pg_restore --create` does, so extensions come from the dump: a
  user who isn't a superuser can't restore one an admin had put in
  `template1` for them.
- **Nobody else connected.** Other sessions on the database refuse the
  restore, listed by pid; `--terminate` ends them first.
- **Confirmed.** pgman shows what it will do and asks; replacing a
  database asks you to type its name. `--yes` skips the question, for
  scripts; without a terminal and without `--yes`, it refuses.
- **Owners and grants left out** (`--no-owner --no-privileges`): a dump
  of a remote environment names roles a local server rarely has.
  `--keep-owners` keeps them.
- **Errors.** pg_restore goes past errors and counts them; pgman lists
  them. A dump from a newer pg_dump sets settings an older server doesn't
  have (`SET transaction_timeout = 0` from pg_dump 17 into a 16 server);
  those are counted as harmless. Any other error makes `pgman restore`
  exit with status 1, so a script can tell the copy is incomplete.

Takes custom, directory and tar dumps. It doesn't take a plain SQL dump
(`--format plain`): run that with `psql` yourself, `psql -d DATABASE -f
FILE`, and none of the checks above apply.

## The dump policy

An environment in `environments.toml` can say what a dump of it may be,
and the defaults follow `protected`:

```toml
[[environments]]
name = "production"
dump = "unmasked"        # unmasked | masked | schema | never
dump_ttl = "14d"         # when pgman copies calls a copy expired: 36h, 14d, 2w
dump_reason = "required" # required | optional
unattended = false       # true: raw dumps may run without a terminal
```

| `dump` | A dump may be |
| --- | --- |
| `unmasked` (the default) | raw, or schema only |
| `masked` | schema only, until pgman can mask (then masked, or schema only) |
| `schema` | schema only |
| `never` | nothing |

A protected environment's dumps need a reason (`dump_reason`), and are
kept 14 days before `pgman copies` calls them expired; others need none
and are kept 30.

- **The reason.** At a terminal pgman asks, offering the ticket in your
  branch's name (`feature/APP-101-invoice-rounding` offers `APP-101`;
  Enter takes it), else the last reason you gave for that target. It is
  one line of up to 120 characters: a ticket id or a few words, not
  people's names. `--reason "…"` or `--ticket APP-101` gives it instead.
  When no reason is needed, the branch's ticket is recorded anyway.
- **A raw dump of a protected environment needs a terminal.** The
  environment's name is typed, and no flag answers that: run from cron,
  CI, a script or an agent's shell, it is refused. A job that must take
  raw copies of one environment can be allowed to, by `unattended =
  true` on that environment in your own `environments.toml` (never a
  project file); each such dump still needs `--reason` or `--ticket`, and
  is journaled as unattended. A schema dump needs no terminal.
- **A project can lower it.** A repository's `.pgman/pgman.toml` can cap
  an environment, never raise it:

  ```toml
  [dumps.environments.production]
  dump = "schema"

  [dumps]
  ticket_keys = ["APP"]   # only these projects' keys count as tickets
  ticket_url = "https://examplecorp.atlassian.net/browse/{ticket}"
  ```

  A `pgman.toml` that is there and can't be read refuses the dump rather
  than dropping its cap. `ticket_url` makes `pgman copies` and its report
  show each ticket as a link.

Masking, and encrypting dumps, are designed but not built yet
([docs/design/masked-dumps.md](design/masked-dumps.md)): until then a
dump is the data as it is, in a file only you can read.

## `pgman copies`

```
pgman copies [list] [--all]
pgman copies show <copy>
pgman copies rm <copy> [--yes]
pgman copies report [--since YYYY-MM-DD] [--format text|markdown|json]
```

Every dump pgman takes goes in a journal on this machine
(`~/.local/share/pgman/journal.jsonl`, readable by you alone): when, of
what, raw or schema, from a protected target or not, why, the file and
its size, when it expires, from the command line, unattended or MCP; and
each restore of it, and its deletion. No data, no connection details, no
secrets.

- `pgman copies` lists the copies still on disk — their id, age, size,
  expiry (`EXPIRED` once past it) and ticket — and the dump files in the
  current directory the journal doesn't know (an older pgman's, or
  pg_dump's by hand).
- `pgman copies show <copy>` gives one in full, with where it was
  restored. A copy is named by the start of its id, or by its file (its
  `-latest` link too).
- `pgman copies rm <copy>` deletes the file (asking first, unless
  `--yes`), records the deletion, and points the `-latest` link at the
  newest copy left, or removes it. A link where the dump was is never
  followed. A copy already deleted by hand is recorded as gone.
- `pgman copies report` is the whole account, oldest first: what was
  taken, why, where it was restored, and when it was deleted. `--format
  markdown` pastes into a ticket; `--format json` is for a script.

A database restored from a copy isn't dropped by `rm` yet.

### The journal in CloudWatch

The journal on your machine is your own record. For a client's
environment you can also send each event to a CloudWatch Logs group in
its AWS account, where the client keeps it:

```toml
[[environments]]
name = "production"
journal = { group = "/pgman/dumps" }
# region and profile default to an rds source's own; otherwise name them:
# journal = { group = "/pgman/dumps", region = "eu-west-2", profile = "prod" }
```

After each dump, restore and deletion of that environment's copies,
pgman sends what is new with your `aws` CLI: one log stream per machine
(a random id, not its name), each event as JSON with `by` set to the IAM
identity that sent it. What leaves your machine is less than the local
line: the dump file's name without its path (which holds your user
name), no server address, no data and no secrets. When the source names
an `account`, the credentials must be for it, or nothing is sent.

A send that fails (expired SSO, no permission) never fails the dump:
the result says how many events are waiting and why, and the next `pgman
dump` or `pgman copies` sends them. Each event is sent once
(`journal.sent`, beside the journal, records which). Events more than
13 days old when the group is first set aren't sent: CloudWatch refuses
them.

Developers need only this, on the group:

```json
{
  "Effect": "Allow",
  "Action": ["logs:CreateLogStream", "logs:DescribeLogStreams", "logs:PutLogEvents"],
  "Resource": "arn:aws:logs:*:123456789012:log-group:/pgman/dumps:*"
}
```

No `logs:Delete*` and no `logs:PutRetentionPolicy`: what reaches the
group can't be removed by whoever sent it. The group's retention (a year
or two is usual) and who may read it are the client's settings.

## A local copy, end to end

```sh
pgman dump staging/ledger ~/dumps/
pgman restore ~/dumps/staging-ledger-latest.dump local --as ledger_staging --clean --terminate
pgman copies rm staging-ledger-latest.dump   # when you're done with it
```

The second command, run again tomorrow, replaces yesterday's copy.

## From a coding agent

`pgman mcp serve` has `dump` and `restore` tools that do the same, each
only after you confirm the call in your MCP client, giving the reason
when the environment asks for one (the agent may suggest it; the one
recorded is yours). A protected environment is dumped schema only over
MCP: its raw copy is yours to take, at a terminal. An agent can refresh
its own local copy (`restore` with `as`) without ever holding the
environment's credentials. Its files stay in the server's working
directory, and `restore` has no `--allow-remote`. See [mcp.md](mcp.md).
