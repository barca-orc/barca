#!/usr/bin/env bash
# --env separates cache/state per environment.
set -euo pipefail

BARCA="${BARCA:-barca}"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/pipeline.py" << 'PY'
from barca import asset

@asset()
def value() -> dict:
    return {"v": 1}
PY

cd "$TMP"

echo "── default env uses legacy layout"
$BARCA get pipeline.py --agent > /dev/null 2>&1
[ -f .barca/metadata.db ] || { echo "FAIL: no default-env DB"; exit 1; }

echo "── named env is fully separated"
$BARCA get pipeline.py --env dev --agent > out-dev.json 2> /dev/null
[ -f .barca/envs/dev/metadata.db ] || { echo "FAIL: no dev-env DB"; exit 1; }
grep -q '"steps_executed":1' out-dev.json \
    || { echo "FAIL: dev env should not share default's cache"; cat out-dev.json; exit 1; }
ls .barca/envs/dev/artifacts/*value*/*.json > /dev/null 2>&1 \
    || { echo "FAIL: dev artifacts not under env dir"; ls -R .barca; exit 1; }

echo "── env cache is warm on second run"
$BARCA get pipeline.py --env dev --agent > out-dev2.json 2> /dev/null
grep -q '"steps_executed":0' out-dev2.json || { echo "FAIL: dev cache miss"; exit 1; }

echo "── history is scoped per env"
DEV_RUNS=$($BARCA history --env dev --pretty | grep -c "get" || true)
DEFAULT_RUNS=$($BARCA history --pretty | grep -c "get" || true)
[ "$DEV_RUNS" = "2" ] || { echo "FAIL: dev history should have 2 runs, got $DEV_RUNS"; exit 1; }
[ "$DEFAULT_RUNS" = "1" ] || { echo "FAIL: default history should have 1 run, got $DEFAULT_RUNS"; exit 1; }

echo "── BARCA_ENV env var works"
BARCA_ENV=staging $BARCA get pipeline.py --agent > /dev/null 2>&1
[ -f .barca/envs/staging/metadata.db ] || { echo "FAIL: BARCA_ENV ignored"; exit 1; }

echo "PASS: env separation e2e"
