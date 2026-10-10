# Reliability failure scenarios for discussion

Status: **discussion draft**, 2026-10-10. Document version: **1**.
Scope: the [ownership, durability and result-identity contracts](reliability-contracts.md),
audited against main `96022e0` / release 0.22.0. This documents scenarios and
unanswered questions, not additional runtime guarantees or newly selected policies.
Explicit user decisions are identified below; other proposals remain undecided.

The catalog covers the failure modes identified in this review. It cannot
enumerate every provider failure or arbitrary user-code side effect. Extend it
when a new boundary or reproduction is found. References to existing tests mean
coverage of those particular cases, not proof of every possible interleaving.

Evidence labels:

- **Reproduced open defect:** a controlled failure exists; scope is explicit.
- **Covered regression:** existing tests exercise the stated boundary in shipped
  code. Older broken behavior is historical, not a claim that the defect persists.
- **Known limitation:** current implementation or accepted workflow permits the
  condition; it is not automatically a bug.
- **Policy example:** hypothetical timeline illustrating a decision. No runtime
  reproduction or approved answer is implied.
- **Open acceptance case:** needed for a proposed feature/fix; not yet proven.

## Ownership: which operation owns which bytes and resources?

Accepted user direction, 2026-10-10: publication must be idempotent. Local and
server execution may update the current result in a shared environment; changed
remote state since the writer's starting observation requires a conflict warning
and explicit `--force`. Stale-cache reads automatically synchronize before use.
Each receipt must match its own uploaded bytes; force is not an integrity bypass.
Assets are assumed pure: their definition and tracked inputs determine the logical
version (`run_hash`) and expected result. Existing distinct hash-addressed files
provide versioning; sensors/other tracked inputs bring changing external observations
into dependent hashes. Reversion builds on these retained computation versions.
Same-hash outputs that differ are purity violations or integrity defects, not
ordinary additional byte versions. Conflict/revert command details, race handling,
saved-version selectors and retention remain to be specified. This discussion
does not require archiving every execution's bytes or promise recovery of bytes
replaced by an intentional same-hash override.

### O1. Two refreshes race with one upload

**Reproduced open defect, primitive scope; #381.** Run A serializes JSON `11`
to a computation-addressed path. A's actual upload copies `11` to the remote
destination. Before A computes its receipt, run B atomically replaces the local
path with JSON `22`. A reopens the path and reports the checksum of `22`.
The remote object is `11`, while the successful receipt describes `22`.

Under the accepted pure-asset model, different values at the same computation
hash violate the assumption. That does not excuse a false receipt: the transfer
must describe its exact uploaded bytes even when a user violates purity or
intentionally forces a conflict.

