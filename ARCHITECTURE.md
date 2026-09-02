# Architecture

A map of the codebase for anyone reading it for the first time — where things
live, how a keystroke becomes a query, and the handful of rules that aren't
enforced by the compiler.

For working rules see [`CLAUDE.md`](CLAUDE.md); for the milestone plan see
[`BACKLOG.md`](BACKLOG.md).

## Shape of the crate

`pgman` is lib + bin. `src/lib.rs` lists the public modules; `src/main.rs` is
a thin binary that parses argv (`clap`), sets up logging, resolves the
connection (`--dsn`, project/IntelliJ/Spring discovery, auto-pick), wires up
the JDBC-tap listeners, and either dispatches a headless subcommand
(`--upgrade`, `--batch`) or enters the TUI.

```
src/main.rs      argv, logging, tap-listener wiring, TUI entry
src/lib.rs       module list
├── app/         the TUI: state, event loop, everything it can do
│                (app.rs is the impl block + App struct; app/*.rs
│                are `impl App` split-outs — see below)
├── ui/          rendering only — `draw(f, app)` is the one entry
│                point; one module per surface (editor / landing /
│                panels / results / schema / tap); ui.rs dispatches
├── query/       pure parsing: reconstruction, completion, lint,
│                schema, highlighting, safety-adjacent classifiers
├── tap/         JDBC-tap ingest — wire format, TCP/UDP/OTLP
│                listeners, replay, and the L2 insights aggregator
├── creds/       discover connections from IntelliJ / Spring configs
└── ...          conn.rs (the DB boundary), safety.rs (the write
                 gate), grid.rs, saved.rs, dbunit.rs, report.rs,
                 batch.rs, demo.rs, splash.rs, theme.rs, tunnel.rs,
                 upgrade.rs, update_check.rs, util.rs, project.rs
```

Unlike a wide-public-API crate, pgman doesn't gate its module surface with
`unreachable_pub` — most of `app`, `ui`, and `query` is `pub` because tests
and `tests/*.rs` integration suites construct `App` directly and drive it
through `on_key` / `run_with`. The boundary that *is* enforced is narrower and
different: which modules may touch the database at all (see "Invariants"
below).

`src/app.rs` (3782 lines) holds the `App` struct, `Mode`, the event loop
(`run_with`), and everything not split into a submodule. Its `mod` block at
the top names the split-outs:

```
mod cmd;      // `\`-command dispatch (dispatch_backslash + \c/\i/\report/\l bodies)
mod editor;   // paste handling, external-editor round trip, buffer mutators
mod handle;   // on_event, on_msg (AppMsg → state), apply_bootstrap_grid
mod history;  // Ctrl-P/Ctrl-N history navigation
mod keys;     // on_<mode>_key handlers — one per Mode variant, 1282 lines
pub mod msg;  // AppMsg — every spawned-task result, generation-tagged
mod spawn;    // every tokio::spawn call: connect, run, schema refresh, …
mod tabs;     // per-tab state snapshot/restore
mod types;    // supporting structs/enums lifted out of app.rs verbatim
mod yank;     // clipboard export (row-as-INSERT, notification payload, …)
```

`src/app/tests.rs` (5472 lines) is the bulk of the test suite — unit and
journey-style tests against `App::for_tests`-shaped fixtures, one function
per behaviour.

## Where to start reading

1. **[`src/app.rs`](src/app.rs)** — the `App` struct (from line 806) and
   `run_with` (the event loop, from line 1217). Read the struct's doc
   comments; nearly every field explains *why* it exists, not just its type.
2. **[`src/app/keys.rs`](src/app/keys.rs)** — the keymap, split into one
   `on_<mode>_key` function per `Mode` variant. Follow any key you care about
   from `on_normal_key` or `on_editor_key`.
3. **[`src/safety.rs`](src/safety.rs)** — `classify` → `evaluate` → `Guard`.
   Every statement the editor can run passes through here first.
4. **[`src/ui.rs`](src/ui.rs)** — `draw(f, app)`, the render dispatcher. Its
   module doc lists the per-surface submodules; `draw` picks the layout for
   the current `Mode` and hands each region to the matching `draw_*`.
