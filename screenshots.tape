# VHS tape for the product page's stills: the environments overview,
# logs → SQL with the N+1 cluster, and a patch dry run.
#
# Render with:
#   cargo install --path . --locked --force   # rebuild the local binary
#   vhs screenshots.tape    # writes target/screenshots/*.png
#
# Look at each PNG before using it: VHS now and then saves a frame
# before its terminal has finished redrawing.
#
# Same terminal as demo.tape at 13pt, so the environments engine
# column keeps its upgrade arrows, against `pgman --demo`'s synthetic data.
# XDG dirs point under target/ so the run reads no real config and
# leaves no history behind.

Output target/screenshots/run.gif

Set Width 1280
Set Height 720
Set FontSize 13
Set FontFamily "MesloLGS Nerd Font Mono"
Set Theme "Catppuccin Mocha"
Set TypingSpeed 0
Set Padding 16

Hide
Type "export PATH=$HOME/.cargo/bin:$PATH XDG_CONFIG_HOME=$PWD/target/screenshots/xdg/config XDG_CACHE_HOME=$PWD/target/screenshots/xdg/cache XDG_DATA_HOME=$PWD/target/screenshots/xdg/data"
Enter
Type "clear && pgman --demo"
Enter
Sleep 2500ms
Show

# Environments: replicas under their primary, pending patches, lag,
# sessions, sizes and engine upgrades.
Type "E"
Sleep 1500ms
Screenshot target/screenshots/demo-environments.png

# A dry run of staging's pending patches, before it runs.
Type "g"
Sleep 200ms
Type "j"
Sleep 200ms
Type "j"
Sleep 200ms
Type "j"
Sleep 200ms
Type "j"
Sleep 400ms
Type "P"
Sleep 2500ms
Tab
Sleep 2000ms
Screenshot target/screenshots/demo-patches.png
Type "q"
Sleep 400ms
Type "q"
Sleep 400ms

# Logs → SQL: a Hibernate log in the editor, reconstructed, and the
# repeated invoice_lines lookup clustered as N+1.
Type "e"
Sleep 400ms
Ctrl+U
Type "2024-01-15 10:00:00.100 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select i.id, i.number, i.total from invoices i where i.status=?"
Enter
Type "2024-01-15 10:00:00.101 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [VARCHAR] - [open]"
Enter
Type "2024-01-15 10:00:00.110 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select l.id, l.description, l.amount from invoice_lines l where l.invoice_id=?"
Enter
Type "2024-01-15 10:00:00.111 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [BIGINT] - [48202]"
Enter
Type "2024-01-15 10:00:00.120 DEBUG 1 --- [nio-8080-exec-3] org.hibernate.SQL : select l.id, l.description, l.amount from invoice_lines l where l.invoice_id=?"
Enter
Type "2024-01-15 10:00:00.121 TRACE 1 --- [nio-8080-exec-3] o.h.type.descriptor.sql.BasicBinder : binding parameter [1] as [BIGINT] - [48207]"
Sleep 500ms
Ctrl+L
Sleep 1500ms
Type "c"
Sleep 1500ms
Screenshot target/screenshots/demo-logs.png
