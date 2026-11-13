#!/usr/bin/env bash
# Verify all version strings are consistent across the project.
set -euo pipefail

extract() {
    local file="$1" pattern="$2"
    local val
    val=$(grep "$pattern" "$file" | head -1 | sed 's/.*"\(.*\)".*/\1/' || true)
    if [ -z "$val" ]; then
        echo "ERROR: could not extract version from $file (pattern: $pattern)" >&2
        exit 1
    fi
    echo "$val"
}

CORE=$(extract crates/barca-core/Cargo.toml '^version')
CLI=$(extract crates/barca-cli/Cargo.toml '^version')
SERVER=$(extract crates/barca-server/Cargo.toml '^version')
MODEL=$(extract crates/barca-model/Cargo.toml '^version')
COMPILE=$(extract crates/barca-compile/Cargo.toml '^version')
DB=$(extract crates/barca-db/Cargo.toml '^version')
PY_TOML=$(extract pyproject.toml '^version')
PY_INIT=$(extract python/barca/__init__.py '__version__')

ERRORS=0
for name_ver in "barca-cli:$CLI" "barca-server:$SERVER" "barca-model:$MODEL" "barca-compile:$COMPILE" "barca-db:$DB" "pyproject.toml:$PY_TOML" "__init__.py:$PY_INIT"; do
    name="${name_ver%%:*}"
    ver="${name_ver#*:}"
    if [ "$ver" != "$CORE" ]; then
        echo "Version mismatch: barca-core=$CORE but $name=$ver"
        ERRORS=1
    fi
done
if [ "$ERRORS" -eq 1 ]; then exit 1; fi
echo "All versions consistent: $CORE"

# The linter pins: prek.toml runs ruff and ty at the versions pyproject.toml's `dev` group pins.
RUFF_HOOK=$(sed -n 's/^rev = "v\(.*\)"/\1/p' prek.toml | head -1)
TY_HOOK=$(sed -n 's/.*"ty==\([^"]*\)".*/\1/p' prek.toml | head -1)
RUFF_DEV=$(sed -n 's/^ *"ruff==\([^"]*\)",.*/\1/p' pyproject.toml | head -1)
TY_DEV=$(sed -n 's/^ *"ty==\([^"]*\)",.*/\1/p' pyproject.toml | head -1)
for pin in "ruff:$RUFF_HOOK:$RUFF_DEV" "ty:$TY_HOOK:$TY_DEV"; do
    IFS=: read -r name hook dev <<<"$pin"
    if [ -z "$hook" ] || [ -z "$dev" ] || [ "$hook" != "$dev" ]; then
        echo "Linter pin mismatch: prek.toml has $name ${hook:-?}, pyproject.toml [dependency-groups] dev has ${dev:-?}"
        ERRORS=1
    fi
done
if [ "$ERRORS" -eq 1 ]; then exit 1; fi
echo "Linter pins consistent: ruff $RUFF_DEV, ty $TY_DEV"
