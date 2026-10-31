# pgman backlog

Open items only. Completed work: docs/backlog/archive.md. Active
window: PLAN.md — an item lives in exactly one of the two.

## Open

### Postgres-compatible databases — from a compatibility run (2026-10-29)

Each tried in docker against 0.4.0: CLI dump/restore (plain and
masked), MCP, and the TUI's panels.

- **TimescaleDB: masked copies of hypertables.** Refused for now (the
  plan names each hypertable). Masking them as they are lost the
  hypertable (it came back a plain table with chunk children) and every
  row of a compressed chunk (opaque `compressed_data`, NULLed). Real
  support: leave the chunks out, record each hypertable's dimensions,
  `create_hypertable` on restore before the rows go in, and read the
  rows through the hypertable (decompressed). Compression settings,
  continuous aggregates and policies would need recreating too. The
  product page leaves TimescaleDB unnamed until then.
- **CockroachDB: only queries work.** pg_dump, snapshot export,
  `pg_blocking_pids`, `EXPLAIN (FORMAT JSON)` and
  `pg_postmaster_start_time` are all refused. Not a target; but when the
  sessions panel fails to open there, the next `q` quits pgman, because
  nothing opened for `q` to close. That part is a bug anywhere a panel
  fails to open.

### Masked dumps — left from phase 2a (2026-10-29)
What the design note's section 2–9 asks for that 2a didn't build; the
later phases (2b encryption, 3 screens, 4 sampled validate, 5 the rest
of the transformers and stable keys, 6 subsetting) are in the design
note.
- **Schema literals.** Scan column defaults, CHECKs, view and function
  bodies and policies for string literals and name them in the summary
  (the schema is copied as it is).
- **The operator's own login.** Find the operator's git email in the
  data and say what it became, so they can log in to the copy.
- **The MCP confirmation's classifier summary**: what will be masked, as
  the terminal's summary says after.
- **A rules file's changes since last confirmed**: show the diff of
  what loosened, and journal a `rules_confirmed` event.
- **`pgman copies prune`**: everything past its expiry, confirming each
  database.
- **The TUI's expired-copies flag** in the header.
- **The rules digest in the dump's journal event** (and whether it was
  confirmed), so the account says which rules masked a copy. From the
  review panel, 2026-10-29.
- **Non-English and non-UK vocabulary**: `nom`, `prenom`, `telefon`,
  PESEL, CPF, BSN… A preset per language or country, chosen in the rules
  file. (privacy.md says the words are English and British.)
- **A unique integer under `hash`** can collide (a keyed hash modulo the
  type's range); a keyed permutation would keep it unique.
- **A person's id with no foreign key.** Keycloak's `event_entity.user_id`
  has none, so it is hashed while every declared reference keeps
  `user_entity.id`: the masked copy's events no longer join to users.
  Synapse is the other way: `users.name` gets a fake name, its ~60
  `user_id` columns a hash. Decide whether an undeclared `*user_id` joins
  the group of a column it matches by type and name. 2026-10-30.
- **Card expiry and other table-aware words.** `exp_month`/`exp_year` on
  a card table, a bare `code` on a TOTP table, a bare `number` on an SMS
  table, `rate` on a pay table: kept, since the column's words alone
  aren't personal. Table words would settle them. 2026-10-30.
- **Classifier noise left from the open-source run (2026-10-30).** `tax`
  and `pay` NULL amounts (`amount_tax`, `pay_invoices_online`);
  `license` a module's licence; `fname` gives Odoo's
  `ir_attachment.store_fname` a fake first name (every attachment's path
  breaks); location words (`geo_status_timeout`, `age_value`); an integer
  or interval named for a secret (`access_token_lifespan`) is still
  NULLed; `client` makes an OAuth client's `name` a person's. From the release
  panel: `visa_type` is kept by the `type` flag word; presets match a
  table's name in any schema.
- **From the v0.4.4 panel (2026-10-31).** Odoo's `/web/assets`
  attachments keep a checksum and lose their content, so a restored web
  client may need `-u web` (not tried on an instance). A rules-file
  `fixed` on a nullable key isn't checked against the referenced table: a
  value it lacks fails the restore's foreign key (it can't be checked
  from the catalog; validate could).
- **`pgman query` against an environment.** It takes `--dsn` only, so
  checking a column's type on an environment behind a bastion and a
  secret means the TUI. A `<target>` like dump's. Found 2026-10-30.

### Environments overview, like ebman's main screen (Tom, 2026-10-19)
One table of every database in an environment, replicas included, each
with its versions. Tom's immediate need is RDS, but the design must also
cover local databases, hand-listed groups of remote ones, and other
clouds. So it's an inventory layer, and RDS is one source of it.
- **Model.** An *environment* is a named group of *targets*. A target is
  one database on one server, with a role (primary, replica or direct),
  an endpoint, a tunnel and a credential reference. Environments live in
  `environments.toml` under `util::config_dir()`, never the project file,
  because they carry tunnels and secret names (the same rule as
  `connections.toml`).
- **Sources** (a trait; each produces targets). `static` takes names
  from `connections.toml` and covers local and hand-listed remote groups.
  `rds` runs `describe-db-instances` through the aws CLI, filtered by
  identifier or tag; it gives the role from the replica's source
  instance, the engine version and the endpoint, with no database
  connection. `docker` finds local containers exposing 5432. Cloud SQL,
  Azure Database and Neon/Supabase come later, behind the same trait.
- **Credential providers** are pluggable too: `password_env` and the AWS
  `secret` exist; GCP Secret Manager, Vault and 1Password are candidates.
  A templated secret name, `demo/database/{db}/credentials` with `{db}`
  and `{role}` filled per target, avoids one entry per database.
- **Probes** fill the columns. Postgres version comes from the source
  when it has one, else `SHOW server_version`. Schema version is
  `max(<column>)` of a configured version table. Pending patches reuse
  `query::patches`. Replica lag is `now() - pg_last_xact_replay_timestamp()`.
  Each probe is a read-only connection through the existing guards.
- **Safety.** A refresh connects to every database, so connecting is
  opt-in per environment (Prod off by default). Schema versions are read
  from the replica where one exists; a primary is touched only when it
  has none.
- **Surfaces.** A TUI screen in ebman's style that refreshes in the
  background on the shared frame clock (house rule: no independent
  tickers), and an `environments` MCP tool returning the same rows.
