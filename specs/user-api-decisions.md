# User API Decisions

Why the barca user API is shaped the way it is. Each section covers a decision,
the alternatives we considered with concrete syntax, and the reasoning.

## Assets vs tasks: two concepts, not one

**Decision:** Assets and tasks are distinct node types with different decorators
(`@asset` vs `@task`), different commands (`get` vs `run`), different caching
behavior.

**Alternatives rejected:**

```python
# A: Unified @step with explicit caching flag
@step(cache=True)
def raw_data() -> dict: ...

@step(cache=False)
def deploy(model) -> dict: ...
```

```python
# B: @node with kind parameter
@node(kind="data")
def raw_data() -> dict: ...

@node(kind="action")
def deploy(model) -> dict: ...
```

```python
# C: No decorator, infer from usage
def raw_data() -> dict:     # returns data = asset
    return load()

def deploy(model) -> None:  # void return = task
    push(model)
```

**Why we rejected each:**

- **C (no decorator):** Without decorators, the entire codebase becomes one huge
  DAG. Every function is a candidate for analysis. Decorators are opt-in markers
  that scope what's in the DAG — without them, barca does far more work than
  necessary and the user has no control over what's orchestrated.

- **B (@node with kind=):** Strings as parameters give you no autocomplete and
  no guidance. More fundamentally, using a generic `@node` for everything takes
  away any guidance on how to think about what you're building. We want to make
  it easy for people to write really good data pipelines, not just generic
  graphs.

- **A (@step with cache flag):** This leaks an implementation detail into the
  API. Users shouldn't think about caching at all. The user declares *what kind
  of thing this is* (data vs action), and barca handles the rest — caching,
  staleness, hashing, invalidation. Making the user choose `cache=True/False`
  on every function is a tax. With `@asset` and `@task`, caching is a
  consequence of the type, not a decision the user makes.

**The deeper reason:** Assets and tasks serve different user mindsets. When
you're thinking about data, you want guarantees around staleness, speed, and
parallelization — the asset model is opinionated and handles state for you so
you can focus on your work. When you're thinking about tasks, the model is
simpler — you're doing things, maybe in parallel, maybe in sequence. The task
model deliberately doesn't bring the baggage and opinions of the asset model.

Separate decorators also serve as guardrails — they teach users the right
mental model. An asset is something like `plants_table` — clearly an object
you can "get." A task is something like `send_slack_notification` — clearly
something you "do." The API aligns with how people naturally name their
functions.

## Tasks can't be inputs to assets (status: rejected today; softening is not planned)

**Decision (current):** DAG validation rejects edges from tasks to assets or
sensors.

**Status (2026-10-10):** still a hard error, and there is no open issue to change
it. The target behavior below records the intent in case one is filed.

**Target behavior:** Allow it with a warning. The downstream asset becomes
uncacheable — it re-runs every time because its task input always re-runs.

```python
# Currently: hard error
# Future: warning + uncacheable asset
@task()
def sync_gcs(): ...

@asset(inputs={"sync": sync_gcs})
def report(sync): ...
# WARNING: 'report' depends on task 'sync_gcs' and will re-run every time
# (uncacheable). Consider using a sensor instead.
```

**Why the guardrail exists:** It pushes users toward better patterns. The
typical case is "do some side-effect, then generate data" — like syncing from
GCS to AWS, then building an asset from the AWS data. But this couples the
side-effect to the data pipeline. The better pattern: run the sync task
independently, add a sensor to AWS that detects new data, and have the asset
depend on the sensor. This preserves caching and decouples the workflows.

**Why we'll soften it:** There are valid cases where you want "always do X
before generating Y." A hard error is too aggressive. A warning + uncacheable
communicates the tradeoff without blocking the user.

## Decorators are no-ops

**Decision:** `@asset()`, `@sensor()`, `@task()` are identity functions. They
return the decorated function unchanged. All metadata is extracted by Rust via
static analysis.

**Alternative rejected:** Dagster's approach — decorators wrap the function
and modify its behavior (adding context objects, I/O managers, etc.).

