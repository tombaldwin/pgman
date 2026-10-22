# Plan — first real release

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

## Where we are (2026-10-22)

pgman is **public but not released**. The GitHub repo is public, with
its history scrubbed of client details, and PRs #1–#14 have merged since
it was recreated. There are **no tags and no releases** yet, the crate
name is **unclaimed on crates.io**, and no Homebrew formula has been
published. The machinery for all three is in the repo: `release.yml`,
`Formula/pgman.rb` with placeholder shas, `scripts/update-formula.sh`.
Until the tag, the only install is `cargo install --git`, which
`--upgrade` can't upgrade in place (the README says so).

Branch protection on `main` blocks force-push and deletion but requires
**no CI checks**; requiring them is a post-release item (Tom's
decisions, below).

The **v0.1.0 release panel ran on 2026-10-22**, and its fixes are in
progress on `release-panel-fixes` (see the section below). After they
land, the next step is Release day.

The September state this section used to describe (a private repo, no
release workflow, a broken README image, nine snapshot-visible polish
defects) was fixed by Phases 0–4 below; the detail is in git history.

---

## Phase 0 — make CI honest · mechanical · **done 2026-09-02**

Landed as six commits (`e096403`..`83a5b00`): CI renamed and hardened,
clippy cleared and denied, MSRV 1.94.1 gated, machete added, deps
unyanked, webpki-roots 1.0, CLAUDE.md lifted. Not yet pushed, so CI has
not run the new workflow; the first push is the verification. The
dependabot PR #17 closes itself on that push.

What it was:

1. `cargo update -p chacha20`; merge dependabot #17 (webpki-roots 1.0 —
   it failed on deny, not on build; re-check after the update).
2. Fix the 31 clippy warnings; flip CI to `-D warnings`. Pin the
   fmt/clippy toolchain (ebman: `dtolnay/rust-toolchain@1.96.0`).
3. Declare `rust-version`; add ebman's `msrv` job (build **and** test)
   and `cargo-machete`. `permissions: contents: read`.
4. Rename `test.yml` → `ci.yml` (the ported release gate greps for it);
   update the README badge.
5. CLAUDE.md / CONTRIBUTING: the green gate is `cargo fmt --all` →
   `cargo clippy --all-targets -- -D warnings` → `cargo test`. Lift
   ebman's CLAUDE.md upgrades that apply: separate commits for review
   fixes, "widening an allowlist is a stop condition", PLAN / BACKLOG
   split, Done → archive.

## Phase 1 — the product bar · behaviour · **done 2026-09-02** (gif deferred to Phase 4)

Landed as 24 commits after Phase 0. All nine snapshot-visible defects
fixed with pinning snapshots, plus three the sweep found (confirm modal
and "blocked by safety" showed Rust enum syntax; schema-browser tree
bled into its details; status lines clipped mid-word). The size sweep
(`tests/sizes.rs`, 36 screens × 4 sizes) is a CI gate with an empty
allowlist. The start card is the real landing screen and the bootstrap
feeds it. The wedge is discoverable: pasted logs are recognised, the
card names F8 and F4, and `--log PATH` opens straight into the picks.
Keyword completion and `\l \x \c \i` close the psql gaps the audit
found. Error lines say what to do next.

Still open from this phase: the sixty-second wedge recording (needs
`vhs`, lands with the README in Phase 4) and a timed first-minute run
against a real database (do it once the update check exists in Phase
3, since that is the one thing that could delay the first frame).

What it was:

1. **Fix the nine snapshot-visible defects above.** Each gets a
   snapshot that pins the fix.
2. **Size sweep.** Every screen and overlay in `--demo` at 80×24,
   100×30, 120×40, 200×50: nothing truncates a value without an
   ellipsis, no overlay wider than the terminal, no hint clipped
   mid-word, no border collision. ebman's snapshot-at-every-size
   harness (`9a0b9ca`) is the model — port the approach, not the file.
3. **First-minute path, timed.** `cd spring-app && pgman` → picker →
   connected → something useful on screen. Target under five seconds
   on a local DB and no blank frame at any point; the splash must
   never delay the picker. Then `?` must read as a keymap, not a wall.
4. **The wedge in sixty seconds.** One end-to-end that sells the
   product and works flawlessly in demo mode: paste a Hibernate log
   fragment → reconstructed, runnable SQL → run it → N+1 cluster
   flagged. That flow is the README gif, the product page hero and
   the first thing in `docs/`. If any step is awkward, that is the
   highest-priority bug in the repo.
