#!/usr/bin/env bash
# Partitioned-asset execution correctness: a partitioned consumer's `inputs={}`
# wiring to its partitioned producer must resolve to the *same* partition's
# output, for both static (`partitions()`) and dynamic (`partitions_from()`)
# partition sources.
#
# Regression coverage for a bug where Coordinator::load_phase resolved a
# partitioned step's upstream dependency against a lookup table that was only
# populated for items visited earlier in stream order. That's correct for the
# planner's initial phases, but not for phases rebuilt by
# dispatch::expand_pending_partitions (dynamic partitions_from partitions):
# its load-balancing bin-packing can place a consumer's partition instance in
# a stream processed before its producer's, silently dropping the dependency
# edge — the consumer then runs with a missing input
# (`TypeError: enrich() missing 1 required positional argument: 'data'`).
#
# Run: bash tests/integration/test_partitions.sh
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

steps() { echo "$1" | python3 -c "import json,sys; print(json.load(sys.stdin).get('steps_executed', -1))"; }
final_output() { echo "$1" | python3 -c "import json,sys; print(json.dumps(json.load(sys.stdin)['final_output']))"; }

# ─── Static partitions + inputs consumer ────────────────────────────────────
echo "=== Static partitions: consumer sees the matching partition's output ==="

cat > "$TMPDIR/static_chain.py" << 'PYEOF'
from barca import asset, partitions

KEYS = ["a", "b", "c"]

@asset(partitions={"k": partitions(KEYS)})
def fetch(k: str) -> dict:
    return {"k": k, "v": 1}

@asset(inputs={"data": fetch}, partitions={"k": partitions(KEYS)})
def enrich(data: dict, k: str) -> dict:
    return {**data, "k2": k}
PYEOF

OUT=$(cd "$TMPDIR" && rm -rf .barca && $BARCA get enrich "$TMPDIR/static_chain.py" --refresh-all 2>/dev/null || true)
S=$(steps "$OUT" 2>/dev/null || echo "ERROR")
[ "$S" = "6" ] && pass "static partitions + inputs: all 6 steps execute" || fail "static partitions + inputs: expected 6 steps, got $S ($OUT)"

# Every enrich[k]'s output must carry the matching fetch[k]'s data (k == k2),
# not another partition's — this is what a dropped/misaligned dependency edge
# would silently get wrong.
FO=$(final_output "$OUT" 2>/dev/null || echo "{}")
MATCH=$(echo "$FO" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('k') == d.get('k2'))" 2>/dev/null || echo "False")
[ "$MATCH" = "True" ] && pass "static partitions + inputs: consumer matches its own partition" || fail "static partitions + inputs: partition mismatch in $FO"

# ─── Dynamic partitions (partitions_from) + inputs consumer ────────────────
echo "=== Dynamic partitions: consumer sees the matching partition's output ==="

cat > "$TMPDIR/dynamic_chain.py" << 'PYEOF'
from barca import asset, partitions_from

@asset()
def universe() -> list:
    return ["a", "b", "c"]

@asset(partitions={"k": partitions_from(universe)})
def fetch(k: str) -> dict:
    return {"k": k, "v": 1}

@asset(inputs={"data": fetch}, partitions={"k": partitions_from(universe)})
def enrich(data: dict, k: str) -> dict:
    return {**data, "k2": k}
PYEOF

# dispatch::expand_pending_partitions splits each partitioned step's keys
# into chunk_size = ceil(partition_count / pool_size) pieces, then bin-packs
# chunks across streams by load alone. With 3 partitions and a pool size >=
# partition count, chunk_size == 1: every partition key becomes its own
# work unit, which is exactly the granularity that let the old code's greedy
# bin-packing interleave fetch/enrich chunks — reliable and deterministic
# (the bin-packer has no randomness), not something that needs many repeats
# to hit, but run twice as cheap insurance against unrelated flakiness.
DYNAMIC_OK=1
for i in 1 2; do
    OUT=$(cd "$TMPDIR" && rm -rf .barca && BARCA_POOL_SIZE=4 $BARCA get enrich "$TMPDIR/dynamic_chain.py" --refresh-all 2>/dev/null || true)
    S=$(steps "$OUT" 2>/dev/null || echo "ERROR")
    # 1 universe + 3 fetch + 3 enrich = 7
    if [ "$S" != "7" ]; then
        DYNAMIC_OK=0
        fail "dynamic partitions + inputs (run $i): expected 7 steps, got $S ($OUT)"
    fi
done
[ "$DYNAMIC_OK" = "1" ] && pass "dynamic partitions + inputs: all 7 steps execute across repeat runs"

# Every enrich[k]'s artifact must carry the matching fetch[k]'s data — not
# just whichever instance `final_output` happens to report — since a dropped
# dependency edge on just one partition instance could hide behind the
# others. There's no CLI syntax to target a single dynamic-partition
# instance (they don't exist in the static DAG at parse time), so read the
# materialized artifacts directly.
(cd "$TMPDIR" && rm -rf .barca && BARCA_POOL_SIZE=4 "$BARCA" get enrich "$TMPDIR/dynamic_chain.py" --refresh-all >/dev/null 2>&1) || true
ENRICH_ARTIFACTS=$(find "$TMPDIR/.barca/artifacts" -type f -name "*.json" -path "*enrich*" 2>/dev/null)
ENRICH_COUNT=$(echo "$ENRICH_ARTIFACTS" | grep -c . || true)
ALL_MATCH=1
for f in $ENRICH_ARTIFACTS; do
    M=$(python3 -c "import json; d=json.load(open('$f')); print(d.get('k') == d.get('k2'))" 2>/dev/null || echo "False")
    [ "$M" = "True" ] || ALL_MATCH=0
done
[ "$ENRICH_COUNT" = "3" ] && [ "$ALL_MATCH" = "1" ] \
    && pass "dynamic partitions + inputs: all 3 partition artifacts match their own producer" \
    || fail "dynamic partitions + inputs: expected 3 matching enrich artifacts, found $ENRICH_COUNT (all_match=$ALL_MATCH)"

# ─── Summary ─────────────────────────────────────────────────────────────────
echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
