# Materialization digests

Status: implemented for producer serialization and recompute detection. Specification version: 1.
Scope: local and confirmed-upload materializations, and server ledger acceptance (RFC-0007,
`barca_core::ledger`); the RFC-0009 store repair policy is a separate boundary under
development.
Conformance: `python/tests/test_materialization_digests.py`, persistence and state-carry Rust
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

At run completion each changed asset emits once (a sensor or task is never reported: it runs
every time, and a sensor's new value under an unchanged run hash is its purpose):

`[barca] warning: <node> produced different bytes for the same code and inputs; the function is not deterministic`

The step report has `bytes_changed: true`. A partitioned report aggregates that flag when any
key changed, while stderr and persisted history identify each changed key. The key is absent
on reports with no event. The diagnostic does not change the run's exit code.

`barca status --json` exposes the latest materialization's `content_hash` when available, and
`last_bytes_change: {node_id, run_hash, content_hash, created_at}` for the last mismatch event.
That event remains visible after later unchanged recomputes. Sensors and tasks have no event. The human status table prints the
last event's timestamp and exact node id. A restored backup preserves the stored flag.

Store mismatch repair (RFC-0009) produces the same diagnostic: a `barca get` that re-uploads a
target whose store copy differs from this machine's (status `unsynced`, `cache.reason`
`mismatch`) prints the changed-bytes warning and sets `steps[].bytes_changed: true` on that
step, without a step running and without a new materialization row, so `last_bytes_change` is
not changed by it.

A server ledger post (RFC-0007) whose run hash the ledger already holds with another digest is
not recorded for that step: the ledger keeps its row, answers `bytes_changed: [<node>]`, and the
posting machine prints the warning above once and sets `bytes_changed: true` on that step's
report.
