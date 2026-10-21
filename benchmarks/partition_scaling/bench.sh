#!/usr/bin/env bash
# Partition scaling (#345): a three-level partition chain at several key counts, cold (fresh
# .barca/) and warm (everything cached). Barca only. Prints wall time and the coordinator's
# peak RSS per case; the cold time should grow about linearly with the key count.
#
#   benchmarks/partition_scaling/bench.sh              # 2000 and 20000 keys
#   KEYS="2000 5000 20000" BARCA_POOL_SIZE=8 benchmarks/partition_scaling/bench.sh
#   BARCA_TRACE_TIMING=1 ...                           # per-stage timings on stderr logs
#   FAIL_HALF=1 KEYS="2000 20000" benchmarks/partition_scaling/bench.sh   # half the keys fail (#461)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BARCA="${BARCA:-barca}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "  BENCHMARK: partition scaling (3 assets x N keys), pool ${BARCA_POOL_SIZE:-default}${FAIL_HALF:+, half the keys fail}"
for n in ${KEYS:-2000 20000}; do
    python3 "$SCRIPT_DIR/generate.py" "$n" "$WORK/k$n"
    for mode in cold warm; do
        [[ $mode == cold ]] && rm -rf "$WORK/k$n/.barca"
        # With FAIL_HALF=1 the run exits 1 (a step failed); anything else is an error.
        status=0
        (cd "$WORK/k$n" && /usr/bin/time -f "%e %M" -o "$WORK/time" \
            "$BARCA" get score assets.py >/dev/null 2>"$WORK/k$n/$mode.log") || status=$?
        if [[ $status -ne 0 && ! ($status -eq 1 && ${FAIL_HALF:-} == 1) ]]; then
            echo "  barca exited $status; see $WORK/k$n/$mode.log" >&2
            exit "$status"
        fi
        # /usr/bin/time puts "Command exited with non-zero status" above the numbers.
        read -r secs kb < <(tail -n 1 "$WORK/time")
        printf '  %6s keys %-4s %8.2f s  coordinator peak RSS %6d MB\n' "$n" "$mode" "$secs" "$((kb / 1024))"
    done
done
