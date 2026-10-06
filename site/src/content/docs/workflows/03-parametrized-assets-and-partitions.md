---
title: "Workflow: Parametrized Assets and Partitions"
description: How one asset definition is materialized many times with different inputs — partitions as first-class asset coordinates.
---

This document specifies how Barca should handle one asset definition being materialized many times with different inputs.

This is the workflow that should cover:

- "run the same asset 50 times with different inputs"
- embarrassingly parallel fan-out
- partitions as first-class asset coordinates
- both static and asset-derived partition universes

This workflow assumes the Barca core constraints documented in [Core Constraints](/core-constraints/).

## The concrete problem

We want to support both of these styles.

## Style 1: partitions from an iterable

```python
from barca import asset, partitions


@asset(partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])})
def fetch_prices(ticker: str) -> dict[str, str]:
    return {"ticker": ticker}
```

## Style 2: partitions derived from an asset

```python
from barca import asset, partitions_from


@asset()
def tickers() -> list[str]:
    return ["AAPL", "MSFT", "GOOG"]


@asset(partitions={"ticker": partitions_from(tickers)})
def fetch_prices(ticker: str) -> dict[str, str]:
    return {"ticker": ticker}
```

In both cases, Barca should understand that this is not one materialization but many:

- `fetch_prices[ticker=AAPL]`
- `fetch_prices[ticker=MSFT]`
- `fetch_prices[ticker=GOOG]`

Those should be independent units for:

- execution
- caching
- provenance
- parallelization

## Recommended API

For the MVP, Barca should support this decorator shape:

```python
@asset(
    name: str | None = None,
    inputs: dict[str, AssetRefLike] | None = None,
    partitions: dict[str, PartitionSpecLike] | None = None,
    serializer: SerializerKind | None = None,
    freshness: Freshness = Always,
    description: str | None = None,
    tags: dict[str, str] | None = None,
)
```

Where:

```python
AssetRefLike = AssetRef | Callable
PartitionSpecLike = Partitions | PartitionsFrom
```

Both partition declaration forms should be supported.

### Direct iterable partitions

```python
from barca import asset, partitions


@asset(partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])})
def fetch_prices(ticker: str) -> dict[str, str]:
    return {"ticker": ticker}
```

`partitions(...)` should accept an iterable of partition values:

```python
Partitions = Iterable[JsonScalar] | Iterable[dict[str, JsonScalar]]
```

### Asset-derived partitions

```python
from barca import asset, partitions_from


@asset()
def tickers() -> list[str]:
    return ["AAPL", "MSFT", "GOOG"]


@asset(partitions={"ticker": partitions_from(tickers)})
def fetch_prices(ticker: str) -> dict[str, str]:
    return {"ticker": ticker}
```

`partitions_from(...)` should accept the same ergonomic sugar as `inputs`:

```python
PartitionsFrom = AssetRef | Callable
```

## Internal model

These are two user-facing ways to describe the same internal concept.

Barca should canonicalize both into a partition source model:

- `partitions([...])` becomes an inline partition source embedded in asset metadata
- `partitions_from(tickers)` becomes an upstream asset-backed partition source

The executor and cache model should not care which syntax the user chose after indexing.

## Why `partitions` is the right concept

This should not be modeled as "parallelism config".

It should be modeled as "one logical asset definition expands into many partitioned asset instances, where the partition universe is itself data."

That distinction matters:

- partitions affect identity
- partitions affect caching
- partitions affect provenance
- parallelism is just an execution consequence of independent partitions

If Barca gets this right, users get parallelism without touching threads or multiprocessing.

It also means users can drive orchestration from upstream data rather than from framework config.

## What a partition means

For Barca, a partition is a named coordinate on an asset definition.

In the asset-derived example:

- asset definition: `fetch_prices`
- partition dimension: `ticker`
- partition source asset: `tickers`
- partition values at planning time: `AAPL`, `MSFT`, `GOOG`

In the direct iterable example:

- asset definition: `fetch_prices`
- partition dimension: `ticker`
- partition source kind: inline iterable
- partition values at indexing time: `AAPL`, `MSFT`, `GOOG`

The logical asset is still one thing:

```text
my_project/assets.py:fetch_prices
```

But materialization addresses are partition-specific:

```text
my_project/assets.py:fetch_prices[ticker=AAPL]
my_project/assets.py:fetch_prices[ticker=MSFT]
my_project/assets.py:fetch_prices[ticker=GOOG]
```

## Why this is better than a separate `@map` or `@parallel` API

A tempting alternative is something like:

```python
map_asset(fetch_prices, ticker=["AAPL", "MSFT", "GOOG"])
```

