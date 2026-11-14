# Plan — after v0.5.2

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

## Where we are (2026-11-14)

**v0.5.2 is released** (2026-11-11): GitHub release, crates.io, the MCP
Registry and the Homebrew tap. The release flow is the one in
`docs/development.md`: one bump commit (Cargo.toml, Cargo.lock,
server.json, CHANGELOG heading and links), tag on a green SHA, draft
from `release.yml`, publish on Tom's go, then the formula and the
site's changelog entry. Earlier windows, v0.2.1 to v0.5.2, are in
`docs/backlog/archive.md` → Release plan history.

On main, unreleased (0.5.3): the agent's reason filled in for a dump,
`restore_confirm = "clean_only"`, and the fixes from a five-reviewer
Fable review (MCP restore known by content, masking leaks, the rules
confirmation listing every rule, MCP's `allow` guards, `set_config`,
an editor COMMIT under `auto_tx`, git in an agent's checkout, …). The
CHANGELOG's Unreleased section lists them.

## Window — after v0.5.2

### 1. What the Fable review left (2026-11-12)

Worked top-down on Tom's "work through the open items", all on main,
unreleased: the TUI's set_config escape, a script's own COMMIT under
auto_tx, apply_patches' cut-off tick, a database named `y`, git in an
agent's checkout, the masking odds, an archive's data files checked as
they load, the docs and test gaps, arboard, weekly fuzzing (whose first
run found `classify("explain8")` overflowing the stack, fixed), PLAN
and CLAUDE.md; `pgman query` argument order and `--target`, the
password hint, connections.toml in the overview, `pgman doctor`; MCP
`describe`, `slow_queries`, `explain` with index suggestions,
`pgman diff` / `schema_diff`, and a dry run's schema changes.

Left in BACKLOG.md, each needing Tom or a design first: the libpq
environment and the editor's Ctrl-A/Ctrl-E (posture and keymap calls),
the main.rs split (main() itself is the 950 lines), index advice across
pg_stat_statements, the tap and hypopg, and schema diff of CHECKs,
views and functions.

### 1b. A backlog pass (2026-11-13/14)

Tom's "continue with the backlog", on main, unreleased: the sessions
panel's failed load, `I`/`:fixture` on derived sources and along the
search path, MCP's read-only hint, a refused cell save, the result
diff's NULL keys, quoting, partial sides, stale pin and column order,
`copies prune`, the `EXPIRED` badge, the rules digest and the rules
confirmation's changes in the journal, schema diff of CHECKs, views and
functions, MCP `schema_lint`'s advisor and `\report`'s evidence, ADV002
and ADV003, the bounded name lookup, a standby's pin, Log4j's bracketed
logger, the wizard inside a transaction, multi-column `JOIN … ON`,
sort and filter after an edit, `Ctrl-C` in row detail, and classifier
noise (measured secrets, `store_fname`, a pay flag). Four items were
killed by evidence (archive.md).

Left, each needing Tom or a design: the libpq environment, Ctrl-A /
Ctrl-E, the main.rs split, index advice, the MCP confirmation's
classifier summary (it would connect before the operator agrees), the
TUI's patch lift, discovered names as dump targets, paging limits'
home (config.toml or pgman.toml), and the classifier's table-aware
words (`amount_tax`, `license`, `client`).

### 1c. The 0.5.3 release panel (2026-11-14)

Five Fable reviewers on 4f4f4b1: safety, masking, TUI correctness, a
hand-driven session against a real server, and PG 10/12/17. Each
verdict was GO-with-fixes. Fixed on main, one commit each: `pgman
query --target` writing to a protected environment under `--yes` (and
without `--primary`), `copies prune` deleting a dump a later one
wrote at the same path, two masking loosenings (a bounding box's
edges, `local_fname`), a dry run reporting invented schema changes
after a read cut short, `I` / `:fixture` after `SET search_path`, the
wizard inside a typed `BEGIN`, a temp table's cell edit, the
completion popup's garbled rows, the DNS cache keeping a timeout for
good, schema diff across majors and a table become a view, and five
smaller ones. What's left is in BACKLOG.md → "From the 0.5.3 release
panel", the masking classifier's pre-existing gaps first.

### 2. Waiting on Tom

- **Release 0.5.3**: on Tom's go.
- **pgman-tap JAR**: go or not yet; `io.poly` groupId; loopback-only
  default with `pgman.tap.allow-remote`. Wire-compatible with 0.2.x (78
  tests, 9/9 golden events) but no publishing setup (Central account,
  namespace DNS record, GPG key, repo, secrets).
- **Required CI checks on main**: branch protection blocks force-push
  and deletion but requires no status checks (checked 2026-10-24), so
  a PR can merge red. Setting them is a permission grant: Tom runs
  `gh api -X PUT …/branches/main/protection` with `ci.yml`'s job names.
- **The client's masking rules file**: review the branch, confirm the
  file at a terminal (`pgman dump` asks), push and PR it in the client's
  repo; decide whether CI runs `masking check`.

## Deferred

- Publishing the `pgman-tap` JVM library: see Window step 1.
- Mutation testing (`mutants.yml` / `scripts/sweep.sh`).
- `unwrap_used` / `expect_used` deny lints.
- `cargo-semver-checks`: meaningful now that 0.1.0 and 0.2.0 are on
  crates.io, though the library is not a public API yet.
- Everything in `BACKLOG.md → Open` that is a feature.