**Why:** Users should be able to import their functions and use them like any
regular Python code. Dagster's wrapping makes it hard to work in a Jupyter
notebook — you can't just call the function with normal arguments. It makes
unit testing harder (you need fixtures or mocks). And from an orchestration
standpoint, it isn't necessary — the Rust binary extracts everything it needs
from the source text.

The orchestrator observes your code — it doesn't become part of it. There's
room for lightweight instrumentation later (Datadog/Sentry tracing), but
decorators should stay thin.

## Static analysis, never import

**Decision:** Barca parses Python source files using ruff's AST parser in Rust.
It never imports user modules during planning.

**Alternative rejected:** Import the module, inspect the decorated objects,
build the graph from runtime metadata. This is what Dagster, Prefect, and
Airflow do.

**Why:** Speed was the primary motivation. Importing Python modules is slow —
30ms+ per import with dependencies. AST parsing in Rust takes <5ms for 2000
assets. The planning phase must be invisible (sub-100ms).

Beyond speed: aside from complex metaprogramming use cases, static analysis
gets the job done. If you're writing clean and relatively simple data
pipelines, the parser handles it.

**What we give up:** Dynamically generated decorators, conditional decorator
application, decorators in loops. These aren't visible to static analysis.
In practice this isn't a real limitation for the target use case.

## `inputs=` with function references

**Decision:** Dependencies are declared via `inputs={"param": upstream_function}`,
where the value is a Python function reference.

**Alternatives rejected:**

```python
# A: String references
@asset(inputs={"data": "raw_data"})
def summary(data): ...
# No autocomplete, typos are silent, no cycle awareness
```

```python
# B: Implicit by parameter name (Dagster's default)
@asset()
def summary(raw_data): ...
# Parameter name auto-resolves to the asset named raw_data
```

```python
# C: Separate graph definition
# pipeline.py
@asset()
def raw_data(): ...
@asset()
def summary(data): ...

# graph.py
pipeline = Graph({summary: {"data": raw_data}})
```

**Why we rejected each:**

- **A (strings):** No autocomplete, no go-to-definition, no rename refactoring.
  Typos fail silently at plan time. Imports force users to think about cycles,
  which is a fundamental part of DAG construction — strings bypass that.

- **B (implicit by param name):** This is how Dagster does it and it's a little
  nice syntactically until you realize you're confounding a Python function with
  the thing that it returns. `def summary(raw_data)` looks like the parameter
  is a function, but it actually receives a dict. In a Jupyter notebook, you'd
  have to call the function, rename the result, and pass that in. It's messy.
  The explicit form `inputs={"data": raw_data}` separates the wiring (raw_data
  produces it) from the interface (the param is called "data" and receives a dict).

- **C (separate graph):** Having the graph topology in one place is nice — but
  as an *output*, not an *input*. When you're designing pipelines, you think
  about nodes in terms of their inputs and outputs, not the global topology.
  `barca plan` generates the graph as an artifact you can study. If you define
  the graph separately, it can still have errors — nodes and edges can drift
  apart. With `inputs=` on the function, you're guaranteed a valid graph that
  you can then study and iterate on.

**Why function references:** They're clean, require the upstream to actually
exist (ImportError if not), force users to think about cycles through Python's
import system, and give you full IDE support for free.

## `barca get` vs `barca run`: two commands

**Decision:** Assets are materialized with `barca get`, tasks are executed with
`barca run`. Using the wrong command gives a clear error.

**Alternatives rejected:**

```bash
# A: Single command, system infers behavior
barca execute pipeline.py
barca execute summary pipeline.py     # figures out it's an asset
barca execute deploy pipeline.py      # figures out it's a task
```

```bash
# B: Noun-verb pattern
barca asset get summary pipeline.py
barca task run deploy pipeline.py
barca sensor check temp pipeline.py
```

**Why we rejected each:**

- **A (single command):** We want people to think about tasks and assets
  separately. Both are valid ways to use an orchestrator, but if you can make
  assumptions about assets, you get a tonne of benefits. Forcing people into
  the get/run mental model gets them thinking about designing things correctly.
  `get` also communicates that barca will try to do the least work possible —
  if it doesn't need to regenerate, it won't. `run` implies that the thing
  runs. Barca guarantees correctness.