This can make later validation reject the bytes or make metadata misdescribe
the result. The proof is a synchronized primitive reproduction using real
serialization/storage/transfer code, not a complete overlapping CLI/HTTP run.
Full integration, frequency and deployed-provider impact remain unverified.
Evidence and acceptance: [#381](https://github.com/barca-orc/barca/issues/381).

Required engineering investigation: also replace the path **before** the upload
opens it. Hashing before upload alone does not bind both operations to the same
snapshot. Distinguish a truthful upload receipt from preserving old remote bytes
forever; a later successful refresh may intentionally overwrite that destination.

### O2. A downstream step reads another run's replacement

**Open acceptance case; #381 investigation, separate defect if demonstrated.**
A producer in run A returns `11`; its consumer has not opened the artifact yet.
Run B refreshes the same computation and writes `22` to the shared local path.
A's consumer opens that path. Could it observe `22` while A's producer outcome
records `11`? A post-upload checksum fix would not necessarily address this.

Reproduce using actual overlapping execution and controlled open boundaries;
inspect whether current loading/LRU behavior prevents the hypothesized read.
No full-runtime failure is claimed. Under purity, both executions should produce
equivalent values, so per-run byte archives are not required by this example.
Investigate wrong checksum/consumer behavior under purity violations without
silently adding general snapshot isolation. Define detection and warning/force
behavior separately from the exact-byte receipt requirement.

### O3. Cancellation lands inside temporary-file creation or deletion

**Covered regression.** A stage is created, but a signal arrives before its name
is registered; alternatively, cancellation arrives while unlinking/unregistering
it. Losing ownership in either interval leaves a partial file behind. Existing
signal coordination keeps the path tracked through acquisition and removal.

Ordinary finalization owns one exact path. Exiting-helper cleanup owns all its
registered stages, including concurrent operations; it must preserve unowned
files and another process's stages. Evidence:
[staged-file plan](staged-file-finalization.md),
[17 staging regressions](../python/tests/test_transfer.py).

### O4. SIGKILL bypasses cleanup, then an orphan collector runs

**Known limitation plus policy example; #83.** A helper creates a stage and is
killed without unwinding. Later, another run is actively writing a similar stage.
A directory-wide age/pattern sweep cannot infer ownership safely just from its
filename or age. Deleting the active writer's stage would turn cleanup into data
loss.

Graceful cleanup coverage does not guarantee cleanup after SIGKILL or power loss.
No new sweep is proposed. Any future collection policy needs an explicit
ownership/liveness rule and reader/writer tests, rather than assuming every
matching temporary file is abandoned.

### O5. SQL setup survives, orchestration views must not leak

**Accepted workflow plus covered regression.** Step A imports a module once and
creates a DuckDB macro; it also receives a temporary input view. Its returned
lazy relation must materialize while that view and connection remain alive.
Step A then succeeds or fails. Step B in the same worker should retain the macro
but must not inherit A's orchestration input binding.

Closing the connection per step loses accepted setup; dropping a view too early
breaks lazy output. User-created catalog collisions are not isolated or restored.
Evidence: [accepted lifetime](duckdb-step-isolation.md),
[worker regressions](../python/tests/test_duckdb_connection.py). No new setup API
or per-step isolation is assumed.

### O6. A delayed writer or retry would replace a newer published result

**Accepted policy; open implementation acceptance.** Local execution observes
published `11`, computes `22`, and starts publication. Another execution publishes
`33`. The local publication or delayed retry would replace `33` with `22`.

This changed-baseline publication is a conflict: warn and require `--force`.
An intentional forced publication may make `22` current; an ordinary retry must
not silently do so. If all these values use the same computation hash, they
violate asset purity, and forcing replacement does not promise preservation of
the previous same-hash bytes. Different hashes already name separate computation
versions. Test the lost-ACK
case as well as two independent runs. Idempotent retry recognition, atomicity at
the conflict-check/write boundary and backend support remain engineering work;
a read-then-write check alone must not be advertised as eliminating every race.
No operation-ID API or distributed locking mechanism is selected here.

### O7. The server has an older cached copy of a valid published result

**Accepted policy; open cross-machine acceptance.** The server caches `11` from
an older computation version. Local execution publishes `22` under a changed
hash with matching metadata/checksum. Before the server
consumes that result again, it obtains the current published identity, detects
its stale cache, fetches the selected bytes and verifies them. This normal read
requires no `--force`. Checking local bytes against an old expected checksum is
insufficient to establish freshness; acquiring current metadata is part of the
contract. If selected metadata and downloaded bytes disagree, that is an integrity
failure rather than an ordinary stale cache or permission to force consumption.

### O8. A valid publication is later found to be semantically wrong

**Accepted versioning direction; open implementation acceptance.** Published
computation version H1 contains `11`. A later version H2 contains `22`; its bytes
and receipts agree, but a user discovers the result is wrong for their workload.
They must be able to revert to the earlier computation version. H1 and H2 already
have different artifact paths; reuse those retained files and their history.
This is not a requirement to archive every refresh of H1. If someone forces
different bytes into H1, recovery of its original bytes is not promised by the
pure-asset model. Reversion needs retained bytes and identity/metadata, not merely
a history row pointing to a deleted or manually replaced artifact.

Specify how reverting changes the current selection and interacts with downstream
cache/lineage, concurrent publication and force. The revert command, retention
window, whether reversion creates a new publication record, and saved-handle/set
semantics remain open. Do not assume indefinite retention or copy every artifact
when unchanged data can be referenced safely.

## Durability: what happened before the crash or lost acknowledgement?

### D1. Upload queued, process dies

**Covered regression.** A worker finishes and queues an upload. The upload is
still blocked when the coordinator is killed. Local output exists, but another
machine has no confirmed reusable artifact. Queued work must not become a
successful remote materialization row. Also hold upload A while upload B finishes:
B's confirmed result should not be hidden by A's position in the queue.

Evidence: [transfer regressions](../crates/barca-core/src/transfer.rs),
[incremental integration](../python/tests/test_incremental_persist.py).

### D2. Upload acknowledged, local commit not yet complete

**Known limitation, with transactional retry coverage.** Bytes reached storage, but their
receipt is still queued or a database batch is failing. SIGKILL now may leave an
unreferenced remote object and no reusable history row. Existence of bytes is not
evidence of a committed result; graceful terminal persistence is a fallback,
but cannot run after SIGKILL.

Evidence: [transactional recorder](committed-progress-recorder.md),
[persistence regressions](../crates/barca-core/src/persist.rs). Those tests prove
rollback/retry, not a SIGKILL at every precommit instruction. Object adoption or
orphan GC is separate policy; do not reconstruct success from filenames alone.

### D3. A batch partially writes, then retries

**Covered regression.** A batch contains outcomes A and B. An actual constraint
fails B's insertion or the counter update. Without transactionality, A could
appear durable while counters disagree; replay could duplicate it. Shipped
behavior rolls back the batch, retains it and retries with run/node deduplication,
including when no new worker result arrives.

Evidence: [persistence regressions](../crates/barca-core/src/persist.rs),
[incremental integration](../python/tests/test_incremental_persist.py).

### D4. Local commit succeeds, shared checkpoint has not published

**Known recovery window plus policy example; #243.** A result commits locally
just after a successful shared checkpoint. Its machine dies before the next
checkpoint. Another machine can recover only the earlier acknowledged snapshot.
During a storage outage, this gap can exceed a minute even if timers keep firing.

The healthy-operation sixty-second cadence is approved; an unconditional
one-minute loss bound is not. Evidence:
[real minute/crash/fresh-root recovery](../python/tests/test_minute_checkpoints.py),
[outage and generation regressions](../crates/barca-core/src/persist.rs).
The real-minute test kills after two rows have been published and proves their
reuse plus execution of remaining work; it does not reproduce every possible
post-checkpoint local-commit/prepublication kill interval in this example.
Discuss how prominently to expose recovery lag, not whether a timer can guarantee
unavailable storage succeeds.

### D5. More progress arrives while a snapshot is uploading

**Covered regression.** A checkpoint snapshots committed rows. More outcomes
arrive while upload is held. Treating upload success as covering those newer
outcomes would incorrectly clear dirty progress. The later outcomes must commit
after this recorder's push and remain eligible for a later publication. Separately,
another local process can commit while that upload is in flight, requiring the
shared-push path to detect local changes and reconcile/publish again. Clean ticks
skip work only relative to this recorder's committed generation.

Evidence: [queued-upload/no-overlap and clean-tick tests](../crates/barca-core/src/persist.rs).
The shared-push local-change and carry checks in the same module cover the
separate concurrent-database-write boundary.

### D6. Remote write succeeds, acknowledgement is lost

**Covered regression.** The helper writes shared history, then the response is
lost. The coordinator cannot know whether the remote write happened. Calling the
object absent or blindly overwriting it can erase a concurrent run. Existing
token/conflict/carry handling retains prior knowledge and reconciles history.
Also cover receiving an ACK and then failing local bookkeeping: the new token
must be retained before that later failure.

Evidence: [real-helper persistence tests](../crates/barca-core/src/persist.rs),
[post-ACK and admission tests](../crates/barca-core/src/state_sync.rs),
[carry tests](../crates/barca-core/src/state_carry.rs). Metadata conflict recovery
does not by itself isolate concurrent artifact bytes; see O1/O2.

### D7. Cancellation correction meets an older successful snapshot

**Covered regression.** A run is cancelled after some durable work. Publishing
its correction is interrupted; a subsequent pull encounters older shared state
or unrelated successful runs. Losing the correction resurrects success; applying
it without matching run/owner identity can cancel someone else's run.

Existing identity checks and carry preserve the correction and unrelated history;
a later successful push publishes it. Evidence:
[cancellation integration](../python/tests/test_remote_cancel.py),
[carry regressions](../crates/barca-core/src/state_carry.rs).

### D8. Deadline fires while database replacement is underway

**Covered admission boundary and known limit.** A checkpoint waits for a lock:
cancellation before admission should leave the DB unchanged. After admission,
WAL/swap/migration work starts. Aborting in the middle merely to meet a timeout
could damage history. Existing atomic work finishes safely; an admitted operation
or filesystem syscall can exceed the nominal budget.

Evidence: [checkpoint admission](checkpoint-admission.md),
[state-sync tests](../crates/barca-core/src/state_sync.rs),
[database crash/copy tests](../crates/barca-core/src/db.rs). No hard real-time
shutdown guarantee is implied.

### D9. Terminal status writes, but one required step row fails

**Covered regression.** Computation completes. A failed insertion would leave
history claiming completion without its outcomes if terminal status committed
separately. Shipped terminal persistence commits required rows/status/counts
together; errors roll back and cannot masquerade as a durable success.

Evidence: [terminal persistence integration](../python/tests/test_terminal_persist.py),
[transaction/owner tests](../crates/barca-core/src/persist.rs).

### D10. Terminal rows survive; captured logs or full diagnostic provenance do not

**Known boundary and open acceptance; #319.** Required terminal rows commit,
then the separate captured-log write fails or the process dies. A restarted UI
can have history without the complete diagnostic record. Existing row atomicity
does not promise atomic outcome-plus-log persistence or full stored provenance.
Tracebacks included in required failed-step rows already participate in the
terminal transaction. The gap here is separate captured logs and broader
diagnostic/outcome completeness, not loss of those committed row fields.

Evidence: [terminal plan's explicit exclusion](terminal-ledger-commit.md),
[#319](https://github.com/barca-orc/barca/issues/319). Specify what diagnostics
must survive together, write-failure behavior and schema compatibility before
implementing full outcomes.

### D11. User work succeeds locally; final remote publication fails

**Policy example; #319/#243.** A computation and local terminal transaction
succeed. Publishing shared metadata fails. On this machine, output and history
exist; a different machine cannot yet recover the same completed state.

Should the user see a failed run, a completed run with a recovery warning, or
separate execution/durability indicators? What should the CLI exit code mean?
This does not imply changing today's checkpoint versus terminal error behavior.
The decision must distinguish failed user work from successful work whose remote
recovery state is incomplete. No answer is selected here.

### D12. Server accepts a run; restart or request retry loses the live handle

**Open acceptance and policy example; #319.** A server returns an accepted-run
handle, but durable allocation/provenance is not yet complete when it restarts.
Alternatively, acceptance succeeds but the HTTP response is lost and the caller
retries, potentially requesting two runs. A future acceptance-time durable ID
should have defined restart behavior; it does not automatically provide request
idempotency or exactly-once user side effects.

Evidence/owner: [#319](https://github.com/barca-orc/barca/issues/319),
[current handle/DB-ID model](../crates/barca-server/src/state.rs).
Scope retry identity separately; do not assume a new idempotency API or promise
exactly-once tasks merely from allocating a durable run ID.

### D13. UI hears completion before commit, or misses events while disconnected

**Known event-delivery limits plus open acceptance; #318/#319.** As a hypothetical
commit-boundary case, a completion event reaches a browser, but durable
persistence fails immediately afterward. Or the browser disconnects, its backlog
expires, and it reconnects after the server restarts. Showing a stream event as
proof of durable completion can contradict history; numbering events alone does
not make their replay durable or indefinite.

Current source retains an unbounded, process-memory backlog; a live subscriber
can lag beyond the broadcast channel's capacity, and the handler discards lag
errors. This is a code-backed limitation, not a newly executed lag reproduction.
Sources: [run channels](../crates/barca-server/src/state.rs),
[SSE handler](../crates/barca-server/src/handlers.rs). A same-process subscription
backlog is distinct from replay after server restart.

Define completion-event meaning, event/run identity, ordering, duplicate handling,
lag recovery, replay limits and the response when replay is unavailable. Consider
resynchronizing from durable state. No replay-storage policy is selected.
Owners: [#318](https://github.com/barca-orc/barca/issues/318),
[#319](https://github.com/barca-orc/barca/issues/319).

### D14. Storage stays down while outcomes accumulate

**Known resource limit and open stress acceptance.** Many partitions complete
while DB writes fail or publication blocks. Retaining work prevents silent loss,
but queued receipts/outcomes consume memory; the current recorder has no fixed
buffer cap. A proposed lossless event backlog has the same resource question.

Document and measure backlog growth, cancellation and eventual retry. Bound
memory or apply backpressure only through a concrete engineering plan, including
how it affects workers and shutdown. Evidence:
[recorder](../crates/barca-core/src/persist.rs); event replay owner #318.

### D15. Cancellation status cannot be written

**Covered regression.** User work is stopped, but an actual database constraint
prevents the cancellation-status update. Treating the run as durably cancelled
and releasing its owner would conceal the write failure. Existing checked
persistence returns the error and retains prior status/owner rather than
synthesizing a successful cancellation record.

Evidence: [cancellation-status write-error regression](../crates/barca-core/src/persist.rs).
Whether and how the UI separately displays stopped execution versus durable
cancellation is part of D11/D13's outcome contract discussion.

### D16. Downloaded metadata is invalid, stale or cannot be merged

**Covered regression.** Shared state downloads successfully, but its schema is
unsupported/corrupt; a different local operation overtakes the download; or
carrying local history into it fails. Installing it merely because transfer
succeeded could destroy good local history or replace a newer database with an
older candidate.

Validate/refuse or discard before replacement; preserve the whole prior DB when
carry fails. Existing tests cover those failures and controlled WAL/current/
previous crash boundaries. Evidence:
[metadata compatibility](metadata-schema.md),
[database replacement regressions](../crates/barca-core/src/db.rs).
Controlled crash points are not proof against every power-loss/storage-device
failure. Provider-durability acceptance remains #86.

### D17. Replayed progress combines rows but leaves the wrong count/status

**Covered regression.** A pulled checkpoint already contains one row for a
running run; local history has two additional committed rows. Carrying the row
union while keeping count one gives contradictory history. Copying local status
unconditionally can also erase an interruption notice or prematurely finalize it.

Existing running-progress carry reconciles distinct durable row counts, retains
cached counts/ownership/finished timestamps and does not finalize the run.
Replay is idempotent. Evidence:
[running-progress carry regressions](../crates/barca-core/src/state_carry.rs).
Full outcome/provenance consistency remains the separate #319 extension.

## Result identity: what does a saved result mean over time?

### R1. Untracked external data violates a pure asset's same-hash assumption

**Accepted model and known assumption violation; #287/#57.** On Monday, an
asset fetches untracked external data and produces `11`. On Tuesday the same
definition and tracked inputs produce `22` after refresh. The hash stays the
same and its file is overwritten. This violates asset purity; it is not a new
logical version that Barca must automatically archive.

The intended workflow introduces changing observations upstream through a sensor
or another tracked input. When that tracked value changes, dependent hashes
change and separate artifact versions are retained. A cache hit creates no new
artifact; a pure same-hash refresh is expected to reproduce equivalent bytes.
Unexpected differences require integrity/purity diagnostics and conflict policy.
An intentional forced same-hash replacement need not retain the prior bytes.
This replaces the earlier original-byte-per-execution proposal. Evidence:
[artifact path semantics](../python/barca/_artifacts.py),
[purity and sensor manual](../crates/barca-cli/docs/cache.md),
[#287](https://github.com/barca-orc/barca/issues/287).

### R2. A result set combines partitions from different refreshes

**Policy example; #287/#57.** A saved set initially has `us=11, eu=100`. Refresh
with changed tracked inputs produces a new US hash/value `22`; EU is still at its
earlier computation version or its update fails. Reading all produces
`us=22, eu=100`. Each partition can be valid independently while the aggregate
mixes different executions or external-data moments.

Is a result set a pinned manifest of specific partition materializations, or a
lookup of each partition's latest result? Does partial refresh publish a new set,
or leave an earlier set selected? If a read spans two concurrent refreshes, what
consistency is promised? Even a pinned manifest does not make independent source
reads a transactional external-data snapshot. No policy is selected.

### R3. A partition disappears or its name is reused

**Covered current-view regression plus policy example.** Yesterday's membership
was `us, eu`; today's is `us, apac`. Historical EU rows remain. Current SQL must
select today's membership and avoid fetching removed EU artifacts; existing
tests cover this. But a future historical saved set may legitimately refer to EU.

Specify whether “all” means current keys, keys in a saved manifest, or something
else. A reused display name must not silently be treated as an old materialization
without defining node/membership identity. Owners #287/#57; evidence:
[membership spec](sql-current-partition-membership.md),
[current SQL tests](../python/tests/test_sql_current_partitions.py).

### R4. Metadata outlives deleted, corrupted or overwritten bytes

**Policy/acceptance example; #287/#83/#243.** A run row and saved handle still
exist, but a lifecycle rule, manual deletion or refresh removes/replaces the
referenced data. History existence alone cannot ensure the bytes remain readable.

For an explicit historical computation-version selector, silently substituting a
different hash would change its meaning. A pure recomputation at the same hash
should reproduce the value, but availability and implicit execution still need
defined behavior; purity violations or untracked inputs may make recovery fail.
Define unavailable/corrupt/expired behavior, any explicit recovery operation and
how much retention is promised. These questions remain open.

### R5. A reader races with garbage collection

**Policy example; #83/#287.** A reader lists a saved set, then starts opening
its selected objects. A collector deletes one between listing and opening; a
lazy reader may still need objects after its initial call returns. Protecting
only a manifest does not necessarily protect the bytes or in-flight readers.

Define which references keep data live and how expiry interacts with active reads.
Do not choose leases, reference counts, indefinite retention or automatic copying
from this example alone.

### R6. Selecting one partition transfers a whole dataset

**Acceptance case under the accepted minimal-movement direction; #287/#57.**
A saved set contains 10,000 large partitions. A caller lists keys or requests
only `region=us`, but an eager implementation downloads/deserializes everything
or builds a new merged file first. Values may be correct while network, memory
and latency costs are unacceptable.

Metadata-only listing and selective artifact reads must be measured by bytes and
read calls as well as returned values. Single/subset/all access is approved;
exact return types, lazy lifetimes and backend range-read capabilities still
need specification. Do not claim every backend supports zero-copy/range reads.

### R7. An explicit subset contains missing, duplicate or reordered keys

**Policy example; #57/#287.** A caller selects `[eu, us, eu, missing]`, or a
multidimensional key omits one dimension. Should the result preserve request
order, canonical order or set semantics? Are missing keys a whole-request error,
per-key errors or partial results? Does reading missing saved data ever execute
anything? Define selection validation and the read-versus-materialize boundary
jointly in CLI/Python; no syntax or error policy is chosen here.

### R8. An upstream refresh changes a downstream result's lineage

**Accepted purity model plus open diagnostics/acceptance; #57/#287/#319.** A
downstream result refers to an upstream computation key. Upstream code violates
purity and a forced refresh replaces that key's bytes. The unchanged hash alone
cannot tell downstream that an untracked observation changed; it is not a reason
to invent an automatic per-execution versioning scheme.

Changing external observations must enter through tracked inputs/sensors so
dependent hashes change. Existing explicit refresh/cascade behavior is relevant
when users force recomputation. Record lineage and specify useful diagnostics
without treating arbitrary upstream side effects as tracked inputs. Verify
actual runtime behavior before asserting an additional cache defect.

## Adjacent evidence that exercises the same boundaries

### A1. Helper code changes; a warm cache hides the change

**Reproduced open correctness cases; #295.** A helper initially produces `11`.
Changing it to `22` through certain unsupported static dependency constructs
leaves the computation hash unchanged; a warm run returns cached `11` until an
explicit refresh. Some conditional hashing and runtime-import policy fixes have
shipped; the remaining reproduced static-cone cases are still open.

Evidence/checklist: [#295](https://github.com/barca-orc/barca/issues/295).
Static dependency completeness is separate from deciding whether saved historical
results follow latest; changing that policy cannot repair a missing dependency.

### A2. A publication probe sees a wheel; the installer cannot yet see it

**Observed release failure plus reproduced metadata disagreement; #384.**
The 0.22.0 availability helper reports an official wheel. The immediately following
uncached installer says the exact version is unavailable. Subsequent probes show
HTML and JSON representations of the same official index at different serials.
A later targeted verifier-only rerun passes; the artifacts are valid.

This proves metadata representations can disagree, not which exact edge served
the original failed installer. Availability on one surface is insufficient
proof of the next operation's visibility. Evidence/times:
[#384](https://github.com/barca-orc/barca/issues/384). Define bounded readiness
verification; do not retry arbitrary dependency/authentication failures or rerun
publishers as if they were harmless reads.

## Questions to discuss, without selecting answers

Publication overwrite policy is now settled at the product level: a changed
baseline requires `--force`; normal stale reads synchronize automatically;
retained computation-hash versions support reversion under the accepted pure-asset
model. O6–O8 and R1/R8 record those requirements and limits. The earlier proposal
to preserve every execution's original bytes is withdrawn.
Their implementation and retention details remain open; the timelines are not
new current-runtime reproductions.

| Question | Scenarios | Owner |
| --- | --- | --- |
| How are same-hash purity violations and upload/consumer races detected without adding per-run archives? | O1, O2, R8 | #381; demonstrated additional consumer defects need their own explicit scope |
| What does success mean when local work is durable but remote recovery is incomplete? | D4, D11, D13 | #319/#243; public CLI/HTTP/UI changes require compatibility review |
| Which outcomes/diagnostics must survive acceptance, failures and restart together? | D9, D10, D12 | #319 |
| What is the event replay window and how does a client recover outside it? | D13, D14 | #318 |
| How do callers select a retained computation version versus current results, and what membership/version set does “all” mean? | R2, R3, R6, R7 | joint #287/#57 |
| How does reverting to a retained hash affect downstream results, and what is the supported retention window? | O8, R4, R5, R8 | #287/#57 coordinated with #83/#243 |
| How long are those bytes kept, and what happens after deletion or during active reads? | O4, R4, R5 | #83/#243 coordinated with #287 |
| What are subset ordering, missing-key and partial-result semantics? | R3, R6, R7 | joint #57/#287 |

## Using the catalog for implementation acceptance

For each future PR, name the scenario IDs it addresses and the ones left open.
Record the exact base/version, controlled interruption/interleaving, observed
rows/objects/events and expected invariant. Use bounded, owned synchronization
rather than guessed sleeps. Measure transfer counts, memory or retry cadence when
those are part of the scenario. Preserve negative controls and old-code failure
proofs where practical. Link a newly demonstrated defect to its canonical ticket.
Update its evidence label only after the corresponding proof or fix lands.

This catalog runs no new reproductions or runtime implementation. Explicit user
decisions above are recorded product requirements; remaining API signatures,
storage migration, retention duration and success-status changes are unselected.
