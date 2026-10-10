# Ownership, durability and result identity

Scope: internal execution, persistence and artifact boundaries, and the proposed
saved-results boundary. Document version: **1**. Status: **current implemented
invariants plus explicitly proposed extensions**, audited against main `96022e0`
and release 0.22.0 on 2026-10-10.

This index connects the existing contracts; it does not replace their native
specifications or implement new behavior. HTTP shapes remain owned by
[OpenAPI](server-api.openapi.yaml), CLI shapes by the
[CLI contract](../crates/barca-cli/docs/contract.md), and metadata compatibility
by [metadata-schema.md](metadata-schema.md). Historical implementation plans
retain their original evidence; their preparation-stage wording is not the
current delivery status. Roadmap [#344](https://github.com/barca-orc/barca/issues/344)
owns delivery status.

[Concrete failure scenarios](reliability-scenarios.md) record timelines, evidence
status and unanswered questions for each boundary. They are discussion examples,
not approvals of the proposed policies below.

## 1. Ownership

Current invariants:

| Resource | Owner and lifetime | Evidence / contract |
| --- | --- | --- |
| User execution and SQL setup | One worker executes one assigned step at a time. Imported modules and the default DuckDB connection persist for that worker process. Independent workers have independent connections. | [DuckDB lifetime](duckdb-step-isolation.md), [worker regressions](../python/tests/test_duckdb_connection.py) |
| Orchestration input views | Worker-owned bindings remain alive through execution and output materialization, then are cleaned on success or failure. This does not reset arbitrary user-created catalog state. | [DuckDB lifetime](duckdb-step-isolation.md) |
| Run progress and terminal persistence | Rust owns database writes. The existing recorder owns committed-progress bookkeeping and periodic publication; it is stopped/awaited before terminal persistence and publication. Transfer helpers report receipts rather than writing the database. | [execution](../crates/barca-core/src/execution.rs), [persistence](../crates/barca-core/src/persist.rs), [minute checkpoints](remote-progress-checkpoints.md) |
| Staged file | The creating operation tracks its exact acquired path through finalization or removal. Its finalizer removes that path; helper shutdown removes all stages registered by that process, including concurrent operations. Cleanup protects unowned files and other processes' stages rather than scanning directories. Graceful cancellation coordinates creation, removal and signal replay. | [staged finalization](staged-file-finalization.md), [transfer lifecycle regressions](../python/tests/test_transfer.py) |
| Shared-state snapshot / replacement | Admission can be cancelled before atomic database work starts. Once admitted, the existing atomic protocol finishes safely. Database guards are released before network waits. | [checkpoint admission](checkpoint-admission.md), [state synchronization](../crates/barca-core/src/state_sync.rs) |

These are resource owners, not a global serialization guarantee. Multiple runs
or processes may coexist. One connection per sequential worker does not imply
one run per server. SIGKILL cannot promise graceful stage cleanup; arbitrary
orphan collection and retention remain separate work. A deadline does not abort
an admitted atomic swap or an individual filesystem syscall midway through it.

Remaining correctness gap: [#381](https://github.com/barca-orc/barca/issues/381)
owns the producer/upload snapshot lifetime when another refresh atomically
replaces a deterministic artifact path. Exact-path cleanup ownership alone does
not guarantee that a receipt hashes the bytes actually uploaded.

## 2. Durability

Treat the following as distinct milestones, rather than one completed flag:

1. **Execution produced output.** Local serialization or a queued transfer is
   insufficient evidence of remote availability.
2. **Configured transfer acknowledged success.** Only confirmed receipts may
   supply remotely reusable progress. Receipt delivery is not a database commit.
3. **Local progress committed.** Step rows and running-run counts commit in one
   transaction. Failed batches remain available for retry; duplicate/replayed
   run/node rows do not increment counts twice.
4. **Shared checkpoint acknowledged.** A snapshot contains committed progress.
   Publication captures that committed generation, uses the existing conflict/
   carry protocol and retains acknowledged tokens. Unknown/lost acknowledgement
   is not equivalent to a confirmed absent remote object.
5. **Terminal ledger committed.** Required terminal rows and status/counts commit
   atomically. Successful owner release follows confirmed terminal persistence;
   a failed write must not masquerade as durable completion. Terminal shared-state
   publication still follows its existing separate success/error policy.

This is a description of evidence boundaries, not a newly implemented public
enum or total order for every step. Local-only runs omit remote milestones;
uploads of different steps can finish out of order; terminal persistence is also
the complete fallback for incremental progress.

Current publication cadence is sixty seconds in healthy operation, with one
in-flight checkpoint and coalesced missed ticks. Clean ticks without newly
committed progress from this recorder skip publication. Its generation does not
track arbitrary writes by other local processes; conflict handling or concurrent
database changes can legitimately require another upload. Outages can exceed that recovery interval;
it is not a universal one-minute data-loss guarantee. Checkpoint failure retains
local progress and retries at the next tick rather than failing successful user
work or spinning.

Owners: [terminal ledger](terminal-ledger-commit.md),
[transactional recorder](committed-progress-recorder.md),
[minute checkpoints](remote-progress-checkpoints.md),
[cancellation reconciliation](cancelled-shared-history.md).
Executable evidence: [persistence regressions](../crates/barca-core/src/persist.rs),
[carry regressions](../crates/barca-core/src/state_carry.rs),
[database regressions](../crates/barca-core/src/db.rs),
[transfer regressions](../crates/barca-core/src/transfer.rs),
[terminal persistence integration](../python/tests/test_terminal_persist.py),
[incremental integration](../python/tests/test_incremental_persist.py),
[real-minute recovery](../python/tests/test_minute_checkpoints.py),
[cancellation integration](../python/tests/test_remote_cancel.py).

Uncommitted queued outcomes can be lost on SIGKILL. Recorder buffering currently
scales with completed outcomes and queued receipts; it has no fixed memory cap.
Required database transactions do not make every subsequent filesystem or
post-publication bookkeeping operation atomic; consult the native metadata and
state-sync evidence for those limits.

Remaining extensions:

- [#319](https://github.com/barca-orc/barca/issues/319): durable identity at
  acceptance, complete outcomes/tracebacks and trigger provenance, compatible
  status lookup after restart. Existing server polling handles and optional
  database run IDs are distinct. Terminal row atomicity does not promise atomic
  captured-log persistence.
- [#318](https://github.com/barca-orc/barca/issues/318): one reporting path,
  ordered event IDs and defined reconnect/lag recovery. Event delivery cannot be
  used as evidence of a database commit. Specify replay lifetime, limits and
  behavior after expiry/restart before claiming lossless durable replay.
- [#243](https://github.com/barca-orc/barca/issues/243) and
  [#83](https://github.com/barca-orc/barca/issues/83): recovery and retention.
  [#86](https://github.com/barca-orc/barca/issues/86) retains real-cloud acceptance.

## 3. Result identity

**Accepted publication direction, 2026-10-10:** local execution may publish an
updated result to shared storage, and server execution may overwrite that result
in turn. Overwriting current artifacts is permitted; publication must be
idempotent. Receipt/checksum correctness still requires describing the exact
uploaded bytes. This does not select immutable historical saved handles or
resolve retries of an earlier publication after a newer publication succeeds;
that retry-ordering detail remains a discussion question.

Keep these concepts distinct:

| Identity | Current meaning | Limit |
| --- | --- | --- |
| Computation key (`run_hash`) | Code/input identity used to choose the deterministic cache path | Recomputing the same key may produce different bytes |
| Artifact checksum | Validation of particular bytes associated with a receipt/result | A checksum is not an immutable locator or a retention policy; #381 covers the snapshot race |
| Durable run identity | Database identity for historical execution records | Today it is distinct from a live server polling handle; #319 owns convergence/compatibility |
| Current partition membership | Current DAG/planning membership for current selection and SQL | Historical rows can remain for removed keys; history does not make those keys current |

[Artifact path construction](../python/barca/_artifacts.py) intentionally permits
refresh to overwrite the same computation-addressed path. Existing history is
therefore not a guarantee that every prior result's bytes remain readable.
[Current SQL membership](sql-current-partition-membership.md) and
[its regressions](../python/tests/test_sql_current_partitions.py) distinguish
current membership from retained history.

**Accepted direction:** save partition results and allow single, explicit subset
and all-partition access, without unnecessary data movement. A metadata-only
selection must not eagerly combine or deserialize every partition. Reading
saved results and explicitly materializing/refreshing results are distinct
operations. [#287](https://github.com/barca-orc/barca/issues/287) owns results and
access; [#57](https://github.com/barca-orc/barca/issues/57) owns selectors and their
composition with execution/refresh/backfill.

**Proposed, awaiting product decision:** a saved handle continues to identify its
original bytes after refresh; a separate latest lookup follows current results.
For partition A changing from 11 to 22, the old saved handle still reads 11.
This is not implemented or approved. If handles instead follow latest, their
mutable semantics must be explicit. Either choice needs defined behavior for
missing/deleted/corrupt data; a reference is not a promise of indefinite retention.

After that decision, specify multidimensional keys, ordering, membership/version
identity, one/subset/all return shapes, CLI/Python compatibility, stale-handle
behavior and reference protection under GC. Reuse existing artifacts when safe;
avoid eagerly copying all partitions merely to create a result-set manifest.
Specify how old bytes can be retained without changing current cache semantics
silently. Listing is metadata-only; selected reads touch only selected artifacts
and reuse validated local data or supported remote reads. Test actual bytes/read
counts alongside values and restart behavior. Exact API signatures and storage/
migration mechanics remain engineering proposals.

## Delivery order and parallel work

1. This documentation PR records the current contracts and open proposals. It
   changes no behavior and closes none of the residual tickets.
2. Prepare independent bounded plans for helper hashing #295, upload snapshot
   correctness #381 and publication readiness #384. They need no saved-handle
   policy decision; serialize merges and coordinate any shared-file edits.
3. Specify #319's durable run identity and outcome persistence first, including
   acceptance failures, legacy handles, schema compatibility and log atomicity
   boundaries. Then implement one independently useful slice per PR.
4. #318's reporting extraction can be prepared independently while preserving
   CLI output. Its terminal events must follow the agreed commit contract;
   replay/resume implementation needs an explicit scope and resource bound.
5. Jointly specify #287/#57 after the saved-handle policy decision. Approve exact
   API shapes and retention/migration dependencies before splitting manifest,
   selector, saved-read and targeted-refresh implementations into sequential PRs.

Each implementation PR updates its native contracts, executable conformance and
manual/site descriptions. Test interruption immediately before/after each owned
boundary, retries/replay, cache-only runs, failures, cancellation and restart.
This document is an index of those guarantees and gaps, not an additional
scheduler, persistence layer or public state machine.
