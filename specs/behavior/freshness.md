# Observations, partition membership and current selections

## Scope and status

Specification version: 2. Status: implemented rules; the sequential historical-selection defect
(#657) is fixed since 0.34.0 with regression controls, and the concurrent-version ordering gap
below stays open.
Scope: sensor meanings, current membership, cache selection, and freshness policy.
Terms: [glossary](../../GLOSSARY.md). Hash completeness and publication mechanics remain owned by
[cache decisions](cache-decisions.md); byte digests by [materialization
digests](../interfaces/materialization-digests.md).
Approved changes to revision ordering and shared execution are separate in
[approved ownership](current-state-and-execution.md).

## Entities and state

Entities are sensor observations, partition domains, computation identities and current
selections. Membership contains current keys; history may retain removed keys. Observation
identity, byte identity and observation order are different concepts.

## Rules and transitions

### Observations

| Return value | Observation transition | Data available downstream |
| --- | --- | --- |
| `True` | Advance freshness, including when no data is supplied | No newly supplied payload |
| `(True, value)` | Advance freshness, even if serialized bytes match the previous payload | `value` |
| `False` or `(False, value)` | Retain the previous observation and payload, when present | Previous payload; the tuple's new value is ignored |
| A non-boolean value | Compare its content with the previous observation | The returned value |

The boolean is orchestration metadata, not downstream data. With no previous observation,
`False` supplies `None`, while `(False, value)` supplies `value`; there is no previous payload
to retain. A two-item tuple must start with a boolean; an invalid flag is an error, not
implicit content comparison. For a dictionary used as a partition source, freshness is compared
per key.
A change signal for the source does not force every unchanged dictionary entry's chain.
A whole-dictionary input still depends on the whole observation.

`observation_hash` is the implemented freshness identity. It is opaque: it is not an ordered
revision, a content digest, or a timestamp. The approved ordered revision in the approved
ownership design
must not be implemented by comparing existing hashes lexicographically.

### Membership and selection

A partition domain is current membership, not the set of every historically recorded key.
For a keyed source, an added or changed value invalidates that key's affected chain; a removed
key leaves current results. Removing membership does not erase history. Artifact eviction
can remove bytes while preserving rows under the normal cache policy.

Example: source values `1 -> 2 -> 1 -> 1`, with an asset doubling the value, require results
`2 -> 4 -> 2 -> 2`. Returning `4` on the last read violates INV-SELECT-01. A cache hit need
not append a new materialization, so choosing the greatest historical row ID is insufficient.

Generation-scoped freshness proofs retain bounded historical-selection exceptions per logical
node. When those exceptions exceed 256 KiB, discard the proof and validate normally. Proof
namespace v3 invalidates old shortcuts once on upgrade. This fixes the sequential reversion
in [#657](https://github.com/barca-orc/barca/issues/657); it does not establish observation
ordering or concurrent-version publication from the approved ownership design.

### Freshness policy

| Context | Meaning |
| --- | --- |
| Explicit `get` / `run` | Resolve the requested dependency cone; reuse matching asset results; run requested tasks and required sensors |
| Scheduled tick | Reconcile its node, the `Always` nodes below it and the `Always` nodes below every unscheduled sensor its run polls; stop propagation at a separate schedule |
| `Manual` asset/task | Pin its existing successful result unless explicitly refreshed; a never-produced pin blocks readers |
| Asset pinned with `barca pin` (0.41.0) | Read as its pinned materialization by every run and route, deprecated triggers included, even when targeted or refreshed; never computed until unpinned; consumers hash its pinned run hash; a pin whose result is gone blocks readers (exit 2, `blocked` under a tick). Unpartitioned assets only. Conformance: `python/tests/test_pins.py`, `freshness.rs` walk tests, `crates/barca-server/tests/openapi.rs` |
| Sensor | Poll whenever required by a run, regardless of its declared freshness. With a `Schedule`, its own ticks target the `Always` nodes below it; without one, every tick that polls it does |
| `Always` node neither below a scheduled node nor below an unscheduled sensor some tick polls | No tick targets it. It is computed by a tick only as an input of a target (an asset with no cached result); otherwise by a request |
| Partitioned `Manual` | Current limitation: follows ordinary computation rather than pinning |

The canonical run interface (`POST /runs`, also used by `--server`) follows CLI freshness
rules. Deprecated trigger routes retain their compatibility behavior, including their older
Manual treatment; see [scheduling](../../crates/barca-cli/docs/scheduling.md#manual-pins-itself).
Manual pins itself; it is not an approval gate that prevents other inputs below it updating.
Status decides by these rules too (#985): a `Manual` or pinned node reads `stale` when its own
code or inputs moved on, and what is below it has the state a run finds, reading it as it is
([cache decisions](cache-decisions.md), "The rules the prediction follows"). A paused node is
held by ticks only, so status says of it what a request would do.

A tick's targets are its scheduled node, that node's followers, and the followers of every
sensor without a `Schedule` that the run polls (#978). The followers of a node are the `Always` nodes reached going
downstream from it, through `Always` nodes and `Manual` assets and tasks (which are passed, not
targeted), not past a node with a `Schedule` or a `Manual` sensor. The run plans the targets'
inputs: the upstream cone of the targets, not past a `Manual` asset or task, a pinned asset or
a paused node, which are read as they are. A sensor in that cone is polled and its observation
recorded, whatever its freshness; an asset in it is computed when it has no cached result. The
polled sensors' followers are targets with cones of their own, so the target set is the least
set closed under both steps. Nothing else is touched.

- The set is static: it depends on the graph alone, not on whether a poll returned a new
  value. A tick whose polled sensors are unchanged executes the polls and nothing else.
- Only a sensor carries a tick to what is beside a target. An asset input does not.
- A polled sensor with a `Schedule` of its own is not followed: its followers are targets of
  its own ticks only, so they are out of date for at most one of its intervals after another
  tick polled it, and a schedule on the sensor and one on a node that reads it never both
  target them. A node with a `Schedule` of its own is targeted by its own ticks only.
- An `Always` task among the targets fires under the task rule (its inputs' versions changed,
  or a task it reads ran since); the scheduled task itself always runs.
- A paused target is skipped and a pinned target is read as its pinned result. The static set
  is computed without them: at run time they also stop the upstream walk, so a tick can poll
  fewer sensors than the static set follows, never an unscheduled one it does not.
- An asset targeted by two schedules is computed once per run hash: a later run finds the
  result, and runs in flight together share the step ([execution](execution.md)). A task is
  never shared between runs, so two ticks that fire in the same second both fire a task they
  have in common. Current limitation.

`barca_core::freshness::tick_targets` is the scheduler's target list and
`barca_core::freshness::tick_reach` the one statement of which schedules reach a node and how
(`Target` or `Input`); a node with no `Target` entry is brought up to date by no tick.
A changed `collect()` consumer needs all applicable inputs. Explicit partition selection does
not promise a partial collect over selected keys.

### The tick record

What a tick decided is recorded, not only what it computed (#960). The scheduler of
`barca serve` writes one row of `schedule_ticks` per due tick of a schedule: `fired`, `paused`,
`skipped` (a run of the node in flight, named) or `failing`. The run a fired tick started adds
its start, its wait in the run queue and, at its end, its status and what it did with each of
the tick's targets: `rebuilt` (the step executed), `up_to_date` (a cache hit, which writes no
materialization), `failed` or `not_reached`. So a tick that found everything fresh and a tick
that never happened leave different evidence.

The table is bounded by its writers (per schedule the newest 1,000 ticks, none older than 14
days, target lists on the newest 20 fired ticks) and is evidence only: no run reads it, and
freshness decisions never depend on it. Status reads it to say what the last tick found
(`last_check`), which decides a node's health only where the read path cannot decide its state
([health](health.md), "Last check"). `freshness_proofs` is a different table: derived
witnesses for incremental publication, rewritten per generation. Fields, write points, the
sensor limitation and the reads are in [health](health.md) ("What a tick records"); columns in
the [metadata schema](../interfaces/metadata-schema.md).

## Invariants

- **INV-OBS-01:** An explicit changed signal can invalidate a consumer without different data bytes.

- **INV-OBS-02:** When a previous payload exists, an explicit unchanged signal cannot replace it
  with a new tuple value.

- **INV-OBS-03:** Observation identity and serialized-byte digest have separate meanings.

- **INV-MEMBER-01:** A removed key is not resurrected by historical rows in current results.

- **INV-SELECT-01:** Cache selection matches the current computation identity, even if its
  materialization predates a more recent computation for that key.

- **INV-CHAIN-01:** Fresh unrelated keys need no function execution to update an aligned chain.

## Failures and recovery

An invalid change flag fails rather than falling back to content comparison. A never-produced
Manual pin blocks readers. Removed keys remain absent from current membership even when their
old results remain in history. Missing bytes are handled under [cache
decisions](cache-decisions.md), independently of freshness.

## Acceptance and evidence

Exercise boolean and tuple signals with/without previous payload, content comparison,
added/changed/removed keys, return to an older input identity, Manual pins, scheduled
propagation and explicit partition selection. The historical-reversion control covers sensor and cached-asset producers.

Conformance: [sensor observations](../../python/tests/test_sensor_observations.py)
(`test_signal_without_data_keeps_asset_chain_fresh`,
`test_unchanged_signal_preserves_data_for_a_new_consumer`,
`test_explicit_change_refreshes_even_with_the_same_payload`),
[sensor content comparison](../../python/tests/test_sensor_freshness.py),
[worker protocol](../interfaces/worker-protocol.md).

Conformance: [partition cache](../../python/tests/test_partition_cache.py)
(`test_a_new_key_runs_only_that_key`, `test_a_removed_key_is_not_resurrected`),
[keyed sources](../../python/tests/test_keyed_partitions.py),
[current SQL membership](../../python/tests/test_sql_current_partitions.py),
[incremental DAGs](../../python/tests/test_incremental_dag.py). The historical reversion control is in [partition audit](../../python/tests/test_partition_audit.py);
existing unpartitioned return-to-an-earlier-value coverage alone does not
prove the partitioned invariant.

Conformance: [freshness rules](../../crates/barca-core/src/freshness.rs)
(`always_nodes_below_a_sensor_a_tick_polls_are_its_targets`,
`a_sensor_polled_for_a_target_below_another_sensor_is_followed_too`,
`a_node_below_a_sensor_with_a_slower_schedule_follows_the_sensors_own_ticks`),
[scheduled execution](../../python/tests/test_serve_schedule.py),
[tick reach](../../python/tests/test_freshness_propagation.py)
(`test_a_tick_rebuilds_the_always_nodes_below_a_sensor_it_polls`,
`test_two_schedules_that_reach_the_same_node_compute_it_once_when_their_ticks_do_not_overlap`,
`test_two_ticks_in_the_same_second_compute_a_shared_asset_once`,
`test_a_paused_or_pinned_node_below_a_polled_sensor_is_left_alone`),
[early cutoff](../../python/tests/test_asset_cutoff.py),
[selection](../../python/tests/test_partition_selection.py).

## Open questions and gaps

The concurrent-version selection ordering gap remains explicit in the
membership rule. Partitioned Manual pinning remains unsupported. Ordered observation publication
is approved in [ownership](current-state-and-execution.md), not implemented by comparing opaque
hashes.
