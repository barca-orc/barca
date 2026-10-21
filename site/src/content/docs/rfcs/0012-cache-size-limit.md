---
title: 'RFC-0012: A Size Limit for .barca'
description: 'max_size caps what barca keeps on disk. Past the limit, barca evicts leftovers, then superseded results, then local copies the store holds, then the least recently read results, and recomputes or refetches anything evicted when it is needed.'
---

- **Status:** Implemented (0.25.0)
- **Date:** 2026-10-20 (accepted 2026-10-20)
- **Touches:** barca-core | barca-cli | HTTP server
- **Supersedes / Related:** Reopens parked #83 (`barca gc`, keep N versions) with a size budget instead of a version count. Relies on the missing-artifact behaviour of #252 (a cached result whose file is gone is refetched or recomputed under the same run hash). Related: #403 (backups under `.barca/backups/`), #390 (pending uploads), #233 (parked artifact I/O bounds).

---

## 1. Summary

barca never deletes anything under `.barca/`. Every recompute under a new run hash adds a
file and keeps the old one, so a project's disk use only grows, and on a long-lived deployment
it fills the disk.

This RFC adds a budget:

```toml
[cache]
max_size = "20GB"
```

After every command and every `serve` run, if `.barca/` is over the budget, barca deletes files
in a fixed order until it is under 80% of it: leftovers first, then results nothing will read
again, then local copies of results the store still holds, then the least recently read
results. Anything evicted is fetched or recomputed the next time it is needed, as a deleted
artifact is today. `barca gc` runs the same eviction on demand.

## 2. Motivation

**A deployment filled its disk** (2026-10-20). Nothing in barca bounds `.barca/`: the remote
manual says `.barca/artifacts/` "is never pruned automatically", and the only remedy is deleting
it by hand.

**What grows, in order of size, from reading the code:**

- `artifacts/<node>/<run_hash><ext>` (and `envs/<env>/artifacts/`): one file per distinct run
  hash. A code edit, an input change or a sensor tick makes a new run hash; the previous file
  stays. A partitioned asset multiplies this by its keys.
- `sql-cache/`: every store object `barca sql` downloaded.
- `staging/<pid>/`: upload snapshots of a process that died before cleaning up.
- `*.moved-aside*` directories barca moved out of an artifact's path and never deletes.
- `backups/` once #403 lands (five rotating copies of the metadata database).

**Deletion is already safe.** Since #252, a cached result whose file is missing is fetched from
the store or recomputed under the same run hash when something needs it, and an artifact
nothing reads is never looked at. Eviction only has to choose what to delete, and in what
order, so the cost is the least recomputation.

## 3. Guide-Level Explanation

### 3.1 Setting the budget

```toml
# barca.toml
[cache]
max_size = "20GB"        # or "500MB", "10%" of the filesystem, or "off"
```

With no `max_size`, the limit is **10% of the filesystem** `.barca/` is on. `"off"` restores
today's unbounded behaviour.

`BARCA_CACHE_MAX_SIZE` overrides it, for a container that sets its limit at deploy time. Units
are decimal (`GB` = 10⁹ bytes) and binary (`GiB`); a bare number is bytes. A percentage is of
the filesystem `.barca/` is on, read at each check.

### 3.2 What happens past the limit

At the end of `barca get`, `barca run`, and every run under `barca serve`:

```
[barca] cache 21.4 GB over its 20 GB limit; evicted 312 files (5.4 GB): 290 superseded results, 22 local copies held by the store
```

One line on stderr, only when something was evicted. Under `--json` the run result gains
`cache: {size_bytes, limit_bytes, evicted_files, evicted_bytes}`.

Later, a step that needs an evicted result fetches it from the store or recomputes it. The
warning says why, instead of today's "artifact … is missing":

```
[barca] pipeline.py:features: its cached result was evicted to stay under the cache limit; computing it again
```

### 3.3 `barca gc`

