# Plan — after v0.2.0

The rolling window of active work, in the shape ebman uses. `BACKLOG.md`
is the reservoir; this file is only *what to do next*. An item lives in
exactly one of the two.

## How the loop runs

Each session: read this file, work the window top-down, prune and refill
it before finishing. Three items in flight is usually right.

**Item classes** — the stage set scales with the item:

| class | stages |
|---|---|
| **mechanical** — covered by an existing guard, or a port of something that already works | dev → verify → green → commit |
| **behaviour** — changes what the tool does | analyse → dev → verify → docs → green → commit |
| **architecture** — refactor, new seam, anything touching >3 modules | analyse → design note → dev → verify → **review** → docs → green → commit |

**Verify-the-claim is its own gate.** Break the code the test claims to
pin, watch it fail, restore. A guard is not done without a `CAUGHT` line
in the report. ebman found five tests in one day that covered less than
their names claimed; none was caught by reading the test.

**Outcomes**: done · re-scoped · killed by evidence (the premise was
wrong — record it, don't drop it) · skipped (a stop condition fired;
say which).

**Parallelism**: fan out read-only work freely. Concurrent edits go in
separate worktrees; edits to `PLAN.md` and `BACKLOG.md` stay on the main
line.

## The bar

pgman fronts the consultancy. www.poly.io/products/pgman already lists
it ("Coming soon", "in active development") with the promise: **Spring
datasource auto-discovery, logs → runnable SQL with N+1 detection,
read-only-by-default DBA tools**. The release flips that page from a
promise to a link, so the bar is not "it works" but "it is not
embarrassing next to ebman, and it visibly does something psql / pgcli /
lazysql / rainfrog / harlequin do not."

The differentiators are real and nothing else in the TUI space has
them: Spring / IntelliJ discovery, Hibernate / RDS-log / JDBC
reconstruction, the JDBC tap with OTLP ingest, per-database safety
guards, DBUnit fixtures, the schema-lint wizard. The risk is not the
wedge; it is that a first-time user hits a rough edge in the first
minute and never reaches it.

## Where we are (2026-10-24)

**v0.2.0 is released**: GitHub release (Latest), crates.io, and the
Homebrew tap (`tombaldwin/homebrew-tap`), with the in-repo formula to
match (PR #26). v0.1.0 went out on 2026-10-22. The release flow is the
one in `docs/development.md`: CHANGELOG heading, tag on a green merge
SHA, draft from `release.yml`, Tom publishes, then the formula.

The phases, panels and release-day checklist this file carried up to
v0.2.0 are in `docs/backlog/archive.md` → Release plan history.

## Window — v0.2.1

Tom, 2026-10-24: "1, 2, 3, then probably 4 but needs looking at first."

### 1. Housekeeping · mechanical · done on `housekeeping-0.2.1`

This file rewritten; done phases archived; stale backlog items dropped.

### 2. First impressions · behaviour

The rough edges a new user meets in the first minute, from the v0.2.0
panel. One commit each; a snapshot pins each visible fix.

- **F-key-only hints**: start card `F4 JDBC tap` / `· F4 to look`, help
  heading `F3 notify`; show `:tap` / `:notify` beside them.
- **README's feature list** doesn't mention cell editing or paging; the
  README `demo.gif` shows 0.1.0's chrome; crates.io's README has relative
  `docs/` links that don't resolve there.
- **`--demo` writes `pgman.log`** although "no disk writes" is claimed.
- **The title's date reads as unexplained** (`pgman 0.1.0 · 2026-10-22`
  on a later day). Say it is the release date, or drop it.
- **Result diff title at 80 cols** cuts off `key: id (primary key)`; put
  the key before the query text.
- **`:fixture`'s status is elided mid-note** at 160 columns, eating
  "(fetched rows only · more not fetched)".
- **Schema wizard detail pane wraps mid-word** at 80x24 / 60x16, and its
  header's last column reads `tit`.
- **The result title is cut by the border at 80 columns** when a filter
  is active and the counts reach five digits.
- **Tap polish**: the N+1 row's caller is cut at the border without `…`;
  the start card at 60x16 drops hint rows with no "more" marker;
  docs/tap.md's "JAR backpressure" label puzzles an OTLP-only user.

### 3. Correctness from the v0.2.0 panel · behaviour

- **Connect hangs behind an ACCESS EXCLUSIVE lock.** Bootstrap's catalog
  reads (`pg_get_indexdef`) wait forever ("connecting…") with no
  `lock_timeout`, and backends pile up behind it.
- **Lost during a running query, card and footer disagree**: the card
  says `the connection to the server closed` while the footer has the
  query's `terminating connection due to administrator command`. Let a
  57P01/57P02 `QueryFailed` behind the card refresh its reason; the
  late-Booted closed-client path drops the forwarder's FATAL too.
- **The header says `Session: writable` on a standby** in the paging
  view's status; read the pinned read-only state there too.

### 4. Look before deciding · review

Not started until 2 and 3 land; findings go to Tom, not into code.

- **pgman-tap JAR**: built locally at `~/git/pgman-tap`, unpublished
  and off the website. What would publishing take (Maven Central
  coordinates, signing, the docs that name it), and is it ready?
- **The poly.io pgman page** (site lane `aa7770e1`) awaits Tom's
  review; check what it says against 0.2.0.

Then cut v0.2.1 the same way as v0.2.0, panel first.

## Deferred

- Publishing the `pgman-tap` JVM library: see Window step 3.
- Mutation testing (`mutants.yml` / `scripts/sweep.sh`).
- `unwrap_used` / `expect_used` deny lints.
- `cargo-semver-checks`: meaningful now that 0.1.0 and 0.2.0 are on
  crates.io, though the library is not a public API yet.
- Everything in `BACKLOG.md → Open` that is a feature.
