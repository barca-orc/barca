---
title: "Batched Partitions"
description: "batch=: one call of a partitioned step computes many keys; grouping, the batch function, what stays the same, calls and known limits."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/batched-partitions.md
---

<!-- Generated from crates/barca-cli/docs/batched-partitions.md (barca docs batched-partitions). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

`batch=` makes one call of a partitioned step compute several keys, for steps where each call
has a cost of its own (loading a model, opening a connection). Below: how keys are grouped into calls, what a batch function receives and
returns, what does not change (run hashes, artifacts, caching per key), how calls are reported
and the known limits. Partitions themselves are [Partitions](/reference/partitions/).

The example pipeline these outputs come from (a `retailer_versions` sensor, `orders` and
`scored` with `batch="all"` over 20 retailers) is in [Partitions: Processing many keys per call](/reference/partitions/#processing-many-keys-per-call), with the `--batch` overrides.

A tick where 3 of 20 retailers changed (real output; the first run made one call of 20 keys per
asset):

```
$ barca get scored pipeline.py --dry-run
Dry run: barca get scored (nothing executed, nothing written)

STATUS    WHY                                                                     STEP
will run  sensors always re-run                                                   pipeline.py:retailer_versions
partial   17 of 20 keys cached; will run: retailer=r03 (value pipeline.py:retailer_versions changed), retailer=r11 (value pipeline.py:retailer_versions changed), retailer=r17 (value pipeline.py:retailer_versions changed); 3 keys in 1 call (batch=all)  pipeline.py:orders
partial   17 of 20 keys cached; will run: retailer=r03 (input pipeline.py:orders changed), retailer=r11 (input pipeline.py:orders changed), retailer=r17 (input pipeline.py:orders changed); 3 keys in 1 call (batch=all)  pipeline.py:scored

7 will run, 34 cached, 0 unknown

$ barca get scored pipeline.py
[pipeline.py:orders[batch=b1]] reading orders of 3 retailers
[barca] batch:pipeline.py:orders 1 call, 3 keys | import 0.0s, load 0.0s, compute 0.0s, split 0.0s, write 0.0s
[barca] batch:pipeline.py:scored 1 call, 3 keys | import 0.0s, load 0.0s, compute 0.2s, split 0.0s, write 0.0s
[barca] 7/41 steps | done in 0.3s
[barca] metadata 0.0s recording this run's results
Run 3bc29e9a2595 | got 'scored' in 0.277s (7 steps, 2 phases)
...

$ barca history 3bc29e9a2595
...
STEP                                STATUS   ATTEMPTS  TIME  PEAK_RSS  RUN_HASH      ARTIFACT
pipeline.py:retailer_versions       success  1         0.0s  61.3 MB   63866cd5003e  .barca/artifacts/pipeline.py--retailer_versions/63866cd5....json (json, 600 B)
pipeline.py:scored[3 keys]          success  1         0.2s  189 MB    -             batch b2
  pipeline.py:scored[retailer=r03]  success  1         0.1s  -         00e495e3007e  .barca/artifacts/pipeline.py--scored_retailer_r03/00e495e3....parquet (parquet, 404 B)
  ...
```

## Choosing the grouping

| `batch=` | Keys per call |
|---|---|
| omitted / `None` | one; the function takes a single key (the default) |
| `20` | at most 20, in key order |
| `"all"` | every key that needs to run, in one call |
| `batches([["r1", "r2"], ["r3", "r4", "r5"]])` | the groups named; a key in no group runs alone |
| `batches([...], strict=True)` | the same, and a key in no group is a usage error (exit 2) |
| `batches_by(region_of)` | keys with the same label (`region_of("r1") == "emea"`) share a call |
| `batches_by(region_of, max_size=20)` | the same, each label's keys split into calls of at most 20 |

- **Key order** is the declared order: a `partitions([...])` list's order, a keyed source's dict
  order, the upstream's order for `partitions_from(<partitioned asset>)`. Grouping is stable
  from run to run. A node of several dimensions orders its keys by their dimension names.
- **Only keys that need to run are grouped.** With a keyed sensor where 3 of 200k retailers
  changed, `batch="all"` makes one call with 3 keys; a call whose keys are all cached makes no
  step, and a partly cached group runs its stale keys together.
- **`batches(<expression>)`**: a literal list is read without importing anything; any other
  expression is evaluated when barca loads the pipeline, with the project importable, as for
  `partitions(<expression>)`; one that raises is a load error naming the node (exit 2; under
  `serve`, an entry in `/health` `load_errors`). `batches_by(fn)` is checked the same way when
  the pipeline loads, then called on the keys that run: a function that raises on a key, or a
  label that is not a string or an integer, is a usage error naming the node (exit 2),
  before any of its keys runs. Derived keys are checked once their source has run. With several
  dimensions, `fn` receives the key as a dict of dimension to value.
- **Where it is allowed:** a partitioned `@asset` or `@task`. On an unpartitioned step it is exit
  2; `@sensor` does not take it. `batch=0`, a negative number, any other string, an empty group,
  a key in two groups, and a partition parameter annotated as a scalar (`retailer: str`) are exit
  2, naming the fix.
- **For one run:** `--batch none|all|N` on `get` and `run` replaces `batch=` of every node that
  declares one; `--batch NODE=V` (repeatable) of the nodes named. A node without `batch=` takes a
  single key and is never batched by an override; naming one is exit 2. The same override is
  `RunRequest.batch` over HTTP (`["all"]`, `["scored=20"]`), `barca.get(..., batch="all")` or
  `batch={"scored": 20}` in Python, and `Schedule("0 * * * *", batch="all")` for the ticks of a
  schedule under `barca serve`.

## Writing a batch function

The partition parameter is the list of the call's keys. Each input reads by where it comes from:

| Input | Annotation | The function receives |
|---|---|---|
| partitioned upstream, same dimension | `dict[str, T]` or none | key to that key's value |
| partitioned upstream, same dimension | `duckdb.DuckDBPyRelation`, `polars.LazyFrame`, `pyarrow.dataset.Dataset` | one lazy table over the call's keys' artifacts, with the dimension as a column; nothing is read until it is queried |
| keyed source (a dict-returning `partitions_from` source) | none, or a dict | key to that key's value |
| unpartitioned upstream, `collect(...)` | as usual | the whole value, once per call |

The function returns either:

- **a dict of key to value**: each value is written as that key's artifact, by the usual type
  rules ([Output Formats and Types](/reference/types/));
- **a table with the dimension as a column**: a pandas, polars or Arrow table, or a DuckDB
  relation. barca splits it into one table per key; a key with no rows gets an empty table with
  the schema. DuckDB relations, polars and Arrow tables stream through DuckDB's
  `COPY ... PARTITION_BY`, so the result is not held in Python memory; pandas is split in memory.
  Each key's rows are sorted by every column (DuckDB's `ORDER BY ALL`), not kept in the order
  the step produced them, so the same rows in any order give the same artifact bytes and a
  `cutoff=True` consumer stays cached. Values that compare equal but differ in representation
  (`-0.0` and `0.0`, distinct NaNs, strings under a case-insensitive collation) can still
  change the bytes; normalize them in the step. DuckDB 2 sorts while it writes; DuckDB 1.x, a
  key written as several files and tables with a BIT or BIGNUM column are read back and
  rewritten sorted, one extra pass over that key. Either way the sort about doubles the write:
  one key of a million rows and four columns took 102-112 ms against 53 ms unsorted with 32
  DuckDB threads, and 154-166 ms against 73 ms with 4; the extra pass took 101-103 ms and
  136-149 ms (barca 0.39.4, DuckDB 2.0.0, `benchmarks/RESULTS.md`).

