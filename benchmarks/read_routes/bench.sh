#!/usr/bin/env bash
# The read routes of `barca serve` and the inspection commands at the size of a real project:
# 300 assets, one 200k-key partitioned node, a few thousand runs. Barca only; not run in CI.
#
#   benchmarks/read_routes/bench.sh                  # generate, seed (about 45 min), measure
#   benchmarks/read_routes/bench.sh DIR              # measure an already seeded project
#   STORE=1 benchmarks/read_routes/bench.sh          # with a file:// artifact store
#   KEYS=20000 RUNS=200 benchmarks/read_routes/bench.sh   # a smaller fixture
#
# The fixture lives under benchmarks/read_routes/.fixture (gitignored); it takes about 1.5 GB.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PY="${PYTHON:-python3}"
DIR="${1:-$HERE/.fixture/project}"
if [ ! -d "$DIR/.barca" ]; then
    "$PY" "$HERE/generate.py" "$DIR" --keys "${KEYS:-200000}"
    if [ -n "${STORE:-}" ]; then
        mkdir -p "$DIR/../store"
        printf '[remote]\nuri = "file://%s"\n' "$(cd "$DIR/../store" && pwd)" > "$DIR/barca.toml"
    fi
    "$PY" "$HERE/seed.py" "$DIR" --runs "${RUNS:-2000}"
fi
exec "$PY" "$HERE/bench.py" "$DIR" --json "$DIR/../results.json"
