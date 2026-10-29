---
title: 'RFC-0013: Batched Partitions'
description: 'batch= lets one call of a partitioned step compute many keys: N at a time, all of them, or groups you name. Each key keeps its own run hash and artifact, so caching, selection and change detection are the same as one key per call.'
---

- **Status:** Accepted; implemented for 0.32.0
- **Date:** 2026-10-23
- **Touches:** barca-core | barca-cli | python/barca | HTTP server
- **Supersedes / Related:** #643 (this design). Builds on keyed partition sources (#622, 0.29.6), `--partition` across the cone (#614, 0.29.3), lazy inputs (`barca docs big-inputs`), dataset sinks (#621, 0.30.0), early cutoff (#636, 0.31.0) and server-wide capacity (#635, 0.31.0).

---

## 1. Summary

A partitioned step runs once per key today. This RFC adds `batch=`, which groups the keys that
need to run into fewer calls:

```python
@asset(partitions={"retailer": partitions_from(cleaned)}, batch="all")
def scored(retailer: list[str], cleaned: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    return cleaned.filter("amount > 0").aggregate("retailer, sum(amount) AS total")
```

The function always receives the list of keys its call computes. The same code therefore
serves one key, a batch of 20, a named group, or every key. Each key keeps the run hash and
artifact it has without batching. Batching changes how many processes and calls do the work,
and nothing about what is cached or when it reruns.

## 2. Motivation

Users have partitioned pipelines over about 200k keys (retailers, customers), with heavy
DuckDB work. One key per call costs them three ways:

- **Per-call overhead.** Each call is a separate step: scheduled, dispatched to a worker,
  reporting back, with its own DuckDB connection and input reads. At 200k keys that overhead
  dominates.
- **Set-based work written one key at a time.** One query over every retailer is far cheaper
  than 200k filtered queries, but the one-key-per-call model forces the second.
- **Workarounds lose barca's guarantees.** Writing an unpartitioned "all retailers" asset
  gives up per-key caching, `--partition` selection, keyed change detection and per-key
  sinks.

Batching has to keep one function. It must handle one key while debugging, 20 at a time on a
schedule, and everything in a backfill, without a code change.

## 3. Guide-Level Explanation

### Choosing the grouping

