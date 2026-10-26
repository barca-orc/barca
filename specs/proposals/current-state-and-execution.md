# Current state and execution ownership

Design revision: 1. Status: approved product behavior, not implemented.
Scope: current selection, validation status, pending reconciliation and shared asset work.
This records the user decisions made during the 0.33.0 audit. It does not prescribe a new
public field, database table, persistent worker pool or transport protocol. Terms:
[glossary](../../GLOSSARY.md). Implemented baseline:
[freshness](../freshness.md), [execution](../execution.md), [inspection](../inspection.md).

## Observation and selection ordering

A running run finishes against its resolved inputs. Observing a newer version is not an
implicit cancellation of existing work. Its result remains available to its original callers
and history. Explicit operator cancellation remains a separate action.

An observation revision needs a consistent order within its reconciliation scope. Input
values, their byte digests, existing opaque `observation_hash` values, wall clocks and worker
completion order do not supply that order. Returning to the same data can be a newer
observation while reusing an older artifact. Multi-source observations need a consistent
snapshot or a versioned validity check; comparing unrelated per-source numbers is insufficient.
The implementation must define that scope and order before concurrent versions are enabled.
Resolved input bytes also need a stable lifetime: a reusable artifact pathname that another
run can overwrite does not establish snapshot isolation. The observed concurrent-reader
limit in [upload receipts](../artifact-upload-receipts.md#concurrent-readers-remain-a-separate-boundary)
must be addressed before enabling concurrent versions; truthful upload receipts alone do not fix it.

- **INV-SELECT-ORDER-01:** Completion against an older observation cannot replace a current
  selection already established against a newer observation.
- **INV-MEMBER-ORDER-01:** Removing a key at a newer observation cannot be undone by an
  older computation completing; its result can remain in history without restoring membership.
- **INV-SNAPSHOT-01:** A running computation's resolved inputs do not change underneath it.
- **INV-COMMIT-01:** A committed selection, its freshness evidence, progress and summary
  updates agree after restart; cache-only selection changes follow the same rule.

| Event | Required effect |
| --- | --- |
| Observe revision 1; begin its computation | Capture the inputs and record the in-flight identity |
| Observe revisions 2 then 3 while 1 runs | Keep 1 running; replace unstarted automatic reconciliation for 2 with 3 |
| Revision 3 starts while 1 still runs | Permitted only with capacity and proven race-safe selection/ownership |
| Revision 3 completes before 1 | Select 3; record 1's later result in history without replacing 3 |
| Reuse an older artifact for revision 3 | Establish selection/evidence for 3; artifact age does not order observations |
| Remove a key while its older computation runs | Preserve the run's result/history, without reviving the key in current membership |
| Cannot safely run versions concurrently | Wait for 1, then reconcile latest pending revision 3 |

Pending automatic asset updates coalesce. Already-running work and distinct explicit task
invocations do not coalesce away. Captured requests for an older snapshot can still need its
result. Admission alone does not promise when all inputs have been resolved; the implementation
must state the resolution point rather than silently retarget running work.

## Sharing asset computations

Use one in-flight computation for matching asset identities: canonical node/partition,
complete resolved input identity, definition, declared environment and relevant execution
policy. Different identities remain independent. Do not deduplicate by node name alone.
Sensors are separate observations; this decision does not merge sensor polls or task actions.

- **INV-SHARE-01:** At most one owner executes a matching in-flight asset identity in the
  sharing scope; acquisition atomically checks completed results and registers ownership.
- **INV-CANCEL-01:** Detaching one run cannot cancel a computation another run still needs.
- **INV-TASK-ID-01:** Distinct explicit task invocations retain separate identities.
- **INV-RECOVERY-01:** Owner failure releases or retries ownership; it cannot strand subscribers
  or let all waiters independently start the same retry.

For requests `{a,b}` and `{b,c}` with matching input identities, only `b` is shared.
Different hashes for `b` are separate computations. Each run retains truthful progress:
shared work may satisfy two requests but is not two physical executions. Success is made
durable before publication to another run; replay must preserve that distinction.

Cancellation of the last subscriber can stop unneeded work, provided no reconciliation still
needs it. Subscription lifetime and execution lifetime differ from the originating run's
lifetime. The existing run-owned pool cannot be destroyed while serving surviving subscribers.
This does not approve keeping workers warm across otherwise unrelated runs.

The initial candidate sharing scope is server-owned execution. Independent CLI processes
and local execution consulting a server ledger require explicit cross-process ownership to
share computation. No global deduplication claim is approved without that mechanism.

## Inspection

Validation, execution and artifact availability are independent facts. An implementation may
return **checking** while it revalidates freshness. It must track which evidence is pending
and what counts are authoritative; it cannot reuse a stale answer indefinitely under that label.

- **INV-STATUS-01:** Pending/invalidated freshness evidence cannot be reported as confirmed fresh.
- **INV-STATUS-02:** Completion of an obsolete validation cannot overwrite a newer status result.
- **INV-STATUS-03:** Unknown counts and known counts are distinguishable. A missing artifact is
  an availability finding, not automatically a code/input freshness change.

A code edit can invalidate a logical node's evidence without allocating a stale object per
key. An unchanged observation can update poll history without invalidating asset freshness.
Selection and membership commits update summaries. External file deletion still requires
reconciliation; immediate exact availability without inspecting external changes is not promised.
Exact status wire fields, UI behavior, revalidation cadence and summary storage remain design
work. OpenAPI/CLI snapshots and the UI must change with implementation, not in this proposal.

## Bounded work and acceptance

- **INV-WINDOW-01:** Retained coordinator execution state is bounded by graph structure,
  admitted key pages, in-flight work and bounded buffers/samples; completed per-key state can
  be released after durable recording. Backpressure reaches key discovery and persistence.
- **INV-BATCH-ORDER-01:** Internal windows cannot silently change documented batch grouping,
  explicit group boundaries or per-key partial-failure behavior.

Cold bootstrap, full refresh, first validation and inspection must be measured separately
from warm one-key execution. Application output size, Python discovery and a real fan-in can
remain proportional to domain size. See [performance](../performance.md) for current gaps.

Required implementation evidence, not currently passing conformance claims:

| Obligation | Acceptance scenario |
| --- | --- |
| Ordered selection / snapshot | Complete revisions out of order, including a newer observation reusing old bytes and a removed key finishing late; restart between artifact publication and selection commit |
| Sharing / identity | Identical requests execute once; differing hashes execute independently; partial key overlap shares only the intersection |
| Cancellation / retry | One subscriber cancels, last subscriber cancels, owner dies, retry races, server restarts; surviving requests still complete correctly |
| Pending reconciliation | Observe 1, 2, 3 while 1 runs; verify already-started work survives and only the latest unstarted automatic version remains |
| Inspection | Invalidate during a check; complete checks out of order; delete an artifact; show truthful checking/counts/availability |
| Bounded work | Large cold/refresh domains and slow workers/commits stay within explicit windows; outputs, batches and recovery match a reference model |

The [TLA+ model](../models/README.md) checks a finite abstraction of ownership, ordered
publication and cancellation in conservative and concurrent modes. It is a safety design
check, not implementation conformance, a complete asset semantics model, or a performance test.
Issues [#654](https://github.com/barca-orc/barca/issues/654),
[#655](https://github.com/barca-orc/barca/issues/655), and
[#656](https://github.com/barca-orc/barca/issues/656) track the implementation gaps.