```python
import pandas as pd
from barca import Failed, asset, partitions, partitions_from

@asset(partitions={"retailer": partitions(["r1", "r2", "r3"])})
def cleaned(retailer: str) -> pd.DataFrame:
    return pd.DataFrame({"retailer": [retailer], "amount": [len(retailer)]})

@asset(partitions={"retailer": partitions_from(cleaned)}, batch=20)
def scored(retailer: list[str], cleaned: dict[str, pd.DataFrame]) -> dict:
    out = {}
    for r in retailer:
        if cleaned[r].empty:
            out[r] = Failed(f"{r} has no rows")    # fails this key alone
        else:
            out[r] = {"retailer": r, "score": int(cleaned[r]["amount"].sum())}
    return out
```

- **Missing key:** a key of the call with no result fails ("batch returned no result for
  retailer=r7"); the other keys succeed.
- **Extra key:** a dict key or a table row whose key is not one of the call's (matched by its
  string form: `1` is the key `"1"`) fails the whole call.
- **`barca.Failed("reason")`** as a key's value fails that key alone (`BatchKeyFailed` in
  history); the other keys succeed and are cached.
- **Lazy tables need tabular artifacts:** parquet, CSV, or a JSON list of records (CSV and
  parquet only for `pyarrow.dataset.Dataset`). Any other artifact (a pickle, a JSON object) is a
  step error naming `dict[str, T]` as the fix; nothing is converted.

