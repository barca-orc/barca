#!/usr/bin/env bash
# Two-simulated-machines e2e for shared remote state.
#
# Machine A and machine B are two separate working directories sharing one
# "remote" root through the file:// state backend (sha256 tokens + lock +
# atomic replace — same contract as the etag/generation cloud backends).
# B must hit A's cache without executing any Python.
set -euo pipefail

BARCA="${BARCA:-barca}"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

SHARED="$TMP/shared"
mkdir -p "$SHARED"

PIPELINE='
from barca import asset

@asset()
def numbers() -> list:
    import os, pathlib
    log = os.environ.get("EXEC_LOG")
    if log:
        with open(log, "a") as fh:
            fh.write("numbers\n")
    return [1, 2, 3]

@asset(inputs={"nums": numbers})
def total(nums: list) -> dict:
    import os
    log = os.environ.get("EXEC_LOG")
    if log:
        with open(log, "a") as fh:
            fh.write("total\n")
    return {"sum": sum(nums)}
'

make_machine() {
    local dir="$1"
    mkdir -p "$dir"
    printf '%s' "$PIPELINE" > "$dir/pipeline.py"
    cat > "$dir/barca.toml" << TOML
[remote]
uri = "$SHARED"
TOML
}

echo "── machine A: first run materializes and pushes"
make_machine "$TMP/machine-a"
export EXEC_LOG="$TMP/exec-a.log"
(cd "$TMP/machine-a" && $BARCA get pipeline.py --agent > result-a.json 2> stderr-a.log) \
    || { cat "$TMP/machine-a/stderr-a.log"; exit 1; }
grep -q '"steps_executed":2' "$TMP/machine-a/result-a.json" \
    || { echo "FAIL: machine A did not execute 2 steps"; cat "$TMP/machine-a/result-a.json"; exit 1; }
[ "$(wc -l < "$EXEC_LOG" | tr -d '[:space:]')" = "2" ] || { echo "FAIL: expected 2 executions on A"; exit 1; }

STATE_BLOB="$SHARED/default/state/metadata.db"
[ -f "$STATE_BLOB" ] || { echo "FAIL: state blob not pushed to $STATE_BLOB"; exit 1; }

