#!/usr/bin/env bash
# candor effect-regression guard (version-aware). Delegates to the candor clone's wrapper,
# which SKIPS enforcement on a candor version change (a tool upgrade is not a code
# regression) and blocks only on a real AS-EFF-005 against a same-version baseline.
set -uo pipefail
ROOT="$(git rev-parse --show-toplevel)"
[ -f "$ROOT/.candor/config" ] && . "$ROOT/.candor/config"
HOME_DIR="${CANDOR_HOME:-$HOME/git/candor}"
if [ -x "$HOME_DIR/cargo-candor" ]; then
  WRAPPER="$HOME_DIR/cargo-candor"
elif WRAPPER="$(command -v cargo-candor)"; then
  :
else
  echo "candor: wrapper not found ($HOME_DIR/cargo-candor, or cargo-candor on PATH); skipping guard"
  exit 0
fi
cd "$ROOT"
exec "$WRAPPER" guard .candor/baseline
