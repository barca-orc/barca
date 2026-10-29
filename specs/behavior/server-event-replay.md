# Attempt-scoped event replay

## Scope and status

Specification version: 1. Status: server, CLI and browser implementation for pending 0.36.0;
cost probes remain incomplete. Scope: in-memory event retention, cursor validation,
replay/live attachment and subscriber lifetime. Exact HTTP shapes belong to
[OpenAPI](../interfaces/server-api.openapi.yaml); accepted full behavior is
[RFC-0016](../../site/src/content/docs/rfcs/0016-server-tokens-and-stream-resume.md).

## Entities and state

Each run attempt owns a 128-bit random epoch and monotonically increasing one-based event
sequence. A run's durable ID can outlive its attempt. A replay hub owns serialized windows
for all channels of a server instance. Each window retains at most 65,536 events and 16 MiB
of serialized payload; the hub retains at most 64 MiB across windows. Live progress metadata,
canonical outcomes, HTTP buffers and worker memory are outside those replay-byte limits.
A subscriber holds a cursor and one shared serialized frame, without a private backlog queue.

## Rules and transitions

- Admission allocates the epoch from OS randomness before accepting a new durable request.
  Recovered/requeued attempts allocate a new epoch even when their run ID is unchanged.
- Emission serializes the event once, assigns its next sequence and notifies readers.
  Per-run limits evict that run's oldest frames. The global index evicts oldest retained
  frames across active and finished windows without scanning every channel.
- Oversized/unserializable events advance the sequence and create an explicit gap. The
  preceding prefix is discarded because no complete suffix can cross that gap.
- No cursor means start at the first event. A cursor resumes strictly after its event.
  Malformed, overlong, zero, overflowed or future sequences fail with 400; another epoch is
  409; missing required history is 410, including a first attachment after truncation.
  A consumed terminal event is 204. Heartbeats carry no event ID.
- Attachment and live reads share the same cursor owner. A lagging subscriber closes at a
  gap rather than skipping it. Channel shutdown drains retained frames, then closes; the
  last owner/reader releases its window and global index entries.
- Running-step progress is maintained separately from replay, with deduplicated key counts.
  Inspection snapshots copy counts rather than all finished partition keys. Completion
  releases that progress metadata. Eviction cannot make a completed live step appear queued.

## Invariants

- **INV-REPLAY-IDENTITY:** event IDs are unique and stable within an attempt; attempts never
  reuse a prior epoch intentionally.
- **INV-REPLAY-BOUNDS:** retained serialized payload and event counts obey per-run/global
  limits; a subscriber does not clone or queue a complete replay window.
- **INV-REPLAY-GAP:** missing history is explicit; replay cannot silently join events across
  an eviction or oversized-event gap.
- **INV-REPLAY-AUTH:** token authorization precedes cursor/channel disclosure, including HEAD
  and conditional requests. Cursor diagnostics never echo arbitrary input.

## Failures and recovery

The stream is advisory, not a durable event archive. A missing channel remains 404. A gap,
restart or window eviction does not resubmit work or change its canonical outcome. Persisted
log APIs remain independent; a live run-view log list can show only the retained tail before
logs become canonical at wrap-up. Cursor errors and terminal acknowledgements are not cached.
OS randomness failure refuses admission instead of substituting a predictable epoch.

## Acceptance and evidence

[Replay and HTTP tests](../../crates/barca-server/src/replay.rs) cover strict resume,
terminal acknowledgement, malformed/future/overflow/other-attempt cursors, first-attach
expiration, slow readers, global/count/byte eviction, oversized gaps, shutdown cleanup,
attachment/emission races, authorization ordering and progress independent of eviction.
[Existing server tests](../../crates/barca-server/tests/api.rs) exercise real execution,
slow SSE reads and the canonical outcome; [OpenAPI conformance](../../crates/barca-server/tests/openapi.rs)
keeps native payloads and route inventory checked.

## Open questions and gaps

CLI capability negotiation, successful-event acknowledgement, canonical-outcome polling,
complete-log gap errors and old-server fallback have executable client tests. Browser tests
cover authenticated expired replay, visible gap notice and canonical completion without
resubmission. Python proxy integration covers Basic/Bearer reconnection after a partial frame. Retention/reconnect/auth latency and memory cost probes are
also still required. Current server tests do not prove those client or benchmark obligations.
