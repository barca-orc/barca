---
title: Deploying barca serve
description: Run barca serve as a long-lived service, in a container or behind nginx, and what happens on restart.
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

- **It binds `127.0.0.1` only, with no authentication.** There is no flag to change the
  address. Anything that should reach it from another host, or from outside its container,
  goes through a proxy that you put in front of it.
- **State is the `.barca/` directory in the project.** It holds the cache, the run history
  and the scheduler's last fire times. Keep it on storage that survives a restart.
- **One instance per project.** Runs in progress and their live events are held in the
  server's memory. Do not run two servers on one project or load-balance across several.

## In a container

This example was run with Docker 29 and barca 0.18.0. It has two services: barca, and an nginx
that shares barca's network namespace so that it can reach `127.0.0.1:8274`.

```dockerfile
# Dockerfile
FROM python:3.12-slim
RUN pip install --no-cache-dir 'barca[parquet]==0.18.0'
WORKDIR /project
CMD ["barca", "serve", "--timezone", "utc"]
```

```yaml
# compose.yaml
services:
  barca:
    build: .
    platform: linux/amd64
    volumes:
      - ./project:/project
      - barca-state:/project/.barca
    stop_signal: SIGINT
    restart: unless-stopped
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
{"read_only":false,"scheduler":true,"status":"ok","version":"0.18.0"}
```

`docker compose ps` shows the `barca` service as `healthy` once the first check passes, and
the proxy starts after that.

What each part is for:

- **The image.** Barca publishes wheels for x86-64 Linux with glibc and for macOS on Apple
  Silicon. Use a Debian-based image. There is no wheel for Alpine (musl)
  ([issue #107](https://github.com/barca-orc/barca/issues/107)) or for Linux arm64; without a
  wheel pip falls back to the sdist, which needs a Rust toolchain to build. That is why the
  service sets `platform: linux/amd64`; on an x86-64 host the line changes nothing. Add your
  pipeline's own dependencies to the same `pip install`.
- **The project mount.** The project is mounted at the working directory. Barca reads the
  source again for every run, so an edit to a function takes effect at the next run. A new
  file, a new scheduled node or a changed cron expression needs a restart.
- **The `.barca` volume.** A named volume at `/project/.barca` keeps the cache, history and
  schedule state across restarts and image rebuilds. Without it, a recreated container
  computes everything again and does not know which ticks it missed.
- **`--timezone`.** Cron is evaluated in the container's local time unless you say otherwise,
  and that is usually UTC whatever the host uses. State it. An unknown name is not an error:
  barca prints a warning and uses local time.
- **`stop_signal: SIGINT`.** Barca shuts down cleanly on SIGINT (Ctrl-C). It has no SIGTERM
  handler, and as process 1 in a container it ignores SIGTERM, so a default `docker stop`
  waits out its timeout and then kills the process.
- **The health check.** `GET /health` returns 200 with the JSON above when the server is up.
  The check runs inside the container, so it can use `127.0.0.1`. The slim image has no
  `curl`, so the check uses Python.
- **The port.** Publishing barca's own port (`-p 8274:8274`) does not work: Docker forwards
  to the container's network interface and barca listens on loopback only, so the connection
  is closed without a reply. The proxy shares barca's network namespace
  (`network_mode: "service:barca"`), listens on all interfaces at 8080, and forwards to
  `127.0.0.1:8274`. The published port belongs on the `barca` service because that service
  owns the namespace. On Linux, `network_mode: host` for barca and a proxy on the host is an
  alternative.

The published port above is bound to the host's loopback. Barca has no authentication, so
before you bind it to anything wider, add [authentication](#authentication) at the proxy.

### What happens on restart

- **Stopped with SIGINT** (`docker compose stop` with the `stop_signal` above, or Ctrl-C):
  runs in progress are cancelled, their workers are stopped, and they are recorded as
  `cancelled`. The process exits with code 0 within a few seconds.
- **Killed** (SIGKILL, out of memory, host lost): steps that had finished are already recorded
  and are served from cache next time. In 0.18.1 and earlier the run in progress stays in
  `barca history` with status `running` for good: those versions look for the run's process
  by its id and host name, and in a container the new server has the same process id and
  another host name. Later versions report it as `interrupted` once a container starts again
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
- **In-memory run handles are lost.** A `run_id` returned by `POST /run` before the restart
  is unknown to `GET /status/{run_id}` afterwards. Finished runs are in `barca history`.

### With a remote store

With a remote store configured (`BARCA_REMOTE_URI` or `[remote]` in `barca.toml`), barca by
default also keeps a shared history in the store. `barca serve` does not support that and
refuses to start:

```
$ BARCA_REMOTE_URI=s3://my-bucket/barca/orders barca serve
barca serve does not support shared remote state yet — set state = "off" in barca.toml (or BARCA_STATE=off) to serve with a local metadata DB

See `barca serve --help`.
```

The exit code is 2. Set `BARCA_STATE=off` in the service's environment:

```yaml
    environment:
      BARCA_REMOTE_URI: s3://my-bucket/barca/orders
      BARCA_STATE: "off"
```

Artifacts are then written locally and uploaded to the store, and the run history stays in
the `.barca` volume. Other machines that use the same store do not see this server's runs in
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

- **Live logs.** The run event stream (`GET /events/{run_id}`) sends `X-Accel-Buffering: no`,
  which makes nginx pass it through as it is produced. Do not add
  `proxy_ignore_headers X-Accel-Buffering`: with it nginx buffers the stream and the UI shows
  no logs until the run ends. An idle stream carries a keep-alive every 15 seconds, which is
  inside nginx's default 60-second read timeout.
- **One instance.** Point nginx at a single `barca serve`.
- **Same network namespace.** Barca listens on `127.0.0.1` only, so nginx must run on the same
  machine, or share barca's network namespace as in the [container example](#in-a-container).

`tests/integration/test_reverse_proxy.sh` in the repository checks the prefix, the redirect,
the UI and its assets, and that log lines arrive while a run is still going, through a real
nginx.

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
