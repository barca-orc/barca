#!/usr/bin/env bash
# Cache and staleness integration tests.
# These test that `barca get` correctly caches results and detects staleness.
#
# Run: bash tests/integration/test_cache.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BARCA="${REPO_ROOT}/.venv/bin/barca"
[ -x "$BARCA" ] || BARCA="$(command -v barca)"
PASS=0
FAIL=0
TMPDIR=$(mktemp -d)

pass() { echo "  ✓ $1"; PASS=$((PASS + 1)); }
fail() { echo "  ✗ $1"; FAIL=$((FAIL + 1)); }

cleanup() { rm -rf "$TMPDIR"; }
trap cleanup EXIT

# Helper: extract steps_executed from barca output JSON
steps() { echo "$1" | python3 -c "import json,sys; print(json.load(sys.stdin).get('steps_executed', -1))"; }

# ─── Basic cache tests ──────────────────────────────────────────────────────
echo "=== Basic cache ==="

cat > "$TMPDIR/chain.py" << 'PYEOF'
from barca import asset

@asset()
def a() -> dict:
    return {"value": 1}

@asset(inputs={"data": a})
def b(data: dict) -> dict:
    return {"value": data["value"] + 10}

@asset(inputs={"data": b})
def c(data: dict) -> dict:
    return {"value": data["value"] * 2}
PYEOF

# First get: should execute all 3 (a→b→c)
OUT1=$($BARCA get c "$TMPDIR/chain.py" -o value --json 2>/dev/null)
S1=$(steps "$OUT1")
[ "$S1" = "3" ] && pass "first get executes all 3" || fail "first get: expected 3 steps, got $S1"

# Second get: should be all cached (0 steps executed)
OUT2=$($BARCA get c "$TMPDIR/chain.py" -o value --json 2>/dev/null)
S2=$(steps "$OUT2")
[ "$S2" = "0" ] && pass "second get is fully cached" || fail "second get: expected 0 steps, got $S2"

# Output should match (`-o value` puts the value in final_output)
V1=$(echo "$OUT1" | python3 -c "import json,sys; print(json.dumps(json.load(sys.stdin)['final_output']))")
V2=$(echo "$OUT2" | python3 -c "import json,sys; print(json.dumps(json.load(sys.stdin)['final_output']))")
[ "$V1" = "$V2" ] && pass "cached output matches" || fail "output mismatch: $V1 vs $V2"

# ─── Target subgraph: unrelated assets don't execute ────────────────────────
echo "=== Target subgraph ==="

cat > "$TMPDIR/two_chains.py" << 'PYEOF'
from barca import asset

@asset()
def a() -> dict:
    return {"chain": "abc", "value": 1}

@asset(inputs={"data": a})
def b(data: dict) -> dict:
    return {"chain": "abc", "value": data["value"] + 1}

@asset(inputs={"data": b})
def c(data: dict) -> dict:
    return {"chain": "abc", "value": data["value"] + 1}

@asset()
def d() -> dict:
    return {"chain": "de", "value": 100}

@asset(inputs={"data": d})
def e(data: dict) -> dict:
    return {"chain": "de", "value": data["value"] + 100}
PYEOF

OUT=$($BARCA get c "$TMPDIR/two_chains.py" 2>/dev/null)
S=$(steps "$OUT")
[ "$S" = "3" ] && pass "get c only runs a→b→c (not d,e)" || fail "subgraph: expected 3 steps, got $S"

# ─── Staleness: modify leaf ─────────────────────────────────────────────────
echo "=== Staleness: modify leaf ==="

# Reset: fresh get of c
$BARCA get c "$TMPDIR/chain.py" 2>/dev/null > /dev/null

# Modify c's function body
sed -i.bak 's/data\["value"\] \* 2/data["value"] * 3/' "$TMPDIR/chain.py"

OUT=$($BARCA get c "$TMPDIR/chain.py" 2>/dev/null)
S=$(steps "$OUT")
[ "$S" = "1" ] && pass "modify leaf: only c re-executes" || fail "modify leaf: expected 1 step, got $S"

# ─── Staleness: modify root ─────────────────────────────────────────────────
echo "=== Staleness: modify root ==="

# Reset chain.py
cat > "$TMPDIR/chain.py" << 'PYEOF'
from barca import asset

@asset()
def a() -> dict:
    return {"value": 1}

@asset(inputs={"data": a})
def b(data: dict) -> dict:
    return {"value": data["value"] + 10}

@asset(inputs={"data": b})
def c(data: dict) -> dict:
    return {"value": data["value"] * 2}
PYEOF

# Fresh baseline
$BARCA get c "$TMPDIR/chain.py" 2>/dev/null > /dev/null

# Modify a's function body
sed -i.bak 's/{"value": 1}/{"value": 99}/' "$TMPDIR/chain.py"

OUT=$($BARCA get c "$TMPDIR/chain.py" 2>/dev/null)
S=$(steps "$OUT")
[ "$S" = "3" ] && pass "modify root: all 3 re-execute" || fail "modify root: expected 3 steps, got $S"

