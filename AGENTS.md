# AGENTS.md

Operating rules for agents in this repository. Product architecture, CLI contract, and
design principles live in [CLAUDE.md](CLAUDE.md). Read this file for **worktrees**,
**reap**, and **cleanup** before starting work and again before ending a session.

Every worktree's build directory costs several GB. Treat teardown as part of the task, not
optional polish: an abandoned tree keeps its build directory until someone deletes it.

## Worktrees

`main` (the trunk checkout) is for fetch/pull and worktree management only. Keep it clean.

### Where

Create worktrees **only** under the trunk’s `.worktrees/` directory, with the helper:

```bash
scripts/new-worktree.sh <slug> [type]   # branch <type>/<slug> (default feat) off origin/main
cd .worktrees/<slug>
```

It runs from the trunk or any tree and does the steps below: `git fetch origin main`,
`git worktree add -b <type>/<slug> .worktrees/<slug> origin/main`, writes the tree's
`.cargo/config.toml` (gitignored: `target-dir = "target-local"`, and `rustc-wrapper =
"sccache"` when sccache is installed), creates `.venv` and runs
`maturin develop --uv --profile fast --extras test` into it. `--no-build` skips the build
(docs-only work), `--release` builds with `--release` instead, and `--base <ref>` branches off
something other than origin/main. By hand:

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
   command. A tree made by `scripts/new-worktree.sh` already does: its `.cargo/config.toml`
   sets `target-dir = "target-local"`. Otherwise, from the tree root:
   ```bash
   export CARGO_TARGET_DIR="$PWD/target-local"   # gitignored
   ```
   Do not share the trunk `target/` between trees. Two trees building at once overwrite the
   same `target/release/barca`, and cargo can count one tree's `barca-core` as already built
   for another, so `maturin develop` installs a binary without your change (seen with several
   parallel agents on 2026-10-20). Before trusting a Python test run, confirm the installed
   binary contains your change, e.g. a string only your branch has. Delete `target-local/`
   on teardown. Skip a full `cargo build` unless you need the binary.

   Build the Python-test binary with the `fast` profile (Cargo.toml):
   `maturin develop --uv --profile fast --extras test`, which `scripts/new-worktree.sh`
   runs. It keeps the release `opt-level` but drops fat LTO and the single codegen unit,
   which are most of a release build and cannot be cached, and builds incrementally. Use
   `--release` only to measure performance or reproduce what a wheel ships. `maturin develop`
   and `cargo build --release` use different settings and do not share output, so a
   `cargo build --release` first only adds time.

   sccache ([mozilla/sccache](https://github.com/mozilla/sccache); `brew install sccache` or
   `cargo install sccache --locked`) is optional. When it is installed, new-worktree.sh sets
   it as the tree's `rustc-wrapper`; it caches compiled dependency crates under a user-level
   directory (`sccache --show-stats` prints it; never inside a tree) and cannot hand one tree
   another tree's code (the key is the compiler inputs). Workspace crates, proc macros,
   binaries and the final link are always built per tree. Leave `CARGO_INCREMENTAL` unset:
   dev and `fast` builds keep incremental compilation for the workspace crates (sccache
   skips only those 12 compilations and caches the rest), and setting it to 0 changes the
   cache key, so a warm cache missed 293 of 318 crates, and an edit rebuilt in 16 s instead
   of 6 s.

   Measured 2026-10-27 on a 16-core M-series Mac with other builds running (load 9-12),
   fresh tree, empty `target-local/`, seconds:

   | build | no sccache | sccache, warm cache |
   |---|---|---|
   | `maturin develop --uv --release --extras test` | 97 | 71 same path, 94 another tree |
   | `maturin develop --uv --profile fast --extras test` | 41 | 39 |
   | `cargo test --workspace --no-run` (dev) | 33 | 32 same path, 34 another tree |
   | edit then `maturin develop`, `--release` | 82 | |
   | edit then `maturin develop`, `--profile fast` | 5-7 | |
   | edit then `cargo test --no-run` (incremental, default) | 6 | |

   A cold sccache adds about 10 s (108 s release, 48 s dev). The Python dependencies come
   from uv's shared cache: `maturin develop` installs the 79 packages of `--extras test` into
   a new `.venv` in under a second.

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
   you started over a broad `pkill`. Send `kill -CONT <pid>` before `kill <pid>`: for
   `parallel()` the coordinator SIGSTOPs the calling worker, and a stopped process never acts
   on SIGTERM. `scripts/remove-worktree.sh` does this for every process tied to the tree.
3. Confirm listeners you opened are gone (`ss`/`lsof` on those ports, or an empty `pgrep`
   for the binary you launched from this worktree).

Done when: no process you started for this task is still running.

## Cleanup

Teardown is mandatory when the branch is merged, the task is abandoned, or the session ends
with no further use for the tree. Do not leave worktrees “for later” unless the user asked
to keep them.

Run the helper from the **trunk** checkout:

```bash
scripts/remove-worktree.sh <slug>              # --dry-run lists what it would stop and remove
```

It reaps: every process whose working directory is inside the tree or whose command line
names it gets SIGCONT, then SIGTERM; one still alive after 5 s stops the script (`--force`
carries on); the shared sccache server is left alone. Then it deletes `target-local/`
(`rm -r`) and the `.cargo/config.toml` it wrote, runs `git worktree remove` (refuses a dirty
tree; `--force` discards it) and `git worktree prune`, and deletes the local branch:
`git branch -d`, or `-D` when `gh` finds the branch's PR merged (a squash merge never looks
merged to git) or with `--force`. An unmerged branch is kept and named.

By hand, from the **trunk** checkout, after reap:

```bash
rm -r .worktrees/<slug>/target-local
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
- New worktree setup: `scripts/new-worktree.sh`; Cursor's hook
  [`.cursor/worktrees.json`](.cursor/worktrees.json) runs `uv sync` only (build into the
  tree's own `target-local/` when you need a binary)
- Releases: `scripts/release.sh X.Y.Z` (CLAUDE.md "Release")
- Driving barca from scripts/agents: `barca docs agents` / `barca docs skill`
- Release notes: put your `CHANGELOG.md` bullet under `## Pending`, never in a released
  `## X.Y.Z` section; CI (`scripts/check-changelog.py`) fails a PR that edits one, unless a
  commit message has a `Changelog-Fix: <reason>` line for a deliberate fix.
