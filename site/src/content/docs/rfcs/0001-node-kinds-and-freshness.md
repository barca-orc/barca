---
title: 'RFC-0001: Node Kinds & Freshness Model'
description: The asset/sensor/task vocabulary and the Always/Manual/Schedule freshness contract.
---

- **Status:** Accepted (retroactive baseline — documents behavior as of v0.6.1)
- **Current behavior:** this page is a historical baseline and some details have changed since v0.6.1. For what barca does now, read the [Decorators API](/reference/api/decorators/) and the [CLI contract](/reference/cli-contract/).
- **Date:** 2026-07-16
- **Touches:** barca-core | python/barca | HTTP server (scheduler)
- **Supersedes / Related:** [RFC-0003](/rfcs/0003-decorator-and-python-api/) (decorator surface), [RFC-0004](/rfcs/0004-http-server-api/) (`barca serve` scheduler)

---

## 1. Summary

Every node barca knows about is exactly one of three kinds — **asset**, **sensor**, or
**task** — and declares one of three freshness values — **Always**, **Manual**, or
**Schedule(cron)**. Node kind fixes whether a node is cached and where it may legally
appear in the graph; freshness fixes when it is expected to re-run. This is the smallest
vocabulary that lets the planner in `barca-core` make caching and validity decisions at
plan time, before any user code runs.

## 2. Motivation

An orchestrator that caches everything by default is wrong for side effects (a deploy
must not be skipped because its last "output" was unchanged), and an orchestrator that
caches nothing is wrong for data (rematerializing an unchanged dataframe on every run
defeats the point of the tool). Barca needed a fixed, small set of node kinds so that
`barca-core`'s planner — which never imports user code — can decide caching and DAG
validity from static AST extraction alone (`crates/barca-core/src/parse.rs`), not from
runtime behavior.

Separately, "how eagerly should this be kept up to date" is a different axis from "is
this cacheable at all." Conflating the two (as a single `schedule=` knob would) can't
express "cache this, but only refresh it by hand," so freshness is a first-class,
three-valued field.

## 3. Guide-Level Explanation

### 3.3 Decorator surface

```python
from barca import asset, sensor, task, Always, Manual, Schedule

@asset()                                  # kind=asset, freshness=Always (default)
def raw_data() -> dict: ...

@asset(freshness=Manual)                  # cached, but only refreshed explicitly
def pinned_baseline() -> dict: ...

@asset(freshness=Schedule("0 5 * * *"))   # cached, refreshed on a cron tick
def daily_report() -> dict: ...

@sensor(freshness=Schedule("*/5 * * * *"))  # kind=sensor — never cached
def inbox_files() -> tuple[bool, list[str]]: ...

@task(inputs={"data": daily_report})      # kind=task — never cached, always re-runs
def send_email(data: dict) -> None: ...
```

`@asset` and `@task` default to `Always`; `@sensor` defaults to `Manual` (there is no
meaningful "always" cadence for an external observation) and rejects `Always` outright.

Full decorator kwargs (`inputs`, `partitions`, `retries`, …) are RFC-0003's scope; this
RFC covers only `freshness` and the kind each decorator declares.

## 4. Reference-Level Explanation

### 4.1 Public API Surface

**Node kinds** (`NodeKind` in `crates/barca-core/src/model.rs`):

| Kind | Decorator | Cached | Valid as input to |
|---|---|---|---|
| asset | `@asset()` | Yes, by `run_hash` | assets, sensors, tasks |
| sensor | `@sensor()` | No — always re-runs | assets, sensors, tasks |
| task | `@task()` | No — always re-runs | tasks only |

Sensors are source nodes only (no upstream `inputs`) and return an
`(update_detected: bool, output)` tuple — the full tuple is what downstream nodes
receive.

**Freshness values** (`Freshness` in `model.rs`):

| Value | Meaning | Valid on |
|---|---|---|
| `Always` | Kept up to date automatically during `barca run`/scheduled ticks | asset, task (default for both) |
| `Manual` | Never auto-updates; only an explicit refresh materializes it | asset, sensor (default for sensor) |
| `Schedule("cron")` | Refreshes when a cron tick has elapsed since last run | asset, sensor, task |

`freshness=` is a keyword argument on `@asset`/`@sensor`/`@task`, parsed and echoed
verbatim into the plan JSON (`barca plan`) and `GET /assets` / `GET /schedule`.

### 4.2 Implementation Details

