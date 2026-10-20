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

- **It binds `127.0.0.1` by default, with no authentication.** `--host 0.0.0.0` (or `--host ::`
  for IPv6) makes it reachable through another interface. Anyone who can reach the port can
  start runs, so use a private network and authenticate at a proxy.
- **State is the `.barca/` directory in the project.** It holds the cache, the run history
  and the scheduler's last fire times. Keep it on storage that survives a restart.
- **One instance per project.** Runs in progress and their live events are held in the
  server's memory. Do not run two servers on one project or load-balance across several.

## What ships today

The working deployment is one `barca serve` with its pipeline code, Python dependencies
and persistent `.barca/` directory on the server. Users trigger runs through the web UI,
HTTP API or `barca.Client`; the work executes on that server's machine.

```bash
# On the server, from the project directory; install the pipeline's dependencies too.
uv add 'barca==0.24.0'
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
same API through the authenticating proxy described below. See the
[Python client](/reference/server-api/#python-client) for the equivalent SDK calls.

The [server-ledger design (RFC-0007)](/rfcs/0007-server-mode/) is accepted; its run contract
over HTTP is implemented (#393). `[server] url`, `BARCA_SERVER`, the CLI `--server`/`--local`
flags and laptop runs sharing the server's ledger are pending #394 and #402.
Ordinary `barca get` and `barca.get()` still run locally. A remote artifact store does
not give another machine access to this server's cache/history; with a remote store the
server backs its history up there, as described below.

## Listen on another interface

```bash
barca serve --host 0.0.0.0             # every IPv4 interface (containers, VMs)
barca serve --host 0.0.0.0 --port 8400
```

A non-loopback address prints a startup warning. `--host` takes an IP address, not a hostname.
In Docker, keep barca's port unpublished and let the proxy reach it over a private bridge
network. An nginx upstream can then be `http://barca:8274/`, with no shared network namespace.

## In a container

This example pins barca 0.24.0. It has two services: barca, and an nginx that shares
barca's network namespace so that it can reach `127.0.0.1:8274`.

```dockerfile
# Dockerfile
FROM python:3.13-slim
RUN pip install --no-cache-dir --only-binary=:all: 'barca==0.24.0'
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
    # Longer than the drain timeout (30 s), so runs in flight finish on `docker compose stop`.
    stop_grace_period: 40s
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
{"read_only":false,"scheduler":true,"status":"ok","version":"0.24.0","load_errors":[]}
```

`docker compose ps` shows the `barca` service as `healthy` once the first check passes, and
the proxy starts after that.

What each part is for:

- **The image.** v0.24.0 publishes core wheels and native archives for both x86-64 and
  arm64 Linux with glibc or musl. Debian slim selects the native GNU wheel; Alpine selects
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
  flight get up to 30 seconds to finish (`--drain-timeout`, `[server] drain_timeout`). Docker
  kills the container after 10 seconds unless told otherwise, so give it longer than the drain
  timeout. With `docker stop` that is `-t 40`; in Kubernetes, `terminationGracePeriodSeconds`
  (default 30) should exceed it too.
- **The health check.** `GET /health` returns 200 with the JSON above when the server is up.
  While it drains it still returns 200, with `"status": "draining"`.
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

The published port above is bound to the host's loopback. Barca has no authentication, so
before you bind it to anything wider, add [authentication](#authentication) at the proxy.

### Native Debian and Alpine targets

The v0.24.0 release verifies fresh official PyPI wheel installations and downloaded
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
RUN pip install --no-cache-dir --only-binary=:all: 'barca==0.24.0'
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
The legacy override is unnecessary for the verified v0.24.0 Linux targets above.

### What happens on restart

- **Stopped with SIGINT or SIGTERM** (`docker compose stop`, `docker stop`, `systemctl stop`,
  Ctrl-C; SIGTERM from 0.20.0, see `stop_signal` above). From 0.27.0 the server drains: new
  runs get `503` with `Retry-After`, the scheduler stops, `/health` reports `draining`, and the
  runs in progress finish and are recorded, artifact uploads included, for up to
  `--drain-timeout` seconds (default 30). The process then exits 0. If the deadline passes or
  a second signal arrives first, the runs left are cancelled: their workers are stopped, the
  steps that finished are kept, and they are recorded as `cancelled` with reason
  `server shutdown`. The process then exits 1, after at most about 12 more seconds (10 for
  the runs to stop, 2 for open connections). Before 0.27.0 the first signal cancelled the runs
  in progress at once and the process exited 0. Give the supervisor a grace period longer
  than the drain timeout (`stop_grace_period` above); otherwise Docker kills the container
  mid-drain, as in the next case.
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
environment). Barca 0.25 and earlier needed `BARCA_STATE=off` here; 0.26.0 removed that
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
