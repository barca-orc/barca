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

[Concrete failure scenarios](reliability-scenarios.md) record timelines, evidence,
explicit user decisions and unanswered questions for each boundary. Discussion
examples do not provide additional approval of unselected proposals.

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

**Accepted product direction, 2026-10-10 (not implemented by this draft):**

- Environments provide separation. Local execution may publish an updated result
  to shared storage; server execution may publish another result in the same
  environment. Publication must be idempotent.
- Asset nodes are assumed pure functions of their definition and tracked inputs.
  The computation hash is the logical result version. External observations and
  side effects belong upstream; changed external data must enter through sensors
  or other tracked inputs to change dependent asset hashes. Purity is an assumption,
  not a runtime guarantee that arbitrary Python code is deterministic.
- A stale local cache automatically synchronizes to the selected published
  result before consumption. Ordinary stale-cache reads do not require `--force`.
- If the published result changed since an execution's starting observation,
  replacing it is a publication conflict: warn and require explicit `--force`.
  A delayed retry encountering a newer result must not silently overwrite it.
- Force permits intentional conflicting publication. It does not make a false
  upload receipt or a mismatch between selected metadata and stored bytes valid.
  Each receipt/checksum must describe the exact uploaded bytes.
- Existing hash-addressed artifacts provide computation versioning: a changed
  hash writes a separate path and earlier paths remain. Cached gets reuse data;
  same-hash refreshes overwrite the same path and are expected to reproduce the
  same bytes. Different bytes under one hash indicate a purity violation or
  integrity defect, not a requirement to archive every execution as a new version.
  Reversion should build on retained computation versions; no new per-execution
  byte-version storage scheme is selected.

Publication/conflict mechanics, baseline observation, retry recognition,
force/revert command surface and retention duration still need an implementation
plan. Automatic cross-machine freshness, conflict forcing and reversion tools are
requirements, not features implemented by this draft. New computation hashes are
ordinary versions; merely retaining multiple hashes is not a checksum conflict.
Saved-result selectors and partition-set membership/version semantics still need
specification. Reuse existing artifacts rather than copying every partition or
introducing per-execution archives to create a result-set manifest.

Keep these concepts distinct:

| Identity | Current meaning | Limit |
| --- | --- | --- |
| Computation key (`run_hash`) | Logical asset version from its definition and tracked inputs; chooses the deterministic cache path | Same-key recomputation is expected to reproduce the same bytes; arbitrary user code can violate purity |
| Artifact checksum | Validation of particular bytes associated with a receipt/result | A checksum is not an immutable locator or a retention policy; #381 covers the snapshot race |
| Durable run identity | Database identity for historical execution records | Today it is distinct from a live server polling handle; #319 owns convergence/compatibility |
| Current partition membership | Current DAG/planning membership for current selection and SQL | Historical rows can remain for removed keys; history does not make those keys current |

[Artifact path construction](../python/barca/_artifacts.py) intentionally permits
refresh to overwrite the same computation-addressed path. Existing history is
therefore not a per-execution archive of different bytes under the same hash.
Different computation versions already have separate paths. See the existing
[cache manual](../crates/barca-cli/docs/cache.md) for purity, sensor inputs and
the current storage layout.
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

**Discussion correction:** the earlier proposal to retain each execution's
original bytes is withdrawn. A saved result can identify an existing computation
version; a same-hash refresh is expected to be equivalent. A forced purity-violating
replacement is not promised to preserve the previous bytes of that logical version.
Exact API selectors for a particular computation version versus current results,
missing/deleted/corrupt behavior and partition-set composition remain to be specified.
A result reference is not a promise of indefinite retention.

Specify multidimensional keys, ordering, membership/version
identity, one/subset/all return shapes, CLI/Python compatibility, stale-handle
behavior and reference protection under GC. Reuse existing artifacts when safe;
avoid eagerly copying all partitions merely to create a result-set manifest.
Build reversion and saved access on retained hash-addressed versions while preserving
the accepted purity/cache semantics. Listing is metadata-only; selected reads touch only selected artifacts
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
5. Jointly specify #287/#57 using the accepted computation-version/purity model. Approve exact
   API shapes and retention/migration dependencies before splitting manifest,
   selector, saved-read and targeted-refresh implementations into sequential PRs.

Each implementation PR updates its native contracts, executable conformance and
manual/site descriptions. Test interruption immediately before/after each owned
boundary, retries/replay, cache-only runs, failures, cancellation and restart.
This document is an index of those guarantees and gaps, not an additional
scheduler, persistence layer or public state machine.
