#!/usr/bin/env bash
# Regenerate the CLI contract snapshots after a deliberate change to the CLI surface:
#
#   - crates/barca-cli/snapshots/help/*.txt            `--help` of every command
#   - crates/barca-cli/docs/contract.md                generated tables (commands, flags, exit
#                                                      codes, JSON schemas, --agent lines)
#   - python/tests/snapshots/cli_contract/*.txt        JSON output schemas and the error envelope
#   - site/src/content/docs/reference/cli-contract.md  the site copy of contract.md
#
# Run from anywhere in the repository; review the diff afterwards, it is the contract change.
# Uses .venv (maturin develop) when present, otherwise the python/maturin on PATH.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if [ -x .venv/bin/python ]; then
    PY=.venv/bin/python
else
    PY=python
fi
if [ -x .venv/bin/maturin ]; then
    MATURIN=.venv/bin/maturin
else
    MATURIN=maturin
fi

echo "==> help snapshots and generated tables (cargo test -p barca)"
BARCA_UPDATE_SNAPSHOTS=1 cargo test -q -p barca contract::

echo "==> rebuilding barca so the JSON snapshots see this source"
"$MATURIN" develop --release -q

echo "==> JSON schema snapshots and tables (pytest)"
BARCA_UPDATE_SNAPSHOTS=1 "$PY" -m pytest -q python/tests/test_cli_contract.py

echo "==> copying contract.md, now complete, to the site"
BARCA_UPDATE_SNAPSHOTS=1 cargo test -q -p barca contract::

echo "==> checking that the snapshots are stable"
cargo test -q -p barca contract::
"$PY" -m pytest -q python/tests/test_cli_contract.py

echo
echo "Updated. Review the change to the contract:"
echo "  git diff -- crates/barca-cli/snapshots crates/barca-cli/docs/contract.md \\"
echo "    python/tests/snapshots site/src/content/docs/reference/cli-contract.md"
