---
title: "Telemetry"
description: "Report every run to Datadog as a trace of the pipeline and barca serve as DogStatsD metrics, and post to Slack when a job starts failing or recovers."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/telemetry.md
---

<!-- Generated from crates/barca-cli/docs/telemetry.md (barca docs telemetry). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

:::note
This page is the manual topic `barca docs telemetry`: the CLI prints the same text.
:::

barca can report every finished run to a telemetry backend, and post to Slack when a job starts
failing or recovers (see the Slack section below; it has its own switch). Each backend is off
unless you switch it on. Datadog is switched on by name:

```bash
export BARCA_TELEMETRY=datadog
barca run publish pipeline.py
```

`BARCA_TELEMETRY` is a comma-separated list of integration names. `datadog` is the only one
it takes. It applies to `barca get`, `barca run` and to every run `barca serve` starts, scheduled
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
sends to). No Datadog library is needed, only a reachable Agent. The trace is the pipeline as it
ran: which assets, files and groups ran, what each cost, and what failed.

```
barca.run        get daily_revenue        the run; error.message names the first failed asset
  barca.step     orders                   one per node
    barca.execute  orders                 the Python call (optional ddtrace in the workers)
  barca.step     regional                 a partitioned node: counts its keys
    barca.partition  regional             one per key that ran (up to 200) or failed
      barca.execute  regional
        sqlite.query                      library spans, when opted in
```

- **One service.** Every span, Python and library spans included, reports as `DD_SERVICE`
  (default `barca`), with `type: worker` and `span.kind: internal`. Coordinator spans carry
  `component: barca`, Python spans `component: python`. Every span carries `env` and
  `version` (from `DD_ENV`, `DD_VERSION`) and `barca.version`, barca's own version.
- **The run** is the root span, `barca.run`. Its resource is the command and the targets'
  short names, `run publish` or `get daily_revenue,orders`; a run of everything in its files
  is `get pipeline.py`, and one with no file named `get all`. `publish` and
  `pipeline.py:publish` are the same resource. The target as typed stays in `barca.target`.
  A scheduled tick is one run with several targets, the scheduled node and the `Always` nodes
  below it ([`barca docs scheduling`](/scheduling/)): its resource names all of them (`get inbox,orders`), its
  `barca.job` is the scheduled node and its `barca.trigger` is `schedule`.
- **A step** is a child span, `barca.step`, one per node, whose resource is the node's short
  name (`orders`). Short names are unique in a project; when two files define a node of the same
  name, both are named by their id (`a.py:orders`, `b.py:orders`).
- **A partition key is never in a resource.** A partitioned node is one `barca.step` that counts
  its keys (`barca.partitions.*`), with a `barca.partition` child for each key that ran, up to
  200 keys per node and 2,000 per run, and for every key that failed. The cap keeps a
  20,000-key node one readable span with bounded payloads; a key past it has no span of its own
  and no Python span, and its log lines join the node's span.
- Cached keys have no span; `barca.partitions.cached` counts them.

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
| run | `barca.reason` | on a `cancelled` run that `barca serve` cancelled because its drain ended first (the drain timeout, or a second SIGTERM/SIGINT): `server shutdown`. Other cancelled runs have no reason |
| run | `barca.trigger` | what started the run: `manual` (a command typed on a machine), `api` (an HTTP request to `barca serve`), `schedule` (a cron tick) or `client` |
| run | `barca.scheduled_at`, `barca.schedule_id` | on a scheduled run or skipped tick only: the tick it was for (RFC 3339, UTC) and the scheduled node's id. `barca.run` start minus `barca.scheduled_at` is how late the run started |
| run | `barca.steps.total`, `.executed`, `.cached`, `.failed`, `.skipped` | step counts; a partition key counts as a step |
| step, partition | `barca.asset`, `barca.node_id` | the node's short name; its full id (with the key on a `barca.partition` span) |
| step | `barca.file`, `barca.kind` | the file that defines the node; `asset`, `task` or `sensor` |
| step | `barca.group` | the `group()`s the node is in, outermost first, joined by `/` (`Company/Sales`); absent outside every group |
| step | `barca.freshness`, `barca.schedule` | `always`, `manual` or `schedule`; the cron expression of a scheduled node |
| step | `barca.upstream` | the node's direct upstream node ids, sorted and comma-separated; absent on a node with none |
| step, partition | `barca.outcome` | `ran`, `cached`, `failed`, or `skipped` (planned, but something it depends on failed or the run stopped first; a zero-length span at the end of the run). A partitioned node is `failed` if any key failed, else `ran` if any ran |
| step | `barca.cached` | `true` for a cached step |
| partition | `barca.partition` | the key, `region=emea` (`key=value` pairs joined by `,`) |
| step, partition | `barca.run_hash`, `barca.digest` | the step's run hash; on a step that ran, SHA-256 of the bytes it wrote, the `content_hash` `barca status --json` shows |
| step, partition | `barca.attempts`, `barca.bytes` | attempts made (see Limits for partitions); size of the result, summed over a node's keys |
| step, partition | `barca.cpu_seconds`, `barca.max_rss_bytes` | when the worker measured them; summed (CPU) or the largest (memory) over a node's keys |
| step | `barca.partitions.total`, `.executed`, `.cached`, `.failed` | on a partitioned node: its keys in this run, and how each ended |
| step, partition | `error.type`, `error.message`, `error.stack` | on a failure: the exception and its traceback. A partitioned node's span names the first failed key and how many failed |

