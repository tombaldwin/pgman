# pgman backlog

Open items only. Completed work: docs/backlog/archive.md. Active
window: PLAN.md — an item lives in exactly one of the two.

## Open

### From the release panel (2026-09-03) — post-release
- **ebman parity, deferred**: a `pgman query 'SQL' --json` subcommand
  alias for `--batch --sql … --format json`; a `--read-write` opt-in flag
  mirroring ebman's `--read-only`; the `D` key means describe in ebman
  and diff here; a Ctrl-K command palette on top of the `:` bar.
- **Tap aggregate memo on a real generation counter.** The memo added
  by the security batch keys on a content fingerprint (length + first
  and last timestamps) from a `thread_local`, because the ring's push
  and clear sites were outside that task's file scope. A `ring_generation:
  u64` bumped on every mutation is the stronger design; the fingerprint
  cannot false-hit but goes cold when a task migrates workers.
- **Update-check response cap is not streaming.** The 64 KiB cap checks
  `Content-Length` before and the body length after; a chunked response
  with no length buffers before the check. Needs reqwest's `stream`
  feature and a `take` — one Cargo feature away.
- **`has_where` is a whole-statement keyword check**, so a `DELETE` whose
  only `WHERE` is inside a subquery classifies as `with WHERE` (Confirm,
  not Block). Needs a real parser; documented in
  `docs/safety-and-privacy.md`.
- **`--tap-record` permission verification** was inconclusive in the
  sandbox (the agent's filesystem view and the subprocess's did not line
  up); the code mirrors `write_private`. Verify by hand on Linux.
- **Batch mode skips the directory-permission repair** that the TUI's
  `init_logging` does (data/config dirs stay at whatever mode they had).

Functional sections (the original M0 / M1 / M1.5 / M2 milestone
buckets were folded in once their initial passes shipped).

### Connection & chrome
- IntelliJ `dataSources.local.xml` password parsing — the
  `parse_local_passwords` referenced in `creds::intellij`'s doc
  comment doesn't exist yet. Today `PGPASSWORD` fills the gap.
- NUMERIC / unknown-type cell rendering in `conn::cell_to_string` —
  currently best-effort `Debug`-style for unknown OIDs.
- TLS `verify-ca` hostname-skip via a custom rustls verifier — the
  `sslmode=verify-ca` URL param currently behaves like `verify-full`
  because we don't override the hostname check.

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
- **Cell editing → UPDATE generation** — Enter on a cell in the
  grid (when the result is from a single table with a primary key)
  opens an inline editor; on save, generates `UPDATE … SET col =
  newval WHERE pk = …` and routes through the existing safety
  guard for confirmation. Every GUI tool has this; matches what
  TablePlus / DataGrip operators expect.
- **Result diff — PK refinement.** Key on the *actual*
  source-table PK (from the catalog) rather than the
  inferred-unique-column heuristic — would need the schema cache
  to carry PK columns (same gap as "Indexes under each table").
  The heuristic already covers the common id-column case.

### Schema browser — follow-ups
- DDL preview pane (live `pg_get_tabledef`-ish query) for the
  focused table.
- Sample-rows preview (live `SELECT * FROM tbl LIMIT 5`).
- **Indexes under each table** — needs a new fetch query
  (`pg_index → pg_class.indrelid → owning relation`) since the
  current `cache.indexes` only carries `(schema, name)` with no
  table linkage. Columns + PK/UK constraints already drill down.
- One-key DDL preview (copy CREATE-TABLE statement) on a focused
  table. Needs a live `pg_get_tabledef`-shaped query — separate
  from the now-shipped SELECT / INSERT template yanks.

### Schema wizard — live-query checks
The pure-cache pass (LINT001-004) + the live pass (LINT101-106)
now cover most painful real-world schema sins. Last one:
- **LINT107 — `serial` columns on tables with no PK using that
  column.** Common Hibernate-generated shape where a serial id
  isn't actually a PK — silently grows duplicate keys. Needs
  pg_depend traversal to detect the implicit sequence linkage.

### Performance / DBA — follow-ups
- **Page beyond `MAX_ROWS`.** v1 streaming landed (rows fetched
  via `query_raw` and capped at `grid::MAX_ROWS` so huge SELECTs
  no longer pull every row into RAM). The grid still shows only
  the first page; a portal-backed "fetch next N" / scroll-to-load
  is the v2.

### Code-review follow-ups (from the open-source-prep review)
- The older single-line inputs (`SaveQueryPrompt` name,
  `GridFilter`, `GridFind`, `HistorySearch`,
  `SchemaBrowserFilter`) still hand-roll append-only editing and
  could adopt the same `text_input::TextInput` widget (cursor
  movement, Home/End, Ctrl-W word-delete, paste) for consistency.

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
A new panel (`Mode::Advisor`) that reads `pg_stat_database`,
`pg_stat_bgwriter`, `pg_stat_statements`, `pg_stat_activity`,
and the tap stream, then produces a **ranked list of tuning
suggestions** with copy-pastable `ALTER SYSTEM SET …` snippets.
Each finding has severity, evidence (the stats that triggered
it), recommendation, and notes on whether a server restart is
needed.
- **Buffer cache hit ratio low** → `shared_buffers` ↑.
- **High temp_bytes / temp_files** → `work_mem` ↑; surface
  the spilling queries from the tap.
- **fsync wait time high** + many short transactions →
  `synchronous_commit=local` (or per-session for the relevant
  pool) / `wal_writer_delay` ↑.
- **Dead-tuple ratios over threshold** (already detected by
  LINT104) → per-table autovacuum scale factors.
- **Lock-wait pile-ups** in pg_stat_activity → `deadlock_
  timeout` review + surface the contending transactions.
- **Connection thrash** (high conn-establish rate from tap
  + low cached-plan rate) → pool sizing / prepared-statement
  cache settings on HikariCP.
- **RDS / Aurora mapping.** Each suggestion includes the
  AWS Parameter Group key + whether it's static (reboot) or
  dynamic. Aurora-specific knobs (`aurora_load_from_s3_role`
  etc.) flagged when the dialect probe says Aurora. Note:
  pg_stat_statements reset isn't available on RDS without
  rds_superuser — advisor surfaces deltas via stored
  snapshots instead.

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
- **Real `:about` command** once a typed-command palette lands. The
  `A` keybinding works in the meantime.
- **Keymap customization** — bindings are hardcoded today. A
  `~/.config/pgman/keymap.toml` letting operators rebind (or alias)
  keys would help muscle-memory transitions from k9s / Vim / Emacs
  and is a baseline accessibility requirement.
- **Multi-tab per-connection** — v1 shipped with shared
  connection (one DSN across all tabs). Per-tab connection
  targeting (each tab pointing at its own DSN) is the v2 — needs
  threading the active client through every query-spawn site,
  separate schema caches, separate cancel tokens.
- **Multi-line error footer.** `last_error` still renders one
  line and truncates at the right edge. Rich detail is now in
  the F2 overlay, so the footer staying one line is less
  painful, but wrapping to 2-3 lines would still help.
- **Vim-style bookmarks — persisted variant.** Session-local
  shipped (`m<a-z>` / `'<a-z>` over the grid (row, col)).
  Persisting them with the auto-save draft so they survive
  restart is a follow-up — would need a JSON-able snapshot
  that also carries the SQL fingerprint to detect re-runs.
