# pgman backlog

## Done

- **Scaffold.** Repo, `Cargo.toml`, lib+bin split, `CLAUDE.md`. Lifted
  `theme.rs` + `util.rs` from ebman. Pure modules implemented with tests:
  `safety` (statement classification + per-DB guards), `query::subst`
  (placeholder substitution), `query::nplus1` (fingerprint + clustering),
  `query::reconstruct` (shared types), `conn` (DSN parsing),
  `creds::spring` (`.properties` parsing + Java-project detection).
  Stub modules with TODO markers: `query::{hibernate,pglog,jdbc}`, `splash`.

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
- `query/reconstruct.rs`: shared types — DONE (scaffold).
- `query/subst.rs`: `?` + `$N` substitution — DONE (scaffold).
- `query/hibernate.rs`: parse `org.hibernate.SQL` + bind lines (HB5 `BasicBinder`
  and HB6 `jdbc.bind`); group by thread; pair SQL with following binds.
- `query/pglog.rs`: parse Postgres/RDS `log_statement` output + `DETAIL:
  parameters:` lines. **Treat as the primary source** — it needs no app redeploy.
- `query/jdbc.rs`: two-pane paste (SQL + typed params).
- `query/nplus1.rs`: fingerprint + clustering — DONE (scaffold); add a
  time-window heuristic once `ReconstructedQuery` carries timestamps.
- `mode_hibernate.rs`: log-import view feeding hibernate + pglog parsers.

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

## v3+ — deferred

- `catalog/` — version-aware catalog trait; EPAS support.
- JPA entity ↔ table mapping.
- Migration safety linter (lock-heavy operations).
- EXPLAIN plan diff. Minimal schema-identifier autocomplete.
