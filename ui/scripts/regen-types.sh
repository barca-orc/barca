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

# ts-rs emits one export test per annotated type, gated behind the `ts` feature.
cargo test --manifest-path "$REPO/Cargo.toml" -p barca-model --features ts export_bindings
cargo test --manifest-path "$REPO/Cargo.toml" -p barca-db --features ts export_bindings
cargo test --manifest-path "$REPO/Cargo.toml" -p barca-core --features ts export_bindings
cargo test --manifest-path "$REPO/Cargo.toml" -p barca-server --features ts export_bindings

# ts-rs leaves trailing whitespace, which the repo's trailing-whitespace hook strips
# from committed files: strip it here, so the output is what is committed and CI's
# `git diff --exit-code` compares like with like.
sed -i.bak 's/[[:space:]]*$//' "$OUT"/*.ts && rm -f "$OUT"/*.ts.bak
echo "regenerated TS bindings → $OUT"
