# Telemetry: runs and steps in Datadog

barca can report every finished run to a telemetry backend. It is off unless you switch an
integration on by name:

```bash
export BARCA_TELEMETRY=datadog
barca run publish pipeline.py
```

`BARCA_TELEMETRY` is a comma-separated list of integration names. `datadog` is the only one
today. It applies to `barca get`, `barca run` and to every run `barca serve` starts, scheduled
ones included.

Telemetry never fails a run. If an integration cannot deliver, the run finishes as it would
have and stderr gets one line:

```
[barca] warning: telemetry 'datadog' did not receive run 0179afe0cb58: cannot reach the Datadog Agent at localhost:8126: Connection refused (os error 61)
```

An unknown name in `BARCA_TELEMETRY`, or an integration whose settings are wrong, is a warning
too, printed once per process. Settings are not checked while the integration is switched off.
Each integration has 3 seconds to deliver a run.

Under `barca serve` a delivery failure is reported when it starts, not on every run, and a
line says when delivery resumes:

```
[barca] telemetry 'datadog' is receiving runs again
```

## Datadog

Each run is one APM trace, sent to the Datadog Agent's trace intake (the same place `ddtrace`
sends to). No Datadog library is needed, only a reachable Agent.

- The run is the root span, `barca.run`. Its resource is the command and target, for example
  `run pipeline.py:publish`. The resource uses resolved node ids, so `publish` and
  `pipeline.py:publish` group as the same job. Multiple targets are sorted; a run without
  targets uses `all`. The original target spelling remains in `barca.target`.
- Each step is a child span, `barca.step`, with the node id as its resource
  (`pipeline.py:orders`, or `pipeline.py:weekly[week=w1]` for a partition).

Settings are Datadog's own environment variables, so a stack that already configures `ddtrace`
services needs only `BARCA_TELEMETRY=datadog` (and usually its own `DD_SERVICE`):

