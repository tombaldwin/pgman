# VHS tape for pgman's README hero gif.
#
# Render with:
#   brew install vhs        # one-time
#   vhs demo.tape           # produces demo.gif next to this file
#
# Flow (~28s):
#   launch → users result grid → /pro live filter → S schema browser
#   (expand) → Q saved queries → load the :param query → answer the
#   prompt → lands in the editor with the query filled + highlighted
#   → quit.
#
# Targets `pgman --demo`, the synthetic-data mode baked into the
# binary, so there's no database and the frame is identical on every
# render. (The JDBC-tap panel is F4; VHS doesn't emit function keys,
# so it's left out of the recording — drive it by hand if you want a
# tap gif.)

Output demo.gif

# Terminal box: wide enough that the result grid + editor don't
# truncate (~120 cols), tall enough for header + editor + grid (~30
# rows). MesloLGS Nerd Font @ 14pt matches the glyph set pgman's
# Powerline chrome expects.
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

# Splash animation (≥3s) + a beat for the dashboard to settle. The
# eye lands on the `users` result grid around t≈4s.
Sleep 4500ms
Sleep 2000ms

# Live-filter the grid to the `pro` rows. `/` opens the filter, the
# grid narrows as each char lands; Enter accepts.
Type "/pro"
Sleep 1500ms
Enter
Sleep 1500ms
# Esc clears the filter, back to the full result.
Escape
Sleep 1000ms

# `S` opens the schema browser; `+` expands every schema so the
# users / orders / order_items tables + columns are visible.
Type "S"
Sleep 1200ms
Type "+"
Sleep 3000ms
Escape
Sleep 800ms

# `Q` opens the saved-queries panel. `j` moves to the `order-by-id`
# entry (the one with a `:order_id` placeholder).
Type "Q"
Sleep 1200ms
Type "j"
Sleep 1200ms

# Enter loads it — because it carries a `:param`, pgman prompts for
# the value first.
Enter
Sleep 1500ms
Type "1001"
Sleep 800ms
Enter
Sleep 800ms

# The substituted query lands in the editor, syntax-highlighted, with
# the cursor ready. Dwell so the viewer reads it.
Sleep 2500ms

# Esc back to the grid, then quit cleanly so the loop point isn't a
# half-rendered overlay.
Escape
Sleep 600ms
Type "q"
Sleep 600ms
