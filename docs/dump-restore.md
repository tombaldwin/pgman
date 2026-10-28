# Dump and restore

Two commands copy a remote environment into a local database for testing,
with no stored passwords and no hand-rolled tunnel script:

```sh
pgman dump staging/ledger                      # from staging's replica
pgman restore staging-ledger-latest.dump local --as ledger_copy --clean
```

Both run your own `pg_dump` / `pg_restore` (macOS: `brew install libpq`;
Debian/Ubuntu: `apt install postgresql-client`). pgman does the parts it
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
pgman dump <target> [FILE] [--primary] [--format custom|plain|directory|tar]
                    [--skip-unreadable] [--pg-bin DIR] [--verbose] [-- PG_DUMP_OPTIONS...]
```

- **From the replica.** When the environment lists a replica of the
  database (and it isn't stopped), the dump is taken there, so a long read
  never weighs on the primary. `--primary` takes the primary instead.
  pgman says which server it used.
- **What the user can't read, checked first.** An application role
  often can't read every table: partitions or views another role owns,
  say. pg_dump would stop at the first, however far into the dump. pgman
  asks the server first and refuses, naming them, before pg_dump starts.
  `--skip-unreadable` leaves exactly those out instead (each named, so
  nothing else is), and the result says how many and which. The copy is
  then incomplete; restoring it is your call.
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
  `<environment>-<database>-<UTC time>.dump` and
  `<environment>-<database>-latest.dump` is pointed at the newest one.
- **Options.** Anything after `--` goes to pg_dump:
  `-- --exclude-table-data=audit_log`, `-- --schema-only`,
  `--format directory -- -j 4`. The connection and output options
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
  the dump is restored into the existing database as it is.
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

Takes custom, directory and tar dumps. A plain SQL dump (`--format
plain`) is run with `psql`.

## A local copy, end to end

```sh
pgman dump staging/ledger ~/dumps/
pgman restore ~/dumps/staging-ledger-latest.dump local --as ledger_staging --clean --terminate
```

The second command, run again tomorrow, replaces yesterday's copy.

## From a coding agent

`pgman mcp serve` has `dump` and `restore` tools that do the same, each
only after you confirm the call in your MCP client. An agent can refresh
its own local copy (`restore` with `as`) without ever holding the
environment's credentials. Its files stay in the server's working
directory, and `restore` has no `--allow-remote`. See [mcp.md](mcp.md).
