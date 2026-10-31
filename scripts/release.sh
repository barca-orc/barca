#!/usr/bin/env bash
# Cut a release: branch, version bump, PR with squash auto-merge.
#
#   scripts/release.sh 0.35.0             # run from the trunk checkout, on an up-to-date main
#   scripts/release.sh 0.35.0 --dry-run   # everything local (worktree, bump, checks, commit);
#                                         # no push, no PR; removes the worktree afterwards
#   scripts/release.sh 0.35.0 --dry-run --keep   # keep the worktree (also kept on failure)
#
# It refuses unless: the trunk is clean, on main and equal to origin/main; X.Y.Z is greater
# than the latest v* tag and the current version; neither the tag nor release/vX.Y.Z exists;
# `## Pending` in CHANGELOG.md has notes; and, pre-1.0, a Pending `### Breaking` comes with a
# minor bump. Skipped versions and non-standard Pending headings are warnings.
#
# Then, in .worktrees/release-vX.Y.Z on branch release/vX.Y.Z off origin/main, it applies the
# bump (scripts/bump-version.py), runs scripts/check-version-sync.sh, scripts/check-changelog.py,
# scripts/changelog-section.py and the lock-file checks, commits `chore(release): vX.Y.Z`,
# pushes, opens the PR into main and enables `gh pr merge --squash --auto`.
#
# When the PR merges, .github/workflows/release-tag.yml tags the merge commit vX.Y.Z and starts
# the release workflow (wheels, GitHub Release, PyPI, barca.sh). Remove the worktree with
# scripts/remove-worktree.sh release-vX.Y.Z.
set -euo pipefail

usage() { sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

version="" dry=0 keep=0
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) dry=1 ;;
        --keep) keep=1 ;;
        -h|--help) usage ;;
        -*) echo "error: unknown option $1" >&2; usage ;;
        *) [ -z "$version" ] || usage; version="${1#v}" ;;
    esac
    shift
done
[ -n "$version" ] || usage
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "error: version must be X.Y.Z, got $version" >&2; exit 2; }

die() { echo "error: $*" >&2; exit 1; }
step() { echo "==> $*" >&2; }

