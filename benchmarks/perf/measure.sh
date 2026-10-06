#!/usr/bin/env bash
# Measure barca's resource usage across the benchmark suite.
# Records wall time, peak RSS, CPU time for each benchmark.
# Results written to benchmarks/perf/results.json
#
# Usage: bash benchmarks/perf/measure.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BARCA="${REPO_ROOT}/.venv/bin/barca"
[ -x "$BARCA" ] || BARCA="$(command -v barca)"

VERSION=$($BARCA --version 2>&1 | awk '{print $2}')
DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
RESULTS_FILE="$SCRIPT_DIR/results.json"

# Detect platform for memory measurement
if [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS: /usr/bin/time -l gives peak RSS in bytes
    TIME_CMD="/usr/bin/time -l"
    parse_rss() { grep "maximum resident set size" | awk '{print $1}'; }
    # macOS /usr/bin/time -l: "0.02 real  0.01 user  0.00 sys"
    parse_user() { grep "real" | head -1 | awk '{print $3}'; }
    parse_sys() { grep "real" | head -1 | awk '{print $5}'; }
else
    # Linux: /usr/bin/time -v gives peak RSS in KB
    TIME_CMD="/usr/bin/time -v"
    parse_rss() { grep "Maximum resident set size" | awk '{print $NF * 1024}'; }
    parse_user() { grep "User time" | awk '{print $NF}'; }
    parse_sys() { grep "System time" | awk '{print $NF}'; }
fi

measure_benchmark() {
    local name=$1
    local asset_file=$2

    rm -rf "$REPO_ROOT/.barca"

    # Run with /usr/bin/time to capture resource usage
    local time_output
    time_output=$($TIME_CMD "$BARCA" get "$asset_file" --refresh-all 2>&1 >/dev/null)

    local wall_time
    wall_time=$($BARCA get "$asset_file" --refresh-all 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin)['elapsed_seconds'])" 2>/dev/null || echo "0")

    local peak_rss
    peak_rss=$(echo "$time_output" | parse_rss || echo "0")

    local cpu_user
    cpu_user=$(echo "$time_output" | parse_user || echo "0")

    local cpu_sys
    cpu_sys=$(echo "$time_output" | parse_sys || echo "0")

    # Get artifact directory size
    local artifact_size=0
    if [ -d "$REPO_ROOT/.barca/artifacts" ]; then
        artifact_size=$(du -sb "$REPO_ROOT/.barca/artifacts" 2>/dev/null | awk '{print $1}' || du -sk "$REPO_ROOT/.barca/artifacts" 2>/dev/null | awk '{print $1 * 1024}' || echo "0")
    fi

    # Get DB size
    local db_size=0
    if [ -f "$REPO_ROOT/.barca/metadata.db" ]; then
        db_size=$(stat -f%z "$REPO_ROOT/.barca/metadata.db" 2>/dev/null || stat -c%s "$REPO_ROOT/.barca/metadata.db" 2>/dev/null || echo "0")
    fi

    echo "  \"$name\": {\"wall_time_s\": $wall_time, \"peak_rss_bytes\": $peak_rss, \"cpu_user_s\": $cpu_user, \"cpu_sys_s\": $cpu_sys, \"artifact_bytes\": $artifact_size, \"db_bytes\": $db_size}"
}

echo "Measuring barca $VERSION performance..."
echo ""

# Build results JSON
{
    echo "{"
    echo "  \"measured_at\": \"$DATE\","
    echo "  \"version\": \"$VERSION\","
    echo "  \"platform\": \"$(uname -s) $(uname -m)\","
    echo "  \"benchmarks\": {"

    first=true
    for bench in trivial chain_100 deep_diamond fan_out_500_50ms; do
        asset_file="$REPO_ROOT/benchmarks/$bench/barca/assets.py"
        if [ ! -f "$asset_file" ]; then
            continue
        fi
        if [ "$first" = true ]; then
            first=false
        else
            echo ","
        fi
        echo -n "    "
        result=$(measure_benchmark "$bench" "$asset_file")
        echo -n "$result"
        # Print progress
        echo "  ✓ $bench" >&2
    done

    echo ""
    echo "  }"
    echo "}"
} > "$RESULTS_FILE"

echo ""
echo "Results written to $RESULTS_FILE"
echo ""
python3 -c "
import json
with open('$RESULTS_FILE') as f:
    data = json.load(f)
print(f\"barca {data['version']} on {data['platform']}\")
print(f\"{'Benchmark':<20} {'Wall':>8} {'RSS':>10} {'CPU(u+s)':>10} {'Artifacts':>10} {'DB':>8}\")
print('-' * 72)
for name, m in data['benchmarks'].items():
    rss_mb = m['peak_rss_bytes'] / 1024 / 1024
    art_kb = m['artifact_bytes'] / 1024
    db_kb = m['db_bytes'] / 1024
    cpu = float(m.get('cpu_user_s', 0)) + float(m.get('cpu_sys_s', 0))
    print(f\"{name:<20} {m['wall_time_s']:>7.3f}s {rss_mb:>8.1f}MB {cpu:>9.3f}s {art_kb:>8.0f}KB {db_kb:>6.0f}KB\")
"