That is attractive operationally, but it is the wrong center of gravity for Barca because:

- it moves core asset identity into a job DSL
- it weakens direct inspectability
- it makes partition provenance less obvious
- it risks turning "run 50 times" into a separate orchestration model

Barca should instead say:

- partitioning is asset metadata
- partition values can come from inline iterables or upstream assets
- execution engine decides how much parallelism to use

## The user-facing behavior

The function is still just a normal Python function:

```python
fetch_prices("AAPL")
# {"ticker": "AAPL"}
```

Barca adds:

- the ability to enumerate all declared partitions
- the ability to materialize one partition or many
- automatic parallel execution of independent partitions

Example helper usage (proposed — `materialize()`/`list_partitions()` are not part of the shipped Python API; see [Tasks and Workflow Management](/workflows/10-tasks-and-workflow-management/) for the real `barca get`/`barca run` interface):

```python
from my_project.assets import fetch_prices
from barca import materialize, list_partitions

list_partitions(fetch_prices)
# [{"ticker": "AAPL"}, {"ticker": "MSFT"}, {"ticker": "GOOG"}]

materialize(fetch_prices, partition={"ticker": "AAPL"})
materialize(fetch_prices)
```

The default `materialize(fetch_prices)` behavior for a partitioned asset should mean:

- resolve the current partition universe from its partition source
- materialize all resolved partitions

Today, `barca get fetch_prices pipeline.py` (or `barca.api.get("fetch_prices", "pipeline.py")`) does the "materialize all resolved partitions" half of this — there is no CLI or API flag yet to target a single partition the way `partition={"ticker": "AAPL"}` implies.

## Parallelization model

Parallelization should not be configured with threads or multiprocessing APIs in user code.

The right model is:

- each partition materialization is an independent runnable unit
- Barca schedules those units onto worker processes
- the executor applies a global or per-job concurrency limit

For this example, if there are 50 partitions, Barca should be able to run many of them concurrently with no special user code.

The user should be able to provide only coarse execution hints, such as:

```python
@asset(
    partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])},
    tags={"concurrency_group": "network"}
)
def fetch_prices(ticker: str) -> dict[str, str]:
    ...
```

Or later:

```python
@job(max_concurrency=8)
```

But the asset API itself should not expose threading or multiprocessing knobs.

## Identity model

This workflow requires a three-level identity model.

### Logical asset identity

The stable user-facing asset:

```text
my_project/assets.py:fetch_prices
```

### Definition identity

The code/config definition:

```text
definition_hash = hash(module_source + decorator_metadata + serializer + uv.lock + ...)
```

### Partitioned run identity

Each partitioned materialization must include the partition key in identity:

```text
run_hash = hash(definition_hash + partition_key + runtime_params + upstream_materialization_ids)
```

For this simple example with no upstream dependencies:

```text
run_hash = hash(definition_hash + {"ticker": "AAPL"} + partition_source_identity)
```

That means:

- changing code invalidates all partitions under that definition
- changing the partition value creates a different materialization
- changing the partition source can change the partition universe
- one partition can be reused while another is recomputed

Where `partition_source_identity` means:

- for `partitions([...])`: a hash of the normalized iterable specification
- for `partitions_from(...)`: the chosen partition source materialization ID

## Filesystem layout

For a partitioned asset, the layout should make partitions explicit.

Recommended shape:

```text
.barcafiles/
  my-project-assets-py-fetch-prices/
    <definition-hash>/
      partitions/
        ticker=AAPL/
          code.txt
          metadata.json
          value.json
        ticker=MSFT/
          code.txt
          metadata.json
          value.json
        ticker=GOOG/
          code.txt
          metadata.json
          value.json
```

This is slightly redundant because `code.txt` is repeated per partition, but it keeps each partition self-describing and easy to inspect.

If that duplication becomes too expensive later, Barca can move shared definition files one level up.

## Turso records

This workflow needs explicit partition metadata.

### `asset_partitions`

- `definition_id`
- `partition_name`
- `partition_source_kind`
- `partition_source_ref`

### `materializations`

Add:

- `partition_key_json`
- `partition_key_hash`
- `partition_source_materialization_id`

This lets Barca answer:

- which partitions exist for an asset definition
- which partitions are stale
- which partitions already have successful materializations

## Materialization flow

When materializing all partitions for `fetch_prices`:

1. Resolve the logical asset.
2. Load the indexed definition.
3. Recompute `definition_hash` as a preflight consistency check.
4. Resolve the current partition universe:
   - for `partitions([...])`, read the normalized inline iterable from metadata
   - for `partitions_from(...)`, resolve and materialize the partition source asset, then read and validate its output
