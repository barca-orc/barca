#!/usr/bin/env bash
# Tear down a worktree made by scripts/new-worktree.sh (AGENTS.md "Reap" and "Cleanup").
#
#   scripts/remove-worktree.sh <slug>            # reap, delete target-local/, remove, delete branch
#   scripts/remove-worktree.sh <slug> --force    # also when the tree is dirty or the branch unmerged
#   scripts/remove-worktree.sh <slug> --dry-run  # list what would be stopped and removed
#
# 1. Reap: stop every process whose working directory is inside the tree or whose command line
#    names the tree (barca serve/get/run, Python workers, Vite, cargo/maturin hangers):
#    SIGCONT (stopped workers), then SIGTERM; a process still alive after 5 s stops the script
#    (--force carries on). The shared sccache server is left running.
# 2. Delete <tree>/target-local/ (the bulk of the disk).
# 3. `git worktree remove` (refuses a dirty tree unless --force), `git worktree prune`.
# 4. Delete the local branch: `git branch -d`, or -D when its PR is merged (squash merges never
#    look merged to git; checked with `gh`) or with --force. Otherwise the branch is kept.
#
# Run it from anywhere except inside the tree being removed.
set -euo pipefail

usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

slug="" force=0 dry=0
while [ $# -gt 0 ]; do
    case "$1" in
        --force) force=1 ;;
        --dry-run) dry=1 ;;
        -h|--help) usage ;;
        -*) echo "error: unknown option $1" >&2; usage ;;
        *) [ -z "$slug" ] || usage; slug="$1" ;;
    esac
    shift
done
[ -n "$slug" ] || usage
case "$slug" in */*|.*) echo "error: give the slug (the directory name under .worktrees/)" >&2; exit 2 ;; esac

trunk="$(cd "$(git rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
tree="$trunk/.worktrees/$slug"
[ -d "$tree" ] || { echo "error: no worktree at $tree" >&2; exit 1; }
case "$(pwd -P)/" in "$(cd "$tree" && pwd -P)/"*)
    echo "error: run this from outside $tree (e.g. from $trunk)" >&2; exit 2 ;;
esac
tree="$(cd "$tree" && pwd -P)"
branch="$(git -C "$tree" symbolic-ref --quiet --short HEAD 2>/dev/null || true)"

run() { if [ "$dry" -eq 1 ]; then echo "would run: $*" >&2; else "$@"; fi; }

# --- 1. Reap -------------------------------------------------------------------------------
# PIDs of this script and its ancestors, never to be killed.
protected=" $$ "
p=$$
while [ "$p" -gt 1 ]; do
    p=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ') || break
    [ -n "$p" ] || break
    protected="$protected$p "
done

find_procs() {
    {
        # Working directory inside the tree.
        lsof -n -w -a -d cwd -Fpn 2>/dev/null | awk -v t="$tree" '
            /^p/ { pid = substr($0, 2) }
            /^n/ { n = substr($0, 2); if (n == t || index(n, t "/") == 1) print pid }'
        # Command line naming the tree.
        pgrep -f -- "$tree" 2>/dev/null || true
    } | sort -un | while read -r pid; do
        case "$protected" in *" $pid "*) continue ;; esac
        cmd=$(ps -o command= -p "$pid" 2>/dev/null) || continue
        case "$cmd" in *sccache*) continue ;; esac   # shared by every tree
        echo "$pid"
    done
}

pids=$(find_procs | tr '\n' ' ')
if [ -n "${pids// /}" ]; then
    echo "stopping processes started from $tree:" >&2
    for pid in $pids; do echo "  $pid $(ps -o command= -p "$pid" 2>/dev/null | cut -c1-120)" >&2; done
    if [ "$dry" -eq 0 ]; then
        # SIGCONT first: a worker the coordinator froze for parallel() (SIGSTOP) would
        # otherwise hold the SIGTERM pending forever.
        kill -CONT $pids 2>/dev/null || true
        kill $pids 2>/dev/null || true
        alive=""
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            alive=""
            for pid in $pids; do kill -0 "$pid" 2>/dev/null && alive="$alive $pid"; done
            [ -z "$alive" ] && break
            sleep 0.5
        done
        if [ -n "$alive" ]; then
            echo "warning: still running after SIGTERM:$alive (stop them, then re-run)" >&2
            [ "$force" -eq 1 ] || exit 1
        fi
    fi
else
    echo "no processes running from $tree" >&2
fi

# --- 2. Build directory --------------------------------------------------------------------
if [ -d "$tree/target-local" ]; then
    echo "deleting $tree/target-local ($(du -sh "$tree/target-local" 2>/dev/null | cut -f1 | tr -d ' '))" >&2
    run rm -r "$tree/target-local"
fi

# --- 3. Worktree ---------------------------------------------------------------------------
# The cargo config new-worktree.sh wrote is ours, not the user's work.
if head -1 "$tree/.cargo/config.toml" 2>/dev/null | grep -q 'scripts/new-worktree.sh'; then
    run rm "$tree/.cargo/config.toml"
    [ "$dry" -eq 1 ] || rmdir "$tree/.cargo" 2>/dev/null || true
fi
if [ "$force" -eq 1 ]; then
    run git -C "$trunk" worktree remove --force "$tree"
elif ! run git -C "$trunk" worktree remove "$tree"; then
    echo "error: $tree has uncommitted or untracked changes; commit them, or pass --force to discard" >&2
    exit 1
fi
run git -C "$trunk" worktree prune
[ "$dry" -eq 1 ] || rmdir "$tree" 2>/dev/null || true

# --- 4. Branch -----------------------------------------------------------------------------
if [ -n "$branch" ] && git -C "$trunk" show-ref --verify --quiet "refs/heads/$branch"; then
    if [ "$force" -eq 1 ]; then
        run git -C "$trunk" branch -D "$branch"
    elif git -C "$trunk" branch -d "$branch" >/dev/null 2>&1 || [ "$dry" -eq 1 ]; then
        [ "$dry" -eq 1 ] && echo "would run: git branch -d $branch" >&2 || echo "deleted branch $branch" >&2
    elif command -v gh >/dev/null 2>&1 &&
        [ -n "$(gh pr list --head "$branch" --state merged --json number --jq '.[].number' 2>/dev/null)" ]; then
        git -C "$trunk" branch -D "$branch"
        echo "deleted branch $branch (its PR is merged)" >&2
    else
        echo "kept branch $branch: not merged into HEAD and no merged PR found; delete it with" \
            "git branch -D $branch" >&2
    fi
fi
if [ "$dry" -eq 1 ]; then echo "dry run: nothing changed" >&2; else echo "removed $tree" >&2; fi
