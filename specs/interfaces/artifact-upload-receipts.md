# Artifact upload receipts

Specification version: 3. Status: implemented. This boundary covers atomic producer
publication, upload source ownership, and confirmed receipt persistence. It preserves the
existing computation-hash artifact paths and sensor content identities. It does not define
immutable historical objects or isolation of concurrent readers of the shared local path.

Conformance: [controlled CLI overlap, primitive storage and cleanup tests](../../python/tests/test_upload_snapshot_receipts.py),
[transfer lifecycle tests](../../python/tests/test_remote_cancel.py),
[drain/requeue with an upload in flight](../../python/tests/test_drain_keeps_computed_steps.py),
[transfer confirmation tests](../../crates/barca-store/src/transfer.rs), and
[checkpoint/terminal persistence](../../crates/barca-run/src/persist.rs).

## Successful uploads

A successful receipt's SHA-256 and byte count describe the exact bytes uploaded by that
request. An atomic replacement of the reusable local artifact path, before the helper opens
its source or after the SDK copies it, cannot change those bytes or the receipt.

For remote-backed plan steps the serializer pins its completed staging file **before**
renaming that file to the reusable artifact path. It sends the private `upload_snapshot`
source alongside the ordinary artifact reference. The coordinator selects the remote
location from the ordinary artifact path and uploads from the snapshot. The snapshot is
never a persisted artifact identity or a public output field. Parallel branches stay local
and do not create upload snapshots.

Pinning uses a hard link where possible. When linking is unavailable, including different
filesystems, it copies the still-owned serializer staging file. This adds one artifact-sized
copy on that path. Same-filesystem pinning uses no additional data copy; replacing a shared
artifact retains the earlier inode until its pending upload is confirmed or abandoned.

The helper also pins the request source for SDKs that reopen filenames. Upload and receipt
hashing/size observation use that one stable snapshot, not the producer's live pathname.
Legacy/unversioned callers lacking a producer snapshot get a truthful receipt for the bytes
opened by the helper, but cannot obtain the earlier producer-ownership guarantee.

## Lifetime and persistence

Snapshots live in a coordinator-PID staging directory under `.barca/staging`, with a unique
run prefix. Confirmation removes the producer snapshot after handing its receipt to the
recorder. The run guard removes its remaining snapshots on cancellation, failure and normal
exit, after workers and the transfer owner are finished. It removes only its own prefix;
another server run may not have serialized anything yet, so an empty live-PID directory is
retained. The existing dead-PID staging cleanup removes it on a later worker startup.

The transfer helper removes its run's snapshots on SIGTERM, socket closure and coordinator
lifeline EOF. Its existing staged-file registry cleans SDK stages on shutdown. If both
coordinator and helper are killed outright, the next worker's dead-PID cleanup is the crash
backstop. An unconfirmed or abandoned (cancelled) upload supplies no successful receipt and
cannot publish a receipt-driven checkpoint row. A completed computation whose upload was
abandoned still gets the fail-soft terminal materialization described next (#1002).

A request that failed after its step succeeded (#390) does not fail the step. The terminal
ledger records the success row at the store location with the worker's producer digest, and the
upload is kept in the `pending_uploads` table (see [metadata-schema.md](metadata-schema.md)).

A request abandoned because the run was cancelled after its step succeeded (#1002) is treated
the same way, whoever cancelled: one cancelled-run path handles every cancellation, and the
conformance exercises a user's Ctrl-C to a CLI run and a server's drain deadline that requeues
the run. The step is computed;
only its publication is unfinished. When its reusable local artifact path is a file, the
terminal ledger of the cancelled run records its success row at the store location with the
worker's producer digest, and the pending row's `error` is `the run was cancelled before the
upload was confirmed`. The pending row is written before the materialization row. A step whose
function was interrupted has no artifact and is not recorded. A requeued server run, or the
next command, retries the pending upload before its own work and takes the step as a cache hit.
The run's upload snapshots are still removed on cancellation: the retry does not need them.
The next command that opens the store re-uploads the reusable local artifact path, before its own
work; a confirmed retry removes the pending row and fills a missing `output_hash` from its
receipt. A retry has no producer snapshot: a later refresh of the same path between the failure
and the retry is uploaded instead, the same read-isolation limit described below.

Checkpoint and terminal materializations preserve the sensor's worker-reported semantic
content hash. Ordinary asset rows use the confirmed upload digest as their output-hash
fallback. No hash/path format changes, new flags, configuration or output keys are introduced.

## Concurrent readers remain a separate boundary

The controlled overlap also demonstrates a distinct read-isolation defect: A serializes `11`,
B atomically installs `222` at the same local computation-hash path, and A's later
`final_output` read can return `222` even while A's upload and confirmed row correctly
identify `11`. This specification and its receipt fix do not claim to repair that read.
A second event-controlled CLI probe uses a pandas-parquet sensor and a Polars consumer.
A uploads a frame holding `11`; its consumer waits at `consume-input` before deserialization.
B replaces the shared local parquet with a frame holding `222`, while B's upload waits at
`upload-open`. Releasing A makes its consumer read `222`, although A's sensor uploaded `11`.
The different frame readers ensure this exercises a real disk read, not the worker's LRU.
The probe used unchanged source, bounded events and joined processes, and removed its
throwaway project. This is an observed downstream read-isolation defect.

Consumers reading the reusable path require a separate ownership design; historical remote objects may also be overwritten by later refreshes.
