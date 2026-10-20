#!/usr/bin/env bash
# Fuzz tests for cache correctness.
# Generates random DAGs, runs them, mutates random nodes, and verifies
# that staleness propagates correctly.
#
# Run: bash tests/integration/test_cache_fuzz.sh [iterations]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BARCA="${REPO_ROOT}/.venv/bin/barca"
[ -x "$BARCA" ] || BARCA="$(command -v barca)"
ITERATIONS=${1:-20}
PASS=0
FAIL=0
TMPDIR=$(mktemp -d)

pass() { PASS=$((PASS + 1)); }
fail() { echo "  ✗ $1"; FAIL=$((FAIL + 1)); }

cleanup() { rm -rf "$TMPDIR"; }
trap cleanup EXIT

steps() { echo "$1" | python3 -c "import json,sys; print(json.load(sys.stdin).get('steps_executed', -1))" 2>/dev/null; }
output() { echo "$1" | python3 -c "import json,sys; print(json.dumps(json.load(sys.stdin).get('final_output', {})))" 2>/dev/null; }

# ─── DAG generators ─────────────────────────────────────────────────────────

generate_linear_chain() {
    local n=$1
    local file="$TMPDIR/linear_${n}.py"
    python3 -c "
from random import Random
rng = Random(42)
lines = ['from barca import asset', '']
for i in range($n):
    if i == 0:
        lines.append(f'@asset()')
        lines.append(f'def node_{i:03d}() -> dict:')
        lines.append(f'    return {{\"step\": {i}, \"val\": {rng.randint(1,100)}}}')
    else:
        prev = f'node_{i-1:03d}'
        lines.append(f'@asset(inputs={{\"{prev}\": {prev}}})')
        lines.append(f'def node_{i:03d}({prev}: dict) -> dict:')
        lines.append(f'    return {{\"step\": {i}, \"val\": {prev}[\"val\"] + {rng.randint(1,50)}}}')
    lines.append('')
print('\n'.join(lines))
" > "$file"
    echo "$file"
}

generate_diamond() {
    local width=$1
    local file="$TMPDIR/diamond_${width}.py"
    python3 -c "
from random import Random
rng = Random(43)
lines = ['from barca import asset', '']
# Source
lines.append('@asset()')
lines.append('def source() -> dict:')
lines.append(f'    return {{\"val\": {rng.randint(1,100)}}}')
lines.append('')
# Width branches
for i in range($width):
    lines.append(f'@asset(inputs={{\"data\": source}})')
    lines.append(f'def branch_{i:02d}(data: dict) -> dict:')
    lines.append(f'    return {{\"branch\": {i}, \"val\": data[\"val\"] * {rng.randint(2,10)}}}')
    lines.append('')
# Merge
deps = ', '.join(f'\"b{i:02d}\": branch_{i:02d}' for i in range($width))
params = ', '.join(f'b{i:02d}: dict' for i in range($width))
lines.append(f'@asset(inputs={{{deps}}})')
lines.append(f'def merge({params}) -> dict:')
lines.append(f'    return {{\"total\": ' + ' + '.join(f'b{i:02d}[\"val\"]' for i in range($width)) + '}')
lines.append('')
print('\n'.join(lines))
" > "$file"
    echo "$file"
}

generate_with_helpers() {
    local file="$TMPDIR/helpers.py"
    python3 -c "
from random import Random
rng = Random(44)
lines = ['from barca import asset', '']
# Helpers
lines.append(f'SCALE = {rng.randint(1,10)}')
lines.append('')
lines.append('def compute(x):')
lines.append(f'    return x * SCALE')
lines.append('')
lines.append('def transform(x):')
lines.append(f'    return compute(x) + {rng.randint(1,100)}')
lines.append('')
# Assets using helpers
lines.append('@asset()')
lines.append('def raw() -> dict:')
lines.append(f'    return {{\"val\": {rng.randint(1,100)}}}')
lines.append('')
lines.append('@asset(inputs={\"data\": raw})')
lines.append('def processed(data: dict) -> dict:')
lines.append('    return {\"val\": transform(data[\"val\"])}')
lines.append('')
lines.append('@asset(inputs={\"data\": processed})')
lines.append('def final_result(data: dict) -> dict:')
lines.append('    return {\"val\": compute(data[\"val\"])}')
lines.append('')
print('\n'.join(lines))
" > "$file"
    echo "$file"
}

# ─── Mutation functions ──────────────────────────────────────────────────────

