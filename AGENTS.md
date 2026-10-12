# AGENTS.md

Operating rules for agents in this repository. Product architecture, CLI contract, and
design principles live in [CLAUDE.md](CLAUDE.md). Read this file for **worktrees**,
**reap**, and **cleanup** before starting work and again before ending a session.

## Worktrees

`main` is the integration checkout. Keep it clean.

1. Create a sibling worktree and topic branch from current `origin/main` before editing:
   ```bash
   git fetch origin main
   git worktree add -b <type>/<slug> ../barca-<slug> origin/main
   ```
   Prefer `/home/mukulram/Personal/barca-<slug>` (matches existing delivery/UI worktrees).
   `.claude/worktrees/<name>` is acceptable for short Claude sessions.
2. Do all edits, builds, installs, and tests inside that worktree.
3. Leave the primary `/home/mukulram/Personal/barca` checkout on `main` with a clean
   `git status` (no staged, unstaged, or untracked source). Move accidental work with
   `git stash push -u` → worktree → `git stash pop`, then delete leftover ignored junk
   (for example `ui/demo/.barca`) from the primary checkout.
4. One topic per worktree. Do not reuse another agent's worktree or branch.

Done when: the primary checkout is clean on `main`, and the work exists only on a topic
branch inside a registered worktree (`git worktree list`).

## Reap

Every long-lived process you start, you stop.

Typical leftovers: `barca serve` / `barca get` / `barca run`, Vite (`pnpm --dir ui dev`),
Playwright, maturin/cargo test hangers, Python workers, local object-store containers you
started for the task.

1. Track PIDs or the ports you bound (UI e2e often uses Vite + `barca serve`).
2. Before yielding the session or switching tasks, terminate those processes (and their
   process groups). Prefer a clean shutdown of the command you started over a broad `pkill`.
3. Confirm listeners you opened are gone (`ss`/`lsof` on those ports, or an empty
   `pgrep` for the binary you launched from this worktree).

Done when: no process you started for this task is still running.

## Cleanup

Remove what you created once the branch is merged, abandoned, or the session ends with
no further use for the tree.

1. Worktree finished: `git worktree remove <path>` after the branch is merged or discarded.
   Delete the local topic branch when it is no longer needed.
   `git worktree prune` for stale registrations.
2. Runtime dirs you created inside the worktree (`.barca/`, `ui/demo/.barca/`,
   `__pycache__/`, Playwright output, temporary seed DBs) — delete them when you are done
   using that demo or test fixture.
3. Do not delete other agents' worktrees, shared sibling checkouts you did not create, or
   the primary `barca` checkout.

Done when: `git worktree list` no longer shows trees you finished, and no orphan temp or
artifact directories remain from your session outside an intentionally kept worktree.

## Pointers

- Architecture and CLI/docs contract: [CLAUDE.md](CLAUDE.md)
- New worktree setup hooks (build/sync): [`.cursor/worktrees.json`](.cursor/worktrees.json)
- Driving barca from scripts/agents: `barca docs agents` / `barca docs skill`