- **B (noun-verb):** Redundant — barca already knows from the source file
  whether something is an asset or task, so making the user say "asset" is
  unnecessary typing. The noun-verb pattern is explicit and helpful while a
  user is learning, but immediately becomes annoying. `get` and `run` are
  two words to learn and they map to natural language.

## `parallel()` takes zero-argument callables

**Decision:** `parallel()` accepts any callable that takes no arguments — a
`functools.partial`, a lambda, a closure, a class with `__call__`. The contract
is: give us a function that's ready to go.

**Alternatives rejected:**

```python
# A: Decorator marks functions as parallelizable
@task()
@parallelizable
def deploy(region, cfg): ...

deploy.fan_out([('us-east', cfg), ('eu-west', cfg)])
```

```python
# B: Tuple syntax
parallel(
    (deploy, ('us-east',), {'cfg': cfg}),
    (deploy, ('eu-west',), {'cfg': cfg}),
)
```

```python
# C: Map-only (no general parallel)
parallel_map(deploy, regions, cfg=cfg)
```

**Why:** The moment you allow functions that take arguments, you need some way
of passing those in. Different frameworks do this differently and it's
consistently annoying. Custom syntax like tuples is another thing to learn.
If users can lean on regular stdlib to solve their problem (partial, lambda,
closure), why make them learn something custom?

`parallel_map(fn, items)` exists as sugar for the common case, but the
primitive is `parallel()` with any zero-arg callable. We give good error
messages if users violate the shape, and they can adapt immediately.

## Ordering-only deps use `_` prefix (status: open design area)

**Decision:** To declare "run X after Y but don't pass data," use
`inputs={"_y": y}`. The `_` prefix signals ordering-only; the parameter
receives `None`.

**Alternatives rejected:**

```python
# A: after= keyword (separate mechanism)
@task(after=[migrate_db, seed_db])
def warm_cache():
    refresh_all()
```

```python
# B: Both inputs= and after= available
@task(
    inputs={"model": trained_model},
    after=[migrate_db]
)
def deploy(model):
    push(model)
```

**Why we rejected each:**

- **A (after= alone):** It's another keyword the user has to learn. `inputs=`
  is how you declare sequence everywhere else, so why introduce a second
  mechanism?

- **B (both available):** `after=` is a new bit of syntax. When unsure, lean on
  the side of adding less — we can always add more later.

**Open questions for this decision:**

- The `_` prefix pattern is a bit indirect. It communicates ordering but
  requires a param the function may not use.
- We should consider not passing `_` prefixed inputs to the function at all —
  handle it before the function is called, so the user doesn't need the
  parameter in their signature.
- This is one of the decisions we're not entirely convinced of. We're noting it
  here so we can revisit when a cleaner pattern emerges.

## Sensors: SensorResult in, SensorResult out (status: parked, not implemented)

**Current implementation:** Sensors return `(updated, data)` tuples. This works
but is an unusual Python contract — ambiguous, hard to remember which element
is which, and doesn't compose well.

**Status (2026-10-10):** `SensorResult` does not exist. The tuple is the
contract (`barca docs assets`); the cache keys on the returned value and the
`bool` is not used. The redesign below is parked with no open issue.

**Redesign direction:** Sensors receive the previous `SensorResult` as a
parameter and return a new `SensorResult`. Same type in, same type out.

```python
from barca import sensor, SensorResult

@sensor()
def inbox(prev: SensorResult | None = None) -> SensorResult:
    files = list(Path('inbox').glob('*.csv'))

    match prev:
        case None:
            # First run — always trigger
            return SensorResult(triggered=True, data=files)
        case SensorResult(triggered=True, data=old_files):
            # Last run triggered — compare against what we had
            if set(files) == set(old_files):
                return SensorResult(triggered=False)
            return SensorResult(triggered=True, data=files)
        case SensorResult(triggered=False):
            # Last run didn't trigger — check again fresh
            return SensorResult(triggered=True, data=files)
```

