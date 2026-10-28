# Plan — after v0.2.1

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

## Where we are (2026-10-28)

**v0.2.1 is released** (2026-10-25): GitHub release (Latest), crates.io,
and the Homebrew tap, with the in-repo formula to match. v0.2.0 and
v0.1.0 went out on 2026-10-24 and 2026-10-22. The release flow is the
one in `docs/development.md`: one bump commit (Cargo.toml, Cargo.lock,
CHANGELOG heading and links), tag on a green SHA, draft from
`release.yml`, publish on Tom's go, then the formula.

v0.2.1 went: a four-reviewer panel, two fix branches merged on main, a
re-review of the fixes (two MEDs: the reload's lock bound racing
earlier work, a reload aborting the operator's transaction), fixed, and
a re-check GO. The phases and panels up to v0.2.0 are in
`docs/backlog/archive.md` → Release plan history.

## Window — after v0.2.1

### 0. Dump, restore, run, apply (2026-10-28) — built, in review

Tom's go 2026-10-28 ("yes to all including the optional mcp work"):
`pgman dump`, `pgman restore`, `pgman run` (scripts with their own
transactions, `\c` between databases), `pgman apply` (the CLI for `P`,
with `--outside`), and MCP `dump` / `restore` behind elicitation. All
on main with integration tests and CAUGHT lines; docs in
`docs/dump-restore.md`, `docs/run.md`, `docs/mcp.md`. Remaining:

- the independent review of d424e1f..HEAD (security and correctness),
  then its fixes;
- a release (v0.3.0) when Tom wants one: a release panel first, as for
  every release.

### 1. Waiting on Tom

- **pgman-tap JAR**: go or not yet; `io.poly` groupId; loopback-only
  default with `pgman.tap.allow-remote`. Wire-compatible with 0.2.x (78
  tests, 9/9 golden events) but no publishing setup (Central account,
  namespace DNS record, GPG key, repo, secrets).
- **The site's changelog** stops at v0.1.0: entries for v0.2.0 and
  v0.2.1 need Tom's OK on wording, then `npm run fallbacks`.
- **Required CI checks on main**: the branch-protection call needs Tom
  (the payload is drafted).

### 2. Next from BACKLOG.md

The v0.2.1 polish follow-ups (BACKLOG.md → From the v0.2.1 polish):
the scratch-safe `demo.tape` and a fresh `demo.gif`, the narrow tap
views, the FATAL paths that miss the connection-lost card.

## Deferred

- Publishing the `pgman-tap` JVM library: see Window step 1.
- Mutation testing (`mutants.yml` / `scripts/sweep.sh`).
- `unwrap_used` / `expect_used` deny lints.
- `cargo-semver-checks`: meaningful now that 0.1.0 and 0.2.0 are on
  crates.io, though the library is not a public API yet.
- Everything in `BACKLOG.md → Open` that is a feature.
