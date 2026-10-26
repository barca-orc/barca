# Inspection and status meaning

## Scope and status

Specification version: 2. Status: implemented behavior and explicit limits.
Scope: what inspection says about computation validity, execution and stored bytes.
Terms: [glossary](../../GLOSSARY.md). Exact JSON keys, status enums and response bodies remain
owned by [OpenAPI](../interfaces/server-api.openapi.yaml) and the [CLI
contract](../../crates/barca-cli/docs/contract.md).
This document adds no wire fields or enum members.

## Entities and state

Inspection distinguishes historical materializations, current membership/selection, evaluated
freshness evidence, artifact availability and live queued/running work. A prediction before
required sensor polling can be unknown.

## Rules and transitions

### Separate facts

| Fact | What it establishes | What it does not establish |
| --- | --- | --- |
| A materialization succeeded | Historical execution produced a result | It is selected or fresh now |
| Cache/freshness decision | Reuse under the evaluated definitions and inputs | Every unused artifact still exists |
| Local/store availability | Whether bytes are present or fetchable under the storage policy | Their computation matches current inputs |
| Running/queued state | Work is active or waiting | A successful current result exists |
| Failed attempt | That attempt did not succeed | Earlier results and history ceased to exist |

Status can intentionally expose a stale last successful value; clients must inspect its
cache/state information. `Manual` can intentionally select a stale pin. Cached freshness
proofs avoid checking unused files; required artifacts still go through recovery.
Digest mismatch, local-only and other remote availability reasons retain their existing
`unsynced` semantics. There is no implication that freshness, availability and execution
are interchangeable statuses.

### Implemented caching and its limits

The server caches inspected graphs, metadata snapshots and status results. Source and DB
stamps, dynamic-key revalidation and background status refresh determine reuse. Inspection
ETags permit HTTP revalidation; a 304 means that representation is unchanged, not that
upstream external data was polled again.

A database change can invalidate cached inspection even after an unchanged sensor run.
Cold and post-run inspection can rebuild per-key state:
[#656](https://github.com/barca-orc/barca/issues/656). Millisecond cached responses are not a
cold-latency or memory guarantee. The [performance contract](performance.md) distinguishes them.

### Approved replacement, not yet a public status

The user approved an explicit **checking** state while freshness is being revalidated,
with freshness, execution and artifact availability tracked separately. That decision is
specified in [approved ownership](current-state-and-execution.md#inspection).
It is not currently a new JSON status string or field. Adding it requires updating the
OpenAPI/CLI owners, snapshots, UI and executable conformance in the implementation PR.

## Invariants

- **INV-INSPECT-01:** Inspection and execution use the same cache-decision rules for a given
  context. A prediction before a sensor is polled can be unknown and is not a guarantee of
  the result after polling.

- **INV-INSPECT-02:** Current partition listings follow membership, not all historical rows.

- **INV-INSPECT-03:** Historical success must not be relabeled as newly executed work.

## Failures and recovery

A failed attempt does not erase earlier results. Missing bytes and digest mismatches are
availability findings with their existing wire semantics. Invalidated read caches can require
rebuilding state; a cached response or HTTP 304 does not mean external sources were polled.

## Acceptance and evidence

Exercise pre-poll predictions, current membership after removal, historical success versus
current reuse, stale Manual pins, remote mismatch/availability and read-route revalidation.
Cold/post-run inspection still needs the #656 scaling evidence.

Conformance: [status](../../crates/barca-core/src/status.rs),
[cache/execution predictions](../../python/tests/test_sensor_freshness.py),
[current SQL membership](../../python/tests/test_sql_current_partitions.py),
[server ledger status](../../python/tests/test_server_ledger.py),
[read-route cache](../../crates/barca-server/src/read_cache.rs).

## Open questions and gaps

Explicit checking revisions, independent status tracking and bounded cold inspection are
approved obligations in [ownership](current-state-and-execution.md). Their public fields and
implementation evidence remain outstanding; #656 tracks inspection cost.
