# VHS tape for pgman's README hero gif.
#
# Render with:
#   brew install vhs        # one-time
#   cargo install --path . --locked --force   # rebuild the local binary
#   vhs demo.tape            # produces demo.gif next to this file
#
# Flow (~30s) — the wedge, start to finish: logs/pasted code → runnable
# SQL → rows:
#   type `pgman --demo` (the local binary is put on PATH off-screen
#   first) → settle on the start card (F8 / F4 named, no query run yet)
#   → e (editor) → clear the demo's canned query → paste a 5-line
#   Hibernate log sample (two binding-parameter lines, one repeated
#   select) → ctrl-l to reconstruct (same path as F8 — VHS 0.11 has no
#   function-key command, see below) → log-pick list (all queries) →
#   `c` for the N+1 cluster view → `c` back → Enter loads the top query
#   into the editor → ctrl-j to run it (same path as F5 — see below) →
#   rows land in the grid → quit.
#
# Targets `pgman --demo`, the synthetic-data mode baked into the binary,
# so there's no database and the frame is identical on every render.
# `--demo` has no real connection (`App::client` stays `None`), but
# request_run still runs the statement through the same safety::evaluate
# guard a live session would, then answers synthetically via
# demo::answer — a SELECT against a table in the fixture schema gets
# back plausible rows shaped by that table's real columns, delivered
# through the exact same AppMsg::QueryOk path a real client uses.
#
# VHS doesn't emit function keys or Ctrl+Enter, so F8 is driven via its
# `ctrl-l` alias and F5 via its `ctrl-j` alias (both documented in
# `docs/logs-to-sql.md` — see `App::on_editor_key_inner`,
# src/app/editor.rs).

Output demo.gif

# Terminal box: wide enough that the picker + editor don't truncate
# (~120 cols), tall enough for header + editor + grid (~30 rows).
# MesloLGS Nerd Font @ 14pt matches the glyph set pgman's Powerline
# chrome expects.
Set Width 1280
Set Height 720
Set FontSize 14
Set FontFamily "MesloLGS Nerd Font Mono"
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms
Set PlaybackSpeed 1.0
Set Padding 16

# Launch against the synthetic fixture. Put ~/.cargo/bin ahead of PATH
# so the locally-built binary (which has --demo) is what runs — but
# keep that plumbing off-screen: the hero should show the command a
# real user types, `pgman --demo`, not a dev's install path.
Hide
Type "export PATH=$HOME/.cargo/bin:$PATH"
Enter
Type "clear"
Enter
Sleep 200ms
Show
Type "pgman --demo"
Sleep 400ms
Enter

# Splash (minimum 600ms) + a beat to settle on the start card — an
# empty grid with the databases line and the F8 / F4 hints, the first
# thing a viewer sees now that --demo opens here instead of on a
# pre-run result.
Sleep 2000ms

# `e` focuses the editor — it still opens on the demo's canned SELECT
# (only the grid was cleared for the start card, not the draft buffer).
Type "e"
Sleep 700ms

# Clear it: the log sample below is what should be in the buffer when
# ctrl-l parses it, not a leftover query.
Ctrl+U
Sleep 400ms

# Type a 5-line Hibernate log sample: an order lookup, then an
# order_items lookup that repeats for a second order_id in the same
# burst — the N+1 signature nplus1.rs clusters on. Both tables are in
# the --demo fixture schema, so ctrl-j below returns real rows instead
# of the "no database" notice. Typed at a fast override speed; these
# lines are long and the ambient TypingSpeed would blow the runtime
# budget.
Type@12ms "2024-01-15 10:00:00.100 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select o.id, o.total_cents from orders o where o.user_id=?"
Enter
Type@12ms "2024-01-15 10:00:00.101 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]"
Enter
Type@12ms "2024-01-15 10:00:00.110 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select oi.id, oi.sku from order_items oi where oi.order_id=?"
Enter
Type@12ms "2024-01-15 10:00:00.111 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [101]"
Enter
Type@12ms "2024-01-15 10:00:00.120 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select oi.id, oi.sku from order_items oi where oi.order_id=?"
Sleep 700ms

# ctrl-l — same import path as F8 — parses the buffer and opens the
# log picker.
Ctrl+L
Sleep 2000ms

# `c` toggles to the N+1 cluster view: the repeated order_items lookup
# collapses to one row, ×2.
Type "c"
Sleep 2000ms

# `c` again back to the all-queries view (index resets to row 0 — the
# order lookup, first in the log).
Type "c"
Sleep 1000ms

# Enter loads the top (first) reconstructed query — the order lookup,
# against the `orders` table — into the editor, bind values
# substituted in.
Enter
Sleep 1000ms

# ctrl-j — same run path as F5 — executes it. `--demo` has no real
# connection, but the statement still clears safety::evaluate's guard
# and comes back through demo::answer with the `orders` table's real
# columns.
Ctrl+J
Sleep 2000ms

# Esc back to Normal mode first — `q` inside the editor just types the
# letter.
Escape
Sleep 400ms
Type "q"
Sleep 600ms