- **Order.** (1) and (2) are done (PLAN.md): the model, the sources,
  the `E` screen, connecting, AWS discovery, and opt-in probes with one
  ssh per bastion, and (3) began with `docker`. Cloud SQL, Azure
  Database and the others follow as they're needed.

### MCP server — after Part A (2026-10-19)
Parts A and B are in PLAN.md. Behind them, from the examplecorp Lead session
and Tom's decisions that day:
- **Applying patches to a copy of a live database.** The copy mode
  (Tab / MCP `mode: "copy"`) uses `CREATE DATABASE … TEMPLATE`, which
  needs nobody else connected, so it only suits an idle local or dev
  database. For a live one the options are a `pg_dump | pg_restore`
  copy (needs the client tools, slow on a big database, schema-only
  misses data-dependent failures) or an RDS snapshot restore (minutes
  to hours, costs money, needs IAM). Wants a decision on which, if any.

### From the robustness pass (2026-10-20) — not yet done
Four Fable reviewers (hands-on tmux, MCP security, environments
correctness, robustness + docs). The keys, status-line, completion,
small-UX, correctness and environments findings landed on branch
`robustness-ux` (PLAN.md); these remain:
- **Flyway `classpath:` locations** are refused, not resolved: pgman
  doesn't know a project's root, so the error names the directory to
  give instead. Resolving them needs a project root, and environments
  have none: pgman's working directory would silently match any
  environment to whatever project it was started in. Wants a
  per-environment `project = "~/src/app"` key — Tom's call.
- **Aurora on a real cluster**: readers sit under their writer from
  `describe-db-clusters`, tested against fixtures only. Whether a
  reader answers the lag probe's `pg_last_wal_receive_lsn` /
  `pg_stat_wal_receiver` usefully is unverified.
- **JOIN … ON with a multi-column foreign key** offers one predicate
  per column pair: the cached `FkEdge` carries no constraint name, so
  the pairs can't be grouped into one `a = b AND c = d` candidate.
- **MCP, informational**: on a read-only session `pg_advisory_lock`,
  `pg_notify`, `pg_logical_emit_message` and `txid_current` still run
  (no data changes; transient). Left off the denylist deliberately.

### From the release panel (2026-09-03) — post-release
- **The tap listeners' channel is bounded by count, not bytes**
  (`TAP_CHANNEL_CAPACITY` = 4096). The 0.1.1 run loop keeps up with
  every burst measured, but if the app truly stalls, 4096 × ~200 KiB
  events could still queue (~800 MiB). A byte budget would make the
  worst case honest. The adapter in `main.rs` also clones every event
  for `--tap-record` even when nothing is recorded.
- **ebman parity, deferred**: a `--read-write` opt-in flag mirroring
  ebman's `--read-only`; the `D` key means describe in ebman and diff
  here; a Ctrl-K command palette on top of the `:` bar.

### After v0.1.0 — Tom's decisions (2026-10-22)
- **Leaving read-only is quit, edit, relaunch** (0.1.1 product panel).
  `:readonly off` points at `safety.toml`; running a write then shows a
  confirm popup for a statement the popup says Postgres will refuse,
  and `y` gets SQLSTATE 25006 with the same hint. Options: a typed,
  session-only `:readonly off` (with its own confirm), and skipping the
  popup when the session is read-only. A safety posture call — Tom's.
- **Postgres-log parameters are all typed `text`**: `select $1 + $2`
  reconstructs as `'1' + '2'` (operator is not unique). JDBC binds are
  typed; leaving numeric-looking values bare would fix that case and
  break a text column holding `007`. Needs a rule.
- **`I` and `:fixture` treat a subquery alias as a table**:
  `SELECT * FROM (SELECT …) sub` infers `public.sub`, a table that
  doesn't exist. Refuse instead.
- **An unqualified FROM is assumed to be in `public`** for the INSERT
  yank and `:fixture`, ignoring `search_path`; `:fixture` also drops a
  given schema.
- **Tests under `tests/` build the library without `cfg(test)`**, so a
  yank reached from a journey or render test would hit the real
  clipboard. None does today; a runtime seam would make it impossible.
