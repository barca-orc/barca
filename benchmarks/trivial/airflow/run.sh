#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV="${BARCA_BENCH_AIRFLOW_VENV:-$SCRIPT_DIR/.venv}"
"$VENV/bin/python" "$SCRIPT_DIR/run.py" 2>/dev/null
