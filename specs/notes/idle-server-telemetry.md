# Idle server telemetry (#313 / roadmap P14)

## Contract

When existing `BARCA_TELEMETRY` configuration enables an integration, a successfully
bound `barca serve` process sends a server-start signal and a heartbeat every five
minutes. Shutdown stops heartbeats immediately and may send one bounded stop signal.
Unconfigured or explicitly disabled telemetry performs no export or periodic work.
There are no new configuration switches, synthetic runs or materialization records.

The public Python/CLI/HTTP surfaces stay unchanged. Extend the existing Rust telemetry
integration boundary with a typed server report and a default no-op server export,
rather than repurpose `RunReport` or call Datadog directly from server lifecycle code.

## Delivery and lifecycle

- Start telemetry only after the listener binds successfully. Do not delay readiness
  for telemetry delivery. Own the task for the server lifetime; cancel it on stop,
  I/O failure or future drop so an embedded server cannot leave a heartbeat behind.
- Emit a fixed `barca.serve.start`, `barca.serve.heartbeat` or `barca.serve.stop` span
  with resource `serve`. Each export has the existing three-second bound. In-flight
  export and interval waits observe cancellation; do not accumulate retries/queues.
- Use a fixed five-minute interval with missed ticks skipped. Sequential delivery
  bounds concurrency to one export task per server, independently of run telemetry.
- An optional final stop export must never extend shutdown beyond three seconds.
  A server's death is ultimately the absence of heartbeats, not guaranteed stop delivery.
- Reuse existing Datadog environment configuration and intake/transport. Explicit
  `DD_TRACE_ENABLED` disable remains effective. Preserve existing run spans and warnings.

## Payload and privacy

Automatic attributes are bounded: lifecycle state, Barca version, node/schedule counts
when static metadata is available, read-only/watch/scheduling modes. Use a fixed resource;
never include project root/path, source-file lists, URI/options/credentials, run IDs,
partition keys or PID as tags. Instance identity, if required, belongs to Datadog's existing
agent/host/service/environment configuration rather than a new Barca deployment concept.
User-selected `DD_TAGS` retain existing behavior; operators control those tags.

Read counts only from already cached server metadata using nonblocking lock attempts.
Never initiate parsing, file I/O, dynamic-partition resolution or user Python imports for
telemetry. Omit unavailable node counts; schedule count reflects the loaded scheduler
view and can initially be zero. Counts are metrics, not resource names.

## Evidence before merge

1. A real idle HTTP server sends a start payload to a local fake Datadog Agent, with
   no user imports, runs or metadata DB writes. A disabled integration sends nothing.
2. Deterministic timer tests prove five-minute cadence, no initial duplicate heartbeat,
   missed-tick behavior and no emission after cancellation/stop. Tests may inject an
   internal interval or use a paused Tokio clock; no production configuration is added.
3. A stalled/refusing Agent cannot delay HTTP readiness or run handling. Shutdown and
   telemetry-task cleanup complete within the existing bound; cancellation aborts delivery.
4. Payload tests assert fixed names/resource and no paths, credentials, run/partition/PID
   tags. Existing run telemetry tests and delivery transports remain green.
5. Update the embedded telemetry manual and matching site documentation with
   the opt-in behavior, exact cadence, delivery limit and limits of missing-heartbeat alerts.

P13 may later replace the shutdown lifecycle but must preserve telemetry task ownership.
OTLP/metrics (#59) and user-recorded measurements are separate work.