5. Enumerate partition keys from the resolved universe.
6. For each partition key:
   - compute `run_hash`
   - check for an existing successful materialization
   - if missing or stale, enqueue a runnable unit
7. Execute runnable units with Barca-managed concurrency.
8. For each unit:
   - launch `uv run`
   - import the real user module
   - call `fetch_prices(ticker=<partition value>)`
   - serialize the output
   - publish the artifact
   - record the partition-specific materialization in Turso

The important extra point is that partition resolution is part of planning, not just execution.

That is true for both styles, but only `partitions_from(...)` introduces a planning dependency on another asset.

## Planner dependency semantics

For `partitions_from(...)`, the partition source asset is not just another runtime input.

It is a planning dependency.

That means Barca may need to materialize `tickers` before it can even know which runnable units exist for `fetch_prices`.

This is a real distinction:

- normal inputs are needed to execute one already-known step
- partition sources are needed to discover the step set itself

Barca should represent that explicitly in metadata and UI.

## What happens when partitions disappear

If `tickers` used to return:

```python
["AAPL", "MSFT", "GOOG"]
```

and later returns:

```python
["AAPL", "MSFT"]
```

Barca should not delete the old `GOOG` materializations.

Instead:

- `GOOG` remains in historical provenance
- `GOOG` is no longer in the current partition universe for future full runs
- UI/TUI can show it as historical or inactive for the current partition source materialization

That preserves auditability and avoids destructive cache behavior.

This is consistent with the broader Barca rule that historical definitions and materializations are append-only and should not be deleted during normal operation.

## Dynamic partition resolution

For `partitions_from(...)`, the partition set is resolved lazily at refresh/run time, not at index time. The partition-defining asset must be materialised before the partitioned asset can determine its partitions. Until then:

- `barca status` and `barca get --dry-run` report the partitioned asset as `unknown` (its partition keys are not known yet)
- the partitioned asset cannot be materialised until the partition-defining asset has a successful materialization

## collect(asset)

`collect(asset)` aggregates all partitions of a partitioned asset into a single list, allowing a non-partitioned downstream asset to consume all partition outputs at once. The planner schedules a `collect()`-consuming asset into a phase gated on the completion of every partition of its upstream (a `FanIn` phase) — it never runs before its inputs exist.

```python
from barca import asset, partitions_from, collect

@asset()
def tickers() -> list[str]:
    return ["AAPL", "MSFT", "GOOG"]

@asset(partitions={"ticker": partitions_from(tickers)})
def fetch_prices(ticker: str) -> dict[str, str]:
    return {"ticker": ticker, "price": str(len(ticker) * 100)}

@asset(inputs={"reports": collect(fetch_prices)})
def aggregate(reports: list[dict[str, str]]) -> dict:
    return {"total": len(reports)}
```

Output type: `list[OutputType]` — one entry per partition, order not guaranteed to match declaration order. `collect()` does not expose partition keys to the downstream asset directly; if you need the key alongside the value, have the partitioned asset include it in its own return value (as `fetch_prices` does above by returning `{"ticker": ticker, ...}`).

If any partition has failed, `collect` blocks entirely — the downstream asset does not run until all partitions succeed.

## Recommended helper APIs

This workflow implies a few useful helpers — proposed, not shipped. None of `list_partitions()`, `materialize()`, or `read_asset()` exist in `barca`'s Python package today; the shipped equivalent is the CLI/`barca.api` surface (`get`, `run`, `plan`, `history`, `stats`) described in [Tasks and Workflow Management](/workflows/10-tasks-and-workflow-management/):

```python
list_partitions(fetch_prices)
materialize(fetch_prices, partition={"ticker": "AAPL"})
materialize(fetch_prices)
read_asset(fetch_prices, partition={"ticker": "AAPL"})
```

For the MVP, that is enough.

Do not add a separate "parallel map" user API yet.

`list_partitions(fetch_prices)` should resolve the current partition source and return the current partition universe, not a static decorator literal.

## Downstream partitioned dependencies

The natural next case is:

```python
@asset()
def tickers() -> list[str]:
    return ["AAPL", "MSFT", "GOOG"]


@asset(partitions={"ticker": partitions_from(tickers)})
def fetch_prices(ticker: str) -> dict[str, str]:
    return {"ticker": ticker}


@asset(
    inputs={"price_blob": fetch_prices},
    partitions={"ticker": partitions_from(tickers)}
)
def normalize_prices(price_blob: dict[str, str], ticker: str) -> dict[str, str]:
    return {"ticker": ticker, "normalized": price_blob["ticker"].lower()}
```

This is correct but slightly repetitive.

For the MVP, that repetition is acceptable because it is explicit.

