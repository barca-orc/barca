# Materialization digests

Status: implemented for producer serialization and recompute detection. Specification version: 3.
Scope: local and confirmed-upload materializations, store mismatch repair (RFC-0009),
and implemented server ledger acceptance (RFC-0007). Upload ownership and concurrent read
limits are specified separately in [artifact-upload-receipts.md](artifact-upload-receipts.md).
Conformance: `python/tests/test_materialization_digests.py`, persistence Rust
tests, and for the ledger `crates/barca-server/tests/api.rs` and `python/tests/test_server_ledger.py`.

Each newly recorded successful materialization stores the SHA-256 of its serialized artifact
bytes in `output_hash`. It is calculated while the producer owns the staged file, before
publishing it. It is a byte digest, not a semantic equality check. Artifact paths do not change.
Legacy materializations may have no digest and establish a baseline on their next recompute.

When inserting a successful materialization, compare with the latest previously recorded
successful digest for the same exact node id and run hash. Ignore failed and digest-less rows.
A differing digest stores `bytes_changed = 1`; an equal digest, a new run hash or no previous
digest stores 0. This comparison and the insert share the recorder's transaction. Checkpoint,
terminal and replay writes remain idempotent for a run/node pair. Partition keys are compared
individually. Returning to an earlier run hash still finds the previous digest for that hash.

For recomputed results, each changed asset eligible for deterministic-byte checking emits once
at run completion.
Sensors, tasks, Manual assets and assets declared `deterministic=False` are excluded:

`[barca] warning: <node> produced different bytes for the same code and inputs; the function is not deterministic`

The step report has `bytes_changed: true`. A partitioned report aggregates that flag when any
key changed, while stderr and persisted history identify each changed key. The key is absent
on reports with no event. The diagnostic does not change the run's exit code.

`barca status --json` exposes the latest materialization's `content_hash` when available, and
`last_bytes_change: {node_id, run_hash, content_hash, created_at}` for the last mismatch event.
That event remains visible after later unchanged recomputes. Sensors, tasks, Manual assets
and assets declared `deterministic=False` expose no mismatch event. The human status table prints the
last event's timestamp and exact node id. A restored backup preserves the stored flag.

A `Manual` asset (RFC-0008) is not checked: it is refreshed by hand with the same code and
inputs to pick up new data, so a refresh that gives new bytes emits no warning, no
`bytes_changed` and no `last_bytes_change` (the stored row keeps its flag). Its consumers fold
its digest into their run hashes (#548), so they are stale after such a refresh.

On recomputation, an asset declared `deterministic=False` also emits no nondeterminism warning,
step `bytes_changed` or status `last_bytes_change`. This suppresses recomputation diagnostics,
not caching or stored producer digests;
persisted byte-comparison flags remain recorded. See the
[cache manual](../../crates/barca-cli/docs/cache.md) and
[deterministic controls](../../python/tests/test_asset_cutoff.py).

Store mismatch repair (RFC-0009) produces the same diagnostic: a `barca get` that re-uploads a
target whose store copy differs from this machine's (status `unsynced`, `cache.reason`
`mismatch`) prints the changed-bytes warning and sets `steps[].bytes_changed: true` on that
step, without a step running and without a new materialization row, so `last_bytes_change` is
not changed by it.
This store-consistency diagnostic applies even to Manual and `deterministic=False` assets;
it is separate from deciding whether a recomputation was unexpectedly nondeterministic.

A server ledger post (RFC-0007) whose run hash the ledger already holds with another digest is
not recorded for that step: the ledger keeps its row, answers `bytes_changed: [<node>]`, and the
posting machine prints the warning above once and sets `bytes_changed: true` on that step's
report only when that node is eligible for deterministic-byte checking. Its local
`bytes_unchecked` list suppresses those client diagnostics for Manual and
`deterministic=False` assets without changing the ledger's mismatch response.
