---
title: Deploying barca serve
description: Run barca serve as a long-lived service, in a container or behind nginx or Traefik, and what happens on restart.
---

`barca serve` is one process: the HTTP API, the cron scheduler and the web UI. The UI is
compiled into the `barca` binary, so there is nothing else to install.

```bash
barca serve                      # API, scheduler and web UI on 127.0.0.1:8274
open http://127.0.0.1:8274/      # redirects to /ui/
```

The UI opens on the Assets table: every node with its cache state, last run, typical duration
and next scheduled run, refreshed every 10 seconds. Failed and stale nodes are listed first.

Three facts shape every deployment:

- **It binds `127.0.0.1` by default, with authentication off.** `--host 0.0.0.0` (or `--host ::`
  for IPv6) makes it reachable through another interface. Anyone who can reach the port can
  start runs unless you enable `--auth token` or authenticate at a proxy; use a private network and TLS.
  Project tokens and local bootstrap are documented in [Server Mode](/reference/server-mode/#opt-in-project-tokens).
- **State is the `.barca/` directory in the project.** It holds the cache, the run history
  and the scheduler's last fire times. Keep it on storage that survives a restart.
- **One instance per project.** Runs in progress and their live events are held in the
  server's memory. Do not run two servers on one project or load-balance across several.

## What ships today

The working deployment is one `barca serve` with its pipeline code, Python dependencies
and persistent `.barca/` directory on the server. Users trigger runs through the web UI, the
HTTP API or the CLI's `--server` flag; the work executes on that server's machine.

```bash
# On the server, from the project directory; install the pipeline's dependencies too.
uv add 'barca==0.28.0'
uv run barca serve pipeline.py --timezone utc
```

Open `http://127.0.0.1:8274/ui/`. From the same machine, start an asset and follow its run:

```bash
curl -X POST http://127.0.0.1:8274/runs -d '{"command": "get", "targets": ["daily_report"]}'
# Copy run_id from the response:
curl -N http://127.0.0.1:8274/runs/RETURNED_RUN_ID/events   # its progress, then its outcome
curl http://127.0.0.1:8274/runs/RETURNED_RUN_ID             # the run and its outcome
```

Replace `daily_report` with an asset in the deployed pipeline. Remote users reach the
same API through the proxy described below.

## Running commands on the server from another machine

The CLI runs `get`, `run`, `status`, `list`, `history` and `stats` on the server with
`--server`, and prints what the same command prints locally: the same stdout JSON, the same
stderr lines, the same exit codes. Ctrl-C cancels the run on the server. It is experimental.

```bash
# On a laptop, in a checkout of the same project (or anywhere: files are optional).
barca get daily_report --server https://barca.internal.example.com
export BARCA_SERVER=https://barca.internal.example.com   # or [server] url in barca.toml
barca run publish --server --agent       # the configured server; agent progress lines
barca history -l 20                      # the server's runs (configured server)
```

```python
import barca

barca.get("daily_report", server=True)   # runs on the configured server, returns the value
```

The server runs its deployed files, never yours. When a file you name differs from the
deployed one, barca prints `[barca] note: <file> differs from the deployment; the server runs
the deployed version` and continues; deploy the change to run it there. Use the proxy's URL,
path prefix included (`https://example.com/barca`). A server that cannot be reached, or one
that is not barca or runs another API version, is exit 3 naming the URL; nothing runs locally
instead. If the connection drops during a run, the client exits 3 naming the run id, and the
run continues on the server (`barca history --server`). `barca docs server-mode` has the full
rules, and the few declared differences from a local run (the server's paths, `triggered_by`).

The client sends no credentials. Reach the server over a private network (a VPN, Tailscale,
an internal load balancer), or through a proxy that admits the client's network.

## The server as the team's ledger

[Share Results With a Team](/team-server/) sets this up in three steps.
The [server-ledger design (RFC-0007)](/rfcs/0007-server-mode/) is accepted and implemented:
the run contract over HTTP (#393), the CLI's `--server` (#394) and the ledger (#402). A laptop
with `[server] url` (or `BARCA_SERVER`) pointing at this server and `[remote]` pointing at its
store runs `barca get` locally, reads cache hits from the server's ledger and posts what it
computes ([Server Mode](/reference/server-mode/)); `barca status`,
`history` and `stats` answer from the server. `--local` runs against the laptop's own ledger
and sends nothing. With a remote store the server backs its history up there, as described below.
`barca.Client` is deprecated in favour of `barca.get(..., server=True)`.

## Restarting without losing work

A deploy, a reboot or a config change restarts the server. Every run it accepted (a request,
a `--server` command, the UI, a scheduled tick) is in its run queue, kept in the metadata DB on
the persistent `.barca/`, so nothing is lost or cancelled:

```bash
barca queue pause --server       # optional: nothing new starts; runs in flight finish
systemctl restart barca          # or: docker restart barca, docker compose up -d
barca queue --server             # the queue is back; still paused if you paused it
barca queue resume --server      # if you paused it
```

1. **Pause (optional).** The runs in flight finish while nothing new starts, so the stop that
   follows waits only for them. The pause is kept across the restart.
2. **Restart.** The supervisor sends SIGTERM: the server drains, and exits 0 once the runs in
   flight finished, or at `--drain-timeout` after putting the ones left back at the head of the
   queue. Runs submitted meanwhile are accepted and queued.
3. **The queue resumes.** The new server prints `[barca] resuming N queued runs` and starts them
   in order under their run ids. `barca get|run --server` and `barca logs -f --server` clients
   waiting on a run reconnect (for up to 60 seconds, `BARCA_SERVER_RECONNECT`) and follow it to
   its end.

A requeued run starts again from its beginning, so a run is executed at least once: assets that
finished are cache hits, but an interrupted step or task runs again. Make tasks with side
effects safe to repeat, or give them a longer drain timeout.

The supervisor's grace period must cover the drain timeout plus what the server takes after it:
12 seconds with a drain timeout of 10 or more (10 for stopped runs to record themselves, 2 for
open connections), plus up to 31 for the last metadata backup with a store and 3 for the
telemetry flush with `BARCA_TELEMETRY`. With a drain timeout under 10 seconds these shrink: half
the drain timeout (at least 2 s) for the runs, 1 s for connections, the drain timeout (at least
5 s) for the backup. So `drain_timeout = 5` without a store needs 9 seconds and fits Docker's
default of 10. Under a supervisor (process 1 of a container, systemd, Kubernetes) `barca serve`
prints the number at startup:

```text
[barca] stop grace: the container runtime must allow at least 9s between SIGTERM and SIGKILL (drain timeout 5s + 4s for stopping runs and closing connections); set docker stop -t or stop_grace_period (Docker's default is 10s)
```

A supervisor that kills the server mid-drain costs the runs in flight the time the drain gave
them: the next server puts them back in the queue (once per run; a run in flight through two
killed drains in a row is recorded as interrupted). The queued runs survive it too.

| Supervisor | Setting | Default | With the default 30 s drain timeout |
|---|---|---|---|
| systemd | `TimeoutStopSec=` | 90 s | enough |
| Docker | `docker stop -t`, `stop_grace_period` | 10 s | `45s` |
| Kubernetes | `terminationGracePeriodSeconds` | 30 | `45` |

More in `barca docs scheduling` ("The run queue", "Stopping the server").

## Listen on another interface

```bash
barca serve --host 0.0.0.0             # every IPv4 interface (containers, VMs)
barca serve --host 0.0.0.0 --port 8400
```

A non-loopback address prints a startup warning. `--host` takes an IP address, not a hostname.
In Docker, keep barca's port unpublished and let the proxy reach it over a private bridge
network. An nginx upstream can then be `http://barca:8274/`, with no shared network namespace.

## In a container

This example pins barca 0.28.0. It has two services: barca, and an nginx that shares
barca's network namespace so that it can reach `127.0.0.1:8274`.

```dockerfile
# Dockerfile
FROM python:3.13-slim
RUN pip install --no-cache-dir --only-binary=:all: 'barca==0.28.0'
WORKDIR /project
CMD ["barca", "serve", "--timezone", "utc"]
```

```yaml
# compose.yaml
services:
  barca:
    build: .
    volumes:
      - ./project:/project
      - barca-state:/project/.barca
    restart: unless-stopped
    # Longer than the drain timeout (30 s) plus 15 s, so runs in flight finish (or go back to
    # the run queue) on `docker compose stop`.
    stop_grace_period: 45s
    ports:
      - "127.0.0.1:8080:8080"
    healthcheck:
      test: ["CMD", "python", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8274/health', timeout=3)"]
      interval: 30s
      timeout: 5s
      start_period: 10s

  proxy:
    image: nginx:1.27-alpine
    network_mode: "service:barca"
    volumes:
      - ./nginx.conf:/etc/nginx/conf.d/default.conf:ro
    depends_on:
      barca:
        condition: service_healthy

volumes:
  barca-state:
```

```nginx
# nginx.conf
server {
    listen 8080;
    location / {
        proxy_pass http://127.0.0.1:8274/;
    }
}
```

```
$ docker compose up -d
$ curl -s http://127.0.0.1:8080/health
{"api_version":1,"cache":{"limit_bytes":3351192780,"size_bytes":284484},"env":"default","load_errors":[],"metadata_error":null,"read_only":false,"scheduler":true,"scheduler_enabled":true,"status":"ok","store_uri":null,"version":"0.28.0"}
```

`docker compose ps` shows the `barca` service as `healthy` once the first check passes, and
the proxy starts after that.

What each part is for:

- **The image.** Every release from 0.24.0 publishes core wheels and native archives for both
  x86-64 and arm64 Linux with glibc or musl. Debian slim selects the native GNU wheel; Alpine selects
  the native musl wheel. The example needs no `platform: linux/amd64` override or Rust
  compiler. Add your pipeline's own dependencies separately; their wheels and Barca's
  optional SQL/parquet/cloud extras have their own platform requirements.
- **The project mount.** The project is mounted at the working directory. Barca reads the
  source again for every run, so an edit to a function takes effect at the next run. A new
  file, a new scheduled node or a changed cron expression needs a restart.
- **The `.barca` volume.** A named volume at `/project/.barca` keeps the cache, history and
  schedule state across restarts and image rebuilds. Without it, a recreated container
  computes everything again and does not know which ticks it missed.
- **`--timezone`.** Cron is evaluated in the container's local time unless you say otherwise,
  and that is usually UTC whatever the host uses. State it. A value barca does not know is a
  usage error: the server exits 2 and names it.
- **Legacy `stop_signal: SIGINT`.** Needed for barca 0.19.0 and earlier. Those releases
  shut down cleanly on SIGINT (Ctrl-C) but have no SIGTERM
  handler, and as process 1 in a container they ignore SIGTERM, so a default `docker stop`
  waits out its timeout and then kills the process. Later releases shut down the same way on
  SIGTERM as on SIGINT, as process 1 too: with them the line can be removed, and leaving it in
  changes nothing.
- **`stop_grace_period`.** On `docker compose stop` (and on a redeploy) barca drains: runs in
  flight get up to 30 seconds to finish (`--drain-timeout`, `[server] drain_timeout`), then go
  back to the run queue. Docker kills the container after 10 seconds unless told otherwise, so
  give it at least the number the startup line prints (`[barca] stop grace: ...`): 42 seconds
  with the default drain timeout and no store, so `45s`. With `docker stop` that is `-t 45`; in
  Kubernetes, `terminationGracePeriodSeconds` (default 30) should be 45 too.
- **The health check.** `GET /health` returns 200 with the JSON above when the server is up.
  While it drains it still returns 200, with `"status": "draining"`. Alert on
  `scheduler_enabled: false` for a server that should schedule, and on a non-null
  `metadata_error`.
- **The metadata DB.** The first line `serve` prints names the environment, the artifact store
  and the database with its format, e.g. `[barca] env prod (--env); artifact store local
  (.barca/envs/prod/artifacts); metadata .barca/envs/prod/metadata.db (db format 1)`. Check it
  names the environment you meant: with `default_env` in `barca.toml` and no `--env`, it is that
  one. A writer server that cannot set the database up (empty, not SQLite, written by a newer
  barca, a malformed schema, or not openable) exits 3 before it binds, naming the file and what
  to do, so the restart policy and the health check show it; before 0.28 it kept serving with
  the scheduler off. A `--read-only` admin server keeps serving and reports the error in
  `metadata_error`. An empty or invalid `.barca/metadata.db` left beside the env's database
  gets a warning naming both; remove it.
  The check runs inside the container, so it can use `127.0.0.1`. The slim image has no
  `curl`, so the check uses Python.
- **The port.** With the default loopback address, publishing barca's own port
  (`-p 8274:8274`) does not work: Docker forwards
  to the container's network interface and barca listens on loopback only, so the connection
  is closed without a reply. The proxy shares barca's network namespace
  (`network_mode: "service:barca"`), listens on all interfaces at 8080, and forwards to
  `127.0.0.1:8274`. The published port belongs on the `barca` service because that service
  owns the namespace. On Linux, `network_mode: host` for barca and a proxy on the host is an
  alternative.

The published port above is bound to the host's loopback. Authentication defaults off, so
before you bind it to anything wider, add [authentication](#authentication) at the proxy.

### Containers with a memory limit

Steps run in separate worker processes, independent ones at the same time. Under a hard memory
limit (`mem_limit`, a pod's limit), heavy steps that become ready together can add up past it,
and the kernel kills one: the step fails with
`worker for pipeline.py:train exited with SIGKILL (possibly the OOM killer)` (see
[A worker that dies](/reference/cli/#a-worker-that-dies)). Give those steps a `weight=`
(experimental): the steps running at once add up to at most the capacity, the pool size unless
`[execution] capacity` in `barca.toml` or `BARCA_CAPACITY` sets it.

```toml
# barca.toml
[execution]
capacity = 4
```

With `@asset(weight=2)` on the heavy assets, two of them run at once and light steps use what is
left. The capacity is per run: two schedules that fire together run with a capacity
each. See [Heavy steps](/reference/api/decorators/#heavy-steps-weight).

### Native Debian and Alpine targets

Every release from 0.24.0 verifies fresh official PyPI wheel installations and downloaded
GitHub native archives on native x86-64 and arm64 runners. Each target runs a core
pipeline, reuses its cache, serves `/health`, and serves the embedded UI without a
compiler or a source checkout.

| Linux runtime | CPU | Native archive |
| --- | --- | --- |
| GNU/glibc (Debian) | x86-64 | `barca-linux-x86_64.tar.gz` |
| GNU/glibc (Debian) | arm64 | `barca-linux-arm64.tar.gz` |
| musl (Alpine) | x86-64 | `barca-linux-musl-x86_64.tar.gz` |
| musl (Alpine) | arm64 | `barca-linux-musl-arm64.tar.gz` |

For an Alpine core deployment, use the same Compose configuration with this image:

```dockerfile
FROM python:3.13-alpine
RUN pip install --no-cache-dir --only-binary=:all: 'barca==0.28.0'
WORKDIR /project
CMD ["barca", "serve", "--timezone", "utc"]
```

These checks cover Barca's stdlib-only core. A pipeline using compiled libraries or
optional extras must also verify those dependencies for its selected CPU and libc;
use Debian if its dependencies require GNU wheels. Native archives include the CLI
and UI; Python workers still need a Python environment with the matching Barca package
and the pipeline's dependencies.

**Keeping v0.21.0:** that release publishes GNU x86-64 Linux and macOS Apple Silicon
wheels, without Linux arm64 or musl wheels. Keep a Debian image and the following
Compose override when deploying its Linux wheel, including on arm64 hosts:

```yaml
services:
  barca:
    platform: linux/amd64
```

Pin `barca==0.21.0` in that image. Without a compatible wheel, pip can fall back to a
source build requiring Rust; `--only-binary=:all:` makes an unavailable wheel explicit.
The legacy override is unnecessary for the Linux targets above, verified on every release
from 0.24.0.

### Logs

`barca serve` writes its lines as text on stderr. A container log collector that files stderr as
errors (Datadog's does) tags all of them, `step:... completed` included, as errors. Set
`BARCA_LOG=json` and each line is one JSON object on stdout, with a real `level` (`info`,
`warning` or `error`), the `run_id` and `node` it is about, and a step's printed output as
`info` lines of its run:

```yaml
    environment:
      BARCA_LOG: json
      BARCA_TELEMETRY: datadog   # optional: adds dd.trace_id and dd.span_id to each run's lines
```

```
{"ts":"2026-10-20T19:45:44.067Z","level":"info","msg":"step:pipeline.py:orders completed 0.0s (1/2)","run_id":"2b585c3e0785","node":"pipeline.py:orders","dd.trace_id":"8371769010332850555","dd.span_id":"1894635935906895539"}
```

With the Datadog integration on, the `dd.trace_id` and `dd.span_id` of a run's lines are its
trace's ids, so a failing `barca.run` span leads to that run's lines and back. What a step writes
to its own stderr (Python's `logging`, by default) stays on stderr. The fields and their
limits are in [Telemetry](/reference/telemetry/#logs-under-barca-serve).

### Traces and metrics in Datadog

With `BARCA_TELEMETRY=datadog` every run the server starts is one APM trace, sent to the
Datadog Agent's trace port, and the server reports itself as DogStatsD metrics on the Agent's
DogStatsD port. Point both at the Agent and name the service once; everything barca sends,
Python and opted-in library spans included, is that one service:

```yaml
    environment:
      BARCA_TELEMETRY: datadog
      DD_AGENT_HOST: datadog-agent    # traces on 8126, DogStatsD on 8125
      DD_SERVICE: pipelines
      DD_ENV: production
      DD_VERSION: "2026.10.20"        # your deploy; barca's own version is barca.version
```

In APM, the `barca.run` resource is the command and the targets (`run publish`), and the
`barca.step` resources are the node names, so a monitor on `barca.run` errors per resource is a
monitor per job, and the trace list says which asset failed and why. A monitor on the
`barca.serve.up` gauge that alerts when it stops arriving or reads 0 catches a server that is
down or was stopped; it is sent every five minutes. Tags, the partition cap and opt-in library
tracing are in [Telemetry](/reference/telemetry/#datadog).

### What happens on restart

- **Stopped with SIGINT or SIGTERM** (`docker compose stop`, `docker stop`, `systemctl stop`,
  Ctrl-C; SIGTERM from 0.20.0, see `stop_signal` above). From 0.29.4 nothing the server
  accepted is lost: the run queue pauses (new runs are still accepted, `202` with
  `status: "pending"`, and wait for the next start), the scheduler stops, `/health` reports
  `draining`, and the runs in progress finish and are recorded, artifact uploads included, for
  up to `--drain-timeout` seconds (default 30). The process then exits 0. If the deadline
  passes first, the runs left are stopped and put back at the head of the queue, and the
  process exits 0; the next server starts them again from the beginning (the steps that
  finished are cache hits). A second signal cancels them instead: they are recorded as
  `cancelled` with reason `server shutdown`, and the process exits 1, after at most about 12
  more seconds (10 for the runs to stop, 2 for open connections). From 0.27.0 to 0.29.3 a
  draining server refused new runs with `503` and cancelled the runs left at the deadline;
  before 0.27.0 the first signal cancelled the runs in progress at once. Give the supervisor
  a grace period longer than the drain timeout plus 15 seconds (`stop_grace_period` above);
  otherwise Docker kills the container mid-drain, as in the next case.
- **Killed** (SIGKILL, out of memory, host lost): steps that had finished are already recorded
  and are served from cache next time. In 0.19.0 and earlier the run in progress stays in
  `barca history` with status `running` for good: those versions look for the run's process
  by its id and host name, and in a container the new server has the same process id and
  another host name. From 0.20.0, barca reports it as `interrupted` once a container starts again
  on the same `.barca` volume, and the first run after the restart writes that to the
  history. That holds for a `.barca` on a named volume, as here. With `.barca` on a bind mount from a Docker Desktop
  host the run can stay `running`: after a restart of the machine, or when another
  container was started before the replacement. The rule and its limits are in
  `barca docs cache`, "While a run is going, and after one is killed".
- **Runs are not resumed.** Nothing restarts a cancelled or killed run. The next tick or
  request starts a new one, which reuses whatever was cached.
- **Schedules catch up once.** If a tick passed while the server was down, the job fires once
  at startup (`[barca] catch-up run pipeline.py:report → ...`). Several missed ticks of one
  job still produce one run.
- **Finished runs are kept, runs in flight are not resumed.** `GET /runs/{run_id}` answers
  from the metadata DB, so a run that finished before the restart shows the same outcome after
  it; its live event stream is gone (`404`). A run still queued at the restart never started.
  A paused schedule stays paused.

### With a remote store

With a remote store configured (`BARCA_REMOTE_URI` or `[remote] uri` in `barca.toml`), the
server uploads artifacts to it and backs its metadata DB up there every minute it changed
(and once on shutdown), keeping the five newest copies:

```yaml
    environment:
      BARCA_REMOTE_URI: s3://my-bucket/barca/orders
```

The run history stays in the `.barca` volume. If that volume is lost, start a container with an
empty one, run `barca state restore` in it before `barca serve`, and the newest backup comes
back from the store (`barca state restore` refuses to run while a server is using the
environment). It takes the newest backup this host and project directory took; a container
with a new host name has none, so it takes the newest and names the host that took it. Barca 0.25 and earlier needed `BARCA_STATE=off` here; 0.26.0 removed that
variable, and setting it is now a usage error.

Other machines that use the same store do not see this server's runs in
`barca history` and do not get cache hits from its results: a cache hit is found through
the history, and theirs has no row for it. Credentials and the store's other settings are in
[Remote storage](/reference/remote-storage/).

## Behind nginx

Barca can be mounted under any path. This is enough, live run logs included:

```nginx
location /barca/ {
    proxy_pass http://127.0.0.1:8274/;
}
```

Open `https://your-host/barca/`. It redirects to `/barca/ui/`, and the UI finds the API under
the same prefix. There is no base-path setting.

- **Live logs.** The run event stream (`GET /runs/{run_id}/events`) sends `X-Accel-Buffering: no`,
  which makes nginx pass it through as it is produced. Do not add
  `proxy_ignore_headers X-Accel-Buffering`: with it nginx buffers the stream and the UI shows
  no logs until the run ends. An idle stream carries a keep-alive every 15 seconds, which is
  inside nginx's default 60-second read timeout.
- **One instance.** Point nginx at a single `barca serve`.
- **The upstream address.** With the default `127.0.0.1`, nginx must run on the same machine
  or share barca's network namespace as in the [container example](#in-a-container). With
  `--host 0.0.0.0`, it can reach barca over a private Docker bridge network or another host.

`tests/integration/test_reverse_proxy.sh` checks the prefix, the redirect, the UI and its assets,
and that log lines arrive while a run is still going through nginx and Traefik. Both proxies
run in separate containers and reach `barca serve --host 0.0.0.0` over the bridge network.
A control check makes nginx ignore the streaming header and confirms that it buffers events.

## Behind Traefik

A path-prefix router with a `stripPrefix` middleware. With the Docker provider, as labels on the
barca container:

```yaml
services:
  barca:
    # ... image, volumes (see below)
    command: ["barca", "serve", "--host", "0.0.0.0"]
    stop_signal: SIGINT
    labels:
      - traefik.enable=true
      - traefik.http.routers.barca.rule=PathPrefix(`/barca`)
      - traefik.http.routers.barca.middlewares=barca-strip
      - traefik.http.middlewares.barca-strip.stripprefix.prefixes=/barca
      - traefik.http.services.barca.loadbalancer.server.port=8274
```

The same routing with the file provider:

```yaml
http:
  routers:
    barca:
      rule: "PathPrefix(`/barca`)"
      middlewares: [barca-strip]
      service: barca
  middlewares:
    barca-strip:
      stripPrefix:
        prefixes: ["/barca"]
  services:
    barca:
      loadBalancer:
        servers:
          - url: "http://barca:8274"
```

Traefik needs no streaming settings: run events pass straight through.

## Authentication

Barca has none. Anyone who can reach the server can start and cancel runs. Put authentication
in front of it at the proxy, for example nginx basic auth or an SSO proxy such as
oauth2-proxy:

```nginx
location /barca/ {
    proxy_pass http://127.0.0.1:8274/;
    auth_basic "barca";
    auth_basic_user_file /etc/nginx/barca.htpasswd;
}
```

## Read-only dashboards

`barca serve --read-only` serves the UI and the API and changes nothing: run and cancel
endpoints return `403`, the scheduler does not start, and the metadata database is read
through a private copy, so it is never opened in place, created or written. Use it to show a
project to people who should only look, including a project that another barca process is
running. The UI shows a `read-only` tag and disables its run buttons.

```bash
barca serve --read-only
```

Every endpoint is in the [Server API reference](/reference/server-api/).
