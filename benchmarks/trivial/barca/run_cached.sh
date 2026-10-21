#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
# Assumes cache is warm (run once before hyperfine --prepare)
"${BARCA:-$REPO_ROOT/.venv/bin/barca}" get single_asset "$SCRIPT_DIR/assets.py" 2>/dev/null