| `batch=` | Keys per call |
|---|---|
| omitted / `None` | one (today's behaviour; the default) |
| `20` | at most 20, in key order |
| `"all"` | every key that needs to run, in one call |
| `batches([["r1", "r2"], ["r3", "r4", "r5"]])` | the groups named; these keys always run together |
| `batches_by(region_of)` | keys with the same label (`region_of("r1") == "emea"`) share a call |
| `batches_by(region_of, max_size=20)` | the same, each group split into calls of at most 20 |

A single run can override the grouping without a code change:

```bash
barca get scored --batch all     # a backfill in one call
barca get scored --batch 20
barca get scored --batch none    # one key per call, to find the retailer that fails
```

### Writing a batch function

The partition parameter is a list of the call's keys. Inputs and outputs are keyed:

```python
@asset(partitions={"retailer": partitions_from(cleaned)}, batch=20)
def scored(retailer: list[str], cleaned: dict[str, pd.DataFrame]) -> dict[str, pd.DataFrame]:
    return {r: score(cleaned[r]) for r in retailer}
```

Or set-based, with a lazy input and a table output split by the key column:

```python
@asset(partitions={"retailer": partitions_from(cleaned)}, batch="all")
def scored(retailer: list[str], cleaned: duckdb.DuckDBPyRelation) -> duckdb.DuckDBPyRelation:
    return cleaned.aggregate("retailer, sum(amount) AS total")
```

Each key's rows are sorted by every column, not kept in the order the step produced them:
the same rows in any order give the same artifact bytes, so a `cutoff=True` consumer of an
unchanged key stays cached. Values that compare equal but differ in representation (signed
zero, distinct NaNs, case-insensitive collations) still need normalizing in the step.

### What stays the same

- **Only keys that need to run are passed in.** With a keyed sensor where 3 of 200k
  retailers changed, `batch="all"` makes one call with 3 keys.
- **Caching is per key.** A key computed in a batch is a cache hit for a run without
  batching, and the other way round.
- **Everything that works on keys keeps working per key:** `barca status`, `--partition`,
  `--refresh`, sinks, cutoff and `collect`.

## 4. Reference-Level Explanation

### 4.1 Declaration and validation

- **Where it's allowed:** `batch=` is accepted on partitioned `@asset` and `@task`. On an
  unpartitioned step or on a `@sensor`, it is a usage error (exit 2) when the file is read. A
  keyed source is already one call.
- **Static values:** `None`, a positive int literal and `"all"` are read statically.
- **Group helpers:** `batches(<expr>)` and `batches_by(<expr>, max_size=<int>)` are new helpers
  exported from `barca`, with no-op stubs. A literal list in `batches(...)` is read statically.
  Any other expression, and the function given to `batches_by`, is evaluated at plan time by the
  Python runtime with the project importable, the same way a computed `partitions(<expression>)`
  is.
- **Load errors:** an evaluation error is a load error naming the node (exit 2; under `serve`
  an entry in `/health` `load_errors`).
- **Invalid values:** `batch=0`, a negative number, any other string, an empty group, a key in
  two groups, or a `batches_by` label that is not a string or int, all exit 2.
- **Keys in no group:** a key that appears in no explicit group runs on its own, in a call by
  itself. `batches([...], strict=True)` makes that an error.
- **Annotations:** when the function's partition parameter is annotated, it must be a list or
  sequence type. A scalar annotation such as `retailer: str` with `batch=` set is exit 2,
  naming the fix.
- **Decorator argument table:** `batch` joins `decorator_args.rs` for `@asset` and `@task`, and
  `batches` and `batches_by` join the helper table.

### 4.2 Per-run override

- **Where:** `--batch none|all|N` on `get` and `run`, `RunRequest.batch`, `barca.get(...,
  batch=)` / `barca.run(..., batch=)` and `Schedule(..., batch=)`.
- **What it overrides:** the decorator's `batch=` for every batched-capable node in the run.
- **The `N` form:** it replaces explicit groups with fixed-size chunks.
- **Nodes without `batch=`:** they are unaffected unless the override names them. A future
  extension could take `--batch scored=all`; v1 overrides every node that declares `batch=`.

### 4.3 Planning

- **Grouping:** the planner computes the keys that will run per node (cache-aware, after
  `--partition` and keyed sources) and groups them by the effective `batch`. Each group is one
  step in the plan.
- **Groups with cached keys:** a group whose keys are all cached makes no step. A partly cached
  group runs its stale keys together.
- **Run hashes:** each key's run hash is computed exactly as without batching. `batch` and the
  groups are not part of any run hash; they are scheduling. Changing a function's signature to
  the batch form is a code change, so its keys rerun once.
- **Phases:** a batch step depends on the steps that produce its keys' inputs. When upstream is
  batched too, a downstream batch may start once every key it covers is ready. It does not wait
  for the whole upstream node.

### 4.4 Inputs

| Upstream | Annotation | The function receives |
|---|---|---|
| partitioned, same dimension | `dict[str, T]` or none | key to that key's value |
| partitioned, same dimension | `duckdb.DuckDBPyRelation`, `polars.LazyFrame`, `pyarrow.dataset.Dataset` | one lazy table over the call's keys' artifacts, with the dimension as a column; nothing loaded until queried |
| keyed source (dict-returning) | `dict` or none | key to that key's value |
| unpartitioned | as today | the whole value, once per call |

A lazy table's dimension column is named after the dimension (`retailer`), matching the
Hive layout of dataset sinks.

### 4.5 Outputs

A batch call returns one of:

- **A dict:** key to value. Each value is serialized as that key's artifact, by the existing
  type rules.
- **A table with the dimension as a column:** a pandas, polars or Arrow table, or a DuckDB
  relation. barca splits it by the column into per-key artifacts. DuckDB relations stream
  through `COPY ... PARTITION_BY` into staging. A key with no rows gets an empty table with the
  schema.

Then:

- **Missing key:** a key of the call with no result fails ("batch returned no result for
  retailer=r7").
- **Extra key:** a key outside the call is an error for the whole call.
- **`barca.Failed("reason")`:** a dict value of `barca.Failed` fails that key alone. The other
  keys succeed and are cached.

### 4.6 Execution

- **Calls:** each batch is one call in one worker. Batches of one node run in parallel up to the
  pool and capacity.
- **Weight:** `weight=` applies per call. `batch="all"` with `weight=8` holds 8 for the call.
- **Retries:** `retries=`, `retry_backoff=` and `timeout_seconds=` apply per call. A retry reruns
  only the call's keys that failed (raised as a whole, or returned `barca.Failed`).
- **Fallbacks:** none. A call that raises fails all its keys, and keys are not split and retried
  singly. `--batch none` is the debugging tool.

### 4.7 Observability

- **History:** a batch step is recorded once, as `scored[20 keys]`, with its keys (the display is
  capped and paged as in #632). `barca history RUN_ID` lists the call, and its keys with
  per-key status.
- **Materializations:** per key, as today, with a `batch` id linking the keys of one call.
- **Status and dry run:** per key, as today. The dry run adds the grouping: "3 keys in 1 call
  (batch=all)".
- **Logs:** a call's stdout and stderr are recorded once and attached to each of its keys, so
  `barca logs --step 'scored[retailer=r7]'` finds them.
- **Peak memory:** recorded per call, for tuning `N`.
- **Telemetry:** one span per call, with the key count.

### 4.8 Composition

- **`--partition`:** narrows first, then groups.
- **Keyed sources:** decide which keys run. Batching groups those keys.
- **`cutoff=True`:** stays per key. Each key's artifact bytes are hashed.
- **Sinks:** per key. A dataset sink writes one directory per key.
- **`collect`:** sees every key's artifact, unchanged.
- **Server-wide capacity:** a call is one admission.
- **Server mode:** ledger lookups and posts stay per key.

## 5. Determinism, Caching & Testing

- **Required test:** batching must never change a cached result. For each form (int, `"all"`,
  explicit groups, `batches_by`), compute with batching, then without, and assert every key is
  a cache hit with an identical run hash and bytes. Then run the reverse direction.
- **Interaction tests:** missing and extra keys, `barca.Failed`, retries of failed keys only,
  per-run override precedence, plan-time evaluation errors, `--partition` with groups, keyed
  sources with groups, and server mode.

## 6. Performance

Benchmark a 3-step chain at 10k and 200k keys:

- **Variants:** no batching, `batch=1000`, and `batch="all"`, measuring wall time, peak RSS and
  steps scheduled.
- **One-key change:** with a keyed sensor where one key changed, `"all"` must be one call with
  one key.
- **Planning cost:** grouping must stay linear in the keys that run, not the keys declared.

Record the results in `benchmarks/RESULTS.md`.

## 7. Drawbacks

- **Two ways to write a partitioned step.** The batch form takes a list, the default form a
  scalar. Validation catches mismatched annotations.
- **One call fails as a unit** unless the function uses `barca.Failed`. A bad key in a
  200k-key `"all"` call fails all of them until it is handled.
- **`"all"` has no automatic memory cap.** It is explicit, and `batch=N` is the cap.

## 8. Rationale & Alternatives

- **Not part of the run hash.** If `batch` counted, changing the batch size would rerun every
  key, defeating backfill-vs-schedule overrides.
- **Not a separate decorator.** `@batched_asset` would duplicate every argument.
  `batch=` composes with everything that already exists.
- **No automatic batching.** Batching changes the function's contract (a list, keyed outputs),
  so it must be opt-in.
- **Not unpartitioned assets plus a key column.** That loses per-key caching, selection,
  keyed change detection and per-key sinks.

## 9. Prior Art

Dagster's multi-partition backfills ("single run" backfill policy) run many partitions in one
run but key results by partition range. Spark and DuckDB process sets natively. This design
keeps per-key results while letting the computation be set-based.

## 10. Decided and Unresolved

Decided:

- **Default:** one key per call. Batching is opt-in.
- **Grouping forms:** int, `"all"`, `batches([...])` and `batches_by(fn, max_size=)`, plus a
  per-run `--batch` override.
- **Failing one key:** `barca.Failed` fails a single key in v1.
- **Memory:** no automatic cap on `"all"`.
- **Lazy-table column:** named after the dimension.

- **Override scope:** a bare `--batch V` overrides every node that declares `batch=`, and
  `--batch scored=V` (repeatable) overrides only the nodes named. Nodes without `batch=` are
  never batched by an override, because their functions take a single key.
- **Multi-dimension keys:** `batches_by` receives the full key as a dict of dimension to value
  (`{"retailer": "r1", "month": "2026-10"}`).
- **Key order:** "key order" is the declared order: dimension order, then each dimension's key
  list, and a keyed source's dict order. Grouping is therefore stable from run to run.
- **Table outputs:** the dimension column's values are matched to keys by their string form
  (`1` matches key `"1"`). A value matching no key of the call is an extra-key error.
- **Lazy inputs:** a lazy table input needs tabular upstream artifacts (parquet, CSV, or JSON
  lists of records). Any other format is a step error naming `dict[str, T]` as the fix. Nothing
  is converted silently.

## 11. Future Possibilities

- **Adaptive batch size** from recorded per-call memory and time.
- **Streaming outputs:** yielding `(key, value)` pairs, so a long call commits keys as it goes.