# ─── Staleness: modify middle ───────────────────────────────────────────────
echo "=== Staleness: modify middle ==="

# Reset
cat > "$TMPDIR/chain.py" << 'PYEOF'
from barca import asset

@asset()
def a() -> dict:
    return {"value": 1}

@asset(inputs={"data": a})
def b(data: dict) -> dict:
    return {"value": data["value"] + 10}

@asset(inputs={"data": b})
def c(data: dict) -> dict:
    return {"value": data["value"] * 2}
PYEOF

$BARCA get c "$TMPDIR/chain.py" 2>/dev/null > /dev/null

# Modify b only
sed -i.bak 's/data\["value"\] + 10/data["value"] + 999/' "$TMPDIR/chain.py"

OUT=$($BARCA get c "$TMPDIR/chain.py" 2>/dev/null)
S=$(steps "$OUT")
[ "$S" = "2" ] && pass "modify middle: b+c re-execute, a cached" || fail "modify middle: expected 2 steps, got $S"

# ─── Staleness: helper function ─────────────────────────────────────────────
echo "=== Staleness: helper function ==="

cat > "$TMPDIR/helper.py" << 'PYEOF'
from barca import asset

def compute(x):
    return x * 2

@asset()
def result() -> dict:
    return {"value": compute(21)}
PYEOF

$BARCA get result "$TMPDIR/helper.py" 2>/dev/null > /dev/null

# Modify helper
sed -i.bak 's/return x \* 2/return x * 3/' "$TMPDIR/helper.py"

OUT=$($BARCA get result "$TMPDIR/helper.py" 2>/dev/null)
S=$(steps "$OUT")
[ "$S" = "1" ] && pass "helper change: asset re-executes" || fail "helper change: expected 1 step, got $S"

# ─── Staleness: global constant ─────────────────────────────────────────────
echo "=== Staleness: global constant ==="

cat > "$TMPDIR/global.py" << 'PYEOF'
from barca import asset

THRESHOLD = 0.5

@asset()
def check() -> dict:
    return {"above": 75 > THRESHOLD}
PYEOF

$BARCA get check "$TMPDIR/global.py" 2>/dev/null > /dev/null

# Modify constant
sed -i.bak 's/THRESHOLD = 0.5/THRESHOLD = 100/' "$TMPDIR/global.py"

OUT=$($BARCA get check "$TMPDIR/global.py" 2>/dev/null)
S=$(steps "$OUT")
[ "$S" = "1" ] && pass "global change: asset re-executes" || fail "global change: expected 1 step, got $S"

# ─── Revert uses old cache ──────────────────────────────────────────────────
echo "=== Revert restores cache ==="

cat > "$TMPDIR/revert.py" << 'PYEOF'
from barca import asset

@asset()
def x() -> dict:
    return {"version": 1}
PYEOF

# v1
OUT_V1=$($BARCA get x "$TMPDIR/revert.py" 2>/dev/null)

# Modify to v2
sed -i.bak 's/"version": 1/"version": 2/' "$TMPDIR/revert.py"
$BARCA get x "$TMPDIR/revert.py" 2>/dev/null > /dev/null

# Revert to v1
sed -i.bak 's/"version": 2/"version": 1/' "$TMPDIR/revert.py"
OUT_REVERTED=$($BARCA get x "$TMPDIR/revert.py" 2>/dev/null)
S=$(steps "$OUT_REVERTED")
[ "$S" = "0" ] && pass "revert: uses old cache (0 steps)" || fail "revert: expected 0 steps, got $S"

# ─── Partitioned cache ──────────────────────────────────────────────────────
echo "=== Partitioned cache ==="

cat > "$TMPDIR/partitioned.py" << 'PYEOF'
from barca import asset, partitions

@asset(partitions={"key": partitions(["a", "b", "c"])})
def fetch(key: str) -> dict:
    return {"key": key, "fetched": True}
PYEOF

OUT1=$($BARCA get fetch "$TMPDIR/partitioned.py" 2>/dev/null)
S1=$(steps "$OUT1")
[ "$S1" = "3" ] && pass "partitioned first run: 3 steps" || fail "partitioned first: expected 3, got $S1"

# Each partition key is cache-checked on its own run hash, so an unchanged re-run executes nothing.
OUT2=$($BARCA get fetch "$TMPDIR/partitioned.py" 2>/dev/null)
S2=$(steps "$OUT2")
[ "$S2" = "0" ] && pass "partitioned second run: all 3 keys served from cache" || fail "partitioned second: expected 0, got $S2"

# Modify function → all partitions re-execute
sed -i.bak 's/"fetched": True/"fetched": False/' "$TMPDIR/partitioned.py"
OUT3=$($BARCA get fetch "$TMPDIR/partitioned.py" 2>/dev/null)
S3=$(steps "$OUT3")
[ "$S3" = "3" ] && pass "partitioned stale: all 3 re-execute" || fail "partitioned stale: expected 3, got $S3"

# ─── Summary ─────────────────────────────────────────────────────────────────
echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
