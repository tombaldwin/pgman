# Masked copies and their audit trail, against the scratch "production"
# and "local" databases (video/render.sh starts and seeds them): what a
# masked dump does to each column, the dump (a protected environment asks
# why), the restore into local, the same customers in production and in
# the copy, then the journal of copies. compose.py cuts it into segments.
# Long commands are typed off-screen and shown whole.
Output target/video/scenes/db-masking.mp4
Set FontSize 24
Source video/scenes/_terminal.tape
Show
Sleep 300ms
Hide
Type `pgman masking show production/ledger | head -16`
Show
Sleep 700ms
Enter
Sleep 4000ms
Ctrl+L
Hide
Type `pgman dump production/ledger app-101.pgman-dump`
Show
Sleep 700ms
Enter
Sleep 1500ms
Type "APP-101 reproduce invoice rounding"
Enter
Sleep 4500ms
Ctrl+L
Hide
Type `pgman restore app-101.pgman-dump local/ledger`
Show
Sleep 700ms
Enter
Sleep 2500ms
Type "y"
Enter
Sleep 3500ms
Ctrl+L
Hide
Type `pgman query --target production/ledger "select first_name, last_name, email, phone, city from customers order by id limit 4" 2>/dev/null | column -ts,`
Show
Sleep 700ms
Enter
Sleep 2500ms
Hide
Type `pgman query --target local/ledger "select first_name, last_name, email, phone, city from customers order by id limit 4" 2>/dev/null | column -ts,`
Show
Sleep 700ms
Enter
Sleep 4000ms
Ctrl+L
Hide
Type `pgman copies`
Show
Sleep 700ms
Enter
Sleep 2500ms
Hide
Type `pgman copies report`
Show
Sleep 700ms
Enter
Sleep 4000ms
