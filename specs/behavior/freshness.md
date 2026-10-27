# Observations, partition membership and current selections

## Scope and status

Specification version: 2. Status: implemented rules with the known selection defect below.
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
| Scheduled tick | Reconcile its node and reachable `Always` nodes; stop propagation at a separate schedule |
| `Manual` asset/task | Pin its existing successful result unless explicitly refreshed; a never-produced pin blocks readers |
| Sensor | Poll whenever required by a run, regardless of its declared freshness |
| Partitioned `Manual` | Current limitation: follows ordinary computation rather than pinning |

The canonical run interface (`POST /runs`, also used by `--server`) follows CLI freshness
rules. Deprecated trigger routes retain their compatibility behavior, including their older
Manual treatment; see [scheduling](../../crates/barca-cli/docs/scheduling.md#manual-pins-itself).
Manual pins itself; it is not an approval gate that prevents other inputs below it updating.
A changed `collect()` consumer needs all applicable inputs. Explicit partition selection does
not promise a partial collect over selected keys.

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

Conformance: [freshness rules](../../crates/barca-core/src/freshness.rs),
[scheduled execution](../../python/tests/test_serve_schedule.py),
[early cutoff](../../python/tests/test_asset_cutoff.py),
[selection](../../python/tests/test_partition_selection.py).

## Open questions and gaps

The concurrent-version selection ordering gap remains explicit in the
membership rule. Partitioned Manual pinning remains unsupported. Ordered observation publication
is approved in [ownership](current-state-and-execution.md), not implemented by comparing opaque
hashes.