5. **Table-stakes audit against pgcli / psql.** Not to match them, but
   so nothing basic is *worse*: completion latency, `\d`-family
   coverage, transaction state visibility, error caret position,
   paste of a multi-statement script, Ctrl-C cancel. List what is
   worse; fix what is cheap; document the rest as known.
6. **Error surfaces.** Every "actionable error" path is tested; check
   that the *text* is actually actionable (what happened, what to do,
   one line each) and that the F2 detail overlay is discoverable from
   the footer when there is an error.

## Phase 2 — release machinery · mechanical · **done 2026-09-02**

Ported from ebman in six commits: `release.yml` (CI-green gate, four
targets, attestation, draft release, crates.io on publish, minus the
MCP registry job), `Formula/pgman.rb` with placeholder shas,
`scripts/update-formula.sh` (creates the tap entry on first run),
`panic = "abort"` and a crate `exclude`, `build.rs` reading the release
date out of CHANGELOG.md into the About overlay, `docs/development.md`.
The crate packages at 276 files / 600 KiB. **Not yet done, needs you:**
`gh secret set CARGO_REGISTRY_TOKEN` on the pgman repo (ebman has one).

What it was:

1. `.github/workflows/release.yml` from ebman, `s/ebman/pgman/`, minus
   the `mcp_registry` job. Keep: `ci-green` gate, 4 targets, provenance
   attestation, draft release, `crates_io` on `release: published`.
2. `Formula/pgman.rb` + `scripts/update-formula.sh` (no `curl` dep;
   the `test do` matches `pgman <version>` — the ` · beta` suffix in
   `--version` still matches). Tap entry in `../homebrew-tap`.
3. Cargo.toml: `exclude = ["target", "fuzz"]`, `rust-version`.
   `panic = "abort"` is safe (`tui.rs` only uses `set_hook`) but optional.
4. Optional: ebman's `build.rs` release-date-from-CHANGELOG for About.
5. `CARGO_REGISTRY_TOKEN` repo secret before the first tag.

## Phase 3 — an upgrade story that survives a binary install · behaviour · **done 2026-09-02**

Three commits: `update_check.rs` (channel detection from the executable
path, crates.io `max_stable_version` via reqwest with a 10 s timeout,
a six-hour cache under the cache dir, all pure parts tested); the check
spawns only after the first frame is drawn and is off for `--demo`,
`--batch`, `--no-update-check` and `PGMAN_NO_UPDATE_CHECK`, with a test
that pins the order; a `⬆ x.y.z` header badge and two About lines
(channel, update command); `--upgrade` runs the right command for
checkout, cargo and Homebrew installs and points standalone binaries
at the releases page. reqwest adds ~170 transitive crates; deny and
machete stay green. Also this phase: a history scan found one fixture
lifted from a real project (a password-shaped value and a project's
database names) — neutralised at HEAD; **the history rewrite is a
maintainer decision, see the session summary.**

What it was:

1. Port ebman's `update_check.rs`: `InstallChannel` from the executable
   path, crates.io `max_version` poll, per-channel upgrade command,
   surfaced in an overlay rather than a status line.
2. `--upgrade`: keep the checkout path when `CARGO_MANIFEST_DIR` is a
   real git tree; otherwise print the channel's command and exit
   non-zero.
3. Take `reqwest` (rustls-tls) as ebman does; the check is opt-out,
   time-boxed, and never blocks the first frame.
4. Tests: channel detection, version comparison, response parsing,
   unreachable → silent.

## Phase 4 — docs and going public · behaviour · **done 2026-09-03**

Landed: seven docs pages and ARCHITECTURE.md, the README for the
binary-install era, the demo re-recorded end to end (start card →
paste → reconstruct → run → rows, 765 KiB), BACKLOG.md groomed to open
items (2467 → 649 lines, Done in `docs/backlog/archive.md`), the 0.1.0
entry annotated. Found and fixed on the way: files were not owner-only
(now 0600/0700); unresolved Spring placeholders became hostnames (now
resolved from the environment or refused with a message); the in-app
help lacked two sections; four house conventions had no guard (now
`tests/guards.rs`); `--demo` could not run a query; the JDBC paste
route was dead code the README advertised. History scan: one fixture
lifted from a real project, neutralised at HEAD.

Done since: history rewritten (a client fixture removed from 214
commits) and force-pushed; `CARGO_REGISTRY_TOKEN` set; the repo is
public with `main` protected against force-push and deletion.

