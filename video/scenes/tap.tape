# The JDBC tap: the app's own queries arriving live, then `v` through
# its views (hotspots, N+1).
Output target/video/scenes/tap.mp4
Source video/scenes/_setup.tape
Sleep 500ms
Type ":tap"
Sleep 300ms
Enter
Sleep 2500ms
Type "v"
Sleep 2200ms
Type "v"
Sleep 2500ms
