#!/usr/bin/env bash
# Create a worktree for one topic, set up the way AGENTS.md asks.
#
#   scripts/new-worktree.sh <slug> [type]     # branch <type>/<slug>, default type feat
#   scripts/new-worktree.sh fix-123-thing fix
#   scripts/new-worktree.sh docs-typo doc --no-build
#
# Steps: fetch origin/main, `git worktree add -b <type>/<slug> <trunk>/.worktrees/<slug>
# origin/main`, write the tree's .cargo/config.toml (private target-local/ build dir, and
# sccache as the rustc wrapper when it is installed), create .venv and run
# `maturin develop --uv --profile fast --extras test` into it (Cargo.toml's `fast` profile:
# release optimizations without fat LTO, incremental; about 40 s cold and 5 s after an edit,
# against about 95 s and 80 s for --release).
#
# The cargo config means every cargo and maturin command run inside the tree builds into its
# own target-local/ without exporting CARGO_TARGET_DIR. sccache, when installed, shares
# compiled dependency crates between trees (cache under its user-level default directory,
# never inside a tree); on a 16-core Mac that saved only a few seconds per cold build, so it
# is optional and everything works without it.
#
# Options:
#   --no-build      create the tree and .venv only; skip `maturin develop`
#   --release       build with --release (what wheels ship) instead of --profile fast
#   --base <ref>    branch off <ref> instead of origin/main
#
# Remove the tree with scripts/remove-worktree.sh <slug>.
set -euo pipefail

usage() { sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

slug="" type="" build=1 base="origin/main" profile="--profile fast"
while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) build=0 ;;
        --release) profile="--release" ;;
        --base) base="${2:?--base needs a ref}"; shift ;;
        -h|--help) usage ;;
        -*) echo "error: unknown option $1" >&2; usage ;;
        *) if [ -z "$slug" ]; then slug="$1"; elif [ -z "$type" ]; then type="$1"; else usage; fi ;;
    esac
    shift
done
[ -n "$slug" ] || usage
type="${type:-feat}"
case "$slug" in */*|.*|"") echo "error: slug must be a plain name like fix-123-thing" >&2; exit 2 ;; esac
case "$type" in feat|fix|refactor|polish|doc|test|perf|remove|chore) ;;
    *) echo "error: type must be a conventional-commit type (feat, fix, refactor, polish, doc, test, perf, remove, chore)" >&2; exit 2 ;;
esac

# The trunk is the parent of the shared git dir, so this works from the trunk or any tree.
trunk="$(cd "$(git rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
tree="$trunk/.worktrees/$slug"
branch="$type/$slug"

if [ -e "$tree" ]; then echo "error: $tree already exists" >&2; exit 1; fi
if git -C "$trunk" show-ref --verify --quiet "refs/heads/$branch"; then
    echo "error: branch $branch already exists (pick another slug, or remove it first)" >&2; exit 1
fi

git -C "$trunk" fetch --quiet origin main
mkdir -p "$trunk/.worktrees"
git -C "$trunk" worktree add --quiet -b "$branch" "$tree" "$base"
# A branch made from origin/main would otherwise track it; push sets the real upstream.
git -C "$tree" branch --quiet --unset-upstream 2>/dev/null || true
echo "worktree: $tree (branch $branch off $base)" >&2

# Per-tree cargo config (ignored by .gitignore). Relative target-dir resolves against the
# tree root, so target-local/ is private to this tree.
mkdir -p "$tree/.cargo"
{
    echo "# Written by scripts/new-worktree.sh; private to this worktree (gitignored)."
    echo "[build]"
    echo "target-dir = \"target-local\""
    if command -v sccache >/dev/null 2>&1; then
        echo "rustc-wrapper = \"sccache\""
    fi
} > "$tree/.cargo/config.toml"

if command -v sccache >/dev/null 2>&1; then
    echo "sccache: on (cache $(sccache --show-stats 2>/dev/null | sed -n 's/^Cache location *//p' | head -1))" >&2
else
    cat >&2 <<'EOF'
sccache: not installed (optional), so this tree compiles every dependency crate itself.
  To share compiled crates between worktrees, install it once:
    brew install sccache        # or: cargo install sccache --locked
  then re-run this script for new trees, or add `rustc-wrapper = "sccache"` under [build] in
  this tree's .cargo/config.toml.
EOF
fi

if ! command -v uv >/dev/null 2>&1; then
    echo "error: uv is not installed (https://docs.astral.sh/uv/); the tree exists but has no .venv" >&2
    exit 1
fi
(cd "$tree" && uv venv --quiet .venv)

if [ "$build" -eq 1 ]; then
    if ! command -v maturin >/dev/null 2>&1; then
        echo "error: maturin is not installed (uv tool install maturin); skipped the build" >&2
        exit 1
    fi
    echo "building: maturin develop --uv $profile --extras test" >&2
    start=$(date +%s)
    (cd "$tree" && VIRTUAL_ENV="$tree/.venv" PATH="$tree/.venv/bin:$PATH" \
        maturin develop --uv $profile --extras test)
    echo "built in $(( $(date +%s) - start ))s: $tree/.venv/bin/barca" >&2
fi

echo "ready: cd $tree" >&2
echo "$tree"
