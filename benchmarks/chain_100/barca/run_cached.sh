#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
"${BARCA:-$REPO_ROOT/.venv/bin/barca}" get asset_099 "$SCRIPT_DIR/assets.py" 2>/dev/null
