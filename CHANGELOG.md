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

- **Release-panel polish**: overlays stay off the header and footer
  (the About box used to paint over its own close hint at 80×24); the
  pickers float inside their panel and fit their rows; the header and
  start card show `pg 16.15`, not the Debian build string; the confirm
  modal sizes to its content; footer text is measured in display
  columns so a Japanese server error no longer clips the action keys;
  the help overlay no longer drifts on narrow terminals; the start card
  budgets its rows correctly at 60 columns and says `running …` while
  the first query is in flight; the log picker scrolls; a buffer over
  256 KiB renders plain; the tap panel no longer hands out a Gradle
  coordinate for an unreleased JAR; a launch without a terminal says so
  instead of `os error 6`; the CLI shows the same connect hints as the
  TUI; a read-only refusal says where `read_only` lives; `--log` refuses
  files over 64 MB; no auto-completion fires inside a pasted log.
- **Security review before the first release** (five findings, each
  reproduced, each now pinned by a test):
  - Anything discovered in the working tree is untrusted. A discovered
    connection never auto-connects, even when it is the only one; the
    picker shows host, `sslmode` and any tunnel before you press Enter.
    `PGPASSWORD` is only applied to a `--dsn`. A `${…}` placeholder
    never resolves into a URL's host or port, and an unresolved
    password placeholder marks the pick rather than being sent as the
    literal string. A project's `[safety]` block can only tighten your
    own. A discovered `ssh_tunnel` is confirmed before `ssh` runs.
  - The statement splitter now understands double-quoted identifiers,
    `$` inside identifiers, `E''` strings and nested comments, refuses a
    script it cannot verify, and the server executes exactly the
    statements the classifier saw. `SELECT … INTO` is a write; the
    read-only setting cannot be flipped from inside a read-only
    session; `pg_terminate_backend` and friends confirm.
  - Passwords containing `/` or `@` were parsed wrongly and reached the
    log unredacted. An unknown or mistyped `sslmode` is now an error
    instead of a silent downgrade to accept-any-certificate.
  - Every JDBC-tap event field is bounded at ingest, connections are
    capped, OTLP bodies are read incrementally, aggregates are memoised
    (75 ms → 1.3 ms per frame at the cap), malformed-frame warnings are
    throttled, the log rolls daily, exported reports strip terminal
    escapes, `--tap-record` is owner-only.
  - Files are private from the first byte (pgman's own atomic writer),
    the `$EDITOR` buffer lives in a private temp dir that is always
    removed, the log and pre-existing directories are repaired to
    owner-only, `XDG_*` overrides are honoured, `--upgrade` no longer
    runs cargo inside the checkout's directory.
- **Everything pgman writes is now owner-only (0600)**: query history,
  the draft, saved queries, report and fixture output, the update-check
  cache; the cache directory is 0700. They were created at the umask
  default, usually world-readable.
- **An unresolved Spring `${PLACEHOLDER}` was used as a literal
  hostname.** `${VAR}` and `${VAR:default}` now resolve from the
  environment; a pick that still has one is labelled in the picker and
  refused with "unresolved placeholder ${DB_HOST} — export it, or put the
  connection in .pgman/pgman.toml" instead of a DNS error.
- **The in-app help had no section for the JDBC tap monitor or the
  result diff**, and omitted `D`, `r`, `/`, `q` and quote autoclose.
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

- **A `:` command bar**, as in ebman and k9s: `:about`, `:help`,
  `:update`, `:quit`, `:readonly on|off`, `:connect NAME`, and every
  `\` command without the backslash (`:l`, `:x`, `:dt`, `:i PATH` …),
  with Tab completion. `?` opens help from any mode. `\c` and `:connect`
  accept a quoted name or a unique prefix.
- **`pgman postgres://…`** as a positional argument; **`--init-config`**
  writes a commented default `safety.toml`; **`--help`** fits on a
  screen, grouped, and names no internal types.
- **Typed JSON in `--batch --format json`**: `null`, numbers and
  booleans instead of strings.
- **`--demo` answers queries** synthetically, through the same safety
  guards as a live session, and opens on the start card — so a talk or
  a recording can go paste-log → reconstruct → run → rows without a
  database.
- **Pasted JDBC is a third way in**: a `?` statement, a blank line, and
  `TYPE:value` parameter lines reconstruct like a log does.
- **Docs**: `docs/keys.md`, `docs/commands.md`, `docs/configuration.md`,
  `docs/safety-and-privacy.md`, `docs/logs-to-sql.md`,
  `docs/development.md`, and `ARCHITECTURE.md`; a README for the
  binary-install era; the demo re-recorded around the wedge.
- **Guard tests for the house conventions** (`tests/guards.rs`): no
  hardcoded colours outside the theme, no `println!` in the TUI, no
  hardcoded home paths, Ctrl-guarded key arms before unguarded ones.
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

**This is the first release's notes.** 0.1.0 was never tagged or published —
no GitHub Release, crate, or binary ever existed for it, so the version
doesn't bump for the actual first release; it becomes one. The June text
below, plus everything currently sitting under [Unreleased] above, will be
folded into this section on release day. Until then they stay separate — this
entry is not yet what ships.

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
