#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
N="${1:-10}"
rm -rf .barca
"${BARCA:-$REPO_ROOT/.venv/bin/barca}" run "fan_out_${N}" "$SCRIPT_DIR/assets.py" --agent 2>/dev/null
