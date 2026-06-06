# Contributing to pgman

Thanks for your interest! pgman is a Rust + [ratatui](https://ratatui.rs) TUI
for PostgreSQL. Contributions of all sizes are welcome.

## Getting started

```sh
git clone https://github.com/tombaldwin/pgman
cd pgman
cargo build
cargo test
```

## Before opening a PR

The CI gates (see `.github/workflows/test.yml`) must pass, so run these locally:

```sh
cargo fmt --all                 # formatting (CI runs --check)
cargo clippy --all-targets      # lints
cargo test                      # unit + render + subprocess + doctests
```

For changes that touch live-database behaviour, also run the integration suite:

```sh
docker compose -f docker-compose.test.yml up -d
cargo test --features integration
docker compose -f docker-compose.test.yml down
```

## House conventions

A few rules keep the codebase consistent — please follow them:

- **Keep logic pure and tested.** Parsers / classifiers / formatters live in
  `parse(&str)`-style functions with `#[cfg(test)] mod tests` alongside them;
  I/O wrappers stay thin. New pure logic needs happy-path + failure tests.
- **Logic lives in the library** (`src/lib.rs` modules), not `src/main.rs` —
  `main.rs` only wires args, logging, and the TUI loop, so the rest stays
  unit-testable.
- **Safety is not optional.** Any new statement-running path must route through
  `safety.rs` (classify → per-database guard → optional rollback transaction).
- **No `println!` / `eprintln!` in the running TUI** — the alternate screen
  swallows them. Use `tracing::*`; output goes to `~/.cache/pgman/pgman.log`.
  (CLI-only / pre-TUI code may print.)
- **Never log credentials.** Show provenance ("creds from application-dev.yml")
  instead of values; log redacted DSNs only.
- **No hardcoded colours or paths.** Use `theme::Theme` fields and
  `util::config_dir()` / `util::cache_dir()`.

There's an optional effect-regression guard (`scripts/candor-guard.sh`,
powered by [candor](https://github.com/tombaldwin/candor)) that flags when a
function unexpectedly gains a network / filesystem / subprocess effect. It's
opt-in — it skips cleanly if you don't have a local candor checkout, so you
don't need it to contribute.

## Commit / PR style

- Conventional-commit-ish subjects are appreciated (`fix(tap): …`,
  `feat(editor): …`) but not enforced.
- Keep PRs focused; describe what changed and why. Note any follow-ups.

By contributing, you agree that your contributions are dual-licensed under
MIT OR Apache-2.0, matching the project license.
