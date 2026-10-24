# Partitions

Partitions split one asset into independent steps, one per key; the keys run in parallel.
Each key is cached on its own: it has its own run hash, so a re-run serves unchanged keys from
cache and executes only keys with no successful materialization (a new key, or a key whose
function or upstream changed). A `collect` fan-in re-runs when its set of inputs changes.

```python
from barca import asset, partitions, partitions_from, collect


@asset(partitions={"region": partitions(["emea", "amer", "apac"])})
def sales(region: str) -> dict:
    return {"region": region, "revenue": len(region) * 100}


@asset(partitions={"region": partitions_from(sales)})   # same keys as `sales`
def margin(region: str, sales: dict) -> dict:
    return {"region": region, "margin": sales["revenue"] * 0.2}


@asset(inputs={"all_sales": collect(sales)})            # fan-in: every partition as a list
def summary(all_sales: list[dict]) -> dict:
    return {"total": sum(s["revenue"] for s in all_sales)}
```

- `partitions([...])` declares keys. A literal list is read statically; any other expression
  (a list comprehension, a function call) is evaluated by the Python runtime at plan time. It
  imports the asset's module the way a worker does, with the module's directory and the project
  root on `sys.path`, so a helper module beside it or a package of the project imports. An
  expression that raises (or a module that fails to import) is a load error naming the node,
  with the traceback, in `barca list`, `barca status`, `get` (exit 2 when it is the target),
  `GET /health` `load_errors` and `barca doctor`; the node and what reads it are left out of
  the plan, as for a file that does not parse. Up to 0.30 such a node got no keys, with only a
  warning.
- The partition key is passed to the function as the parameter named in `partitions={...}`. A
  dimension that is not a parameter of the function is an error when the file is read, exit 2
  (`barca docs assets`, "Inputs and parameters").
- Changing the keys: adding a key runs that key and no other; removing a key or reordering the
  list runs no key. Downstream, a `partitions_from` consumer runs for the new key only, and a
  `collect` fan-in runs again whenever a key is added or removed. This is the same however the
  keys are written: a literal list, a module-level constant, any other expression, or
  `partitions_from`. The keys are not part of the function's definition (`barca docs cache`,
  "Which decorator arguments count"); renaming the dimension is, and re-runs every key. (Up to
  0.18, editing a list written inside the decorator re-ran every key.)
- `partitions()`, `partitions_from()` and `collect()` take exactly one argument, by position. A
  keyword (`partitions(values=[...])`, `collect(asset_fn=sales)`), no argument or two are
  errors when the file is read, exit 2 (`barca docs assets`, "Accepted arguments").
- `partitions=` works on `@task` and `@sensor` as on `@asset`: one run per key.
- `partitions_from(upstream)` on a partitioned `upstream` gives the asset the same keys, and
  each key is called with the key and that key's output of `upstream`, passed as the parameter
  named after it: above, `margin(region="emea", sales=<the emea output of sales>)`. To receive
  it under another name, list it in `inputs=` too: `@asset(inputs={"s": sales},
  partitions={"region": partitions_from(sales)})` calls `margin(region, s)`. It chains
  (`partitions_from(margin)`) and works with keys from `partitions(<expression>)`. Each key of
  the consumer depends on its own key of `upstream` only: adding a key to `sales` runs that key of
  `sales` and of `margin`, and serves the other keys from cache.
- The dimension must keep the upstream's name (`"region"` above), `upstream` must have a single
  dimension, and `partitions_from(upstream)` must be the asset's only dimension. Anything else is
  a usage error (exit 2) when the DAG is built.
- Mixing `partitions()` and `partitions_from()` dimensions on one asset is unsupported
  and fails before execution with exit 2. Use only derived dimensions or declare every
  dimension explicitly with `partitions([...])`.
- `partitions_from(keys)` on an *unpartitioned* asset or sensor that returns a list uses the
  list's values as keys. They are only known once `keys` has run (a dry run reports the asset
  as `unknown` until then), and the list itself is not passed to the function.
