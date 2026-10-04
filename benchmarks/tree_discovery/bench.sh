#!/usr/bin/env bash
# Tree discovery: `barca list` with no file arguments over a generated project of 1000 .py files
# (100 pipelines, the rest helpers), against naming the 100 pipeline files explicitly.
# Budget (issue #202): discovery stays under 100 ms wall. Barca only; nothing executes.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BARCA="${BARCA:-barca}"
PROJECT="$(mktemp -d)/tree"
python3 "$SCRIPT_DIR/generate.py" "$PROJECT" "${FILES:-1000}" "${PIPELINES:-100}"
cd "$PROJECT"
FILES_ARG="$(find . -name 'pipeline_*.py' | sort | tr '\n' ' ')"

echo "  BENCHMARK: tree discovery ($(find . -name '*.py' | wc -l | tr -d ' ') .py files, $(echo "$FILES_ARG" | wc -w | tr -d ' ') pipelines)"
hyperfine \
    --warmup 3 \
    --runs "${1:-20}" \
    --export-markdown "$SCRIPT_DIR/results.md" \
    --command-name "barca list (discovery)" "$BARCA list --json --all >/dev/null" \
    --command-name "barca list <100 files>" "$BARCA list $FILES_ARG --json --all >/dev/null"
