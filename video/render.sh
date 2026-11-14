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

W=target/video
fonts() {
  mkdir -p "$W/fonts"
  for w in 400 600 800; do
    f="$W/fonts/inter-$w.woff"
    [ -s "$f" ] || curl -sfL -o "$f" \
      "https://cdn.jsdelivr.net/npm/@fontsource/inter@5.0.18/files/inter-latin-$w-normal.woff"
  done
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
    echo "recording $s"
    vhs "video/scenes/$s.tape" >"$W/scenes/$s.log" 2>&1 || { tail -5 "$W/scenes/$s.log"; exit 1; }
  done
fi
fonts
python3 video/compose.py
