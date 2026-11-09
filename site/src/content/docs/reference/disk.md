---
title: "Disk Use"
description: "The size limit for .barca/, what barca gc evicts and in which order, and how the metadata database keeps to its share."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/disk.md
---

<!-- Generated from crates/barca-cli/docs/disk.md (barca docs disk). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

This topic owns what the size limit of `.barca/` counts, what is evicted in which order, what is
never evicted and how the metadata database keeps to its own share; `barca gc` runs the same
eviction on demand.

`.barca/` has a size limit. Without one, every recompute under a new run hash (a code edit, an
input change, a sensor tick) adds an artifact and keeps the old one, so the cache only grows.

```toml
# barca.toml
[cache]
max_size = "20GB"        # or "500MB", "20GiB", a number of bytes, "10%" of the filesystem, or "off"
```

- With no `max_size`, the limit is **10% of the filesystem** `.barca/` is on. `"off"` removes the
  limit.
- `BARCA_CACHE_MAX_SIZE` overrides `barca.toml`, for a container that sets its limit when it is
  deployed. An invalid value is a usage error (exit 2) naming where it came from.
- `KB`, `MB`, `GB`, `TB` are decimal (`GB` = 10^9 bytes); `KiB`, `MiB`, `GiB`, `TiB` are binary;
  a bare number is bytes. A percentage is of the filesystem's size, read at each check.
- The budget covers `.barca/` across every environment: artifacts, metadata DBs, staging,
  `sql-cache/` and `range-cache/`, excluding recognized local metadata backups. Each DB is
  counted and keeps to its own share ("The metadata database" below). Backup files, owned sidecars, unsent markers and temporary
  copies in `.barca/backups/` and `.barca/envs/<env>/backups/` are reported separately,
  outside this budget; the five-copy rotation stays. Unrelated files and directories merely
  named `backups` remain budgeted. Physical totals still include backup storage.

**When.** At the end of `barca get` and `barca run` (after results are recorded and uploads have
finished), after every run under `barca serve`, and once a minute under `barca serve`. When
`.barca/` is over the limit, barca deletes files in this order until it is at or below **80%** of
the limit:

| Order | What | Which first | Costs when needed again |
|---|---|---|---|
| 1 | Leftovers: `staging/<pid>/` of processes that are not running, `*.moved-aside*` directories, files in an artifact directory that no metadata row of any environment names, `sql-cache/` copies and `range-cache/` directories not used in the last 5 minutes | dead staging, then moved-aside, then unnamed files (oldest first), then the least recently used copies, then the least recently used range directories | nothing for most; a ranged read of a lazy input fetches its ranges again |
| 2 | Superseded results: a node (a partition key, for a partitioned asset) whose newest successful result is another one | oldest first | a recompute, only if the code and inputs return to that exact state |
| 3 | Local copies the artifact store holds (its upload was confirmed with a digest) | least recently read first | a download |
| 4 | Current results | least recently read first | a recompute |

**Never evicted:**

- what the command produced, read or returns: a path `barca get` just printed still exists when
  the command exits;
- what a run in flight in the same process holds (under `barca serve`, every run in progress
  pins the results it reads); while a run is starting and its plan is not known yet, only tiers
  1 and 2 are evicted;
- with a run in progress in **another process** on this `.barca/` (the `running` rows of
  `barca history`, any environment), only tiers 1 and 2, and no staging directory (that process
  may be in another container, where a pid that looks dead here is alive);
- the current result of a `freshness=Manual` node that the store does not hold: nothing but a
  person recomputes a `Manual` node. Barca finds `Manual` nodes by reading the project's source
  files (no import); if one of them does not parse, no current result without a store copy is
  evicted;
- a result whose upload to the artifact store is still pending (`unsynced`, `local only` in `barca status`):
  its local copy is the only one until the next command uploads it;
- a file in an artifact directory that no result names, while a run is in progress on this
  `.barca/` (in any process) or for a minute after it was written (unless it is older than the
  metadata DB): its row may not be written yet;
