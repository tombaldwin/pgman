# Running a script: `pgman run`

```
pgman run <target> <FILE> [--yes]
```

Runs a SQL script that manages its own transactions, the way `psql -f`
does, through pgman's connection (the secret read at connect, the ssh
tunnel) and its guard rails. `<target>` names a database as for
[`pgman dump`](dump-restore.md#naming-a-database); a script always runs
on the primary, never a replica.

```sql
-- fix-balances.sql
CREATE INDEX CONCURRENTLY accounts_owner_idx ON accounts (owner_id);
BEGIN;
UPDATE accounts SET balance = 0 WHERE closed;
INSERT INTO audit_log (what) VALUES ('zeroed closed accounts');
COMMIT;
\c ledgerlog
INSERT INTO events (msg) VALUES ('balances fixed');
```

```sh
pgman run staging/ledger fix-balances.sql
```

## What it checks first

Before anything connects for writing:

- Every database the script touches must be writable in `safety.toml`
  (`read_only = false`).
- A statement that a guard set to `block` stops refuses the whole
  script.
- The script must split cleanly into statements (every quote and comment
  closed). Unicode-escaped names (`U&"…"`) and two-phase commit are
  refused.
- No transaction may be left open at a `\c` (psql would drop it with the
  connection) or at the end of the script.

## Confirming

pgman shows what will run: how many statements, how many transactions,
how many statements run outside one, which database each part runs on,
and anything the `confirm` guards would ask about (`TRUNCATE`, …). Answer
`y`, or pass `--yes` in a script.

A protected target (`protected = true`, or a name containing "prod") is
confirmed by typing its name. `--yes` doesn't answer that, and without a
terminal pgman refuses.

## While it runs

Statements run one at a time. Anything outside `BEGIN … COMMIT` commits
as it runs, so `VACUUM` and `CREATE INDEX CONCURRENTLY` work as they do in
psql. Each statement waits at most 5s for a lock (a script's own `SET
lock_timeout` overrides that), so a busy table fails the script fast
rather than queueing it behind live traffic, and runs for at most the
database's `statement_timeout_ms` from `safety.toml` (30s unless you set
it; `0` is no limit). The confirmation says both. What can't run in a
transaction — `VACUUM`, `CREATE INDEX CONCURRENTLY` — has no time limit:
it can take hours, and a concurrent build cut short is left `INVALID`. If
one fails anyway (a lock it waited for), the report says to drop the
invalid index before trying again.

If a statement fails, the script stops there and pgman says:

- **committed:** statements that ran outside a transaction, and
  transactions that reached their `COMMIT`;
- **rolled back:** the transaction that was open, and how many of its
  statements were undone;
- **not run:** everything after the failure.

Ctrl-C cancels the running statement on the server and reports the same
way.

## Another database: `\c`

`\c DATABASE` (or `\connect`) on a line of its own switches the rest of
the script to another database: a second, logging database on the same
server, say. In an environment, pgman uses that environment's entry for
the database on the same server when there is one (it may have its own
secret). Otherwise it keeps the same server and credentials and swaps the
database, as psql does; a database named like prod ("prod" in it) is
then protected, so the script is confirmed by typing its name. A quoted name works (`\c "Odd name"`); another
user, host or port does not. No other psql meta-command (`\set`, `\i`,
…) is read, and a script containing one is refused.

## Patches outside a transaction

A patch that can't run in a transaction (`VACUUM`, `CREATE INDEX
CONCURRENTLY`) is applied from the environments view: `P` plans it, `o`
plans it outside a transaction, confirmed by typing the environment's
name. `pgman apply` does the same from the command line; see
[environments.md](environments.md).
