# Design note: the config advisor

Status: decided and landed 2026-10-23, first round (read-only
server-stats findings) of the "L3 — tuning advisor" backlog block.

## Scope of this round

Findings from server statistics, each with its evidence, a conservative
suggested setting, and the RDS / Aurora parameter-group mapping:

| code | finding | reads |
|---|---|---|
| ADV000 | the advisor itself is limited (stats too young, no `pg_stat_statements`, no `pg_read_all_stats`) | context query |
| ADV001 | buffer cache hit ratio low → `shared_buffers` | `pg_stat_database` |
| ADV002 | temp spills → `work_mem` (with the spilling statements when `pg_stat_statements` is there) | `pg_stat_database`, `pg_stat_statements` |
| ADV003 | dead tuples past a table's autovacuum point → per-table `autovacuum_vacuum_scale_factor` | `pg_stat_user_tables`, `pg_class.reloptions` |
| ADV004 | lock waits piling up → the blocking chain, `idle_in_transaction_session_timeout` / `log_lock_waits`, `lock_timeout` for the migration session only | `pg_stat_activity`, `pg_blocking_pids` |
| ADV005 | checkpoints requested rather than timed → `max_wal_size` only when the WAL written could have forced them | `pg_stat_bgwriter` (≤ PG16) / `pg_stat_checkpointer` (PG17+), `pg_stat_wal` (PG14+) |

Out of this round (stay in BACKLOG): fsync / `synchronous_commit`
(needs `pg_stat_wal` timing, which is off by default), connection thrash
(needs the tap), stored snapshots for deltas, the `C` agent handoff, and
running any `ALTER SYSTEM`. **pgman never writes to the server here**:
every statement is a fixed `SELECT`; suggestions are text to yank.

## Where it shows

