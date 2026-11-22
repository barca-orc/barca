# Execution, batches and run ownership

## Scope and status

Specification version: 8. Status: implemented rules; shared in-flight asset steps between a server's runs are implemented
as experimental with three known gaps (Open questions and gaps); the rest of shared computation
is approved design only.
Scope: logical work, batch completion, run recovery, and process ownership.
Terms: [glossary](../../GLOSSARY.md). Python module/connection state is owned by [Python
runtime](python-runtime.md).
Wire fields remain owned by [OpenAPI](../interfaces/server-api.openapi.yaml),
[worker protocol](../interfaces/worker-protocol.md), and the [CLI
contract](../../crates/barca-cli/docs/contract.md).

## Entities and state

A run owns a request, progress and outcome. Logical steps identify canonical node/partition
work; one batch call can complete multiple steps, and one worker executes multiple calls. Runs
can be queued, active, terminal or diagnosed interrupted; exact wire states belong to the
interface contracts.

## Rules and transitions

### Requests and computations

A run has its own request, progress and outcome. A partitioned step identifies a node and
canonical partition key. A function call may execute several such steps in one batch.
Neither a partition key nor a logical step implies a separate Python process.

A scheduled downstream task can be up to date under tick rules; this is different from an
explicit task invocation. A sensor is polled as required, not served as a cached action.
Ordering inputs receive `None`; data dependencies receive results. Ordering-only failure
behavior follows the coordinator dependency rules, not ordinary data-failure propagation.

### Run lifecycle

| Event | Required transition/effect |
| --- | --- |
| Server accepts a request | Persist its queue entry before acknowledging its run identity |
| Admission/capacity permits execution | Begin work under that run's cancellation and progress ownership |
| Work succeeds | Commit completed results and progress; a final successful outcome does not invent computations for cache hits |
| A key fails | Preserve completed peers; skip data dependents that cannot proceed: only the same key of a dependent that reads it key for key (reported `upstream_key_failed`), the whole of one that reads every key (`upstream_failed`); the run goes on to later phases; finish the run with its real failures |
| A scheduled tick meets a paused node (`asset_pauses`, `barca pause`) | Do not compute it: a paused target is reported `skipped` with reason `paused`; a paused node upstream of a target is read as its latest result, and a target reading a paused node that has never been computed is `skipped`, reason `paused`. A request naming a paused node is refused (usage) unless it allows paused targets |
| Caller cancels | Stop owned work, persist truthful progress/outcome and reap owned helpers |
| Server drains/restarts | Apply queue requeue/interruption rules and reuse already committed successes |

A cancelled, interrupted, or infrastructure-error run cannot infer cache hits from planned
nodes with no recorded result: an interrupted call may have started without completing.
Run inspectors preserve recorded results and label dispatch counts as started, not successful.
The run inspector offers confirmed cancellation while queued or running and reports the
request separately from the eventual terminal state.