mutate_random_node() {
    local file=$1
    # Change the FIRST node's return value to guarantee output changes propagate
    python3 -c "
import random, re
random.seed()
with open('$file') as f:
    content = f.read()
# Find the first 'return {' and change its first number
m = re.search(r'(def node_000\(\)[^}]*?\"val\": )(\d+)', content)
if m:
    new_num = str(int(m.group(2)) + random.randint(100, 999))
    content = content[:m.start(2)] + new_num + content[m.end(2):]
else:
    # Diamond: change source's value
    m = re.search(r'(def source\(\)[^}]*?\"val\": )(\d+)', content)
    if m:
        new_num = str(int(m.group(2)) + random.randint(100, 999))
        content = content[:m.start(2)] + new_num + content[m.end(2):]
with open('$file', 'w') as f:
    f.write(content)
" 2>/dev/null
}

mutate_helper() {
    local file=$1
    # Change the SCALE constant to a very different value
    python3 -c "
import random, re
random.seed()
with open('$file') as f:
    content = f.read()
if 'SCALE = ' in content:
    old = re.search(r'SCALE = (\d+)', content).group(1)
    # Always make it significantly different
    new_val = int(old) + random.randint(100, 999)
    content = content.replace(f'SCALE = {old}', f'SCALE = {new_val}', 1)
    with open('$file', 'w') as f:
        f.write(content)
" 2>/dev/null
}

# ─── Fuzz test loop ─────────────────────────────────────────────────────────

echo "Running $ITERATIONS fuzz iterations..."
echo ""

for i in $(seq 1 "$ITERATIONS"); do
    # Pick a random DAG shape
    SHAPE=$((RANDOM % 3))
    rm -f "$REPO_ROOT/.barca/metadata.db"

    case $SHAPE in
        0)
            # Linear chain (5-15 nodes)
            N=$(( (RANDOM % 11) + 5 ))
            FILE=$(generate_linear_chain $N)
            TARGET="node_$(printf '%03d' $((N-1)))"
            DESC="linear($N)"
            ;;
        1)
            # Diamond (3-8 width)
            W=$(( (RANDOM % 6) + 3 ))
            FILE=$(generate_diamond $W)
            TARGET="merge"
            DESC="diamond($W)"
            ;;
        2)
            # Helpers
            FILE=$(generate_with_helpers)
            TARGET="final_result"
            DESC="helpers"
            ;;
    esac

    # First run: execute everything
    OUT1=$($BARCA get "$TARGET" "$FILE" -o value --json 2>/dev/null)
    S1=$(steps "$OUT1")
    V1=$(output "$OUT1")

    # Second run: should be fully cached
    OUT2=$($BARCA get "$TARGET" "$FILE" -o value --json 2>/dev/null)
    S2=$(steps "$OUT2")
    V2=$(output "$OUT2")

    if [ "$S2" != "0" ]; then
        fail "[$i] $DESC: second run not cached (got $S2 steps)"
        continue
    fi
    if [ "$V1" != "$V2" ]; then
        fail "[$i] $DESC: cached output differs from first run"
        continue
    fi

    # Mutate a random node
    if [ "$SHAPE" = "2" ]; then
        mutate_helper "$FILE"
    else
        mutate_random_node "$FILE"
    fi

    # Third run: should re-execute SOMETHING (mutation invalidates)
    OUT3=$($BARCA get "$TARGET" "$FILE" -o value --json 2>/dev/null)
    S3=$(steps "$OUT3")
    V3=$(output "$OUT3")

    if [ "$S3" = "0" ]; then
        fail "[$i] $DESC: mutation not detected (still 0 steps after change)"
        continue
    fi

    # Output should differ (we changed a value)
    if [ "$V3" = "$V1" ]; then
        fail "[$i] $DESC: output unchanged after mutation (should differ)"
        continue
    fi

    # Fourth run: should be cached again (mutation is stable)
    OUT4=$($BARCA get "$TARGET" "$FILE" -o value --json 2>/dev/null)
    S4=$(steps "$OUT4")
    V4=$(output "$OUT4")

    if [ "$S4" != "0" ]; then
        fail "[$i] $DESC: not cached after mutation stabilized (got $S4 steps)"
        continue
    fi
    if [ "$V3" != "$V4" ]; then
        fail "[$i] $DESC: unstable output after mutation"
        continue
    fi

    pass
done

echo ""
echo "Fuzz results: $PASS passed, $FAIL failed (of $ITERATIONS iterations)"
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