```
$ barca gc --dry-run
would evict 1,204 files (38.2 GB), leaving 14.9 GB of 20 GB:
  leftovers            3 dirs     1.1 GB
  superseded         1,180 files  33.0 GB
  held by the store     21 files   4.1 GB
$ barca gc                       # evict now, to 80% of max_size
$ barca gc --max-size 5GB        # a one-off tighter budget
$ barca gc --json
```

`barca gc` works with no `max_size` configured: then it evicts only leftovers and superseded
results, which never costs a recompute of anything current.

### 3.4 Inspection

- `barca status` shows `evicted` (with a reason) for a node whose current result was evicted, as
  a kind of `stale` that costs a fetch or a recompute; `--dry-run` predicts it.
- `barca doctor` reports cache size against the limit, and free space on the filesystem.
- `GET /health` under `serve` adds `cache: {size_bytes, limit_bytes}`.

## 4. Reference-Level Explanation

### 4.1 Public API Surface

- **Config:** `[cache] max_size` in `barca.toml`; `BARCA_CACHE_MAX_SIZE`. Experimental.
- **CLI:** `barca gc [--max-size SIZE] [--dry-run] [--json] [--env ENV]`, experimental, with
  `--help` examples, a manual section and snapshots.
- **JSON:** `cache` on `get`/`run` results when eviction ran; `barca gc --json` reports per tier;
  `evicted` as a `barca status` state with `cache.reason: "evicted"`.
- **Exit codes:** eviction never fails a command. A file it cannot delete is skipped with one
  warning. `barca gc` exits 3 only if the metadata database cannot be read.
- **Metadata:** `materializations` gains `evicted_at`, added non-destructively under the
  metadata format policy. Rows are never deleted, so `barca history` and `barca stats` keep
  every run.

### 4.2 Eviction order

Size is everything under the project's `.barca/` (all environments), including the metadata
database and backups, which are counted but never evicted. When size exceeds `max_size`, barca
evicts in this order until size is at or below 80% of `max_size`:

1. **Leftovers.** `staging/<pid>/` of processes that are not running; `*.moved-aside*`
   directories; `sql-cache/` files, least recently used first.
2. **Superseded results.** An artifact whose `(node, partition key)` has a newer successful
   result. Nothing reads it unless the code or inputs return to that exact state. Oldest first.
3. **Local copies the store holds.** With an artifact store, a current result whose store object
   exists and whose recorded digest matches (the `cached` condition of RFC-0009). It is fetched
   again on demand. Least recently read first.
4. **Current results, least recently read first.** They are recomputed on demand.

Never evicted:

- results this command produced or returned. A path `barca get` just printed still exists when
  the command exits.