Wire and metadata status spellings are intentionally left to their owners; queued, live,
terminal and historical states are not interchangeable. See the [run
queue](../../crates/barca-server/src/queue.rs)
and [durability rules](cache-decisions.md#recording-progress).

### Batches and partial failure

Batch selection groups stale keys without changing their individual result identities.
Explicit selection narrows keys before grouping. A JSON array passed to `--partition` names
exact key combinations, not the cross-product of their dimension values. Prediction, execution,
refresh, and queue persistence preserve those combinations. Upstream nodes with fewer named
dimensions receive their projection; unrelated dimensions remain unrestricted. Reports include
`exact_keys` so a dimension summary cannot be mistaken for the selected combinations. Controls:
[`test_partition_selection.py`](../../python/tests/test_partition_selection.py),
`queue::tests::queued_exact_keys_do_not_widen_after_reload`, and the key-explorer browser tests.
 Group order and explicit `batches(...)`
boundaries must not depend on how a worker pool happens to divide streams. Packing chunks
into the same stream must not introduce dependencies between unrelated nodes or between
chunks of the same node: a call can span those chunks and waits for all its keys to be ready.
Declared input dependencies still govern readiness (#1100).

A table result (DuckDB, Arrow or polars) is split into one artifact per key whose rows are
sorted by every column. The artifact bytes of a key depend only on its rows and schema: not
on the order the step produced them, on the number of files the writer used, or on the
worker's DuckDB settings, which the split leaves as it found them. Values that compare equal
but differ in representation are not normalized; that is the step's job.

The durable unit remains a key's result, not the entire batch. Failures are reported through
existing exit codes and per-key history. `Failed(reason)` expresses a key-level failure;
it does not turn an invalid/missing batch result into a success.

### Completion, cancellation and restart

Artifacts are finalized before metadata refers to completed results. Checkpoint and terminal
writes preserve successful work across interruption according to [cache
decisions](cache-decisions.md)
and [upload receipts](../interfaces/artifact-upload-receipts.md). Completion recording is at
least once;
replay must not duplicate the same run/key result. This is not an exactly-once promise for
user side effects or external sinks.

An explicit cancellation stops that run's work and reaps its owned helpers. A step that
finished before the cancellation stays recorded even when its upload to a store was abandoned;
the upload is kept pending ([upload receipts](../interfaces/artifact-upload-receipts.md), #1002). A server records
why it cancelled a run (`runs.cancellation`): a request or its own stop, the token of the
request, and whether the run had started; one cancelled while it waited in the queue executed
nothing and names the runs it waited behind, among them one doing the same work. A server does
not deduplicate identical requests: each is a run, and the later one waits in the queue or
shares the earlier one's in-flight asset steps. A server drain
stops admission and waits; its configured deadline/requeue rules determine interrupted work.
Retry after a user exception may use the same worker. Crash, timeout, `sys.exit()` and failed
import retries use a replacement worker. Backoff is enforced, not merely declared.
These are physical step retries under the declared budget, not new invocation attempts.
The approved invocation-recovery intent and its open physical task-retry policy are separated
in [operations](operations.md#invocation-retry-and-snapshots).

`parallel()` has its own lifetime: a suspended parent can require a replacement worker.
Ordinary pool bounds must not be advertised as a bound on all nested parallel process counts.
Remote transfer uses a separate helper. Worker pools are currently run-owned.

### A command beside a server

Approved 2026-11-19 (#1056 track A, #1054) and implemented. One process owns a project
environment's metadata database while `barca serve` runs on it: the server. A `barca` command
started in that project does not open the database beside it; it finds the server and sends its
work there.

The server announces itself with a Unix socket beside the environment's metadata database
([layout](../interfaces/dot-barca-layout.md): `.barca/serve.sock`, or
`.barca/envs/<env>/serve.sock`), on which it serves the same HTTP API as on its TCP address
([OpenAPI](../interfaces/server-api.openapi.yaml), `x-project-socket`). Only the server that
holds the environment's `serve.lock` opens it: a second server on the same environment (a
read-only one beside a writer) serves on TCP only. The socket is removed when the server exits.
Access is by file permission and needs no token ([authentication](server-authentication.md)).
A path too long for a Unix socket (107 bytes on Linux, 103 on macOS and the BSDs) leaves the
server without a socket: it says so in a warning at startup and serves on TCP only.

A command checks for the socket after entering the project root, for the environment it uses
(`--env`, `BARCA_ENV`, else barca.toml's `default_env`, else `default`). No socket file: nothing
else happens. The check is one `stat` when the environment is named on the command line or in
`BARCA_ENV`, and two otherwise (`.barca/envs`, then the socket); barca.toml is read for
`default_env` only in a project that has a named environment. A socket nothing listens on (a
killed server's) refuses the connection and is ignored without a word. A socket that accepts
but does not answer `GET /health` within 5 seconds is ignored with a warning on stderr. In
both cases the command runs on this machine as if there were no server.

With a live socket, and neither `--local` nor `--server` on the command line:

| Command | Where it runs |
| --- | --- |
| `status`, `history`, `stats`, `logs`, `queue`, `pause`, `resume`, `pin`, `unpin`, `cancel` | answered by, or sent to, the server over the socket, as with `--server` |
| `sql` | the server resolves the views and answers a query over barca's own tables; a query over results runs in the command's process, over the artifacts in this project |
| `get`, `run` | submitted to the server's run queue, as with `--server`: the server executes its deployed files, and the command follows the run and prints its result |
| `gc` | run by the server (`POST /gc`), between its own writes |
| `state restore`, `state reset` | refused (exit 3) while the server holds the environment, naming the server and what to do; this does not depend on the socket |
| `list`, `plan`, `doctor`, `docs`, `tokens`, `state backups`, `history --reconcile`, `serve` | on this machine, as without a server |

The command prints once on stderr that the project's server is used. A command that names no
file uses every file the server has deployed; a file the server has not deployed is a usage
error naming the deployed files. `--local` runs any of these on this machine as before the
socket existed (and then waits on the database lock like any second process); `--server URL`
names a server explicitly. The socket takes precedence over a configured server (`BARCA_SERVER`,
`[server] url`): the server in this project owns this project's database.

A `--read-only` server opens the socket as any other and answers on it exactly as on its TCP
address: the reads are answered, and every write (`get`, `run`, `gc`, `pause`, `resume`, `pin`,
`unpin`, `cancel`, `queue pause`) is refused with its read-only error (403, exit 2), whose
remediation names `--local`. The command does not fall back to this machine on its own.

There is no route for taking a metadata backup on request: no command takes one on demand, and
the server takes its own (RFC-0015).

#### The database lock

The cross-process lock on a metadata database is `<db>.lock`, held for each operation. A
process that waits 60 seconds for it fails with an infrastructure error that names the process
holding it at that moment (pid and command line, read from the kernel's lock table; Linux
only, and "could not be named" elsewhere) and points at the server. The table is read up to
five times, because one read can skip a lock that is held while others come and go, and a
process a matching line names is the holder only if it has the lock file open (the table
identifies a file by inode, which another filesystem repeats). A lock found free during the
look is taken, and the operation proceeds. A holder the table does not name (a process outside
this container, or on another machine that shares the directory) is reported as that, not as
"could not be named". Nothing is written to disk to make the holder known. `BARCA_TRACE_TIMING` prints, at the end of a run, how many of the
process's lock acquisitions had to wait, their total wait and the longest.

### Process storage overrides

`BARCA_REMOTE=off` selects local artifacts and local history, overriding configured remote
locations/options without rewriting historical rows. Recorded remote results are not fetched
or inspected while off. A matching recorded local result may be reused; otherwise it is
computed locally. Empty/unset values retain configuration and other values are usage errors.
The selected local environment is unchanged. This does not block network I/O in user code.
The removed `BARCA_STATE=off` spelling and shared-history pull are not current alternatives.

Run lifecycle views use `pending`, `running`, `success`, `failed`, `cancelled` and
`interrupted`. A failure outside a step still has status `failed`; its envelope preserves
the failure category and exit code. Every terminal non-success carries a reason; legacy
records without evidence state that limitation. Accepted pending requests are inspectable
in history before execution starts, including after restart. Exact envelopes and time
representations are owned by the HTTP and CLI interface specifications.

## Invariants

- **INV-TASK-01:** Distinct explicit task invocations execute separately; an asset cache hit
  does not turn a requested task into a cached action.

- **INV-POOL-01:** Ordinary partition execution reuses the configured worker pool across the
  run's phases. Processes are started on demand, not per partition.

- **INV-PROGRESS-01:** Completed work, failures, cache reuse and skipped dependents must be
  accounted for in the run; cached references do not create fictitious computations.

- **INV-BATCH-01:** Successful keys in a partially failed batch retain their results and may
  advance downstream work; one key's failure does not discard its successful peers.

- **INV-BATCH-02:** A later cache-aware request reuses matching successful keys and retries
  keys still needing work. Explicit refresh can intentionally recompute successes.

- **INV-BATCH-03:** A raised call fails all unfinished keys in that call. Worker death cannot
  silently count uncompleted keys as successful.

- **INV-OWNER-01:** While a server holds an environment and its socket answers, a `get`, `run`
  or `gc` started in that project without `--local` or `--server` is executed by the server; the
  command's process writes nothing to that environment's metadata database.

- **INV-OWNER-02:** A project with no server pays no more than two `stat` calls for the check,
  and a socket that does not answer never fails a command: it runs on this machine.

## Failures and recovery

A failed key preserves successful peers and blocks applicable data dependents: the same key of a
dependent on exactly the same dimensions that reads it as a plain input, down the chain, and the
whole of a dependent that reads every key (a `collect()` fan-in, an unpartitioned node, other
dimensions). `barca status` reports a node with keys blocked this way `blocked`. A step that
fails whole stops a single-target run at its phase, as before. Cancellation
stops run-owned work and persists completed progress. Restart follows durable queue and
owner-diagnosis rules; it does not establish exactly-once external side effects. The completion
and lifecycle rules below define the precise existing behavior.

## Acceptance and evidence

Exercise explicit tasks, cached assets, per-key partial batch failure, worker death/retry,
cancellation, durable queue acknowledgement and drain/restart. Assert actual executions
separately from cache hits and preserve completed peers.

Conformance: [task CLI](../../python/tests/test_task_logs.py),
[scheduler](../../python/tests/test_serve_schedule.py),
[coordinator](../../crates/barca-engine/src/coordinator.rs),
[worker ownership](../../crates/barca-engine/src/io_loop.rs),
[progress](../../python/tests/test_progress_counts.py).

Conformance: [batched partitions](../../python/tests/test_batched_partitions.py),
[stable partition artifacts](../../python/tests/test_partition_artifact_stability.py),
including `test_a_call_that_raises_fails_every_key_and_a_retry_reruns_only_failed_keys`
and `test_worker_death_retries_every_unfinished_key_of_the_call`.
Grouping syntax and experimental status:
[RFC-0013](../../site/src/content/docs/rfcs/0013-batched-partitions.md).

Conformance: [incremental persistence](../../python/tests/test_incremental_persist.py),
[run queue/restart](../../python/tests/test_serve_queue.py),
[runtime limits](../../python/tests/test_serve_runtime_limits.py),
[worker failures](../../python/tests/test_worker_survives_exceptions.py),
[helper shutdown](../../crates/barca-proc/src/helper_proc.rs).

Conformance (cancellation records, #925): [token, reason and a queued cancellation](../../crates/barca-server/tests/auth.rs)
(`a_run_records_its_requesting_token_and_reason_and_who_cancelled_it`) and
[the same work already running](../../crates/barca-server/src/runs/tests.rs)
(`a_queued_cancellation_names_the_same_work_already_running`).

Conformance (a command beside a server): [project socket](../../python/tests/test_project_socket.py)
(the socket is created owner-only and removed; a killed server's is ignored and replaced; every
routed command reaches the server; `--local` does not; `gc` beside a recording run is run by the
server; restore and reset refuse; a token-mode server asks no token on it; a read-only server
refuses writes; named environments; a subdirectory; a socket that accepts and never answers),
[the socket file](../../crates/barca-server/src/project_socket.rs), [`POST /gc`](../../crates/barca-server/tests/openapi.rs)
and [the lock's holder and wait count](../../crates/barca-db/src/db/mod.rs). Not exercised: the
path-length warning end to end (unit-tested only), a second local user, and macOS.

Conformance: [process storage overrides](../../python/tests/test_process_storage_overrides.py).
Configuration details: [remote manual](../../crates/barca-cli/docs/remote.md).

## Open questions and gaps

Runs of one `barca serve` share the asset steps they have in flight
([#654](https://github.com/barca-orc/barca/issues/654), [`claims.rs`](../../crates/barca-service/src/serve/claims.rs)):
a step is claimed under its display id (partition key included) and run hash when one of the
run's workers leases it, and another run that reaches the same step while the claim is held
waits instead of executing it, then reports it `cached` and records no row of its own. A run
that was already going when the step finished and reaches it afterwards takes that result the
same way (the last 4096 finished steps are kept for this); a run that starts later looks it up
in the cache. Tasks,
sensors and `parallel()` branches are never shared; different run hashes and keys the runs do
not have in common are independent. A claim ends without a result when its step fails an
attempt, loses its worker, or its run ends or is cancelled; one waiting run then claims and
executes the step. Conformance: [shared steps](../../python/tests/test_serve_shared_steps.py),
[claims](../../crates/barca-service/src/serve/claims.rs).

Known gaps against [the approved design](current-state-and-execution.md): cancelling the run that
owns a shared step stops it, and the waiting run executes it again from the start
(INV-CANCEL-01 asks that the computation survive); the owner's result is handed to waiting runs
when its worker reports it, before the owner's row is durable; and a run that decided a step
before another run finished it, and leases it after that run's claim ended, executes it again.
Independent CLI processes, and local runs against a server's ledger, share nothing.

A command beside a server: the lock itself is unchanged (#1056 A7, A8): `--local` beside a busy
server, and `history --reconcile`, `doctor` and `state backups`, still take turns on it from
outside. A command a step starts in the project (through `barca.get()`, say) is not sent to the
server: it runs in the step's process, since on the server it would wait for a run slot its own
caller holds.

Full bootstrap and validation still retain work proportional to partition count:
[#655](https://github.com/barca-orc/barca/issues/655). Bounded scheduling is an accepted target,
not a current whole-engine memory guarantee. Performance workload scope and budgets are
owned by [the performance contract](performance.md).

Conformance (run lifecycle, #943): [CLI, HTTP, persistence and restart](../../python/tests/test_run_status_contract.py).
