# AGENTS.md

Operating rules for agents in this repository. Product architecture, CLI contract, and
design principles live in [CLAUDE.md](CLAUDE.md). Read this file for **worktrees**,
**reap**, and **cleanup** before starting work and again before ending a session.

Every worktree's build directory costs several GB. Treat teardown as part of the task, not
optional polish: an abandoned tree keeps its build directory until someone deletes it.

## Worktrees

`main` (the trunk checkout) is for fetch/pull and worktree management only. Keep it clean.

### Where

Create worktrees **only** under the trunk’s `.worktrees/` directory:

```bash
# run from the trunk checkout (the primary clone, not an existing worktree)
git fetch origin main
mkdir -p .worktrees
git worktree add -b <type>/<slug> .worktrees/<slug> origin/main
cd .worktrees/<slug>
```

Allowed path: `<trunk>/.worktrees/<slug>` (gitignored; already listed in `.gitignore`).

Not worktree homes: sibling directories (`../barca-*`, `../anything`), `.claude/worktrees/`,
`/tmp/...`, or any path outside `<trunk>/.worktrees/`. If a tool defaults elsewhere, move or
recreate under `.worktrees/<slug>` before editing.

### How

1. One topic per worktree. Do not reuse another agent’s tree or branch.
2. Do all edits, builds, installs, and tests inside that worktree.
3. Build into a **private** target dir inside the tree, for every `cargo` and `maturin`
   command:
   ```bash
   export CARGO_TARGET_DIR="$PWD/target-local"   # from the tree root; gitignored
   ```
   Do not share the trunk `target/` between trees. Two trees building at once overwrite the
   same `target/release/barca`, and cargo can count one tree's `barca-core` as already built
   for another, so `maturin develop` installs a binary without your change (seen with several
   parallel agents on 2026-10-20). Before trusting a Python test run, confirm the installed
   binary contains your change, e.g. a string only your branch has. Delete `target-local/`
   on teardown. Skip a full `cargo build` unless you need the binary.
4. Leave the trunk on `main` with a clean `git status`. Move accidental work with
   `git stash push -u` → `.worktrees/<slug>` → `git stash pop`, then delete leftover ignored
   junk on the trunk (for example `ui/demo/.barca`).

Done when: trunk is clean on `main`, and the work lives only under `.worktrees/<slug>` on a
topic branch (`git worktree list` shows that path).

## Reap

Every long-lived process you start, you stop — before removing the worktree and before
ending the session.

Typical leftovers: `barca serve` / `barca get` / `barca run`, Vite (`pnpm --dir ui dev`),
Playwright, maturin/cargo test hangers, Python workers, local object-store containers you
started for the task.

1. Track PIDs or the ports you bound (UI e2e often uses Vite + `barca serve`).
2. Terminate those processes (and their process groups). Prefer shutting down the command
   you started over a broad `pkill`.
3. Confirm listeners you opened are gone (`ss`/`lsof` on those ports, or an empty `pgrep`
   for the binary you launched from this worktree).

Done when: no process you started for this task is still running.

## Cleanup

Teardown is mandatory when the branch is merged, the task is abandoned, or the session ends
with no further use for the tree. Do not leave worktrees “for later” unless the user asked
to keep them.

From the **trunk** checkout, after reap:

```bash
git worktree remove .worktrees/<slug>          # add --force only if abandoning dirty work
git branch -d <type>/<slug>                    # or -D if abandoned / already on remote PR
git worktree prune
rmdir .worktrees/<slug> 2>/dev/null || true    # if remove left an empty husk
```

Also remove runtime junk you created if it somehow remains: `.barca/`, `ui/demo/.barca/`,
`__pycache__/`, Playwright output, temporary seed DBs, per-worktree `target/` / `node_modules`
(only under the tree you are deleting).

Periodic hygiene (start of session or when disk is tight):

```bash
git worktree list                              # drop anything you own that is finished
git worktree prune                             # clear registrations for deleted paths
du -sh .worktrees/* .claude/worktrees 2>/dev/null   # anything outside .worktrees/ is debt
```

If you find trees under `.claude/worktrees/` or sibling `barca-*` paths, treat them as
orphans: reap, `git worktree remove` / `prune`, delete the directory. Do not create new ones
there.

Do not delete other agents’ active `.worktrees/<slug>` entries or the trunk checkout itself.

Done when: `git worktree list` shows only the trunk plus worktrees still in active use;
`.worktrees/` holds no finished trees; no orphan dirs remain under `.claude/worktrees/` or
sibling paths from your session.

## Pointers

- Architecture and CLI/docs contract: [CLAUDE.md](CLAUDE.md)
- New worktree setup hooks: [`.cursor/worktrees.json`](.cursor/worktrees.json) (`uv sync`
  only; build into the tree's own `target-local/` when you need a binary)
- Driving barca from scripts/agents: `barca docs agents` / `barca docs skill`
