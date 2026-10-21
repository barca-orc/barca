#!/usr/bin/env bash
# Boot `barca serve` for a measurement window and wait until it's ready.
#   start.sh <job_file> <port>
# Writes the server PID to /tmp/barca_sched_bench.pid. `bench.sh` drives the
# timing/observation; `stop.sh` tears it down. Mirrors the readiness pattern in
# benchmarks/fan_out_500_50ms/dagster_server/start.sh.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
BARCA="${BARCA:-$REPO_ROOT/.venv/bin/barca}"

JOB_FILE="${1:?usage: start.sh <job_file> <port>}"
PORT="${2:?usage: start.sh <job_file> <port>}"

# --timezone utc so cron evaluation is deterministic regardless of host TZ.
${BARCA_BENCH_PIN:-} "$BARCA" serve "$JOB_FILE" --port "$PORT" --timezone utc \
    >"/tmp/barca_sched_bench.log" 2>&1 &
echo $! > /tmp/barca_sched_bench.pid

for _ in $(seq 1 30); do
    if curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
        echo "barca serve ready (pid $(cat /tmp/barca_sched_bench.pid), port $PORT)"
        exit 0
    fi
    sleep 1
done
echo "barca serve failed to start; log:" >&2
cat /tmp/barca_sched_bench.log >&2 || true
exit 1
