# Inspection and status meaning

## Scope and status

Specification version: 4. Status: implemented behavior and explicit limits.
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

`GET /state?targets=a,b` (experimental) limits status and duration queries to the targets'
upstream cone. A group id target (`group:FILE:NAME`) expands to every node in the group and its
nested groups, and `files=` adds every node of the named source files; an unknown group or file
is `404`. An empty or omitted selection means the whole graph; each selection has its
own cached status. A current WAL DB is read in place even under `--read-only`, without
migrating or checkpointing it (SQLite may create its WAL/SHM sidecars). Fallback copies and
migrations and history computations run off the request threads. A read-only server warms
status once at startup even while an external writer changes the DB, then on settled changes
while polled. Evidence: `read_cache` unit tests, the router's
`state_targets_select_only_the_requested_upstream_cone` and
`state_targets_take_groups_and_files_in_place_of_their_nodes`, and the synthetic GB-class
[`bench_status.py`](../../benchmarks/planning_pipeline/bench_status.py) measurement.

### Artifact inspection evidence

Status copy checks and shape inspection share a disposable per-environment evidence cache,
limited to 8 MiB of serialized cache JSON, 8192 evidence/barrier entries and 64 KiB per value,
plus one fixed 16-byte contention epoch. Local evidence is keyed by absolute
request path, device, inode, size, mtime, ctime, format/sample/field options where relevant,
and storage-option, credential and Python import-environment scope. Check file identity before and after computing evidence; a file
that changes during reading must not publish reusable evidence. A missing file is checked
again rather than trusted as a cached local observation. Verified selected local copies are
preferred for shape only after successful copy verification, preserving historical/selected-artifact
distinctions. A store-check failure must not promote an unverified local copy for shape.

Local digest/shape evidence requires a change timestamp at least one second behind the
observation clock. Recent/future timestamps bypass both lookup and retention, including
when all stat fields collide after delete/recreate. A later settled observation may be
retained. This assumes filesystem change timestamps distinguish writes at least one second
apart; filesystem metadata deliberately forged across that boundary is outside the cache
contract. Older disposable-cache formats are discarded.

Cloud availability and remote shape observations have a fixed five-minute lifetime. Hits
do not extend it; a backwards clock invalidates time evidence. Cloud copy observations also
include local materialization identity/digest, so a refresh or recovery cannot inherit an
older cloud size. Confirmed uploads/fetches invalidate inspected remote subjects before
acknowledgement. Bounded invalidation barriers prevent an older in-flight observation from
restoring stale evidence. A busy invalidation lock conservatively changes a remote epoch;
local evidence and ordinary unrelated remote subjects remain reusable. This is advisory availability,
not a freshness proof or a promise of current remote bytes. Required reads, transfers and
ledger acceptance bypass this cache. Strict `stat_objects` remains the ledger path.

The native status helper opts into evidence using `inspection_cache` and aligned `paths` /
`locals` arrays in its internal `_state stat` request. It returns `objects` (the existing
store objects) and `locals` (null or `{size_bytes, sha256}`). Requests without that option
retain strict stat semantics. `_inspect` accepts the same optional cache location while
preserving its shape-array output. The transfer helper receives the private optional
`--inspection-cache` argument from resolved native configuration; its request/reply frames
and the worker wire protocol remain unchanged. Uninspected bulk uploads only check the
cache-file stamp, without parsing or rewriting evidence for every new artifact.

Cache files use atomic replacement and owner-readable/writable files. Concurrent writers
merge completed evidence under a nonblocking cross-process write lock; a busy lock or an
unwritable/corrupt/oversized file may lose cache work and causes later re-observation. No
metadata-ledger write is needed. At most eight copy checks and the existing bounded remote
shape readers run at once. Cold reads remain proportional to selected artifact bytes.

The grouped UI and sidebar render from graph/group metadata before cache status is available.
Pending state is not presented as never-run or cached. Status arrival preserves row and graph
geometry; a status error does not hide the graph. Pipeline/group views ask only for their
upstream cone, while All pipelines and the sidebar's health dots ask for everything. A sidebar
dot appears only when all of its file's state is known. Evidence:
[`state-loading.spec.ts`](../../ui/e2e/state-loading.spec.ts).

The grouped list and graph expose one shared run action for the selected member in the
topbar and node panel. Assets and sensors use cache-aware `get`; tasks use `run`. Read-only
mode disables both controls. An accepted target and its containing groups show live activity
before the first SSE event; completed runs refresh status/history, and View run opens the
durable run. Ungrouped node panels use the same action. Evidence:
[`grouped-run.spec.ts`](../../ui/e2e/grouped-run.spec.ts), including real run admission,
completion, history/logs, read-only controls, held-first-event activity and admission errors.

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
[read-route cache](../../crates/barca-server/src/read_cache.rs),
[inspection evidence](../../python/tests/test_inspection_cache.py), and the
[real CLI payload-read regression](../../python/tests/test_store_inspection_cost.py).

## Open questions and gaps

Explicit checking revisions, independent status tracking and bounded cold inspection are
approved obligations in [ownership](current-state-and-execution.md). Their public fields and
implementation evidence remain outstanding; #656 tracks inspection cost.
