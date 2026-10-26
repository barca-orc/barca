# Barca

Language for the task runner and asset manager. A term describes the product concept;
its storage representation and implementation belong in the specifications.

## Language

**Asset**:
A reusable data result whose validity depends on its definition and inputs.
_Avoid_: cached task

**Task**:
An action requested for execution. Separate explicit invocations remain separate actions.
_Avoid_: uncached asset

**Sensor**:
An observation function that reports whether external state changed and may supply data.
_Avoid_: scheduled asset

**Partition key**:
The named coordinates identifying one part of a node's work or results.

**Partition domain**:
The current set of partition keys belonging to a node.
_Avoid_: partition history

**Observation**:
The external state or change signal reported by a sensor, together with any supplied data.

**Observation revision**:
The identity of an observation used to distinguish freshness changes from changes in data bytes.
An ordered observation revision additionally identifies which observation is newer.
_Avoid_: content hash, completion timestamp

**Run**:
An execution of a request or scheduled reconciliation, with its own progress and outcome.
_Avoid_: computation

**Computation**:
The work producing one asset result for a particular definition, inputs and partition key.
Several runs can need the same computation.
_Avoid_: run

**Materialization**:
A recorded production of a result. History can contain several materializations for the same key.
_Avoid_: current selection

**Current selection**:
The result selected for a node and partition under the inputs being considered; it can be an
older materialization reused from cache.
_Avoid_: latest materialization

**Freshness evidence**:
The information establishing whether a selection matches the definitions, membership and
observations against which it was validated.
_Avoid_: artifact availability

**Artifact availability**:
Whether a result's bytes are present locally, can be fetched, or are missing.
_Avoid_: freshness

**Reconciliation**:
Bringing requested assets and their dependency chains up to date with the relevant observations.
_Avoid_: unconditional refresh

**Subscription**:
A run's interest in the outcome of a shared computation.