Timing:

- A step that ran is placed by its worker's clock: the span covers the function and writing its
  result. For a step that was retried, that is its last attempt. A partitioned node's span runs
  from its first key's start to its last key's end.
- A cached step is a zero-length span at the start of the run.
- A failed step carries its duration: the span runs from the start of its first attempt to the
  moment it failed for good, so retries and the wait between them are inside it, and a step that
  timed out lasts about its `timeout_seconds`. This is wall time, so it can be longer than the
  step's `elapsed_seconds` in `barca get --json`, which adds up only the time the attempts ran.
- A step whose function ran but whose upload to a remote store failed (`UploadError`) is
  placed by its worker's clock like a step that ran.

Every run is kept: the trace is sent with sampling priority 2 (kept by the user, `_dd.p.dm:-4`).
Trace ids are 128 bits: the span's `trace_id` is the lower half and `_dd.p.tid` the upper. Step
spans are marked as measured, which asks Datadog to compute its trace metrics for `barca.step`
(per node) as well as `barca.run`.

Useful things to build on this in Datadog: a monitor on `barca.run` errors per `resource`; a
monitor that a scheduled run's trace arrived in its window; step duration by `resource` (the
node); `@barca.group:*Sales*` to see one group and the groups in it; the share of `@barca.outcome:cached` per
run; `@barca.partitions.failed:>0` for partitioned nodes with failed keys.

## Python calls within jobs

For Python execution spans, install the optional SDK in the same Python environment that runs
your jobs:

```bash
uv add 'barca[datadog]'
export BARCA_TELEMETRY=datadog
export DD_SERVICE=barca
barca run publish pipeline.py
```

