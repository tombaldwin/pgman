# Changelog

All notable changes to pgman are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

### Changed

- **ratatui 0.29 → 0.30.2, crossterm 0.28 → 0.29**, via `tb-tui-common`
  0.2.0. No code changes needed — the errors a naive bump produces are a
  version clash, not an API migration. All 16 committed insta render
  snapshots matched the layout accepted under 0.29.
- **Dependency majors**: `quick-xml` 0.36 → 0.42, `toml` 0.8 → 1.1,
  `criterion` 0.5 → 0.8, `tokio-postgres-rustls` 0.13 → 0.14, plus the
  pinned GitHub Actions.
- **`webpki-roots` 0.26 → 1.0**. No code changes needed — the Mozilla
  root bundle fallback in `conn.rs` uses the same `TLS_SERVER_ROOTS`
  shape.

### Fixed

- **A yanked `chacha20` 0.10.1**, pulled in transitively via `rand` →
  `postgres-protocol`, had `cargo-deny` red. Fixed with
  `cargo update -p chacha20` to 0.10.2.

- **Server notices could be lost on exit in batch mode.** `batch::run`
  drained the notice channel in a detached task and returned without
  sequencing the two, so a `RAISE NOTICE` / `RAISE WARNING` could vanish
  whenever process exit won the race. Now awaited (bounded at 2s) on
  both the success and failure paths.

- **Two RUSTSEC advisories** (2026-0194, 2026-0195) — denial-of-service
  paths in `quick-xml` 0.36's attribute and namespace handling. Fixed by
  upgrading rather than waived. `cargo-deny` had been red since
  2026-07-25.
- **The integration-test job**, red since 2026-07-25. Two tests predated
  a safety-gate tightening and were being refused by it; they now
  confirm explicitly with `--yes`.

### Added

- **End-to-end coverage that the binary honours the batch safety gate.**
  Previously tested only at the unit level — the two tests above were
  covering it by accident, and by failing.

### Note on downgrades

`toml` 1.x implements TOML spec 1.1.0, and pgman *writes* TOML
(`saved::save_to`). Verified and now pinned by test: saved-query files
are still written with TOML 1.0-compatible escapes, so a file written by
this build loads in an older pgman.

## [0.1.0] — 2026-06-06

First public beta. pgman is pre-1.0; expect rough edges and breaking
changes before 1.0. Highlights (see `BACKLOG.md` for the full record):

- **Connect & browse** — auto-discovery of datasources from Spring
  `application*.yml`/`.properties` (incl. profile overlays), IntelliJ
  `.idea/dataSources.xml`, and `.pgman/pgman.toml`; schema browser; results
  grid with filter / find / sort / bookmarks; result diff.
- **Query reconstruction** — Hibernate logs, Postgres/RDS server logs, and
  pasted JDBC turned into runnable SQL; N+1 detection.
- **Safety** — read-only-by-default connections, `statement_timeout`,
  per-database guard rails classifying every statement.
- **Editor** — syntax highlighting, `pg_format`, history, saved queries (with
  `:param` prompts, rename, search), DBUnit fixture apply (`Ctrl-D`) and
  capture (`\fixture`) with per-database clean strategy.
- **Performance / DBA** — slow-query and active-session panels, EXPLAIN tree,
  schema-lint wizard.
- **JDBC tap** — live app-side query observability (TCP / UDP / OTLP ingest)
  with hotspots, per-caller and per-pool rollups, transaction view, baseline
  diff, and live N+1 detection; Markdown / HTML report export.

[Unreleased]: https://github.com/tombaldwin/pgman/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tombaldwin/pgman/releases/tag/v0.1.0
