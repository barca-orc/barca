# Execution lifecycle, freshness requests and retention

## Scope and status

Specification version: 2. Status: approved product intent; implementation conformance has not
been established. This records requirements, not shipped commands, configuration keys,
wire fields or an implementation plan. Existing interface owners remain authoritative.
See [agent interaction](agents.md), [resource bounds](resources.md), and
[current-state ownership](current-state-and-execution.md). Outstanding choices are in the
[requirements handoff](../../docs/engineering/requirements-handoff.md).
Scope: freshness requests, invocation/attempt identity, snapshots, lifecycle and retention.

## Entities and state

An invocation fixes target and arguments and owns recorded attempts. An attempt uses captured
code/configuration and resolved inputs. Requests, execution owners, background freshness demand,
sensor checks and retained artifact versions have separate lifetimes. A manager can be
admitting, draining or stopped.

## Rules and transitions

### Fresh data requests and sensors

A data request honors the node's code-declared freshness policy. For Always, report stale
or refreshing state, reconcile the requested dependency chain, then return the fresh correct
result. This applies to data exploration as well as execution; metadata inspection may
report current state directly. Do not refresh unrelated branches merely to inspect one target.

Freshness is relative to the request's initial check and resolved observations. Updates
observed later need not keep that request waiting indefinitely; subsequent requests account
for newer observations. This is not a promise of external-source time travel.

Managed server/background operation proactively keeps Always chains fresh. One-off local
requests reconcile on demand without requiring a daemon. Sensors should declare an explicit
schedule; missing schedules default to one minute with a clear warning in managed mode.
A required one-off check runs immediately rather than waiting for a polling interval.

Sensor polling failures continue with exponential backoff, default maximum 15 minutes,
configurable. A direct Always request triggers the next check immediately despite background
backoff, sharing matching checks/work rather than multiplying them. This policy is sensor
recovery, not permission to retry ordinary tasks automatically.

If freshness cannot be verified because the source is unavailable, fail promptly with the
cause and offer an available older result explicitly labeled possibly stale. Do not silently
return it as fresh success or block the user's work indefinitely. Managed recovery continues
where a manager is running. Exact fallback selection and timeout interfaces remain open.

### Invocation, retry and snapshots

An invocation ID fixes its target and arguments. Reusing it with different target/arguments
fails validation and requires a new ID. Attempts remain individually recorded under that ID.
Explicit resubmission joins running work, returns an existing success, or retries a failed or
interrupted invocation. Recovering a lost submission response only retrieves the existing
state; automatic transport retries must not execute a failed task again. The precise public
mechanism distinguishing these actions remains interface design work.

Tasks retry only when explicitly requested, including after crashes. Automatically recover
interrupted asset computations; mark interrupted tasks clearly and require explicit retry.
This does not promise exactly-once side effects. A failed branch blocks its dependents with
the original cause; independent branches continue and successful results are preserved.

Each run uses code and configuration captured at its start, including downstream nodes that
have not started. Edits do not change active work. Explicit retries capture current code and
configuration for the new attempt, respecting current code-declared freshness policies:
Always refreshes stale dependencies first; otherwise rerun the requested node without
unnecessary dependency execution. Record code identity and resolved inputs for each attempt.
Do not infer that historical inputs remain valid after a code change; resolve under current
code and policy. Incompatible original arguments fail promptly; corrected arguments need a
new invocation. How complete project imports/configuration are snapshotted needs verification;
copying an entry module alone is not proof of isolation.

### Cancellation, ownership and shutdown

A client timeout/disconnection alone does not cancel execution. Work remains accessible by
ID until explicitly cancelled. Cancelling a queued invocation removes it immediately; it must
not start unless explicitly retried. Cancelling shared work detaches that requester; work
continues while another requester or background freshness demand needs it.

Cancellation first requests graceful termination, then force-stops after a configurable
timeout. Clean the entire owned process tree and release resources, not just the immediate
worker. A foreground Ctrl+C follows the same rule; a second interrupt forces cleanup.

Coordinator/server shutdown drains: reject new work promptly with a clear retryable shutting
down error, pause schedules and sensor polling, and allow active work a configurable grace
period. Status and logs remain readable until exit. At the deadline, stop remaining work
gracefully then force-stop its process trees. A second shutdown signal skips the remaining
grace period and forces cleanup. Preserve recovery state/outcomes, flush logs, and release
locks, connections and listeners. The precise durable transition ordering is implementation
work; cleanup must not imply that state can be discarded before recovery is possible.

Startup reconciles interrupted work before admitting new work, recovering assets while
leaving tasks interrupted and avoiding duplicate surviving workers. One-off execution exits
and cleans its owned workers/connections when finished; a pre-existing server remains running.
Expose why work waits: resources, dependencies, freshness checks or shutdown, in human output
and structured status. Do not kill shared or unrelated processes as part of one run's cleanup.

### Artifact retention

Default to retaining the latest three artifact versions per asset partition, configurable.
Apply retention retroactively to existing historical artifacts as well as new writes. This
supersedes the earlier preference for exclusively explicit eviction: ordinary version
retention now cleans naturally, without an additional automatic disk-space eviction policy.

Provide explicit cleanup taking a requested amount of space to reclaim, deleting oldest
eligible artifact versions first. It may prune below the configured retained-version count,
but must protect currently selected versions and artifacts needed by active runs. Apply
those protections to automatic retention too. Report actual reclaimed space and any shortfall
when protections prevent reaching the target. Exact command syntax is not approved here.

Retain execution history and logs when deleting artifact bytes; make artifact unavailability
explicit rather than presenting history as retrievable data. History/logs are considered
cheap enough to retain; no automatic history/log eviction policy was approved. Shared bytes,
remote-store deletion scope and the definition of a distinct retained version need design.

## Invariants

- **INV-INVOCATION-01:** An invocation identity cannot be reused with different target or arguments.
- **INV-RETRY-01:** Automatic transport recovery cannot execute a failed task again; task retry
  requires explicit intent.
- **INV-ATTEMPT-01:** Active attempts retain their captured code/configuration; an explicit new
  attempt captures current code/configuration.
- **INV-RETENTION-01:** Automatic retention and explicit cleanup protect selected artifacts and
  artifacts needed by active runs while retaining execution history and logs.

## Failures and recovery

Freshness outages fail promptly and offer any available older result as possibly stale. Tasks
require explicit retry after failure/crash; interrupted assets recover without duplicating
surviving ownership. Cancellation and shutdown escalate from graceful termination to forced
process-tree cleanup while preserving recovery state. Retention protects current and active-use
artifacts and keeps history/logs.

## Acceptance and evidence

Cover a source outage with a possibly stale alternative, immediate request during sensor
backoff, request-time freshness during newer observations, duplicate submission after a lost
reply, explicit task retry under changed code/policy, incompatible arguments, queued and
shared cancellation, shutdown during active work, second-signal cleanup, restart recovery,
and retroactive retention with pinned/shared artifacts. These are required scenarios, not
claims of passing runtime conformance or new finite-model coverage.

## Open questions and gaps

Open choices include direct-check deadlines and fallback selection; deduplication record
lifetime and pre-acknowledgement IDs; environment/dependency snapshot scope; drain/cancellation
deadlines and uncertain remote owners; distinct retained versions, shared-byte accounting and
remote deletion scope. The [discussion handoff](../../docs/engineering/requirements-handoff.md)
keeps these questions and unanswered proposals separate from approved rules.
