# Product experience

Design revision: 1. Status: approved product intent; implementation conformance is partial.
Scope: first use, terminal feedback and discoverability. These are observable outcomes and
preferences, not a choice of rendering library, database layout, module structure or scheduler.
Current flags/output shapes remain owned by the [CLI contract](../crates/barca-cli/docs/contract.md).
Changes to those shapes need normal contract, snapshot and manual updates before shipping.

## Two immediate entry points

A user should be able to start locally from an existing Python function, without a server or
configuration ceremony:

- Add `@task` for a minimal task runner and immediate execution.
- Add `@asset` for a dependency graph with excellent caching, straightforward refreshes and
  flows. Connect dependencies using the existing Python declaration interface.

Neither entry point is a prerequisite for the other. Partitions, scheduling, remote execution
and management become discoverable as the need arises; their complexity should not dominate
first use. Existing assets and tasks keep the same semantics as the project grows.

## Normal successful execution

Show the result or artifact location, plus a compact summary of what ran, what was reused
from cache, and elapsed time. Keep large results usable through references and exploration;
success should not require reading a wall of internal scheduling details.

Offer a brief contextual pointer to `barca sql` for data exploration and `barca.get()` for
Python/Jupyter access. Adapt the pointer to the relevant result and actual supported syntax.
Do not add explanatory prose to machine-readable stdout or invent an available command.
Frequency, placement and presentation of these hints are implementation choices.

## Long-running work and user logs

Show the current step or active work, elapsed time and estimated time remaining when the
estimate is credible. An unknown estimate should remain unknown rather than imply precision.
Stream the logs the user writes during execution. With concurrent steps, attach compact
origin information identifying the task/asset and partition, preserving the message itself.

Keep orchestration progress compact so user logs remain useful. The choice of live display,
line updates, ordering/presentation and terminal capabilities is open. Machine-readable
results and log/progress channels must remain composable under the CLI contract.

## Failures

Identify the failing task/asset and partition, explain the useful cause, and show the most
pertinent traceback by default. Do not require another command when that traceback already
makes the error obvious. Offer an exact supported command for deeper investigation where
useful. Complete logs and fuller diagnostic context can remain available on demand.

This does not prescribe a fixed number of traceback frames or hide the user's exception.
Relevance, grouping and formatting are implementation decisions; enough context to locate
and understand the failure is the acceptance criterion.

## Interaction and discoverability

Normal commands run without interactive prompts. Missing or ambiguous information produces
a clear error and a useful next command rather than waiting for input. Ordinary commands
must compose in scripts and agent workflows as well as in an interactive terminal.

Optional guided experiences may be interactive. A future `barca tutorial` is an
[unscheduled idea](proposals/api-ideas.md), not an implemented command or a prerequisite for
using the core product.

Agent information parity, discovery and large structured results are specified in
[agent interaction](proposals/agents.md). Shared budgets, FIFO ready-work admission and
coordinator overhead bounds are in [project resources](proposals/resources.md).
The requirements discussion remains open; the first-use decisions above are not an exhaustive
product acceptance specification.

## Acceptance and freedom

Evaluate the experience with these scenarios: a first task, a first cached asset and refresh,
a cache hit, a long step with logs, concurrent partition logs, an obvious Python failure,
a deeper failure needing investigation, and missing/ambiguous input in a noninteractive
process. Check useful results, attribution, next actions and machine-output integrity.

These scenarios are acceptance obligations, not a claim that a new end-to-end experience
suite already passes. Existing [CLI conformance](../python/tests/test_cli_contract.py),
[task/log behavior](../python/tests/test_task_logs.py), and
[live logs](../python/tests/test_live_logs.py) cover parts of the implemented surface.
They do not establish every approved default above.

Correctness invariants and explicit workload budgets are requirements. Prefer progressive
disclosure, fewer concepts and the least necessary work. Rendering details, storage strategy,
internal interfaces and algorithms remain open when they satisfy those requirements.
