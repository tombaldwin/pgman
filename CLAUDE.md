# AI working rules for pgman

This file is read by Claude Code (and similar agents) on session start. Follow it.

## What this project is

`pgman` is a Rust + ratatui TUI for PostgreSQL, aimed at Java / AWS shops.
Sibling to `ebman` (AWS Elastic Beanstalk TUI). Source under `src/`. Backlog in
`BACKLOG.md`. Tests live alongside the code in `#[cfg(test)] mod tests` blocks.

The crate is **lib + bin**: `src/lib.rs` exposes the modules, `src/main.rs` is a
thin binary. Keep logic in the library so it stays unit-testable; `main.rs` only
wires args, logging, and (later) the TUI loop.

## Where to start

`PLAN.md` is the rolling window of active work — read it first, work it
top-down, prune and refill it before finishing. `BACKLOG.md` is the
reservoir behind it. **An item lives in exactly one of the two.**

`PLAN.md` also carries the item classes (which stages an item needs) and
the verify-the-claim gate. This file stays what it has always been: how
to build, what green means, and the stop conditions.

## Mandatory loop for autonomous work

When the user asks for autonomous work ("run autonomously", "build the next
milestone", "next", or any directive to ship multiple items without per-step
approval), you **must**:

1. **Build green before claiming done.** All three, in this order:

   ```bash
   cargo fmt --all
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

   Not `cargo build`. It does not run clippy, and it does not compile
   test targets — which is where most of the recent work lives, so "no
   new warnings" from a bare build covers none of it. CI runs fmt and
   clippy and will reject what a build-only check passed.

   Two gates run in CI that you cannot easily reproduce and should think
   about before pushing, not after: **candor** (the `Db` effect — a
   query/exec/transaction call — must stay in `src/conn.rs` and
   `src/query/`; a UI/app function that reaches the database directly is
   an architectural leak, checked by `ci/candor-check.sh`) and
   **cargo-deny**.

2. **Self-review every meaningful change.** After each substantive feature,
   review your own diff for bugs, dead code, missed edge cases, and
   inconsistencies. The review goes in your message to the user.

3. **Act on review findings — don't just list them, but keep them
   separable.** Fix them in the same *run*; put them in *separate
   commits*. Mixing a feature, a couple of drive-by fixes and a refactor
   into one commit destroys `git bisect` on exactly the class of subtle
   regression this codebase keeps hitting. CONTRIBUTING.md asks
   contributors for a change that does one thing; the same applies here.

   The original rule still holds: anything you identify in a self-review
   that is a bug, an inconsistency, dead code, or a borderline-design
   choice that could be tightened *must be fixed in the same turn*,
   unless the user has been asked and has explicitly deferred it. "I
   noticed X but left it" is not acceptable in autonomous mode.

4. **Show every guard can fail.** A test that cannot fail is worse than
   none, because it reads as coverage. Any new guard or invariant test
   must be demonstrated to fail against the thing it guards: break the
   code the test claims to pin, watch it fail, restore the code, and put
   the `CAUGHT` line in your report. pgman has no `scripts/mutate.sh`
   yet, so do this by hand — but do it; a guard that has never been
   watched failing is unverified.

   Also add tests for new pure logic. Any new parser / classifier /
   formatter / pure helper needs `#[cfg(test)]` tests covering happy
   path and failure modes. Extract pure logic out of UI/event handlers
   to make it testable.

5. **Update `BACKLOG.md`** when items move pending → done or new items appear.

   It holds **open items only**. Completed work belongs in
   `docs/backlog/archive.md` — that file does not exist yet; PLAN.md
   Phase 4 creates it. Until then, Done entries stay where they are, but
   nothing new gets added to Open that is already done — `BACKLOG.md`
   already carries stale Done-shaped entries in Open and that is a known
   defect, not a pattern to repeat.

   And a follow-up recorded *inside* a completed entry is invisible —
   anyone scanning for open items will miss it. If work remains, it gets
   its own open item, not a sentence tucked into a done one.

## Stop conditions — skip and continue, don't halt

Skip the item, move to the next, and record the skip in the final summary if you
hit:

- **Widening an allowlist to make a guard go quiet.** When a guard
  fires, the fix is the code, not the list. Adding a RUSTSEC id to
  `deny.toml`'s `ignore`, reaching for `#[allow(...)]`, adding an
  exception to `.candor/policy`, re-accepting an insta snapshot without
  reading the diff, or adding `--yes` / a confirm-bypass in a test to
  make `safety.rs` stop refusing — all of it is a stop condition, not a
  step. Every guard has an escape hatch; every hatch is shorter than the
  fix; a one-line addition with a plausible reason produces a diff that
  reads as considered. That is exactly why this decision is not yours to
  make mid-run. Skip the item, say which guard fired and what it would
  have taken, and let the maintainer rule.
- A destructive action that wasn't pre-authorised.
- A refactor touching more than ~3 modules and not clearly required.
- A design trade-off with no obvious winner.
- The same compile error failing twice.
- Any other hard blocker.

The final message must explicitly list skipped items alongside what shipped,
what was reviewed-and-fixed, and what tests were added. Each skip needs a
one-line reason so the user can decide whether to retry or drop it.

## House conventions (don't re-discover by breaking)

- **Pure parsing lives in `parse(&str)` / classifier functions.** `query/*`,
  `creds/spring.rs`, `safety.rs`, `conn.rs` keep their logic pure and tested;
  I/O wrappers stay thin.
- **One shared reconstruction type.** Hibernate / Postgres-log / JDBC parsers
  all produce `query::reconstruct::ReconstructedQuery`. Don't fork it.
- **Safety is not optional.** Statements run from the editor go through
  `safety.rs`: classify → per-database `Guard` → optional rollback transaction.
  New statement-running paths must route through it.
- **No hardcoded colours.** Use `theme::Theme` fields. Hardcoded `Color::*` is a
  regression.
- **No hardcoded paths.** Use `util::config_dir()` / `util::cache_dir()` /
  `util::config_file(...)`.
- **No `println!` / `eprintln!` in the running TUI** — the alternate screen
  swallows them. Use `tracing::*`; output goes to `~/.cache/pgman/pgman.log`.
  (CLI-only code before the TUI exists may print.)
- **Never log credentials.** Resolved passwords/tokens must not reach `tracing`
  or the UI. Show *provenance* ("creds from application-dev.yml → SSM /…")
  instead of values.
- **Match-arm order matters.** Guarded `KeyCode::Char(..) if Ctrl` arms come
  before the unguarded arm for the same char.
- **One frame clock, multiple animation sources.** Splash, loading, and the
  dashboard each register interest in redraws; don't spawn independent tickers.

## What "done" looks like for each landed item

Code passes `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test`; new pure logic has tests; every new guard has a demonstrated
`CAUGHT`; `BACKLOG.md` reflects the change; the final message lists what
shipped, what was reviewed-and-fixed, what tests were added, what was skipped
(one-line reasons), and any follow-ups deferred.

## When not in autonomous mode

When the user drives step-by-step, prefer brief recommendations over large
changes. Keep `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`,
and `cargo test` green at every commit point.
