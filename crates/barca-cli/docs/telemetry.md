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
  `run publish`.
- Each step is a child span, `barca.step`, with the node id as its resource
  (`pipeline.py:orders`, or `pipeline.py:weekly[week=w1]` for a partition).

Settings are Datadog's own environment variables, so a stack that already configures `ddtrace`
services needs only `BARCA_TELEMETRY=datadog` (and usually its own `DD_SERVICE`):

| Variable | Meaning | Default |
|---|---|---|
| `DD_TRACE_AGENT_URL` | `http://host:port` or `unix:///path/to/apm.socket` | unset |
| `DD_AGENT_HOST`, `DD_TRACE_AGENT_PORT` | Agent address when `DD_TRACE_AGENT_URL` is unset | `localhost`, `8126` |
| `DD_TRACE_ENABLED` | when set to anything but `true` or `1`, the Datadog integration is off, silently (as with Python's `ddtrace`) | on |
| `DD_SERVICE` | service name on every span | `barca` |
| `DD_ENV`, `DD_VERSION` | `env` and `version` tags | unset |
| `DD_TAGS` | `key:value` pairs, separated by commas or spaces, added to every span | unset |

What the spans carry:

| Span | Tag or metric | Value |
|---|---|---|
| run | `barca.run_id` | the run id `barca get --json` prints (also on every step span) |
| run | `barca.command`, `barca.target` | `get`, `run` or `serve`, and the target as given (several targets: comma-separated). `serve` is a `barca serve` scheduled run over both assets and tasks (`barca docs scheduling`) |
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

## What is sent

Exception messages and tracebacks are sent as they are (the message cut to its first 2,000
characters and the traceback to its last 4,000, each marked `[cut]`), so
a secret that appears in an error message, and the absolute paths and source lines in a
traceback, reach Datadog. So do node ids, the target as given, run hashes, and store locations
that appear in an upload error. Values of variables declared with `env=[...]` and the contents
of results are not sent.

## Limits

- A run that fails before it starts sends nothing: a syntax error in the pipeline, an unknown
  target, or a shared-state pull that fails. Under `barca serve` a pipeline broken that way
  produces no error trace, so alert on missing runs as well as on errors.
- A run that is killed sends nothing. A cancelled run is sent, without a span for the step that
  was in flight.
- A step that did not run because an upstream failed has no span.
- `barca.attempts` is absent on a cached step, and on a partition that ran: attempts are
  counted per step, not per key. A partition that failed does carry its own count.
- Calls made through `parallel()` inside a step have no spans of their own, and are not in
  `barca.steps.total`.
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
- The run's resource is the target as it was given: `run publish` from the CLI,
  `run pipeline.py:publish` for a scheduled run of the same task.
- `https://` Agent URLs and URLs with credentials are not supported. An IPv6 address goes in
  brackets: `http://[::1]:8126`.

See also: `barca docs scheduling`, `barca docs contract`.
