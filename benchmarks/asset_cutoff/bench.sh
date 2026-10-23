#!/usr/bin/env bash
# Early cutoff (#628): a global asset every customer reads reruns and writes the same bytes.
# With `@asset(cutoff=True)` the 2N partition keys below it stay cached; without it they all
# rerun. Also times a tick where nothing changed (the planning cost of the extra phase a cutoff
# asset takes), `--dry-run` and `barca status` after the feed moved, and counts the plan's phases.
# Prints wall time, the coordinator's peak RSS and the steps executed per case.
#
#   benchmarks/asset_cutoff/bench.sh                              # cutoff and plain at 10k
#   KEYS="10000 200000" MODES="cutoff plain" benchmarks/asset_cutoff/bench.sh
#   RUNS=1 KEYS=200000 MODES=plain benchmarks/asset_cutoff/bench.sh
#
# At 200k keys a fixture holds about 400k artifact files, so it is made in the current
# directory (a tmpfs /tmp runs out of inodes), or in WORK; each is deleted once measured.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BARCA="${BARCA:-barca}"
WORK="${WORK:-$(mktemp -d "$PWD/cutoff-bench.XXXXXX")}"
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

echo "  BENCHMARK: asset cutoff (feed -> rates -> billed, invoice), pool ${BARCA_POOL_SIZE:-default}, median of $RUNS"
printf '  %-7s %7s  %-22s %9s %8s %7s\n' mode keys case wall peakRSS steps
for mode in ${MODES:-cutoff plain}; do
    for n in ${KEYS:-10000}; do
        dir="$WORK/$mode-$n"
        python3 "$SCRIPT_DIR/generate.py" "$n" "$dir" "$mode"
        cd "$dir"
        phases=$("$BARCA" plan pipeline.py | python3 -c "import json,sys; print(len(json.load(sys.stdin)['phases']))")
        printf '  %-7s %7d  %-22s %9s\n' "$mode" "$n" "plan phases" "$phases"
        timed "$dir/cold.log" "$BARCA" get invoice pipeline.py --json >"$dir/cold.txt"
        read -r secs mb steps <"$dir/cold.txt"
        printf '  %-7s %7d  %-22s %8.2fs %6dMB %7s\n' "$mode" "$n" "cold" "$secs" "$mb" "$steps"
        for case in "global reran, same" "nothing changed" "dry-run" "status"; do
            : >"$dir/times"
            for _ in $(seq "$RUNS"); do
                case "$case" in
                    "global reran, same")
                        python3 bump.py
                        timed "$dir/tick.log" "$BARCA" get invoice pipeline.py --json ;;
                    "nothing changed")
                        timed "$dir/tick.log" "$BARCA" get invoice pipeline.py --json ;;
                    "dry-run")
                        python3 bump.py
                        "$BARCA" get feed pipeline.py >/dev/null 2>&1
                        timed "$dir/dry.log" "$BARCA" get invoice pipeline.py --dry-run --json ;;
                    "status")
                        timed "$dir/status.log" "$BARCA" status pipeline.py --json ;;
                esac >>"$dir/times"
            done
            secs=$(cut -d' ' -f1 "$dir/times" | median)
            mb=$(cut -d' ' -f2 "$dir/times" | median)
            steps=$(tail -n 1 "$dir/times" | cut -d' ' -f3)
            printf '  %-7s %7d  %-22s %8.2fs %6dMB %7s\n' "$mode" "$n" "$case" "$secs" "$mb" "$steps"
        done
        # Leave the next case a clean slate.
        "$BARCA" get invoice pipeline.py >/dev/null 2>&1
        cd - >/dev/null
        rm -rf "$dir"
    done
done
