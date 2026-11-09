---
title: "Telemetry (Datadog)"
description: "Report every step and every run to Datadog as a trace of its own, and barca serve as DogStatsD metrics and JSON logs."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/telemetry.md
---

<!-- Generated from crates/barca-cli/docs/telemetry.md (barca docs telemetry). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

barca can report every step and every run to a telemetry backend, and post to Slack when a job
starts failing or recovers ([Slack Alerts](/reference/slack/); it has its own switch). Each backend is off
unless you switch it on. Datadog is switched on by name:

```bash
export BARCA_TELEMETRY=datadog
barca run publish pipeline.py
```

`BARCA_TELEMETRY` is a comma-separated list of integration names. `datadog` is the only one
it takes. It applies to `barca get`, `barca run` and to every run `barca serve` starts, scheduled
ones included.

Telemetry never fails a run. If an integration cannot deliver, the run finishes as it would
have and stderr gets one line, naming the first delivery that failed (a step's or the run's):

```
[barca] warning: telemetry 'datadog' did not receive step pipeline.py:orders: cannot reach the Datadog Agent at localhost:8126: Connection refused (os error 61)
```

An unknown name in `BARCA_TELEMETRY`, or an integration whose settings are wrong, is a warning
too, printed once per process. Settings are not checked while the integration is switched off.
Each delivery has 3 seconds.

A delivery failure is reported when it starts, not on every step or run, and a line says when
delivery resumes:

```
[barca] telemetry 'datadog' is receiving runs again
```

## Datadog

Every step that runs is one APM trace, and every run one more, sent to the Datadog Agent's
trace intake (the same place `ddtrace` sends to). No Datadog library is needed, only a
reachable Agent. An asset DAG has thousands of assets and partitions, so a run of it is not one
request but thousands: each step execution is traced as a web service traces a request, with
its own latency, error rate and throughput per asset on the service page, and the run is a
small trace that says how the whole thing went.

```
barca.step       orders                   one trace per step that ran or failed (the root)
  barca.execute  orders                   the Python call (optional ddtrace in the workers)
    sqlite.query                          library spans, when opted in

barca.step       regional                 one trace per partition key; the key is a tag

barca.run        get daily_revenue        one trace per run; error.message names the first failed asset
```

- **One service.** Every span, Python and library spans included, reports as `DD_SERVICE`
  (default `barca`), with `type: worker` and `span.kind: internal`. Coordinator spans carry
  `component: barca`, Python spans `component: python`. Every span carries `env` and
  `version` (from `DD_ENV`, `DD_VERSION`) and `barca.version`, barca's own version.
- **A step** is the root span of its own trace, `barca.step`, whose resource is the node's
  short name (`orders`). Short names are unique in a project; when two files define a node of
  the same name, both are named by their id (`a.py:orders`, `b.py:orders`). Its trace is sent
  the moment the step ends, so a failure is in Datadog seconds after it happens, hours before a
  long run finishes, and a run that is killed has sent every step that finished.
- **A partition key is a step of its own**, and never in a resource: a node with 20,000 keys is
  one resource, `regional`, with a trace per key behind it, the key in `barca.partition` and in
  `barca.node_id` (`pipeline.py:regional[region=apac]`). There is no cap on keys.
- **A cached step sends nothing**, nor does a skipped one (planned, but something it depends on
  failed or the run stopped first): the run span counts them (`barca.steps.cached`,
  `barca.steps.skipped`), and a step that did no work makes no trace.
- **The run** is one span, `barca.run`, the root of a trace of its own with no children. Its
  resource is the command and the targets' short names, `run publish` or
  `get daily_revenue,orders`; a run of everything in its files is `get pipeline.py`, and one
  with no file named `get all`. `publish` and `pipeline.py:publish` are the same resource. The
  target as typed stays in `barca.target`. A scheduled tick is one run with several targets, the
  scheduled node and the `Always` nodes below it ([Scheduling With Cron](/scheduling/)): its resource names
  all of them (`get inbox,orders`), its `barca.job` is the scheduled node and its
  `barca.trigger` is `schedule`. It is sent when the run ends, after its steps.
- **A run's traces share `barca.run_id`.** `@barca.run_id:0179afe0cb58` lists a run's steps
  beside its run span; `@barca.job:pipeline.py:publish` every step of a job over time. Trace
  ids are deterministic in the run and the step, so a worker's Python spans, the step's log
  lines and the coordinator's root span agree without a message between them.

Settings are Datadog's own environment variables, so a stack that already configures `ddtrace`
services needs only `BARCA_TELEMETRY=datadog` (and usually its own `DD_SERVICE`):

