#!/usr/bin/env bash
# Partition scaling (#345): a three-level partition chain at several key counts, cold (fresh
# .barca/) and warm (everything cached). Barca only. Prints wall time and the coordinator's
# peak RSS per case; the cold time should grow about linearly with the key count.
#
#   benchmarks/partition_scaling/bench.sh              # 2000 and 20000 keys
#   KEYS="2000 5000 20000" BARCA_POOL_SIZE=8 benchmarks/partition_scaling/bench.sh
#   BARCA_TRACE_TIMING=1 ...                           # per-stage timings on stderr logs
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BARCA="${BARCA:-barca}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "  BENCHMARK: partition scaling (3 assets x N keys), pool ${BARCA_POOL_SIZE:-default}"
for n in ${KEYS:-2000 20000}; do
    python3 "$SCRIPT_DIR/generate.py" "$n" "$WORK/k$n"
    for mode in cold warm; do
        [[ $mode == cold ]] && rm -rf "$WORK/k$n/.barca"
        (cd "$WORK/k$n" && /usr/bin/time -f "%e %M" -o "$WORK/time" \
            "$BARCA" get score assets.py >/dev/null 2>"$WORK/k$n/$mode.log")
        read -r secs kb <"$WORK/time"
        printf '  %6s keys %-4s %8.2f s  coordinator peak RSS %6d MB\n' "$n" "$mode" "$secs" "$((kb / 1024))"
    done
done
