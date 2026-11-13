# Architecture map and cleanup sequence

Status: close-out, 2026-11-04. This document was the implementation map and refactor
sequence written at `ce56f197` on 2026-10-26 (runtime baseline `fa396a5d`, 0.33.0). It is kept
as the record of what the map found and what the sequence became; it is not a behavioral
contract, not evidence that approved requirements are implemented, and not a plan anyone
works from: the remaining items are scheduled in [DIRECTION.md](../../DIRECTION.md), "Now, in
order" (the 0.39.6 cleanup). The historical-selection PR this map waited on,
[#658](https://github.com/barca-orc/barca/pull/658), was closed unmerged on 2026-10-26; the
sequential fix shipped in 0.34.0 (#657, [behavior conformance](behavior-conformance.md)).

What shipped, by step of the sequence below:

- Step 1, bookkeeping out of execution: 0.33.1 ([#659](https://github.com/barca-orc/barca/pull/659),
  `execution/bookkeeping.rs`).
- Step 2, one publication boundary: the write boundary in 0.39.5 (#800 item 16,
  [#819](https://github.com/barca-orc/barca/pull/819), `db/` and `persist/recording.rs`, a test
  that fails on a write outside them), and `db.rs` split by table family after it
  ([#821](https://github.com/barca-orc/barca/pull/821)). Guarded, ordered publication (step 2b)
  is not done.
- Step 4, shared computation: shipped first, as experimental, in 0.39.4
  ([#796](https://github.com/barca-orc/barca/pull/796), `serve/claims.rs`), ahead of steps 2b
  and 3; its three gaps are in #654.
- Beside the sequence, 0.39.5 also delivered one `CacheState` enum (#802), serve out of the
  embedded hot loop (`RunHost`, `serve/`, [#817](https://github.com/barca-orc/barca/pull/817)),
  the `.barca/` layout specification (#805) and the Python dedupe.

What remains: step 2b (ordered publication), step 3 (captured code/configuration and stable
input bytes), the rest of step 4 (#654) and step 5 (#655, #656). The structural items the
0.39.6 list adds (`execution.rs` split by phase, a `StepStatus` enum, one artifact format map,
`runs.rs`/`client.rs` splits, store inspection out of `status.rs`, a narrower `barca-core` pub
surface) come from a second audit on 2026-11-04 and are listed only in DIRECTION.md.

Behavior owners remain in [the specification index](../../specs/README.md). The target responsibilities
below follow [approved ownership](../../specs/behavior/current-state-and-execution.md),
[operations](../../specs/behavior/operations.md), and [resource bounds](../../specs/behavior/resources.md).
Names describe responsibilities, not mandated new crates, services, tables or public types.

## Bookkeeping extraction

The first implementation slice introduces
[execution bookkeeping](../../crates/barca-core/src/execution/bookkeeping.rs): `RunResults`
owns collected outputs, failures, attempts, timings, observations, reports and batch identities.
It decodes finished artifact evidence, merges only plan outputs, annotates report metrics and
builds the borrowed terminal ledger projection. `RunEvidence` holds the existing component
and cost snapshot buffers; the driver determines status and when they are constructed.

`RunSession` retains recorder, worker and store lifetimes, and execution retains freshness,
capacity, cancellation and publication decisions. The shutdown, upload drain, recorder stop,
terminal write, log write, telemetry and backup sequence is preserved. This extraction does
not implement the remaining ownership, snapshot or bounded-work requirements below.

## Current implementation

### Admission and run lifetime

The CLI delegates execution through [core commands](../../crates/barca-core/src/commands.rs).
On the server, [runs](../../crates/barca-server/src/runs.rs) validates requests and drives
execution; [queue](../../crates/barca-server/src/queue.rs) admits runs using run slots and
[durable queue records](../../crates/barca-core/src/serve/run_queue.rs). [Server state](../../crates/barca-server/src/state.rs)
holds live run state, events, slots and shared capacity. [Server lifecycle](../../crates/barca-server/src/lib.rs)
owns startup and drain. Queue acceptance is durable before acknowledgement.

These are run owners, not owners of shared asset computations. Existing run IDs and queue
replay do not establish the proposed client invocation/attempt contract or transport retry
deduplication. Today requests can join the queue during drain; deadline cancellation can
requeue the run without separating assets from explicit tasks. Crash-interrupted running
queue entries are handled differently from draining entries. Preserve these existing rules
in structural refactors; change them explicitly to meet the approved operations intent.

### Preparation, decisions and execution

[Loading](../../crates/barca-compile/src/load.rs), [project import analysis](../../crates/barca-compile/src/project_modules.rs),
[target resolution](../../crates/barca-compile/src/targets.rs) and [planning](../../crates/barca-compile/src/planner.rs)
prepare a dependency cone. [Cache decisions](../../crates/barca-core/src/cache.rs) and
[incremental proofs](../../crates/barca-core/src/incremental.rs) decide which work can be reused.
[Dispatch](../../crates/barca-engine/src/dispatch.rs) expands partitions and builds provided inputs.

[Execution](../../crates/barca-core/src/execution.rs) joins these pieces. Its `RunSession` owns
a recorder, worker pool and store synchronization; `RunState` retains decision state, outputs,
reports, failures, observations, pins and accounting. It handles phase progression, transfer
recovery and finalization. This is the main concentration of responsibilities to untangle.
The existing `selection.rs` narrows requested partitions; its name does not make it an owner
of ordered current-result publication.

[Coordinator](../../crates/barca-engine/src/coordinator.rs) already separates scheduling state
from [worker I/O](../../crates/barca-engine/src/io_loop.rs). Keep that separation. The worker pool
is reused across phases of one run, while [capacity](../../crates/barca-core/src/serve/capacity.rs)
can be shared across server runs. Sharing capacity does not deduplicate matching work.
The coordinator's per-item retries are also distinct from a new user-requested invocation attempt.

### Results, publication and bytes

[Persistence](../../crates/barca-core/src/persist.rs) records successful steps through
`StepRecorder`, then fills gaps and records terminal state through `persist_run`. Its
`StepRow` already gives both paths a common representation. The terminal ledger is
transactional; that alone does not establish ordered observation publication. The recorder
uses an unbounded channel and run state retains per-key maps: current warm incremental
optimizations are not a bound on cold/full-domain execution.

[Freshness state](../../crates/barca-core/src/freshness.rs), incremental proof saving and
[database operations](../../crates/barca-db/src/db/mod.rs) participate in current-state decisions.
[Observation](../../crates/barca-engine/src/observation.rs) and
[source observations](../../crates/barca-model/src/source_observation.rs) provide evidence;
their hashes do not supply a monotonic reconciliation order. Result history, current
membership/selection and freshness evidence need explicit responsibilities before multiple
versions can publish concurrently.

[Store synchronization](../../crates/barca-core/src/store_sync.rs),
[transfer](../../crates/barca-core/src/transfer.rs), [pending uploads](../../crates/barca-db/src/pending_uploads.rs)
and the [server ledger](../../crates/barca-core/src/ledger.rs) handle availability and sharing
of completed artifacts. [Upload receipts](../../specs/interfaces/artifact-upload-receipts.md) protect producer
upload evidence, not every downstream reader's input lifetime. [GC](../../crates/barca-core/src/gc/mod.rs)
already has planning and pin protection, but implements size-based eviction, not the approved
three-version retention policy. Preserve these distinctions rather than treating a history
row or upload receipt as proof that bytes are available and isolated.

### Inspection and recovery

[Core status](../../crates/barca-core/src/status.rs) and [queries](../../crates/barca-core/src/queries.rs)
derive inspection results. [Server inspection](../../crates/barca-server/src/inspect.rs) reads
metadata snapshots; [running projections](../../crates/barca-server/src/running.rs) combine live
events with cache state. A metadata read snapshot is not an execution input snapshot.
Explicit checking revisions, independent availability and bounded cold inspection remain
acceptance obligations in [inspection](../../specs/behavior/inspection.md).

[Run owner diagnosis](../../crates/barca-db/src/run_owner.rs) distinguishes proven alive,
proven gone and unknown owners. This protects against treating an unreachable owner as dead;
retain that distinction. [Recover](../../crates/barca-core/src/recover.rs) primarily recovers
missing cached artifacts; it is not the proposed shared-computation ownership manager.
[History reconciliation](../../crates/barca-core/src/queries.rs) and server queue replay have
different scopes and must not be collapsed into one unconditional retry path.

The [Python source loader](../../python/barca/_source_import.py) reads source from disk with
hash-validated bytecode and caches imported modules. [Workers](../../python/barca/_worker.py)
load modules when needed. Static source hashing and a cached module do not freeze all project
imports for a run: a later-starting worker or first import can read edited source. A complete
run-start code/configuration snapshot is therefore an implementation gap; environment and
dependency boundaries remain a design question in the [handoff](requirements-handoff.md).

## Responsibility boundaries to establish

- **Request and attempt:** owns target/arguments, admission, requester cancellation, progress
  and outcome. Adapters translate CLI/HTTP into core operations and render their results.
  Disconnecting a subscriber and terminating a computation are separate decisions.
- **Prepared execution:** owns the captured definition/configuration and resolved input
  references used by an attempt. Explicitly state when inputs resolve. Input ownership lasts
  through all consumers, including a consumer whose originating requester has detached.
- **Computation execution:** owns scheduling, workers, capacity, retries and physical result
  production. Initially keep run-local ownership; only a later server-owned registry may
  attach several requesters to one complete resolved asset identity. Tasks and sensor polls
  must not enter that registry as ordinary cached assets.
- **Result recording and selection:** records computation history independently of whether
  the result may become current. One publication operation checks observation order and
  updates selection, membership, evidence and summaries consistently. Cache-only reuse must
  use it too. An old completion can succeed for its callers without becoming current.
- **Artifact lifetime:** owns finalized byte references, read-through, active pins, upload
  staging and cleanup eligibility. Freshness decisions consume availability findings rather
  than guessing from history. Do not extend upload staging into a blanket isolation claim.
- **Inspection projection:** combines durable selection/evidence, availability and live work
  without becoming another scheduler or writer. Checking results need revision guards;
  bounded pages and summaries must distinguish unknown counts from zero.

Keep these as in-process responsibilities. There is no evidence here that adding services,
changing persistence technology or replacing the Python protocol would improve the boundary.

## Refactor sequence

### 1. Separate run bookkeeping from execution decisions

First implementation PR: isolate run result accumulation and ledger/report construction
inside `execution.rs`, using the existing `RunSession`, `RunState`, `StepRow` and `RunLedger`
boundaries. Group state by decisions, produced results and run reporting; avoid merely moving
the whole session into another large module. Keep phase decisions, recorder/worker/store
drop order, event order and terminal-write timing unchanged. No schema or public API change.

Review criterion: the phase driver can call a small set of bookkeeping operations without
building persistence rows or output envelopes inline. Run the existing
[incremental persistence](../../python/tests/test_incremental_persist.py),
[terminal persistence](../../python/tests/test_terminal_persist.py),
[progress](../../python/tests/test_progress_counts.py) and
[batch](../../python/tests/test_batched_partitions.py) coverage against a build of that change.
Preserve failure/cancellation evidence, not just happy-path output snapshots.

### 2. Make result publication an explicit boundary

Second implementation PR: route existing successful-result recording and cache-reuse state
updates through clearly owned operations. Keep current transaction semantics first; audit
incremental saves, checkpoint writes, terminal writes and server-ledger acceptance for bypasses.
The sequential selection fix (0.34.0) is in place; build on it. Preserve current behavior with existing
[history reconciliation](../../python/tests/test_history_reconcile.py) and persistence coverage.

Note, 2026-11-04 (#800, item 16): every write to the metadata database now goes through a
named owner function, in `db/` (one module per table family) or
`persist/recording.rs`; callers keep their transactions, checks and error messages. Item 18
then split the rest of `db.rs` into the same `db/` families; `db/mod.rs` keeps the connection,
lock and transaction helpers.
`db/write_boundary_tests.rs` fails on an `INSERT`, `UPDATE`, `DELETE` or `REPLACE` literal in
non-test code anywhere else in `barca-core` (`db/schema/ddl.rs` migrations and the read-only `sql/`
aside). This is the boundary only: no write is ordered or guarded differently.

Follow with a separate behavior PR defining the reconciliation scope/order and guarded
publication. Add real controlled overlaps: newer completion first, newer observation reusing
old bytes, removal while old work runs, cache-only selection and restart between byte
finalization and metadata commit. The finite model is a design reference, not this test gate.

### 3. Establish stable execution inputs and byte lifetimes

Define the run-start source/configuration capture boundary and the ownership of resolved
artifact inputs. Exercise delayed first imports, a replacement worker after an edit and
helpers imported only downstream. Verify that recorded definition identity matches executed
code. Specify the environment boundary before claiming a complete snapshot.

Exercise an input path being replaced while a reader is delayed, cancellation during reads
and GC while inputs are pinned. Retain [receipt controls](../../python/tests/test_upload_snapshot_receipts.py)
but add reader isolation coverage. This changes guarantees and needs the runtime/manual
owners updated; it is not a file-move refactor. Conservative serialization within the defined
scope remains the fallback until concurrent versions are safe.

### 4. Introduce server-owned computation subscriptions

Only after guarded publication and stable inputs, acquire completed-result reuse or in-flight
ownership atomically for a complete resolved asset identity. Move the required execution
lifetime out of the originating run; do not simply share its pool pointer. Preserve run-local
accounting and separate logical satisfaction from physical execution. Independent CLI
processes remain outside this sharing scope.

Gate on [ownership acceptance](../../specs/behavior/current-state-and-execution.md#acceptance-and-evidence):
partial overlap, different identities, one/last requester cancellation, pending reconciliation,
owner death, retry races and restart. Existing
[concurrent CLI tests](../../python/tests/test_concurrent_processes.py) show coexistence, not
single-owner execution. Track this work with issue #654.

Note, 2026-11-04: [#796](https://github.com/barca-orc/barca/pull/796) (0.39.4) shipped the
incremental part of this step ahead of steps 2b and 3: a server-wide claim per asset step
([`claims.rs`](../../crates/barca-core/src/serve/claims.rs)) lets a run wait on another run's in-flight
step and take its artifact as a cache hit. Execution lifetime still belongs to the originating
run's pool. It is kept on under `serve` as experimental (#800, item 8). What remains of this
step, in #654: shared work surviving the cancellation of the run that started it
(INV-CANCEL-01), a handoff that waits for the owner's durable row (step 2b's guarded
publication), and an atomic decide-and-claim that closes the decide-to-lease race. Step 2b's
write boundary is in place (#800, item 16, below); its guarded publication and step 3 are
still not done.

### 5. Bound retained work and inspection

Once result durability has one boundary, introduce backpressure from persistence through
admission and partition discovery; release completed per-key coordinator/report state after
durable recording. Bound channels as well as worker count. Preserve explicit batch boundaries,
partial failures, full outputs and fan-in semantics. Measure cold bootstrap, full refresh,
slow commits and warm incremental execution separately against [performance](../../specs/behavior/performance.md).
This addresses issue #655; worker-capacity tests alone cannot establish it.

Inspection work (issue #656) can proceed independently after selection ownership is clear:
guard checks by evidence revision, project summaries/pages and avoid re-expanding the entire
domain for a small query. Validate invalidation during checking, out-of-order checks, missing
bytes, changing membership and page consistency. Change existing wire owners alongside any
new status representation.

## Decisions that should stay explicit

Structural step 1 and the first part of 2 are done (above). Steps 2b and 3 add guarantees and
must satisfy their scenario gates before activation; step 4 shipped early as experimental and
still owes its gates (#654). Lifecycle admission/recovery, invocation
deduplication, retention and resource enforcement should be separate behavior changes with
their own contract updates, rather than incidental changes during extraction.

The unanswered proposals in the [handoff](requirements-handoff.md) remain
unapproved. Retention version identity and remote deletion scope, direct-check deadlines,
invocation deduplication lifetime and dependency snapshot scope also remain open. Do not
implement a new error schema by interpreting this architecture map as approval.

Validation of this document checks navigation and evidence references only. No runtime
refactor, snapshot isolation, shared execution or performance improvement ships with it.

The [behavioral conformance series](behavior-conformance.md) records the first implementation
slices, exact obligation scenarios and remaining decision gates. Durable result-history writes
now share [one recording owner](../../crates/barca-db/src/recording.rs); receipt
eligibility and transaction lifetimes remain with their existing callers. This is a structural
boundary, not ordered current-state publication.