- the metadata DBs, their logs, locks and backups, and the databases `barca state reset` and
  `barca state restore` moved aside (`metadata.db.reset-<UTC>`, `metadata.db.pre-restore`):
  never deleted (a DB's old rows can be: "The metadata database" below). Backups are outside the budget; the DBs and moved-aside copies are counted.
  Nothing outside `.barca/` is ever deleted, and the artifact store is
  never touched.

**The metadata database.** Each environment's database may use a tenth of the limit, at most
1 GiB. When its file is past that, barca trims it down to 80% of its share, at the same moments as
eviction and for at most 5 seconds at a time, and nothing is evicted in a check that ran out of
that time still trimming. Eviction counts a database as at most its share, so one past it never
deletes artifacts that fit: `.barca/` can exceed the limit by what the database holds past its
share until a trim gets it back. `barca gc` trims until done, and `barca gc --dry-run` trims nothing. With the limit `off` only `barca gc` trims, to 1 GiB. The order:

| Order | What goes | What you notice |
|---|---|---|
| 1 | The change digests (`components`) of results that are not a node's latest, and all those recorded before 0.37.1 | A node whose digests went says a plain `changed` until it next runs |
| 2 | The stored outcome of all but the last 100 runs | `GET /runs/{id}` of an older run answers without `outcome` |
| 3 | Results older than their node's latest success whose file is already gone | Going back to exactly that code and those inputs computes again, instead of fetching from the store; `barca history` and `barca stats` no longer count them |
| 4 | All but the last 1000 runs, with their logs (not a run in the queue or still running) | `barca history` and `barca logs` no longer have them |

Results whose file is on disk, a node's latest success and anything newer, and the newest byte
change of a node are never removed, so a database whose current results alone take more than its
share stays over it. It says what it did on stderr:

```
[barca] metadata 39.2 GB over its 1.1 GB share of the limit: now 848 MB; cleared 566986 component records and 16826 run reports, removed 0 old results and 0 old runs
```

The file shrinks through `VACUUM`, which holds the database's lock while it rewrites the live data
(runs that start meanwhile wait) and which runs only when no run is alive, as do orders 3 and 4: they delete history, so they run only if a
`VACUUM` of what orders 1 and 2 cleared was not enough. On a server that is never idle the file
keeps its size and reuses the freed space until a moment with no run, and eviction still counts
the whole file.

**After `barca state reset` or `barca state restore`.** The artifacts the old database named stay
on disk, and the new one names none of them (after a restore, none the restored copy does not
name). They are leftovers: the next check past the limit evicts them first, and `barca gc
--max-size off` deletes them now. Until then, putting the old database back (`barca state restore
--from <the moved file>`) uses them again; one that was evicted is computed again when it is
needed. A reset says how much stays:
`[barca] the 129 KB of artifacts under .barca/artifacts stay on disk, and no record names them now: ...`.
With a server, a result fetched from the store for a hit in the server's ledger is also a file
this machine's database does not name; evicting it costs a download.

**What you see.** One line on stderr when something was evicted:

```
[barca] cache 21.4 GB over its 20 GB limit; evicted 312 files (5.4 GB): 290 superseded results, 22 local copies held by the store
```

Under `--json` the run result gains `"cache": {"size_bytes", "limit_bytes", "evicted_files",
"evicted_bytes"}` (sizes after eviction); the key is absent when nothing was over the limit. If
backups exist, `size_bytes` includes them; only budgeted bytes trigger eviction. If
everything that may be evicted is not enough, barca says so once per process:
`[barca] warning: cache working set 26 GB is larger than max_size 20 GB; results will be
recomputed often`.

An evicted result is handled like a missing artifact ("A cached result whose artifact is
missing", above), with its own wording: it is downloaded again if the store holds it, or computed
again under the same run hash, only when something needs to read it.

```
[barca] pipeline.py:features: its cached result was evicted to stay under the cache limit; computing it again
```

The step's `reason` is `evicted`. `barca status` shows the node as `stale` with reason `evicted`,
and `--dry-run` predicts `run` with reason `evicted`. A result the store holds is predicted
`cached`: it costs a download, not a recompute.

**`barca gc`** runs the same eviction now:

```bash
barca gc --dry-run              # what would be evicted, per tier, and what it leaves
barca gc                        # evict now, down to 80% of max_size
barca gc --max-size 5GB         # a one-off tighter budget
barca gc --max-size off         # no limit: every leftover and superseded result, nothing else
barca gc --json                 # {size_bytes, limit_bytes, target_bytes, after_bytes, tiers, evicted, failed, held, ...}
```

GC JSON keeps `size_bytes` and `after_bytes` as physical totals including backups.
`budget_before_bytes` and `budget_after_bytes` exclude `backup_bytes`; the limit and 80%
target apply to those budgeted bytes. Human output labels backups as outside the budget.
`barca doctor` reports `budget_bytes`, `backup_bytes` and physical `size_bytes` in its
cache-size check. Excluding backups is not a free-space guarantee: they still occupy disk.

`--dry-run` lists exactly what `barca gc` then deletes (the same order, from the same
measurement), unless something changes in between. With the limit `off`, `barca gc` evicts every
leftover and superseded result, which never costs a recompute of anything current. `--env NAME`
evicts only that environment's results (leftovers are shared; every environment's rows decide
which files are named). A file that cannot be deleted is skipped with one warning; `barca gc`
exits 3 only when a metadata DB cannot be read.

When `barca gc` cannot get down to its target, a `held by:` line says what holds the rest, and
`held` in the JSON lists it, largest first, as `{what, bytes, evictable}`: `results` (files a
result names), `unreferenced` (files none names, kept for now), `metadata` (the metadata DBs),
`db_copies` (databases reset and restore moved aside), `backups` and `other`. The DBs, their
moved-aside copies and the backups are never evicted (`evictable: false`); delete a
`metadata.db.reset-<UTC>` yourself once you no longer need to undo the reset:

```
nothing to evict: 355 KB budgeted of 1 KB
0 B metadata backups outside the budget; 355 KB physical total
held by: 115 KB metadata databases (never evicted), 111 KB databases moved aside by state reset/restore (never evicted; delete them once you no longer need to undo), 2 B other
```

**How it is measured.** The check after a command adds the recorded sizes of the results the
metadata DBs hold, walks everything outside the artifact directories, and applies what the last
full walk of the artifact directories found. A full walk happens when that says `.barca/` is over
the limit, at least once a day, and under `barca gc`. The check under the limit takes about 2 ms
on a small project and about 0.1 s with 100,000 results recorded. Eviction after a command stops
after 5 seconds; what is left waits for the next command. When eviction cannot get under the
limit, the check does not plan again for 10 minutes unless the size grows.

A full walk reads the metadata DBs 20,000 rows at a time and lets go of barca's lock between
them, so runs, and `barca serve`'s other requests, go on while it reads; under `barca serve` it
runs on a thread of its own. A result whose file the walk found is not looked at again. One
full walk of a `.barca/` runs at a time in a process: a check that finds one under way (a run
that ends while `barca serve` walks) skips its own, and the walk under way records the size.
With 465,000 artifact files and 2.8M recorded results a full walk takes minutes on a cold
filesystem cache.

**Read times.** "Least recently read" is the `last_read_at` of a result: set when a step is given
it as an input, when `barca get` returns it, and when a `barca sql` query names its view. File
access times are not used (most filesystems do not keep them). A key of a partitioned asset
with 256 keys or more whose `last_read_at` is less than an hour old keeps it: a run that reads
200k cached keys does not write 200k rows, and an hour does not change which results are the
least recently read.

Known limits:

- Sizes are file lengths, not the blocks the filesystem allocates.
- barca reacts to its own limit only: it does not evict because something else filled the disk,
  and a run that writes more than the free space fails with the operating system's error.
- A run in another process that has just decided a superseded result is cached (an older run
  hash it went back to) can find it gone while it reads it; it is then computed again.
- Eviction deletes no `materializations` rows. Only trimming the metadata database does ("The metadata database").

See also: [`barca docs cache`](/reference/cache/), [`barca docs layout`](/reference/layout/), [`barca docs remote`](/reference/remote-storage/), [`barca docs contract`](/reference/cli-contract/).

---

*The same text is in your terminal: `barca docs disk`.*
