# Unapproved product and interface decisions

Status: unapproved, unimplemented alternatives retained from the former
[user API rationale](../../docs/engineering/user-api-decisions.md). These are not the current contract,
not accepted requirements, and not a roadmap. Reconsider them only with a new decision
and corresponding tests; no user should write code against them.

| Idea | Current behavior | Unresolved tradeoff |
| --- | --- | --- |
| Optional `barca tutorial` | Core commands and manual are available without a tutorial | A possible guided walkthrough of tasks, assets, refresh/cache behavior and exploration; no command is implemented or approved for delivery |
| A typed `SensorResult`, passed into and returned from sensors | Sensors have no DAG inputs; return a boolean, `(changed, value)`, or a value for comparison | Adds a public type and previous-observation injection; neither is implemented |
| Omit underscore-prefixed ordering parameters from calls | Declared ordering inputs receive `None` | Changes normal Python call signatures and compatibility |

Rejected, not open: task-to-asset inputs. DAG validation rejects a task as an input of an asset
or sensor (`dag.rs`, "cannot be an input of an asset"), the asset caching model depends on it,
and no softening is planned; a new decision would have to reopen it.

Scheduling, remote sink support, retry backoff and the missing-bytes fallback (a cached result
whose bytes are missing is fetched from the store, and recomputed when no copy exists;
`barca docs remote`, "Failures") are implemented. They are not open
questions. Current rules live in [freshness](../behavior/freshness.md),
[execution](../behavior/execution.md), [artifact formats](../interfaces/artifact-formats.md), and the
[terminal manual](../../crates/barca-cli/docs/scheduling.md).

## Resource configuration syntax

The resource policy is approved in [resources](../behavior/resources.md), but this possible
configuration spelling is unapproved and unimplemented:

```toml
[resources]
cpu = 2
memory = "8GiB"
```

No supported TOML keys are introduced by this example.

## Recovery and structured outcomes — unanswered

These proposals were presented before the requirements discussion paused. None is
approved; existing behavior and approved machine-information parity remain in effect. (A third,
the missing-bytes fallback, has shipped as described above and is no longer a proposal.)

1. Disk full: fail affected writes clearly, preserve existing valid artifacts and suggest
   cleanup; do not silently evict beyond configured retention.
2. Structured outcomes: expose the same outcomes, causes and recovery actions as human
   output through stable fields without prose parsing. Broad information parity is already
   approved; this proposal concerns its precise outcome/error contract.

Implementation of these proposals requires a decision and corresponding contract/conformance
updates. They do not block the initial structural refactors in the engineering plan.

## Physical task retry authorization

The approved [operations intent](../behavior/operations.md#invocation-retry-and-snapshots)
requires explicit intent to start a new attempt of a failed/interrupted task invocation.
Existing declarations separately configure automatic physical step retries, including worker
replacement. Does `retries=N` count as that caller's explicit authorization for physical task
retries, or should the future policy require another explicit action after each failure?

Status: unapproved policy choice. No option is selected by this documentation cleanup.
Resolve this before changing existing retry behavior; distinguish invocation attempts,
physical executions and their code/configuration identities in the eventual public contract.