- **psql backslash command routing — extras.** `\d` / `\dt` /
  `\dn` / `\?` / `\q` / `\timing` / `\l` / `\x` shipped (see
  Done → Editor). Remaining: `\df` (functions) — needs a
  list-functions catalog query.

### Completion — next round
- **JOIN ON FK suggestions** — in `JOIN orders o ON `, look at
  `pg_constraint` FK rows linking in-scope tables and offer
  `users.id = orders.user_id`-style predicates as multi-token
  candidates. Catalog fetch needs a new query for foreign-key edges.
  Higher-value, bigger scope than the other completion items here.

### Reuse from the sibling `ebman` project
Survey is in [Done]. Lift as the milestones reach them:
- `shell.rs` — PTY wrapper + key→bytes; verbatim. Use for `psql` /
  `pg_dump` / `claude` handoff (M2 / advisor / snapshots).
- Toast stack, Braille spinner, pill-chain widgets (`ui.rs`) — lift
  as chrome is built out.
- `state.rs` — line-oriented persisted state; adapt for saved
  connections / query history.
- `mode_action.rs` — `ConfirmModal` + countdown state machine; adapt
  for `safety::Guard::Confirm` prompts and backup / restore.
- `commands.rs` — `:command` registry feeding help + Ctrl-K palette.
- `form.rs` — multi-field modal; connection editor + safety.toml
  editor.
- `mode_detail.rs` — tabbed drill-down pattern.
- Smaller: `keys.rs`, `plugins.rs`, `update_check.rs`, `control.rs`,
  `cost_cache.rs` (→ schema / plan cache), `report_bug.rs` (PII
  scrubber).
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
  `custom` (config-driven table mapping; uflexi is one preset). Note:
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

