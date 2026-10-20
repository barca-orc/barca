#!/usr/bin/env bash
# Integration tests for the barca CLI.
# Run: bash tests/integration/test_cli.sh
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

# Extract the JSON line from barca stdout (last line; user prints come before it).
barca_json() { echo "$1" | tail -1; }

# The target's value: `barca get` prints a pointer to the artifact; read the file it names.
final() { barca_json "$1" | python3 -c "import json,sys; d=json.load(sys.stdin)['final_output']; print(json.dumps(json.load(open(d['_barca_artifact']['path']))))"; }

# ─── Test: trivial asset ────────────────────────────────────────────────────
echo "=== Trivial asset ==="

cat > "$TMPDIR/trivial.py" << 'PYEOF'
from barca import asset

@asset()
def hello() -> dict:
    return {"msg": "hello"}
PYEOF

OUTPUT=$($BARCA get "$TMPDIR/trivial.py" 2>/dev/null)
final "$OUTPUT" | grep -q '"msg"' && pass "correct output" || fail "wrong output: $OUTPUT"

# ─── Test: linear chain passes values ────────────────────────────────────────
echo "=== Linear chain ==="

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

OUTPUT=$($BARCA get "$TMPDIR/chain.py" 2>/dev/null)
CHAIN_VAL=$(final "$OUTPUT" | python3 -c "import json,sys; print(json.load(sys.stdin)['value'])")
[ "$CHAIN_VAL" = "22" ] && pass "chain passes values correctly" || fail "chain result: $CHAIN_VAL"

# ─── Test: user print() visible, protocol hidden ────────────────────────────
echo "=== User print statements ==="

cat > "$TMPDIR/prints.py" << 'PYEOF'
from barca import asset

@asset()
def chatty() -> dict:
    print("VISIBLE_USER_OUTPUT")
    return {"value": 42}

@asset(inputs={"data": chatty})
def consumer(data: dict) -> dict:
    print(f"GOT_{data['value']}")
    return {"result": data["value"] * 2}
PYEOF

# User prints go to terminal via Stdio::inherit (visible to the user but not
# captured by $(...)). We verify the result is correct and protocol is hidden.
OUTPUT=$($BARCA get "$TMPDIR/prints.py" 2>/dev/null)
PRINT_VAL=$(final "$OUTPUT" | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])")
[ "$PRINT_VAL" = "84" ] && pass "correct result with prints" || fail "wrong result: $PRINT_VAL"
echo "$OUTPUT" | grep -q '"node_id"' && fail "PROTOCOL LEAKED to stdout" || pass "protocol hidden from user"

# ─── Test: fan-out parallelism ───────────────────────────────────────────────
echo "=== Fan-out parallelism ==="

cat > "$TMPDIR/fanout.py" << 'PYEOF'
from barca import asset

@asset()
def a() -> dict:
    return {"x": 1}

@asset()
def b() -> dict:
    return {"x": 2}

@asset()
def c() -> dict:
    return {"x": 3}

@asset(inputs={"a": a, "b": b, "c": c})
def merge(a: dict, b: dict, c: dict) -> dict:
    return {"sum": a["x"] + b["x"] + c["x"]}
PYEOF

OUTPUT=$($BARCA get "$TMPDIR/fanout.py" 2>/dev/null)
FAN_VAL=$(final "$OUTPUT" | python3 -c "import json,sys; print(json.load(sys.stdin)['sum'])")
[ "$FAN_VAL" = "6" ] && pass "fan-out merge correct" || fail "fan-out result: $FAN_VAL"

# ─── Test: aliased inputs ────────────────────────────────────────────────────
echo "=== Aliased inputs ==="

cat > "$TMPDIR/alias.py" << 'PYEOF'
from barca import asset

@asset()
def raw_prices() -> dict:
    return {"price": 100}

@asset(inputs={"data": raw_prices})
def normalized(data: dict) -> dict:
    return {"normalized_price": data["price"] / 100}
PYEOF

OUTPUT=$($BARCA get "$TMPDIR/alias.py" 2>/dev/null)
ALIAS_VAL=$(final "$OUTPUT" | python3 -c "import json,sys; print(json.load(sys.stdin)['normalized_price'])")
[ "$ALIAS_VAL" = "1.0" ] && pass "aliased input works" || fail "alias result: $ALIAS_VAL"

# ─── Test: multi-file ────────────────────────────────────────────────────────
echo "=== Multi-file ==="

cat > "$TMPDIR/file1.py" << 'PYEOF'
from barca import asset

@asset()
def from_file1() -> dict:
    return {"source": "file1"}
PYEOF

cat > "$TMPDIR/file2.py" << 'PYEOF'
from barca import asset

@asset()
def from_file2() -> dict:
    return {"source": "file2"}
PYEOF

OUTPUT=$($BARCA get "$TMPDIR/file1.py" "$TMPDIR/file2.py" 2>/dev/null)
echo "$OUTPUT" | grep -q "steps_executed.*2\|steps_executed\":2" && pass "multi-file runs both assets" || fail "multi-file result: $OUTPUT"

# ─── Test: noisy stderr doesn't corrupt protocol ────────────────────────────
echo "=== Noisy stderr safety ==="

cat > "$TMPDIR/noisy.py" << 'PYEOF'
import sys, json
from barca import asset

@asset()
def noisy() -> dict:
    # Simulate a library logging structured JSON to stderr
    print(json.dumps({"node_id": "fake", "output": "injected"}), file=sys.stderr)
    print('{"node_id": "fake2", "output": "injected2"}', file=sys.stderr)
    return {"real": True}
PYEOF

OUTPUT=$($BARCA get "$TMPDIR/noisy.py" 2>/dev/null)
NOISY_VAL=$(final "$OUTPUT" | python3 -c "import json,sys; print(json.load(sys.stdin)['real'])")
[ "$NOISY_VAL" = "True" ] && pass "real output survives noisy stderr" || fail "noisy stderr broke output: $NOISY_VAL"
echo "$OUTPUT" | grep -q '"injected"' && fail "FAKE OUTPUT LEAKED into results" || pass "fake stderr JSON ignored"

# ─── Test: sensor payload unpacking ──────────────────────────────────────────
echo "=== Sensor payload unpacking ==="

cat > "$TMPDIR/sensor_pipe.py" << 'PYEOF'
from barca import asset, sensor

@sensor()
def check_temp() -> tuple:
    return (True, {"temp": 72})

@asset(inputs={"data": check_temp})
def process(data: dict) -> dict:
    return {"value": data["temp"] * 2}
PYEOF

OUTPUT=$($BARCA get "$TMPDIR/sensor_pipe.py" 2>/dev/null)
SENSOR_VAL=$(final "$OUTPUT" | python3 -c "import json,sys; print(json.load(sys.stdin)['value'])")
[ "$SENSOR_VAL" = "144" ] && pass "sensor payload unpacked correctly" || fail "sensor payload wrong: $SENSOR_VAL"

# ─── Test: help works ────────────────────────────────────────────────────────
echo "=== Help ==="

$BARCA -h 2>&1 | grep -q "Invisible asset orchestrator" && pass "barca -h works" || fail "no short help"
$BARCA --help 2>&1 | grep -q "fully materialized" && pass "barca --help works" || fail "no help"
$BARCA get --help 2>&1 | grep -q "cache-aware" && pass "barca get --help works" || fail "no get help"

# ─── Summary ─────────────────────────────────────────────────────────────────
echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
