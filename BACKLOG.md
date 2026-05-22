# pgman backlog

## Done

- **Scaffold.** Repo, `Cargo.toml`, lib+bin split, `CLAUDE.md`. Lifted
  `theme.rs` + `util.rs` from ebman. Pure modules implemented with tests:
  `safety` (statement classification + per-DB guards), `query::subst`
  (placeholder substitution), `query::nplus1` (fingerprint + clustering),
  `query::reconstruct` (shared types), `conn` (DSN parsing),
  `creds::spring` (`.properties` parsing + Java-project detection), `splash`.
- **Query reconstruction parsers.** `query::pglog` (Postgres / RDS server
  logs — `statement` / `parse`-`bind`-`execute`, parameters paired by pid)
  and `query::hibernate` (Hibernate 5 & 6 logs — SQL paired with binds by
  thread). Both pure and tested. Stub module remaining: `query::jdbc`.

## v1 — the wedge

Ship: "paste a log or open me in a Spring project → runnable SQL → run it,
safely." Nothing else.

### M0 — shell + connection
- TUI event loop: `app.rs`, `app/msg.rs`, `ui.rs`. One frame clock with
  animation sources (splash / loading).
- `splash.rs`: animate the elephant; dismiss on keypress or connection-ready.
- `conn.rs`: real connection via `tokio-postgres` + `deadpool-postgres`.
  Apply safety session settings on connect (`default_transaction_read_only`,
  `statement_timeout`).
- Results grid for an arbitrary query (cap rows; reuse ebman's view-cache shape).

### M1 — query reconstruction (the hero)
- `query/reconstruct.rs`: shared types — DONE.
- `query/subst.rs`: `?` + `$N` substitution — DONE.
- `query/hibernate.rs`: HB5 + HB6 log parsing, thread-grouped — DONE.
  Follow-ups: reassemble `hibernate.format_sql=true` multi-line SQL.
- `query/pglog.rs`: Postgres / RDS log parsing — DONE. Follow-ups: a SQL line
  containing a log-level token still confuses line-splitting.
- `query/nplus1.rs`: fingerprint + clustering — DONE. Follow-up: time-window
  heuristic once `ReconstructedQuery` carries timestamps.
- `query/jdbc.rs`: two-pane paste (SQL + typed params) — pending.
- `mode_hibernate.rs`: log-import view feeding hibernate + pglog parsers —
  pending (needs the M0 TUI).

### M1.5 — Spring auto-connect
- `creds/spring.rs`: `.properties` parsing + Java detection — DONE (scaffold).
- Add `application.yml` parsing (profiles, `${}` placeholders) — needs
  `serde_yaml`. **Verify real Spring/SSM/1Password mechanics before building
  placeholder resolution** — the `${op://}`-as-property-source assumption is
  unconfirmed.
- Auto-detect on launch; show provenance; **require a keypress to confirm** the
  resolved target before connecting.

### M2 — editor
- `mode_editor.rs`: run statements + `EXPLAIN` / `EXPLAIN ANALYZE`.
- DML-aware EXPLAIN: never `ANALYZE` a mutation outside a rollback transaction.
- Every run routes through `safety::evaluate`.

## v2 — AWS (not started)

- `discover_aws.rs` + `mode_databases.rs`; `--rds <id>` launch handoff from ebman.
  (Add a one-line backlog item in *ebman* for the spawn-pgman action.)
- `rds_logs.rs` + `mode_logs.rs` — `DescribeDBLogFiles` / CloudWatch; feeds pglog.
- `creds/{ssm,secrets}.rs` + `creds/onepassword.rs` — placeholder resolution.
- `migrate/` — `MigrationStrategy` trait + `flyway` / `liquibase` / `custom`
  (config-driven table mapping; uflexi is one preset). Note: `pending()` is not
  implementable for a DB-table-only strategy — make it an optional capability.
- `mode_params.rs` — RDS parameter groups; view/diff + `ModifyDBParameterGroup`
  with confirmation; surface pending-reboot.
- `mode_dashboard.rs` — live PG stats + RDS CloudWatch metrics.
- `advisor.rs` + `mode_advisor.rs` — health-snapshot → `claude` CLI review;
  interactive handoff via `handoff.rs`. Snapshot must be scrubbable (egress).

## v2 — local DB sync & backups (not started)

Pull a remote database down for local testing; keep tagged backups.

- `snapshot.rs` — snapshot store under `util::data_dir()`: each snapshot is a
  `pg_dump` artifact (custom format) plus a metadata record (source DSN
  redacted, timestamp, size, db version, tag, pinned flag). A TOML/JSON index
  lists them.
- `mode_snapshots.rs` — list / create / restore / tag / pin / delete.
  - Create: `pg_dump` a remote DB → store. Capture the server major version.
  - Restore: `pg_restore` (or `psql`) into a chosen local target DB, with a
    confirm step (restore is destructive to the target).
  - Pin protects a snapshot from prune; tags group them (`pre-migration`,
    `prod-2026-05`, …).
- Version skew: `pg_dump` / `pg_restore` must be ≥ the server major version —
  detect and warn, or locate a matching client binary.
- Retention: optional prune of un-pinned snapshots past a count/age limit.
- Future (v3+): on-restore redaction / anonymisation of sensitive columns —
  a per-table/column rule set applied during restore. Listed below.

## v3+ — deferred

- **Redaction / anonymisation** on snapshot restore — declarative rules
  (`null` a column, fake an email, hash an id) applied as data lands locally,
  so local testing never holds real PII.
- `catalog/` — version-aware catalog trait; EPAS support.
- JPA entity ↔ table mapping.
- Migration safety linter (lock-heavy operations).
- EXPLAIN plan diff. Minimal schema-identifier autocomplete.
