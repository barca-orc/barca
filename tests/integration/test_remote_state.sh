#!/usr/bin/env bash
# A store as this machine's backup and read-through cache (RFC-0009), end to end.
#
# A working directory uses a plain-directory store: artifacts upload, the metadata DB is
# backed up there when a run ends, a machine whose local copies are gone reads its cache hits
# back from the store without executing any Python, and a wiped machine gets its history back
# with `barca state restore`.
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

echo "── machine A: first run materializes, uploads and backs up"
make_machine "$TMP/machine-a"
export EXEC_LOG="$TMP/exec-a.log"
(cd "$TMP/machine-a" && $BARCA get pipeline.py --agent -o value --json > result-a.json 2> stderr-a.log) \
    || { cat "$TMP/machine-a/stderr-a.log"; exit 1; }
grep -q '"steps_executed":2' "$TMP/machine-a/result-a.json" \
    || { echo "FAIL: machine A did not execute 2 steps"; cat "$TMP/machine-a/result-a.json"; exit 1; }
[ "$(wc -l < "$EXEC_LOG" | tr -d '[:space:]')" = "2" ] || { echo "FAIL: expected 2 executions on A"; exit 1; }

[ ! -e "$SHARED/default/state/metadata.db" ] || { echo "FAIL: a shared history object was written"; exit 1; }
BACKUP=$(ls "$SHARED"/default/state/backups/*.db | tail -1)
[ -f "$BACKUP" ] || { echo "FAIL: no backup in the store"; ls -R "$SHARED"; exit 1; }
[ -f "${BACKUP%.db}.json" ] || { echo "FAIL: backup without its sidecar"; exit 1; }
cmp -s "$BACKUP" "$TMP/machine-a/.barca/backups/$(basename "$BACKUP")" \
    || { echo "FAIL: the local and store copies of the backup differ"; exit 1; }

echo "── the backup is a complete standalone SQLite database"
[ ! -e "$BACKUP-wal" ] || { echo "FAIL: WAL sidecar leaked next to the backup"; exit 1; }
ROWS=$(python3 -c "
import sqlite3
conn = sqlite3.connect('$BACKUP')
print(conn.execute(\"SELECT COUNT(*) FROM materializations WHERE status='success'\").fetchone()[0])
")
[ "$ROWS" = "2" ] || { echo "FAIL: expected 2 materialization rows in the backup, got $ROWS"; exit 1; }

echo "── artifacts are content-addressed under the shared root"
ls "$SHARED"/default/artifacts/*numbers*/*.json > /dev/null 2>&1 \
    || { echo "FAIL: no content-addressed artifact for numbers"; ls -R "$SHARED"; exit 1; }

echo "── workers wrote locally; the store copy was uploaded and recorded"
ls "$TMP"/machine-a/.barca/artifacts/*numbers*/*.json > /dev/null 2>&1 \
    || { echo "FAIL: no local artifact on machine A"; ls -R "$TMP/machine-a/.barca"; exit 1; }
grep -q "uploaded 2 artifacts" "$TMP/machine-a/stderr-a.log" \
    || { echo "FAIL: no upload summary on A"; cat "$TMP/machine-a/stderr-a.log"; exit 1; }
grep -q "backed up the metadata DB" "$TMP/machine-a/stderr-a.log" \
    || { echo "FAIL: backup not reported on A"; cat "$TMP/machine-a/stderr-a.log"; exit 1; }
BAD=$(python3 -c "
import sqlite3
conn = sqlite3.connect('$BACKUP')
rows = conn.execute(\"SELECT artifact_path FROM materializations WHERE status='success'\").fetchall()
print(sum(1 for (p,) in rows if not p.startswith('$SHARED/default/artifacts/')))
")
[ "$BAD" = "0" ] || { echo "FAIL: $BAD rows record a non-store artifact path"; exit 1; }

echo "── machine A without its local copies: full cache hit, zero Python executions"
rm -rf "$TMP/machine-a/.barca/artifacts"
mkdir -p "$TMP/machine-b"
export EXEC_LOG="$TMP/exec-b.log"
(cd "$TMP/machine-a" && $BARCA get pipeline.py --agent -o value --json > "$TMP/machine-b/result-b.json" 2> "$TMP/machine-b/stderr-b.log") \
    || { cat "$TMP/machine-b/stderr-b.log"; exit 1; }
grep -q '"steps_executed":0' "$TMP/machine-b/result-b.json" \
    || { echo "FAIL: machine B re-executed steps"; cat "$TMP/machine-b/result-b.json"; exit 1; }
[ ! -f "$EXEC_LOG" ] || { echo "FAIL: machine B ran Python:"; cat "$EXEC_LOG"; exit 1; }
grep -q '"sum":6' "$TMP/machine-b/result-b.json" \
    || { echo "FAIL: machine B did not resolve the cached value"; cat "$TMP/machine-b/result-b.json"; exit 1; }

echo "── it fetched only what it read (the final output), uploaded nothing"
ls "$TMP"/machine-a/.barca/artifacts/*total*/*.json > /dev/null 2>&1 \
    || { echo "FAIL: final output not fetched"; ls -R "$TMP/machine-a/.barca"; exit 1; }
if ls "$TMP"/machine-a/.barca/artifacts/*numbers* > /dev/null 2>&1; then
    echo "FAIL: an intermediate it never read was fetched"; exit 1
fi
grep -q "fetched 1 cached artifact" "$TMP/machine-b/stderr-b.log" \
    || { echo "FAIL: final-output fetch not reported on B"; cat "$TMP/machine-b/stderr-b.log"; exit 1; }
if grep -q "uploaded" "$TMP/machine-b/stderr-b.log"; then
    echo "FAIL: cached artifacts were uploaded again"; cat "$TMP/machine-b/stderr-b.log"; exit 1
fi

echo "── machine D: partial cache hit fetches exactly the cached input it consumes"
# The same history as A, restored from the store into a new working directory.
make_machine "$TMP/machine-d"
(cd "$TMP/machine-d" && $BARCA state restore > /dev/null 2> restore-d.log) \
    || { cat "$TMP/machine-d/restore-d.log"; exit 1; }
grep -q "from the store copy" "$TMP/machine-d/restore-d.log" \
    || { echo "FAIL: D did not restore from the store"; cat "$TMP/machine-d/restore-d.log"; exit 1; }
cat >> "$TMP/machine-d/pipeline.py" << 'PYEOF'

@asset(inputs={"nums": numbers})
def doubled(nums: list) -> list:
    import os
    with open(os.environ["EXEC_LOG"], "a") as fh:
        fh.write("doubled\n")
    return [n * 2 for n in nums]
PYEOF
export EXEC_LOG="$TMP/exec-d.log"
(cd "$TMP/machine-d" && $BARCA get doubled pipeline.py --agent -o value --json > result-d.json 2> stderr-d.log) \
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
for m in machine-e; do
    mkdir -p "$TMP/$m"
    printf '%s' "$FANOUT" > "$TMP/$m/fanout.py"
    printf '[remote]\nuri = "%s"\n' "$SHARED" > "$TMP/$m/barca.toml"
done
(cd "$TMP/machine-e" && $BARCA get shard fanout.py --agent -o value --json > shard-e.json 2> shard-e.log) \
    || { cat "$TMP/machine-e/shard-e.log"; exit 1; }
grep -q '"steps_executed":4' "$TMP/machine-e/shard-e.json" \
    || { echo "FAIL: E should run universe + 3 shards"; cat "$TMP/machine-e/shard-e.json"; exit 1; }
# Without its local copies: universe and every shard are cached in the store; expanding the
# partitions needs universe's artifact on this disk.
rm -rf "$TMP/machine-e/.barca/artifacts"
(cd "$TMP/machine-e" && $BARCA get shard fanout.py --agent -o value --json > shard-f.json 2> shard-f.log) \
    || { cat "$TMP/machine-e/shard-f.log"; exit 1; }
grep -q '"steps_executed":0' "$TMP/machine-e/shard-f.json" \
    || { echo "FAIL: should be a full cache hit"; cat "$TMP/machine-e/shard-f.json" "$TMP/machine-e/shard-f.log"; exit 1; }
ls "$TMP"/machine-e/.barca/artifacts/*universe*/*.json > /dev/null 2>&1 \
    || { echo "FAIL: partition source not fetched"; ls -R "$TMP/machine-e/.barca"; exit 1; }
(cd "$TMP/machine-e" && $BARCA run fan fanout.py --agent > fan-e.json 2> fan-e.log) \
    || { cat "$TMP/machine-e/fan-e.log"; exit 1; }
grep -q '\[0,1,4,9\]\|\[0, 1, 4, 9\]' "$TMP/machine-e/fan-e.json" \
    || { echo "FAIL: parallel() results lost"; cat "$TMP/machine-e/fan-e.json" "$TMP/machine-e/fan-e.log"; exit 1; }

if [ "$(id -u)" != "0" ]; then
    echo "── upload failure after the step succeeded: exit 0, a warning, the result kept pending"
    make_machine "$TMP/machine-g"
    cat >> "$TMP/machine-g/pipeline.py" << 'PYEOF'

@asset()
def unshippable() -> int:
    return 42
PYEOF
    chmod 555 "$SHARED/default/artifacts"
    set +e
    (cd "$TMP/machine-g" && $BARCA get unshippable pipeline.py --agent --json > result-g.json 2> stderr-g.log)
    STATUS=$?
    set -e
    chmod 755 "$SHARED/default/artifacts"
    [ "$STATUS" = "0" ] || { echo "FAIL: run with a failed upload exited $STATUS"; cat "$TMP/machine-g/stderr-g.log"; exit 1; }
    grep -q "warning: 1 result not uploaded" "$TMP/machine-g/stderr-g.log" \
        || { echo "FAIL: no warning names the upload"; cat "$TMP/machine-g/stderr-g.log"; exit 1; }
    python3 - "$TMP/machine-g/result-g.json" "$TMP/machine-g/.barca/metadata.db" "$TMP/machine-g/.barca/metadata.db" << 'PYEOF' || exit 1
import json, sqlite3, sys
result = json.load(open(sys.argv[1]))
assert result["uploads"]["pending"] == 1, result
assert result["uploads"]["nodes"] == ["pipeline.py:unshippable"], result
assert "PermissionError" in result["uploads"]["error"], result
# Recorded as a success at its store location, the run a success, the upload pending.
shared = sqlite3.connect(sys.argv[2])
status, path = shared.execute(
    "SELECT status, artifact_path FROM materializations WHERE node_id LIKE '%unshippable%' "
    "ORDER BY id DESC LIMIT 1"
).fetchone()
assert status == "success" and path.endswith(".json"), (status, path)
assert shared.execute("SELECT status FROM runs ORDER BY rowid DESC LIMIT 1").fetchone()[0] == "success"
local = sqlite3.connect(sys.argv[3])
assert local.execute("SELECT COUNT(*) FROM pending_uploads").fetchone()[0] == 1
PYEOF

    echo "── once the store is writable again, the next run uploads it without recomputing"
    (cd "$TMP/machine-g" && $BARCA get unshippable pipeline.py --agent --json > rerun-g.json 2> rerun-g.log) \
        || { echo "FAIL: rerun after fixing the store failed"; cat "$TMP/machine-g/rerun-g.log"; exit 1; }
    grep -q "uploaded 1 pending result" "$TMP/machine-g/rerun-g.log" \
        || { echo "FAIL: rerun did not retry the pending upload"; cat "$TMP/machine-g/rerun-g.log"; exit 1; }
    ls "$SHARED"/default/artifacts/*unshippable*/*.json > /dev/null 2>&1 \
        || { echo "FAIL: rerun did not upload the artifact"; exit 1; }
    python3 -c "
import json, sqlite3
r = json.load(open('$TMP/machine-g/rerun-g.json'))
assert r['steps_executed'] == 0 and 'uploads' not in r, r
assert sqlite3.connect('$TMP/machine-g/.barca/metadata.db').execute('SELECT COUNT(*) FROM pending_uploads').fetchone()[0] == 0
" || { echo "FAIL: pending upload not cleared"; exit 1; }
fi

echo "── a wiped machine: nothing comes back by itself; barca state restore brings the history"
# Every directory here backs up to the same prefix, so name A's newest backup (the other
# machines' interleave with it: one store per machine is the supported setup). The store keeps
# the five newest across all of them, so A runs once more first and its copy is the newest.
(cd "$TMP/machine-a" && $BARCA get pipeline.py --agent -o value --json > /dev/null 2> last-a.log) \
    || { cat "$TMP/machine-a/last-a.log"; exit 1; }
A_BACKUP=$(basename "$(ls "$TMP"/machine-a/.barca/backups/*.db | tail -1)")
rm -rf "$TMP/machine-a/.barca"
(cd "$TMP/machine-a" && $BARCA state backups --json > backups-a.json 2> backups-a.log) \
    || { cat "$TMP/machine-a/backups-a.log"; exit 1; }
python3 -c "
import json
listed = json.load(open('$TMP/machine-a/backups-a.json'))['backups']
assert listed and all(b['where'] == 'store' for b in listed), listed
" || { echo "FAIL: the store's backups are not listed"; cat "$TMP/machine-a/backups-a.json"; exit 1; }
(cd "$TMP/machine-a" && $BARCA state restore --from "$A_BACKUP" > /dev/null 2> restore-a.log) \
    || { cat "$TMP/machine-a/restore-a.log"; exit 1; }
export EXEC_LOG="$TMP/exec-restored.log"
(cd "$TMP/machine-a" && $BARCA get pipeline.py --agent -o value --json > result-r.json 2> stderr-r.log) \
    || { cat "$TMP/machine-a/stderr-r.log"; exit 1; }
grep -q '"steps_executed":0' "$TMP/machine-a/result-r.json" \
    || { echo "FAIL: the restored history did not serve the cache"; cat "$TMP/machine-a/result-r.json"; exit 1; }
[ ! -f "$EXEC_LOG" ] || { echo "FAIL: ran Python after a restore:"; cat "$EXEC_LOG"; exit 1; }
unset EXEC_LOG

echo "PASS: store backup and read-through cache e2e"
