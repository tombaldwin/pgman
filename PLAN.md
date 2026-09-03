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

## Where we actually are (measured 2026-09-02)

pgman has **never shipped**. `CHANGELOG.md` records "[0.1.0] — 2026-06-06,
first public beta" and the README wears a public-beta badge, but the
GitHub repo is **private**, has **no tags** and **no releases**, the
crate name is **unclaimed on crates.io**, there is no release workflow,
formula or binary, and `pgman --upgrade` only works from a local
checkout. The 0.1.0 "release" was a version-bump commit (`9b91375`).

In good shape: 1028 unit tests + 8 integration-style suites, all green
locally; CI green on main; 16 render snapshots; property tests, fuzz
targets, benches; candor data-layer gate; cargo-deny; dependabot;
SECURITY / CONTRIBUTING / CoC / issue templates.

Red or soft:

| gate | state |
|---|---|
| cargo-deny | green since Phase 0 (was red on a yanked `chacha20`) |
| clippy | clean, denied in CI since Phase 0 |
| MSRV | 1.94.1 declared and gated (build + test) since Phase 0 |
| README `demo.gif` | referenced, **not in the repo** — broken image |
| BACKLOG.md | 2467 lines, ~1570 Done; Open carries items already marked done |

### Polish defects visible in the committed snapshots alone

These are in `tests/snapshots/*.snap` today — nobody has to run the
app to see them. Several are the exact bugs ebman fixed in 0.36.0
*after* release; here they can be fixed before.

- **Help overlay wraps continuations to the overlay's left margin**
  ("doesn't lose the session)", "matches)", "redo" on their own lines
  with no key beside them). ebman `3784298`.
- **Footer key strip clips mid-hint** at 80 columns: `… T slow · L se`,
  `… Q s`. ebman `f9b87c1`. Shed whole hints, never half of one.
- **Sessions panel misaligns** when the state is `idle in transaction`
  — the value is wider than its column and the row shears.
- **Slow-queries panel divider** is drawn with `│` at both ends instead
  of `├ ┤`, so it does not join the border.
- **Schema wizard truncates mid-word with no ellipsis**
  (`table without PRIMARY KEY or UN`).
- **Completion popup's border collides with the result panel's** —
  `┌ 2 matches · Tab to┐──────` sits on top of the panel's top edge.
- **Landing screen is two empty boxes** — `(empty — press e to focus)`
  over `(no rows)`. k9s lands you in a populated view. Land in the
  schema browser, or a summary (db, size, top tables, connections),
  and make the editor one keypress away.
- **Connection picker says "no connection — start pgman with --dsn"**
  directly above a picker offering two connections.
- **Splash has a 3-second minimum** (`app.rs:1022`). The ebman survey
  in the backlog said adopt the rendering but *not* the minimum;
  keypress dismisses, but the demo tape still waits 4.5 s for it.

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

## Hands-on UX round — 2026-09-03 · **in progress**

Tom ran the binary in a real Spring project (uflexi) and the first
minutes produced what no reviewer had: F5 is wrong on a Mac, the
editor looked like a one-line prompt, tabs were invisible, there was
no way to resize or maximise a pane, and formatting needed an external
binary. Landed since: **Enter runs a `;`-terminated statement**
(psql's rule) with Alt-Enter as the unterminated escape hatch; the
editor **opens five lines** and grows to 40 % of the body; **zoom and
manual sizing** per tab; an **always-visible tab bar** labelled by each
tab's query; **built-in Ctrl-F formatting** (`sqlformat`, `pg_format`
when installed, never on run or paste, and it refuses rather than
mangle a dollar-quoted body) and **auto-indent**.

In flight: Alt chords are unusable in iTerm, so the primary bindings
become plain keys outside typing modes — `] [` and digits for tabs,
`z` / `Z` zoom, `+ - 0` size — with `ctrl-]` from the editor, on-screen
hints for all of them, and `J` / `K` stepping rows in the row-detail
view while keeping the highlighted field.

**The lesson, recorded in memory**: five reviewers and four rounds did
not find any of this. Fifteen minutes of the maintainer using the tool
for its actual purpose did.

## Release day

```
git tag v0.1.0 && git push --tags       # after CI is green on the SHA
gh release edit v0.1.0 --draft=false    # after checking the 4 tarballs
scripts/update-formula.sh v0.1.0        # commit + push both repos
brew tap tombaldwin/tap && brew install pgman
```

## Explicitly deferred past the release

- `pgman-tap` JVM library (separate repo, 1–2 weeks). The tap panel's
  "Route 2" hint stays flagged in-development; OTLP is the documented
  route.
- Mutation testing (`mutants.yml` / `scripts/sweep.sh`).
- `unwrap_used` / `expect_used` deny lints.
- `cargo-semver-checks` — meaningful once 0.1.0 is on crates.io.
- Everything in `BACKLOG.md → Open` that is a feature.