here="$(cd "$(dirname "$0")" && pwd)"
trunk="$(cd "$(git rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
[ "$(git rev-parse --show-toplevel)" = "$trunk" ] || die "run this from the trunk checkout ($trunk), not a worktree"
tag="v$version"
branch="release/$tag"
slug="release-$tag"
tree="$trunk/.worktrees/$slug"

# --- Validate ------------------------------------------------------------------------------
step "checking the trunk"
[ "$(git -C "$trunk" branch --show-current)" = "main" ] || die "the trunk is not on main"
[ -z "$(git -C "$trunk" status --porcelain)" ] || die "the trunk has uncommitted changes (git status)"
git -C "$trunk" fetch --quiet --tags origin main
[ "$(git -C "$trunk" rev-parse HEAD)" = "$(git -C "$trunk" rev-parse origin/main)" ] ||
    die "main is not origin/main; run: git pull --ff-only"

latest="$(git -C "$trunk" tag --list 'v[0-9]*' --sort=-v:refname | head -1)"
if [ -n "$latest" ]; then
    highest="$(printf '%s\n%s\n' "${latest#v}" "$version" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)"
    [ "$highest" = "$version" ] && [ "${latest#v}" != "$version" ] ||
        die "$version is not greater than the latest tag $latest"
fi
git -C "$trunk" rev-parse --quiet --verify "refs/tags/$tag" >/dev/null && die "tag $tag already exists"
[ -z "$(git -C "$trunk" ls-remote --heads origin "$branch")" ] || die "origin already has a branch $branch"
git -C "$trunk" show-ref --verify --quiet "refs/heads/$branch" && die "a local branch $branch already exists"
[ -e "$tree" ] && die "$tree already exists"
if [ "$dry" -eq 0 ]; then
    command -v gh >/dev/null 2>&1 || die "gh is not installed (needed to open the PR)"
    gh auth status >/dev/null 2>&1 || die "gh is not logged in (gh auth login)"
    open="$(cd "$trunk" && gh pr list --base main --state open --limit 200 --json headRefName \
        --jq '.[].headRefName | select(startswith("release/v"))')" || die "gh pr list failed"
    [ -z "$open" ] || die "a release PR is already open: $open"
fi
# Version and changelog rules, against main's files (edits nothing).
python3 "$here/bump-version.py" --root "$trunk" --check "$version"

# --- Branch and bump -----------------------------------------------------------------------
step "creating $tree on $branch"
git -C "$trunk" worktree add --quiet --no-track -b "$branch" "$tree" origin/main
# Until the branch is pushed, a failure (or a finished dry run) removes the worktree and branch,
# so the script can simply be re-run. --keep leaves them for inspection.
pushed=0
cleanup_dry() {
    if [ "$pushed" -eq 0 ] && [ "$keep" -eq 0 ]; then
        git -C "$trunk" worktree remove --force "$tree" 2>/dev/null || true
        git -C "$trunk" branch -D "$branch" >/dev/null 2>&1 || true
        git -C "$trunk" worktree prune
    fi
}
trap cleanup_dry EXIT

step "bumping to $version"
python3 "$here/bump-version.py" --root "$tree" "$version"

step "checking the bump"
(cd "$tree" && scripts/check-version-sync.sh >&2)
(cd "$tree" && python3 scripts/check-changelog.py --base origin/main >&2)
(cd "$tree" && python3 scripts/changelog-section.py "$version" >/dev/null)
(cd "$tree" && cargo metadata --locked --offline --format-version 1 --no-deps >/dev/null) ||
    die "Cargo.lock does not match the bumped Cargo.toml files"
if command -v uv >/dev/null 2>&1; then
    (cd "$tree" && uv lock --check --quiet) ||
        die "uv.lock does not match the bumped pyproject.toml"
fi
expected="CHANGELOG.md Cargo.lock crates/barca-cli/Cargo.toml crates/barca-core/Cargo.toml crates/barca-server/Cargo.toml pyproject.toml python/barca/__init__.py uv.lock"
changed="$(git -C "$tree" diff --name-only | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//')"
[ "$changed" = "$expected" ] || die "unexpected files changed: $changed"
git -C "$tree" diff --stat >&2

# --no-verify: the hooks lint and type-check code this commit does not touch; the checks above
# cover what it does touch, and CI runs everything on the PR.
git -C "$tree" commit --quiet --no-verify -am "chore(release): $tag"
step "committed $(git -C "$tree" rev-parse --short HEAD) chore(release): $tag"

notes="$(cd "$tree" && python3 scripts/changelog-section.py "$version")"
body="$(cat <<EOF
Release $tag: version bump across crates/*/Cargo.toml, Cargo.lock, pyproject.toml, uv.lock and \`barca.__version__\`; \`## Pending\` in CHANGELOG.md becomes \`## $version ($(date -u +%F))\`.

Opened by \`scripts/release.sh\`. Squash auto-merge is on; when this merges, \`.github/workflows/release-tag.yml\` tags the merge commit \`$tag\` and starts the release workflow (wheels, GitHub Release, PyPI, barca.sh).

## Release notes

$notes
EOF
)"

if [ "$dry" -eq 1 ]; then
    step "dry run: stopping before push"
    echo "would run: git push -u origin $branch" >&2
    echo "would run: gh pr create --base main --head $branch --title 'chore(release): $tag'" >&2
    echo "would run: gh pr merge $branch --squash --auto" >&2
    echo "--- PR body ---" >&2
    echo "$body" >&2
    if [ "$keep" -eq 1 ]; then
        echo "kept $tree; remove it with: scripts/remove-worktree.sh $slug --force" >&2
    fi
    exit 0
fi

# --- Push, PR, auto-merge ------------------------------------------------------------------
step "pushing $branch"
pushed=1
git -C "$tree" push --quiet -u origin "$branch"
step "opening the PR"
url="$(cd "$tree" && gh pr create --base main --head "$branch" --title "chore(release): $tag" --body "$body")"
echo "$url"
step "enabling squash auto-merge"
(cd "$tree" && gh pr merge "$url" --squash --auto) ||
    echo "warning: could not enable auto-merge; merge $url by hand once CI passes" >&2
cat >&2 <<EOF
Next: CI runs on $url and it squash-merges when green (rebase it if main moves: gh pr update-branch $url).
The merge is tagged $tag by the release-tag workflow, which starts the release; watch with
  gh run list --workflow release.yml --limit 1
Then remove the worktree: scripts/remove-worktree.sh $slug
EOF
