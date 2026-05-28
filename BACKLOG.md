# pgman backlog

The shape: `## Done` is the historical record. `## Open` is what's
actually open. v2+ sections live below. Anything that's shipped lives
under Done, no matter which milestone it came from.

## Open

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
- **Profile-specific overrides** (`application-prod.yml` overrides
  `application.yml`). Today each profile file is parsed
  independently and a profile-only file with a partial datasource
  block (e.g. password override) produces no pick.
- `${...}` placeholder resolution. (Blocked — needs real-world
  verification of Spring / SSM / 1Password mechanics; the
  `${op://}`-as-property-source assumption is still unconfirmed.)

### Reconstruction
- N+1: time-window heuristic once `ReconstructedQuery` carries
  timestamps.

### Editor — domain features
- **Saved queries — v2.** v1 ship covers save / list / load /
  delete. Still to come: in-panel rename / search, and
  `:param`-style placeholder prompts on load (`SELECT … WHERE
  id = :id` → prompt for id at load time).
- Capture-current-state → write a fixture (reverse of the DBUnit
  apply script).
- Per-database `CleanMode` config (which truncate strategy each db
  uses for DBUnit apply).

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
- **Result diff.** `D` pins the current result as A; the next run
  becomes B; a diff view shows row-by-row adds / removes / changes
  keyed by the source table's PK (or by full-row hash when no PK is
  inferred). Killer feature for "did my migration / batch update
  break anything?" workflows. Probably wants its own
  `Mode::ResultDiff` and a small `query::row_diff` pure module.

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

- **OTLP-over-HTTP peer ingest.** *(landed; see Done →
  JDBC tap.)* Cheapest path to a populated tap panel; any
  OTel-equipped JVM can stream spans straight in without
  the pgman-tap JAR. Operator sets
  `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318`
  + `OTEL_EXPORTER_OTLP_PROTOCOL=http/json` and starts
  pgman with `--tap-otlp :4318`.
- **Auto-enable `--tap-listen` in detected Spring projects.**
  *(landed; see Done → JDBC tap.)* When
  `creds::spring::detect_java_project` fires and the
  operator didn't pass `--tap-listen`, pgman binds
  `127.0.0.1:7432` automatically with an explicit startup
  log line. OTLP stays opt-in (port collision risk with
  the standard OTel collector).
- **Empty-state install hint.** *(landed; see Done →
  JDBC tap.)* TapMonitor "no events yet" now renders two
  copy-pastable setup routes: Route 1 (OpenTelemetry,
  works today) with the env vars + `--tap-otlp` flag, and
  Route 2 (pgman-tap, richer context) with the Spring
  Boot starter snippet, honestly flagged as
  in-development.
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
- **Optional: `:tap-replay <path>`** *(~1 day)*. Useful as
  a demo / development tool even before the JAR exists —
  replay a captured event stream into the listener. Lets us
  exercise L2 / L3 / L4 against realistic workloads without
  needing the JVM library in hand. Not on the critical
  path for "just works," but reduces the JAR's blocking
  effect on downstream layers.

Once the four critical-path items above land, the experience
is: `cd my-spring-app/ && pgman` → tap panel populates in
seconds, schema browser fully functional, EXPLAIN one
keypress away, and L4's evidence-packet handoff is the
right shape for fixing the issues the operator sees.

#### L1 — receive + render *(landed; see Done → JDBC tap)*
The pgman-side receive + render layer is shipped. Open
work that still belongs to L1:
- **UDP listener.** *(landed; see Done → JDBC tap.)* Opt-in
  via `--tap-udp <addr>`; one `TapEvent` JSON per datagram,
  no framing. For cases where the JVM side must never block
  on telemetry and lossy delivery is acceptable.
- **OTLP-over-HTTP peer ingest.** *(landed; see Done →
  JDBC tap.)* Reaches any OTel-equipped JVM via
  `--tap-otlp :4318` without an additional JAR install.
  Trade-off retained: OTLP spans don't carry rich
  `pool`/`caller`/`txn` fields by default, so the
  pgman-tap JAR still produces the best signal — but for
  shops already on OTel this is one config line away from
  live monitoring.
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
- **Hot-template grouping.** *(landed; see Done → JDBC tap.)*
  Aggregates the in-memory ring by fingerprint and shows
  count, error count, p50/p95/p99 latency, distinct callers,
  last caller. Sort cycles total-time / call-count / p95-
  latency via `s`. `v` toggles between list and hotspots
  views.
- **N+1 live detection.** *(landed; see Done → JDBC tap.)*
  Sliding-window scan over the ring; fires when 5+
  `(txn-or-conn, fingerprint)` events land within 200ms.
  Findings carry the offending caller frame as the
  actionable pointer. The chrome `N+1 ×N` badge surfaces
  the live count without opening the panel.
- **Transaction view.** *(landed; see Done → JDBC tap.)*
  Sixth TapMonitor view groups events by synthetic `txn`
  id (fallback `conn` for autocommit), shows open vs
  committed vs rolled-back state, surfaces long-held
  transactions and the classic "47 SELECTs + 1 COMMIT"
  N+1 shape at the txn level.
- **Per-caller rollup.** *(landed; see Done → JDBC tap.)*
  Fourth TapMonitor view: groups by innermost caller frame,
  surfaces "which app code path owns the DB time?" with the
  same sort modes as Hotspots. Events without a caller frame
  land in the `<unknown>` bucket so the rollup stays
  total-conserving.
- **Pool-saturation gauge.** Count distinct `conn` IDs per
  pool over time vs the configured HikariCP max (carried in
  the heartbeat event, or configured via
  `pgman.tap.pool-max`). Flag thrash.
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
- **Baseline diff.** *(landed; see Done → JDBC tap.)*
  `Shift-B` from any tap view captures the current hotspots
  as a baseline; fifth TapMonitor view renders the diff
  (new / regressed ≥2× p95 / disappeared / unchanged).
  Killer "what changed since I pressed B?" view.

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
- **Report export.** *(landed; see Done → JDBC tap.)*
  `\report` / `\report <path>` dumps current advisor +
  tap insights as Markdown or HTML (format inferred from
  the path extension). Default path lives under the cache
  dir with a wall-clock-stamped filename.

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
- **psql backslash command routing — extras.** `\d` /
  `\dt` / `\dn` / `\?` / `\q` / `\timing` shipped (see Done →
  Editor). Remaining: `\df` (functions), `\l` (databases — needs
  a list-databases catalog query), `\x` (psql-style row view —
  we have RowDetail / CellDetail already, but `\x` could toggle
  some kind of expanded-output default).
  `public.users`; `\dt` → open browser at schemas-with-tables;
  `\timing` → toggle a query-duration footer. Familiarity bridge
  for psql migrants — the muscle memory is identical.

### Completion — next round
- **JOIN ON FK suggestions** — in `JOIN orders o ON `, look at
  `pg_constraint` FK rows linking in-scope tables and offer
  `users.id = orders.user_id`-style predicates as multi-token
  candidates. Catalog fetch needs a new query for foreign-key edges.
  Higher-value, bigger scope than the other completion items here.

### Reuse from ebman (`/Users/tom/git/ebman/src/`)
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

## Done

Historical record. Newest at the top within each section.

### JDBC tap

- **L2 — pool column in Transactions view + report.**
  `TxnStats` gains `pool: Option<String>` (populated from
  the first event in the bucket that carries one). Both
  the live `Mode::TapMonitor` Transactions render and the
  report's Transactions section (Markdown + HTML) add a
  Pool column. Surfaces "did this write hit the replica
  pool by mistake?" as long as pools have meaningful
  names. Partial credit toward read-replica awareness;
  full classification still waits on the JAR to ship
  `pool_role`. 3 new tests (group_by_txn carries pool
  from first-with-pool event, distinct pools stay in
  distinct buckets, report renders Pool column +
  populated value in both Markdown and HTML).

