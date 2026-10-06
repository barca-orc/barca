---
title: 'RFC-0003: Decorator Surface & Python API'
description: '@asset/@sensor/@task/@sink, partitions, parallel(), and the barca.api programmatic surface.'
---

- **Status:** Accepted (retroactive baseline — documents behavior as of v0.6.1)
- **Current behavior:** this page is a historical baseline and some details have changed since v0.6.1. For what barca does now, read the [Decorators API](/reference/api/decorators/) and the [Python API](/reference/api/python/).
- **Date:** 2026-07-16
- **Touches:** python/barca | barca-core
- **Supersedes / Related:** [RFC-0001](/rfcs/0001-node-kinds-and-freshness/) (node kind/freshness kwargs), [RFC-0005](/rfcs/0005-artifact-serialization-and-storage/) (serializer selection)

---

## 1. Summary

`python/barca` ships two distinct surfaces in one package: **decorator stubs**
(`@asset`, `@sensor`, `@task`, `@sink`, `@unsafe`, `parallel`/`parallel_map`) that are
pure no-ops read statically by the Rust binary, and a **programmatic API**
(`barca.get`/`run`/`plan`/`history`/`stats` in `api.py`) that shells out to the `barca`
binary for one-shot scripted use. Neither talks to a running server — that's
`barca.Client`, covered by [RFC-0004](/rfcs/0004-http-server-api/).

## 2. Motivation

Users write plain Python functions; barca needs a way for them to declare intent
(what's an asset vs. a side-effecting task, what depends on what, how to partition
work) that (a) reads naturally as Python, (b) is statically extractable by
`ruff_python_parser` without importing the module, and (c) still runs correctly if the
user executes the file directly outside of barca (e.g. `python pipeline.py` in a test).
A no-op decorator stub achieves all three: it's real Python, AST-visible, and
identity-behaved at runtime.

## 3. Guide-Level Explanation

### 3.2 Python API

```python
import barca

value = barca.get("summary", "pipeline.py")           # cache-aware
result = barca.run("deploy", "pipeline.py", refresh=["fetch"])
plan = barca.plan("pipeline.py")                        # {"total_steps": ..., "phases": [...]}
runs = barca.history(limit=25)
s = barca.stats("summary", "pipeline.py")
```

Each of these shells out to the `barca` binary (see
[RFC-0002](/rfcs/0002-cli-surface/)) and deserializes the result — `get`/`run` return
the target's deserialized value directly, not the wrapping JSON envelope.

### 3.3 Decorator surface

```python
from barca import asset, sensor, task, sink, unsafe, partitions, partitions_from, collect, asset_ref
from functools import partial
from barca import parallel, parallel_map

@asset(partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])})
def price(ticker: str) -> dict:
    return fetch_price(ticker)

@asset(inputs={"prices": collect(price)})
@sink('./summary.json')
def summary(prices: list[dict]) -> dict:
    return aggregate(prices)

@sensor(freshness=Schedule("*/5 * * * *"))
def inbox_files() -> tuple[bool, list[str]]:
    files = list(Path("inbox").glob("*.csv"))
    return len(files) > 0, [str(f) for f in files]

@task()
def deploy_us(model) -> str: ...

@task()
def deploy_eu(model) -> str: ...

@task()
def deploy_all(model) -> None:
    results = parallel(partial(deploy_us, model), partial(deploy_eu, model))

@unsafe
def reads_globals() -> str:
    return CONFIG["value"]   # not AST-trackable; opt out of purity warnings
```

## 4. Reference-Level Explanation

### 4.1 Public API Surface

**Decorator kwargs** (full signatures live in
[Decorators API](/reference/api/decorators/); this RFC is the design record):

