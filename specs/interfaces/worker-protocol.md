# Worker and transfer protocol

Specification 5; protocol version 5. Batched calls are implemented for 0.32.0.

## Framing and negotiation

Each frame is a four-byte unsigned big-endian length followed by UTF-8 JSON.
Readers reject payload lengths greater than 256 MiB before allocation. Python's
stdlib suffices; optional orjson changes encoding speed, not the wire layout.

The helper first sends `{"type":"hello","role":"worker","protocol_version":5}`
(or role `transfer`). The coordinator validates the role and integer version and
acknowledges `{"type":"hello","role":"coordinator","protocol_version":5}`.
A missing/mismatched version or role gets `protocol_error` with `message` and the
expected version, then connection refusal. The error says to reinstall matching
Barca packages. Connection and negotiation share the ten-second startup deadline.
Rejected helper processes are killed and reaped before the run returns. Worker cleanup
that runs off the event loop is joined at pool shutdown, including rejected starts
already removed from the pending-worker map.
Rejected, exited and cancelled helpers are reaped; no work precedes acknowledgement.

A worker's hello also carries `worker_id`, the integer from its `BARCA_WORKER_ID`
environment variable. The coordinator starts every worker a wave of work needs
before waiting on any, and workers connect in whatever order they finish starting:
the id pairs each connection with the process the coordinator started (the
peer's process id, `SO_PEERCRED`, when a hello has none). A connection that
matches no worker still starting is closed. A coordinator ignores unknown hello
fields, so the key is additive within each protocol version.
Version 5 requires helpers that preserve explicit sensor signals and report compact source
observations. Version 4 and earlier helpers are refused
before receiving work, with a reinstall message; matching barca packages are required.
The historical standalone stderr batch protocol is independently version 2.

## Compact tables

A table is `{header: object, dims: [string], rows: [[value]], missing?: [[integer]]}`.
The header holds fields present and equal on every item. Each row has exactly one
cell per dimension. Dimensions are unique and cannot overlap header keys. Omitted
cells are null with their column index in that row's optional `missing` mask; an
explicit null has no missing index. Masks have one entry per row, valid unique
integer column indexes, and no booleans. Missing fields and null remain distinct.

`execute_batch` has `items: table`, optional `kwargs: table`, and a coordinator-owned
`result_journal` path. Items contain execution metadata (`node_id`, source, function,
kind, serializer, inputs, argument references, annotations and policy). Equal fields
appear once in the header. Keyword/partition dimensions appear once in the kwargs
table; constant dimensions may be hoisted. The worker reconstructs `direct_kwargs`.
No-input/argument/type/sink empties, null optional fields, `branch:false` and zero
(timeout disabled) are omitted. Missing timeout means zero, not the 300-second
coordinator default. Retry policy stays coordinator-owned; artifact directory is
already hoisted into worker environment setup. Singleton `execute` uses the same
normalized step fields without a table.

In 0.32.0 (RFC-0013), `execute_batch` may also carry `call: {id: string}`. Its items
are the keys of one function call, rather than separate invocations: partition arguments
become lists, and `aligned_inputs` names upstream inputs assembled as keyed dicts or lazy
tables. The worker reports each key separately through the existing result journal;
successful artifacts additionally carry `batch` (the call id) and `batch_keys` (the call's
key count). A worker dying during a call fails every unfinished key in it. Weight and
retry admission apply once per call. Frames without `call` retain sequential execution.
Conformance: `python/tests/test_batched_partitions.py` exercises the real worker, per-key
cache identities, failures and retries, and server execution.

Two additive item fields of protocol version 3 (0.29.6) serve keyed partition sources
(`partitions_from` over a source that returns a dict). An `inputs` entry
`{"_keyed": true, "path": <artifact>, "key": <key>}` asks the worker for that key's value in
the source's JSON artifact; the worker passes it only when the function takes the parameter
(by name, or `**kwargs`), and reads each source artifact once per process.
`partition_source: true` (omitted when false) marks a step whose output a `partitions_from`
reads for keys: the worker checks the value is a list, or a dict with string or integer keys
(an integer key becomes its string) and JSON-writable values, writes it as JSON, and otherwise
fails the step with an error naming the key or the expected types.

Results are one `results` message per assigned batch (a successful prefix plus the
failure if execution stops), or one single-row frame for singleton execution:

```json
{"type":"results","node_ids":["a","b"],"statuses":["step_completed","blocked"],"elapsed_seconds":[0.01,null],"details":{"header":{},"dims":["format","path","reason","size_bytes"],"rows":[["json","/a.json",null,12],[null,null,"sensor",null]],"missing":[[2],[0,1,3]]}}
```

All parallel arrays and detail rows have equal lengths. Status is `step_completed`,
`step_error` or `blocked`. Completion metadata requires path, format and byte size;
elapsed time comes from its parallel array. Error metadata requires error type,
message and traceback, with elapsed time in the array. Optional error metadata is
`phase`, where the step failed: `load_inputs`, `user_code` (importing the module, calling
the function, checking what it returned), `split_output`, `write_output` or `sink`. For
`user_code` the traceback holds the user's frames only (barca's and importlib's removed);
for the others it holds every frame. An absent or unknown `phase` is read as unknown.
Blocked metadata requires reason. Optional metadata retains timing, digest, sinks, frame
reader and the private upload snapshot reference. Unknown statuses, invalid identity/time types, malformed
tables, missing required metadata and inline `json` artifact data are rejected.
Legacy per-item result message variants are not accepted after v3 negotiation.

## Completion ownership while batching

The coordinator creates one private temporary result journal per multi-item batch
under `.barca/staging/<coordinator-pid>/`. The worker appends UTF-8 JSON lines, each a
fixed four-cell metadata row `[node_id,status,elapsed,details]`, before beginning the
next item. The append is unbuffered; the OS retains complete rows if only the worker
process crashes. These transient receipts do not require machine-crash durability.

While a batch is running, a background sender samples new completions every 10 ms and
coalesces `{"type":"result_checkpoint","completed":N}` notifications. Only that first
N-row prefix is published when the notification is read; future journal rows must wait.
The notification follows the prefix's stdout/log frames in the socket. This prevents a
completion from overtaking its own queued stdout. Older notifications without a count
are harmless no-ops; final frames and EOF still recover their receipts.

The coordinator also drains the final frame's result count, and drains all available
receipts before `submit` (the worker blocks for its reply) or EOF (after complete socket
frames). An incomplete final journal line is ignored. Earlier completions close their
own leases before only the in-flight item consumes retry budget; unstarted items return
to the queue unchanged. Progress and uploads continue during a long later step. Sampling
is not a hard real-time deadline. The counted-prefix addition is optional and retains
protocol version 3.

The final compact result frame is deduplicated against journal receipts; conflicting
or out-of-order/unleased receipts fail the connection. A worker becomes assignable
again only after the final frame, even when every lease was closed from the journal.
This prevents a following batch from replacing an earlier journal prematurely. The
journal is removed when its I/O task ends the batch, disconnects or is aborted. It
contains metadata and file references, never artifact values. Dead coordinator PID
staging cleanup remains the crash backstop. Workers never access the metadata DB.

## Step timing metadata

Completion artifact metadata and error details optionally include `import_seconds`
when loading the module took longer than one second. Missing means no slow import was
reported; module reuse adds none. It is included in the existing elapsed duration,
not added on top. Compact tables and checkpoint receipts preserve the field. This
additive optional metadata was introduced within version 3 and remains in version 4. Timeouts identify the step id.
[Timing conformance](../../python/tests/test_run_step_timing.py) exercises retries,
slow imports, timeout identity and worker disconnects; protocol tests cover optional
error metadata.

## A worker after a failed step

Error details optionally include `worker_continues: true`: the step raised an exception that
unwound cleanly, and the worker goes on with the rest of its batch and then takes more work
(#523). The coordinator closes the failed lease and keeps the worker; a retry may run in the
same process. Without it (an older worker, or one that may not be sound: an exception
while its module was imported or its function was looked up, `SystemExit`,
`KeyboardInterrupt`, `MemoryError`, a step still running past `timeout_seconds`, or an
unfinished `parallel()` exchange), the worker stops executing its batch, and the coordinator
stops it, returns its unstarted leases to the queue front and starts a fresh worker when ready
work needs one. A worker that dies is reported as before, with its exit status. The field is
additive optional metadata introduced within version 3 and retained in version 4.
[Conformance](../../python/tests/test_worker_survives_exceptions.py) counts worker starts and
dispatched steps from the coordinator's `BARCA_TRACE_TIMING` summary.

## Other messages and artifact data

Coordinator → worker: `execute`, `execute_batch`, `parallel_response`, `cancel`, `done`.
Worker → coordinator: `results`, `result_checkpoint`, `log`, `submit`, `heartbeat`,
`repeated_warnings`. Coordinator → transfer: `put`, `get`, `probe`, `stat`, `shutdown`.
Transfer → coordinator: `done`, `error`, correlated by request id. `stat` (`{type, id,
remote}`, added for RFC-0009's `barca get` repair) asks whether an object is in the store and
moves no bytes: `done` carries its `size_bytes` and, for a store that is a plain directory, its
`sha256`; a missing object is an `error` with `missing: true`. It is additive: the protocol
version is unchanged, and a coordinator sends it only to a helper from the same build.

`log` is `{type, node_id, line, stream?, t?}`, one frame per line, sent when the line is
complete (or flushed at the end of the step). `stream` is `stdout` or `stderr` (absent:
`stdout`): the worker records Python's `sys.stdout` and `sys.stderr` while a step runs, and
writes stderr through to its own stderr as well. `t` is when the line was written, seconds
since the epoch as a float (absent: the coordinator stamps it on arrival). Both are additive
fields of protocol version 3. Lines are not batched, so a line written just before a worker
dies still arrives.

A successful parallel response requires `artifact: {path,format,frame_type?}`. Every
branch return value, including small JSON, is written to its run/group-owned file.
Legacy inline result/value/text paths are removed. Readers use the returned frame
reader and files stay live through the calling step's lifetime. Argument/partition
selectors remain execution metadata; materialized outputs and DAG inputs travel by
file reference. Transfer replies preserve digest, size, fetch/mismatch and failure
attempt/missing fields; the transfer protocol never carries file bytes.

## Value helper requests

`python -m barca._value` is a one-shot helper, not a socket peer: one JSON document on
stdin, one JSON array on stdout, one answer per item in request order; a fatal error is
a non-zero exit whose last stderr line is the message. A read request (`barca get -o
value`) is `{"artifacts": [{"path", "format"}]}`, answered with `{"value"}` or
`{"error"}`. A sign request (`barca get -o url`, RFC-0011) is
`{"op": "sign", "version": 1, "objects": [{"uri", "expires", "filename"}]}`, with
`expires` in whole seconds (1 to 604800) and `filename` the download name; each answer is
`{"url", "expires_at"}` (RFC 3339 UTC, `Z`) with an optional `note`, or
`{"error", "kind"}` with `kind` one of `usage`, `missing`, `credentials`, `store`. The
helper refuses any other sign `version` before signing anything. A signed URL appears
only in a successful answer on stdout; errors and notes never contain one.
Conformance: [signed URL tests](../../python/tests/test_signed_urls.py) and the answer
parser tests in [values.rs](../../crates/barca-core/src/values.rs).

## Version policy and conformance

Protocol, package, plan and DB versions are independent. Additive optional fields
may retain a version when both readers have documented defaults. Changed meanings,
framing, required fields or incompatible variants require a version bump. Older
packages are rejected before work; no legacy inline negotiation fallback exists.

[wire.rs](../../crates/barca-core/src/wire.rs) tests an actual Rust → Python stdlib → Rust
round trip of 10,000 execution rows and mixed completion/error/blocked rows, including
null/missing values and private upload references. Malformed shapes and inline data
are negative controls. [result_journal.rs](../../crates/barca-core/src/result_journal.rs)
checks partial writes, duplicates, invalid receipts and deletion. The worker I/O conformance
test queues valid stdout/checkpoint/results traffic ahead of the reader and rejects
publication of receipts before their own stdout frames.
[compact runtime tests](../../python/tests/test_compact_worker_protocol.py) observe real
multi-item result frames and prove crash/exception retry accounting without replaying
earlier side effects. [parallel tests](../../python/tests/test_parallel_results.py) cover
nested handoff and file-only values; [handshake tests](../../python/tests/test_protocol_handshake.py)
cover wrong versions, cancellation and reaping. The worker-pool shutdown test in
[io_loop.rs](../../crates/barca-core/src/io_loop.rs) holds a kill/wait job behind a gate
and requires shutdown to wait for it. Runtime framing tests cover size limits.

Plan documents independently carry `plan_version:1` on CLI and HTTP output. Consumers
should reject unsupported versions; absence identifies an older unversioned document.
The CLI contract and OpenAPI Plan schema own that shape. DB compatibility remains in
[metadata-schema.md](metadata-schema.md); this change does not bump its format.

## Sensor observations and source discovery

A sensor item may carry `previous_sensor` with the prior local artifact path, format,
size and content digest. This is producer-owned data, not access to the metadata DB.
An unchanged observation can reuse it without rewriting the artifact. Results carry
`sensor_changed` independently of `content_hash`: the coordinator records a freshness
revision while the content digest continues to describe actual bytes.

An unpartitioned source of `partitions_from` reports `source_observation`: count,
`keyed`, membership digest, and, when verified previous bytes are available, the previous
content digest, changed key/value pairs, changed keys in producer order, and removed keys.
The worker checks the previous bytes against their recorded digest before comparing.
Rust hashes affected values using the existing canonical JSON contract. A delta is usable
only against the validated previous source digest; otherwise ordinary cache validation
applies. Full snapshots stay in worker memory and artifacts; proofs persist only compact
counts and digests. Missing or malformed observations never establish freshness.

Producer source deltas are optional evidence capped at 256 KiB of conservative JSON size.
Oversized changed values or removed-key lists send only the compact summary; the engine
then uses normal artifact validation. Artifact payload size never expands the protocol frame.
