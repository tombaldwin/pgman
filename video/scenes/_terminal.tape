# The terminal every scene records in, and its shell set up off-screen:
# this checkout's release build first on PATH, scratch HOME and XDG dirs
# in target/video, so no real config reaches the recording and the
# recording writes nothing outside target/. Ends hidden.
Set Width 1680
Set Height 880
Set FontSize 18
Set FontFamily "MesloLGS Nerd Font Mono"
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms
Set Padding 20
Set Framerate 30
Hide
Type `export PATH="$PWD/target/release:$PATH" HOME="$PWD/target/video/home" XDG_CONFIG_HOME="$PWD/target/video/cfg" XDG_CACHE_HOME="$PWD/target/video/cache" XDG_DATA_HOME="$PWD/target/video/data"`
Enter
# The video databases' password (video/render.sh makes it), for the
# db-* scenes' connections.toml and the Spring app's ${DB_PASSWORD}.
Type `export VIDEO_PW="$(cat target/video/db/pw 2>/dev/null)"; export DB_PASSWORD="$VIDEO_PW"`
Enter
Type "clear"
Enter
Sleep 300ms