| Variable | Meaning | Default |
|---|---|---|
| `DD_TRACE_AGENT_URL` | `http://host:port` or `unix:///path/to/apm.socket` | unset |
| `DD_AGENT_HOST`, `DD_TRACE_AGENT_PORT` | Agent address when `DD_TRACE_AGENT_URL` is unset | `localhost`, `8126` |
| `DD_TRACE_ENABLED` | when set to anything but `true` or `1`, the Datadog integration is off, silently (as with Python's `ddtrace`) | on |
| `DD_SERVICE` | the service of every span and metric | `barca` |
| `DD_ENV`, `DD_VERSION` | `env` and `version` tags | unset |
| `DD_TAGS` | `key:value` pairs, separated by commas or spaces, added to every span and metric | unset |
| `DD_DOGSTATSD_URL` | where `barca serve` metrics go: `udp://host:port` or `unix:///path/to/dsd.socket` | unset |
| `DD_DOGSTATSD_SOCKET` | a DogStatsD Unix socket, when `DD_DOGSTATSD_URL` is unset | unset |
| `DD_DOGSTATSD_PORT` | DogStatsD port on `DD_AGENT_HOST`, when neither is set | `8125` |
| `BARCA_TRACE_LIBRARIES` | `1`: patch every library `ddtrace` supports in the workers (see below) | off |

What the spans carry:

| Span | Tag or metric | Value |
|---|---|---|
| all | `barca.run_id` | the run id `barca get --json` prints |
| all | `barca.version`, `barca.project` | barca's version; the project root directory's name |
| run, step | `barca.job` | canonical resolved job ids, sorted and comma-separated, or `all` |
| run | `barca.command`, `barca.target` | `get` or `run`, and the target as given |
| run | `barca.targets`, `barca.files` | the targets' short names; the files the command was given |
| run | `barca.status` | `success`, `failed` or `cancelled`; the span is an error unless `success`. `skipped` for a `barca serve` tick that started nothing, not an error |
| run | `barca.failed_asset`, `barca.failed_node_id`, `error.type`, `error.message` | on a failed run: the first step that failed, its exception type, and `<asset>: <message>` (`regional[region=apac]: no data for apac` for a key), so the trace list says why. A cancelled run has `error.type` `Cancelled`; a run that failed outside its steps `RunFailed` |
| run | `barca.reason`, `barca.in_flight_run` | on a skipped tick only: `in_flight` and the `run_id` of the run of the same node that was still going, or `paused` for a paused schedule (no `barca.in_flight_run`). The span has no steps, no duration, and an id of its own |
| run | `barca.reason` | on a `cancelled` run that `barca serve` cancelled at a second SIGTERM/SIGINT during its drain: `server shutdown`; on the stopped attempt of a run the drain timeout put back in the queue: `server restart (requeued)` (the run itself starts again after the restart). Other cancelled runs have no reason |
| run | `barca.trigger` | what started the run: `manual` (a command typed on a machine), `api` (an HTTP request to `barca serve`), `schedule` (a cron tick) or `client` |
| run | `barca.scheduled_at`, `barca.schedule_id` | on a scheduled run or skipped tick only: the tick it was for (RFC 3339, UTC) and the scheduled node's id. `barca.run` start minus `barca.scheduled_at` is how late the run started |
| run | `barca.steps.total`, `.executed`, `.cached`, `.failed`, `.skipped` | step counts; a partition key counts as a step |
| step | `barca.asset`, `barca.node_id` | the node's short name; the step's full id, with its partition key (`pipeline.py:regional[region=apac]`) |
| step | `barca.file`, `barca.kind` | the file that defines the node; `asset`, `task` or `sensor` |
| step | `barca.group` | the `group()`s the node is in, outermost first, joined by `/` (`Company/Sales`); absent outside every group |
| step | `barca.freshness`, `barca.schedule` | `always`, `manual` or `schedule`; the cron expression of a scheduled node |
| step | `barca.upstream` | the node's direct upstream node ids, sorted and comma-separated; absent on a node with none |
| step | `barca.outcome` | `ran` or `failed`: a cached or skipped step has no trace |
| step | `barca.partition` | on a partition key: the key, `region=emea` (`key=value` pairs joined by `,`) |
| step | `barca.run_hash`, `barca.digest` | the step's run hash; on a step that ran, SHA-256 of the bytes it wrote, the `content_hash` `barca status --json` shows |
| step | `barca.attempts`, `barca.bytes` | attempts the step took, retries included; the size of its result |
| step | `barca.cpu_seconds`, `barca.max_rss_bytes` | when the worker measured them |
| step | `error.type`, `error.message`, `error.stack` | on a failure: the exception and its traceback |

Timing:

- A step that ran is placed by its worker's clock: the span covers the function and writing its
  result. For a step that was retried, that is its last attempt.
- A failed step carries its duration: the span runs from the start of its first attempt to the
  moment it failed for good, so retries and the wait between them are inside it, and a step that
  timed out lasts about its `timeout_seconds`. This is wall time, so it can be longer than the
  step's `elapsed_seconds` in `barca get --json`, which adds up only the time the attempts ran.
- A step whose function ran but whose upload to a remote store failed (`UploadError`) is
  placed by its worker's clock like a step that ran.
- The run span covers the run; its steps' traces are not inside it (they are traces of their
  own), so the run's waterfall is one span.