**Why this is better than the tuple:**

- **Same type in and out.** The function receives a `SensorResult` and returns
  a `SensorResult`. No ambiguous tuple ordering.
- **Match-friendly.** Python's match statement gives you exhaustive handling of
  all three states: `None` (first run), `SensorResult(triggered=True, ...)`
  (last run triggered), `SensorResult(triggered=False)` (last run didn't).
- **Previous value accessible.** The user gets `prev.data` from the last
  triggered run, enabling sophisticated change detection (thresholds,
  timestamps, deltas) without barca needing to know the domain.
- **`prev.triggered` is useful.** Knowing whether the last run triggered lets
  the user implement hysteresis, debouncing, or "only trigger if the condition
  persisted across two consecutive checks."

**What downstream receives:** When `triggered=True`, downstream assets receive
`sensor.data`. When `triggered=False`, downstream doesn't run.

**Why not implicit diffing:** The update condition might not be "data changed."
It could be "the latest date is higher than the previous date" or "the count
exceeded a threshold." Automatic diffing would require barca to serialize and
compare full outputs on every check — expensive and fragile for large datasets.
The sensor author knows their domain.

**Why @sensor stays as a separate decorator:** A sensor is not just an asset
with a schedule. It actively decides whether to trigger downstream (the
`triggered` flag). An asset always produces data and barca decides staleness
via hash comparison. A sensor *controls* staleness. The name communicates
intent — "this is observing external state" — which guides users toward the
right patterns.

**Implementation note:** `SensorResult` should be a dataclass with
`__match_args__` so Python's structural pattern matching works. It needs
`triggered: bool` and `data: Any` at minimum.

## Freshness: when stale nodes actually execute

**Decision:** Freshness controls *when* a stale node executes, not *whether*
it's stale. Staleness is determined by hash comparison (code changed, upstream
changed). Freshness determines the response to staleness.

- **Always (default):** Execute immediately when stale. This is the free
  guarantee barca gives you — if it detects that an asset isn't fresh, it
  refreshes it and you don't have to think about it.
- **Schedule("0 5 * * *"):** Execute on the next cron tick after becoming stale.
  The node is marked stale immediately but waits for its schedule to fire.
- **Manual:** Only execute when explicitly asked, even if stale. Opt-in for
  nodes you don't want running automatically.

**Why Always is the default:** Barca's core promise is correctness without
effort. If upstream changed, your downstream should reflect that. Making users
opt in to correctness (by declaring `freshness=Always`) would defeat the
purpose. Instead, users opt *out* when they have a reason (expensive
computation, external rate limits, manual approval workflows).

**Schedule on mid-DAG nodes:** A scheduled asset in the middle of the DAG
means "I know upstream changed, but don't re-run me until my schedule fires."
This is a rate-limiting mechanism for expensive computations. The node is
marked stale, but execution is deferred to the cron tick. This is valid and
useful — not every node should react instantly to upstream changes.

**Manual as a gate:** A Manual node blocks downstream Always nodes from
auto-updating. If `config` is Manual and `report` depends on it, `report`
can't be fresher than `config`. This creates deliberate control points in the
DAG where a human must explicitly approve re-execution.

**Status (2026-10-10):** the reverse of what an earlier version of this note
said. Only `Schedule` has an effect, and only in `barca serve`
([Core constraints](../site/src/content/docs/core-constraints.md)). `Always`
and `Manual` are parsed and shown by `barca list`, and nothing acts on them:
`barca get` and `barca run` are decided by the cache alone. What they should do
under `serve` is RFC-0008 (PR #276, decision in #253).

## Design principle: when unsure, add less

Several decisions above share a common thread: when there are multiple valid
approaches and no clear winner, we choose the one that adds fewer concepts to
the API. We can always add `after=`, expand `parallel()`, or refine the sensor
contract later. We can't easily remove things once users depend on them.

This is not minimalism for its own sake. It's recognition that every new concept
in the API is a tax on every user — they must learn it, decide when to use it,
and remember it exists. The bar for adding a concept should be high.
