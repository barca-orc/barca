#!/usr/bin/env bash
# Regenerate the TypeScript bindings in ui/src/lib/generated/ from the Rust
# API-boundary types (the single source of truth). Run after changing any type
# exposed by barca-server. CI's merge tier runs this and `git diff --exit-code` to
# catch drift (.depot/workflows/ci.yml, rust-tests).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$REPO/ui/src/lib/generated"
mkdir -p "$OUT"
export TS_RS_EXPORT_DIR="$OUT"

# Export tests live in the libraries. Resolve all `ts` features together: separate
# commands rebuild the dependency graph repeatedly and compile unrelated integration
# test binaries, exhausting the release runner's disk before generation finishes.
cargo test --manifest-path "$REPO/Cargo.toml" --lib \
  -p barca-model -p barca-python -p barca-db -p barca-store \
  -p barca-analysis -p barca-run -p barca-service -p barca-server \
  --features barca-model/ts,barca-python/ts,barca-db/ts,barca-store/ts,barca-analysis/ts,barca-run/ts,barca-service/ts,barca-server/ts \
  export_bindings

# ts-rs leaves trailing whitespace, which the repo's trailing-whitespace hook strips
# from committed files: strip it here, so the output is what is committed and CI's
# `git diff --exit-code` compares like with like.
sed -i.bak 's/[[:space:]]*$//' "$OUT"/*.ts && rm -f "$OUT"/*.ts.bak
echo "regenerated TS bindings → $OUT"
