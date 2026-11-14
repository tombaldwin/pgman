#!/usr/bin/env bash
# Record pgman's video scenes and compose the overview and the feature
# loops into target/video/out/.
#
# Usage:
#   video/render.sh              # record every scene, compose everything
#   video/render.sh logs envs    # re-record only these scenes, compose everything
#   video/render.sh --compose    # compose from the scenes already recorded
#
# Needs vhs (brew install vhs), ffmpeg, python3 with Pillow, and the
# MesloLGS Nerd Font the tapes ask for (as demo.tape does). Every pgman
# launch runs under scratch HOME and XDG dirs in target/video, so no
# real config reaches the recording and nothing outside target/ is written.
set -euo pipefail
cd "$(dirname "$0")/.."

W="$PWD/target/video"
# poly.io's look: the site's fonts (Space Grotesk, the Neuropolitical
# wordmark) and pgman's sprite, fetched from www.poly.io once.
brand() {
  mkdir -p "$W/brand"
  local site=https://www.poly.io
  get() { [ -s "$W/brand/$2" ] || curl -sfL -o "$W/brand/$2" "$site$1"; }
  for w in 300 400 600 700; do get "/fonts/space-grotesk/space-grotesk-$w.woff2" "space-grotesk-$w.woff2"; done
  get /fonts/neuropolitical/299997_0_0.woff neuropolitical.woff
  get /images/pgman-elephant-pixel.svg pgman-elephant-pixel.svg
}

# The scenes named db-*: two scratch Postgres containers, "production"
# seeded with made-up people (video/seed/seed.py) and "local" emptied for
# each recording to restore into, and a config naming them, with a
# throwaway age key for the protected environment's dumps. The
# containers stay up between renders; `docker rm -f pgman-video-prod
# pgman-video-local` removes them.
db() {
  mkdir -p "$W/db"
  [ -s "$W/db/pw" ] || { openssl rand -hex 12 > "$W/db/pw"; chmod 600 "$W/db/pw"; }
  local pw; pw=$(cat "$W/db/pw")
  for spec in prod:55441 local:55442; do
    local name=pgman-video-${spec%%:*} port=${spec##*:}
    if ! docker ps --format '{{.Names}}' | grep -qx "$name"; then
      docker rm -f "$name" >/dev/null 2>&1 || true
      docker run -d --name "$name" -e POSTGRES_PASSWORD="$pw" -e POSTGRES_DB=ledger \
        -p "127.0.0.1:$port:5432" postgres:16 >/dev/null
    fi
    until docker exec "$name" pg_isready -q 2>/dev/null; do sleep 1; done
  done
  # pg_isready answers before the entrypoint's restart; wait for the real one.
  until docker exec pgman-video-prod psql -U postgres -d ledger -qAtc 'select 1' >/dev/null 2>&1; do sleep 1; done
  if [ "$(docker exec pgman-video-prod psql -U postgres -d ledger -qAtc "select to_regclass('customers') is not null")" != t ]; then
    python3 video/seed/seed.py | docker exec -i pgman-video-prod psql -q -U postgres -d ledger -v ON_ERROR_STOP=1 >/dev/null
  fi
  until docker exec pgman-video-local psql -U postgres -d postgres -qAtc 'select 1' >/dev/null 2>&1; do sleep 1; done
  docker exec pgman-video-local psql -q -U postgres -d postgres -c 'DROP DATABASE IF EXISTS ledger WITH (FORCE)' -c 'CREATE DATABASE ledger' >/dev/null
  local cfg="$W/db/cfg/pgman"
  if [ ! -s "$cfg/keys/default.txt" ]; then
    rm -rf "$W/db/cfg"; mkdir -p "$cfg"
    XDG_CONFIG_HOME="$W/db/cfg" HOME="$W/home" target/release/pgman key new >/dev/null
  fi
  local pub; pub=$(grep -o 'age1[a-z0-9]*' "$cfg/keys/default.txt" | head -1)
  cat > "$cfg/connections.toml" <<TOML
[[connections]]
name = "prod-ledger"
url = "postgres://postgres@127.0.0.1:55441/ledger?sslmode=disable"
password_env = "VIDEO_PW"

[[connections]]
name = "local-ledger"
url = "postgres://postgres@127.0.0.1:55442/ledger?sslmode=disable"
password_env = "VIDEO_PW"
TOML
  cat > "$cfg/environments.toml" <<TOML
[[environments]]
name = "production"
protected = true
encrypt = "age:$pub"
source = { kind = "static", connections = ["prod-ledger"] }

[[environments]]
name = "local"
source = { kind = "static", connections = ["local-ledger"] }
TOML
}

if [ "${1:-}" != "--compose" ]; then
  cargo build --release --quiet
  scenes=("$@")
  if [ ${#scenes[@]} -eq 0 ]; then
    for t in video/scenes/[a-z]*.tape; do scenes+=("$(basename "$t" .tape)"); done
  fi
  for s in "${scenes[@]}"; do
    # A fresh scratch home per scene: each starts from pgman's first run.
    rm -rf "$W/home" "$W/cfg" "$W/cache" "$W/data"
    mkdir -p "$W/home" "$W/cfg" "$W/cache" "$W/data" "$W/scenes"
    case "$s" in db-*) db; cp -R "$W/db/cfg/pgman" "$W/cfg/" ;; esac
    echo "recording $s"
    vhs "video/scenes/$s.tape" >"$W/scenes/$s.log" 2>&1 || { tail -5 "$W/scenes/$s.log"; exit 1; }
  done
fi
brand
python3 video/compose.py
