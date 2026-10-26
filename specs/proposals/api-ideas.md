# Unapproved product and interface decisions

Status: unapproved, unimplemented alternatives retained from the former
[user API rationale](../../docs/engineering/user-api-decisions.md). These are not the current contract,
not accepted requirements, and not a roadmap. Reconsider them only with a new decision
and corresponding tests; no user should write code against them.

| Idea | Current behavior | Unresolved tradeoff |
| --- | --- | --- |
| Allow task-to-asset inputs with an uncacheable warning | DAG validation rejects task inputs to assets and sensors | Weakens the asset caching model; no softening is planned |
| Optional `barca tutorial` | Core commands and manual are available without a tutorial | A possible guided walkthrough of tasks, assets, refresh/cache behavior and exploration; no command is implemented or approved for delivery |
| A typed `SensorResult`, passed into and returned from sensors | Sensors have no DAG inputs; return a boolean, `(changed, value)`, or a value for comparison | Adds a public type and previous-observation injection; neither is implemented |
| Omit underscore-prefixed ordering parameters from calls | Declared ordering inputs receive `None` | Changes normal Python call signatures and compatibility |

Scheduling, remote sink support, and retry backoff are implemented. They are not open
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

These three proposals were presented before the requirements discussion paused. None is
approved; existing behavior and approved machine-information parity remain in effect.

1. Missing artifact bytes: try the configured remote store before recomputation, then
   recompute under the request freshness policy if unavailable. Retained history alone
   does not establish result availability. The exact fallback policy remains undecided.
2. Disk full: fail affected writes clearly, preserve existing valid artifacts and suggest
   cleanup; do not silently evict beyond configured retention.
3. Structured outcomes: expose the same outcomes, causes and recovery actions as human
   output through stable fields without prose parsing. Broad information parity is already
   approved; this proposal concerns its precise outcome/error contract.

Implementation of these proposals requires a decision and corresponding contract/conformance
updates. They do not block the initial structural refactors in the engineering plan.
