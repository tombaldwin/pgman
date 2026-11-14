# Spring discovery: pgman started inside a Spring Boot project
# (video/spring-app) lists the datasources its application*.yml files
# declare, ${DB_PASSWORD} read from the environment and the unresolved
# one marked, and connects to nothing until Enter. --no-tap off-screen:
# a Java project starts the tap listener, whose port may be taken on
# the machine recording. The db-* config's environments are removed
# off-screen: with environments configured, pgman opens their picker.
Output target/video/scenes/db-spring.mp4
Set FontSize 18
Source video/scenes/_terminal.tape
Type `rm -f "$XDG_CONFIG_HOME"/pgman/*.toml && cd video/spring-app && alias pgman="pgman --no-tap" && clear`
Enter
Sleep 300ms
Show
Sleep 300ms
Type "pgman"
Sleep 300ms
Enter
Sleep 3000ms
Enter
Sleep 3000ms