5. **[`src/conn.rs`](src/conn.rs)** + **[`src/query/`](src/query)** — the
   data layer. `conn.rs` is the connection/DSN/transaction primitives;
   `query/*` is every pure parser/classifier that doesn't need a live
   connection (only `query/explain.rs::run_cost_explain` and `query/schema.rs`
   / `query/lint.rs`'s catalog fetchers touch the network).

## `query/*` — one line each

- `backslash` — psql-style `\` command parsing (`\c`, `\i`, `\d`, `\watch`, …).
- `clause` — clause-context classifier (are we after `SELECT`, inside `FROM`,
  …) for grammar-aware completion.
- `complete` — SQL identifier completion (3044 lines — the biggest query
  module; keyword/table/column candidates plus the Tab-cycle logic).
- `explain` — parses `EXPLAIN (FORMAT JSON)` into a `PlanNode` tree; also
  the one query module with a live-query helper (`run_cost_explain`).
- `from_parse` — best-effort `(schema?, table, alias?)` extraction from
  `FROM`/`JOIN`, even on an incomplete buffer.
- `hibernate` — reconstructs runnable SQL from application-side Hibernate
  logs (`?` placeholders + bind-parameter log lines).
- `highlight` — pure SQL syntax highlighter for the editor.
- `jdbc` — reconstructs runnable SQL from pasted JDBC (SQL + typed param list).
- `json_cell` — tree-shaped view of a JSONB cell for the CellDetail navigator.
- `lint` — the schema wizard (`W` key): pure checks over `SchemaCache`.
- `logdetect` — sniffs pasted/buffer text for log framing so the editor can
  suggest Ctrl-L / F8.
- `nplus1` — clusters reconstructed queries by shape to surface N+1 selects.
- `params` — named-placeholder (`:name`) handling for saved-query prompts.
- `pglog` — reconstructs runnable SQL from Postgres/RDS server logs (the
  primary reconstruction source; `$N` placeholders + `parameters:`).
- `reconstruct` — the shared output type (`ReconstructedQuery`) every
  reconstruction source produces. Don't fork it.
- `row_diff` — result-diff: compares two grids row-by-row for `Mode::ResultDiff`.
- `schema` — the `SchemaCache` completion/browser catalog, built at connect.
- `select_list` — best-effort column-name extraction from a SELECT statement.
- `sessions` — `pg_stat_activity` + `pg_blocking_pids()` parsing.
- `slow_queries` — `pg_stat_statements` inventory parsing.
- `subst` — type-aware placeholder substitution shared by all three
  reconstruction sources.
- `vocabulary` — keyword/function/operator lookup tables for completion.

## `tap/*` — the JDBC ingest pipeline

`tap::TapEvent` is the wire shape; `tap::parse` turns one frame (UDP
datagram or TCP length-prefixed message) into an event. Async listeners live
in `tap/listener.rs` (TCP + UDP) and `tap/otlp.rs` (OpenTelemetry HTTP
ingest — an alternative to the pgman-tap JAR for any OTel-instrumented JVM).
`tap/replay.rs` feeds a captured JSONL file through the same pipeline for
demos and offline analysis. `tap/insights.rs` is L2: pure aggregation over
the in-memory event ring (hotspots, N+1 groupings, baselines). `tap/mod.rs`
(3117 lines) holds the wire types and the shared parse/format logic.

## How a keystroke becomes a query

```
crossterm event
  └─ App::on_event              app/handle.rs
      └─ App::on_key            app.rs        (global keys: F1/F2/F3, Ctrl-C, splash-dismiss)
          └─ App::on_editor_key app/keys.rs    (per-Mode dispatch; F5 here)
              └─ App::request_run              app.rs:2414
                  ├─ backslash intercept        query::backslash::parse_backslash_command
                  ├─ safety::evaluate           safety.rs   → Decision { kind, guard, wrap_in_tx }
                  │    ├─ Guard::Block   → last_error = blocked_by_safety_message(..)
                  │    ├─ Guard::Confirm → pending_run = Some(..); mode = Mode::Confirm
                  │    └─ Guard::Allow   → (cost-preview gate, then) spawn_run
                  └─ App::spawn_run                app/spawn.rs:232
                      └─ tokio::spawn { conn::execute(&client, sql, kind, &decision) }
                          └─ AppMsg::QueryOk { generation, grid, .. }   app/msg.rs
                              └─ App::on_msg                             app/handle.rs
                                  ├─ generation check (drop if stale)
                                  ├─ self.grid = grid; reset_grid_view()
                                  ├─ psql `\x` on?  → open_row_detail() → Mode::RowDetail
                                  └─ EXPLAIN kind?  → parse plan → Mode::ExplainTree
                                      (else stays in the grid)
              └─ ui::draw                        ui.rs   (next frame, driven by run_with's loop)
```

The loop itself is `App::run_with` (`app.rs:1217`): it `select!`s over
terminal events, the `AppMsg` channel (`msg_rx`), and one frame-clock
interval, mutating `App` and redrawing every iteration. Nothing in `spawn_*`
runs synchronously on this loop — every DB call is a spawned `tokio::spawn`
task that reports back as an `AppMsg`, so a slow query never freezes input.

A confirmed write (`Guard::Confirm`, operator presses the confirm key) still
goes through `spawn_run`; the difference is only that `request_run` parked it
in `pending_run` / `Mode::Confirm` first instead of calling `spawn_run`
immediately. A write with `decision.wrap_in_tx` (the profile's `auto_tx`)
leaves the transaction open on success — `QueryOk` sets `tx_open = true` and
`mode = Mode::TxDecision`, and the operator explicitly commits or rolls back.

Batch/multi-statement buffers (`safety::split_statements` finds more than one
statement) take a parallel path, `request_run_batch`, which classifies every
piece and uses the *most restrictive* guard across the batch.

## Invariants

The compiler won't catch you breaking these, so each has something else
behind it.

**Direct database access stays in the data layer.** `src/conn.rs` (the
connection/transaction primitives) and `src/query/*` (the query modules) are
the only places allowed to make a direct `Db` call — a UI/app function that
queries Postgres directly is an architectural leak. Enforced in CI by
[`ci/candor-check.sh`](ci/candor-check.sh) against `.candor/policy`
(`deny Db` outside those paths), using the stable `candor-scan` scanner.
Transitively reaching `Db` through a `conn`/`query` call is fine; only a
*direct* call outside the boundary is flagged. There are currently zero
documented exceptions (`ALLOW_FNS=""` in the script).

**Every executed statement passes through `safety.rs`.** `classify` → per-
database `Guard` (`evaluate`) → optional rollback-wrapped transaction
(`wrap_in_tx`). `App::request_run` / `request_run_batch` are the only
callers that reach `spawn_run` for editor-issued SQL, and both call
`safety::evaluate` first (`--batch` mode: `batch.rs` calls the same
`safety::evaluate` before executing). Grepping for a second, uncontrolled
`spawn_run` call site is the way to check this hasn't drifted — as of this
writing there is exactly one (`app.rs:2414`'s `request_run`, `app.rs`'s
`request_run_batch`, and the cost-preview follow-up all route through the
same `Guard` match).

**No hardcoded colours.** `src/theme.rs`'s module doc states it directly:
"UI code must read colours from a `Theme`; no hardcoded `Color::*`." `Theme`
carries every colour pgman uses (severity, status, chrome, syntax
highlighting, splash palette) as named fields; `ui/*` reads `app.theme.*`.
This is convention, not test-enforced — no `Color::` grep-guard exists in
`tests/`, unlike the DB-boundary check.

**No hardcoded paths.** `src/util.rs`'s `config_dir()` / `cache_dir()` /
`data_dir()` / `config_file(name)` resolve under `~/.config/pgman`,
`~/.cache/pgman`, and `~/.local/share/pgman` respectively — draft/history/
saved-queries persistence, `safety.toml`, and the log file all go through
these rather than a literal path.

**No `println!`/`eprintln!` in the running TUI.** The alternate screen
swallows them. `src/main.rs::init_logging` sends `tracing::*` output to
`~/.cache/pgman/pgman.log` (level via `RUST_LOG`, default `info`). CLI-only
paths (`--batch`, `--upgrade`, argument-parse errors) print directly by
design — they exit before the TUI ever opens. This, too, is convention-
enforced only; there's no grep-guard test over `src/app` / `src/ui` like
ebman's `no_tui_stdout.rs`.

**Credentials are never logged.** `conn::redact_url` masks userinfo and
password params before any DSN is written to `tracing` or shown in the UI —
callers log *provenance* ("project connection 'prod' → postgres://user:***@…")
rather than resolved secrets. `main.rs`'s Spring/IntelliJ discovery functions
consistently call `dsn.redacted()` or `conn::redact_url(..)` before any
`tracing::info!`.

**Ctrl-guarded match arms come before the unguarded arm for the same key.**
The clearest example is `App::on_editor_key` (`src/app/editor.rs`): every
`KeyCode::Char(c) if ctrl` arm (`r`, `e`, `a`, `l`, `d`, `w`, `f`, `x`, `s`,
`o`, `j`, `c`, `p`, `n`, `u`, …) is listed before the catch-all plain-typing
arm, `KeyCode::Char(c) if !key.modifiers.intersects(CONTROL | ALT)` — which
is itself guarded to explicitly exclude Ctrl/Alt rather than relying on arm
order to shadow correctly. No `syn`-based arm-order test exists in this repo
(unlike ebman's `key_arm_order.rs`) — this is currently convention plus the
explicit negative guard on the catch-all arm.

**One frame clock, multiple animation sources.** `App::run_with` owns a
single `tokio::time::interval(110ms)`; `App::wants_animation()` (`app.rs:1394`)
gates whether that tick's branch even fires, checking `splash_visible`,
`query_running`, `watch.is_some()`, `Mode::About`, `ConnState::Connecting`,
and auto-refresh-eligible panels. Nothing else spawns its own ticker —
splash, `\watch`, and panel auto-refresh all read the same `anim_tick`.

**The update check spawns after the first draw, once.** `run_with`'s loop
body calls `tui.draw(self)?` before checking
`self.update_check_enabled && !self.update_check_spawned`; the flag flips
immediately so a later loop iteration can't spawn it twice. This keeps the
very first frame from blocking on a network round-trip to crates.io.

**Footer text never clips mid-word.** `ui.rs`'s `fit_hints` (line 522) and
`fit_status` (line 608) truncate at hint/word boundaries rather than a raw
byte cut, appending an `F1 +N more` marker or an ellipsis as appropriate.
Pinned across four terminal widths by `tests/sizes.rs`, which renders every
`Mode` (plus a couple of `Normal`/`TapMonitor` sub-states) at each size and
snapshots the result — a clipped word shows up as an ordinary snapshot diff.

## Testing

Tests live beside the code in `#[cfg(test)] mod tests` blocks; `app`'s live
in [`src/app/tests.rs`](src/app/tests.rs) (one function per behaviour, 5472
lines). Pure logic — parsers, classifiers, the reconstruction sources, the
completion engine — is deliberately extracted into `query/*` so it can be
unit-tested with plain `&str`-in assertions, no `App` or connection required.

`tests/*.rs` layers on top:

- **`journeys.rs`** — drives an `App` through a sequence of `on_key` calls
  and asserts on resulting state; synchronous, no async runtime needed.
- **`runloop.rs`** — one level deeper: drives the actual `run_with` `select!`
  loop via `HeadlessTui` + a synthetic event channel, frame ticks included.
- **`render.rs`** — renders via ratatui's `TestBackend` and asserts on
  specific cells/colours/strings (not `insta`, deliberately — survives
  layout tweaks that a full snapshot wouldn't).
- **`snapshots.rs`** / **`sizes.rs`** — `insta` snapshot suites; `snapshots`
  is one fixed size per `Mode`, `sizes` sweeps every `Mode` across four
  terminal widths to catch clipping/collision regressions.
- **`crash_recovery.rs`** — the editor-draft auto-save survives a mid-edit
  panic, backed by `util::write_atomic`'s rename-based durability.
- **`properties.rs`** — `proptest` invariants (shrinks failures to a minimal
  repro; regressions recorded in `tests/properties.proptest-regressions`).
- **`subprocess.rs`** — the two real-subprocess paths (`$EDITOR` via `\e`,
  `pg_format` via Ctrl-F) against shell stubs in `tests/bin/`.
- **`integration.rs`** — against a real Postgres, gated behind
  `--features integration` (`docker compose -f docker-compose.test.yml up -d`).

`fuzz/fuzz_targets/` covers `dsn_parse`, `hibernate_parse`, `pglog_parse`,
`project_parse`, `safety_classify`, and `tokenize` — the parsers most
exposed to attacker- or log-shaped input. `benches/hot_paths.rs` benchmarks
the per-frame-hot pure functions (highlighting, completion) so a slowdown
there shows up before it costs frame rate.

`src/demo.rs` builds the synthetic, fully-populated `App` that
`pgman --demo` runs against and that `tests/sizes.rs` / `tests/snapshots.rs`
render from — schema cache, saved queries, tap-event ring, and result grid
are all realistic without a live database.