Without the SDK, the run and step timeline above still works. With it, each step's Python call
is a `barca.execute` span, a child of its `barca.step` (or of its key's `barca.partition`) in the
same trace and the same service. Its resource is the node's short name, like the step's, and it
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
retry settings apply. Python spans can arrive before the finished run trace. A killed or
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
| `dd.trace_id`, `dd.span_id` | with the Datadog integration on: the run's trace id (its lower 64 bits) and the id of the step's span (a partition key's own span when it has one, else its node's), or of the run span on a line with no node, as decimal strings. Datadog's log pipeline reads these attributes as the trace and span ids, so a line links to its trace |

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
  recorded with the run all the same (`barca logs <run_id> --stream stderr`, `barca docs logs`).
- A notice from a worker process, such as `importing <file>`, is a JSON line without a run or
  a node.
- `level` is read from the line, so a new kind of line may be filed at `info` until barca
  learns it.

## What is sent

Exception messages and tracebacks are sent as they are (the message cut to its first 2,000
characters and the traceback to its last 4,000, each marked `[cut]`; a node's failed keys after
its first 20 carry 200 characters of message and no traceback), so
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
- A run that is killed sends nothing. A cancelled run is sent, without a span for the step that
  was in flight.
- A skipped step is reported for a node none of whose keys ran; a partitioned node some of whose
  keys ran before the run stopped is reported by those keys, without a count of the rest.
- A partition key past the cap (200 per node, 2,000 per run) has no `barca.partition` span and no
  Python span; it is counted in its node's `barca.partitions.*`. A failed key always has a span.
  A trace larger than 4 MB is sent in several requests to the Agent, as trace chunks.
- On a skipped tick's span, `barca.targets` is the node's name from its id, without the
  same-name check the other spans get.
- `barca.attempts` is absent on a cached step, and on a partition that ran: attempts are
  counted per step, not per key. A partition that failed does carry its own count.
- Calls made through `parallel()` are not in `barca.steps.total` and have no synthetic
  `barca.step` span; the optional Python SDK reports their executions as described above.
- The traceback of a chained exception (`raise ... from e`) does not include its cause.
- A failed upload to a remote store is not in the trace: the step shows as succeeded, and
  the upload is retried by the next command ([`barca docs remote`](/reference/remote-storage/)).
- The run is reported before the history is backed up. A backup that then fails is not
  reflected in the trace.
- Uploads to a remote store, and the time spent waiting for them, are not spans.
- Sending happens at the end of the run and can add up to 3 seconds to it when the Agent does
  not answer. Under `barca serve` that run counts as still going, so a schedule faster than
  that skips ticks. Resolving an Agent host name that does not resolve can keep the process
  alive a little longer than the 3 seconds.
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

Set `BARCA_SLACK_WEBHOOK_URL` to a Slack [incoming webhook](https://api.slack.com/messaging/webhooks)
and barca posts to its channel when a job **starts failing**, when it **recovers**, and **once a
day while it keeps failing**. It never posts for an ordinary success, a fully cached run, a
skipped `barca serve` tick, a cancelled run or a run that was killed. Like Datadog it covers
`barca get`, `barca run` and every run `barca serve` starts.

```bash
export BARCA_SLACK_WEBHOOK_URL=https://hooks.slack.com/services/T000/B000/XXXX
barca serve --timezone utc
```

The variable is the whole switch: Slack is not named in `BARCA_TELEMETRY` (naming it there is
an unknown-integration warning that points here). Unset or empty, Slack is off and silent. A
generic `SLACK_WEBHOOK_URL`, which CI systems and other tools set, is ignored. Set the variable
on the deployment only: a laptop with it exported posts its own failures too.

| Variable | Meaning | Default |
|---|---|---|
| `BARCA_SLACK_WEBHOOK_URL` | the incoming webhook URL; it must be `https://`. Any other value is a warning and nothing is sent | unset (off) |
| `BARCA_SLACK_NOTIFY` | `changes`: starts failing, recovered, and the daily reminder. `failures`: every failed run, plus recovered. `all`: every finished run that executed at least one step | `changes` |
| `BARCA_UI_URL` | base URL of the web UI (as reached through its proxy, e.g. `https://barca.example.com/ui/`); the run id in a post links to that run | unset, no link |

A **job** is the canonical resolved target, the same as `barca.job` on a Datadog span:
`nightly` and `pipeline.py:nightly` are one job. After each finished run barca compares its
outcome with the job's previous finished run in the metadata database and with the last post
it made for the job, so it works the same across separate CLI processes and across a server
restart:

| Previous run | This run | `changes` posts |
|---|---|---|
| success, or none | failed | **is failing** |
| failed | failed | **is still failing**, if the last post for the job was 24 hours ago or more |
| failed | success | **recovered** |
| success | success | nothing |

A run where every step was cached counts as a success. A cancelled run, a skipped tick and a
run recorded as `interrupted` do not change the job's state.

What a post looks like (the run id links to the web UI when `BARCA_UI_URL` is set):

```
🔴 pipeline.py:daily_revenue is failing
Step pipeline.py:orders failed after 3 attempts in 4m 12s
KeyError: 'region'
env prod · host barca-prod-1 · run 0179afe0cb58
barca history 0179afe0cb58
```

```
✅ pipeline.py:daily_revenue recovered
Failing since 2026-10-19 06:00 UTC (4 runs)
env prod · host barca-prod-1 · run 0179b2a1c4d0
```

A post is one Block Kit message with a plain `text` fallback. It carries the job, the first
failed step (and how many failed), the exception type and the first line of its message (at
most 300 characters), attempts, duration, env, host and run id. Tracebacks are not sent. The
wording is not a contract and may change in any release.

**Delivery.** The post goes out after the run's results are recorded, within the same 3-second
limit as every telemetry integration, and never changes the run's outcome or exit code. When
the webhook cannot be reached or refuses the post, stderr gets the usual telemetry warning,
naming the failure category and never the webhook URL:

```
[barca] warning: telemetry 'slack' did not receive run 0179afe0cb58: HTTP 404
```

Categories are `connection failed`, `HTTP <code>` (`HTTP 429 (rate limited)` when Slack
rate-limits), `no reply within 3s` and `request failed`. barca does not wait out a `429`. A post
that was not delivered is not recorded, so the next run of the job posts the transition again.

**Creating the webhook.** At [api.slack.com/apps](https://api.slack.com/apps) choose *Create New
App*, then *From a manifest*, pick the workspace and paste:

```yaml
display_information:
  name: barca
  description: Job failures and recoveries from barca
oauth_config:
  scopes:
    bot:
      - incoming-webhook
settings:
  org_deploy_enabled: false
  socket_mode_enabled: false
  token_rotation_enabled: false
```

Then *Install to Workspace*, choose the channel, and copy the webhook URL from *Incoming
Webhooks*. The URL is a secret: anyone holding it can post to that channel, so keep it out of
`barca.toml` and the repository. A webhook posts to one channel; to route projects to different
channels, give each deployment its own webhook.

Limits:

- A run that fails before it starts (a syntax error, an unknown target, a shared-state pull
  that fails) is not a finished run and posts nothing, as with Datadog.
- With shared remote state, the record of the last post travels with the metadata database
  when the run pushes it. A run whose push conflicts and is replayed can lose that record, and
  the next run may then post the same transition again. Two runs of one job finishing at the
  same moment can both post.
- A run without targets is the job `all`, whatever files it reads.
- The reminder interval is fixed at 24 hours. There is no job filter and no per-job channel.
- Runs recorded before this version have no job and do not count as a previous run.

See also: [`barca docs scheduling`](/scheduling/), [`barca docs groups`](/reference/groups/), `barca docs partitions`,
[`barca docs contract`](/reference/cli-contract/).