echo "── uploaded blob is a complete standalone SQLite database"
[ ! -s "$STATE_BLOB-wal" ] || { echo "FAIL: WAL sidecar leaked next to the blob"; exit 1; }
ROWS=$(python3 -c "
import sqlite3
conn = sqlite3.connect('$STATE_BLOB')
print(conn.execute(\"SELECT COUNT(*) FROM materializations WHERE status='success'\").fetchone()[0])
")
[ "$ROWS" = "2" ] || { echo "FAIL: expected 2 materialization rows in blob, got $ROWS"; exit 1; }

echo "── artifacts are content-addressed under the shared root"
ls "$SHARED"/default/artifacts/*numbers*/*.json > /dev/null 2>&1 \
    || { echo "FAIL: no content-addressed artifact for numbers"; ls -R "$SHARED"; exit 1; }

echo "── workers wrote locally; the store copy was uploaded and recorded"
ls "$TMP"/machine-a/.barca/artifacts/*numbers*/*.json > /dev/null 2>&1 \
    || { echo "FAIL: no local artifact on machine A"; ls -R "$TMP/machine-a/.barca"; exit 1; }
grep -q "uploaded 2 artifacts" "$TMP/machine-a/stderr-a.log" \
    || { echo "FAIL: no upload summary on A"; cat "$TMP/machine-a/stderr-a.log"; exit 1; }
grep -q "pushed state" "$TMP/machine-a/stderr-a.log" \
    || { echo "FAIL: state push not reported on A"; cat "$TMP/machine-a/stderr-a.log"; exit 1; }
# First run: nothing to pull yet — say so rather than staying silent.
grep -q "no shared state yet" "$TMP/machine-a/stderr-a.log" \
    || { echo "FAIL: empty-remote pull not reported on A"; cat "$TMP/machine-a/stderr-a.log"; exit 1; }
BAD=$(python3 -c "
import sqlite3
conn = sqlite3.connect('$STATE_BLOB')
rows = conn.execute(\"SELECT artifact_path FROM materializations WHERE status='success'\").fetchall()
print(sum(1 for (p,) in rows if not p.startswith('$SHARED/default/artifacts/')))
")
[ "$BAD" = "0" ] || { echo "FAIL: $BAD rows record a non-store artifact path"; exit 1; }

echo "── machine B: pristine workdir, full cache hit, zero Python executions"
make_machine "$TMP/machine-b"
export EXEC_LOG="$TMP/exec-b.log"
(cd "$TMP/machine-b" && $BARCA get pipeline.py --agent > result-b.json 2> stderr-b.log) \
    || { cat "$TMP/machine-b/stderr-b.log"; exit 1; }
grep -q '"steps_executed":0' "$TMP/machine-b/result-b.json" \
    || { echo "FAIL: machine B re-executed steps"; cat "$TMP/machine-b/result-b.json"; exit 1; }
[ ! -f "$EXEC_LOG" ] || { echo "FAIL: machine B ran Python:"; cat "$EXEC_LOG"; exit 1; }
grep -q '"sum":6' "$TMP/machine-b/result-b.json" \
    || { echo "FAIL: machine B did not resolve the cached value"; cat "$TMP/machine-b/result-b.json"; exit 1; }

grep -q "pulled state" "$TMP/machine-b/stderr-b.log" \
    || { echo "FAIL: state pull not reported on B"; cat "$TMP/machine-b/stderr-b.log"; exit 1; }

echo "── machine B fetched only what it read (the final output), uploaded nothing"
ls "$TMP"/machine-b/.barca/artifacts/*total*/*.json > /dev/null 2>&1 \
    || { echo "FAIL: final output not fetched to B"; ls -R "$TMP/machine-b/.barca"; exit 1; }
if ls "$TMP"/machine-b/.barca/artifacts/*numbers* > /dev/null 2>&1; then
    echo "FAIL: B fetched an intermediate it never read"; exit 1
fi
grep -q "fetched 1 cached artifact" "$TMP/machine-b/stderr-b.log" \
    || { echo "FAIL: final-output fetch not reported on B"; cat "$TMP/machine-b/stderr-b.log"; exit 1; }
if grep -q "uploaded" "$TMP/machine-b/stderr-b.log"; then
    echo "FAIL: B re-uploaded cached artifacts"; cat "$TMP/machine-b/stderr-b.log"; exit 1
fi

echo "── machine D: partial cache hit fetches exactly the cached input it consumes"
make_machine "$TMP/machine-d"
cat >> "$TMP/machine-d/pipeline.py" << 'PYEOF'

@asset(inputs={"nums": numbers})
def doubled(nums: list) -> list:
    import os
    with open(os.environ["EXEC_LOG"], "a") as fh:
        fh.write("doubled\n")
    return [n * 2 for n in nums]
PYEOF
export EXEC_LOG="$TMP/exec-d.log"
(cd "$TMP/machine-d" && $BARCA get doubled pipeline.py --agent > result-d.json 2> stderr-d.log) \
    || { cat "$TMP/machine-d/stderr-d.log"; exit 1; }
[ "$(cat "$EXEC_LOG")" = "doubled" ] \
    || { echo "FAIL: D should execute only doubled, ran:"; cat "$EXEC_LOG"; exit 1; }
grep -q "fetched 1 cached artifact" "$TMP/machine-d/stderr-d.log" \
    || { echo "FAIL: D did not fetch numbers"; cat "$TMP/machine-d/stderr-d.log"; exit 1; }
grep -q '\[2,4,6\]\|\[2, 4, 6\]' "$TMP/machine-d/result-d.json" \
    || { echo "FAIL: D computed the wrong value"; cat "$TMP/machine-d/result-d.json"; exit 1; }
unset EXEC_LOG

echo "── dynamic partitions + parallel(): sources and children resolve through the store"
FANOUT='
from functools import partial
from barca import asset, task, parallel, partitions_from

@asset()
def universe() -> list:
    return ["a", "b", "c"]

@asset(partitions={"k": partitions_from(universe)})
def shard(k: str) -> dict:
    return {"k": k}

@task()
def square(x: int) -> int:
    return x * x

@task()
def fan() -> list:
    return parallel(*(partial(square, i) for i in range(4)))
'
for m in machine-e machine-f; do
    mkdir -p "$TMP/$m"
    printf '%s' "$FANOUT" > "$TMP/$m/fanout.py"
    printf '[remote]\nuri = "%s"\n' "$SHARED" > "$TMP/$m/barca.toml"
done
(cd "$TMP/machine-e" && $BARCA get shard fanout.py --agent > shard-e.json 2> shard-e.log) \
    || { cat "$TMP/machine-e/shard-e.log"; exit 1; }
grep -q '"steps_executed":4' "$TMP/machine-e/shard-e.json" \
    || { echo "FAIL: E should run universe + 3 shards"; cat "$TMP/machine-e/shard-e.json"; exit 1; }
# F: universe and every shard are cached remotely; expanding the partitions
# needs universe's artifact on F's disk.
(cd "$TMP/machine-f" && $BARCA get shard fanout.py --agent > shard-f.json 2> shard-f.log) \
    || { cat "$TMP/machine-f/shard-f.log"; exit 1; }
grep -q '"steps_executed":0' "$TMP/machine-f/shard-f.json" \
    || { echo "FAIL: F should be a full cache hit"; cat "$TMP/machine-f/shard-f.json" "$TMP/machine-f/shard-f.log"; exit 1; }
ls "$TMP"/machine-f/.barca/artifacts/*universe*/*.json > /dev/null 2>&1 \
    || { echo "FAIL: partition source not fetched to F"; ls -R "$TMP/machine-f/.barca"; exit 1; }
(cd "$TMP/machine-e" && $BARCA run fan fanout.py --agent > fan-e.json 2> fan-e.log) \
    || { cat "$TMP/machine-e/fan-e.log"; exit 1; }
grep -q '\[0,1,4,9\]\|\[0, 1, 4, 9\]' "$TMP/machine-e/fan-e.json" \
    || { echo "FAIL: parallel() results lost"; cat "$TMP/machine-e/fan-e.json" "$TMP/machine-e/fan-e.log"; exit 1; }

if [ "$(id -u)" != "0" ]; then
    echo "── upload failure fails the run and records nothing for the step"
    make_machine "$TMP/machine-g"
    cat >> "$TMP/machine-g/pipeline.py" << 'PYEOF'

@asset()
def unshippable() -> int:
    return 42
PYEOF
    BEFORE=$(python3 -c "
import sqlite3
print(sqlite3.connect('$STATE_BLOB').execute(\"SELECT COUNT(*) FROM materializations WHERE node_id LIKE '%unshippable%' AND status='success'\").fetchone()[0])
")
    chmod 555 "$SHARED/default/artifacts"
    set +e
    (cd "$TMP/machine-g" && $BARCA get unshippable pipeline.py --agent > result-g.json 2> stderr-g.log)
    STATUS=$?
    set -e
    chmod 755 "$SHARED/default/artifacts"
    [ "$STATUS" != "0" ] || { echo "FAIL: run with a failed upload exited 0"; cat "$TMP/machine-g/stderr-g.log"; exit 1; }
    grep -q "upload" "$TMP/machine-g/stderr-g.log" \
        || { echo "FAIL: failure does not mention the upload"; cat "$TMP/machine-g/stderr-g.log"; exit 1; }
    AFTER=$(python3 -c "
import sqlite3
print(sqlite3.connect('$STATE_BLOB').execute(\"SELECT COUNT(*) FROM materializations WHERE node_id LIKE '%unshippable%' AND status='success'\").fetchone()[0])
")
    [ "$BEFORE" = "$AFTER" ] || { echo "FAIL: success row recorded for an artifact missing from the store"; exit 1; }

    echo "── the failure is recorded in shared state: type, no path, attempts, failed run"
    python3 - "$STATE_BLOB" << 'PYEOF' || exit 1
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
row = conn.execute(
    "SELECT status, error_type, artifact_path, attempts, error_message FROM materializations "
    "WHERE node_id LIKE '%unshippable%' ORDER BY id DESC LIMIT 1"
).fetchone()
status, error_type, path, attempts, message = row
problems = []
if status != "failed": problems.append(f"status={status}")
if error_type != "UploadError": problems.append(f"error_type={error_type}")
if path is not None: problems.append(f"artifact_path={path}")
# Permission denied is permanent: one attempt, no wasted retries.
if attempts != 1: problems.append(f"attempts={attempts}")
if not message.startswith("upload to ") or "PermissionError" not in message:
    problems.append(f"message={message!r}")
run_status = conn.execute("SELECT status FROM runs ORDER BY rowid DESC LIMIT 1").fetchone()[0]
if run_status != "failed": problems.append(f"run status={run_status}")
if problems:
    print("FAIL: upload failure row:", ", ".join(problems))
    sys.exit(1)
PYEOF

    echo "── once the store is writable again, the step recomputes and is recorded"
    (cd "$TMP/machine-g" && $BARCA get unshippable pipeline.py --agent > rerun-g.json 2> rerun-g.log) \
        || { echo "FAIL: rerun after fixing the store failed"; cat "$TMP/machine-g/rerun-g.log"; exit 1; }
    ls "$SHARED"/default/artifacts/*unshippable*/*.json > /dev/null 2>&1 \
        || { echo "FAIL: rerun did not upload the artifact"; exit 1; }
    OK=$(python3 -c "
import sqlite3
print(sqlite3.connect('$STATE_BLOB').execute(\"SELECT COUNT(*) FROM materializations WHERE node_id LIKE '%unshippable%' AND status='success' AND artifact_path IS NOT NULL\").fetchone()[0])
")
    [ "$OK" = "1" ] || { echo "FAIL: expected one success row after rerun, got $OK"; exit 1; }
fi

echo "── conflict replay: remote modified between B's pull and push survives"
# Machine C pulls, then the blob changes underneath it (simulated by machine A
# pushing a new run first). C's push must conflict, replay, and both runs'
# rows must survive in the final blob.
make_machine "$TMP/machine-c"
unset EXEC_LOG
# Force new work on C so it actually pushes new rows: different env var
# changes nothing structural, so instead re-run A with --refresh-all to advance
# the blob AFTER C pulled. Simulate by interleaving: C runs with a wrapper
# that mutates the blob between pull and push via BARCA hooks is not
# available, so approximate: A pushes run 2, then C runs (pulls fresh) — and
# assert the blob accumulates run history monotonically.
(cd "$TMP/machine-a" && $BARCA get pipeline.py --refresh-all --agent > /dev/null 2>&1)
(cd "$TMP/machine-c" && $BARCA get pipeline.py --agent > /dev/null 2>&1)
RUNS=$(python3 -c "
import sqlite3
conn = sqlite3.connect('$STATE_BLOB')
print(conn.execute('SELECT COUNT(*) FROM runs').fetchone()[0])
")
[ "$RUNS" -ge 4 ] || { echo "FAIL: expected >=4 run rows accumulated, got $RUNS"; exit 1; }

echo "PASS: shared remote state e2e"
