# Project resource bounds

Design revision: 1. Status: approved requirements; implementation remains incomplete.
Requirements are still being elicited. This is not a new supported TOML section or a claim
that existing capacity/weight controls enforce CPU or memory quotas. Current measured
budgets and gaps remain in [performance](../performance.md).

## Scope and defaults

A resource budget applies to the whole project on one machine: overlapping CLI invocations,
server requests, scheduled runs, managed background work, workers and their children share
it. Starting another run must not silently multiply the project's allocation. Independent
projects are separate scopes; this does not introduce a cross-machine execution budget.

Follow familiar DuckDB-style defaults: use the CPU allocation available to Barca and 80% of
the available memory allocation, respecting tighter container/OS limits. Users can override
the project budget explicitly. Here allocation means the resource entitlement, not a promise
to repeatedly resize a budget using momentary free-memory readings.

Proposed, not currently accepted configuration syntax:

```toml
[resources]
cpu = 2
memory = "8GiB"
```

The CPU unit is a core-equivalent: two CPU-seconds per wall-clock second, aggregated across
the project, including internal DuckDB/NumPy threads and subprocesses. It is actual CPU
consumption, not merely a worker count, concurrency admission weight, or library thread hint.
The configurable job-memory budget covers Python workers, child processes and user-job data.
Coordinator overhead has the separate tighter bounds below; accounting must also ensure the
combined project respects its parent allocation.

CPU and memory limits must be truthful about enforcement. DuckDB's buffer-manager memory
setting alone cannot establish the hard job-memory bound. Enforcement mechanisms, supported
platforms, accounting details and quota granularity remain implementation/design work; an
unsupported hard guarantee cannot silently become a best-effort hint.

## Coordinator cost

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

## Admission, sharing and exhaustion

First come, first served is the default, equally for humans, agents, CLI/UI and schedules.
Runs share available capacity. A large partitioned run must not reserve the whole machine
until it ends or enqueue all future work ahead of later runs. Schedule ready work FIFO;
already-running work finishes rather than being preempted for another caller's priority.
No separate interactive/agent/schedule priority policy is approved.

Hold back new work as a job-memory budget fills. If running jobs exceed a configured hard
memory budget, report a resource-limit failure and preserve completed results. Resource
exhaustion must not erase successful peers or masquerade as a cache hit. Internal ownership,
backpressure, estimation and process-control mechanisms remain open.

Acceptance must cover overlapping invocation types, a large run alongside a later small run,
multithreaded Python libraries, child processes, a memory-growing job, and useful diagnostics
for the effective budget and waits. Fixed coordinator bounds require separate cold,
inspection, concurrent and long-lived measurements. The finite ownership model does not
prove CPU/memory enforcement or fair scheduling.

## Questions still open

- Policy when the requested hard enforcement is unavailable on a supported platform.
- Exact memory accounting/enforcement seam and quota burst/measurement intervals.
- Defaults and diagnostics for temporary disk, artifact/log/history retention, and file/connection pressure.
- Cancellation/termination deadlines and escalation for a job that does not cooperate.

These are not permission to relax an agreed hard limit or introduce undocumented priorities.
