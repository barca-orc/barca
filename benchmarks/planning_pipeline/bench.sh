#!/usr/bin/env bash
# The planning pipeline (#250) at the large size (#233's scale, about 22M forecast rows): wall
# time and peak memory, cold and warm. Barca only; not run in CI. See bench.py for options.
#
#   benchmarks/planning_pipeline/bench.sh                    # scale 67, pool 4
#   benchmarks/planning_pipeline/bench.sh --scale 8          # the size the remote tests use
#   BARCA_REMOTE_URI=s3://bucket/prefix benchmarks/planning_pipeline/bench.sh   # artifacts in a store
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
exec "${PYTHON:-python3}" "$SCRIPT_DIR/bench.py" "$@"