Barca can validate that:

- both assets declare the same partition dimension
- both assets derive that dimension from compatible partition sources
- the downstream partition key selects the matching upstream partition

Later, Barca can add ergonomic sugar for inherited partitions. It should not start there.

## Why not auto-inherit partitions immediately

It is tempting to let Barca infer:

- that `normalize_prices` should inherit `ticker`
- that `price_blob` should resolve to the matching upstream partition

That magic is appealing, but it is risky for the first implementation because:

- it hides identity rules
- it makes multi-input partition alignment ambiguous
- it creates more special cases in `load_inputs()`

The MVP should prefer explicit partition declaration and explicit validation.

## `load_inputs()` behavior for partitioned assets

Proposed — `load_inputs()`/`load_call()` are not part of the shipped Python API (see the note under [Recommended helper APIs](#recommended-helper-apis) above).

For a partitioned downstream asset:

```python
load_inputs(normalize_prices, partition={"ticker": "AAPL"})
```

should return:

```python
{
  "price_blob": {"ticker": "AAPL"}
}
```

It should not automatically add the partition value as a hidden argument.

If the function wants the partition value, it should accept it explicitly:

```python
def normalize_prices(price_blob: dict[str, str], ticker: str) -> dict[str, str]:
    ...
```

And a broader helper can later exist:

```python
load_call(normalize_prices, partition={"ticker": "AAPL"})
```

which could return all call kwargs including partition-bound parameters.

For the MVP, `materialize(...)` can build those full call kwargs internally without exposing that extra API yet.

## Critical tradeoffs and holes

### Partition values are not the same as arbitrary runtime parameters

Partitions should be durable, enumerable, and identity-bearing.

If users want one-off runtime args, that should be a separate concept later.

Do not collapse "partitions" and "ad hoc params" into one decorator field in the MVP.

### Partition-source outputs need a constrained shape

If partitions come from assets, Barca needs a narrow contract for what that asset can return.

For the MVP, a partition source asset should return one of:

- `list[str]` for a single partition dimension
- `list[int]` for a single partition dimension
- `list[dict[str, JsonScalar]]` for explicit multi-dimension partition keys

Barca should reject anything else with a clear validation error.

That keeps partition planning deterministic and easy to inspect.

### Derived partitions create lifecycle questions

Deriving partitions from upstream data is the right model, but it creates lifecycle questions:

- who defines the partition universe
- when does it change
- how do downstream assets react
- how is staleness computed

For `partitions_from(...)`, the MVP answer should be:

- the partition source asset defines the universe
- the universe changes when that asset's materialization changes
- downstream partitioned assets use the current successful partition source materialization when planning
- stale and new partition units are computed by diffing the old and new partition universes
- removed partition keys become historical, not deleted

For `partitions([...])`, the universe changes when the decorator metadata changes, which means the asset definition changes.

### Automatic parallelization needs backpressure

"Run 50 partitions" is easy to say and easy to abuse.

Barca should treat partition fan-out as schedulable work with concurrency limits, not as a fire-and-forget process explosion.

### Partitioned assets are not 50 different logical assets

They are one logical asset definition with 50 partitioned materializations.

That distinction should be preserved in the UI, CLI, and metadata model.

## Recommended implementation stance

For the MVP:

- add `partitions={...}` to `@asset`
- support `partitions(iterable)`
- support `partitions_from(...)`
- treat each partition key as an independent runnable unit
- key cache reuse by `definition_hash + partition_key + partition_source_identity + upstream materialization IDs`
- keep parallelism in the executor, not in user code
- require explicit partition declarations on downstream assets
- treat partition resolution as a planning step backed by an upstream asset
- defer partition inheritance sugar

This is the narrowest design that still gives users a genuinely useful parallelization primitive.

## Acceptance criteria

- A user can declare a partitioned asset with `partitions(iterable)`.
- A user can declare a partitioned asset with `partitions_from(...)`.
- Barca indexes one logical asset definition and the appropriate partition source metadata.
- Targeting a single partition (`materialize(fetch_prices, partition={"ticker": "AAPL"})` or equivalent) runs only one partition. Not yet available — `barca get fetch_prices pipeline.py` runs all partitions today.
- `barca get fetch_prices pipeline.py` (or `materialize(fetch_prices)`) runs all partitions.
- Independent partitions can execute concurrently with Barca-managed concurrency.
- Successful materializations are cached per partition.
- Changing code invalidates all partitions for that asset definition.
- Changing the inline iterable changes the partition universe for future runs.
- Changing the partition source asset updates the partition universe for future runs.
- Changing one partition value does not invalidate or overwrite the others.
