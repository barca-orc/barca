#!/usr/bin/env bash
# `barca status` and the read routes of `barca serve` with many partitioned nodes and a long
# history: 1,000 nodes, 1.2M partition keys, 2.8M result rows, 3,000 runs. Barca only; not run
# in CI.
#
#   benchmarks/wide_partitions/bench.sh              # generate, seed (a few minutes), measure
#   benchmarks/wide_partitions/bench.sh DIR          # measure an already seeded project
#   KEYS=200 benchmarks/wide_partitions/bench.sh     # a smaller fixture
#
# Seeding runs four of the ten files and gives every key five older results; one file is left
# for the measured run, so measure a seeded project once (or seed it again). The fixture lives
# under benchmarks/wide_partitions/.fixture (gitignored); it takes about 8 GB.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PY="${PYTHON:-python3}"
DIR="${1:-$HERE/.fixture/project}"
if [ ! -d "$DIR/.barca" ]; then
    "$PY" "$HERE/generate.py" "$DIR" --keys "${KEYS:-1200}"
    (cd "$DIR" && barca get pipe_00.py pipe_01.py pipe_02.py pipe_03.py > /dev/null)
    "$PY" "$HERE/history.py" "$DIR"
fi
exec "$PY" "$HERE/bench.py" "$DIR" --json "$DIR/../results.json"
