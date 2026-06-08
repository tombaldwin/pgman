#!/usr/bin/env bash
# candor-check.sh — DB-boundary gate for pgman.
#
# Direct database access (the `Db` effect) must stay in the DATA LAYER — src/conn.rs (connection +
# transaction primitives) and src/query/ (query modules). A UI/app function that performs a DIRECT Db
# call is an architectural leak: route it through the data layer instead. (Transitively reaching Db via
# a conn/query call is fine — only a DIRECT Db call outside the data layer is flagged.) Mirrors
# .candor/policy.
#
# Uses the STABLE scanner (`cargo install candor-scan`) — a syntactic effect report, no nightly, no DB.
# https://crates.io/crates/candor-scan
#
# Exit 0 = clean; exit 1 = a Db call appeared outside the data layer.
set -uo pipefail

DIR="${1:-.}"
# Functions allowed to perform a DIRECT Db call OUTSIDE the data layer (documented exceptions, by leaf
# name). Empty — the boundary is fully enforced: every direct Db call lives in src/conn.rs or src/query/.
ALLOW_FNS=""

command -v candor-scan >/dev/null 2>&1 || {
  echo "candor: candor-scan not found — install it: cargo install candor-scan" >&2
  exit 2
}

report="$(mktemp)"
trap 'rm -f "$report"' EXIT
candor-scan "$DIR" --json > "$report" || { echo "candor: candor-scan failed" >&2; exit 2; }

viol="$(ALLOW="$ALLOW_FNS" REPORT="$report" python3 - <<'PY'
import json, os
allow = set(os.environ["ALLOW"].split())
doc = json.load(open(os.environ["REPORT"]))
for f in doc.get("functions", []):
    if "Db" not in f.get("direct", []):
        continue  # no DIRECT Db call (transitive-only is fine)
    loc = f.get("loc", "")
    if loc.startswith("src/conn.rs") or loc.startswith("src/query/"):
        continue  # in the data layer — allowed
    if f["fn"].rsplit("::", 1)[-1] in allow:
        continue  # documented exception
    print("  " + f["fn"] + "  @ " + loc)
PY
)"

if [ -n "$viol" ]; then
  echo "candor: ✗ DIRECT database access OUTSIDE the data layer (src/conn.rs, src/query/):" >&2
  printf '%s\n' "$viol" >&2
  echo "candor: route the query through conn/query, or add a documented exception in ci/candor-check.sh. (.candor/policy)" >&2
  exit 1
fi
echo "candor: ✓ direct Db access is confined to the data layer (conn.rs + query/)."
