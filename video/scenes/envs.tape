# Environments: production with its replica (lag, a blocked session),
# staging with pending migrations, a stopped dev, a local container;
# then `v`, schema drift: each database's version (a custom version
# table, Flyway) by environment, behind or pending in yellow.
Output target/video/scenes/envs.mp4
Source video/scenes/_setup.tape
Sleep 800ms
Type "E"
Sleep 2500ms
Type "j"
Sleep 1500ms
Type "v"
Sleep 3000ms