Every trace is kept: each is sent with sampling priority 2 (kept by the user, `_dd.p.dm:-4`).
Trace ids are 128 bits: the span's `trace_id` is the lower half and `_dd.p.tid` the upper. A
`barca.step` span is the entry span of its trace, so Datadog computes trace metrics (hits,
errors, latency) for it per `resource`, the asset, as it does for an endpoint.

Useful things to build on this in Datadog: the service page's resource list is the assets, with
requests, errors and latency per asset; a monitor on `barca.step` errors per `resource`; a
monitor on `barca.run` errors per `resource`, or that a scheduled run's trace arrived in its
window; `@barca.run_id:<id>` for everything one run did; `@barca.group:*Sales*` to see one
group and the groups in it; `@barca.partition:region=apac` for one key's history;
`@barca.outcome:failed @barca.asset:regional` for a node's failed keys.

## Python calls within jobs

For Python execution spans, install the optional SDK in the same Python environment that runs
your jobs:

```bash
uv add 'barca[datadog]'
export BARCA_TELEMETRY=datadog
export DD_SERVICE=barca
barca run publish pipeline.py
```

Without the SDK, the step and run traces above still work. With it, each step's Python call
is a `barca.execute` span, a child of its `barca.step` in the step's trace and the same service.
Its resource is the node's short name, like the step's, and it
carries `component: python`, `barca.asset`, `barca.node_id`, `barca.file`, `barca.kind`,
`barca.group`, `barca.partition`, `barca.job`, `barca.run_id`, `barca.version`,
`barca.outcome` and the numeric `barca.attempt`. Retries produce separate executions.
`parallel()` children also get Python execution spans, attached to their nearest
non-dynamic ancestor step. Spans you create with `ddtrace` nest inside the execution. Each
execution restores the prior Python trace context so workers can run several steps without
mixing their traces.

**Library spans are opt-in.** barca does not patch every library a step imports; choose them
with `ddtrace`'s own settings, or patch all of them:

```bash
export DD_PATCH_MODULES=sqlite3:true,requests:true   # these libraries
export DD_TRACE_PSYCOPG_ENABLED=true                 # or one at a time
export BARCA_TRACE_LIBRARIES=1                       # or every library ddtrace supports
```

A patched library's spans are children of the `barca.execute` span and keep `DD_SERVICE`: the
worker sets `DD_TRACE_REMOVE_INTEGRATION_SERVICE_NAMES_ENABLED=true` (unless you set it) before
`ddtrace` loads, so `sqlite`, `requests` and the like do not become services of their own.

The SDK flushes before the worker reports completion, including a failed attempt, because
barca may immediately stop or replace that worker. This adds a delivery round trip per
executed step and can delay jobs when the Agent is unavailable; the SDK's timeout and
retry settings apply. A step's Python spans can arrive before its `barca.step` root. A killed or
cancelled worker may lose its in-flight Python span. SDK initialization or delivery failures
never change a step's result.

## Logs under `barca serve`

By default `barca serve` writes its lines as text on stderr, as `get` and `run` do. A container
log collector that files stderr as errors then tags every one of them, a step that completed
included, as an error, and nothing in them names the run. `BARCA_LOG=json` makes `barca serve`
write each of its lines as one JSON object on stdout instead:

```bash
export BARCA_LOG=json
export BARCA_TELEMETRY=datadog
barca serve pipeline.py
```