**Remaining, and yours:** hand the product page the hero gif, the
install commands and the GitHub link; drop "Coming soon". Then
Release day below.

What it was:

1. **CHANGELOG**: the first tag is **v0.1.0** (never published, so no
   bump); on release day fold Unreleased into `[0.1.0] — <date>`.
2. **README**: regenerate `demo.gif` from the Phase 1 wedge flow;
   rewrite Install (brew tap / cargo install / tarball) and Upgrade.
3. **docs/**: `keys.md`, `commands.md`, `configuration.md`,
   `safety-and-privacy.md` (what is stored locally, TLS semantics,
   tap listeners), `development.md`, and a `logs-to-sql.md` walkthrough.
4. **ARCHITECTURE.md** module map.
5. **BACKLOG grooming**: Done → `docs/backlog/archive.md`; strip done
   items out of Open.
6. **Secrets scan** of full history, then flip public, branch
   protection, required checks. Flip *before* tagging: the arm64 Linux
   runner and attestations are for public repos.
7. Hand the product page what it needs: hero gif, install command,
   GitHub link, drop "Coming soon".

## Release panel — 2026-09-03 · **Fable panel: GO WITH FIXES, last batch landing**

Five Opus reviewers ran four rounds (three NO-GOs, ~60 findings, all
fixed with guards demonstrated failing). Then a four-reviewer Fable
panel on the same angles found what Opus had missed: the placeholder
resolver and the DSN parser disagreed on where a host starts; the
read-only floor was liftable by `set_config`, a quoted GUC, `DO`, or a
mid-script `COMMIT`; and, driving the release binary against a live
server by hand, a 100 % CPU hang on the first guarded write, `?` for
every non-text column type, and quote autoclose corrupting typed
literals. All fixed with tmux captures before and after. The product
reviewer's second pass: fourteen of fifteen verified, one partial (a
second site of the "blames a safety.toml that does not exist" hint),
six tolerate-level items — the final batch in flight. Lesson recorded
in the memory note: a review that only reads code and snapshots misses
the first five minutes against a real server.

## Done since the release panel (2026-09-03 → 2026-10-20)

Collapsed from their working notes; the detail is in git history and
CHANGELOG.md.

- **Hands-on UX round** (2026-09-03): Enter runs a `;`-terminated
  statement, a five-line editor, zoom and sizing, an always-visible tab
  bar, built-in formatting, plain-key bindings for iTerm (`] [`, digits,
  `z`, `+ - 0`) with on-screen hints.
- **MCP server, Parts A and B** (2026-10-19): `doctor`, `connections`,
  `query`, `sessions_locks`, `pending_patches`, Secrets Manager
  credentials, `primary` / replica roles. Smoke-tested read-only against
  examplecorp local and demo; Prod untouched.
- **Environments** (2026-10-19): the `E` overview as home, `rds` /
  `static` / `docker` sources, connect from a row, AWS discovery (TUI,
  CLI, MCP), opt-in probes through replicas with one ssh per bastion,
  the backlog batch (bold headers, tunnel cache, auto-probe, docker).
- **Engineer batch** (PR #42, 2026-10-20): upgrade targets, migration
  tools, blocked/long sessions, lag trend, drift (`v`), RDS log errors
  (`l`), `schema_lint`, and applying patches (`P`, `apply_patches`)
  after a Fable safety review whose eleven findings were each fixed in
  their own commit.

## Robustness pass — 2026-10-20 · review · **done** (`robustness-pass`, then `robustness-ux`)

Tom: "anything else you can do for a few hours to make things as useful
and robust as possible. Maybe a review?" Four Fable reviewers, read-only,
in parallel: a hand-driven tmux session (`--demo` and the test Postgres),
MCP security, environments correctness, and robustness plus docs drift.
Findings are verified, fixed one per commit with a CAUGHT where a guard
is added, and anything deferred goes to BACKLOG.md with a reason.

Landed: two security HIGHs (`U&"…"` escaped names past the classifier,
reproduced against a real server; MCP results without a byte budget),
replica lag on a quiet primary, `--batch`'s silent 1000-row cap,
`:connect` echoing a password, `:readonly` lying about the live session,
conflicting `sslmode`s, a 10 s connect timeout, Hibernate `show_sql`
lines, find eating `n`, Flyway baselines, cursor-by-name on refresh,
probes keeping their last reading, the demo's dead panels, and more.
A streaming standby now runs in the test compose (and CI uses it).

Then the backlog it left (Tom: "do 1-6", branch `robustness-ux`): plain
keys for F2–F4 and a second press for `ctrl-c` / `ctrl-w`, a `WATCH`
badge, stale errors cleared by later successes, a schema reload, `\d`
that says when nothing matches, the two completion bugs, the small-UX
list, IPv6 DSN hosts, a capped `\i`, a working `--log -`, and the seven
environments findings (two forks in worktrees took the last two).
Killed by evidence: "no grid cursor after a run until Esc" — the row
highlight and the header's column cursor are there in the editor and
after Esc alike (checked in tmux against `--demo`). The few that
remain are under "From the robustness pass" in BACKLOG.md.

## Backlog pass — 2026-10-21 · behaviour · **done** (`backlog-work`)

Tom: "work through the backlog". The tractable open items, in three
worktree batches (polish, correctness, schema / editor) plus the main
line; the detail is in CHANGELOG → The backlog pass and the archive.
Every guard has a CAUGHT.

Killed by evidence: `Q` / `/` silent no-ops, the tap memo counter,
`:about`, IntelliJ local passwords (see the archive). Skipped, each
with its reason in BACKLOG.md: Flyway `classpath:` (wants a
per-environment `project` key), TextInput adoption (architecture),
copying a live database (Tom's decision). Not attempted, as features
that each want a design note: cell editing, paging past `MAX_ROWS`,
per-tab connections, keymap customisation, the help overlay's
sections, the JDBC tap layers, v2.

## v0.1.0 release panel — 2026-10-22 · review · **fixes in progress**

A release panel ran on 2026-10-22 against the public repo. Its findings
are being fixed one per commit on `release-panel-fixes`, in parallel
worktrees: code fixes in some (the first to land blocks server-control
functions on a read-only profile), text in another (the unbuilt pgman-tap library no longer presented as
installable, the MCP write surface described as it is, the `--git`
install's upgrade caveat, `beta` used throughout for maturity, the
1000-row grid cap documented, `demo.gif` out of the crate). Anything
deferred goes to BACKLOG.md with a reason.

## Release day

From the release-risk reviewer. In order; each step waits on the one
before it.

0. README banner, badge, Install and Upgrade sections; docs/development.md
   "Not published yet" (the Distribution list); the `Formula/pgman.rb`
   header comment: all flipped to released, in one commit.
1. CHANGELOG `## [Unreleased]` → `## [0.1.0] — 2026-10-DD` (em dash,
   ISO date; the release workflow greps for it exactly). Add a new empty
   `## [Unreleased]` above it, and keep the link refs.
2. Check the crates.io token's scope is publish-new: the crate doesn't
   exist yet.
3. Land it via PR, and wait for `ci.yml` to go green on the merge SHA on
   `main`.
4. Run `cargo publish --dry-run --locked` and `cargo package --list` on
   that SHA.
5. `git tag v0.1.0 <sha> && git push origin v0.1.0` — not `--tags`.
6. Watch `release.yml`: a draft release with 4 tarballs, `.sha256` files
   and attestations.
7. `gh attestation verify` on one tarball; run `--version` and check the
   About date.
8. `gh release edit v0.1.0 --draft=false` publishes to crates.io. If that
   fails: `gh workflow run release.yml -f tag=v0.1.0`.
9. `cargo install pgman --locked` in a clean dir; check the crates.io
   page renders.
10. `scripts/update-formula.sh v0.1.0`, push the tap, PR the in-repo
    formula, and run `brew install` / `brew test`.
11. The product page.

## Tom's decisions — 2026-10-22

Post-release, each an item in BACKLOG.md → "After v0.1.0":

- Require CI checks on `main`. Branch protection has none today.
- Pin GitHub Actions to commit SHAs.
- The two TLS backends in the build (aws-lc-rs and ring): settle on one.

## Explicitly deferred past the release

- `pgman-tap` JVM library (separate repo, 1–2 weeks). It is planned,
  not built: the tap panel's "Route 2" says not yet released, and
  OTLP (`--tap-otlp` with the OpenTelemetry Java agent) is the
  documented route.
- Mutation testing (`mutants.yml` / `scripts/sweep.sh`).
- `unwrap_used` / `expect_used` deny lints.
- `cargo-semver-checks` — meaningful once 0.1.0 is on crates.io.
- Everything in `BACKLOG.md → Open` that is a feature.