- `partitions_from(source)` on an unpartitioned asset or sensor that returns a **dict** uses the
  dict's keys as keys, and each key's call receives **that key's value** as the parameter
  named after the source (or the `inputs=` key that names it); a function without that
  parameter gets the key only. Each key's run hash folds in its own value and the source's
  run hash, not the whole dict: when one value changes, that key and what `partitions_from`
  chains below it run again, and every other key is served from cache. See "Updating only
  what changed, across many partitions" below.
- What a `partitions_from` source may return: a list (or tuple) of keys, or a dict of key ->
  value. Dict keys are strings or integers; an integer key becomes its string (`{1: "a"}`
  gives the key `"1"`, as JSON writes it), and two keys that become the same string, or any
  other key type (a tuple, a bool, `None`), are an error. Values must be JSON-writable (str,
  int, float, bool, None, and lists and dicts of them; not NaN). The source is written as
  JSON whatever its return annotation. Anything else fails the source step (exit 1) with an
  error naming the key, or the types it may return. Up to 0.29.5 a dict was not read for keys,
  and its consumer failed with `TypeError: ... missing 2 required positional arguments`.
- A source that returns an empty list or dict has no keys: its `partitions_from` consumers
  run nothing, and a `collect` over them receives an empty list.
- `collect(upstream)` inside `inputs=` aggregates all partitions of `upstream` into one list.
- A partitioned asset in an *unpartitioned* asset's `inputs=` without `collect()`, for example
  `@asset(inputs={"all_sales": sales})`, is a usage error (exit 2) naming both fixes:
  `collect(sales)` for one list of every partition, or `partitions_from(sales)` to run once per
  key. (Up to 0.11 this silently passed the list, like `collect`.)
- An unpartitioned asset in `inputs=` is passed whole to every key, for example
  `@asset(inputs={"m": multiplier}, partitions={"k": partitions(["a", "b"])})` calls the
  function with `k` and `m`. It runs once, before any key, and its run hash is part of every
  key's run hash: changing it, or `--refresh multiplier`, re-runs every key (and, with the
  default cascade, everything downstream of them).
- Artifacts are stored per key, for example
  `.barca/artifacts/pipeline.py--sales_region_emea/<run_hash>.json`. `barca plan` lists one step
  per key, all under the same node id (`pipeline.py:sales`).
- A fan-in (`collect`) runs in its own phase after every partition has finished, also when the
  asset has one key or barca has one worker.
