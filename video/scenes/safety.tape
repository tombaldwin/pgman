# Safety: a DELETE without WHERE is refused before it reaches the
# server.
Output target/video/scenes/safety.mp4
Source video/scenes/_setup.tape
Sleep 800ms
Type "e"
Sleep 500ms
Type "delete from invoices;"
Sleep 500ms
Enter
Sleep 3500ms
