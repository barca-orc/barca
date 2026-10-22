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
  (a list comprehension, a function call) is evaluated by the Python runtime at plan time.
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
- `partitions_from(keys)` on an *unpartitioned* asset that returns a list uses the list's values
  as keys. They are only known once `keys` has run (a dry run reports the asset as `unknown`
  until then), and the list itself is not passed to the function.
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

See also: `barca docs sinks` (one file per partition), `barca docs examples/partitions`.
