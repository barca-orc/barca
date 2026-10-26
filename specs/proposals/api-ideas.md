# Unscheduled API ideas

Status: unapproved, unimplemented alternatives retained from the former
[user API rationale](../user-api-decisions.md). These are not the current contract,
not accepted requirements, and not a roadmap. Reconsider them only with a new decision
and corresponding tests; no user should write code against them.

| Idea | Current behavior | Unresolved tradeoff |
| --- | --- | --- |
| Allow task-to-asset inputs with an uncacheable warning | DAG validation rejects task inputs to assets and sensors | Weakens the asset caching model; no softening is planned |
| A typed `SensorResult`, passed into and returned from sensors | Sensors have no DAG inputs; return a boolean, `(changed, value)`, or a value for comparison | Adds a public type and previous-observation injection; neither is implemented |
| Omit underscore-prefixed ordering parameters from calls | Declared ordering inputs receive `None` | Changes normal Python call signatures and compatibility |

Scheduling, remote sink support, and retry backoff are implemented. They are not open
questions. Current rules live in [freshness](../freshness.md),
[execution](../execution.md), [artifact formats](../artifact-formats.md), and the
[terminal manual](../../crates/barca-cli/docs/scheduling.md).
