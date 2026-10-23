#!/usr/bin/env bash
# Keyed partition sources: one tick of a sensor that returns {retailer: version} for N
# retailers, feeding a two-asset `partitions_from` chain (cleaned -> scored), with one
# retailer's version changed and everything else cached. Compared with a partitioned sensor
# that runs once per retailer. Prints wall time, the coordinator's peak RSS and the steps
# executed per case; also the same tick with nothing changed, `--dry-run` and `barca status`.
#
#   benchmarks/keyed_partitions/bench.sh                         # keyed at 10k, 50k, 200k
#   KEYS="10000 50000" MODES="keyed partitioned" benchmarks/keyed_partitions/bench.sh
#   RUNS=5 BARCA_POOL_SIZE=8 benchmarks/keyed_partitions/bench.sh
#
# At 200k keys a fixture holds about 600k artifact files, so it is made in the current
# directory (a tmpfs /tmp runs out of inodes), or in WORK; each is deleted once measured.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BARCA="${BARCA:-barca}"
WORK="${WORK:-$(mktemp -d "$PWD/keyed-bench.XXXXXX")}"
[[ -z ${KEEP:-} ]] && trap 'rm -rf "$WORK"' EXIT
RUNS="${RUNS:-3}"

# One timed command: prints "seconds peak_MB steps_executed".
timed() {
    local log="$1"
    shift
    /usr/bin/time -f "%e %M" -o "$WORK/time" "$@" >"$WORK/out.json" 2>"$log"
    read -r secs kb < <(tail -n 1 "$WORK/time")
    steps=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('steps_executed','-'))" "$WORK/out.json" 2>/dev/null || echo -)
    echo "$secs $((kb / 1024)) $steps"
}

median() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }

echo "  BENCHMARK: keyed partitions (sensor + cleaned + scored), pool ${BARCA_POOL_SIZE:-default}, median of $RUNS"
printf '  %-12s %7s  %-22s %9s %8s %7s\n' mode keys case wall peakRSS steps
for mode in ${MODES:-keyed}; do
    for n in ${KEYS:-10000 50000 200000}; do
        dir="$WORK/$mode-$n"
        python3 "$SCRIPT_DIR/generate.py" "$n" "$dir" "$mode"
        cd "$dir"
        timed "$dir/cold.log" "$BARCA" get scored pipeline.py --json >"$dir/cold.txt"
        read -r secs mb steps <"$dir/cold.txt"
        printf '  %-12s %7d  %-22s %8.2fs %6dMB %7s\n' "$mode" "$n" "cold" "$secs" "$mb" "$steps"
        for case in "one changed" "nothing changed" "dry-run" "status"; do
            : >"$dir/times"
            for _ in $(seq "$RUNS"); do
                case "$case" in
                    "one changed")
                        python3 bump.py r000001
                        timed "$dir/tick.log" "$BARCA" get scored pipeline.py --json ;;
                    "nothing changed")
                        timed "$dir/tick.log" "$BARCA" get scored pipeline.py --json ;;
                    "dry-run")
                        python3 bump.py r000002
                        "$BARCA" get versions pipeline.py >/dev/null 2>&1
                        timed "$dir/dry.log" "$BARCA" get scored pipeline.py --dry-run --json ;;
                    "status")
                        timed "$dir/status.log" "$BARCA" status pipeline.py --json ;;
                esac >>"$dir/times"
            done
            secs=$(cut -d' ' -f1 "$dir/times" | median)
            mb=$(cut -d' ' -f2 "$dir/times" | median)
            steps=$(tail -n 1 "$dir/times" | cut -d' ' -f3)
            printf '  %-12s %7d  %-22s %8.2fs %6dMB %7s\n' "$mode" "$n" "$case" "$secs" "$mb" "$steps"
        done
        # Leave the next case a clean slate.
        "$BARCA" get scored pipeline.py >/dev/null 2>&1
        cd - >/dev/null
        rm -rf "$dir"
    done
done
