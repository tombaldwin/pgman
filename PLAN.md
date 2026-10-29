# Plan — after v0.3.0

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

## Window — after v0.3.0

### 0. v0.3.1 released (2026-10-28)

The Homebrew formula depends on libpq and its wrapper names it in
PGMAN_PG_BIN; dump and restore look in --pg-bin, PGMAN_PG_BIN,
Homebrew's libpq, then PATH. Published on Tom's ask: GitHub release,
crates.io, the tap (11e8b14). Checked with a real `brew upgrade pgman`
(it pulled libpq in; a dump with libpq off PATH found it).

### v0.3.0 released (2026-10-28)

`pgman dump`, `restore`, `run`, `apply` and MCP `dump` / `restore`, on
Tom's go. Published 2026-10-28: GitHub release, crates.io, the Homebrew
tap (with a libpq caveat). The panel (four reviewers) and its re-checks
are in `docs/backlog/archive.md`; what they left for later is in
BACKLOG.md → From the dump/restore review.

### 1. Masked dumps and the life of a copy — phase 1 built

`docs/design/masked-dumps.md`, third draft (2026-10-28): two review
passes (four Fable reviewers, then the same four plus a fresh one) and
Tom's decisions. Masked by default with built-in rules (undecided columns
NULLed, or the type's empty value, and named); encrypted from phase 2b;
every copy tracked from dump to deletion with a reason and a journal,
local and to CloudWatch; kept from an MCP-only agent. Thirteen decisions
made, plus `unattended = true` (Tom: allow it, operator level only).

Phase 1 built 2026-10-28, unreleased: the dump policy (`src/policy.rs`),
the typed name and reason for protected dumps, `--schema-only`,
`--reason`, `--ticket` (from the branch), MCP protected dumps schema
only, the journal (`src/journal.rs`) and `pgman copies` list, show, rm,
report (`src/copies.rs`). The design note's "Phase 1 as built" says
where it differs.

Phase 1b built 2026-10-29, unreleased: the journal to CloudWatch
(`src/journal_send.rs`).

Phase 2a built 2026-10-29 on the `masking` branch, not merged: masked
dumps and their restore, the rules file, `masking show`/`check`, the
protected default to masked, MCP masked dumps with confirmed rules and
no raw protected copies, `copies rm` dropping databases, the dump
directory and synced-folder checks, the leak test, `docs/privacy.md`.
The design note's "Phase 2a as built" lists what differs and what is
left (in BACKLOG.md). Next: a review panel on the branch, then merge;
then 2b (encryption).

### 2. Waiting on Tom

- **pgman-tap JAR**: go or not yet; `io.poly` groupId; loopback-only
  default with `pgman.tap.allow-remote`. Wire-compatible with 0.2.x (78
  tests, 9/9 golden events) but no publishing setup (Central account,
  namespace DNS record, GPG key, repo, secrets).
- **The site's changelog**: entries for v0.2.0, v0.2.1 and v0.3.0 sent
  to the site lane (poly · site) to land, 2026-10-28. Tom no longer
  approves the wording first: write them for each release and tell him.
- **Required CI checks on main**: the branch-protection call needs Tom
  (the payload is drafted).

### 3. Next from BACKLOG.md

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
