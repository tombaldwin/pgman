# The JDBC tap over the route 0.5.3 ships: pgman listening for OTLP, and
# the spans the OpenTelemetry Java agent would export (video/tap/
# otlp-burst.py, posted off-screen once the empty panel has shown) arriving through the
# real parser. So the screen shows what the agent delivers: SQL, timing
# and service name, no rows, binds or calling method. The DSN and
# --no-update-check go in an alias off-screen.
Output target/video/scenes/db-tap.mp4
Set FontSize 18
Source video/scenes/_terminal.tape
Type `alias pgman='command pgman --no-update-check "postgres://postgres:$VIDEO_PW@127.0.0.1:55441/ledger?sslmode=disable"' && ( (sleep 6.5; python3 video/tap/otlp-burst.py | curl -s -o /dev/null -H 'Content-Type: application/json' --data-binary @- http://127.0.0.1:4318/v1/traces) & ) && clear`
Enter
Sleep 300ms
Show
Sleep 300ms
Type "pgman --tap-otlp :4318"
Sleep 300ms
Enter
Sleep 1800ms
Type ":tap"
Sleep 300ms
Enter
Sleep 5000ms
