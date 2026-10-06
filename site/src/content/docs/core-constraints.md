---
title: Core Constraints
description: Deliberate MVP constraints that define the shape of the Barca product.
---

These are deliberate MVP constraints for Barca.

They are not incidental implementation details. They define the shape of the product.

## Directed acyclic graphs only

Barca only supports DAGs.

Cycles are explicitly disallowed.

That means:

- no self-dependencies
- no mutual recursion across assets
- no longer dependency loops anywhere in the asset graph

If a cycle is detected during indexing or job planning, Barca should fail immediately with a clear graph error.

### Why this is the right constraint

- it keeps scheduling and cache reuse understandable
- it keeps staleness propagation simple
- it avoids inventing vague semantics for cyclic materialization
- it matches the dominant orchestrator model for asset graphs

For the MVP, Barca should not attempt fixed-point computation, iterative cycles, or special loop semantics.

If users need iteration, they should write it inside a single asset function.

## Python interpreter resolution

This section originally proposed requiring `uv` for Python execution and environment
management. That was never implemented: Barca does not depend on `uv`, check for it, or
manage a virtualenv on the user's behalf. At runtime the Rust CLI resolves a `python`/`python3`
binary sitting next to the `barca` executable (i.e. the same virtualenv `barca` was installed
into, `uv`-managed or not) and falls back to whatever `python3` is on `PATH` — see
`find_python()` in `crates/barca-core/src/commands.rs`. Any environment manager that puts a
working `python3` on `PATH` or alongside the binary works today.

## Preflight consistency is required

Before executing a planned asset step, Barca should verify that the currently importable asset definition still matches the indexed definition.

At minimum, that means checking:

- module path resolves
- function name resolves
- current `definition_hash` matches the planned `definition_hash`

If not, execution should fail fast and require re-indexing.

This prevents the orchestrator from running stale plans against changed source.

## History is append-only

Barca should not delete old asset definitions or old materializations as part of normal operation.

Instead, Barca should:

- keep prior definitions
- keep prior materializations
- mark old states as stale, superseded, inactive, or historical as appropriate
- render history in the UI/TUI rather than hiding it

This is a core part of the product.

The point of Barca is not only to run assets, but to preserve approximate lineage over time.

### What this means in practice

- if code changes, create a new asset definition record
- if a partition disappears, keep its historical materializations
- if a run becomes stale, mark it stale rather than deleting it
- if an asset is removed from the current codebase, keep its prior history and mark it inactive

The storage model should therefore be append-only for definitions and materializations, with status flags rather than destructive updates.

### Pruning

History accumulates over time. The intent is a `barca prune` command that permanently removes
history unreachable from the current active DAG (removed assets, removed partition values, old
definition hash versions no longer referenced by any current asset) as an explicit, destructive
opt-in. **This command does not exist yet** — there is currently no way to reclaim disk space
from old artifacts/materializations short of manually clearing `.barca/`.

## Freshness declarations

Every asset, sensor, and task declares how eagerly Barca keeps its output up to date. The `freshness` parameter is the core primitive — not `schedule`.

Three freshness kinds exist:

- `Always` (default for `@asset` and `@task`)
- `Manual`: intended to mean Barca never auto-updates this node, even when stale
- `Schedule("cron_expr")`: brings this node up to date on each cron tick (recomputed only if its inputs changed; a task always runs)

Today, `freshness` is parsed, stored, and echoed back in the plan JSON, but only the `Schedule`
kind has runtime teeth: `barca serve`'s cron scheduler (`crates/barca-server/src/scheduler.rs`)
polls `Schedule`-freshness nodes and fires them on their cron tick. Nothing in the executor
currently branches on `Always` vs. `Manual` — regular `barca get`/`barca run` caching is driven
entirely by content-hash matching (see "Freshness is provenance-based, not recency-based"
below), not by this field. In particular, **`Manual` does not currently block downstream
auto-materialization**, and Barca does not reject an explicit `Always` on a `@sensor` — both are
still just design intent.

Sensors default to `Manual` freshness (they have no meaningful "always" refresh cadence).

## Freshness is provenance-based, not recency-based

Barca should decide freshness from provenance identity, not from whether something was run most recently.

That means:

- if the current `definition_hash` matches an older definition snapshot, that older snapshot becomes current again
- if the full `run_hash` matches an older successful materialization, that materialization is fresh again immediately
- Barca should reuse that prior materialization without recomputing it

This is an important invariant.

If code changes from version A to version B and later returns to version A, Barca should be able to reuse the original A outputs as long as the full provenance matches.

### Example

- `a` at definition hash `H1`
- `b` computed from `a@H1`
- later `a` changes to definition hash `H2`
- later `a` changes back to definition hash `H1`

If Barca already has a successful `b` materialization whose `run_hash` corresponds to `a@H1`, that `b` materialization should be considered fresh again without recomputation.

This is why append-only history matters: old valid provenance states remain reusable.

## Asset continuity is approximate but explicit

Barca needs a notion of "this is probably the same asset as before" so that code history remains legible across changes.

For the MVP, use this continuity policy:

- primary continuity key: explicit asset `name` if provided
- otherwise: repo-relative file path + function name
- if two live assets resolve to the same continuity key during indexing, fail with a duplicate-asset error

That is the safe default.

### Why not use `filepath/function name OR function definition` as identity

That rule is too loose for automatic identity because:

- two distinct assets can share nearly identical function definitions
- a copied function in another file may be new work, not the same asset
- a renamed or moved asset should usually keep history, but naive source matching can merge unrelated code

So for the MVP:

- use the continuity key above for canonical live identity
- use source similarity only as a non-authoritative history hint

Barca can later surface probable rename/move suggestions such as:

- "this definition looks similar to a previously indexed asset"

But it should not silently merge those histories automatically.

## User code is the execution source of truth

Barca stores inspected source snapshots for provenance, debugging, and reproducibility metadata.

Barca does not execute those stored snapshots directly.

Instead, the runner imports the real module from the user's codebase and executes the real function in the `uv` environment.

That keeps:

- imports honest
- tracebacks accurate
- helper/module semantics intact
- environment problems correctly attributed to user code

## Practical implications

These constraints imply:

- asset discovery requires importable, source-backed Python modules
- notebook-defined assets are not first-class indexed assets in v1
- graph validation is a mandatory indexing step
- the execution engine must verify the planned `definition_hash`
- dependency cone hashing is part of cache invalidation and provenance
- definitions and materializations are append-only records
- duplicate live continuity keys should fail indexing
- old matching provenance can become current again without rerunning