- **Report polish: HTML parity + summary block.** Pulled
  the HTML renderer up to feature parity with Markdown by
  adding the missing Callers / Transactions / Baseline-
  diff sections. New pure `SummaryStats` + `summary_stats`
  helper shared by both renderers — total events, unique
  fingerprints, N+1 + lint counts, total + open
  transactions, baseline label ("none captured" / "no
  changes vs baseline" / "N changed fingerprint(s)").
  Summary block renders at the top of both formats so a
  reader gets the gist before scrolling. 5 new tests
  (event-count + baseline-label derivations, open-txn
  count, markdown summary block placement before
  divider, HTML has all six section headings, HTML
  summary inline count).

- **`--tap-record <path>` capture sink.** Pair to
  `--tap-replay`: every event flowing through the adapter
  task is teed to a JSONL append-only file. New
  `Serialize` derives on `TapEvent` / `TapKind` /
  `TxnOutcome`; new `record_line(event)` pure helper
  ensures `received_at_unix_micros` is dropped on
  serialize (it's `#[serde(skip)]`), so the capture is
  clock-skew-safe across hosts. The replay path
  re-stamps on receive — captured + replayed events are
  indistinguishable from live ones downstream. Adapter
  task opens the file once in append mode, writes each
  event on its own line, flushes per-event so a Ctrl-C
  doesn't lose the tail. Write failures log + drop the
  record (live pipeline is the priority). Startup warning
  when `--tap-record` is set without any source transport
  (would otherwise silently produce nothing). Adapter is
  spawned only when at least one source is configured;
  the record sink rides on it. 3 new tests
  (round-trip-via-replay, received_at-not-serialized,
  heartbeat-and-txn-boundary-kinds round trip).

- **L1 — UDP listener (opt-in transport).** Companion
  to the TCP listener for cases where the JVM side must
  never block on telemetry (production critical paths)
  and lossy delivery is fine. New `run_udp_listener(addr,
  tx)` in `src/tap.rs` — `tokio::net::UdpSocket::bind`,
  recv loop, decode each datagram via the existing
  `tap::parse`, stamp `received_at_unix_micros`, forward
  through `tx`. `TAP_UDP_MAX_DATAGRAM = 64 KiB` so a
  hostile or misbehaving sender can't push us into
  oversize allocations. Parse failures logged via
  `tracing::warn` and dropped — one bad datagram never
  takes out the listener. CLI: new `--tap-udp <addr>`
  flag (same address parser as `--tap-listen`); shares
  the same `tap::TapEvent → AppMsg::TapEvent` adapter
  task as the TCP listener so the App side is transport-
  agnostic. 3 new tests (decodes-one-event end-to-end via
  real loopback sockets, drops-malformed-and-keeps-
  serving, serves-multiple-in-succession).

- **L6 — report export.** New `src/report.rs` module:
  pure `render_markdown` / `render_html` over a
  `ReportSnapshot` (title / generated_at / connection /
  lint_findings / hotspots / callers / transactions /
  nplus1 / baseline_diff). `format_for_path(path)` picks
  HTML for `.html`/`.htm`, Markdown otherwise. Every
  section always appears even when empty so a diff of two
  reports surfaces additions cleanly. Markdown escapes
  pipes / backticks / newlines; HTML escapes `<>&"`.
  Backslash dispatcher in app.rs adds `BackslashCmd::
  Report(Option<String>)` + `dispatch_report` that
  snapshots state via `App::report_snapshot()`, renders,
  and writes via `tui_common::util::write_atomic`. Default
  path is `cache_dir/report-<unix-secs>.md`. Standard
  `\report` / `\report <path>` shape; status-line confirms
  the write. New `format_unix_secs_utc(secs)` pure helper
  with civil-from-days conversion (Howard Hinnant's
  algorithm) — no chrono dep. Cheatsheet entry added; help
  snapshot regenerated. 13 new tests (10 pure
  `render_markdown` / `render_html` / format detection /
  HTML escaping / Markdown escaping / empty-section
  placeholders / hotspots + lint tables, 3 integration:
  `\report` writes markdown, `\report report.html` writes
  HTML, format_unix_secs_utc pins epoch / leap-year
  anchors).

- **L2 — transaction view.** Sixth TapMonitor view groups
  events by synthetic `txn` id (fallback `conn` for
  autocommit traffic). New `TxnStats` struct in
  `src/tap.rs` (`txn`, `conn`, `app`, `statement_count`,
  `error_count`, `distinct_fingerprints`,
  `last_fingerprint`, `first_ts_unix_micros`,
  `last_ts_unix_micros`, `span_micros`,
  `total_query_micros`, `outcome: Option<TxnOutcome>`).
  `group_by_txn(events)` walks the ring once, bucketing
  query events by key (prefers `txn`, falls back to
  `conn` for autocommit) and closing each bucket with the
  matching `TxnBoundary` event. Events with neither txn
  nor conn are dropped; orphan boundary events with no
  preceding queries are dropped. Sort: open transactions
  first (span desc — longest-held are most diagnostic),
  then closed (statement_count desc). Pgman side:
  `TapView::Transactions` is the 4th stop in the new
  6-view cycle (List → Hotspots → Callers → Transactions
  → NplusOne → Baseline → List), `current_txns()` on App,
  `tap_txns_cursor`. UI: `draw_tap_monitor_txns` renders
  state / stmts / distinct fingerprints / span / db-time
  / txn-or-conn · last sql. Open in health_yellow,
  rollbacks in red, commits default. 13 new tests
  (12 pure group_by_txn covering per-txn bucketing,
  boundary closes, commit-vs-rollback, open-before-closed
  sort, span-desc, statement_count-desc, autocommit-via-
  conn fallback, drop-ungroupable, drop-orphan-boundary,
  heartbeat-skip, distinct fingerprints, empty input;
  1 view+key integration).

- **L2 — baseline diff view.** Captures the current
  hotspots snapshot on `Shift-B`, then renders the diff
  vs the live ring as a fifth TapMonitor view. New
  `HotspotDiff` + `DiffKind` (New / Regressed /
  Disappeared / Unchanged) in `src/tap.rs`; pure
  `diff_hotspots(baseline, current, include_unchanged)`
  iterates the union of fingerprints, classifies each per
  the regression threshold (`BASELINE_REGRESSION_FACTOR =
  2×` current vs baseline p95), sorts regressed-first then
  new then disappeared, ties broken on fingerprint for
  determinism. Zero-baseline-p95 doesn't false-positive as
  a regression.
  App side: `TapBaseline { captured_at, event_count,
  hotspots }` struct, `tap_baseline: Option<TapBaseline>`
  on App, `tap_baseline_cursor` for navigation,
  `capture_tap_baseline()` (Shift-B handler, universal
  across all five views), `current_baseline_diff()` method.
  `TapView::Baseline` is the fifth stop in the `v` cycle.
  `c` clears the ring but preserves the captured snapshot
  (operator might want to re-fill the ring against the
  same baseline post-deploy); recapture is the second
  `Shift-B`.
  UI: `draw_tap_monitor_baseline` shows a two-line header
  with capture age (`baseline_age_label` formats as
  Xs/Xm/Xh ago) + capture stats + current ring state,
  then a table of changed fingerprints with `change`
  label, `Δcalls` signed delta, `current calls`, `Δp95`
  as a factor (`2.5×`), `current p95`, fingerprint.
  Regressions in `health_red`, new in `health_green`,
  disappeared in `health_yellow`. Empty-baseline state
  prompts "Press Shift-B from any tap view to freeze the
  current hotspots." Empty-diff state ("nothing changed")
  in `health_green` as a positive signal. 14 new tests
  (9 pure `diff_hotspots` covering new / regressed-≥2× /
  small-regression-no-flag / disappeared / sort ordering /
  include-unchanged / empty inputs / fingerprint tiebreak
  / zero-baseline-no-divbyzero, 5 view+key integration
  covering Shift-B capture from any view, diff after
  capture flags new fingerprint, `c` preserves baseline,
  recapture overwrites, 5-view cycle including baseline,
  + 2 render-path tests).

- **Auto-enable `--tap-listen` in Java projects + setup
  hint empty state.** Two small UX wins on the "open
  pgman in a Java project and it just works" path:
  - main.rs threads a `java_project_detected` flag from
    the existing `creds::spring::detect_java_project`
    call; when set + no `--tap-listen` explicit, the
    `--tap-listen` value defaults to `":7432"`. Startup
    log says "Java project detected — auto-enabling
    --tap-listen :7432 (pass --tap-listen explicitly to
    override)" so the auto-bind is never invisible. OTLP
    stays opt-in because its standard port (4318)
    collides with the OTel collector and surprise-binding
    would steal traffic.
  - `tap_setup_hint_lines(theme)` in ui.rs renders the
    no-JAR empty state with two copy-pastable routes:
    Route 1 (OpenTelemetry — flag + env vars; works
    today), Route 2 (pgman-tap — `build.gradle` +
    `application.yml` snippets, honestly flagged "JAR is
    in development — Route 1 works today"). The
    pgman-tap-connected case keeps the original
    one-liner. 2 new tests (pure hint composition + a
    render-path integration check confirming both routes
    appear).

- **OTLP-over-HTTP peer ingest.** Fast path to a populated
  tap panel for any JVM already running the OpenTelemetry
  Java agent — no pgman-tap JAR needed. New listener +
  pure parser in `src/tap.rs`:
  - `parse_otlp_json(body)` walks an
    `ExportTraceServiceRequest`, maps each span whose
    `db.system=postgresql` + `db.statement` set onto a
    `TapEvent` (sql, duration computed from start/end
    UnixNano, status.code=2 → error chain, service.name
    from resource attributes → `app`). Spans without
    `db.statement` (connection open/close) or with a
    different `db.system` are silently skipped; returns
    `(events, skipped_count)` so the listener can log a
    one-line summary. `params_redacted: true` on every
    OTLP-sourced event so the operator knows values were
    stripped by the agent (typical OTel default for PII).
  - `run_otlp_listener(addr, tx)` async task with a
    minimal HTTP/1.1 server: accepts only
    `POST /v1/traces` with `application/json`. Returns
    405 for non-POST, 404 for other paths, 415 with the
    `OTEL_EXPORTER_OTLP_PROTOCOL=http/json` hint when the
    operator picks the wrong protobuf default, 400 with a
    useful message for malformed bodies, 200 + `{}` on
    success (the empty `ExportTraceServiceResponse` that
    OTel collectors expect).
  - Header parser is case-insensitive (RFC), caps the
    header section at 16 KiB, body cap at 16 MiB. Bigger
    bodies get rejected before we allocate. No keep-alive,
    no chunked encoding — OTel exporters always use
    Content-Length, so v1 stays minimal.
  - CLI: new `--tap-otlp [host:port|:port|port]` flag that
    spawns the listener before the TUI. The existing
    `--tap-listen` (TCP for the JAR) and the new
    `--tap-otlp` share one adapter task that translates
    `tap::TapEvent → AppMsg::TapEvent` so the tap module
    stays App-agnostic and the App side doesn't need to
    care which transport produced an event.
  - 19 new tests (12 pure parser covering happy path,
    non-Postgres filter, missing-statement skip, numeric
    UnixNano, error status with + without message, multi-
    span resource, empty/missing top-level, malformed
    body / non-utf8, no service.name; 7 HTTP server
    covering case-insensitive headers, oversize-body
    rejection, truncated-headers handling, end-to-end
    routing, 405/404/415/400 error paths, trailing-slash
    normalization, real-socket round trip).
  - Operator setup: set
    `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:4318`
    + `OTEL_EXPORTER_OTLP_PROTOCOL=http/json` on the JVM,
    start pgman with `--tap-otlp :4318` — live queries in
    the panel without any pgman-tap JAR.

- **L2 — per-caller rollup.** Fourth TapMonitor view
  groups by innermost caller frame (`caller[0]`). New
  `CallerStats` struct in `src/tap.rs`
  (`caller`, `count`, `error_count`, `total/p50/p95/p99
  micros`, `distinct_fingerprints`, `last_fingerprint`,
  `last_app`). `group_by_caller(events, sort)` buckets and
  computes per-bucket nearest-rank percentiles, routing
  events with no caller frame to `UNKNOWN_CALLER` so the
  rollup is total-conserving. `sort_callers` shares
  `HotspotSort` with the Hotspots view (TotalTime /
  CallCount / P95Latency). Pgman side: `TapView::Callers`
  is the third view in the L1/L2 cycle (`v` now rotates
  `List → Hotspots → Callers → NplusOne → List`).
  `current_callers()` on App; `tap_callers_cursor` for
  navigation; `s` cycles the sort with a status flash. UI:
  `draw_tap_monitor_callers` renders calls / err / p50 /
  p95 / distinct-fingerprints / caller · last-fingerprint;
  errored rows in health_red, focused row highlighted.
  Empty-state message references the `<unknown>` bucket so
  operators know events without caller capture still
  appear. 9 new tests (8 pure grouping/percentile/sort
  covering caller bucketing, unknown fallback, distinct
  fingerprints, non-query filtering, error count + p50/p95,
  mean, total-conservation invariant, sort-in-place; 2
  view-cycle + key navigation including sort cycle + clear
  reset).

- **L2 — live N+1 detection.** Bursts of
  `(txn-or-conn, fingerprint)` matching the offline
  classifier's signature (5+ events within 200ms) surface
  in a third TapMonitor view. New `NplusOneFinding` struct
  + `detect_nplus1(events, window_micros, min_repeats)`
  pure function in `src/tap.rs`. Algorithm: bucket events
  by `(group_key, fingerprint)` where `group_key` prefers
  `txn`, falls back to `conn` so autocommit traffic still
  groups, then walks each bucket sorted by `ts_unix_micros`
  with a two-pointer sliding window to find the longest
  run ≥ `min_repeats` inside the time window. Heartbeat /
  TxnBoundary events skipped. Default window 200ms
  (`NPLUS1_WINDOW_MICROS`), default threshold 5
  (`NPLUS1_MIN_REPEATS`) — matches the offline classifier's
  operating point. Pgman side: `TapView::NplusOne` is the
  third view in the L1/L2 cycle (`v` now rotates
  `List → Hotspots → NplusOne → List`). `current_nplus1()`
  on App computes findings on demand from the ring;
  `tap_nplus1_cursor` navigates the rendered list. UI:
  `draw_tap_monitor_nplus1` renders calls / span / group
  (txn or conn) / caller · fingerprint per row, in
  health-yellow so findings stand out from the
  recency/hotspots views. 11 new tests (10 pure
  detect_nplus1 covering threshold edges, window
  boundaries, separate-txn / separate-fingerprint isolation,
  autocommit-via-conn fallback, longest-run preservation,
  non-query filtering, sort ordering, empty input; 1 view-
  cycle integration; 1 cursor + clear integration).

- **L2 — hot-template grouping.** First insight on top of
  the L1 ring. New `Hotspot` struct in `src/tap.rs`
  (`fingerprint`, `example_sql`, `count`, `error_count`,
  `total_micros`, `p50/p95/p99_micros`, `distinct_callers`,
  `last_caller`, `last_app`). `group_hotspots(events, sort)`
  buckets by SQL fingerprint via the existing
  `query::nplus1::fingerprint` (literals collapsed to `?`,
  case-folded, whitespace normalised), then computes
  nearest-rank percentiles per bucket. `HotspotSort`
  cycles `TotalTime → CallCount → P95Latency → TotalTime`;
  `sort_hotspots` resorts in place without re-aggregating.
  Heartbeat / TxnBoundary events skipped (they don't carry
  SQL). Pgman side: `TapView` enum (`List` / `Hotspots`)
  on App, `tap_sort` + `tap_hotspots_cursor`. `v` toggles
  view (mnemonic; vim's g/G stays as top/bottom within a
  view); `s` cycles sort in hotspots view. `c` clears the
  ring from either view and resets both cursors. UI:
  `draw_tap_monitor` dispatches to `draw_tap_monitor_list`
  (existing L1 view) or `draw_tap_monitor_hotspots` (new):
  per-row calls / err / p50 / p95 / p99 / fingerprint +
  last caller. Title bar shows current view + sort label.
  21 new tests (14 pure grouping/percentile/sort, 5
  view-toggle + key bindings, 2 render-path integration).

- **L1 — receive + render.** Pgman-side ingestion of the
  JDBC tap event stream landed end-to-end. New
  `src/tap.rs` module:
  - `TapEvent` schema (v=1) with kind discriminator
    (`query` / `heartbeat` / `txn_boundary`), shared
    context (`app`, `pool`, `conn`, `txn`),
    Query-specific fields (`sql`, `params`,
    `params_redacted`, `duration_micros`, `rows`,
    `error: Vec<String>` cause chain,
    `caller: Vec<String>` short stack), Heartbeat field
    (`dropped_events_total`), and TxnBoundary field
    (`txn_outcome: Commit | Rollback`). `received_at_unix_micros`
    is `#[serde(skip)]` and stamped by the listener so a
    skewed JVM clock is recoverable. Unknown fields
    silently ignored (forward-compat).
  - `parse(bytes)` enforces version + per-kind required
    fields; never panics on malformed input.
  - TCP length-prefixed listener (`run_tcp_listener`,
    `read_frame` helper) accepts the JAR's
    `writeInt(len); write(json)` framing. Per-connection
    parse failures are logged via `tracing::warn!` and
    the connection continues; a clean EOF at a frame
    boundary closes cleanly.
  - App side: `Mode::TapMonitor` + ring buffer
    (`tap_events: VecDeque<TapEvent>`, `TAP_CAP = 2000`,
    cursor-following eviction), `AppMsg::TapEvent`
    (NOT generation-gated — the listener is independent
    of the DB connection so events survive reconnects),
    `TapHealth` for liveness + dropped-events tracking
    (heartbeats update health without polluting the ring).
  - UI: `draw_tap_monitor` panel (time · duration · app ·
    SQL preview), F4 universal keybinding from any mode,
    footer hint, cheatsheet entry, status-line summary
    that distinguishes "JAR connected, no traffic"
    (heartbeats arrived, ring empty) from "no tap yet."
    `TAP` chrome badge lights once any event lands.
    `format_duration(micros)` pure helper renders µs /
    ms / s.
  - CLI: `--tap-listen [host:port|:port|port]`. Listener
    + adapter spawned before the TUI; adapter translates
    `tap::TapEvent` into `AppMsg::TapEvent` so the tap
    module stays App-agnostic. `parse_tap_addr` accepts
    bare port / `:port` for ergonomics and rejects
    garbage with a useful message.
  - Tests: 31 tap-module (schema parsing across kinds,
    forward-compat ignored fields, framing edge cases,
    TCP round-trip end-to-end), 8 app-handler (ring +
    cap + eviction-cursor invariant, kind routing,
    F4 universal, status summary, key navigation +
    clear + close), 5 CLI (`parse_tap_addr` shapes),
    1 ui (`format_duration` units).

### Schema browser

- **Table sizes in the detail pane.** Third-pass fetch
  (alongside the existing `SCHEMA_SQL` + `CONSTRAINTS_SQL`)
  using `pg_relation_size` + `pg_total_relation_size`. New
  `TableSize { table_bytes, total_bytes }` + cache field
  `table_sizes`. Detail pane renders `size: total 4.20 GiB ·
  heap 1.82 GiB` above the columns list. New
  `format_bytes(u64)` pure helper uses IEC units (KiB / MiB /
  GiB / TiB). 2 new tests.
- **Column type + NOT NULL info.** `SCHEMA_SQL` extended to pull
  `format_type(atttypid, atttypmod)` and `attnotnull` alongside
  the column name. New `ColumnMeta { name, type_name, not_null }`
  + `columns_meta_by_table: HashMap<(schema, table),
  Vec<ColumnMeta>>` on `SchemaCache`, populated in the same
  fetch pass. Schema browser Column rows now render `· id :
  integer NOT NULL`; the detail pane shows `type:` + `nullable:`
  lines. Falls back to the bare-name render when the cache lacks
  metadata. The existing `columns_by_table` is kept untouched so
  identifier completion stays on its narrow data path. 2 new
  tests.

### Schema wizard

- **LINT104 + LINT105 — bloat & missing-comment checks.** Two
  more live checks slotted into `fetch_live`:
  - **LINT104** (MED) tables with > 20% dead-tuple ratio (and
    at least 1000 live rows so dev databases don't drown the
    panel in noise). Detail shows the actual ratio so operators
    triage by severity. Suggestion: `VACUUM (VERBOSE, ANALYZE)
    <schema>.<table>;`.
  - **LINT105** (LOW) tables with no `obj_description` —
    operators discovering the schema have no in-band docs.
    Template suggestion: `COMMENT ON TABLE <schema>.<table>
    IS '…';`.
  4 new tests (2 builders + 2 SQL smoke checks).
- **LINT102 / LINT103 / LINT106 — three more live checks.**
  Building on the LINT101 plumbing, the live-query pass now also
  surfaces:
  - **LINT102** (MED) indexes with `idx_scan = 0` in
    `pg_stat_user_indexes`, excluding indexes backing UNIQUE / PK
    constraints. Detail carries a "confirm stats are warm before
    dropping" caveat; SQL suggestion is `DROP INDEX …;`.
  - **LINT103** (MED) tables with two or more indexes sharing
    the exact same `indkey` tuple. Strict equality (an index on
    `(a)` and one on `(a, b)` are NOT flagged — one might be a
    legitimate smaller lookup). No SQL suggestion: picking which
    of the duplicates to drop is judgment, not mechanical.
  - **LINT106** (HIGH) schemas mixing `timestamp` (no tz) and
    `timestamptz` columns. Almost always a forgotten `WITH TIME
    ZONE` in a migration that silently corrupts stored values
    across sessions in different timezones.
  `fetch_live` was restructured to run each check
  *independently* — one permission-denied (e.g. read on
  `pg_stat_user_indexes`) no longer kills the others. Partial
  failures log via `tracing::warn` and drop from the return; only
  total failure (every check errored) surfaces as Err. 4 new
  builder tests + 1 SQL-constants smoke check.
- **LINT101 — FK without leading-column index.** First live-
  query check on top of the wizard's pure-cache pass. New
  `query::lint::FK_WITHOUT_INDEX_SQL` joins `pg_constraint` /
  `pg_class` / `pg_namespace` and filters by `NOT EXISTS`-ing a
  `pg_index` row whose leading column matches `conkey[1]`. Async
  fetcher `lint::fetch_live(client)` is wired through a new
  `AppMsg::LiveLintLoaded` variant; `start_schema_lint` spawns
  it alongside setting the pure findings, and the handler merges
  the result into `schema_lint_findings`, re-sorts by severity,
  and clamps the cursor. Drop-silently if the operator already
  left the panel; surface a status hint (not an error) if the
  catalog query failed (e.g. permission denied). Each finding
  carries a ready-to-yank `CREATE INDEX ON <schema>.<table>
  (col_list);` suggestion. 5 new tests (pure builder ×2,
  handler merge / late-arrival / failure).
- **`W` opens a schema-lint panel.** New `query::lint` pure
  module + `Mode::SchemaLint`. Runs four checks against the
  cached catalog (sub-millisecond, no live queries) and lists
  findings sorted high → low severity:
  - **LINT001** (HIGH) tables with no PRIMARY KEY / UNIQUE
    constraint (carries a ready-to-yank `ALTER TABLE … ADD
    PRIMARY KEY (…);` suggestion).
  - **LINT002** (MED) mixed-case identifiers — schema, table,
    or column names with any uppercase letter, which Postgres
    will then require everyone to `"…"`-quote forever.
  - **LINT003** (MED) tables or columns named after a SQL
    reserved keyword (`user`, `order`, `select`, …).
  - **LINT004** (LOW) a single schema mixes snake_case and
    mixed-case naming conventions.
  Findings carry `severity`, stable code, fully-qualified
  `object`, free-text `detail`, and an optional SQL
  `suggestion` (yankable via `y`). Split panel: top half lists
  findings (severity-coloured pills); bottom half shows the
  focused finding's detail + suggestion. Keys: j/k navigate,
  g/G top/bottom, PageUp/Down by 10, `y` yank suggestion, `r`
  re-run, q/esc close. Footer + cheatsheet + first-entry tip
  cover it. 10 new tests (7 pure-logic, 3 handler) + 1 insta
  snapshot. Live-query checks (FK without index, unused
  indexes, bloated tables, missing comments) deferred as
  follow-ups — they need `pg_index` / `pg_stat_user_indexes` /
  `pg_stat_user_tables` fetches the cache doesn't carry yet.

### Schema browser navigation

- **`[` / `]` jump by schema, `+` / `−` expand-all / collapse-
  all, PageUp / PageDown.** A fully-expanded tree was a j/k slog
  past every column; `]` now jumps to the next Schema row (and `[`
  to the previous), skipping over table internals in one stroke.
  `+` expands every schema + table in the cache; `−` collapses
  everything and re-clamps the cursor to the new tail. PageUp /
  PageDown move 10 rows at a time. New pure `Direction` enum and
  `next_schema_row_idx(rows, from, dir)` helper for the jump,
  6 new tests, footer hint + first-entry tip + cheat-sheet
  refreshed.

### Performance / DBA

- **Streaming results — v1.** Both `conn::run_statement` and
  `conn::run_query` now drive their fetches through a shared
  `stream_rows` helper that pulls from a `query_raw` `RowStream`
  and stops at `grid::MAX_ROWS` instead of materializing the full
  result. The helper peeks one extra row to detect overflow and
  sets `Grid.truncated`. UI surfaces this in two places: the
  result-table title gets a `· capped at 1000` suffix, and the
  post-query status line carries the same hint so the operator
  sees it without needing the grid focused. 3 new tests (status
  flagged when truncated, status unchanged when not, render shows
  the cap in the title). v2 (page-beyond-cap) is now in the open
  backlog under Performance.
- **LISTEN / NOTIFY arrivals panel (F3).** Capture
  `tokio_postgres::AsyncMessage::Notification` in the existing
  connection-poll loop (previously discarded) and forward it
  through a new `notification_tx` channel parallel to the
  notices channel. New `NotificationMsg { channel, pid,
  payload }`, `AppMsg::Notification`, App ring buffer capped at
  `NOTIFICATION_CAP = 200`, `Mode::Notifications` panel
  rendering channel/pid/payload columns. `c` clears, `y` yanks
  the focused payload, `F3` opens from anywhere (same universal
  pattern as F1/F2). Operator subscribes via `LISTEN <chan>` in
  the editor; the server-side subscribe goes through unchanged
  — we only added the surface for the arrivals. Each connect
  gets its own notification_rx so stale events from a prior
  session can't leak. 3 new tests (ring cap, F3-from-any-mode,
  clear).
- **Auto-refresh ticker (`R`) for SlowQueries / Sessions panels.**
  Toggles a `5 s` polling cycle that re-loads `pg_stat_statements`
  / `pg_stat_activity` without keypresses. Hot path: extends
  `wants_animation` (so the frame clock keeps ticking) and the
  main loop's tick fires `tick_auto_refresh` alongside
  `tick_watch`. Gated on `!query_running` so the refresh never
  stacks on top of an in-flight query. Status flashes
  `auto-refresh on (5s)` / `off`. 3 new tests (toggle, gated-by-
  disabled, gated-by-query-running).
- **`K` terminate session in the active-sessions panel.** New
  `Mode::ConfirmTerminate` + `App::pending_terminate: Option<i32>`
  carry the target pid through a y/n confirmation modal. On
  confirm, `SELECT pg_terminate_backend($1)` fires async; on
  success, the panel auto-refreshes via the existing
  `AppMsg::SessionsLoaded` round-trip; on failure the standard
  error pipeline surfaces it. Cancel (n/esc) returns to the
  sessions list with a "terminate cancelled" status. Footer
  hint + first-entry tip + cheatsheet refreshed. 4 new tests
  (open + cancel paths, empty-list guard, pid carried through).

### Result grid

- **Vim-style bookmarks (`m<a-z>` / `'<a-z>`).** Two pending-key
  flags on App (`pending_mark_set`, `pending_mark_jump`) consume
  the next keystroke in Normal as the bookmark letter. Set
  captures the current `(visible row idx, col idx)` into a
  `HashMap<char, GridBookmark>`. Jump moves both cursors back,
  clamping to the current column count and reporting "row not
  visible in current filter" when the bookmark's row has been
  filtered out. Session-local (not persisted). Cheatsheet
  refreshed. 4 new tests (set, jump, jump-to-unset, m-followed-
  by-non-letter cancels silently).

- **Find within grid (`f`).** New `Mode::GridFind`, distinct
  from `/` filter — find HIGHLIGHTS / jumps to matching cells
  instead of hiding non-matching rows. Pure helper
  `compute_grid_find_matches(grid, visible_rows, needle)`
  returns row-major `(visible_row_idx, col_idx)` pairs;
  case-insensitive substring; honours the current filter
  (matches drawn from the visible-rows subset). `n` / `N` cycle
  through matches with wraparound; Enter accepts, Esc clears.
  Footer hint + terminal cursor positioning added for the new
  typing mode. 5 new tests (3 pure helper + 2 handler flows).

### Errors & diagnostics

- **Rich error overlay (F2).** `conn::QueryErr` extended with a
  new `detail: Option<QueryErrDetail>` carrying severity / code
  / detail / hint / where / schema / table / column / data_type
  / constraint extracted from `tokio_postgres::DbError`. The
  `AppMsg::QueryFailed` variant carries the detail through; the
  handler stashes it on `App::last_error_detail`. F2 from any
  non-detail mode opens `Mode::ErrorDetail` — a labelled overlay
  rendering each field as a `label: value` line (with the message
  / detail / hint wrapped to the popup width). Severity pill at
  the top of the overlay is red. Cleared on the next successful
  query (`AppMsg::QueryOk` zeros it). The footer's `⚠`-prefixed
  error line gets a `· F2 detail` suffix when detail is
  available so the affordance is discoverable. 3 tests (open
  on F2, no-op when no error, OK clears detail).

### Result grid

- **FK navigation (`F`).** With a single-table SELECT result
  focused on a column that's an FK, `F` opens a new tab with
  `SELECT * FROM <parent>.<table> WHERE <parent_col> = <value>
  LIMIT 100;` pre-loaded in the editor (F5 to run). New
  `cache.fk_edges: Vec<FkEdge>` populated by a new fetch pass
  (`FK_EDGES_SQL`) — uses `WITH ORDINALITY` to zip `conkey`
  with `confkey` so multi-column FKs map column-pair-by-pair.
  New helper `cache.fk_edge_for_child(schema, table, col)`
  does the case-insensitive lookup. Multi-tab makes "go back"
  trivial: close the new tab → originating result is right
  there. 5 new tests (2 cache-level + 3 handler).

### Tabs

- **Multi-tab v1 (shared connection).** New `TabSnapshot`
  carries per-tab editor + result-grid state; App grows
  `tabs: Vec<TabSnapshot>` + `active_tab: usize`. The active
  tab's state lives in App's existing fields — switch
  snapshots out / loads in. **Existing code is untouched** —
  multi-tab is invisible to every read site. Shared across
  tabs: connection, schema cache, history, saved queries,
  notifications, error state, theme, safety profile.
  Universal keys (work in every non-typing mode):
  - `Ctrl-T` new tab (cap `TAB_CAP = 9`)
  - `Ctrl-W` close active (no-op on last; blocked during a
    running query, like switch)
  - `Ctrl-Tab` next · `Ctrl-Shift-Tab` previous
  - `Alt-1`..`Alt-9` jump directly to tab N
  Tab bar renders one line under the header — and only when
  tabs.len() > 1, so the single-tab default UX is byte-
  identical. Tab labels auto-derive from each buffer's first
  non-blank line (truncated to 20 chars). Cheatsheet gets a
  dedicated "tabs" section. 7 new tests (open / cycle / close
  with neighbour-load / close-only-tab guard / cap / alt-jump
  / query-running guard).

### Editor

- **Saved queries v1.** New `saved.rs` pure module
  (`SavedQuery { name, body }`, `SavedQueries::{ upsert, remove,
  get }`, atomic TOML round-trip via `save_to` / `load_from`).
  Stored at `util::data_dir()/saved.toml`. Loaded on startup,
  persisted on quit AND after every save / delete (the on-quit
  write is the safety net, not the primary). New
  `Mode::SavedQueries` panel + `Mode::SaveQueryPrompt` modal:
  - `Ctrl-S` from Editor → name prompt (pre-filled with a
    sanitised default derived from the buffer's first 40 chars
    via `default_query_name`).
  - Enter persists; Esc cancels.
  - `Q` from Normal (or `Ctrl-O` from Editor) opens the list.
  - `Enter` loads the focused entry into the editor; `d` deletes.
  Split-panel render: list of `name · body-preview` on top, full
  body of the focused entry below (wrapped). Footer + cheatsheet
  + Normal-mode hint advertise `Q` / `Ctrl-S` / `Ctrl-O`. 11
  new tests (5 pure-module + 6 handler flows).
- **History entry deletion (`Ctrl-D` in reverse-i-search).**
  When a match is shown, Ctrl-D removes that entry from the
  in-memory `history` ring (the persistence side picks up the
  shorter ring on the next quit, so disk catches up). Re-steps
  the search from the end so the next match (or "no match")
  surfaces. Useful after pasting a query with inline secrets.
  Cheatsheet + footer hint refreshed. 1 new test.
- **Bracket autoclose + line-comment toggle.** Three new pure
  helpers in app.rs:
  - `editor_insert_pair(buffer, cursor, c)` — typing `(` /
    `[` / `{` inserts the matching close and positions the
    cursor between the pair.
  - `editor_maybe_skip_close(buffer, cursor, c)` — typing
    `)` / `]` / `}` over a matching close-char already at the
    cursor just advances past it (so `(` `)` produces `()` with
    the cursor outside).
  - `editor_toggle_line_comment(buffer, cursor)` — `Ctrl-/`
    toggles a `-- ` line comment at the start of the focused
    line. Removes the marker if already present, inserts it
    otherwise. Cursor preserved relative to the line's content.
  Crossterm reports Ctrl-/ as either `Char('/')` or `Char('_')`
  depending on terminal — both accepted. 8 new tests on the pure
  helpers. Cheatsheet section refreshed.
- **Quote autoclose (`'` / `"`).** Two new pure helpers:
  - `editor_maybe_pair_quote(buffer, cursor, c)` — pairs a
    quote and seats the cursor between, gated by a conservative
    neighbour-check (`prev` and `next` must each be EOB,
    whitespace, or punctuation that isn't `_`/alnum/the same
    quote). The gate keeps the feature out of SQL `''` escaping
    inside string literals and out of contractions like `it's`
    in comments — typing a quote there falls through to a
    literal insert.
  - `editor_maybe_skip_quote(buffer, cursor, c)` — same skip-
    over idea as the close-bracket helper but with the same
    prev-char gate as `pair_quote`. Inside a SQL string literal
    (`'don|'`, prev is `n`) skip refuses so the operator can
    build `'don''t'` via a normal `''` escape; at a quote-
    boundary (`''` with cursor between, prev is `'`) skip fires
    and the cursor exits the pair.
  Editor's `KeyCode::Char(c)` branch tries skip-quote first,
  then pair-quote, then literal insert — so typing `'` at EOF
  produces `''` with the cursor between, a second `'` exits
  the pair, typing `'` inside `it` produces `it'` (literal),
  and typing `'` inside `'don|'` produces `'don''` (the SQL
  escape). 12 new tests (6 pair-quote + 3 skip-quote + 3
  end-to-end via `on_key`).
- **psql backslash command routing.** Editor buffers starting
  with `\` are intercepted by `request_run` before the
  safety/spawn path and dispatched as meta-commands:
  - `\d` → open schema browser. `\d <name>` → opens it with the
    schema-browser filter pre-populated to `<name>`, so the
    operator sees the matching schema/table/column with its
    ancestors immediately (existing filter UX surfaces the path).
  - `\dt` / `\dn` → schema browser (default view).
  - `\?` / `\h` → open the help cheatsheet (Editor anchor).
  - `\q` / `\quit` → quit.
  - `\timing [on|off]` → toggle elapsed-ms in the post-run
    status footer. New `query_started: Option<Instant>` field
    captured at `spawn_run` and read in QueryOk; cleared on
    QueryFailed.
  - `\xyz` (unknown) → actionable error in `last_error`, buffer
    not sent to the server.
  Pure parser in new `query::backslash` module (`BackslashCmd`
  enum + `parse_backslash_command`). 7 parser tests + 6 dispatch
  tests + cheatsheet section refreshed.

### Discoverability

- **F1 opens help from any mode.** Previously only `?` from
  Normal worked, so an operator stuck in the editor couldn't
  reach the cheatsheet without esc-ing first. `F1` is now a
  universal handler at the top of `on_key`; closes on F1 / esc
  / `?` / `q` and **restores the mode you came from** (not Normal)
  via a new `help_origin` field. Brand-new `help_anchor_for(Mode)`
  picks the section heading that matches your origin, and
  `draw_help` pre-scrolls the cheatsheet there — open help from
  the schema browser and you land on its keys directly.
- **Cheatsheet brought current.** New `help_body()` builder
  emits sections + an `anchor → line index` map. Covers
  everything shipped this batch and the last several: undo/redo,
  schema-browser `/` filter and `s` / `i` yanks, LogPick cluster
  toggle, slow queries / sessions panels, JSON cell-detail tree,
  persistent history, EXPLAIN tree, the lot.
- **Footer always advertises `F1 help`.** Mode hints get a
  global ` · F1 help` suffix unless the mode already mentions
  help (Help, Confirm, TxDecision, the typing-input modes).
  Operators see the affordance everywhere they look without
  having to know to look.
- **First-entry mode tip.** Each non-trivial mode flashes a
  one-line tip into the status footer on its first open in the
  session (`tip · / filter · enter expand · s yank SELECT · i
  yank INSERT · F1 full keys` for Schema Browser, similar for
  ExplainTree / SlowQueries / Sessions / LogPick / RowDetail /
  CellDetail). Subsequent visits stay quiet via a per-mode
  `mode_seen: HashSet<Mode>` flag. 4 new tests + 7 snapshots
  re-accepted for the new footer / cheatsheet layout.

### Editor

- **Undo / redo.** New `editor_undo` / `editor_redo` rings on App
  (`Vec<UndoEntry>`, capped at `UNDO_CAP = 100`). `on_editor_key`
  now wraps the original handler: snapshots `(buffer, cursor)`
  pre-mutation, calls the inner, pushes the snapshot to the undo
  ring iff the buffer changed. Coalescing: consecutive char-
  inserts within `UNDO_COALESCE_WINDOW = 500ms` merge into one
  undo step (so `qwerty` is one undo, not six). Backspace / Enter
  / paste / paste / Ctrl-U are non-coalescing — each is its own
  step. New mutation invalidates redo (divergent edit = new
  history branch). Pure `should_coalesce_undo(last_kind,
  last_end, new_kind, now, window)` helper for testability. 10
  new tests.

### Connection & chrome

- **Kitty-protocol keyboard disambiguation.** Tui::enter pushes
  `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES` so
  terminals supporting the protocol (kitty, alacritty, wezterm,
  foot, ghostty, recent xterm) reliably deliver `Ctrl-Enter` as
  `KeyCode::Enter + CONTROL` instead of folding it into plain
  Enter. Best-effort: terminals without the protocol silently
  ignore the enable sequence and F5 remains the universal run
  shortcut. Flags popped on `suspend()` (`\e` external editor)
  and re-pushed on `resume()`.

### Shared crate adoption

- **Migrated to `tb-tui-common` 0.1.0.** New crates.io dep
  consumed under the name `tui_common`. Three duplicated modules
  retired from pgman's local source:
  - `src/font_probe.rs` — deleted; `pgman::font_probe` is now a
    re-export of `tui_common::font_probe` so call sites
    (`font_probe::resolve_icons_setting`, `AutoResolved`, the
    detectors) stay untouched.
  - `theme::IconStyle` — re-exports `tui_common::theme::IconStyle`
    instead of defining it locally. The pgman-specific `Theme`
    struct stays (it carries the colour palette).
  - `util::parse_bool` — dropped (no callers in pgman; ebman has
    its own copy via the same crate).
  - `util::write_atomic` — initially kept local with the pid +
    nanos temp-suffix fix from the code-review pass; **upstreamed
    in `tb-tui-common` 0.1.1** and the local copy retired. pgman
    callers go directly through `tui_common::util::write_atomic`.

### Persistence & safety

- **Persistent query history.** Bash-style line-oriented file at
  `util::data_dir()/history.log`. Multi-line SQL escaped to one
  line via `\\` / `\\n` (round-trip tested). Loaded on startup
  via `app::load_history` and persisted on graceful quit
  alongside the editor draft. Cap matches the in-memory ring
  (`HISTORY_CAP = 50`). 5 new tests.
- **Pre-flight cost preview.** New `SafetyProfile::
  cost_preview_threshold_rows` (default 0 = disabled, opt-in
  via safety.toml). When a plain `RunKind::Run` SELECT / WITH /
  TABLE / VALUES query without a `LIMIT` lands, the run path
  first sends `EXPLAIN (FORMAT JSON)`. If the top node's
  `Plan Rows` exceeds the threshold, the existing
  `pending_run` / `Mode::Confirm` machinery opens a prompt
  ("cost preview: estimated 4,200,000 rows (threshold N) —
  proceed?"). Under threshold, the run proceeds with a status
  hint. EXPLAIN failures fall through (the real query will fail
  with the same error). New `AppMsg::CostPreviewLoaded`; pure
  `is_cost_checkable` + `format_row_estimate`; 4 unit tests on
  the pure parts.

### Reconstruction

- **LogPick cluster view (`c` toggle).** New `LogPickView` enum +
  `log_pick_clusters` cache let the picker flip between "all
  queries" (one row per reconstructed query) and "N+1 clusters"
  (one row per fingerprint with its repeat count). `c` toggles
  the view, resets the cursor, and updates the status line. Enter
  in the cluster view loads the cluster's example SQL into the
  editor. Footer hint advertises the new key. 3 new tests cover
  the visible-len, the cursor reset on toggle, and the Enter →
  editor flow in the cluster view.
- **LogPick session summary header.** New pure
  `nplus1::summarize(queries)` + `SessionSummary::one_line()`
  surface a one-line triage view above the picker rows: `N queries
  · M N+1 cluster(s) (K of N repeated)`, plus a second line
  showing the top cluster's leader SQL with `(×count)`. Timing-
  independent (per-query durations are a separate backlog item).
  3 new tests covering empty input, multi-cluster aggregation, and
  singular/plural wording.
- **Hibernate `format_sql=true` multi-line reassembly.** Continuation
  lines (`looks_like_continuation`: leading whitespace + no `[…]`
  bracket, non-empty) are appended to the most-recently-opened
  thread's open SQL. `finalize` trims trailing whitespace and skips
  records that ended up empty. Works under multi-thread interleaving
  because Hibernate prints the formatted output atomically per log
  call — the chunk for one thread is contiguous. 5 new tests (the
  multi-line reassembly, the multi-thread non-interference case, the
  empty-with-no-continuations drop, plus `looks_like_continuation`
  acceptance and rejection coverage).
- **pglog header detection is now string-literal-safe.**
  `split_record` no longer treats a level-shaped token anywhere in
  the line as a record header. A header must follow a `[<digits>] `
  pid bracket that appears near the start of the line (cap 80
  chars) with no preceding `'` (heuristic for "we're inside a SQL
  string literal continuation"). Anchored `LEVEL:` at the very
  start of a line still works (pid-less logs). Reproducer + 4 new
  edge-case tests pin the fix.

### Completion

- **`nextval('|')` literal-context completion.** New pure helper
  `detect_nextval_literal(buf, cursor)` walks `'` toggles to find
  the open string under the cursor, then checks the head for
  `<word-boundary>nextval(<ws>` (case-insensitive). When detected,
  `extract_identifier` synthesizes an `Identifier` so the editor's
  replace range tracks the in-string partial, and `candidates_for`
  short-circuits to `cache.sequences` (filtered by prefix; public
  sequences bare, others schema-qualified). 8 unit tests cover the
  detector edge cases (case-insensitive, word-boundary,
  closed-string, comment-cases, not-nextval-context) plus the
  end-to-end candidate emission.
- **CREATE TABLE column-type completion.** Inside the column-
  definition list, the classifier now alternates between two new
  `ClauseContext` variants: `CreateTableColumns` (column-name
  position — empty candidate list so we don't mis-suggest existing
  column names for fresh ones) and `CreateTableColumnType` (TYPE_NAMES
  filtered by prefix). The flip happens on the first identifier in
  a column-name slot; `,` returns to the column-name slot. Supports
  `CREATE TEMP TABLE` / `CREATE UNLOGGED TABLE` / `CREATE TABLE IF
  NOT EXISTS` via a `pending_create_kind` flag that survives the
  intermediate keywords. 5 unit tests.
- **Unicode-aware identifier walker.** `extract_identifier` now
  walks `char_indices()` (forward + backward) and accepts any
  Unicode alphabetic codepoint plus `_` as an identifier-
  continuation character. `café`, `naïve`, `пользователь` and CJK
  identifiers complete end-to-end. Backward walk crosses `.` to
  resolve `schema.table.col`. Numeric-literal rejection still keys
  off ASCII digits so `1.5` doesn't get mis-parsed.

### Schema browser

- **In-tree filter (`/`).** New `Mode::SchemaBrowserFilter` +
  `schema_browser_filter: Option<String>` on App. `/` opens a
  live incremental filter; typing narrows the tree in place,
  Enter accepts, Esc clears. Pure `filter_schema_browser_rows`
  keeps ancestor rows visible when a deeper descendant matches
  (so a matching column still shows its parent table + schema)
  and forces all schemas/tables open while filtering. 8 new
  tests covering the pure filter and the key flow.
- **One-key SELECT / INSERT template yanks.** With a Table /
  Column / Constraint row focused, `s` copies a
  `SELECT * FROM <schema>.<table> LIMIT 100;` template to the
  clipboard; `i` copies a column-aware `INSERT INTO … (cols…)
  VALUES (NULL, …);` template. Identifiers needing it get
  Postgres-style `"…"` quoting via a new `quote_ident` pure
  helper. Footer hint advertises the new keys. 6 unit tests
  (builders + negative-path handler tests).
- **Table drill-down (columns + constraints).** Enter on a Table
  row in the schema browser toggles a third tree level showing the
  table's columns (catalog order) followed by its unique / primary-
  key constraints (alphabetical). The `expanded` set keys schemas
  by name and tables by `"schema.table"` (helper:
  `schema_browser_table_key`). Table rows now carry `expanded`,
  `column_count`, `constraint_count`; the renderer shows a ▶/▼
  marker on each Table row and the counts in the summary line.
  Indexes are deferred — the cache lacks the table-of-owner
  linkage. Collapsing a schema clamps the cursor so it can't drift
  past the new last row. 4 unit tests + the existing schema-browser
  snapshot re-accepted.

### UX

- **`[RO]` / `[TX]` status badges.** Persistent pills in the
  footer when the connection is read-only (per safety profile)
  or an auto-tx is open. Stable order, both can stack. New
  `footer_badges` pure helper; 3 unit tests; existing render
  snapshots re-accepted with the badge prepended.
- **Editor cursor + footer-input cursor.** `Mode::Editor` and the
  two footer-typing modes (`GridFilter`, `HistorySearch`) now place
  the real terminal cursor at the typed position via
  `Frame::set_cursor_position`. The existing reversed-block in the
  editor stays as a fallback for terminals that blink the OS cursor
  off during long pauses.
- **Esc-as-no-op in Normal and ConnPick** (already implemented; now
  pinned by tests so the behaviour can't drift). Only `q` /
  `Ctrl-C` quit; reflex Esc from inside an overlay can no longer
  abandon the session.

### Result grid

- **Cell-truncation marker.** New `grid::truncate_cell_parts`
  returns the kept text + the `…` suffix separately. The grid
  renderer styles the suffix with `theme.accent` + bold so
  truncated cells visibly signal that RowDetail / CellDetail
  will reveal the rest. Existing `truncate_cell` kept as a
  round-trip-equivalent thin wrapper; 4 new tests pin the
  parts logic.
- **JSON path navigator in CellDetail.** When a cell parses as a
  JSON object or array, CellDetail flips from wrapped-text into a
  collapsible tree: j/k navigate, Enter / Space / h / l
  expand/collapse the focused container, `y` yanks the jq-style
  path (`.foo[0].bar`, `.` for root). Scalars and non-JSON cells
  fall back to the existing renderer. Pure logic in
  `query::json_cell` (parse + flatten with a `collapsed: HashSet`),
  rendered by `ui::render_json_tree`; auto-scrolls the popup to
  keep the cursor row visible. 10 unit tests (parser + flatten +
  open/close + navigation + collapse) and one insta snapshot.
- **Yank focused row as INSERT** (`I`). Builds `INSERT INTO <s>.<t>
  (col, …) VALUES (…)` for the focused row when the source query
  is a single-table SELECT (joins / no-FROM / aggregates surface an
  actionable error). Pure helpers `infer_single_source_table` and
  `format_sql_literal` (NULL for empty, unquoted for
  numerics/booleans, single-quote escape via `''` doubling).
  `last_run_sql` + `grid_source` captured at query dispatch so the
  inference runs at yank time.

### Foundations (M0)

- **Scaffold.** Repo, `Cargo.toml`, lib+bin split, `CLAUDE.md`. Lifted
  `theme.rs` + `util.rs` from ebman. Pure modules implemented with
  tests: `safety` (statement classification + per-DB guards),
  `query::subst` (placeholder substitution), `query::nplus1`
  (fingerprint + clustering), `query::reconstruct` (shared types),
  `conn` (DSN parsing), `creds::spring` (`.properties` parsing +
  Java-project detection), `splash`.
- **TUI event loop** (`app.rs`, `app/msg.rs`, `ui.rs`, `tui.rs`). One
  frame clock, gated on splash / loading.
- **`splash.rs`** — animated elephant, dismissed on keypress or
  connect. 3-second minimum kept across the ConnPick path.
  Trunk-tip animation + occasional blink.
- **`font_probe.rs`** — lifted from ebman; resolves `auto` → IconStyle.
- **`conn.rs`** — real connection via `tokio-postgres`; applies safety
  session settings (`default_transaction_read_only`,
  `statement_timeout`).
- **`grid.rs`** — results grid type + column-width / truncation
  helpers.
- **TLS via `tokio-postgres-rustls`** — connector tries native trust
  roots (`rustls-native-certs`) then falls back to Mozilla's
  `webpki-roots` so RDS / managed Postgres "just works". `sslmode=`
  URL param honoured (`disable` / `prefer` (default) / `require` /
  `verify-*`).
- **Panic hook** restores the terminal (alt-screen + raw-mode off)
  before the default hook prints the backtrace.
- **Scrollable help overlay** (j/k/g/G + PageUp/PageDown,
  "↑/↓ N more" hints).
- **Diagnostic connection-failure view** — target DSN, source /
  origin (`--dsn flag` / `auto-picked IntelliJ data source 'x'`),
  full error chain (walks `Error::source` so "Connection refused"
  actually appears), and an actionable hint for known failure modes
  (refused / timeout / DNS / auth / missing db / TLS-required). Plus
  `r` retry and `p` re-open picker keys.
- **Project config at `.pgman/pgman.toml`** (intended for git):
  named `[[connections]]` feed the startup picker; `[safety]`
  overrides merge with the global `~/.config/pgman/safety.toml`
  per-key. Passwords come from `PGPASSWORD` / per-connection
  `password_env`, never the file.
- **IntelliJ data-source discovery** — pre-TUI scan of
  `.idea/dataSources.xml` + `.idea/dataSources.local.xml` (merged by
  UUID; local file's `<user-name>` and schema-mapping db names fill
  in for the committed file). One postgres source → auto-DSN;
  multiple → `Mode::ConnPick` picker. Per-database disambiguation
  when schema-mapping has multiple.
- **Spring properties / YAML / bootstrap discovery** — scans
  `src/main/resources/{application,bootstrap}*.{properties,yml,yaml}`,
  emits one pick per `<prefix>.url` + `.username` + `.password`
  triple. YAML support via a focused YAML → dot-notation flattener
  (handles nested mappings, comments, quoted scalars; skips lists /
  anchors). Credentials read from file but only redacted DSNs are
  logged.
- **Row-detail modal** (Enter on a grid row): psql `\x`-style
  expanded view, one labelled value per column, values wrapped.
  Field cursor (j/k) with auto-scroll; `y` yanks the focused value
  via `arboard`.
- **Per-cell zoom** from RowDetail (`Enter` on a focused field) opens
  `Mode::CellDetail` — a larger popup showing the single value
  wrapped + scrollable.
- **`:about` overlay** (`A` key) — same content as splash (elephant +
  version + credits).
- **SSH tunnel built-in** — `[[connections]]` entries gain an
  `ssh_tunnel = "user@bastion"` field; the `--dsn` URL accepts
  `?ssh_tunnel=user@bastion[:port]`. A new `tunnel` module shells
  out to the system `ssh` (subprocess, `-N -L 127.0.0.1:LOCAL:
  REMOTE_HOST:REMOTE_PORT bastion -o BatchMode=yes`) and the
  tunnel handle lives in `Booted` so the App keeps the ssh process
  alive for the session. `tokio-postgres`'s `hostaddr` field lets
  us connect to the local end while TLS still verifies against the
  real server name. Provenance: `redacted()` appends `via
  ssh://user@bastion` and a tunnel-failure error surfaces a
  dedicated `connect_hint` ("try `ssh -v` manually — we run with
  BatchMode=yes"). 13 unit tests covering parsing + the param /
  config plumbing.

### Query reconstruction (M1)

- `query/reconstruct.rs` — shared `ReconstructedQuery` type.
- `query/subst.rs` — `?` + `$N` parameter substitution.
- `query/hibernate.rs` — HB5 + HB6 log parsing, thread-grouped.
- `query/pglog.rs` — Postgres / RDS log parsing
  (`statement` / `parse`-`bind`-`execute`).
- `query/nplus1.rs` — fingerprint + clustering.
- `query/jdbc.rs` — pasted SQL + `TYPE:value` parameter lines.

### Editor (M2)

- **SQL editor** — multi-line buffer with cursor, multi-byte safe.
  Up / Down preserve preferred char-column. Home / End act per line.
  Editor pane grows dynamically (3-row min, 12-row cap) and scrolls
  long buffers to follow the cursor (`clamp_editor_scroll`).
- **Run paths** — `Ctrl-R` / `F5` to run, `Ctrl-E` / `F6` EXPLAIN,
  `Ctrl-A` / `F7` EXPLAIN ANALYZE. Persistent `tokio-postgres` client
  held by `App`; subsequent queries reuse the session.
- **Safety routing** — every run goes through `safety::evaluate`:
  `Block` rejects, `Confirm` opens a modal, `Allow` runs. `auto_tx`
  wraps DML in `BEGIN` / `COMMIT`.
- **DML-aware EXPLAIN ANALYZE** — writes wrap in `BEGIN` /
  `ROLLBACK` so the mutation never lands.
- **Multi-statement run** — `safety::split_statements` splits on `;`
  outside strings / comments. Most-restrictive guard wins; routed
  through `conn::run_batch` / `run_batch_in_tx_open`.
- **Commit / rollback prompt for `auto_tx`** —
  `conn::run_in_tx_open` leaves the tx open; `Mode::TxDecision`
  blocks input until `y` commits or `n` / `esc` rolls back.
- **Affected-row count** for non-row-returning statements via
  `conn::run_statement`.
- **Query history** — 50-entry ring buffer; `Ctrl-P` / `Ctrl-N`
  navigate; live draft preserved on `Ctrl-N` past newest.
- **Log import** — `Ctrl-L` / `F8` parses the buffer through
  `hibernate::parse` + `pglog::parse`; `Mode::LogPick` lists
  reconstructed queries; Enter loads `runnable_sql` into the editor.
- **DBUnit fixtures** — `dbunit::parse_flat_xml` reads a
  FlatXmlDataSet; `generate_clean` / `generate_inserts` /
  `generate_apply_script` produce a `TRUNCATE` (or `DELETE FROM`) +
  `INSERT` script in correct FK order. `Ctrl-D` / `F9` reads the
  buffer as a fixture path and replaces it with the apply script.
- **Bracketed paste** — terminal wraps pasted text in escape codes;
  crossterm delivers `Event::Paste(String)` instead of streaming
  each char. CRLF / CR normalised to LF.

### psql parity (table-stakes credibility gaps)

The eight basics every Postgres user expects on day one. Shipped as a
unified pass after the SSH-tunnel work.

- **Cancel running query** (Ctrl-C) — sends a PostgreSQL
  `CancelRequest` via `tokio_postgres::CancelToken`; the original
  `execute` future resolves with the cancellation error and lands as
  the normal `QueryFailed` message. In tunneled sessions the cancel
  TCP inherits the parent `Config` so it rides through the same ssh
  forward. Top-level Ctrl-C still quits in non-Editor modes; in
  Editor it cancels mid-query or no-ops on idle.
- **Error position cursor jump** — `conn::QueryErr` extracts
  `as_db_error().position()` from `tokio_postgres::Error`; the
  `QueryFailed` handler converts the 1-indexed char position to a
  byte offset (multibyte-safe via `char_indices`) and moves the
  editor cursor. EXPLAIN / EXPLAIN ANALYZE wrappers shift the
  position back by the wrapper prefix so the cursor lands inside
  the user's buffer.
- **History search (Ctrl-R)** — new `Mode::HistorySearch` +
  `HistorySearchState`. Reverse-incremental substring match
  (case-insensitive). Buffer mirrors the current match so the
  operator previews before Enter. Ctrl-R again walks older. Esc
  restores the pre-search snapshot. Bash-style status:
  `(reverse-i-search) 'q'` / `(failed reverse-i-search) 'q'`. Run
  rebinds to F5-only + Ctrl-Enter / Ctrl-J to free Ctrl-R.
- **`\watch` / repeat query** (Ctrl-W) — re-runs the buffer (or
  last history entry) every 2 s, routed through the same safety
  pipeline. Any key stops. Refused during an open auto_tx so we
  can't pile up runs on a paused session.
- **NOTICE / WARNING / RAISE surfacing** — connection driver
  switched from `connection.await` to `Connection::poll_message`,
  intercepting `AsyncMessage::Notice` and forwarding via an
  unbounded channel. App pumps through `AppMsg::Notice` to a
  bounded ring buffer (50 entries) + status footer; tracing
  captures detail / hint at info level.
- **`\e` external editor** (Ctrl-X) — pending-flag pattern: editor
  key handler sets the flag; main `run()` loop suspends the TUI
  (new `Tui::suspend` / `Tui::resume`), writes the buffer to a
  per-pid temp file, execs `$EDITOR` (multi-word like `code --wait`
  split on whitespace; falls back to `$VISUAL`, then `vi`), reads
  back on save. Always resumes the TUI even if the editor errored.
- **Connection switching mid-session** — `c` in Normal mode opens
  the existing `ConnPick` picker. Cancels any in-flight query
  first. The picker's Enter handler already does the reconnect
  (`start_connect` after swapping `self.dsn`); on Booted the new
  client + tunnel land and the old tunnel drops on a worker
  thread.
- **Batch / pipe mode** — `pgman --batch --sql "…" --format
  csv|tsv|json|expanded`. SQL via `--sql` or stdin. New
  `conn::connect_only` skips the schema fetch / bootstrap that
  the TUI needs. `batch::run` writes the formatted result to
  stdout; notices go to stderr. Exit 0 on success, 1 on query
  failure, 2 on connect / arg failure. Pure formatters
  (`format_csv` / `tsv` / `json` / `expanded`) are unit-tested
  with RFC-4180 quoting, control-char escaping, and `\x`-style
  column padding.

### Editor — authoring polish

Three quality-of-life features after the psql-parity sweep.

- **Semantic syntax highlighting.** New `query::highlight` module:
  pure two-pass lex + classify. Keywords / functions / strings /
  comments / numbers / identifiers via hand-rolled lexer (handles
  `'`-escapes, dollar-quoted `$tag$ … $tag$`, nested block
  comments, multi-byte safe). Identifiers then resolve against the
  schema cache + in-scope tables / aliases / CTEs / virtual columns
  — known stays default-coloured, unknown turns red (typo flag).
  Loose resolution: any cache hit anywhere counts. Theme grows two
  fields (`syn_string`, `syn_unknown`) for the dark / light /
  high-contrast variants; everything else reuses existing colours.
  The editor render walks per-line spans and overlays the cursor's
  REVERSED glyph on top of whatever syntax colour it sits in. 17
  unit tests cover the lexer.
- **Auto-save editor buffer.** `util::data_dir()` (new helper —
  `~/.local/share/pgman`) holds `draft.sql`; on quit we
  `write_atomic` the current buffer there, on startup `main` reads
  it back and seeds `editor_buffer` + cursor. Empty buffer clears
  the persisted draft so a deliberate clean-out survives a restart.
  Load lives in `main.rs` (not `App::new`) so unit tests don't
  pull a live developer's draft from disk.
- **`pg_format` buffer reformat (Ctrl-F).** Subprocess to
  `pg_format` (the standard `pgformatter` Perl tool); buffer
  piped to stdin, prettyprinted output replaces the editor. Missing
  binary surfaces an actionable error (`brew install pgformatter
  or apt install pgformatter`). Done inline since pg_format is
  sub-second; spawn_blocking would just add plumbing.

### Performance / DBA — slow queries + active sessions

- **`T` slow queries panel** (Mode::SlowQueries). Loads from
  `pg_stat_statements` via a tagged catalog query
  (`/* pgman:slow */ … LIMIT 50`). Pure parser in
  `query::slow_queries::parse` resolves columns by name so a future
  Postgres rename doesn't silently mangle the result. Renders as
  `total_ms / mean_ms / calls / rows / query`; the focused row's
  full SQL appears in a detail strip below. Enter copies the SQL
  into the editor for tuning; `r` refreshes; `q/esc` close.
  Failure path detects "relation does not exist" and appends a
  hint pointing at `CREATE EXTENSION pg_stat_statements`.
- **`L` active sessions panel** (Mode::Sessions). Loads
  `pg_stat_activity` with `pg_blocking_pids()` joined in; blocked
  rows sort to the top and render in red. Columns:
  `pid / user/app / state / age(s) / blocked / query`. `r`
  refreshes; `q/esc` close. Self-exclusion via
  `pid <> pg_backend_pid()` so the panel doesn't list itself.
- Both panels go through a new `AppMsg::SlowQueriesLoaded` /
  `AppMsg::SessionsLoaded` round-trip (generation-tagged like the
  other messages); the catalog queries skip the safety pipeline
  since they're admin reads.
- 7 unit tests on the pure parsers (`slow_queries` + `sessions`)
  + 5 App-side handler tests + 2 insta snapshots.

### Schema browser

- **`S` schema browser** — new `Mode::SchemaBrowser` overlay. Left
  pane: tree of schemas → tables (collapsed by default; Enter
  toggles each schema). Right pane: details for the focused
  row — `schema: X · N tables` for schemas, `schema.table` with
  column list + constraint list (from `cache.constraints`) for
  tables. Pure flatten helper (`flatten_schema_browser`) — no
  live queries, served entirely from the schema cache. j/k
  navigate, Enter expand, g/G jump, q/esc close. Five unit tests
  + one insta snapshot. Live DDL / sample-rows / table size are
  follow-ups in the Open section above.

### EXPLAIN plan visualizer

- **EXPLAIN plan visualizer** — Ctrl-E / Ctrl-A now send
  `EXPLAIN (FORMAT JSON) …` / `EXPLAIN (ANALYZE, FORMAT JSON) …`,
  parse the JSON via new `query::explain::parse`, and pop a
  dedicated `Mode::ExplainTree`. The tree renders one node per
  line with indent + expand/collapse glyph (`▼` / `▶` / `·` for
  leaves), node-type-aware stats (actual time / cost; actual rows
  / plan rows), and the *hottest* node (highest `Actual Total
  Time` under ANALYZE, else `Total Cost`) is red-bolded so the
  bottleneck pops at a glance. j/k navigate; Enter toggles
  collapse on the focused subtree; g/G jump to root/last; q/Esc
  close. Per-node `extras` (Filter, Index Cond, Hash Cond, …)
  show under the focused row only — keeps the tree readable.

### Result grid — sort / filter / export

- **Sort by column.** `h` / `l` (and Left / Right) move the column
  cursor — the focused header reverses so it's obvious which column
  is targeted. `s` cycles: off → ASC → DESC → off. Snapshots the
  raw row order before the first sort so the "off" state restores
  it without re-running the query. Numeric-aware compare via
  `grid::cmp_cells` (so `2` sorts before `10`); empty strings (NULL
  renderings) sort last per Postgres's default `NULLS LAST`.
- **Live row filter (`/`).** `Mode::GridFilter` — each char updates
  `grid_filter` and rebuilds the visible-row index in place; the
  status footer shows `filter: /pat · m/N row(s)`. Case-
  insensitive substring across every cell. `n` / `N` step through
  matches in the visible order. Enter accepts; Esc clears.
- **Export to clipboard (`Y`).** Serialises the *visible* rows
  (post-sort, post-filter) via the existing `batch::format_csv`
  formatter and pushes to the system clipboard via `arboard`.
  Status shows the copied row count.
- **RowDetail / CellDetail correctness through filter+sort.** New
  `App::selected_grid_row_idx()` maps the visible cursor index
  through `grid_visible_rows` to the actual `grid.rows` index;
  all three downstream features (row detail, cell zoom, cell
  yank) go through it so a filtered or sorted view never opens
  the wrong row.

### Completion

The largest section. Built bottom-up: tokenizer → FROM parser →
clause classifier (scope-stack) → completion engine, all pure-tested.

**Core engine**
- Schema cache fetched at connect via `pg_catalog` (best-effort —
  permission failure → empty cache → completion disabled cleanly).
  Now includes sequences, indexes, and `pg_constraint` rows for
  unique / primary keys.
- Tab cycles candidates; `alias.col` only offers the aliased
  table's columns; `schema.|` only offers that schema's tables;
  bare-identifier completion is biased toward columns of tables in
  the current FROM / JOIN scope (including subquery FROMs after the
  cursor).
- 3-segment qualified completion (`schema.table.col`).
- Popup overlay anchored under the editor, up to 8 visible rows,
  auto-scroll, active row highlighted.
- Esc during a cycle restores the originally-typed prefix.

**Grammar awareness**
- `query::clause` classifier scans tokens of the current statement
  and returns `ClauseContext` + an optional write target. Tolerant
  of mid-typed buffers.
- Scope-stack: subquery `(...)` no longer leaks ctx after `)`;
  `WITH cte AS (SELECT em` classifies correctly inside the body.
- Branches: `StatementStart`, `TableRef`, `SelectList`, `Predicate`,
  `HavingPredicate`, `OrderOrGroup`, `InsertColumns(t)`,
  `UpdateAssign(t)`, `Values`, `ExplainOptions`, `VacuumOptions`,
  `GucParameter`, `GucValue`, `TypeName`, `ConstraintName`,
  `DropTarget(DropKind::{Table|Index|Sequence})`.
- UPDATE / DELETE write target folded into in-scope so WHERE
  completion works without a FROM.

**Vocabulary** — single source of truth at `query::vocabulary`.
Module-level tests assert "all entries uppercase (or lowercase for
GUCs / types / values), no dups". Adding a function / operator is a
one-line append.
- Aggregate / scalar / window functions in SelectList.
- Predicate operators (`LIKE`, `IN`, `BETWEEN`, `IS NULL`, `EXISTS`,
  `IS DISTINCT FROM`, `SIMILAR TO`, …).
- JOIN variants (`INNER JOIN`, `LEFT OUTER JOIN`, `NATURAL JOIN`,
  `LATERAL JOIN`, …).
- DDL verbs, session keywords, perms, Postgres catalog helpers
  (`PG_SIZE_PRETTY`, `VERSION`, `TXID_CURRENT`, …), string /
  numeric / array / JSON families.
- `EXPLAIN (...)` options, `VACUUM (...)` / `ANALYZE (...)` options
  (shared list).
- `SHOW` / `SET` GUC parameters + values (`on` / `off` / `true` /
  `false` / `default`).
- `CAST(expr AS |)` type names (~40 entries).
- `INSERT … ON CONFLICT (col) DO UPDATE SET col = EXCLUDED.col` —
  EXCLUDED registered as a virtual table; constraint names scoped
  to the write target.
- `DROP TABLE / INDEX / SEQUENCE / VIEW / MATERIALIZED VIEW`,
  `REINDEX <kind> name`.
- `COPY tab (col_list)`, `TRUNCATE tab`.

**CTE / subquery column inference**
- `extract_ctes` returns `Vec<CteDef>` with inferred columns.
  Explicit `WITH foo(a, b) AS …` lists win; otherwise the body's
  SELECT list is parsed.
- `parse_from_tables` registers subquery aliases
  (`FROM (SELECT ...) sub`) and stores inferred columns on the
  `TableRefInQuery.virtual_columns` field.
- `SELECT *` expansion in CTE / subquery bodies via
  `SelectItem::{Named, Star, StarOf}` + the resolved variants
  (`resolve_select_columns`, `extract_ctes_resolved`,
  `parse_from_tables_resolved`).
- UNION column inference is implicit — extractor stops at UNION so
  only the first arm's columns survive.

**Continuations** — `vocabulary::continuations` lists what naturally
follows each ClauseContext (`AFTER_TABLE_REF`, `AFTER_PREDICATE`,
…). Surfaced as Keyword candidates AFTER identifiers so the cycle
prioritises columns / tables / aliases. In SelectList, FROM ranks
BEFORE FORMAT.

**UX polish**
- Case preservation — `sel|` → `select`; `SEL|` → `SELECT`.
- Longest-common-prefix expansion — Bash-style two-phase Tab. First
  Tab on `t_|` against `t_users`, `t_user_logs`, `t_user_roles`
  expands to `t_user_`; second Tab picks the first match.
- Live narrowing while typing — narrowing keys (chars, Backspace,
  Delete) keep the cycle alive and refresh the candidate list. Esc
  still undoes back to the pre-Tab state.
- Exact-match auto-commit — typing the full name + Tab dismisses
  the popup. Honours the cache's canonical case (`USERS` → `users`).
- Column candidates show owning table in popup — `email (column · u)`
  or `email (column · users)`. Aliases show their underlying table.
  Non-public tables show their schema.
- Footer hint switches to `type to narrow · tab cycle · esc undo`
  while the popup is up.
- Bold the matched prefix in popup rows.
- Auto-trigger after `.` — `users.|` pops the column list with no
  Tab. Numeric literals (`3.14`) excluded by char-aware lookup.
- Auto-trigger after identifier-introducing keywords + space
  (`FROM `, `JOIN `, `WHERE `, `AND `, …) opens the popup.
- Tab on whitespace / empty prefix opens the context-aware popup
  with no auto-insert.
- Function completions insert `NAME(` so the cursor lands inside.
- Ctrl-Space alias for Tab — industry-standard "open popup"
  shortcut.
- Fuzzy / subsequence fallback when prefix-anchored returns nothing
  and the typed prefix is ≥3 chars. Ranked by match tightness;
  capped at 30 results. Qualified prefixes narrow to the qualifier's
  children.

### ebman survey

Survey of `/Users/tom/git/ebman/src/` to decide what's worth lifting.
The lifts themselves are in [Open → Reuse from ebman]; the survey
itself is done.
- Adopt ebman's splash *rendering* but NOT its 3s minimum duration —
  overlap with connect, keep instantly dismissable.
