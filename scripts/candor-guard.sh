#!/usr/bin/env bash
# candor effect-regression guard (version-aware). Delegates to the candor clone's wrapper,
# which SKIPS enforcement on a candor version change (a tool upgrade is not a code
# regression) and blocks only on a real AS-EFF-005 against a same-version baseline.
set -uo pipefail
ROOT="$(git rev-parse --show-toplevel)"
[ -f "$ROOT/.candor/config" ] && . "$ROOT/.candor/config"
HOME_DIR="${CANDOR_HOME:-$HOME/git/candor}"
[ -x "$HOME_DIR/cargo-candor" ] || { echo "candor: wrapper not found ($HOME_DIR/cargo-candor); skipping guard"; exit 0; }
cd "$ROOT"
exec "$HOME_DIR/cargo-candor" guard .candor/baseline
