#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
export BARCA_BENCH_STATE="$(mktemp -d)"
"$REPO_ROOT/.venv/bin/barca" get "$SCRIPT_DIR/assets.py" --refresh-all 2>/dev/null
