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

PY_TOML=$(extract pyproject.toml '^version')
PY_INIT=$(extract python/barca/__init__.py '__version__')

# Every crate under crates/ must carry the version of pyproject.toml.
ERRORS=0
for manifest in crates/*/Cargo.toml; do
    ver=$(extract "$manifest" '^version')
    if [ "$ver" != "$PY_TOML" ]; then
        echo "Version mismatch: pyproject.toml=$PY_TOML but $manifest=$ver"
        ERRORS=1
    fi
done
if [ "$PY_INIT" != "$PY_TOML" ]; then
    echo "Version mismatch: pyproject.toml=$PY_TOML but __init__.py=$PY_INIT"
    ERRORS=1
fi
if [ "$ERRORS" -eq 1 ]; then exit 1; fi
echo "All versions consistent: $PY_TOML"

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
