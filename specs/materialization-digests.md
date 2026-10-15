# Materialization digests

Status: implemented for producer serialization and recompute detection. Specification version: 1.
Scope: local and confirmed-upload materializations; server ledger acceptance and the RFC-0009
store repair policy are separate boundaries under development.
Conformance: `python/tests/test_materialization_digests.py`, persistence and state-carry Rust tests.

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

At run completion each changed node emits once:

`[barca] warning: <node> produced different bytes for the same code and inputs; the function is not deterministic`

The step report has `bytes_changed: true`. A partitioned report aggregates that flag when any
key changed, while stderr and persisted history identify each changed key. The key is absent
on reports with no event. The diagnostic does not change the run's exit code.

`barca status --json` exposes the latest materialization's `content_hash` when available, and
`last_bytes_change: {node_id, run_hash, content_hash, created_at}` for the last mismatch event.
That event remains visible after later unchanged recomputes. The human status table prints the
last event's timestamp and exact node id. Carried shared history preserves the stored flag;
receiving history does not reclassify an old materialization against a new row order.

Store mismatch repair and server posts must produce the same diagnostic when implemented
under RFC-0009 and RFC-0007. This specification does not claim those proposed paths are shipped.