| Decorator | Kind | Key kwargs |
|---|---|---|
| `@asset` | asset | `name`, `inputs`, `partitions`, `serializer`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags` |
| `@sensor` | sensor | `name`, `freshness` (Manual/Schedule only), `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags` |
| `@task` | task | `name`, `inputs`, `freshness`, `timeout_seconds`, `retries`, `retry_backoff`, `description`, `tags` |
| `@sink` | (stacks on `@asset`) | `path`, `serializer` |
| `@unsafe` | (marker, any kind) | none |

`retries` is total attempts (1 = no retry); `retry_backoff` is the linear per-attempt
delay in seconds (`retry_backoff * attempt`). Both are read statically and owned by the
Rust retry loop — the Python stub never executes a retry itself.

**Partition/reference helpers** (used inside `inputs=`/`partitions=`):
`partitions(values)`, `partitions_from(source)`, `collect(source)`, `asset_ref(canonical_name)`.
`partitions(...)` accepts either a literal list (extracted statically) or an arbitrary
expression evaluated by the Python runtime at plan time — the one place in this surface
that is *not* purely static (see §4.3).

**`parallel`/`parallel_map`** — only recognized inside `@task` bodies (not `@asset`
bodies). `parallel(*callables)` takes `functools.partial`-wrapped calls to other
`@task`-decorated functions; static analysis resolves `partial(fn, ...)` arguments
(including inside a starred generator/comprehension) to build the dependency graph, but
fully dynamic call sets (`parallel(*work_items)`) are only resolvable at runtime. A
failed branch is returned as `ParallelError` (`.error` holds the message), not raised.

**`barca.api` functions** — `get`, `run`, `plan`, `history`, `stats` — signatures and
return shapes are in [RFC-0002](/rfcs/0002-cli-surface/) §4.1 (they mirror the CLI's
JSON/table output 1:1). `BarcaError` is the one exception type raised across this
surface.

**Exports** — everything in `python/barca/__init__.py`'s `__all__` is the supported
surface: decorators, freshness markers, partition helpers, `parallel`/`parallel_map`/
`ParallelError`, the five `api.py` functions, `BarcaError`, and `Client`/`Run` (re-exported
from `client.py`, see [RFC-0004](/rfcs/0004-http-server-api/)).

### 4.2 Implementation Details

Every decorator in `python/barca/__init__.py` is an identity function or identity
decorator factory — see the file directly, it's short. `api.py`'s `_find_binary()` /
`_exec()` mechanics (binary discovery, version-mismatch warning, stdout-JSON parsing)
are incidental; see [RFC-0002](/rfcs/0002-cli-surface/) §4.1 for the contract they must
honor. `_worker.py` execution mechanics are out of scope for this RFC — that's
`barca-core`'s dispatch/protocol layer, covered in [Architecture](/architecture/).

### 4.3 Rust ↔ Python Boundary

This is the RFC where the "never import user code during planning" invariant is most
visible at the decorator level:

- Decorator kwargs, `inputs=`, and node kind/freshness are extracted by
  `ruff_python_parser` AST inspection in `crates/barca-core/src/parse.rs` — **no
  import**.
- `partitions(...)` is the one documented exception: a literal list is extracted
  statically, but an arbitrary expression (list comprehension, function call) is
  deferred to a **Python runtime evaluation at plan time** — this is a real, narrow
  import of user code, scoped to evaluating that one expression, not executing the
  asset body.
- `parallel()`'s dynamic call sets (`parallel(*work_items)`) are unresolvable
  statically for the same reason — the dependency graph for those branches doesn't
  exist until the task body actually runs, at which point `parallel()` talks to the
  coordinator over the worker's Unix domain socket (`python/barca/_runtime.py`) rather
  than through any static path.
- `@unsafe` exists precisely because AST analysis cannot always determine purity
  (global reads, I/O); it's a documented escape hatch, not a bypass of the static
  invariant — barca still never imports the function to classify it.

### 4.4 Node-Kind Semantics

`@asset`/`@sensor`/`@task` map 1:1 to the three node kinds from
[RFC-0001](/rfcs/0001-node-kinds-and-freshness/); `@sink` is not a node kind, it's a
leaf output stacked on `@asset` (§4.5). The cache-poisoning guard (tasks can't feed
assets/sensors) is enforced regardless of how `inputs=` is spelled.

### 4.5 Edge Cases

- `@sink` is a leaf — no other node may take a sink as an input. Multiple `@sink`
  decorators may stack on one `@asset`. A sink failure never fails the parent asset;
  it's logged (`[barca] SINK FAILED: ...`) and recorded in run metadata.
- For partitioned assets, each partition writes its own sink file with the partition
  key injected before the extension.
- Sensors are source nodes only — no `inputs=` kwarg is meaningful on `@sensor`.
- Ordering-only dependencies use the `_`-prefix convention on a task's `inputs=` key
  (`inputs={"_dep": some_node}`) — barca skips artifact deserialization and the
  parameter receives `None`.

## 5. Determinism, Caching & Testing

Decorator kwargs (`inputs`, `partitions`, source code) feed `definition_hash`/`run_hash`
computation — see [RFC-0005](/rfcs/0005-artifact-serialization-and-storage/) and
[Core Constraints](/core-constraints/). `retries`/`retry_backoff` do not affect the
cache key (a retried-then-successful run caches identically to a first-try success).
Covered by `crates/barca-core/src/parse.rs` unit tests (decorator/kwarg extraction) and
`python/tests/` (stub identity behavior, `api.py` subprocess/parsing behavior).

## 6. Performance

Decorator *parsing* is on the hot path (part of the AST-extraction phase measured in
`benchmarks/trivial`); decorator *execution* (the no-op call itself) is not — it's
Python-side and negligible next to the function body it wraps. `partitions(...)`'s
runtime-evaluation fallback (§4.3) adds a real Python startup cost when triggered — it
is the one place this surface can regress the "invisible" hot path, and any change here
should be checked against `benchmarks/trivial` and a partitioned scenario.

## 7. Drawbacks

Two extraction paths (static AST vs. runtime-evaluated `partitions(...)`) is more
surface than a purely static design, and it's easy for a contributor to accidentally
widen the runtime-eval escape hatch to other kwargs without realizing it breaks the
no-import invariant elsewhere.

## 8. Rationale & Alternatives

An import-based decorator (rejected) — where `@asset(...)` actually registers the
function in a global registry at import time, as Dagster's does — was rejected because
it requires importing user code during planning, which breaks the invisibility/safety
goal (import side effects, missing dependencies, slow imports all become the planner's
problem). No-op stubs plus static AST extraction keeps planning side-effect-free.

## 9. Prior Art

Dagster's `@asset`/`@op` decorators (import-based registration) and Prefect's
`@flow`/`@task` (also import-based) are the direct comparison point — see
[Framework Comparison](/comparisons/framework-comparison/),
[vs. Dagster](/comparisons/dagster/).

## 10. Unresolved Questions

Should `partitions(...)`'s runtime-eval fallback be extended to other kwargs (e.g. a
dynamic `inputs=` mapping), or is the static/dynamic split intentionally frozen at its
current boundary?

## 11. Future Possibilities

Widening `parallel()`'s static resolution to more dynamic call-set shapes (today only
`partial(fn, ...)` literals and comprehensions over them are AST-resolvable) would let
more workloads get dependency-graph visibility (`barca plan`) without executing.