## What stays the same

- **Caching is per key.** Each key has the run hash, artifact and materialization it has without
  batching. `batch=` and the grouping are not part of any hash, so a key computed in a call of
  1000 is a cache hit for `--batch none`, and the other way round. Changing a function to the
  batch form (a list instead of a scalar) is a code change: its keys run once more.
- **Per key:** `barca status`, `--partition` (it narrows first, then groups), `--refresh`,
  keyed sources (they decide which keys run; batching groups those), `cutoff=True` (each key's
  bytes are hashed), sinks (a dataset sink writes one directory per key), `collect` (it reads
  every key's artifact), and a server's ledger (lookups and posts per key).
- **One call is one step to run:** it runs in one worker; the calls of a node run in parallel up
  to the pool and the capacity. `weight=` applies per call (`batch="all"` with `weight=8` holds 8
  for the call), as does one admission against a server's shared capacity. `retries=`,
  `retry_backoff=` and `timeout_seconds=` apply per call; a retry reruns only the keys that
  failed, together.
- **A call that raises fails all its keys.** Keys are not split and retried one at a time:
  `--batch none` is the debugging tool.

## Seeing calls

- **Progress:** the bar and `--agent` count keys, so a run reports thousands of
  `step:...[key] completed 0.0s` lines however few calls it makes. Each batched node also gets
  one line before the end-of-run line: `[barca] batch:<node> 7 calls, 3077 keys | import 0.1s, load
  1.2s, compute 8.4s, split 3.1s, write 14.2s`. Each number is a sum over the node's calls, which
  can run in parallel, so they can add up to more than the run's wall time. A later
  `[barca] metadata 2.1s recording this run's results` is the coordinator handling each finished
  key and writing the metadata DB; part of it overlaps the workers rather than adding to the wall
  time. These lines are also in `barca logs RUN_ID`, with or without `--pretty`.
- **JSON:** a batched step of a real run gains `batch_execution` `{calls, keys, import_seconds,
  load_seconds, compute_seconds, split_seconds, write_seconds}`, and the run gains
  `metadata_seconds` on success. `compute` is the function call. A lazy table input is read during
  it, and a returned DuckDB relation runs during `split`, so read the two together for such a
  step. `write` is writing and hashing each key's artifact and sinks.
- **What is not counted:** a call that wrote no key (it raised, or every key failed) is not in
  these totals; its time is in its keys' `elapsed_seconds` as before. A key's `elapsed_seconds` is
  its share of the call plus its own write, not the time of a call of its own.
- **History:** `barca history RUN_ID` lists a call as `scored[N keys]` with its total time and the
  call's peak memory, its keys under it (the first 10; `--json` lists every key, each with the
  call's id in `batch`).
- **Logs:** a call's stdout and stderr are recorded once, under `scored[batch=<id>]`, and found
  from any of its keys: `barca logs RUN_ID --step 'scored[retailer=r7]'`. A failed key's
  `output_tail` is its call's output.
- **Dry run:** a batched step gains `batch` `{grouping, keys, calls}` in JSON and "3 keys in 1
  call (batch=all)" in its WHY column. `calls` is `null` for `batches(...)`/`batches_by(...)` over
  more keys than the dry run lists (20).
- **Failed keys:** `barca status --json` and `GET /state` give a node's `partitions.failed` and
  the first 20 `failed_keys`, the keys whose latest attempt failed; a node below it that reads
  those keys key for key is `blocked` ([Partitions: Failed keys block their own keys](/reference/partitions/#failed-keys-block-their-own-keys)). `GET /assets/{name}/partitions`
  pages through the keys that have an attempt (`?status=failed`, `?q=`) and groups the failures'
  errors. `barca history RUN_ID --json` adds `steps_page.nodes`: each node's keys, failed keys,
  summed time and calls over the whole run, however few keys `steps` lists.
- **Peak memory** is measured per call (each key of a call reports the call's peak), to tune `N`.
- **Telemetry:** with `BARCA_TELEMETRY=datadog` and ddtrace in the worker, one `barca.execute`
  span per call, with `barca.batch.keys`; the coordinator's spans stay per node and per key.

## Known limits

- **Memory:** `"all"` has no automatic cap; `batch=N` is the cap. A call returning a pandas frame
  is split in memory.
- **Several dimensions:** a call's keys are tuples of values (in the order of the dimension
  names, sorted) in dicts and returned dicts. Lazy table inputs carry every partition dimension
  as a column.
- **Progress lines** stay per key (`--agent` prints a `completed` line per key), and a call's
  time is shared evenly between its keys in `elapsed_seconds`.
- **A `batches_by` function** runs in a separate Python process when the keys that run are
  known, once per batched node per phase (about 50 ms plus the module's import).
- **The dry run** counts calls from the keys it lists: for `batches(...)` and `batches_by(...)`
  over more than 20 keys, `calls` is `null`.

## When a consumer's keys are stale

`--batch` (`RunRequest.batch` over HTTP, `batch=` in `barca.get`) groups the keys that already
need to run. It is not part of any hash, so on its own it recomputes nothing: `barca get scored
pipeline.py --batch scored=all` after a full run runs no step. To recompute a partitioned
consumer's keys, name it, or an asset above it, in `--refresh`:

```bash
barca get scored pipeline.py --refresh cleaned                         # cleaned, then every key of scored
barca get scored pipeline.py --refresh scored --batch scored=all       # every key of scored, in one call
```

- **A refresh reaches its own cone only.** It recomputes the assets it names and what is below
  them on the way to the run's target. Run hashes cover code and inputs, not output bytes, so
  when a refreshed asset writes new bytes (its data changed in place), a consumer outside that
  cone keeps its run hash and its old result: after `barca get cleaned pipeline.py --refresh
  cleaned`, `scored` is served from cache as it was, and `barca status` reports it `cached`.
  Refresh with a target below it (`barca get scored pipeline.py --refresh cleaned`), or name it
  in `--refresh`. Data that changes in place is better read through a sensor or
  `freshness=Manual`: their bytes count in what reads them ([The Cache](/reference/cache/)).
- **A request with both `refresh` and `batch`** recomputes because of `refresh`: the same
  `refresh` list without `batch` recomputes the same keys, grouped by the declared `batch=`.

See also: [Partitions](/reference/partitions/), [The Cache](/reference/cache/), [Assets, Sensors and Inputs](/reference/assets/), [CLI Contract](/reference/cli-contract/).

---

*The same text is in your terminal: `barca docs batched-partitions`.*
