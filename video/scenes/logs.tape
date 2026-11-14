# Logs to SQL: pgman pointed at a Spring app's Hibernate log
# (video/billing-api.log), the picker of reconstructed queries, `c` for
# the N+1 view (invoice_lines looked up once per invoice), `c` back,
# then the first query loaded with its bind filled in, and run.
Output target/video/scenes/logs.mp4
Set FontSize 18
Source video/scenes/_terminal.tape
Type "cd video"
Enter
Type "clear"
Enter
Sleep 300ms
Show
Sleep 300ms
Type "pgman --demo --log billing-api.log"
Sleep 500ms
Enter
Sleep 4000ms
Type "c"
Sleep 3000ms
Type "c"
Sleep 800ms
Enter
Sleep 1500ms
Enter
Sleep 3000ms
