# Project resource bounds

## Scope and status

Specification version: 2. Status: approved requirements; implementation remains incomplete.
Requirements are still being elicited. This is not a new supported TOML section or a claim
that existing capacity/weight controls enforce CPU or memory quotas. Current measured
budgets and gaps remain in [performance](performance.md).
Scope: aggregate project resource allocation, admission and exhaustion.

## Entities and state

The budget scope is one project on one machine, including its coordinator processes, CLI/server
work, workers and children. Ready work waits for admission; admitted jobs consume aggregate CPU
and memory. Hard enforcement and best-effort enforcement are distinguishable outcomes.

## Rules and transitions

### Scope and defaults

A resource budget applies to the whole project on one machine: overlapping CLI invocations,
server requests, scheduled runs, managed background work, workers and their children share
it. Starting another run must not silently multiply the project's allocation. Independent
projects are separate scopes; this does not introduce a cross-machine execution budget.

Follow familiar DuckDB-style defaults: use the CPU allocation available to Barca and 80% of
the available memory allocation, respecting tighter container/OS limits. Resource settings are
optional. Users can override
the project budget explicitly. Here allocation means the resource entitlement, not a promise
to repeatedly resize a budget using momentary free-memory readings.

The CPU unit is a core-equivalent: two CPU-seconds per wall-clock second, aggregated across
the project, including internal DuckDB/NumPy threads and subprocesses. It is actual CPU
consumption, not merely a worker count, concurrency admission weight, or library thread hint.
The configurable job-memory budget covers Python workers, child processes and user-job data.
Coordinator overhead has the separate tighter bounds below; accounting must also ensure the
combined project respects its parent allocation.

CPU and memory limits must be truthful about enforcement. DuckDB's buffer-manager memory
setting alone cannot establish the hard job-memory bound. Enforcement mechanisms, supported
platforms, accounting details and quota granularity remain implementation/design work. Use
the strongest available enforcement; when hard enforcement is unavailable, proceed with
best-effort controls and clearly expose the limitation in human and structured status.

### Coordinator cost

| Scope | Approved bound |
| --- | --- |
| Really simple work | <20 MB total project Rust coordination memory |
| Demanding/worst-case coordination, including cold planning and inspection | <100 MB total project Rust coordination memory |
| Ordinary operations | Target <50 ms of Barca's own overhead, excluding Python startup and user-job execution |

These are approved targets, not newly passing whole-engine benchmarks. Count all project
coordination processes; measuring one CLI or server process alone cannot establish the
project bound. Define native/private/shared memory accounting explicitly and report peak
and settled values. The existing measured budgets use decimal bytes; MB in this target is
20,000,000 / 100,000,000 bytes, rather than a larger undocumented allowance.

Ordinary-work acceptance includes a simple task/asset request, a no-work check, an aligned
one-key update, a bounded discovery/inspection page and run-status interaction. Measure
native CPU and added latency separately; distinguish resource/queue wait and external I/O
from useful computation without hiding unexplained coordination overhead. Cold/full-domain
work needs its own scaling evidence rather than extrapolation from one-key results.

### Admission, sharing and exhaustion

First come, first served is the default, equally for humans, agents, CLI/UI and schedules.
Runs share available capacity. A large partitioned run must not reserve the whole machine
until it ends or enqueue all future work ahead of later runs. Schedule ready work FIFO;
already-running work finishes rather than being preempted for another caller's priority.
No separate interactive/agent/schedule priority policy is approved.

Hold back new work as a job-memory budget fills. If running jobs exceed a configured hard
memory budget, report a resource-limit failure and preserve completed results. Resource
exhaustion must not erase successful peers or masquerade as a cache hit. Identify the
affected attempt and allow unrelated work to continue. Internal ownership,
backpressure, estimation and process-control mechanisms remain open.


## Invariants

- **INV-RESOURCE-01:** Starting another invocation does not multiply the project resource budget.
- **INV-RESOURCE-02:** Unavailable hard enforcement is exposed as a limitation, never reported
  as an enforced hard limit.
- **INV-RESOURCE-03:** Resource exhaustion preserves completed results and identifies the
  affected attempt.

## Failures and recovery

Hold new work when job memory fills. A running job exceeding an enforced hard limit fails with a
resource-limit cause while completed peers survive. Where hard enforcement is unavailable,
expose the effective best-effort limitation in human and structured status; do not claim an
enforced quota.

## Acceptance and evidence

Exercise overlapping CLI/server/scheduled/background work, a large run followed by a small run,
multithreaded libraries and children, memory growth and effective-limit diagnostics. Measure
aggregate coordinator memory, CPU and latency for cold, inspection, concurrent and long-lived
workloads. These are outstanding acceptance obligations, not passing quota conformance.

## Open questions and gaps

- Exact memory accounting/enforcement seam and quota burst/measurement intervals.
- Defaults and diagnostics for temporary disk and file/connection pressure.
- Exact cancellation/termination deadlines; graceful then forced cleanup is approved.

[Operations](operations.md) owns approved artifact retention, cleanup and process lifecycle.

Best-effort fallback must be visible; it must not claim an unenforced hard limit or introduce
undocumented priorities.
