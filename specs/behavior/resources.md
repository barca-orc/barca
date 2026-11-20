# Project resource bounds

## Scope and status

Specification version: 3. Status: approved requirements; implementation remains incomplete.
Version 3 replaces first come, first served admission with requested runs first and a bounded
wait for scheduled runs, implemented for the capacity a server's runs share (#1003).
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

Runs share available capacity. A large partitioned run must not reserve the whole machine
until it ends or enqueue all future work ahead of later runs. Already-running work finishes
rather than being preempted for another caller's priority.

Requested runs first is the default (decided 2026-11-17, #1003). Ready work of a requested run
is admitted before ready work of a scheduled run. Humans, agents, the CLI and the UI are all
requests and are not ranked against each other. Requested runs are admitted in the order they
first asked, not interleaved with each other, and likewise among scheduled runs. A scheduled
run's ready step that has waited a bound (120 seconds by default) is admitted next, so a
stream of requests cannot starve schedules. Nothing is preempted, so the longest a request
waits for the work running when it asks is the rest of that work. An operator can restore
first come, first served for requests and schedules alike.

Motivation, measured on 0.43.1 with `barca serve --capacity 1` and sleep steps: a requested
run of 85 steps overlapping a scheduled run of 110 steps took 202 s under first come, first
served against 43 s alone, because the two alternated step by step; with requested runs first
it took 44 s and the scheduled run's wall time was unchanged (within 2.5 s).

As implemented (#1003): the order applies to the capacity a server's runs share
(`[server] capacity`, `barca serve --capacity`), with `[server] admission =
"requested_first"` (default) or `"first_come"` and `[server] scheduled_max_wait_seconds`
(default 120). A run's origin is scheduled when a schedule's tick started it and requested
otherwise. A run gives back the weight of a step that ended in the same call that asks for its
next step, so another run cannot be admitted to it in between. `GET /queue`
`capacity.waiting` lists the waiting steps in admission order with each step's `origin`. The
bound applies to each scheduled step, not to the scheduled run: under an unbroken stream of
requests a scheduled run advances one step per bound. The server's run queue already started
requested runs ahead of waiting scheduled runs; the bound does not apply there. A run without
a shared capacity is admitted against its own capacity only and has no line.

A request may decline its priority (decided 2026-11-19; `queue: "last"` of `POST /runs`,
`--queue last` of `barca get` and `barca run`, experimental). Such a run is ordered with
scheduled work wherever requested and scheduled work are ranked: for a run slot it starts
after the requests that did not decline, among the scheduled runs in the order the server
accepted them, and it does not use the slot past `max_runs`; for the shared capacity its
origin is scheduled, the bound included. The default is unchanged, as is the order among
scheduled runs. The choice is part of the queue's durable description of the run, so it
holds across a restart while the run waits; it is not recorded with the finished run. A
command that runs on the caller's machine has no queue and refuses the option as a usage
error. Conformance: `python/tests/test_serve_queue_last.py`.

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

Requested runs first (#1003) is checked by the
[shared capacity's unit tests and simulation](../../crates/barca-service/src/serve/capacity.rs)
(order, aging bound, first-come opt-out, weights above 1, the handoff race), the
[engine's handoff test](../../crates/barca-engine/src/io_loop/tests.rs)
(`a_step_that_ends_gives_its_weight_back_with_the_next_ask`) and
[real servers](../../python/tests/test_serve_runtime_limits.py) at capacity 1 and 2 with a
scheduled run and an overlapping request.

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
