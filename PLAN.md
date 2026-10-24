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

### 1–3. Done on `housekeeping-0.2.1` (2026-10-24)

Housekeeping, the nine first-impressions items and the three panel
correctness items, each in its own commit with a CAUGHT per guard;
detail in CHANGELOG `[Unreleased]` and the archive. Follow-ups are in
BACKLOG.md → From the v0.2.1 polish, and the panel section.

### 4. Look before deciding · review

Looked at 2026-10-24 (read-only); findings went to Tom, who decides.
The JAR is wire-compatible with 0.2.0 (78 tests, 9/9 golden events)
but has no publishing setup; the site shows v0.2.0 but its changelog
stops at v0.1.0.

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