Node-kind and freshness parsing lives in `crates/barca-core/src/parse.rs`
(`extract_freshness`); DAG-level validity (the cache-poisoning guard below) lives in
`crates/barca-core/src/dag.rs`. See [Architecture](/architecture/) for how this feeds
the planner, and [Architecture Decisions](/architecture-decisions/) for the execution
engine this vocabulary drives.

### 4.3 Rust ↔ Python Boundary

Node kind and freshness are extracted entirely by `ruff_python_parser` AST inspection —
**barca never imports user code to determine them.** This is why `Schedule(...)`'s cron
argument must be a string literal in source (`extract_freshness` matches on the AST
keyword node directly); an expression that can't be read statically (e.g.
`Schedule(compute_cron())`) is not resolvable and is rejected at parse time. Contrast
this with `partitions(...)`, which *can* fall back to runtime evaluation by the Python
side — freshness cannot, because the Rust coordinator needs it before any worker exists.

### 4.4 Node-Kind Semantics

The table in §4.1 **is** the contract this RFC establishes. The cache-poisoning guard —
a task must never be an input to an asset or sensor, because a task always re-runs and
would keep any cacheable downstream perpetually stale — is enforced in
`crates/barca-core/src/dag.rs` (`DagError::TaskAsInput`) as a hard indexing-time error,
not a lint.

### 4.5 Edge Cases

- Today, `freshness` is parsed, stored, and echoed back in plan JSON, but only the
  `Schedule` kind has runtime teeth: `barca serve`'s cron scheduler
  (`crates/barca-server/src/scheduler.rs`, see [RFC-0004](/rfcs/0004-http-server-api/))
  polls `Schedule`-freshness nodes and fires them. Nothing in the executor currently
  branches on `Always` vs. `Manual` for `barca get`/`barca run` — that caching is driven
  entirely by content-hash matching (definition hash + run hash), not by this field. In
  particular, `Manual` does **not** currently block downstream `Always` assets from
  auto-updating (`@sensor`'s `Always` rejection is enforced; a `Manual` upstream
  ceiling is design intent, not yet implemented) — see
  [Core Constraints](/core-constraints/).
- Barca does not reject an explicit `Always` on a `@task` decorator (it's already the
  task default and a no-op to state), but it does reject `Always` on `@sensor`.

## 5. Determinism, Caching & Testing

Freshness itself is not part of the cache key — cache validity is provenance-based
(`definition_hash` / `run_hash` matching, see [Core Constraints](/core-constraints/)),
not recency-based. Node kind *does* gate caching: sensors and tasks are unconditionally
excluded from the cache-hit path regardless of hash match. Covered by
`crates/barca-core/src/dag.rs` and `parse.rs` unit tests (`cargo test`) and the DAG
validation integration tests in `tests/integration/`.

## 6. Performance

Not on the hot path per se — this is a one-time AST classification during parsing, not
a per-step cost. No `benchmarks/` regression risk from this vocabulary as such; changes
to *how* freshness is evaluated at scale would need a `trivial`/`chain_100` rerun.

## 7. Drawbacks

A fixed three-kind vocabulary is less expressive than Dagster's more granular
`AutoMaterializePolicy` or Prefect's flow/task/deployment split — barca trades
expressiveness for a planner that never has to import user code to make a caching
decision.

## 8. Rationale & Alternatives

A single `schedule=` field (rejected) can't express "cached, but manual-only" separately
from "cached, refresh on this cron" — freshness needed at least three values. Making
node kind and freshness orthogonal (rather than, say, a single closed enum of five
"node types") keeps the DAG validity rules (§4.4) independent of refresh cadence, so
adding a fourth freshness kind later doesn't touch node-kind semantics at all.

## 9. Prior Art

Dagster's asset/op/sensor split and `AutoMaterializePolicy`; Prefect's flow/task model
has no equivalent cached-asset concept. See
[Framework Comparison](/comparisons/framework-comparison/).

## 10. Unresolved Questions

- Should `Manual` freshness actually block downstream `Always` propagation, as
  [Core Constraints](/core-constraints/) states as intent? Currently unimplemented.
- Should sensors support a fourth "self-timed" freshness (poll continuously rather than
  cron-tick), or is `Schedule("* * * * *")` (every minute) sufficient?

## 11. Future Possibilities

A `Freshness::Reactive` kind driven by sensor output (auto-materialize only when the
upstream sensor detects a change) is a natural extension once `Manual`-ceiling
propagation lands.
