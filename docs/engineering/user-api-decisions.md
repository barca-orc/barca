# User API rationale

Status: rationale for implemented choices, checked against the 0.33.0 baseline.
This document explains decisions; the normative owners are [the specification index](../../specs/README.md).
[Unscheduled API ideas](../../specs/proposals/api-ideas.md) and
[approved execution/status changes](../../specs/behavior/current-state-and-execution.md) are separate.

## Assets, tasks and sensors

Assets represent reusable data; tasks represent requested actions; sensors observe external
state. Separate decorators teach these distinctions and let cache behavior follow the kind
of work. A single `step(cache=True|False)` would make each user configure that policy.

Task inputs to assets or sensors are a hard validation error. A task performing a transfer
can be independent of a sensor observing its destination, preserving cacheable data chains.
Allowing an uncacheable asset with a warning is an unscheduled alternative, not a future promise.

Sensors accept `True`/`False`, `(changed, value)`, or a value for content comparison. Explicit
signals are honored from 0.33.0; before that the tuple flag was ignored. The observation
identity is separate from the bytes digest, so a timestamp-only signal can refresh an asset
that produces its data later. Dictionary partition sources compare values per key.
A typed `SensorResult` and injected previous observation are not implemented or approved.

Normative owner: [freshness](../../specs/behavior/freshness.md).

## Ordinary Python declarations

Decorators are identity functions. Users can import and call their functions directly without
an orchestration context. The Rust parser extracts declarations using static analysis.

Static extraction does not mean every planning operation is import-free: nonliteral
`partitions(...)` expressions and computed `batches_by(...)` groups use an importable project
subprocess. Literal keys/groups are read statically. `doctor` does not evaluate those expressions.
This bounded exception keeps declaration reading fast while supporting computed domains.

`inputs={"data": raw_data}` separates the upstream function reference from the receiving
parameter name. It supports IDE navigation and explicit dependency resolution. Implicit
parameter-name wiring and a separate user-maintained graph definition were rejected because
they obscure that distinction or duplicate the graph.

Underscore-prefixed declared inputs express ordering without data; those parameters receive
`None`. Omitting the parameters entirely is an unscheduled idea, not current calling behavior.

Normative owners: [declaration model](../../specs/interfaces/user-api.allium),
[cache provenance/completeness](../../specs/behavior/cache-decisions.md),
[decorator grammar tests](../../crates/barca-core/tests/grammar_spec.rs).

## Get data, run actions

`get` requests assets and resolves the necessary cone with cache-aware decisions. `run`
requests a task; matching asset ancestors can remain cached. A sensor can be explicitly
targeted with either command and is polled. Separate commands make the
expected reuse visible without redundant `asset get` / `task run` nesting.
The complete targeting, refresh and output rules are in the existing CLI contract.

`parallel()` accepts partial-like callables exposing a function, positional arguments and
keywords; the supported public form is `functools.partial`. It does not currently accept
arbitrary lambdas, closures or callable objects. `parallel_map` supplies partials for the
common mapping case. Results retain argument order; user branch failures become
`ParallelError` values. Failure to transport a branch result is a separate error and must
not silently replace the value with `None`. Standalone Python uses sequential execution.

Normative owners: [execution](../../specs/behavior/execution.md), [task results](../../specs/interfaces/task-output.md),
[manual](../../crates/barca-cli/docs/tasks.md), [CLI contract](../../crates/barca-cli/docs/contract.md).

## Freshness and scheduling

`Always` is the default for assets/tasks; sensors default to `Manual` but are polled whenever
required by a run. A scheduled tick reconciles its node and reachable `Always` descendants,
stopping at a separate schedule. The schedule is an automatic trigger under `serve`; an
explicit request does not need to wait for the next cron tick to compute ordinary stale work.

`Manual` pins itself rather than acting as an approval gate over its descendants. Readers
use its existing result and can update for other changed inputs. Explicit refresh recomputes
it. A never-produced pin blocks readers. Partitioned Manual nodes and deprecated HTTP aliases
retain the documented exceptions; do not generalize the unpartitioned canonical-run rule to them.

Normative owner: [freshness](../../specs/behavior/freshness.md); examples and compatibility details:
[scheduling manual](../../crates/barca-cli/docs/scheduling.md).

## Prefer fewer concepts

Defaults should do the least work needed to honor the request. New public concepts need a
clear benefit over the existing declarations. An implementation optimization must preserve
observable results and documented failure behavior. Benchmark wins on one workflow do not
establish that cold bootstrap, inspection, remote-ledger execution or every graph shape has
the same cost.
