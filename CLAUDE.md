# AI working rules for pgman

This file is read by Claude Code (and similar agents) on session start. Follow it.

## What this project is

`pgman` is a Rust + ratatui TUI for PostgreSQL, aimed at Java / AWS shops.
Sibling to `ebman` (AWS Elastic Beanstalk TUI). Source under `src/`. Backlog in
`BACKLOG.md`. Tests live alongside the code in `#[cfg(test)] mod tests` blocks.

The crate is **lib + bin**: `src/lib.rs` exposes the modules, `src/main.rs` is a
thin binary. Keep logic in the library so it stays unit-testable; `main.rs` only
wires args, logging, and (later) the TUI loop.

## Mandatory loop for autonomous work

When the user asks for autonomous work ("run autonomously", "build the next
milestone", "next", or any directive to ship multiple items without per-step
approval), you **must**:

1. **Build green before claiming done.** `cargo build` must succeed with no new
   warnings. `cargo test` must pass. Fix failures before moving on.
2. **Self-review every meaningful change.** After each substantive feature,
   review your own diff for bugs, dead code, missed edge cases, and
   inconsistencies. The review goes in your message to the user.
3. **Act on review findings — don't just list them.** Anything that is a bug,
   inconsistency, dead code, or a tightenable design choice must be fixed in the
   same turn, unless the user has explicitly deferred it.
4. **Add tests for new pure logic.** Any new parser / classifier / formatter /
   pure helper needs `#[cfg(test)]` tests covering happy path and failure modes.
   Extract pure logic out of UI/event handlers to make it testable.
5. **Update `BACKLOG.md`** when items move pending → done or new items appear.

## Stop conditions — skip and continue, don't halt

Skip the item, move to the next, and record the skip in the final summary if you
hit: a destructive action that wasn't pre-authorised; a refactor touching more
than ~3 modules and not clearly required; a design trade-off with no obvious
winner; the same compile error failing twice; any other hard blocker.

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

Code compiles with no new warnings; all tests pass; new pure logic has tests;
`BACKLOG.md` reflects the change; the final message lists what shipped, what was
reviewed-and-fixed, what tests were added, what was skipped (one-line reasons),
and any follow-ups deferred.

## When not in autonomous mode

When the user drives step-by-step, prefer brief recommendations over large
changes. Keep `cargo build` and `cargo test` green at every commit point.
