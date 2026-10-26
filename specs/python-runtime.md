# Python worker lifetime

Specification version: 1. Status: implemented.
Scope: module and DuckDB connection lifetime within a worker. Process/socket ownership is
specified by [execution](execution.md) and [worker protocol](worker-protocol.md).

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

- **INV-WORKER-STATE-01:** Shared import-time setup survives calls on the same worker.
- **INV-WORKER-VIEW-01:** Orchestration-created input views are cleaned after success and
  failure without advertising restoration of arbitrary project-created catalog state.

Conformance: [DuckDB connection tests](../python/tests/test_duckdb_connection.py), including
`test_worker_shared_setup_survives_but_input_views_are_cleaned`,
[worker module/import behavior](../python/barca/_worker.py),
[connection adapter](../python/barca/_duckdb.py).
The [reproduction](reproductions/duckdb_step_isolation.py) demonstrates the shared-session
tradeoff; it is not an acceptance test for per-step isolation. Earlier reasoning is retained
in [the historical note](notes/duckdb-step-isolation.md).