- **Log formats not yet read**: Log4j's bracketed logger (`DEBUG
  [org.hibernate.SQL] select …`); p6spy's multi-line Spring Boot
  starter format; Hibernate 5 ARRAY binds (only Hibernate 6's form is
  converted). datasource-proxy values containing `,` are ambiguous in
  its default format and split there (documented).
- **Tap: 64 silent connections lock a real client out for up to 30 s**
  (documented trade-off of the connection cap). A per-connection idle
  timeout shorter than 30 s, or evicting the oldest idle connection.
- **Require CI checks on `main`.** Branch protection blocks force-push
  and deletion but still requires no status checks (checked
  2026-10-24), so a PR can merge red. Setting them is a permission
  grant: Tom runs `gh api -X PUT …/branches/main/protection` with the
  nine `ci.yml` job names.

Functional sections (the original M0 / M1 / M1.5 / M2 milestone
buckets were folded in once their initial passes shipped).

### From the v0.2.0 release panel (2026-10-23) — LOWs left for later
- **Paging misses functions it can't see**: a function called inside a
  view, through `t.func` column notation, a user operator, a cast to a
  user type, or an RLS policy's function still pages. (A schema's own
  function by a listed built-in's name is caught by the probe.) An xid
  check after the first FETCH would catch writes (not `nextval`).
- **Qualify the rest of pgman's own SQL with `pg_catalog.`**: the paging
  probe, the cell-edit check and every recovery probe (the connect's,
  `apply_patches`', MCP's replica checks, the dashboard's, the advisor's
  context read) are; the schema fetch, the rest of the advisor's
  queries, the sessions and slow-query panels and the backslash commands
  still call `lower`, `unnest`, `format_type`, `pg_get_*`, … unqualified,
  so a schema listed ahead of `pg_catalog` on the operator's path can
  answer them (psql qualifies for this reason). And a failed probe reads
  as `in a transaction, so no paging`; say the probe failed instead.
- **Untested by mutation**: the driver's use of `severity_word` (needs a
  server with a non-English `lc_messages`); the session read-only passed
  at `run_generated_select` and the cell edit (pgman's own SQL, which
  can't carry the escape).
- **The cell-edit confirm shows a stale column name** (`SET b`) before
  the server-side identity check refuses it. Warn at plan time when an
  unaliased result column's name differs from the schema cache.
- **A standby pin outlives a promotion** across a same-DSN reconnect, as
  an environment's replica pin does. Re-ask `pg_is_in_recovery()` and
  unpin when it says no.
- **MCP read-only refusals carry no hint** since the 25006 hint moved out
  of `QueryErr::from`; give MCP its own (it forces read-only itself).
- **The 25006 hint footer at 80 columns** (panel's 3c) didn't reproduce
  for the fix agent at heights 16–30 in editor, grid and row detail. Get
  the panel's exact steps and size before closing it.
- **Advisor RDS facts to confirm**: the 24 h `idle_in_transaction_session_timeout`
  default is confirmed for Aurora only, not from AWS's RDS docs. Aurora's
  `autovacuum_vacuum_scale_factor` default is 0.1 (ADV003 reads
  pg_settings, so harmless). `aws_cli` always targets the Aurora cluster
  group; a future instance-only parameter would need the instance group.
- **Untested by mutation** (hand-driven only, need a real client): the
  `standby` plumbing in `spawn_run`; the lost-connection client drop;
  the `Failed` refusals in `db` / `spawn_run` / `spawn_page_request`.
- **`a_cell_edit_that_is_not_one_row_changes_nothing` flaked once** in a
  full integration run with other worktrees sharing the database.
- **`D` after closing a diff diffs a new result against the stale pinned A**,
  even from another table; only the title's A label shows it. Pin instead
  when the new grid's columns/source differ, or say A is still pinned.
- **The PK-keyed diff needs identical column order**: `SELECT id,t,n` vs
  `SELECT n,t,id` falls back to a full-row key (every row removed+added).
  Match key columns by origin and compare the rest by name.
- **An inferred diff key accepts a NULL key value**, and a change line
  renders SQL NULL and the string `'NULL'` the same.
- **A pinned diff side drops `truncated`**, so the title never says a side
  is a partial paged result.
- **Cell edit compares the FROM table by name only**: a same-named view in
  an earlier search_path schema over the base table is bypassed (same
  row, but INSTEAD OF triggers / CHECK OPTION skipped).
- **A row visible but not updatable (RLS / privilege)** reports "the UPDATE
  matched no row (deleted, or its key changed)"; say it may be policy.
- **A static environment's primary row is never probed** in the overview
  (STATUS/SCHEMA/… stay `—`) while its replica row is; same in v0.1.0.
- **Advisor**: ADV003's `ALTER TABLE … SET (autovacuum…)` queues behind a
  long query or an open pgman cursor; in the "autovacuum is behind" case a
  lower scale factor doesn't help the overdue vacuum.

### Credentials & config
- `${...}` placeholder resolution. (Blocked — needs real-world
  verification of Spring / SSM / 1Password mechanics; the
  `${op://}`-as-property-source assumption is still unconfirmed.)

### Reconstruction
- N+1: time-window heuristic once `ReconstructedQuery` carries
  timestamps.

### Editor — domain features
- Saved queries — possible v3 nice-to-haves: tags / folders,
  fuzzy match, body-only vs name-only scoping.

### Editor — authoring polish
A second round of editor quality-of-life features past the syntax-
highlighting / pg_format / auto-save / persistent-history / undo
batch.
- **Multi-line comment toggle for selection** — current
  `Ctrl-/` toggles the focused line. We don't have selection
  yet; once we do, apply per-line.

### Result grid
- **A failed cell save loses what was typed.** An UPDATE the server
  rejects (an invalid numeric) closes the editor; it should reopen
  with the value.
- **Cells holding a line break or control character can't be edited
  inline** (refused with a reason); a multi-line editor would.
- **`--demo` can't edit cells**: its grids carry no column origins.
- **Editing several cells of one row** as a single UPDATE.
- **An edit doesn't re-apply the filter or sort** to the edited cell,
  and a tab switched away from mid-save keeps its old snapshot.
- **An edit waiting on a row lock** can only be cancelled from the
  editor (`e`, then `Ctrl-C`).
- **Possible flake**: `a_patch_planned_outside_a_transaction_runs_vacuum_and_concurrently`
  failed once in a full parallel integration run; not reproduced.

### From the dump/restore review (2026-10-28)

- **The formula's `brew audit --strict` notes** (all older than 0.3.1):
  `license any_of: ["MIT", "Apache-2.0"]` rather than the SPDX string;
  `version` is redundant with the URL (but `scripts/update-formula.sh`
  rewrites that line, so change both together); the desc's lowercase
  "k9s" is the product's name and stays.

- **"prod" in a name protects with no override for a connection or a
  URL** (v0.3.0 panel, DBA L4). `restore --as ledger_products` and a
  local `product_catalog` database are refused as protected. An
  environment has `protected = false`; a connection has nothing. Tom's
  call: a `protected = false` on a connections.toml entry, or a word
  match rather than a substring.
- **`--skip-unreadable` with thousands of partitions** (DBA L5) passes
  one `--exclude-table` each, and pg_dump runs a catalog query per
  pattern. pg_dump 17+ takes `--filter FILE`: write the excludes there
  when the client is new enough.
- **Close the last window on a directory-format dump** (v0.3.0 security
  re-check, LOW). Between creating the partial directory and pg_dump's
  start, a swap would make pg_dump chdir through the link; the identity
  check still refuses to move it into place, but pg_dump would have
  written elsewhere. Open the directory right after making it
  (O_DIRECTORY|O_NOFOLLOW) and fchdir to it in pre_exec.
- **A resolver that times out stalls every dump, restore and run**
  (v0.3.0 security re-check, LOW). Protection resolves each protected
  static connection's host on every target, synchronously; with DNS
  down (VPN off) rather than NXDOMAIN, each costs the resolver's
  timeout before the first prompt. Resolve once per run, with a short
  timeout, falling back to the name.
- **SIGTERM / SIGHUP in the TUI's own paths** are handled; the CLI's are
  now too. A second signal ends the CLI at once; check that the TUI's
  patch run does the same.

- **A connection to a protected `rds` environment's host.** `restore`
  and `run` protect a `connections.toml` entry by its environment, its
  name or its host's name. A personal connection to the same RDS host
  that a protected `rds`-sourced environment lists (found through the
  aws CLI) isn't matched unless one of those says prod. Matching it
  means an RDS listing on every restore; worth it only if such
  connections turn up.

### From the v0.2.1 polish (2026-10-24)
- **`demo.gif` still shows 0.1.0's chrome.** `demo.tape` installs into
  the real `~/.cargo/bin` and runs under the real HOME; make it use a
  `target/` binary and scratch HOME / XDG dirs, then re-record.
- **The schema wizard's detail strip at 60x16** is clipped by height
  with no "more" marker.
- **The tap status still says "JAR backpressure"** (`src/app/types.rs`)
  for a drop count only TCP / UDP senders report; docs/tap.md now
  explains it, the UI string doesn't.
- **`--demo`'s no-log decision is pinned only as a pure function**:
  nothing fails if `main()` calls `init_logging()` unconditionally
  again. A pty-driven subprocess test would pin the call site.
- **The tap's transactions and pools views at 60 columns** leave their
  text column no room: the fixed numeric columns fill the row, which
  ends on a bare `…`. Drop a numeric column at that width instead.
- **The tap's pools view at 80 columns** heads its column `POOL · APP`
  but shows the pool only: the app name never gets the 8 columns
  `pair_cell` needs.
- **`\e` under `--demo`** still writes (and deletes) a temp file for the
  external editor; decide whether that breaks the "no disk writes"
  promise or is the operator asking for it.

- **The schema reload has no `lock_timeout` backstop**: it skips
  relations under another session's ACCESS EXCLUSIVE lock as the connect
  does, but a lock on a catalog table can still stall it.
- **A FATAL through `TxClosed`, `CellUpdated` or `PageLoaded`** (or one
  the bootstrap's own queries swallow) doesn't reach the
  connection-lost card yet; only `QueryFailed`'s does.
- **A failed `:readonly on` under a read-only profile** drops the header
  to `writable`; the error branch only keeps a replica pin.
- **The FATAL-ends-the-session check under a non-English
  `lc_messages`** is covered by `severity_word`'s pure test only.

### Performance / DBA — follow-ups
- **Paging: one unexplained cursor close while hand-driving.** After
  Esc and `M` arrived together (read as Alt-M), a later `M` said the
  cursor had closed while the server still showed `idle in
  transaction`. Not reproduced in four tries; look with the log on.
- **Filtering after scrolling down leaves the grid scrolled** so only
  the selected row shows, blank below (reproduces unpaged at 900 rows;
  paging hits it more, since a fetch jumps the selection to row 1001).
- **Paging's 60 s idle / 10 min open limits are constants**; they could
  be `pgman.toml` settings.
- **The advisor and the live lint run on the editor's session**, so
  inside an open user transaction `pg_stat_*` reads are the
  transaction's cached snapshot and a refresh can be stale.
- **MCP and the HTML report don't carry the advisor's evidence.**
  `schema_lint` returns lint findings only; `report.rs` lists ADV rows
  without evidence or `advice`.
- **ADV002's suggested role** is the worst spiller's, even when that
  spill is past 4x `work_mem` and the setting can't fix it; pick per
  role instead.

### Code-review follow-ups (from the open-source-prep review)
- The older single-line inputs (`SaveQueryPrompt` name,
  `GridFilter`, `GridFind`, `HistorySearch`,
  `SchemaBrowserFilter`) still hand-roll append-only editing and
  could adopt the same `text_input::TextInput` widget (cursor
  movement, Home/End, Ctrl-W word-delete, paste) for consistency.
  Tried in the backlog pass (2026-10-21) and skipped: it retypes five
  state fields used ~90 times across 14 files — an architecture item
  wanting its own design note, not a drive-by.

### JDBC tap — layered build
The committed observability path. **pgman-tap is not a
ground-up JDBC wrapper** — it's a thin
[`QueryExecutionListener`](https://github.com/jdbc-observations/datasource-proxy)
on top of `datasource-proxy`, which already handles the
hard parts (prepared-statement unwrapping, parameter
capture, HikariCP compatibility, Spring Boot decorator
wiring). The JAR's job is to assemble `TapEvent`s and ship
them to pgman; pgman's job is to combine the app-side
stream with its DB-side introspection. Each layer below is
independently shippable; **L1 + L2 have landed on the
pgman side** (receive + render + CLI flag + hot-template
grouping + per-caller rollup + live N+1 detection). **L5
is deferred** until L1-L4 prove out and users ask for an
auto-tune loop (workload-replay drift is genuinely unsolved
and competes with mature tools — revisit then).

#### Readiness — "open pgman in a Java project and it just works"
Strategic checkpoint: how close are we to the user
experience where running pgman in a Spring Boot project
gives instant live-query monitoring without manual wiring?
Roughly **60-70% there**, with the gap concentrated in a
small number of items.

**What already works** when an operator runs pgman in a
Java project today:
- DB connection auto-discovered from `application*.yml` /
  `application*.properties` / `.idea/dataSources.xml` /
  `.pgman/pgman.toml`.
- Full DB-side analysis: schema browser, EXPLAIN tree,
  sessions + slow-queries panels, schema wizard
  (LINT001-106), safety classifier, history /
  saved-queries / bookmarks.
- The full tap-receiving pipeline: TCP listener on
  `--tap-listen`, four-view TapMonitor (List / Hotspots /
  Callers / N+1), live N+1 detection, chrome `N+1 ×N` and
  `TAP` badges.

**What's missing for "just works"** — items below are the
critical path, ordered by leverage:

- **`pgman-tap` JVM library — separate repo**
  *(critical, 1-2 weeks)*. The single biggest blocker for
  end-to-end "just works." A thin `QueryExecutionListener`
  on `datasource-proxy` + a Spring Boot starter. Required
  v1 features documented in the L1 open list below: PII
  redaction with defaults, threshold-gated caller stack
  walk, heartbeat every 5s, synthetic txn ids with
  `TxnBoundary` emission, TCP length-prefixed transport.
  Once shipped, operator wiring is "add one Gradle line +
  one `application.yml` line" → live events in the panel.

Once that last critical-path item lands, the experience
is: `cd my-spring-app/ && pgman` → tap panel populates in
seconds, schema browser fully functional, EXPLAIN one
keypress away, and L4's evidence-packet handoff is the
right shape for fixing the issues the operator sees.

#### L1 — receive + render *(pgman side landed; see docs/backlog/archive.md → JDBC tap)*
The pgman-side receive + render layer is shipped. Open
work that still belongs to L1:
- **`pgman-tap` JVM library** *(separate repo / Gradle
  module)*. A `QueryExecutionListener` registered against
  `datasource-proxy`, plus a Spring Boot starter that wires
  it via a `BeanPostProcessor` when
  `pgman.tap.enabled=true`. Required v1 features:
  - PII redaction with sensible defaults (mask numeric
    strings ≥11 digits, mask anything matching an email
    regex, mask columns in `pgman.tap.redact-cols`).
    `params_redacted` rides on the event so pgman renders
    a visible marker.
  - `caller` stack walk opt-in + threshold-gated
    (`pgman.tap.caller=true`,
    `pgman.tap.caller-threshold-micros`, default 5000).
    Java stack capture costs 1-10μs per call; at 10k QPS
    this matters.
  - Heartbeat every 5s with cumulative
    `dropped_events_total` from the JAR's in-process ring.
  - Synthetic `txn` ids: `conn-id#seq`, rolled at
    `Connection.commit/rollback`. Emit a `TxnBoundary`
    event at the same moment so pgman can retroactively
    close out the synthetic txn.

- **Tap: nothing turns the `TAP` badge amber when events stop.**
  `TapHealth`'s doc promises it; no code implements it.
- **`--tap-record`'s drop marker mixes two drop counts on replay.** It
  is a heartbeat carrying pgman's own listener drops, so a replay adds
  them to the sender's `dropped_events_total`.
- **The start card's TAP line turns on queries only**, while the
  footer badge now also lights for heartbeats and commits/rollbacks.
- **After `c` (clear) with queries but no heartbeats, the List view
  shows the setup hint** instead of an empty list.
- **OTLP ingest ignores `db.query.text` / `db.system.name`** (the
  newer semantic-convention names); such spans are skipped
  (documented in `docs/tap.md`).
- **The tap event overlay has no `y`** to copy the SQL or the stack,
  and a long word (an exception class name) is hard-split at the
  column edge rather than wrapped at a `.`.

#### L2 — insights
Once events are flowing, the panel becomes an analysis surface.
All of these are pure-classifier work over the ring buffer,
no new I/O.
- **Pool-saturation gauge.** *(partial: live gauge landed;
  see Done → JDBC tap.)* The fifth TapMonitor view groups the
  ring by pool name and shows distinct-connection breadth,
  peak in-flight concurrency, query volume / errors, busy
  time, and p95. Still pending — the `saturation %` vs the
  configured HikariCP max: a max value isn't derivable from
  query events, so it waits on the JAR shipping `pool-max`
  in its heartbeat (or a `pgman.tap.pool-max` config).
- **Read-replica awareness.** *(partial: pool display
  landed; pool-role classification still pending.)* The
  Transactions view + report now show which connection
  pool each txn ran against (`primary` / `replica` /
  whatever the operator named their HikariCP pools). Once
  the JAR ships `pgman.tap.pool-role=primary|replica`,
  pgman can colour-code reads vs writes and flag writes
  hitting the replica pool (the common Spring
  `@Transactional` bug). Until then, pool name alone is
  enough to spot misroutings if the operator's naming
  convention encodes the role.

#### L3 — tuning advisor
The server-stats round shipped in the schema wizard (`W`: ADV001–005
with evidence, stats window, setting and the RDS / Aurora mapping —
`docs/backlog/config-advisor.md`). Still open:
- **fsync wait time high** + many short transactions →
  `synchronous_commit=local` (or per-session for the relevant
  pool) / `wal_writer_delay` ↑. Needs `pg_stat_wal` timing.
- **Temp spills from the tap**: name the spilling app queries, not
  only `pg_stat_statements` rows.
- **Connection thrash** (high conn-establish rate from tap
  + low cached-plan rate) → pool sizing / prepared-statement
  cache settings on HikariCP.
- **Stored snapshots for deltas**: `pg_stat_statements` reset isn't
  available on RDS without rds_superuser, so diff stored snapshots.

#### L4 — agent handoff (Claude Code, Cursor, Aider, …)
pgman's job is to build a **structured evidence packet**;
the agent's job is the code change. The handoff is
agent-agnostic: writing the packet + a Markdown prompt to a
known path is the lock-in point; which agent picks it up is
the operator's choice. The framing is "starts the
conversation with structured evidence," not "fixes the bug
automatically" — real-world JPA N+1 has many shapes
(`@EntityGraph`, DTO projection, service-layer rewrite,
`@BatchSize`) and the agent will need to read the calling
code to decide.

New keybinding `C` on any actionable finding (N+1 /
missing-index / slow-query / advisor item) opens a
sub-menu of supported actions; the chosen one:
1. Writes `~/.cache/pgman/handoffs/<uuid>.json` (the
   evidence) and `~/.cache/pgman/handoffs/<uuid>.md` (a
   prompt that references the JSON via `@<path>`).
2. Copies the `.md` path to the clipboard.
3. If a recognised agent CLI is on PATH
   (`claude` / `cursor` / `aider` / `cody`), offers to
   shell out to it with the prompt pre-loaded; otherwise
   the operator pastes the path into whatever they use.

The evidence packet is versioned and stable. **Crucially,
it includes the contents of the source files the agent
will need to read** — otherwise the agent burns prompt
tokens grepping:
```json
{
  "v": 1,
  "kind": "n_plus_one" | "missing_index" | "slow_query"
        | "advisor_finding",
  "issued_at": "...",
  "summary": "...",
  "evidence": {
    "fingerprint": "SELECT * FROM accounts WHERE id = ?",
    "call_count": 47,
    "window_ms": 180,
    "p95_micros": 1200,
    "samples": [ /* up to 5 TapEvents with full SQL,
                    params (post-redaction), duration,
                    caller stack */ ],
    "explain": "...",                  /* on-demand fetched */
    "schema": { /* table + column + index info for involved
                   relations from the schema cache */ },
    "pg_stat_statements_row": { ... }  /* when available */
  },
  "source_context": [
    /* The actual files the agent will need to read.
       For N+1: the entity at caller[0], its repository,
       and the calling service. For missing-index: the
       Liquibase / Flyway migration dir's most recent
       changeset (so the agent matches existing style).
       For slow-query: any SQL fragments or @Query annotated
       methods that reference the involved tables. */
    { "path": "src/main/java/com/example/Order.java",
      "contents": "..." },
    { "path": "src/main/java/com/example/OrderRepository.java",
      "contents": "..." }
  ],
  "project_hints": {
    "build_tool": "gradle" | "maven",
    "test_command": "./gradlew test",
    "migration_dir": "src/main/resources/db/changelog",
    "agent_detected": "claude" | "cursor" | "aider" | null
  }
}
```
Categories in order of leverage:
- **N+1 → JPA fix.** Evidence includes the entity, its
  repository, and the calling `@Service` method. Agent
  proposes `@EntityGraph` / `JOIN FETCH` / `@BatchSize` /
  `@Fetch(FetchMode.SUBSELECT)` *or* a DTO projection /
  service rewrite. Operator reviews and approves.
- **Missing index → Liquibase / Flyway migration.** Pulls
  LINT101 evidence + the actual query patterns from the tap
  stream + the project's most recent migration as a style
  template; agent writes a changeset with rollback included.
- **Slow query → rewrite.** EXPLAIN + schema + tap template +
  any `@Query` JPQL/SQL annotations referencing the involved
  tables. Agent proposes a rewrite; pgman validates by
  running it and diffing the plan.
- **Advisor finding → config change.** For app-side
  (HikariCP, prepared-statement cache): agent edits
  `application.yml`. For server-side (`ALTER SYSTEM`):
  pgman runs the SQL after a confirmation step.
- **MyBatis / jOOQ / plain JDBC.** Primary support is
  Hibernate / JPA. For jOOQ-generated SQL the fix surface
  is narrower (mostly "add an index"); for plain JDBC the
  agent edits the SQL string directly. MyBatis sits in
  between.

After accept, pgman keeps watching the live tap; the
fingerprint disappearing / p95 dropping is the success
signal, shown back in-panel.

#### L5 — workload capture + replay + auto-tune loop *(deferred)*
**Not in v1.** The biggest unlock conceptually, but several
of the hard parts compete with mature tools (pgreplay,
pgbench, JMH, k6) and the genuinely novel piece — closing
the loop with an agent picking among candidates — only
makes sense once L1-L4 prove out. Honest constraints to
revisit before scoping:
- **Workload-replay drift is unsolved.** Replaying recorded
  SQL against a different dataset returns different rows,
  breaks FK constraints, hits caches differently. pgreplay-go
  handles the simple cases; the hard cases require synthetic
  data generation tied to the captured workload.
- **RDS snapshot restore is 5-15 min** and incurs RDS
  charges per candidate. Default has to be local PG via
  Testcontainers / `pg_tmp`; RDS-clone is the power-user
  path.
- **pg_stat_statements_reset()** requires superuser, not
  available on RDS. Bench needs to use stored snapshots and
  diff.
- **Real benchmarking craft** (warmup runs, percentile
  stabilization, randomised ordering, cold/warm cache
  modes) isn't optional once the agent is making choices
  based on the numbers.
- **Standalone deliverables that have already shipped** —
  `--tap-record <path>` (JSONL capture sink on the adapter
  task) and `--tap-replay <path>` (JSONL streamer into the
  same pipeline) both landed early. The remaining L5
  pieces (replay-script generator, bench harness, agent
  loop) stay deferred.

### Observability — comparison matrix
Where pgman + tap sits relative to existing tooling, so the
differentiation is explicit and we don't reinvent things
we don't need to:

| Tool                     | App capture | DB stats   | Advisor       | Code-fix loop      | Cost    | Posture       |
|--------------------------|-------------|------------|---------------|--------------------|---------|---------------|
| p6spy                    | logs only   | ✗          | ✗             | ✗                  | free    | dev library   |
| datasource-proxy         | programmable| ✗          | ✗             | ✗                  | free    | library       |
| Hibernate Statistics     | counts only | ✗          | ✗             | ✗                  | free    | built-in      |
| OpenTelemetry + Jaeger   | ✓ generic   | partial    | ✗             | ✗                  | free    | generic APM   |
| pgwatch / pgwatch3       | ✗           | ✓ rich     | ✗             | ✗                  | free    | self-hosted   |
| POWA                     | ✗           | ✓          | ✓ HypoPG      | ✗                  | free    | self-hosted   |
| PgHero                   | ✗           | ✓ basic    | ✓ basic       | ✗                  | free    | Rails app     |
| pganalyze                | ✗           | ✓ rich     | ✓ best-in-class| ✗                 | $$$     | SaaS          |
| Datadog DBM + APM        | ✓ via APM   | ✓          | rule-based    | ✗                  | $$$$    | SaaS          |
| AWS Performance Insights | ✗           | ✓ (RDS)    | partial       | ✗                  | $ (AWS) | RDS-only SaaS |
| **pgman + tap (planned, L1-L4)** | ✓ (JDBC) | ✓     | ✓ (L3)        | ✓ agent (L4)       | free    | offline TUI   |
| **pgman + tap (planned, L1-L6)** | ✓ (JDBC) | ✓     | ✓ best-in-class| ✓ agent (L4)      | free    | offline TUI   |

**Strategic bar: match or beat pganalyze.** pganalyze is the
best-in-class advisor in this space — they earn the $$$
SaaS price because their Index Advisor (workload-aware,
HypoPG-driven) is genuinely strong. L1-L4 alone get us
*close* to pganalyze on advisor quality plus a code-fix
loop they don't have; L6 below is the additional work
needed to match them on the analytical depth that justifies
their price. The combination of (offline + free + agent
handoff + matching-or-better advisor) is a stronger product
than pganalyze for the right audience, even though we'll
never be a hosted multi-server dashboard.

#### L6 — closing the gap to pganalyze
The asks below are what's needed to claim *match-or-beat
on advisor depth*. Each is independently shippable; the
Index Advisor is the highest-leverage and should land
first.

- **HypoPG-driven Index Advisor.** Workload-aware index
  recommendations using the live tap stream +
  `pg_stat_statements` + the schema cache as inputs, scored
  by predicted query-time impact via HypoPG hypothetical
  indexes. The advisor:
  - enumerates candidate single-column / composite indexes
    from the predicates seen in real queries (the tap and
    pg_stat_statements both provide this);
  - calls `hypopg_create_index(...)` for each candidate,
    re-EXPLAINs the affected fingerprints, sums the
    predicted savings weighted by call frequency;
  - drops the hypothetical, returns a ranked list with
    impact estimates ("would save ~2.3s/min based on
    current workload");
  - hands the top finding to L4 to write the Liquibase /
    Flyway migration.
  Requires `CREATE EXTENSION hypopg` (available everywhere
  including RDS as of recent versions). Gracefully degrades
  to "missing extension" when it isn't installed.
  **This is the L6 feature that earns pganalyze parity.**
- **Time-series history (local SQLite).** Append-only
  `~/.cache/pgman/history.db` capturing pg_stat_*
  snapshots every 30s while pgman is running. ~1 GB / 30
  days of moderate-workload data; rotates on a configurable
  cap. Views over a time window let the advisor diff
  current vs. baseline instead of relying on the live ring
  buffer alone. Cleanly within "offline-first" — no daemon,
  no network, no shared state across operators.
- **Plan-change detection.** Per fingerprint, store
  `EXPLAIN (FORMAT JSON)` snapshots in the history DB; diff
  on each new run; surface plan-shape changes (new
  sequential scan, switch from index to bitmap, cost ↑×N).
  Often the first signal of statistics drift or autovacuum
  falling behind.
- **Bloat tracking — time series.** Promote LINT104 from a
  point-in-time check to a tracked metric per table; show
  the trend and the slope. Pairs with the autovacuum
  advisor finding so the recommendation is "your
  autovacuum is keeping up / falling behind."
- **Multi-server fleet view.** The connection picker
  already enumerates multiple DSNs. A new `Mode::Fleet`
  panel aggregates advisor findings across them — one
  table per finding type, columns per server — so an
  operator running pgman against a dev / staging / prod
  trio sees patterns at a glance. Cheaper than pganalyze's
  fleet view because there's no hosted cost; harder
  because there's no shared store.
- **Alerting via webhooks** *(opt-in)*. When a finding
  triggers (N+1 detected, plan-change regression, bloat
  ratio over threshold, advisor recommendation
  promoted to high-severity), optional outbound HTTP POST
  to a configured URL. Stays consistent with offline-
  first: pgman doesn't *run* the alerting service, it just
  pokes a webhook when the operator's terminal sees
  something worth knowing. Slack / PagerDuty / Discord /
  GitHub Issues all support inbound webhooks; one
  configuration point covers them.

#### Where pgman *will* lose to pganalyze
Honest about it so we don't chase the wrong wins:
- **Multi-server hosted dashboard.** They have a web UI,
  shared state, multi-user access, persistent history
  centralized. pgman is per-operator + per-session +
  offline. The fleet view in L6 narrows the gap; it
  doesn't close it.
- **Long-running collection.** Their daemon collects 24/7;
  pgman only collects while it's open. Time-series history
  (L6) helps when an operator opens pgman after the fact
  (the history DB stays around) but doesn't replace
  continuous unattended monitoring.
- **Email / SMS / PagerDuty native integrations.** Our
  webhooks are the operator's responsibility to route.
- **Enterprise SSO, audit logs, RBAC.** Not in scope.

#### Where pgman beats pganalyze (even at advisor parity)
- **Code-fix loop (L4).** pganalyze's advice ends at "add
  this index" / "run this VACUUM". pgman's L4 goes through
  to a Liquibase changeset, a JPA refactor, an
  `ALTER SYSTEM` execution — committed back to the
  operator's repo via an agent.
- **App-side context (tap).** pganalyze sees only the DB.
  pgman correlates DB facts with `caller`, `app`, `pool`,
  `txn` — so "this hot template comes from
  `OrderService.loadOrders`" is one keystroke away, not a
  jira ticket back to the developer.
- **Interactive EXPLAIN with hypothetical indexes inline.**
  Press a key on a slow query to see "what would this look
  like if I added that index?" without modifying the
  database. pganalyze does this in their dashboard; pgman
  does it in the terminal next to the query.
- **Free + offline + air-gapped.** A bracket of users
  pganalyze fundamentally can't reach.
- **Open source.** Forkable, inspectable, extensible. Less
  important than the technical wins but matters to the
  audience.

### What we're explicitly NOT building
Stated so it doesn't get rediscovered and accidentally
re-scoped in:
- **Our own JDBC wrapper.** Build on `datasource-proxy`.
- **Bytecode instrumentation / a Java agent.** Heavier to
  build and maintain; OTel ingest covers the same audience.
- **A hosted dashboard or daemon.** Per-operator,
  per-session. The local SQLite history (L6) is allowed;
  a long-running multi-user service is not.
- **Multi-tenant / SaaS deployment.** Forever.
- **A production-path proxy.** See "Wire-protocol proxy"
  below — kept as a deferred fallback only.
- **A full pg_stat_statements / pg_stat_activity
  replacement.** Surface them, don't replicate them.
- **Workload replay v1.** L5 is deferred precisely because
  the existing tools cover most of it and the closed-loop
  piece needs L1-L4 to prove value first.
- **Email / SMS / PagerDuty / etc.** Webhook out; let the
  operator route.
- **Enterprise SSO / RBAC / audit.** If you need it,
  pganalyze is the right answer.

### Observability — fallback paths
The JDBC tap doesn't cover every situation. These stay on
the list for the cases where it can't reach.

- **Log-tail / live monitor mode** *(non-JDBC workloads with
  log access)*. New `Mode::LogMonitor` panel + `log_tail.rs`
  module that streams the Postgres CSV log
  (`log_destination=csvlog`, `log_min_duration_statement` set
  low) and pipes each line through the existing
  `query::reconstruct` parser. Same insights as L2 but with
  lower context (no caller stack, no pool name, params
  redacted when the log redacts them). Useful when the
  workload isn't JDBC, or before the JAR is deployable.
  Implementation hooks: `log_tail::start(path, tx)` using
  the `notify` crate for rotation events with a polling-by-
  inode fallback; auto-detect path via `SHOW log_directory`
  / `SHOW log_filename`; reuse the SSH-tunnel infra so
  remote logs come along for free.

- **Wire-protocol proxy** *(deferred; polyglot fallback
  only)*. `pgman --proxy --listen :6432 --upstream
  prod-db:5432` speaking v3 frontend/backend, byte-pumping
  with a tee parser that decodes the few message types we
  care about (`Q`, `P`, `B`, `E`, `D`, `C`, `R`, `N`/`E`)
  and passes the rest opaquely. Only worth building if a)
  we need polyglot workload visibility, b) logs aren't
  accessible, and c) the JDBC tap isn't sufficient. Costs
  called out so they don't get rediscovered: ~3-6 weeks for
  a v1 that doesn't lose connections under load; TLS-on-
  both-sides passthrough is a cert-management mess; auth-
  method translation (SCRAM) non-trivial; cancel-request
  handling needs paired secret-key rewriting per session.
  Production deployment of a proxy is a different product
  anyway — pgbouncer-shaped, not pgman-shaped.

### UX
- **Two footer messages still carry a full config path** (2026-10-22,
  found doing the read-only hint). Neither is a `hint:` line. One is the
  startup error for a `config.toml` that doesn't parse
  (`redact::config_default_from`: `<full path>: <toml error> —
  redaction is on until it parses`, set as `last_error`, so F2 shows
  the same text). The other is the environments save status (`added
  <names> to <full path>`). Name the file in the footer, as the
  read-only refusal now does. The parse error would need its path in
  an error detail for F2 to keep it.
- **The F2 error overlay wraps mid-word and cuts a long hint at 80x24**
  (2026-10-22). `push_kv` wraps by character (`transact` / `ion`,
  `do` / `cs/configuration.md`), and at 80x24 the read-only refusal's
  path hint runs past the overlay's last row with no sign that there
  is more.
- **Keymap customization** — bindings are hardcoded today. A
  `~/.config/pgman/keymap.toml` letting operators rebind (or alias)
  keys would help muscle-memory transitions from k9s / Vim / Emacs
  and is a baseline accessibility requirement.
- **Multi-tab per-connection** — v1 shipped with shared
  connection (one DSN across all tabs). Per-tab connection
  targeting (each tab pointing at its own DSN) is the v2 — needs
  threading the active client through every query-spawn site,
  separate schema caches, separate cancel tokens.
- **Vim-style bookmarks — persisted variant.** Session-local
  shipped (`m<a-z>` / `'<a-z>` over the grid (row, col)).
  Persisting them with the auto-save draft so they survive
  restart is a follow-up — would need a JSON-able snapshot
  that also carries the SQL fingerprint to detect re-runs.
### Reuse from the sibling `ebman` project
Already shared through `tb-tui-common`: chrome, splash, overlay,
`TextInput`, theme, font probe and the path helpers. pgman has its own
update check and `:` command registry (`COMMAND_NAMES`, feeding Tab
completion and help). Still to lift, as features reach them:
- `shell.rs` — PTY wrapper + key→bytes; verbatim. Use for `psql` /
  `pg_dump` / `claude` handoff (snapshots / advisor).
- Toast stack (`ui.rs`).
- `state.rs` — line-oriented persisted state.
- `mode_action.rs` — `ConfirmModal` + countdown state machine; adapt
  for backup / restore.
- `form.rs` — multi-field modal; connection editor + safety.toml
  editor.
- `mode_detail.rs` — tabbed drill-down pattern.
- `report_bug.rs` (PII scrubber), `cost_cache.rs` (→ plan cache).
- Skip (AWS-specific): `profiles.rs`, `sso.rs`, `aws.rs`.

## v2 — AWS (not started)

- **RDS context panel** — for the current connection, pull the last
  hour of CloudWatch CPU / connections / IOPS / read-write latency
  into a corner widget. Connects the slow query you're tuning to the
  metrics that are actually on fire. Wants the same AWS-creds
  resolution as the items below.
- `discover_aws.rs` + `mode_databases.rs`; `--rds <id>` launch handoff
  from ebman. (Add a one-line backlog item in *ebman* for the
  spawn-pgman action.)
- `rds_logs.rs` + `mode_logs.rs` — `DescribeDBLogFiles` / CloudWatch;
  feeds pglog.
- `creds/{ssm,secrets}.rs` + `creds/onepassword.rs` — placeholder
  resolution.
- `migrate/` — `MigrationStrategy` trait + `flyway` / `liquibase` /
  `custom` (config-driven table mapping; a real schema is one preset). Note:
  `pending()` is not implementable for a DB-table-only strategy —
  make it an optional capability.
- `mode_params.rs` — RDS parameter groups; view / diff +
  `ModifyDBParameterGroup` with confirmation; surface pending-reboot.
- `mode_dashboard.rs` — live PG stats + RDS CloudWatch metrics.
- `advisor.rs` + `mode_advisor.rs` — health-snapshot → `claude` CLI
  review; interactive handoff via `handoff.rs`. Snapshot must be
  scrubbable (egress).

## v2 — local DB sync & backups (not started)

Pull a remote database down for local testing; keep tagged backups.

- `snapshot.rs` — snapshot store under `util::data_dir()`: each
  snapshot is a `pg_dump` artifact (custom format) plus a metadata
  record (source DSN redacted, timestamp, size, db version, tag,
  pinned flag). A TOML / JSON index lists them.
- `mode_snapshots.rs` — list / create / restore / tag / pin / delete.
  - Create: `pg_dump` a remote DB → store. Capture the server major
    version.
  - Restore: `pg_restore` (or `psql`) into a chosen local target DB,
    with a confirm step (restore is destructive to the target).
  - Pin protects a snapshot from prune; tags group them
    (`pre-migration`, `prod-2026-05`, …).
- Version skew: `pg_dump` / `pg_restore` must be ≥ the server major
  version — detect and warn, or locate a matching client binary.
- Retention: optional prune of un-pinned snapshots past a count / age
  limit.

## v3+ — deferred

- **Redaction / anonymisation** on snapshot restore — declarative
  rules (`null` a column, fake an email, hash an id) applied as data
  lands locally, so local testing never holds real PII.
- `catalog/` — version-aware catalog trait; EPAS support.
- JPA entity ↔ table mapping.
- Migration safety linter (lock-heavy operations).
- EXPLAIN plan diff.