```
{"ts":"2026-10-20T19:45:44.006Z","level":"info","msg":"scheduled run pipeline.py:heartbeat → 2b585caaa002","node":"pipeline.py:heartbeat"}
{"ts":"2026-10-20T19:45:44.067Z","level":"info","msg":"step:pipeline.py:orders completed 0.0s (1/2)","run_id":"2b585c3e0785","node":"pipeline.py:orders","dd.trace_id":"8371769010332850555","dd.span_id":"1894635935906895539"}
{"ts":"2026-10-20T19:45:44.068Z","level":"info","msg":"tick 3","run_id":"2b585c3e0785","node":"pipeline.py:heartbeat","dd.trace_id":"8371769010332850555","dd.span_id":"2430261483699309868"}
{"ts":"2026-10-20T19:45:44.073Z","level":"info","msg":"2/2 steps | done in 0.0s","run_id":"2b585c3e0785","dd.trace_id":"8371769010332850555","dd.span_id":"1993890634025969937"}
```

| Field | Value |
|---|---|
| `ts` | when the line was written, UTC with milliseconds |
| `level` | `error` for a failure (`step:<id> failed: ...`, `... steps \| failed in ...`, an error that stops the server), `warning` for a warning, a skipped tick or a cancelled run, `info` for everything else |
| `msg` | the line as barca prints it without `BARCA_LOG`, less its `[barca] ` prefix |
| `run_id` | the run the line belongs to: the `barca.run_id` of its spans and the id `barca logs` and `barca history` take. Absent on server lines (startup, schedule ticks, reloads) |
| `node` | the step the line is about: on `step:` lines, on a step's output and on a schedule tick's lines |
| `dd.trace_id`, `dd.span_id` | with the Datadog integration on: the ids of the trace and span the line belongs to, as decimal strings. A line about a step (a partition key is a step of its own) joins the step's trace at its `barca.step` span; a line with no node joins the run's trace at its `barca.run` span. Datadog's log pipeline reads these attributes as the trace and span ids, so a line links to its trace |

What a step prints (its stdout) is an `info` line with its run and node. Without `BARCA_LOG`
`barca serve` does not write a step's output at all; it is kept with the run either way
(`barca logs <run_id>`).

`BARCA_LOG` changes only `barca serve`. `get`, `run` and every other command keep progress and
errors on stderr as text; given `BARCA_LOG=json` they say so once
(`[barca] warning: BARCA_LOG applies to barca serve only; this command writes its usual lines`)
and change nothing else. `BARCA_LOG=text`, or unset, is the default; another value stops
`barca serve` with a usage error (exit 2).

Limits:

- What a step writes to its stderr is not barca's line and stays on stderr as it is: Python's
  `logging` writes there by default, and so does a traceback from a worker that died. It is
  recorded with the run all the same (`barca logs <run_id> --stream stderr`, [barca logs](/reference/logs/)).
- A notice from a worker process, such as `importing <file>`, is a JSON line without a run or
  a node.
- `level` is read from the line, so a new kind of line may be filed at `info` until barca
  learns it.

## A step's trace from the web UI and `barca history`

A run traced with `BARCA_TELEMETRY=datadog` is recorded as traced in the metadata database, and
each step that ran or failed is listed with the trace it was sent as. Trace ids are derived from
the run id and the step, so nothing extra crosses the worker protocol and nothing is asked of
Datadog:

- `barca history <run_id> --json` and `GET /runs/{run_id}` give such a step `trace_id` (the
  128-bit id in 32 hex digits) and `span_id` (its `barca.step` root span, in decimal). The keys
  are absent for a cached or skipped step, for a run without the integration and for a run
  recorded by a barca before 0.41.0. Experimental.
- `GET /project` names the Datadog web app in `telemetry.datadog_url`: from `DD_SITE`, else
  `[telemetry] datadog_site` in `barca.toml`, else `https://app.datadoghq.com` when
  `BARCA_TELEMETRY=datadog` is set for the server; `null` otherwise. A two-label site gets
  `app.` (`datadoghq.eu` opens `https://app.datadoghq.eu`), a regional one is used as it is
  (`us3.datadoghq.com`). A value that is not a host name stops `barca serve` with exit 2.

```toml
[telemetry]
datadog_site = "datadoghq.eu"
```

The Runs page then shows **View trace** on each step row and on each failed step at the top of
the run panel, opening `<datadog_url>/apm/trace/<trace_id>`, and **Copy incident summary** adds
a `[trace](...)` link to each failed step. Without a Datadog URL, or for a step without a trace
id, neither is shown. A run a laptop posted to a server's ledger (`triggered_by: client`) has no
trace ids on the server.

## What is sent