| Variable | Meaning | Default |
|---|---|---|
| `DD_TRACE_AGENT_URL` | `http://host:port` or `unix:///path/to/apm.socket` | unset |
| `DD_AGENT_HOST`, `DD_TRACE_AGENT_PORT` | Agent address when `DD_TRACE_AGENT_URL` is unset | `localhost`, `8126` |
| `DD_TRACE_ENABLED` | when set to anything but `true` or `1`, the Datadog integration is off, silently (as with Python's `ddtrace`) | on |
| `DD_SERVICE` | coordinator service; optional Python spans use this name plus `-python` | `barca` |
| `DD_ENV`, `DD_VERSION` | `env` and `version` tags | unset |
| `DD_TAGS` | `key:value` pairs, separated by commas or spaces, added to every span | unset |

What the spans carry:

| Span | Tag or metric | Value |
|---|---|---|
| run, step | `barca.job` | canonical resolved job ids, sorted and comma-separated, or `all` |
| run | `barca.run_id` | the run id `barca get --json` prints (also on every step span) |
| run | `barca.command`, `barca.target` | `get` or `run`, and the target as given |
| run | `barca.status` | `success`, `failed` or `cancelled`; the span is an error unless `success` |
| run | `barca.steps.total`, `barca.steps.executed`, `barca.steps.cached` | step counts |
| step | `barca.node`, `barca.kind` | node id; `asset`, `task` or `sensor` |
| step | `barca.outcome` | `ran`, `cached` or `failed` |
| step | `barca.run_hash` | the step's run hash |
| step | `barca.attempts`, `barca.bytes` | attempts made (see Limits for partitions); size of the result |
| step | `barca.cpu_seconds`, `barca.max_rss_bytes` | when the worker measured them |
| step | `error.type`, `error.message`, `error.stack` | on a failed step: the exception and its traceback |

Timing:

- A step that ran is placed by its worker's clock: the span covers the function and writing its
  result. For a step that was retried, that is its last attempt.
- A cached step is a zero-length span at the start of the run.
- A failed step is a zero-length span at the end of the run. Its duration is not reported.

Every run is kept: the trace is sent with sampling priority 1. Step spans are marked as
measured, which asks Datadog to compute its trace metrics for `barca.step` as well as
`barca.run`.

Useful things to build on this in Datadog: a monitor on `barca.run` errors per `resource`; a
monitor that a scheduled run's trace arrived in its window; step duration by `barca.node`; the
share of `barca.outcome:cached` per run.

## Python calls within jobs

For Python execution spans and nested library calls, install the optional SDK in the
same Python environment that runs your jobs:

```bash
uv add 'barca[datadog]'
export BARCA_TELEMETRY=datadog
export DD_SERVICE=barca
barca run publish pipeline.py
```

Without the SDK, the run and step timeline above still works. With it, workers enable
Datadog's supported library integrations and create `barca.execute` spans in the
`<DD_SERVICE>-python` service (`barca-python` by default). Their resource is the canonical
job name, for example `pipeline.py:publish`; `barca.node` identifies the step executing.
Each execution is a child of the corresponding `barca.step` in the same trace. SQL, HTTP,
and user-created `ddtrace` spans are nested within that execution. Datadog's standard
Python SDK integration settings apply; arbitrary function calls are not traced automatically.

In APM, select the Python service and the `barca.execute` operation: its resources list
jobs that have executed. Each Python execution span represents one step or attempt; use
`barca.run` for job-run counts. Open a trace to see the run, steps and Python calls. You can also
filter spans by `@barca.job:pipeline.py:publish` or `@barca.run_id:<id>`. APM lists observed
executions; jobs that have never run need Barca's job catalog.

Python execution spans carry `barca.job`, `barca.run_id`, `barca.node`, `barca.kind`,
`barca.outcome`, and the numeric `barca.attempt`. Retries produce separate executions.
`parallel()` children also get Python execution spans, attached to their nearest
non-dynamic ancestor step. Each execution restores the prior Python trace context so
workers can run several steps without mixing their traces.

The SDK flushes before the worker reports completion, including a failed attempt, because
Barca may immediately stop or replace that worker. This adds a delivery round trip per
executed step and can delay jobs when the Agent is unavailable; the SDK's timeout and
retry settings apply. Python spans can arrive before the finished run trace. A killed or
cancelled worker may lose its in-flight Python span. SDK initialization or delivery failures
never change a step's result.

## What is sent

Exception messages and tracebacks are sent as they are (the message cut to its first 2,000
characters and the traceback to its last 4,000, each marked `[cut]`), so
a secret that appears in an error message, and the absolute paths and source lines in a
traceback, reach Datadog. So do node ids, the target as given, run hashes, and store locations
that appear in an upload error. Values of variables declared with `env=[...]` and the contents
of results are not sent by the Rust exporter. When the optional Python SDK is enabled,
its own collection and redaction rules apply to Python spans, including exception
tracebacks, SQL and HTTP metadata; the Rust exporter's error truncation does not apply to them.

## Limits

- A run that fails before it starts sends nothing: a syntax error in the pipeline, an unknown
  target, or a shared-state pull that fails. Under `barca serve` a pipeline broken that way
  produces no error trace, so alert on missing runs as well as on errors.
- A run that is killed sends nothing. A cancelled run is sent, without a span for the step that
  was in flight.
- A step that did not run because an upstream failed has no span.
- `barca.attempts` is absent on a cached step, and on a partition that ran: attempts are
  counted per step, not per key. A partition that failed does carry its own count.
- Calls made through `parallel()` are not in `barca.steps.total` and have no synthetic
  `barca.step` span; the optional Python SDK reports their executions as described above.
- The traceback of a chained exception (`raise ... from e`) does not include its cause.
- A failed upload to a remote store shows the step as failed (`error.type` `UploadError`),
  although its function ran.
- The run is reported before the shared history is pushed. A push that then fails is not
  reflected in the trace.
- Uploads to a remote store, and the time spent waiting for them, are not spans.
- Sending happens at the end of the run and can add up to 3 seconds to it when the Agent does
  not answer. Under `barca serve` that run counts as still going, so a schedule faster than
  that skips ticks. Resolving an Agent host name that does not resolve can keep the process
  alive a little longer than the 3 seconds.
- `https://` Agent URLs and URLs with credentials are not supported. An IPv6 address goes in
  brackets: `http://[::1]:8126`.

See also: `barca docs scheduling`, `barca docs contract`.

## Idle server visibility

With telemetry configured, `barca serve` sends `barca.serve.start` after it binds its
listener, `barca.serve.heartbeat` every five minutes (300 seconds), and a best-effort
`barca.serve.stop` on SIGINT or SIGTERM. These are separate traces with resource `serve`;
they do not create runs, import pipeline modules, or write materialization history.
An idle or read-only server remains visible without running a job. Without telemetry
configured, there are no lifecycle exports or heartbeat tasks.

Each signal includes `barca.lifecycle`, `barca.version`, `barca.read_only`, `barca.watch`,
and `barca.scheduling`, plus numeric `barca.files` and `barca.schedules`. `barca.nodes`
is included only when node metadata is already cached by the server. Schedule counts
reflect the scheduler's current loaded view; an initial signal can precede that view.
Signals omit source paths, storage addresses, credentials, process ids, and run ids.
Use the existing `DD_SERVICE`, `DD_ENV`, and `DD_TAGS` to identify your deployment;
user-supplied tags are forwarded as usual.

Delivery happens in the background and cannot delay readiness. A slow or unavailable
Agent gets at most three seconds per delivery; missed heartbeat ticks are skipped
without queuing retries. Stopping cancels an in-flight start or heartbeat export, and
the final stop signal can add at most three seconds to shutdown. Dropping the server
aborts lifecycle delivery. An abrupt process exit cannot guarantee a stop signal.
Lifecycle delivery failures are warned once until recovery, separately from run
telemetry failures, with diagnostics that omit the Agent address and response text.
