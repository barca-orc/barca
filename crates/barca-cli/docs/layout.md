# Where things live: the project root and `.barca/`

Everything barca keeps is under `.barca/` in the project root. Below: where that root
is and what the directory holds, what happens when several barca processes share one project,
what a run records while it goes and after it is killed, and what an upgrade recomputes.

## Where things live

```
.barca/metadata.db                          run history and materialization records (a local libSQL database)
.barca/artifacts/<node>/<run_hash>.<ext>    one file per result
.barca/envs/<env>/                          the same layout for a named environment (`barca docs cache`, "Environments")
.barca/backups/                             with a store: the five newest backups of the metadata DB (`barca docs remote`)
```

Every other entry barca writes under `.barca/` (locks, `run-owners/`, `staging/`, `sql-cache/`,
`range-cache/`, `live-logs/`, `branches/`, `inspection-cache/`, `auth/`, `gc-state.json`, the
files `state restore` and `state reset` move aside, a batched call's `.batch-*` directory), who
owns it, when it is removed and whether `barca gc` evicts it or the size budget counts it, is the
table in `specs/interfaces/dot-barca-layout.md`, the one owner of the layout; the site's
configuration page points there too. Delete nothing by hand to force a recompute: use
`--refresh` (`barca docs refresh`, "Forcing a recompute").

`<ext>` is `.json`, `.pkl`, `.parquet` or `.csv` (see `barca docs types`). An artifact's path
names the computation, not the bytes: the run hash covers the step's code and inputs, so the same
step with the same inputs always writes the same path, and computing it again overwrites the file
(`barca docs artifacts`, "Recompute byte checks"). To keep artifacts and a backup of the history in a bucket, set
`BARCA_REMOTE_URI` and your cloud's credentials (`barca docs remote`).

`.barca/` lives in the **project root**: the nearest directory at or above the one you run barca
from that holds a `barca.toml`. Without a `barca.toml` above you, the current directory is the
root. Barca changes into the root before doing anything, so:

- running from a subdirectory reads and writes the same cache as running from the root
  (`cd sub && barca get out ../pipeline.py` is a cache hit after `barca get out pipeline.py`);
- file arguments are read relative to where you typed them, and node ids are relative to the
  root, so `pipelines/p.py:out` is the same id from any directory;
- steps run with the root as their working directory, so `Path("data.txt")` inside a step means
  the root's `data.txt` wherever you invoke barca;
- stderr says `barca: project root: <path>` whenever the root is not the current directory;
- the project root is the boundary of the dependency cone (`barca docs hashing`, "Not followed").

Put an empty `barca.toml` at the top of a project to anchor it. Barca records the metadata
format version and refuses a database it does not support rather than rebuilding it
(`specs/interfaces/metadata-schema.md`).

## Concurrent runs

Several barca processes can run in one project at once (parallel scripts or agents, `barca serve`
alongside the CLI). The metadata DB is a single-file database that one process opens at a time,
so each process holds a short lock on it (`.barca/metadata.db.lock`) only while it reads or
writes, and releases it while your Python runs. Processes queue instead of failing. If a
process waits more than 60 seconds for the lock you get an error that names the lock file;
an `... File is locked by another process` error means something outside barca (a DB browser, a
backup tool, an older barca) has `.barca/metadata.db` open.

If a required database write fails, the command reports an infrastructure error (exit 3) and
records no new outcome; what was already recorded stays. With an artifact store, a refresh in
one process can replace an artifact another process is reading: the path names the computation,
not the bytes. What an upload records in that case is specified in
`specs/interfaces/artifact-upload-receipts.md`.

## While a run is going, and after one is killed

A run records each completed step in `.barca/metadata.db` while it runs, normally within about
half a second (with an artifact store, once its upload is confirmed). The order of these writes
and how a failed write is retried are specified in `specs/behavior/cache-decisions.md`.

```bash
barca status pipeline.py        # from another terminal: steps the running get has finished are `cached`
barca history --json            # the run is `running`; `steps_executed` is the steps recorded so far
```

- **Progress.** `barca status` in a second terminal shows what a running `barca get` or
  `barca run` has finished: an asset is `cached`, a partitioned asset is `partial` with its
  `cached` / `missing` counts. A step that is still running shows its previous state.
- **A killed run keeps what it finished.** If the process is killed (`kill -9`, out of memory, a
  lost machine), the next `barca get` serves the recorded steps from cache and computes the rest.
  A step is recorded only after its artifact is completely written, so a recorded step always has
  its file. A step that finished in the last half second before the kill may not be recorded: it
  runs again. Failed steps are recorded when the run ends, so a killed run records its successes
  only.
- **History says so.** `barca history` reports a run whose process no longer exists as
  `interrupted`, with `finished_at` and `elapsed_seconds` `null` (nobody saw it end) and
  `steps_executed` at what it had recorded. It does so as soon as the process is gone, and
  also for a run killed with its container, once a container starts again on the same
  `.barca` (see the limits below). The next `get` or `run` writes `interrupted` to the history.
  Ctrl-C and SIGTERM (a supervisor, `docker stop`) are different: the run stops its workers,
  records itself and is `cancelled` (exit 130). With an artifact store that also holds while
  artifacts upload or download or the history is backed up, and the cancelled run then backs
  its record up for at most 10 seconds; a second Ctrl-C ends that (`barca docs remote`, "Ctrl-C").

**When a run is `interrupted`.** Only when barca can observe that the run's process is gone;
whenever it cannot, the run stays `running`. Barca looks at the process table on the same
kernel, at a marker file the process holds open (`.barca/run-owners/`) from another container on
the same kernel, and at whether the owner's kernel has shut down for a marker on a local disk
(`specs/behavior/cache-decisions.md`). Machine ids, host names and clocks decide nothing: a killed run
reads `interrupted` at once, and a suspended laptop or a stopped process (Ctrl-Z) does not make a
live run look dead.

Known limits:

- With a store (`barca docs remote`) all of this holds on the machine the run is on. Newly
  committed progress is backed up every minute in healthy operation, without ending the run,
  and the final outcome when it ends; a backup is restored only by `barca state restore`.
- With a remote artifact store, an unfinished upload is never recorded as reusable progress.
  A failed upload is recorded at the end of the run as a pending upload, `unsynced` in
  `barca status` and uploaded by the next command (`barca docs remote`, "When an upload fails
  after the steps succeeded"). A failed backup leaves local history intact and is retried;
  ticks with nothing new take no backup.
- A run that is going is shown as `running` from everywhere that sees the project directory.
  The one situation in which a history says `interrupted` for a run that is going somewhere is
  a copy of the whole disk (a cloned or restored virtual machine) taken during the run and
  started as another machine: the copy's history is a copy too, and in it the run will never
  end. And two virtual machines resumed from the same memory snapshot are one machine as far as
  anything observable goes (same boot id, same processes): if they share a project directory, a
  run killed on one is shown as `interrupted` there while its twin on the other is still going.
- What stays `running` although its process is gone:
  - a run another machine started (its row came with a restored backup), until that machine
    records it;
  - with Docker Desktop, a run killed on the host as seen from a container, and a run
    killed in a container as seen from the host: they are different kernels and the
    directory between them is not a local disk. The side the run was on shows
    `interrupted`, and once it has recorded that, so does the other;
  - a run killed with its container when `.barca` is on a Docker Desktop bind mount (which
    gives no file handle) and another container was started before the replacement, so
    that the replacement got another pid namespace number. Restarting one container gives
    the same number in practice. A named volume for `.barca` does not depend on it;
  - a run killed in a container by a restart of the machine or of Docker's virtual machine
    when `.barca` is on a bind mount from a Docker Desktop host or on a network
    filesystem;
  - a run killed by a restart of the machine when the project is on a network filesystem.
- A marker is removed by its process when its runs have ended. One left by a killed process
  stays while its run reads `running`, and is removed by a later `get` or `run` once no
  `running` run names it and it is an hour old; so is one left by a process killed between
  creating its marker and recording its run. Markers are empty files.

After verifying that the process or container has stopped, explicitly reconcile an uncertain
row with `barca history --reconcile RUN_ID --json`. This records only that row as
`interrupted` and prints it in the normal history envelope (`runs`, `total: 1`,
`truncated: false`); `--fields` and human output also work. It refuses a proven live owner,
a terminal row, an unknown id, and combining `--reconcile` with `--limit` or `--all`.
It preserves recorded step counts, successful materializations and artifacts, and leaves
unknown end time and duration as `null`. It does not stop a process. Ordinary history reads
still keep an uncertain owner `running`; reconciliation is your assertion that it stopped.
The change is local to the selected environment's metadata database.

## Upgrading

A barca release that changes how run hashes are computed recomputes the affected steps once, on
the first run after upgrading; the release notes say so (`CHANGELOG.md`, GitHub Releases).

See also: `barca docs cache`, `barca docs disk`, `barca docs discovery`, `barca docs status`,
`barca docs remote`, `barca docs contract`.
