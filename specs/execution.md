# Execution, batches and run ownership

Specification version: 1. Status: implemented rules; shared computation is approved design only.
Scope: logical work, batch completion, run recovery, and process ownership.
Terms: [glossary](../GLOSSARY.md). Python module/connection state is owned by [Python runtime](python-runtime.md).
Wire fields remain owned by [OpenAPI](server-api.openapi.yaml),
[worker protocol](worker-protocol.md), and the [CLI contract](../crates/barca-cli/docs/contract.md).

## Requests and computations

A run has its own request, progress and outcome. A partitioned step identifies a node and
canonical partition key. A function call may execute several such steps in one batch.
Neither a partition key nor a logical step implies a separate Python process.

- **INV-TASK-01:** Distinct explicit task invocations execute separately; an asset cache hit
  does not turn a requested task into a cached action.
- **INV-POOL-01:** Ordinary partition execution reuses the configured worker pool across the
  run's phases. Processes are started on demand, not per partition.
- **INV-PROGRESS-01:** Completed work, failures, cache reuse and skipped dependents must be
  accounted for in the run; cached references do not create fictitious computations.

A scheduled downstream task can be up to date under tick rules; this is different from an
explicit task invocation. A sensor is polled as required, not served as a cached action.
Ordering inputs receive `None`; data dependencies receive results. Ordering-only failure
behavior follows the coordinator dependency rules, not ordinary data-failure propagation.

Conformance: [task CLI](../python/tests/test_task_logs.py),
[scheduler](../python/tests/test_serve_schedule.py),
[coordinator](../crates/barca-core/src/coordinator.rs),
[worker ownership](../crates/barca-core/src/io_loop.rs),
[progress](../python/tests/test_progress_counts.py).

## Run lifecycle

| Event | Required transition/effect |
| --- | --- |
| Server accepts a request | Persist its queue entry before acknowledging its run identity |
| Admission/capacity permits execution | Begin work under that run's cancellation and progress ownership |
| Work succeeds | Commit completed results and progress; a final successful outcome does not invent computations for cache hits |
| A key fails | Preserve completed peers; skip data dependents that cannot proceed; finish the run with its real failures |
| Caller cancels | Stop owned work, persist truthful progress/outcome and reap owned helpers |
| Server drains/restarts | Apply queue requeue/interruption rules and reuse already committed successes |

Wire and metadata status spellings are intentionally left to their owners; queued, live,
terminal and historical states are not interchangeable. See the [run queue](../crates/barca-server/src/queue.rs)
and [durability rules](cache-decisions.md#recording-progress).

## Batches and partial failure

Batch selection groups stale keys without changing their individual result identities.
Explicit selection narrows keys before grouping. Group order and explicit `batches(...)`
boundaries must not depend on how a worker pool happens to divide streams.

- **INV-BATCH-01:** Successful keys in a partially failed batch retain their results and may
  advance downstream work; one key's failure does not discard its successful peers.
- **INV-BATCH-02:** A later cache-aware request reuses matching successful keys and retries
  keys still needing work. Explicit refresh can intentionally recompute successes.
- **INV-BATCH-03:** A raised call fails all unfinished keys in that call. Worker death cannot
  silently count uncompleted keys as successful.

The durable unit remains a key's result, not the entire batch. Failures are reported through
existing exit codes and per-key history. `Failed(reason)` expresses a key-level failure;
it does not turn an invalid/missing batch result into a success.

Conformance: [batched partitions](../python/tests/test_batched_partitions.py),
including `test_a_call_that_raises_fails_every_key_and_a_retry_reruns_only_failed_keys`
and `test_worker_death_retries_every_unfinished_key_of_the_call`.
Grouping syntax and experimental status: [RFC-0013](../site/src/content/docs/rfcs/0013-batched-partitions.md).

## Completion, cancellation and restart

Artifacts are finalized before metadata refers to completed results. Checkpoint and terminal
writes preserve successful work across interruption according to [cache decisions](cache-decisions.md)
and [upload receipts](artifact-upload-receipts.md). Completion recording is at least once;
replay must not duplicate the same run/key result. This is not an exactly-once promise for
user side effects or external sinks.

An explicit cancellation stops that run's work and reaps its owned helpers. A server drain
stops admission and waits; its configured deadline/requeue rules determine interrupted work.
Retry after a user exception may use the same worker. Crash, timeout, `sys.exit()` and failed
import retries use a replacement worker. Backoff is enforced, not merely declared.

`parallel()` has its own lifetime: a suspended parent can require a replacement worker.
Ordinary pool bounds must not be advertised as a bound on all nested parallel process counts.
Remote transfer uses a separate helper. Worker pools are currently run-owned.

Conformance: [incremental persistence](../python/tests/test_incremental_persist.py),
[run queue/restart](../python/tests/test_serve_queue.py),
[runtime limits](../python/tests/test_serve_runtime_limits.py),
[worker failures](../python/tests/test_worker_survives_exceptions.py),
[helper shutdown](../crates/barca-core/src/helper_proc.rs).

## Process storage overrides

`BARCA_REMOTE=off` selects local artifacts and local history, overriding configured remote
locations/options without rewriting historical rows. Recorded remote results are not fetched
or inspected while off. A matching recorded local result may be reused; otherwise it is
computed locally. Empty/unset values retain configuration and other values are usage errors.
The selected local environment is unchanged. This does not block network I/O in user code.
The removed `BARCA_STATE=off` spelling and shared-history pull are not current alternatives.

Conformance: [process storage overrides](../python/tests/test_process_storage_overrides.py).
Configuration details: [remote manual](../crates/barca-cli/docs/remote.md).

## Limits and accepted changes

Matching overlapping asset requests can currently compute the same result twice:
[#654](https://github.com/barca-orc/barca/issues/654). No global in-flight sharing is promised
by this implemented contract. [The approved design](proposals/current-state-and-execution.md)
defines the sharing, cancellation and version-order requirements for the replacement.

Full bootstrap and validation still retain work proportional to partition count:
[#655](https://github.com/barca-orc/barca/issues/655). Bounded scheduling is an accepted target,
not a current whole-engine memory guarantee. Performance workload scope and budgets are
owned by [the performance contract](performance.md).
