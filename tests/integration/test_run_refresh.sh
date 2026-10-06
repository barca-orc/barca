#!/usr/bin/env bash
# `barca run` / `barca get` cache semantics: upstream assets cache-aware by default,
# --refresh <names> selective, --refresh-all for the whole cone (--no-cache is its deprecated
# spelling and warns).
#
# Run: bash tests/integration/test_run_refresh.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BARCA="${BARCA:-${REPO_ROOT}/.venv/bin/barca}"
[ -x "$BARCA" ] || BARCA="$(command -v barca)"
PASS=0
FAIL=0
TMPDIR=$(mktemp -d)

pass() { echo "  ✓ $1"; PASS=$((PASS + 1)); }
fail() { echo "  ✗ $1"; FAIL=$((FAIL + 1)); }
cleanup() { rm -rf "$TMPDIR"; }
trap cleanup EXIT

steps() { echo "$1" | python3 -c "import json,sys; print(json.load(sys.stdin).get('steps_executed', -1))"; }
check() { # name expected actual
  if [ "$3" = "$2" ]; then pass "$1 (steps=$3)"; else fail "$1: expected $2 steps, got $3"; fi
}

cat > "$TMPDIR/pipe.py" << 'PYEOF'
from barca import asset, task

@asset()
def raw() -> dict:
    return {"v": 1}

@asset(inputs={"d": raw})
def clean(d: dict) -> dict:
    return {"v": d["v"] + 1}

@task(inputs={"d": clean})
def validate(d: dict) -> dict:
    return d
PYEOF

cd "$TMPDIR"
echo "=== barca run refresh semantics ==="

"$BARCA" run validate pipe.py > /dev/null          # cold: raw + clean + validate
check "warm run executes task only" 1 "$(steps "$("$BARCA" run validate pipe.py)")"
check "--refresh clean re-runs clean + task" 2 "$(steps "$("$BARCA" run validate pipe.py --refresh clean)")"
check "--refresh raw,clean re-runs both + task" 3 "$(steps "$("$BARCA" run validate pipe.py --refresh raw,clean)")"
check "--refresh-all re-runs whole cone" 3 "$(steps "$("$BARCA" run validate pipe.py --refresh-all)")"
check "--no-cache (deprecated) equals --refresh-all" 3 "$(steps "$("$BARCA" run validate pipe.py --no-cache 2>/dev/null)")"
WARN=$("$BARCA" run validate pipe.py --no-cache 2>&1 >/dev/null)
if [[ "$WARN" == *"[barca] warning: --no-cache is deprecated"* ]]; then
  pass "--no-cache warns that it is deprecated"
else
  fail "--no-cache did not print a deprecation warning"
fi

echo "=== barca get: the same refresh vocabulary ==="
check "warm get is fully cached" 0 "$(steps "$("$BARCA" get clean pipe.py)")"
check "get --refresh raw cascades to clean" 2 "$(steps "$("$BARCA" get clean pipe.py --refresh raw)")"
check "get --refresh raw --no-cascade" 1 "$(steps "$("$BARCA" get clean pipe.py --refresh raw --no-cascade)")"
check "get --refresh-all re-runs the cone" 2 "$(steps "$("$BARCA" get clean pipe.py --refresh-all)")"

echo ""
echo "Passed: $PASS  Failed: $FAIL"
[ "$FAIL" -eq 0 ]
