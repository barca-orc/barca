---
title: "barca gc"
description: "Evict cached results to keep .barca/ under its size limit (experimental)."
---

Evict cached results so `.barca/` stays under its size limit (`[cache].max_size` or
`BARCA_CACHE_MAX_SIZE`, default 10% of the filesystem; see
[Cache size](/reference/config/#cache-size)). barca already does this after every `get` and
`run` once the limit is passed; `barca gc` does it now. Experimental.

## Usage

```txt wrap
barca gc [--dry-run] [--max-size SIZE] [--json]
```

```bash
barca gc --dry-run              # what would be evicted, per tier, and what it leaves
barca gc                        # evict now, down to 80% of the limit
barca gc --max-size 5GB         # a one-off tighter budget
barca gc --max-size off         # only leftovers and superseded results, all of them
barca gc --json                 # {size_bytes, limit_bytes, target_bytes, after_bytes, tiers, evicted, failed, restricted, dry_run}
```

```
$ barca gc --dry-run --max-size 400KB
would evict 4 files (1.4 MB), leaving 342 KB of 400 KB:
  superseded                 3 files    807 KB
  least recently read        1 file     549 KB
```

Each tier that has something to evict gets a line; `leftovers` and `held by the store` appear
the same way. With nothing to evict it prints one line, such as `nothing to evict: 284 KB of 3.4 GB`.

Every leftover and superseded result is always evicted, even under the limit, so each node keeps
only its newest result locally. Then, while over the target, local copies the artifact store
holds go, then the least recently read current results. `--dry-run` lists exactly what `barca gc` then deletes.
While another run is in progress on this `.barca/`, only leftovers and superseded results are
considered (`restricted` says why). `--env NAME` evicts only that environment's results. A file
that cannot be deleted is skipped with a warning; exit 3 only when a metadata database cannot be
read. The remote store is never touched.

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/exit-codes/)