Exception messages and tracebacks are sent as they are (the message cut to its first 2,000
characters and the traceback to its last 4,000, each marked `[cut]`), so
a secret that appears in an error message, and the absolute paths and source lines in a
traceback, reach Datadog. So do node ids, partition keys, group names, cron expressions, the
target as given, the files named on the command line, the project directory's name, run hashes,
and store locations that appear in an upload error. Values of variables declared with `env=[...]` and the contents
of results are not sent by the Rust exporter. When the optional Python SDK is enabled,
its own collection and redaction rules apply to Python spans, including exception
tracebacks, SQL and HTTP metadata; the Rust exporter's error truncation does not apply to them.

## Limits

- A run that fails before it starts sends nothing: a syntax error in the pipeline, an unknown
  target, or an artifact store that cannot be reached. Under `barca serve` a pipeline broken that way
  produces no error trace, so alert on missing runs as well as on errors.
- A run that is killed has sent the traces of the steps that finished; its run trace, and the
  step in flight, are lost. A cancelled run sends its run trace, without the step that was in
  flight.
- A skipped step has no trace; the run's `barca.steps.skipped` counts it, by node for a node
  none of whose keys ran and by key for the keys of a node some of whose keys ran.
- On a skipped tick's span, `barca.targets` is the node's name from its id, without the
  same-name check the other spans get.
- Calls made through `parallel()` are not in `barca.steps.total` and have no trace of their
  own; the optional Python SDK reports their executions inside the calling step's trace.
- A step's trace is one request to the Agent. A run of 20,000 keys makes 20,000 requests over
  its course, as 20,000 web requests would; Datadog's ingestion controls apply per `resource`.
- The traceback of a chained exception (`raise ... from e`) does not include its cause.
- A failed upload to a remote store is not in the trace: the step shows as succeeded, and
  the upload is retried by the next command ([Remote Storage](/reference/remote-storage/)).
- The run is reported before the history is backed up. A backup that then fails is not
  reflected in the trace.
- Uploads to a remote store, and the time spent waiting for them, are not spans.
- A step's trace is sent in the background and never delays the run. The run's trace is sent at
  the end and can add up to 3 seconds to it when the Agent does not answer. Under `barca serve`
  that run counts as still going, so a schedule faster than that skips ticks. Resolving an Agent
  host name that does not resolve can keep the process alive a little longer than the 3 seconds.
- `https://` Agent URLs and URLs with credentials are not supported. An IPv6 address goes in
  brackets: `http://[::1]:8126`.

## Metrics from `barca serve`

With the Datadog integration on, `barca serve` reports itself as DogStatsD metrics, not traces,
so an idle or read-only server stays visible without running a job and without filling APM with
one-span traces. It sends, over UDP to the Agent's DogStatsD port (or its Unix socket):

| Metric | Type | Value |
|---|---|---|
| `barca.serve.up` | gauge | `1` once it listens and every five minutes (300 seconds) after; `0` on a SIGINT or SIGTERM stop |
| `barca.serve.schedules` | gauge | schedules in the scheduler's current view (an initial signal can precede it) |
| `barca.serve.files` | gauge | pipeline files served |
| `barca.serve.nodes` | gauge | nodes, only when the server has already loaded its graph |

Each is tagged `service`, `env`, `version`, `DD_TAGS`, `barca.version`, `barca.project`,
`barca.read_only`, `barca.watch` and `barca.scheduling`. A Datadog event, `barca serve started`
or `barca serve stopped`, goes with the first and the last. A monitor on
`barca.serve.up` that alerts when the metric stops arriving or reads 0 catches a server that is
down. These signals do not create runs, import pipeline modules, or write materialization
history; they omit source paths, storage addresses, credentials, process ids and run ids.
Without telemetry configured there are no lifecycle signals and no heartbeat task.

The address is `DD_DOGSTATSD_URL` (`udp://host:port` or `unix:///path`), else
`DD_DOGSTATSD_SOCKET`, else `DD_AGENT_HOST` and `DD_DOGSTATSD_PORT` (default `localhost:8125`).
Datagrams are fire-and-forget: a stopped Agent loses them without a warning, as with every
DogStatsD client. Delivery happens in the background and cannot delay readiness; the final
stop signal can add at most three seconds to shutdown, and an abrupt process exit cannot
guarantee it. A misconfigured address is warned once until it recovers, separately from run
telemetry failures, with diagnostics that omit the address.

## Slack

Posting to Slack when a job starts failing, recovers or is still failing a day later has its own
switch, `BARCA_SLACK_WEBHOOK_URL`, and its own topic: [Slack Alerts](/reference/slack/).

See also: [Scheduling With Cron](/scheduling/), [Groups](/reference/groups/), [Partitions](/reference/partitions/),
[CLI Contract](/reference/cli-contract/).

---

*The same text is in your terminal: `barca docs telemetry`.*
