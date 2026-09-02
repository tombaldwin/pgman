# VHS tape for pgman's README hero gif.
#
# Render with:
#   brew install vhs        # one-time
#   cargo install --path . --locked --force   # rebuild the local binary
#   vhs demo.tape            # produces demo.gif next to this file
#
# Flow (~25s) — the wedge: logs/pasted code → runnable SQL:
#   launch → demo's pre-loaded result grid settles → e (editor) → clear
#   the demo's canned query → paste a 5-line Hibernate log sample (two
#   binding-parameter lines, one repeated select) → ctrl-l to reconstruct
#   (same path as F8 — VHS 0.11 has no function-key command, see below)
#   → log-pick list (all queries) → `c` for the N+1 cluster view → `c`
#   back → Enter loads the top query into the editor → quit.
#
# Targets `pgman --demo`, the synthetic-data mode baked into the binary,
# so there's no database and the frame is identical on every render.
# `--demo` never opens a real connection (`App::client` stays `None`),
# so running the loaded query (F5) would only produce a "not connected"
# status — the recording stops once the query is loaded into the editor
# rather than staging a run that can't complete.
#
# VHS doesn't emit function keys, so F8 is driven via its `ctrl-l` alias
# (`App::start_log_import` — see src/app/editor.rs) instead.

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

# Launch against the synthetic fixture. `~/.cargo/bin/pgman` rather
# than bare `pgman` so VHS picks up the locally-built binary that has
# --demo.
Type "~/.cargo/bin/pgman --demo"
Sleep 400ms
Enter

# Splash (minimum 600ms) + a beat to settle on the demo's pre-loaded
# `users` result grid.
Sleep 1500ms

# `e` focuses the editor — it opens on the demo's canned SELECT.
Type "e"
Sleep 700ms

# Clear it: the log sample below is what should be in the buffer when
# ctrl-l parses it, not a leftover query.
Ctrl+U
Sleep 400ms

# Type a 5-line Hibernate log sample: an order lookup, then an item
# lookup that repeats for a second order_id in the same burst — the
# N+1 signature nplus1.rs clusters on. Typed at a fast override speed;
# these lines are long and the ambient TypingSpeed would blow the
# runtime budget.
Type@12ms "2024-01-15 10:00:00.100 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select o.id, o.total from orders o where o.customer_id=?"
Enter
Type@12ms "2024-01-15 10:00:00.101 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [42]"
Enter
Type@12ms "2024-01-15 10:00:00.110 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select i.id, i.sku from item i where i.order_id=?"
Enter
Type@12ms "2024-01-15 10:00:00.111 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [INTEGER] - [101]"
Enter
Type@12ms "2024-01-15 10:00:00.120 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select i.id, i.sku from item i where i.order_id=?"
Sleep 700ms

# ctrl-l — same import path as F8 — parses the buffer and opens the
# log picker.
Ctrl+L
Sleep 2000ms

# `c` toggles to the N+1 cluster view: the repeated item lookup
# collapses to one row, ×2.
Type "c"
Sleep 2000ms

# `c` again back to the all-queries view.
Type "c"
Sleep 1000ms

# Enter loads the top (first) reconstructed query into the editor,
# bind values substituted in.
Enter
Sleep 2500ms

# Esc back to Normal mode first — `q` inside the editor just types the
# letter.
Escape
Sleep 400ms
Type "q"
Sleep 600ms
