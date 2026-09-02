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

- **First-minute polish, found by rendering every screen at four
  terminal sizes** (`tests/sizes.rs`, 144 snapshots, now a CI gate):
  the footer clipped key hints mid-word at 80 columns (now sheds whole
  hints and says `F1 +N more`), and clipped status and error lines the
  same way (now ellipsised so the action keys survive); the help
  overlay wrapped descriptions to the left margin with no key beside
  them; the sessions panel sheared on `idle in transaction`; the
  slow-queries divider did not join its border; the schema wizard and
  the schema browser cut text mid-word with no ellipsis, and the
  browser's tree ran into its detail pane; the completion popup fused
  with the result panel's border; the safety confirm modal showed a
  Rust enum (`Delete { has_where: false }`) instead of a sentence; the
  connection picker said "no connection — start pgman with --dsn" above
  a picker offering connections.
- **The splash held for three seconds** even when the connection had
  resolved or the picker was the landing screen. Now a 600 ms floor,
  dismissed early by either, or by any key.
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

- **An update check that knows how you installed pgman.** Once per
  six hours, after the first frame is drawn, one request to crates.io;
  a `⬆ x.y.z` badge in the header and the exact upgrade command in the
  About overlay (`brew upgrade pgman`, `cargo install pgman --locked
  --force`, or the releases page). Off with `--no-update-check` or
  `PGMAN_NO_UPDATE_CHECK`, and always off in `--demo` and `--batch`.
- **`--upgrade` works for every install channel** it can act on:
  checkout, cargo, and Homebrew. A standalone binary is told where the
  releases are instead of being told it "is not a working tree".
- **Release machinery**, ported from ebman: a tag-triggered workflow
  that refuses to build from a commit CI never passed, builds four
  targets with build provenance, drafts the GitHub Release, and
  publishes to crates.io when the draft is published; a Homebrew
  formula and the script that bumps it; the release date in the About
  overlay, read from this file at build time.
- **A start card on connect** instead of a grid of database names.
  Connection, databases and sizes, the six main keys, and the two
  things nothing else does: `F8` logs → SQL and `F4` JDBC tap.
- **Pasting a log into the editor is recognised** — the status line
  and editor title say "looks like a Hibernate log · ctrl-l / F8 to
  reconstruct queries", so the headline feature no longer depends on
  knowing it exists.
- **`pgman --log PATH`** (`-` for stdin) opens straight into the
  reconstructed queries from a Hibernate or Postgres server log.
- **SQL keyword completion** after identifiers (two characters in,
  never after a `.` qualifier, case follows what you typed), and
  **`\l`, `\x`, `\c`, `\i`** for psql muscle memory.
- **End-to-end coverage that the binary honours the batch safety gate.**
  Previously tested only at the unit level — the two tests above were
  covering it by accident, and by failing.

### Note on downgrades

`toml` 1.x implements TOML spec 1.1.0, and pgman *writes* TOML
(`saved::save_to`). Verified and now pinned by test: saved-query files
are still written with TOML 1.0-compatible escapes, so a file written by
this build loads in an older pgman.

## [0.1.0] — 2026-06-06

**Never tagged or published.** This entry records a version bump and a
README badge; no tag, GitHub Release, crate, or binary ever existed for
0.1.0. Everything below shipped for real in 0.2.0, the first release.

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