- Which keys run, and each key's run hash, do not depend on the number of workers
  (`BARCA_POOL_SIZE`, by default the machine's core count). Up to 0.18.1 they could: with a
  chain of `partitions_from` assets and fewer workers than keys, some keys of the second asset
  were recorded under a run hash that depended on how the keys were split across workers, and
  ran again when the split changed. Such a key, and a fan-in over it, runs once more after
  upgrading; with at least as many workers as the two assets have keys together, nothing
  changes.

## Updating only what changed, across many partitions

With many keys (10k, 200k retailers), poll once for all of them and let each key's value say
whether it changed. One sensor runs one query that returns every retailer and its version;
`partitions_from(retailer_versions)` makes one key per retailer, and each key is computed
again only when its version changed:

```python
import duckdb
from barca import Schedule, asset, partitions_from, sensor


@sensor(freshness=Schedule("*/5 * * * *"))
def retailer_versions() -> tuple[bool, dict]:
    rows = duckdb.connect("warehouse.duckdb", read_only=True).execute(
        "select retailer, max(updated_at)::varchar from orders group by retailer"
    ).fetchall()
    return True, dict(rows)                       # {"r000001": "2026-10-22 09:14:03", ...}


@asset(partitions={"retailer": partitions_from(retailer_versions)})
def cleaned(retailer: str, retailer_versions: str) -> dict:
    # retailer_versions is this retailer's value only: its max(updated_at)
    orders = duckdb.connect("warehouse.duckdb", read_only=True).execute(
        "select * from orders where retailer = ?", [retailer]
    ).df()
    return {"retailer": retailer, "rows": len(orders), "as_of": retailer_versions}


@asset(partitions={"retailer": partitions_from(cleaned)})
def scored(retailer: str, cleaned: dict) -> dict:
    return {**cleaned, "score": cleaned["rows"] * 0.1}
```

```bash
barca get scored pipeline.py          # first run: every retailer
barca get scored pipeline.py          # the sensor runs; every key is cached
barca get scored pipeline.py --dry-run    # after the sensor saw new data: which keys, and why
barca serve pipeline.py               # every 5 minutes: the sensor, then the changed retailers
```

A tick where 1 of 10,000 retailers changed, on the fixture of `benchmarks/keyed_partitions`
(the same chain, its sensor named `versions`), run with `barca get` in a terminal; under
`barca serve` the schedule runs the same:

```
$ barca get scored pipeline.py
[barca] 3/20001 steps | done in 0.2s
Run 7a0c4e7b7357 | got 'scored' in 0.249s (3 steps, 2 phases)

retailer=r000000: .barca/artifacts/pipeline.py--scored_retailer_r000000/5ededa19....json
...                                                   (one path per retailer, 10,000 in all)

$ barca history --limit 1
RUN_ID         CMD     TARGET                   STATUS      STEPS CACHED   TIME STARTED
7a0c4e7b7357   get     scored                   success         3  19998   0.2s 2026-10-23 01:57:48 UTC
```

Three steps ran: the sensor, and the changed retailer's key of `cleaned` and of `scored`;
the other 19,998 keys were served from cache. A dry run after the sensor has seen a new value
(a scheduled poll, or `barca get versions pipeline.py`) names the key and why:

```
$ barca get scored pipeline.py --dry-run
Dry run: barca get scored (nothing executed, nothing written)

STATUS    WHY                                                                     STEP
will run  sensors always re-run                                                   pipeline.py:versions
partial   9999 of 10000 keys cached; will run: retailer=r000010 (value pipeline.py:versions changed)  pipeline.py:cleaned
partial   9999 of 10000 keys cached; will run: retailer=r000010 (input pipeline.py:cleaned changed)  pipeline.py:scored

3 will run, 19998 cached, 0 unknown
```

- In JSON (`--dry-run --json`, `barca status --json`), a partitioned step's `partitions`
  object has `changed`: for each previewed key that ran before, what changed in its run hash
  (`value ID` for a dict source's new value). A new key has no entry. Like every sensor
  prediction, the dry run assumes the sensor returns what it last returned.
- The value hash is canonical JSON: the value as written to the source's JSON artifact, with
  object keys sorted at every level and no whitespace, then SHA-256. Reordering the dict, or
  the keys of a nested dict, changes nothing; an integer outside 64 bits is compared as a
  float.
- Editing the source's code (its query) changes its run hash, and every key runs again, as for
  anything that reads a sensor (`barca docs cache`). Its `freshness=` does not count.
- The source may be an asset instead of a sensor. With `@asset(cutoff=True)` on it, each key
  folds in its own value only, not the source's run hash, so when the asset reruns (an upstream
  changed) only the keys whose value changed run. Without `cutoff`, every rerun of the asset
  reruns every key.
- A removed key runs nothing; `barca get scored` returns the remaining keys. A new key runs
  that key only. A `collect(scored)` downstream runs whenever a key is added, removed or
  recomputed, since its inputs changed.
- Return only what identifies a retailer's data (a version, a max timestamp, an etag), not the
  data itself: the whole dict is written once per poll and read by every worker that runs a
  key.
- The cost of a tick grows with the number of keys even when one changed: each key is hashed
  and looked up. Measured on one loaded machine (`benchmarks/RESULTS.md`, "Keyed
  partitions"), `barca get` of the chain with one key changed: 0.32 s at 10k keys, 1.7 s at 50k,
  7 s at 200k (it also checks each returned artifact is there); a `barca serve`
  tick at 200k: 4.3 to 4.6 s. A sensor that runs once per key
  (`@sensor(partitions={"retailer": partitions(KEYS)})`) makes one call per key on every
  tick instead: 3.3 s at 10k keys, 16 s at 50k.
- Under `barca serve`, what a tick keeps does not grow with the keys it served from cache:
  its outcome lists the keys it computed and counts the others ("Many keys" below). At 200k
  keys with one key changing per tick, the metadata DB grows by about 50 KB per tick and
  the server's memory stays flat (`benchmarks/RESULTS.md`, "Bounded run records").

### A global step every key reads

An unpartitioned asset that every key reads (a rate table, a dimension, a model) reruns every
key below it whenever it reruns, even when it wrote the same bytes. Mark it
`@asset(cutoff=True)` and what reads it folds in the bytes it wrote instead of its run hash: a
rerun that writes the same bytes leaves every key cached, new bytes rerun all of them. Before
it runs, a dry run reports the keys below it `unknown` (reason `cutoff_pending`). On a
partitioned asset, `cutoff=True` applies per key. `barca docs cache`, "Early cutoff", has the
details.

## Processing many keys per call

A partitioned step runs once per key by default. `batch=` groups the keys that need to run into
fewer calls of the function (experimental, 0.32.0, RFC-0013). The function then receives the
list of its call's keys, so the same code serves one key, a batch of 20, or every key:

```python
import json
import pathlib

import duckdb
from barca import asset, partitions_from, sensor


@sensor()
def retailer_versions() -> tuple[bool, dict]:
    # {"r01": "2026-10-22 09:14:03", ...}: one version per retailer, from one query
    return True, json.loads(pathlib.Path("versions.json").read_text())


@asset(partitions={"retailer": partitions_from(retailer_versions)}, batch="all")
def orders(retailer: list[str]) -> duckdb.DuckDBPyRelation:
    # One scan for every retailer of the call; barca splits the result by `retailer`.
    print(f"reading orders of {len(retailer)} retailers")
    return duckdb.sql(f"select * from 'orders.csv' where list_contains({retailer!r}, retailer)")


@asset(partitions={"retailer": partitions_from(orders)}, batch="all")
def scored(retailer: list[str], orders: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    # `orders` is one lazy table over the call's keys, with the `retailer` column.
    return orders.aggregate("retailer, sum(amount) AS total, count(*) AS n")
```

```bash
barca get scored pipeline.py                     # first run: every retailer, one call per asset
barca get scored pipeline.py --dry-run           # which keys run, and in how many calls
barca get scored pipeline.py --batch none        # one key per call, to find the retailer that fails
barca get scored pipeline.py --batch 2           # at most 2 keys per call, this run only
barca get scored pipeline.py --batch scored=all --batch orders=100   # per node
```

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
[barca] 7/41 steps | done in 0.3s
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

### Choosing the grouping

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

### Writing a batch function

The partition parameter is the list of the call's keys. Each input reads by where it comes from:

| Input | Annotation | The function receives |
|---|---|---|
| partitioned upstream, same dimension | `dict[str, T]` or none | key to that key's value |
| partitioned upstream, same dimension | `duckdb.DuckDBPyRelation`, `polars.LazyFrame`, `pyarrow.dataset.Dataset` | one lazy table over the call's keys' artifacts, with the dimension as a column; nothing is read until it is queried |
| keyed source (a dict-returning `partitions_from` source) | none, or a dict | key to that key's value |
| unpartitioned upstream, `collect(...)` | as usual | the whole value, once per call |

The function returns either:

- **a dict of key to value**: each value is written as that key's artifact, by the usual type
  rules (`barca docs types`);
- **a table with the dimension as a column**: a pandas, polars or Arrow table, or a DuckDB
  relation. barca splits it into one table per key; a key with no rows gets an empty table with
  the schema. DuckDB relations, polars and Arrow tables stream through DuckDB's
  `COPY ... PARTITION_BY`, so the result is not held in Python memory; pandas is split in memory.

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

### What stays the same

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

### Seeing calls

- **History:** `barca history RUN_ID` lists a call as `scored[N keys]` with its total time and the
  call's peak memory, its keys under it (the first 10; `--json` lists every key, each with the
  call's id in `batch`).
- **Logs:** a call's stdout and stderr are recorded once, under `scored[batch=<id>]`, and found
  from any of its keys: `barca logs RUN_ID --step 'scored[retailer=r7]'`. A failed key's
  `output_tail` is its call's output.
- **Dry run:** a batched step gains `batch` `{grouping, keys, calls}` in JSON and "3 keys in 1
  call (batch=all)" in its WHY column. `calls` is `null` for `batches(...)`/`batches_by(...)` over
  more keys than the dry run lists (20).
- **Peak memory** is measured per call (each key of a call reports the call's peak), to tune `N`.
- **Telemetry:** with `BARCA_TELEMETRY=datadog` and ddtrace in the worker, one `barca.execute`
  span per call, with `barca.batch.keys`; the coordinator's spans stay per node and per key.

### Known limits

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

## Getting partitions

`barca get` on a partitioned asset returns every key, sorted by key: one artifact path per key,
and in JSON a `final_output` object keyed by partition key (`dimension=value`, dimensions sorted
and joined with `,` for a compound key). `-o value` maps each key to its value instead.

```bash
barca get sales pipeline.py                                  # one path per key
barca get sales pipeline.py -o value                         # {"region=amer": {...}, "region=apac": {...}, "region=emea": {...}}
barca get sales pipeline.py --partition region=emea,amer     # only these keys run and are returned
barca get margin pipeline.py --partition '{"region":"apac"}'           # a JSON object (for compound keys)
```

- When some keys fail, the run fails (exit 1, `failed_node` names the key). The keys that
  succeeded are returned; a failed key has no artifact and runs again on the next `get`.
- Past 1000 keys, without `--partition`, only the keys the run computed are listed (below).

### Many keys

A run lists at most 1000 keys. When a partitioned target returns more and no `--partition`
selected them, `final_output` lists the keys this run computed and counts the ones it served
from cache:

```json
{"_barca_partitions": {
  "keys": 200000, "computed": 1, "cached": 199999, "truncated": false,
  "outputs": {"retailer=r000042": {"_barca_artifact": {"path": "...", "format": "json", "size_bytes": 49}}},
  "hint": "200000 keys, more than 1000: outputs lists only the keys this run computed; ..."}}
```

- `outputs` holds what the 1000-key object would hold for those keys: pointers, values under
  `-o value`, links under `-o url`. A cold run computes every key: `outputs` lists the first
  1000 by key and `truncated` is `true`.
- The table and `-o path` print the computed keys' paths, then a note on stderr
  (`199999 of 200000 keys not listed (199999 cached): ...`).
- `--partition` returns every key it selects, however many: name the keys whose paths you
  need. `barca sql "select * from scored"` reads every current key's rows.
- `barca.get()` in Python raises `BarcaError` for such a target; pass `partition=`.
- The run's outcome under `barca serve` (`GET /runs/{run_id}`, the `run_finished` event, the
  metadata DB) holds the same object, so a tick that recomputed one key of 200k records a few
  kilobytes. Runs recorded before 0.30.2 keep the outcome they stored, every key included.

## Selecting keys

`--partition DIMENSION=VALUE[,VALUE...]` selects partition keys for the whole run, for
backfills, reruns and debugging; a JSON object selects on several dimensions, each a value or a
list of values. It applies to every node the run plans, upstream of the target and downstream:

```bash
barca get margin pipeline.py --partition region=apac                    # sales and margin: apac only
barca get margin pipeline.py --partition region=apac --refresh sales    # recompute apac of both, nothing else
barca get pipeline.py --partition region=emea,amer                      # no target: every asset and sensor
barca get pipeline.py --partition region=emea --dry-run                 # what would run; what is skipped
barca run notify pipeline.py --partition region=emea                    # a task's cone, the same way
barca plan pipeline.py --partition region=emea                          # the narrowed plan
```

- **Narrowed.** Every planned partitioned node with a named dimension computes only the keys
  whose value of that dimension is selected: `partitions_from` chains, `partitions([...])` nodes
  that declare the same dimension name, and partitioned sensors and tasks. With a JSON object a
  node is narrowed by the named dimensions it has. `--refresh sales` with a selection recomputes
  only the selected keys of `sales` (and, by the cascade, of what is below it).
- **The same keys as a full run.** A selected key is computed exactly as without the flag, with
  the same run hash and artifact, so a later run without `--partition` serves it from cache and
  computes only the other keys, and a selection after a full run is all cache hits.
- **Planned as without the flag.** A partitioned node with none of the named dimensions keeps
  all its keys (from cache when fresh): its consumers read all of it. An unpartitioned input of a
  narrowed node runs as usual.
- **Skipped.** A node that reads every key of a narrowed node is not run: computed over the
  selected keys only it would be a different result. That is a `collect()` fan-in, or a
  partitioned node whose dimensions differ from those of a narrowed input. Everything below a
  skipped node is skipped too. Each is a step with status `skipped` (action `skipped` in a dry
  run), reason `partition_selection` and a detail naming the fix (run without `--partition` to
  update it), and stderr prints one note:

  ```
  [barca] note: --partition skips a fan-in that reads every key: summary. Run without --partition to update it.
  ```

  Skipped nodes do not change the exit code: a run whose selected keys all succeeded exits 0. A
  skipped node that is a target, or that `--refresh` names, is a usage error.
- **No target** means the default `get` target set, every asset and sensor, for those keys; the
  return value is that of the last asset planned, as without the flag. `barca run` needs a task:
  the selection narrows its cone, and a task the selection would skip is a usage error.
- **Usage errors (exit 2), before anything runs:** a dimension that no node in the cone has; a
  value that is a key of no narrowed node with that dimension (the error lists a sample of the
  valid values), checked when those keys are known before the run.
- **Keys known late.** The keys of `partitions_from(<a list asset>)` are known only once the list
  asset ran (it runs whole). They are narrowed then; a node with none of the selected values gets
  a warning on stderr, and computes and returns nothing.
- **In JSON**, `get`, `run`, `--dry-run` and `plan` carry `partition_selection`: `selection`
  (dimension to the selected values), `nodes` (each narrowed node with its `selected` and
  `skipped` key counts, `null` while its keys are not known) and `skipped_nodes`. `--server`
  (`RunRequest.partition`) and `barca.get(..., partition=...)`, `barca.run` and `barca.plan` take
  the same selection with the same meaning.
- **Planning cost.** Only the selected keys of a narrowed node are built: on a three-asset chain
  with 200,000 keys, planning 10 keys takes about 70 ms, against about 380 ms (and over 850 MB) for
  every key.
- Up to 0.29.2, `--partition` narrowed only the target: upstream partitioned assets brought every
  key up to date, `--refresh` of an upstream asset recomputed all its keys, and `--partition`
  without a target was a usage error.

`barca plan` uses the same worker-pool size as execution: available cores, overridden
by a positive `BARCA_POOL_SIZE`. Warnings sort by node, parameter and kind. Key previews
list the first twenty keys in lexical order, independently of worker chunking.

See also: `barca docs sinks` (one file per partition, or one Hive-partitioned dataset with a
directory per key), `barca docs examples/partitions`.
