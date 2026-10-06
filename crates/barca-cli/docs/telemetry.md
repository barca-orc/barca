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

An unknown name in `BARCA_TELEMETRY` is a warning too. Each integration has 3 seconds to
deliver a run.

## Datadog

Each run is one APM trace, sent to the Datadog Agent's trace intake (the same place `ddtrace`
sends to). No Datadog library is needed, only a reachable Agent.

- The run is the root span, `barca.run`. Its resource is the command and target, for example
  `run publish`.
- Each step is a child span, `barca.step`, with the node id as its resource
  (`pipeline.py:orders`, or `pipeline.py:weekly[week=w1]` for a partition).

Settings are Datadog's own environment variables:

| Variable | Meaning | Default |
|---|---|---|
| `DD_TRACE_AGENT_URL` | `http://host:port` or `unix:///path/to/apm.socket` | unset |
| `DD_AGENT_HOST`, `DD_TRACE_AGENT_PORT` | Agent address when `DD_TRACE_AGENT_URL` is unset | `localhost`, `8126` |
| `DD_SERVICE` | service name on every span | `barca` |
| `DD_ENV`, `DD_VERSION` | `env` and `version` tags | unset |
| `DD_TAGS` | `key:value` pairs, separated by commas or spaces, added to every span | unset |

What the spans carry:

| Span | Tag or metric | Value |
|---|---|---|
| run | `barca.run_id` | the run id `barca get --json` prints (also on every step span) |
| run | `barca.command`, `barca.target` | `get` or `run`, and the target as given |
| run | `barca.status` | `success`, `failed` or `cancelled`; the span is an error unless `success` |
| run | `barca.steps.total`, `barca.steps.executed`, `barca.steps.cached` | step counts |
| step | `barca.node`, `barca.kind` | node id; `asset`, `task` or `sensor` |
| step | `barca.outcome` | `ran`, `cached` or `failed` |
| step | `barca.run_hash` | the step's run hash |
| step | `barca.attempts`, `barca.bytes` | attempts made; size of the result |
| step | `barca.cpu_seconds`, `barca.max_rss_bytes` | when the worker measured them |
| step | `error.type`, `error.message`, `error.stack` | on a failed step: the exception and its traceback |

Timing:

- A step that ran is placed by its worker's clock: the span covers the function and writing its
  result.
- A cached step is a zero-length span at the start of the run.
- A failed step is a zero-length span at the end of the run. Its duration is not reported.

Every run is kept: the trace is sent with sampling priority 1.

Useful things to build on this in Datadog: a monitor on `barca.run` errors per `resource`; a
monitor that a scheduled run's trace arrived in its window; step duration by
`barca.node`; the share of `barca.outcome:cached` per run.

Limits: `https://` Agent URLs are not supported. Uploads to a remote store and the time spent
waiting for them are not spans yet. A run that is killed (not cancelled) sends nothing.

See also: `barca docs scheduling`, `barca docs contract`.