In the schema wizard (`W`), the panel that already holds the DBA
findings — LINT104 (dead tuples) and LINT102 (unused indexes) are
already statistics-based, so the wizard is not schema-only today. A
separate `Mode::Advisor` (the backlog's sketch) would need a new key
and split one triage list into two; the brief is to surface it where
the findings already live. Advisor findings merge into the same list
and sort (severity, then code, then object), use the same keys (`j/k`,
`y` yanks the suggestion, `r` re-runs everything), plus one new key:
`e` yanks the evidence query, so the operator can run it themselves in
the editor.

The detail strip grows for an advisor finding (up to all but the
list's five rows) and keeps its 7 rows for a lint finding. What to do
comes first — the setting (current → suggested, restart or not) and
the suggestion — so 80×24 shows it; then the stats window and the
evidence, the reasoning, the RDS / Aurora line, and the prose.

ADV003 and LINT104 can both name a table. They say different things:
LINT104 is "reclaim it now" (`VACUUM`), ADV003 is "this table's
autovacuum settings let it get here" (a storage parameter). Both rows
stay; ADV003's detail points at LINT104.

## Shape

- `query::advisor` — SQL constants, the pure classifiers (one per
  code, input a plain struct of numbers, output `Option<Finding>`), and
  `fetch(client, &Thresholds)` which runs each check independently (one
  failing does not stop the others, as `lint::fetch_live`).
- `query::advisor::rds` — the parameter mapping as pure data: the
  parameter-group key, static (reboot) or dynamic, unit, and how Aurora
  differs (`Same`, `Differs(why)`, `NotApplicable(why)`, or
  `Unverified(why)` for one AWS's Aurora PostgreSQL parameter tables
  don't list — `lock_timeout`: "set it per role or session; check your
  parameter group"). A parameter both Aurora tables (cluster and
  instance) list says an instance's DB parameter group can override the
  cluster's value, and its cluster command carries that as a comment.
- `lint::Finding` gains `advice: Option<Box<Advice>>` (evidence lines,
  the evidence SQL, the stats window, the setting with current and
  suggested value). Lint findings carry `None`.
- `AppMsg::AdvisorLoaded` mirrors `LiveLintLoaded`. `--demo` answers
  from a fixture.
- It runs on the editor's session, after the live lint, so ages use
  `clock_timestamp()`: inside an open transaction `now()` is the
  transaction's start.

The server kind comes from the advisor's own context query: Aurora
when `aurora_version()` exists, RDS when the `rds.extensions` setting
exists, otherwise self-hosted. The suggestion is written for that
kind: `ALTER SYSTEM SET …` self-hosted, the parameter-group line on
RDS / Aurora (where `ALTER SYSTEM` is refused).

Whether a change needs a restart comes from the server
(`pg_settings.context = 'postmaster'`), not only from the table: the
table is what RDS calls it, the server is what it does.

## The stats window

Every cumulative finding says how long its counters have accumulated:
`stats_reset` of the view it read (`pg_stat_database`,
`pg_stat_bgwriter` / `pg_stat_checkpointer`), or, when that is NULL
(never reset), "since at least server start" with the postmaster start
time — a lower bound, since PG15+ keeps statistics over a clean
restart. Below **1 hour** of accumulation the ratio checks (ADV001,
ADV002, ADV005) do not fire; ADV000 says why instead. ADV003 reads
current dead-tuple counts and ADV004 is a point-in-time snapshot, so
neither depends on the window, and they say "now" instead.

## Thresholds (conservative; documented so nobody mistakes them for precision)

- **ADV001** — hit ratio `blks_hit / (blks_hit + blks_read)` over the
  current database. Only with ≥ 100 000 block requests (≈ 800 MB of
  logical I/O — a quiet dev database is noise). < 0.99 → LOW, < 0.95 →
  MED. A "read" may still come from the OS page cache, so a low ratio
  is a hint, not proof. Self-hosted, the suggestion is twice the
  current `shared_buffers`, bounded by "≤ 25 % of RAM" (pgman does not
  know the RAM). On RDS and Aurora there is no command: RDS's default
  is already about 25 % of instance memory and AWS's guidance is to
  keep it there, and Aurora's is larger still, so the text says keep
  it (on RDS, put the default back if the parameter group lowered it)
  — the working set likely exceeds the instance and the fix is a
  larger class or less I/O per query, not the parameter.
- **ADV002** — `temp_bytes` in the current database. Nothing below 64
  MiB in total. ≥ 256 MiB per hour of the window → MED, else LOW.
  Suggested `work_mem`: twice the current; when `pg_stat_statements`
  names a top spiller whose per-call spill is under 4× the current
  value, the next power of two above that spill (clamped to 2×–4×) —
  a first step, not a fit: sorting in memory takes more than the bytes
  spilled, and `temp_blks_written` per call sums every sort / hash node
  and parallel worker. The suggestion is per role (`ALTER ROLE … SET
  work_mem`), not global: `work_mem` is per sort node per connection;
  the detail says it reaches new sessions only (a pool such as
  HikariCP keeps its connections until `maxLifetime`, 30 min by
  default). Up to three spilling statements are listed
  (`temp_blks_written`, per call); one that reads `<insufficient
  privilege>` gets the `pg_read_all_stats` / `GRANT pg_monitor` line.
- **ADV003** — tables with ≥ 10 000 live rows whose dead tuples are ≥
  10 % of live. Cause, in order: autovacuum disabled (table reloption
  or global) → re-enable; dead past the table's own trigger
  (`threshold + scale × live`) → autovacuum is behind (a long
  transaction holding the xmin horizon, or busy workers); otherwise
  the scale factor allows it → lower it. Suggested per-table
  `autovacuum_vacuum_scale_factor`: 0.05 (0.01 at ≥ 10 M rows), only
  when below the effective one.
- **ADV004** — client backends waiting on a lock (`pg_blocking_pids`
  non-empty) for ≥ 5 s → MED; ≥ 5 waiters or a wait ≥ 60 s → HIGH.
  The evidence is the chain: each root blocker (blocks others, is not
  itself blocked) with state, transaction age, its waiters and the
  start of its query. Root idle in transaction → suggest
  `idle_in_transaction_session_timeout = '5min'` if it is off or as
  good as off: zero, over an hour (AWS's default parameter groups set
  it to 86400000 ms, 24 h, so on RDS / Aurora it is never `0`), or
  longer than the head's transaction has run; the reasoning shows the
  value (`1d = 24h`);
  otherwise `log_lock_waits = on` if it is not (it logs each wait past
  `deadlock_timeout` with the holder, and changes no session's
  behaviour), and once it is, `SET lock_timeout = '5s'` **in the
  migration / DDL session only**, with the caveat in the suggestion
  itself: as `ALTER ROLE … SET` for the application, every statement of
  that role waiting over 5 s would fail — its Flyway / Liquibase
  migrations included — and Java applications generally don't retry.
  (`deadlock_timeout` does not help a pile-up: it only decides when a
  deadlock check runs.) Without `pg_read_all_stats` the other sessions'
  queries read `<insufficient privilege>`; the finding says so.
- **ADV005** — requested checkpoints / all checkpoints > 20 % with ≥
  10 checkpoints in the window. A requested checkpoint is not always a
  WAL-volume one: CREATE / DROP DATABASE, a base backup
  (`pg_backup_start`), a manual `CHECKPOINT` and a shutdown all request
  one, and `max_wal_size` changes none of them. So on PG14+ the check
  reads `pg_stat_wal.wal_bytes` and bounds how many the WAL could have
  forced: Postgres starts one each `max_wal_size / (1 +
  checkpoint_completion_target)` of WAL, counted in whole
  `wal_segment_size` segments less one (`CheckPointSegments - 1`; the
  segment size is read, not assumed 16 MB), so at most `wal_bytes` /
  that (an upper bound — timed checkpoints used some of the WAL too). At
  least half the requested → MED, "forced by WAL volume", suggested
  `max_wal_size` twice the current. Fewer → LOW, "not by WAL volume",
  the other causes listed and no setting (on the compose primary: 587
  requested, 2.8 GB of WAL at 1GB — room for about five).
  `pg_stat_wal` has its own `stats_reset`: when it is more than a
  minute from the checkpoint counters', the WAL rate is carried over to
  their window and the evidence says it is an estimate; under an hour
  old, or before PG14, or unreadable, the finding hedges (LOW, the
  causes listed, double `max_wal_size` only if the server log's
  `log_checkpoints` lines say `checkpoint starting: wal`). Not on
  Aurora: it has no checkpoints of its own (the storage layer applies
  WAL), so the check is skipped there.

## Privileges and missing pieces

`pg_stat_database`, `pg_stat_bgwriter` / `pg_stat_checkpointer`,
`pg_stat_user_tables` and `pg_settings` are readable by any role.
`pg_stat_statements` may be missing (not installed, or not in
`shared_preload_libraries`); other sessions' query text needs
`pg_read_all_stats` (in `pg_monitor`). Each limit becomes one ADV000
LOW finding naming the fix (`CREATE EXTENSION pg_stat_statements;`,
`GRANT pg_monitor TO …`), the same words the slow-queries panel uses.
A check that errors is skipped and named in the status line; all
failing is an error, as for the live lint.

## Tests

Pure: every classifier at its boundaries (fixture numbers), the
version split (SQL chosen by `server_version_num`), the window text
(reset, never reset, too young), value parsing and formatting
(`8kB`-unit `shared_buffers`, `kB` `work_mem`, `MB` `max_wal_size`),
and the RDS table (every suggested setting has a row; static vs
dynamic matches `pg_settings.context` for the ones PG documents as
`postmaster`). Integration (docker, PG16): a temp spill under a tiny
`work_mem`, dead tuples by update churn with autovacuum off, and a
lock wait from two connections, each producing its finding from real
evidence; thresholds are passed in so the young test cluster's window
does not hide them.
