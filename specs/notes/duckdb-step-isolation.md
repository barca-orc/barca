# DuckDB connection lifetime (#302 / roadmap P02)

Status: **accepted product decision, 2026-10-09**.
The user chose one connection per process because a process executes steps sequentially.
Per-step connection isolation and a new setup API are not planned. Related: #299, #84.

## Contract

Barca uses DuckDB's default connection once per worker process. User modules remain cached;
import-time configuration via `barca.duckdb_connection()` runs once when the module imports.
Macros, extensions, credentials, settings and user-created objects intentionally persist on
that connection. Independent workers have independent connections. Standalone Python retains
DuckDB's ordinary default-connection behavior.

Barca's relation input views are temporary orchestration bindings: they remain live through
execution and output/sink materialization, then are dropped after success, user exceptions
and serialization failures. Lazy relations must not be consumed after their connection closes
or passed to later steps through the in-process artifact LRU. Existing code enforces this.

This is not a catalog-isolation or session-reset promise. A user-created `picked` view can
shadow a later Python variable named `picked`. Use relation operations, CTEs or unique names
with deliberate cleanup. An input-view name that collides with a project object is not an
isolated namespace: existing tables are skipped and an existing view may be replaced and
subsequently dropped. Do not depend on preserving such collisions. The worker does not restore
arbitrary user-created catalog state, including after a user exception.

## Investigation evidence

```sh
PYTHONPATH=python python specs/reproductions/duckdb_step_isolation.py
```

Executed against DuckDB 1.5.6 and worker code at 523e0a4:

- A consumer returns 80.0 before a user-created `picked` view, then 165.5 afterwards.
- Repeating the producing step fails because that user-created view already exists.
- A failed step's user-created view persists; Barca-owned input views have a separate cleanup.
- An import-time macro works with the shared connection but fails on a fresh connection when
  its module remains cached.
- Transaction rollback removes a new table but does not undo `SET threads`.
- Lazy relations require their connection to stay live through consumption/materialization.

These are intentionally shared session semantics under the accepted decision, rather than a
requirement for Barca to isolate every user-created object. The executable probe captures the
tradeoff; it is not a test asserting a desired per-step isolation guarantee.

## Rejected implementation directions

Fresh default connections lose supported import-time setup. Re-importing source/helper modules
per step repeats arbitrary side effects and changes module identity and worker performance.
A hidden transaction conflicts with user transactions and cannot reset all session state.
Deleting only newly created objects cannot restore overwritten baseline objects/data/settings.
A new setup decorator or hook framework is unnecessary for the accepted process lifetime.

## Regression evidence and scope

`test_worker_shared_setup_survives_but_input_views_are_cleaned` exercises the real daemon step
function across success, user failure and injected materialization failure. Each history then
runs a second step using the same cached module, connection and LRU. It proves import-time
macros and deliberate project views remain usable, helper SQL resolves the input, returned lazy
relations materialize before input cleanup, and Barca's input view is absent after each step.
Existing end-to-end tests exercise the actual worker protocol and standalone getter.

No public API, execution lifetime or connection-reset implementation changes are needed.
The broad isolation request in #302 is not planned under this decision. Future worker reuse
must preserve these semantics; separate setup discussions must not silently change them.
