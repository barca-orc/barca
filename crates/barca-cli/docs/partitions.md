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
- The partition key is passed to the function as the parameter named in `partitions={...}`.
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
- A fan-in (`collect`) runs in its own phase after every partition has finished.

See also: `barca docs sinks` (one file per partition), `barca docs examples/partitions`.
