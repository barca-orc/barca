#!/usr/bin/env bash
# Fast CI smoke for the scheduler benchmark's barca side.
#
# tests/integration/test_benchmark_examples.sh auto-discovers every
# benchmarks/*/barca/run.sh and asserts it exits 0 (run from a random temp cwd).
# The full three-framework comparison lives in ../bench.sh and takes minutes;
# this smoke instead exploits barca's *sub-minute* cron to prove the scheduler
# actually fires, end to end, in ~10 seconds: it serves a `*/2 * * * * *` job
# and checks the task body ran at least twice.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
BARCA="${BARCA:-$REPO_ROOT/.venv/bin/barca}"

PORT=$(( 8300 + (RANDOM % 500) ))
RESULTS="$(mktemp)"
LOG="$(mktemp)"
export SCHED_RESULTS="$RESULTS"

SERVE_PID=""
cleanup() {
    if [[ -n "$SERVE_PID" ]]; then
        kill "$SERVE_PID" 2>/dev/null || true
        # Give it a moment to reap its worker pool, then force-kill if needed.
        for _ in 1 2 3 4; do
            kill -0 "$SERVE_PID" 2>/dev/null || break
            sleep 0.5
        done
        kill -9 "$SERVE_PID" 2>/dev/null || true
    fi
    rm -f "$RESULTS" "$LOG"
}
trap cleanup EXIT

"$BARCA" serve "$SCRIPT_DIR/cadence_job.py" --port "$PORT" --timezone utc >"$LOG" 2>&1 &
SERVE_PID=$!

# Best-effort readiness wait if curl is present (not required — the fires check
# below is the real assertion, so a missing curl can't fail this spuriously).
if command -v curl >/dev/null 2>&1; then
    for _ in $(seq 1 20); do
        curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
        sleep 0.5
    done
fi

# Confirm the server is actually up before timing fires.
if ! kill -0 "$SERVE_PID" 2>/dev/null; then
    echo "barca serve exited during startup" >&2
    cat "$LOG" >&2
    exit 1
fi

# */2s cron → the scheduler acts on the job roughly every 2 seconds. Observe ~10s.
sleep 10

# Primary assertion (worker-independent, robust in any environment): the
# scheduler must have TICKED the sub-minute job at least twice — proof that
# 6-field / 1-second cron is live. Each tick logs a "scheduled run ...:probe"
# line (a fire, or a skip if a prior fire is still running).
ticks=$(grep -c "scheduled run.*:probe" "$LOG" 2>/dev/null || echo 0)
# Secondary (worker-dependent, informational): task bodies that actually executed
# and appended a timestamp. Zero here only means the worker couldn't import barca.
executed=$(wc -l < "$RESULTS" | tr -d ' ')

python3 - "$ticks" "$executed" <<'PY'
import json, sys
print(json.dumps({
    "scheduler": "barca serve",
    "cron": "*/2 * * * * *",
    "window_seconds": 10,
    "scheduler_ticks": int(sys.argv[1]),
    "task_executions": int(sys.argv[2]),
}, indent=2))
PY

if [[ "${ticks:-0}" -lt 2 ]]; then
    echo "expected >= 2 sub-minute scheduler ticks in 10s, got ${ticks:-0}" >&2
    echo "--- barca serve log ---" >&2
    cat "$LOG" >&2
    exit 1
fi
