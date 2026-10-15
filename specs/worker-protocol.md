# Worker and transfer protocol

Specification 1; protocol version 3. **Draft implementation, not released.** Issue #82
also requires the compact batch profile below before this work is ready to merge.

## Framing and negotiation

Every frame is a four-byte unsigned big-endian payload length followed by UTF-8 JSON.
The existing readers reject frames larger than 256 MiB. JSON is readable and Python's
stdlib suffices; optional orjson changes encoding speed, not the wire format.

The helper's first frame is:

```json
{"type":"hello","role":"worker","protocol_version":3}
```

A transfer helper uses `role: "transfer"`. The coordinator checks the expected role and
an integer version equal to its own, then acknowledges:

```json
{"type":"hello","role":"coordinator","protocol_version":3}
```

Mismatch, missing version or a wrong role gets `protocol_error` with `message` and the
expected `protocol_version`, then connection refusal. The actionable error names the
helper's version and says to reinstall Barca in the Python environment used for the run.
Connection plus negotiation is bounded by the existing ten-second startup deadline.
A rejected, exited or cancelled helper is stopped and reaped before startup returns;
no work is assigned before acknowledgement. Standalone calls without `BARCA_SOCKET`
remain standalone. The historical stderr batch protocol has its own version 2.

## Current draft messages

After negotiation, requests and replies retain the existing tagged JSON shapes:

| Direction | Message types |
| --- | --- |
| coordinator → worker | `execute`, `execute_batch`, `parallel_response`, `cancel`, `done` |
| worker → coordinator | `step_completed`, `step_error`, `blocked`, `log`, `submit`, `heartbeat`, `repeated_warnings` |
| coordinator → transfer | `put`, `get`, `probe`, `shutdown` |
| transfer → coordinator | `done`, `error` (matched to requests by `id`) |

[protocol.rs](../crates/barca-core/src/protocol.rs) owns typed result/transfer shapes;
[io_loop.rs](../crates/barca-core/src/io_loop.rs) builds execution requests.
[_runtime.py](../python/barca/_runtime.py) and [_worker.py](../python/barca/_worker.py)
are the Python peers. Transfer replies retain hash, size, fetch/mismatch and failure
attempt/missing metadata. The transfer protocol never carries file bytes.

## Compact batch profile still to implement

This is the remaining requirement of #82, not a claim about the current draft:

- Hoist batch-invariant execution fields (source, function, serializer, artifact directory,
  retry policy) into a header.
- Put partition dimension names once beside value rows, rather than repeating each name
  in 10,000 dictionaries.
- Omit empty/default fields whose absence has the same meaning on the peer.
- Encode per-item result status and elapsed values as arrays in a batched result message.
  Preserve the coordinator's per-step lease, retry and crash-recovery guarantees while
  choosing when to flush; coalescing must not silently charge the wrong step's retry budget.
- Artifact data travels by reference. The current small inline JSON branch-result and
  legacy inline response paths need to be removed or replaced as part of this profile.
- A Rust ↔ Python conformance round trip must cover representative requests and replies,
  including a 10,000-row batch, with malformed/missing-field controls.

## Version policy and conformance

Worker/transfer protocol versions are independent of the package, plan document and DB
format versions. Optional additive fields may stay within a version when both readers
have the documented default; changed meanings, framing, required fields or incompatible
message variants require a protocol version bump. A mismatched package fails before work
rather than pretending to negotiate an older format.

Current negotiation controls are in [Rust protocol tests](../crates/barca-core/src/protocol.rs)
and [real Python/CLI handshake tests](../python/tests/test_protocol_handshake.py).
[transfer tests](../crates/barca-core/src/transfer.rs) keep failure/cancellation cleanup
under negotiation. The batch conformance coverage remains outstanding.

Plan documents independently carry `plan_version: 1`, on both CLI and HTTP output.
Consumers of persisted plans should reject unsupported versions (absence means an
unversioned older document). The CLI contract and OpenAPI `Plan` schema own that shape.
Database format compatibility remains owned by [metadata-schema.md](metadata-schema.md);
this change neither bumps the DB format nor replaces history.
