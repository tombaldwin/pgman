#!/usr/bin/env bash
# candor effect-regression guard. Delegates to the cargo-candor wrapper, which blocks
# (exit 1) on a real AS-EFF-005 against a same-engine baseline, and refuses to evaluate
# (exit 2) when the baseline was made by another engine build: refresh it with
# 'cargo candor snapshot .candor/baseline', review the effect delta, and commit it.
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