- results an in-flight run on this `.barca/` will read: `serve` pins the run hashes of every run
  it has in flight; a CLI command that finds another live run (the `runs` row's pid and host,
  #214) evicts only tier 1 and 2.
- the current result of a `Manual` node that has no store copy, because nothing recomputes a
  `Manual` node except a person (RFC-0008).
- results with an upload pending (#390), until the upload succeeds.

**Least recently read** is a `last_read_at` column on `materializations`, updated when a step
reads the result, `barca get` returns it or `barca sql` queries it. File access times are not
used: most filesystems are mounted `noatime` or `relatime`.

**When the working set does not fit.** If tiers 1 to 4 cannot get under the limit without
touching protected results, barca evicts what it may and warns once per process:
`cache working set 26 GB is larger than max_size 20 GB; results will be recomputed often`.

### 4.3 Implementation Details

- Accounting comes from the database (`artifact_size_bytes` of non-evicted rows), plus a walk of
  the non-artifact directories, so the check after a command costs a query and a short walk, not
  a full scan. A full walk reconciles files the database does not know about (written by an older
  barca, or by hand) at most once a day, or under `barca gc`.
- Eviction runs after results are persisted and uploads drained, bounded to a few seconds per
  command; whatever is left waits for the next command. `serve` also checks on its minute
  checkpoint.
- Deleting a file and setting `evicted_at` happen in that order; a crash in between leaves a row
  that looks cached with a missing file, which #252 already handles.
- Partitioned results evict per key.

### 4.4 Rust ↔ Python Boundary

None for eviction, which is all in the coordinator. Workers report reads they already make, so
`last_read_at` is set from the coordinator's own record of which inputs a step was given.

### 4.5 Edge Cases

- **The same artifact path in two environments** cannot happen: environments have separate
  artifact roots.
- **A remote store is not touched.** It is not this machine's disk, and it is the backup. Bucket
  lifecycle rules are the tool for bounding it; the manual says so.
- **Disk full before the check.** A run that writes more than the free space still fails with the
  OS error. barca reacts to its own budget only; it does not evict because something else filled
  the disk (decided 2026-10-20).
- **`max_size` smaller than the metadata database** evicts everything evictable and warns.

## 5. Determinism, Caching & Testing

Run hashes and cache decisions are unchanged; an evicted result is `evicted` instead of `cached`
and costs a fetch or a recompute, never a different value.

1. Tier order: a project with leftovers, superseded results, store-held copies and current
   results over a small limit evicts in that order and stops at 80%.
2. Protection: the command's own outputs, in-flight pins under `serve`, a storeless `Manual`
   result and a pending upload are never evicted.
3. After eviction, `barca get` of each target returns the same bytes (digest compared), with
   `fetched` or `recomputed` as expected and the `evicted` warning wording.
4. `barca gc --dry-run` predicts exactly what `barca gc` then deletes.
5. A CLI run alongside a live run evicts only tiers 1 and 2.
6. `last_read_at` moves on step input, `get` and `sql`.
7. The working-set warning fires once.
8. `trivial` with no limit configured does not regress; with a limit, the post-run check stays
   under a stated bound on a project with 100k materialization rows.

## 6. Performance

With no limit: nothing. With a limit and size under it: one aggregate query and a walk of the
small directories per command. Eviction cost is file deletion, bounded per command.

## 7. Drawbacks

- An evicted current result costs a recompute, which can be expensive; tier ordering and
  least-recently-read keep that rare, but a limit smaller than the working set will thrash.
- `last_read_at` is a write per read, batched into the run's existing persistence transaction.

## 8. Rationale & Alternatives

- **Keep N versions per node (#83).** Bounds versions, not bytes: one large asset with five
  versions can still fill a disk, and a thousand small ones are harmless. The request is a disk
  budget.
- **Evict by age (TTL).** Old does not mean unneeded; a monthly asset's current result is old and
  still read.
- **Delete the whole cache when full.** That is what people do by hand today; it recomputes
  everything, including current results that are expensive.
- **Content-addressed dedupe** (#246, parked) would remove duplicate bytes across run hashes but
  not bound growth.

## 9. Prior Art

Bazel's disk cache (`--disk_cache` with `--experimental_disk_cache_gc_max_size`), pip and uv
cache prune, Docker's `builder prune --keep-storage`. Dagster and Prefect leave local storage to
the user.

## 10. Decided and Unresolved

Decided 2026-10-20: the low watermark is fixed at 80%.

Decided by the maintainer on 2026-10-20:

- **Default:** with no `max_size`, the limit is 10% of the filesystem, so a fresh deployment
  cannot fill its disk. Breaking line: a project whose cache was larger sees more fetches and
  recomputes after upgrading; `max_size = "off"` restores the old behaviour.
- **No free-space floor:** barca evicts against its own budget only.
- **Release:** 0.25.0, with the output change (#447).

Unresolved: none. The low watermark was the last question (fixed at 80%, or configurable); it is
fixed at 80%.

## 11. Future Possibilities

- Bounding the store with lifecycle rules generated from the same tiers.
- Per-node retention hints (`@asset(keep=...)`) if the global order proves wrong for some nodes.
