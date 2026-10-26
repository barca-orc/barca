# Python worker lifetime

## Scope and status

Specification version: 2. Status: implemented.
Scope: module and DuckDB connection lifetime within a worker. Process/socket ownership is
specified by [execution](execution.md) and [worker protocol](../interfaces/worker-protocol.md).

## Entities and state

A worker owns one interpreter, cached project modules and a process-local DuckDB connection.
Calls have temporary orchestration input views; user-created module/catalog state can survive
between calls. Separate workers have separate interpreter and connection state.

## Rules and transitions

A worker imports project modules using ordinary Python source/module identity and caches
modules across calls during its run. User import-time setup and deliberate state can persist
between steps on that worker. Separate workers have separate interpreter state. Decorators
remain identity functions; this lifetime is not a promise of isolated Python globals per key.

DuckDB uses its default connection once per worker process. Import-time configuration via
`barca.duckdb_connection()` persists there, including macros, extensions, credentials,
settings and user-created objects. It is not a fresh connection or catalog per step.

Barca-created input views remain live through execution and output/sink materialization,
then are removed on success, user exceptions and serialization failure. Lazy relations must
not outlive their connection or be carried to later steps through the artifact LRU.
User-created catalog changes are not reset or rolled back by Barca. Colliding view names can
be replaced and dropped; use deliberate names and cleanup rather than relying on collision
preservation. An import-time macro must not be lost through a hidden per-step reset.

## Invariants

- **INV-WORKER-STATE-01:** Shared import-time setup survives calls on the same worker.

- **INV-WORKER-VIEW-01:** Orchestration-created input views are cleaned after success and
  failure without advertising restoration of arbitrary project-created catalog state.

## Failures and recovery

Orchestration input views are removed after success, user exceptions and materialization
failures. User-created objects/settings are not rolled back or restored. A lazy relation must be
materialized while its connection and temporary views are live; it cannot be carried through the
artifact LRU to another step.

## Acceptance and evidence

Verify import-time macros and deliberate module/catalog state survive calls on one worker while
orchestration views are cleaned after success, user failure and serialization failure. Compare
separate-worker state and lazy-result materialization lifetime.

Conformance: [DuckDB connection tests](../../python/tests/test_duckdb_connection.py), including
`test_worker_shared_setup_survives_but_input_views_are_cleaned`,
[worker module/import behavior](../../python/barca/_worker.py),
[connection adapter](../../python/barca/_duckdb.py).
The [reproduction](../../docs/engineering/reproductions/duckdb_step_isolation.py) demonstrates
the shared-session
tradeoff; it is not an acceptance test for per-step isolation. Earlier reasoning is retained
in [the historical note](../../docs/engineering/history/duckdb-step-isolation.md).

## Open questions and gaps

Per-step isolation is deliberately not promised. Full run-start source/configuration capture
remains an approved operations obligation; cached modules and hash-validated bytecode alone do
not establish it.
